//! PDF files: opening them within limits and rendering their pages, in a
//! process of their own.
//!
//! Parsing and rendering are done by hayro, a rasteriser written in Rust. It
//! runs no scripts, fetches nothing and opens nothing by itself. Around it this
//! crate adds what a viewer needs before it can be handed files from anywhere:
//!
//! - [`check`] looks at every stream of an opened file the way the parser sees
//!   it and refuses the file if a stream inflates past a ceiling, a picture is
//!   too large, or something cannot be checked ([`Limits`]). The parser keeps
//!   its own limits on nesting (512 levels of arrays and dictionaries, 50 of
//!   nested forms and patterns, 256 cross-reference sections).
//! - Opening and rendering happen in a child process ([`worker`], [`child`]):
//!   the same executable started with a sub-command. Its allocations are
//!   counted and it exits when it passes its memory budget; a page that takes
//!   too long gets the process killed and restarted. Whatever a file does to
//!   it, the window and the other documents stay.

pub mod check;
pub mod child;
pub mod fonts;
pub mod inflate;
pub mod worker;

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::Duration;

use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::{LoadPdfError, Pdf};
use hayro::vello_cpu::Pixmap;
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{PixmapSettings, RenderCache, RenderSettings};

pub use check::Refusal;
pub use worker::{Program, Worker};

/// Ceilings on what a file may ask for.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Size of the file.
    pub max_file_bytes: u64,
    /// Objects in the file.
    pub max_objects: usize,
    /// Pages in the document.
    pub max_pages: usize,
    /// Size of one stream after its filters.
    pub max_stream_bytes: usize,
    /// Pixels in one picture.
    pub max_image_pixels: u64,
    /// Pixels in one rendered page; larger requests are rendered smaller.
    pub max_render_pixels: u64,
    /// Memory the child process may allocate.
    pub max_memory_bytes: u64,
    /// Time to open and check a file.
    pub open_time: Duration,
    /// Time to render one page.
    pub page_time: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_file_bytes: 1 << 30,
            max_objects: 4_000_000,
            max_pages: 50_000,
            max_stream_bytes: 256 << 20,
            // The same as for pictures in other documents (`aster-image`).
            max_image_pixels: 100_000_000,
            max_render_pixels: 16_000_000,
            max_memory_bytes: 3 << 30,
            open_time: Duration::from_secs(60),
            page_time: Duration::from_secs(20),
        }
    }
}

/// Which limit a file broke.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exceeded {
    /// The file itself is larger than `max_file_bytes`.
    File,
    /// More objects than `max_objects`.
    Objects,
    /// More pages than `max_pages`.
    Pages,
    /// A stream inflates to more than `max_stream_bytes`.
    Stream,
    /// A picture has more pixels than `max_image_pixels`.
    Image,
    /// The child process needed more than `max_memory_bytes`.
    Memory,
    /// Opening took longer than `open_time`.
    Time,
}

/// Why a file could not be opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenError {
    /// It is encrypted and the password (none, at first) does not open it.
    NeedsPassword,
    /// A password was given and it is wrong.
    WrongPassword,
    /// It is encrypted in a way that cannot be read.
    UnsupportedEncryption,
    /// It breaks one of the [`Limits`].
    TooLarge(Exceeded),
    /// Something in it cannot be checked: a filter we do not decode, data
    /// that does not decode, a picture without a readable size.
    Unchecked,
    /// It is not a PDF, or too damaged to read.
    Damaged,
    /// The parser failed unexpectedly (a panic, or the process ended).
    Crashed,
    /// The file could not be read.
    Unreadable(std::io::ErrorKind),
}

/// The size of a page in points (1/72 inch), as it is shown: after its
/// `/Rotate`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageSize {
    /// Width in points.
    pub width: f32,
    /// Height in points.
    pub height: f32,
}

/// What is known once a file is open.
#[derive(Debug, Clone, PartialEq)]
pub struct Info {
    /// Every page's size, in order.
    pub pages: Vec<PageSize>,
    /// The file is encrypted (it may still have opened without a password).
    pub encrypted: bool,
}

/// A rendered page: premultiplied RGBA, on white.
#[derive(Clone)]
pub struct Rendered {
    /// Page index.
    pub page: usize,
    /// The scale that was asked for (pixels per point).
    pub scale: f32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `width * height * 4` bytes.
    pub pixels: Vec<u8>,
}

impl std::fmt::Debug for Rendered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Rendered(page {}, {}x{})",
            self.page, self.width, self.height
        )
    }
}

/// Something a [`Worker`] reports.
#[derive(Debug)]
pub enum Event {
    /// The file is open.
    Opened(Info),
    /// The file could not be opened; the worker has stopped.
    Failed(OpenError),
    /// A page asked for with [`Worker::want`].
    Page(Rendered),
    /// A page that cannot be drawn: it failed, took too long or needed too
    /// much memory. It is not asked for again.
    PageFailed(usize),
}

/// An open document. Lives on one thread (the parser's caches are not
/// shared); the child process owns one.
pub struct Document {
    pdf: Pdf,
    info: Info,
    limits: Limits,
    settings: InterpreterSettings,
}

impl Document {
    /// Open and check `bytes`. Call on a thread with a large stack
    /// ([`STACK_BYTES`]).
    pub fn open(bytes: Vec<u8>, password: &str, limits: Limits) -> Result<Document, OpenError> {
        if bytes.len() as u64 > limits.max_file_bytes {
            return Err(OpenError::TooLarge(Exceeded::File));
        }
        let encrypted = has_encrypt_key(&bytes);
        let opened = catch_unwind(AssertUnwindSafe(|| Pdf::new_with_password(bytes, password)));
        let pdf = match opened {
            Err(_) => return Err(OpenError::Crashed),
            Ok(Err(LoadPdfError::Decryption(error))) => {
                use hayro::hayro_syntax::DecryptionError as E;
                return Err(match error {
                    E::PasswordProtected if password.is_empty() => OpenError::NeedsPassword,
                    E::PasswordProtected => OpenError::WrongPassword,
                    _ => OpenError::UnsupportedEncryption,
                });
            }
            Ok(Err(LoadPdfError::Invalid)) => return Err(OpenError::Damaged),
            Ok(Ok(pdf)) => pdf,
        };
        match catch_unwind(AssertUnwindSafe(|| check::check_document(&pdf, &limits))) {
            Err(_) => return Err(OpenError::Crashed),
            Ok(Err(Refusal::Exceeded(exceeded))) => return Err(OpenError::TooLarge(exceeded)),
            Ok(Err(Refusal::Unchecked)) => return Err(OpenError::Unchecked),
            Ok(Ok(())) => {}
        }
        let pages = catch_unwind(AssertUnwindSafe(|| {
            pdf.pages()
                .iter()
                .map(|page| {
                    let (width, height) = page.render_dimensions();
                    PageSize { width, height }
                })
                .collect::<Vec<_>>()
        }))
        .map_err(|_| OpenError::Crashed)?;
        if pages.is_empty() {
            return Err(OpenError::Damaged);
        }
        Ok(Document {
            pdf,
            info: Info { pages, encrypted },
            limits,
            settings: fonts::settings(),
        })
    }

    /// Pages and whether the file is encrypted.
    pub fn info(&self) -> &Info {
        &self.info
    }

    /// The scale actually used for `page` when `scale` is asked for: smaller
    /// when the page would pass `max_render_pixels` or 16 384 pixels a side.
    pub fn fitted_scale(&self, page: usize, scale: f32) -> f32 {
        let Some(size) = self.info.pages.get(page) else {
            return scale;
        };
        fitted_scale(*size, scale, self.limits.max_render_pixels)
    }

    /// Render `page` at `scale` pixels per point. `Err` if the parser
    /// panicked (the caller should start over: its caches and locks may be
    /// poisoned), `Ok(None)` if there is nothing to draw.
    // The only failure is a caught panic; there is nothing to say about it.
    #[allow(clippy::result_unit_err)]
    pub fn render<'s>(
        &'s self,
        cache: &RenderCache<'s>,
        page: usize,
        scale: f32,
    ) -> Result<Option<Pixmap>, ()> {
        let fitted = self.fitted_scale(page, scale);
        let pages = self.pdf.pages();
        let Some(page_ref) = pages.get(page) else {
            return Ok(None);
        };
        let pixmap = catch_unwind(AssertUnwindSafe(|| {
            hayro::render(
                page_ref,
                cache,
                &self.settings,
                &RenderSettings::default(),
                &PixmapSettings {
                    x_scale: fitted,
                    y_scale: fitted,
                    bg_color: WHITE,
                },
            )
        }))
        .map_err(|_| ())?;
        if pixmap.width() == 0 || pixmap.height() == 0 {
            return Ok(None);
        }
        Ok(Some(pixmap))
    }
}

/// Whether the file names an encryption dictionary. Only to tell the user;
/// nothing about safety depends on it.
fn has_encrypt_key(bytes: &[u8]) -> bool {
    bytes.windows(9).any(|w| {
        &w[..8] == b"/Encrypt" && (w[8].is_ascii_whitespace() || b"/<[(0123456789".contains(&w[8]))
    })
}

/// `scale`, or less so that the page stays within `max_pixels` and 16 384
/// pixels a side.
pub fn fitted_scale(size: PageSize, scale: f32, max_pixels: u64) -> f32 {
    let (w, h) = (size.width.max(1.0), size.height.max(1.0));
    let mut fitted = scale.max(0.01);
    let side = 16_384.0;
    fitted = fitted.min(side / w).min(side / h);
    let pixels = f64::from(w * fitted) * f64::from(h * fitted);
    if pixels > max_pixels as f64 {
        fitted *= (max_pixels as f64 / pixels).sqrt() as f32;
    }
    fitted
}

/// Stack for threads that call into the parser. Its nesting limits keep
/// recursion to a few hundred levels; this leaves room for unoptimised
/// builds.
pub const STACK_BYTES: usize = 256 << 20;

#[cfg(test)]
mod builder;
#[cfg(test)]
mod tests;

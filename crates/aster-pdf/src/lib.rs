//! PDF files: opening them within limits and rendering their pages.
//!
//! Parsing and rendering are done by hayro, a rasteriser written in Rust. It
//! runs no scripts, fetches nothing and opens nothing by itself. Around it this
//! crate adds what a viewer needs before it can be handed files from anywhere:
//!
//! - [`scan`] looks at the whole file before the parser does and refuses one
//!   whose streams inflate past a ceiling, whose pictures are too large, or
//!   that has too many objects ([`Limits`]). The parser keeps its own limits
//!   on nesting (512 levels of arrays and dictionaries, 50 of nested forms
//!   and patterns, 256 cross-reference sections) and skips pages it has
//!   already visited.
//! - Everything that calls into the parser runs on a [`Worker`] thread with a
//!   large stack, inside `catch_unwind`: a panic in the parser becomes an
//!   error for that file or page, not the end of the program. A stack
//!   overflow or running out of memory would still end the process; the
//!   limits are what keep those away.

pub mod fonts;
pub mod inflate;
pub mod scan;

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::object::{ObjectIdentifier, Stream};
use hayro::hayro_syntax::{LoadPdfError, Pdf};
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{PixmapSettings, RenderCache, RenderSettings};

pub use scan::Exceeded;

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
            max_render_pixels: 40_000_000,
        }
    }
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
    /// It is not a PDF, or too damaged to read.
    Damaged,
    /// The parser failed unexpectedly (a panic, caught).
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

/// An open document. Lives on one thread (the parser's caches are not
/// shared); [`Worker`] owns one.
pub struct Document {
    pdf: Pdf,
    info: Info,
    limits: Limits,
    settings: InterpreterSettings,
}

impl Document {
    /// Check and open `bytes`. Call on a thread with a large stack
    /// ([`STACK_BYTES`]); [`Worker`] does.
    pub fn open(bytes: Vec<u8>, password: &str, limits: Limits) -> Result<Document, OpenError> {
        let scanned = scan::scan(&bytes, &limits).map_err(OpenError::TooLarge)?;
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
        let checked = catch_unwind(AssertUnwindSafe(|| check_opened(&pdf, &scanned, &limits)));
        match checked {
            Err(_) => return Err(OpenError::Crashed),
            Ok(Err(exceeded)) => return Err(OpenError::TooLarge(exceeded)),
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
            info: Info {
                pages,
                encrypted: scanned.encrypted,
            },
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

    /// Render `page` at `scale` pixels per point. `None` if the page cannot
    /// be drawn (the parser panicked on it, or it is empty).
    pub fn render<'s>(
        &'s self,
        cache: &RenderCache<'s>,
        page: usize,
        scale: f32,
    ) -> Option<Rendered> {
        let fitted = self.fitted_scale(page, scale);
        let pages = self.pdf.pages();
        let page_ref = pages.get(page)?;
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
        .ok()?;
        let (width, height) = (u32::from(pixmap.width()), u32::from(pixmap.height()));
        if width == 0 || height == 0 {
            return None;
        }
        Some(Rendered {
            page,
            scale,
            width,
            height,
            pixels: pixmap.data_as_u8_slice().to_vec(),
        })
    }
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

/// Checks that need the parser: the object count it found, the number of
/// pages, and, in encrypted files, the streams the scan could not read.
fn check_opened(pdf: &Pdf, scanned: &scan::Scan, limits: &Limits) -> Result<(), Exceeded> {
    if pdf.len() > limits.max_objects {
        return Err(Exceeded::Objects);
    }
    if pdf.pages().len() > limits.max_pages {
        return Err(Exceeded::Pages);
    }
    if scanned.encrypted {
        let integers = std::collections::HashMap::new();
        for stream in scanned
            .streams
            .iter()
            .filter(|s| !s.checked && !s.structural)
        {
            let Some((number, generation)) = stream.id else {
                continue;
            };
            let id = ObjectIdentifier::new(number, i32::from(generation));
            let Some(found) = pdf.xref().get::<Stream<'_>>(id) else {
                continue;
            };
            let raw = found.raw_data();
            scan::check_stream(&stream.dict, &raw, &integers, limits)?;
        }
    }
    Ok(())
}

/// Stack for threads that call into the parser. Its nesting limits keep
/// recursion to a few hundred levels; this leaves room for unoptimised
/// builds.
pub const STACK_BYTES: usize = 256 << 20;

/// Something a [`Worker`] reports.
#[derive(Debug)]
pub enum Event {
    /// The file is open.
    Opened(Info),
    /// The file could not be opened; the worker has stopped.
    Failed(OpenError),
    /// A page asked for with [`Worker::want`].
    Page(Rendered),
    /// A page that cannot be drawn.
    PageFailed(usize),
}

enum Request {
    Want(Vec<(usize, f32)>),
}

/// A thread that owns one open document and renders its pages on request.
/// Dropping it stops the thread once its current page is done.
pub struct Worker {
    requests: Sender<Request>,
}

impl Worker {
    /// Open `bytes` on a new thread. `events` is called on that thread.
    pub fn spawn(
        bytes: Vec<u8>,
        password: String,
        limits: Limits,
        events: impl FnMut(Event) + Send + 'static,
    ) -> Worker {
        Worker::start(Ok(bytes), password, limits, events)
    }

    /// Read the file at `path` and open it, both on a new thread. A file
    /// larger than `max_file_bytes` is not read.
    pub fn open_file(
        path: PathBuf,
        password: String,
        limits: Limits,
        events: impl FnMut(Event) + Send + 'static,
    ) -> Worker {
        Worker::start(Err(path), password, limits, events)
    }

    fn start(
        source: Result<Vec<u8>, PathBuf>,
        password: String,
        limits: Limits,
        mut events: impl FnMut(Event) + Send + 'static,
    ) -> Worker {
        let (requests, inbox) = mpsc::channel();
        let body = move || {
            let bytes = match source {
                Ok(bytes) => bytes,
                Err(path) => match read(&path, &limits) {
                    Ok(bytes) => bytes,
                    Err(error) => return events(Event::Failed(error)),
                },
            };
            run(bytes, &password, limits, inbox, &mut events)
        };
        let spawned = std::thread::Builder::new()
            .name("pdf".into())
            .stack_size(STACK_BYTES)
            .spawn(body);
        // Without a thread the requests go nowhere and nothing is reported;
        // the caller sees no answer, as for a file that takes long.
        drop(spawned);
        Worker { requests }
    }

    /// Render these pages, in this order, replacing what was asked before.
    /// Pages already sent at the same scale are sent again.
    pub fn want(&self, pages: Vec<(usize, f32)>) {
        let _ = self.requests.send(Request::Want(pages));
    }
}

fn read(path: &Path, limits: &Limits) -> Result<Vec<u8>, OpenError> {
    let size = std::fs::metadata(path)
        .map_err(|e| OpenError::Unreadable(e.kind()))?
        .len();
    if size > limits.max_file_bytes {
        return Err(OpenError::TooLarge(Exceeded::File));
    }
    std::fs::read(path).map_err(|e| OpenError::Unreadable(e.kind()))
}

fn run(
    bytes: Vec<u8>,
    password: &str,
    limits: Limits,
    inbox: Receiver<Request>,
    events: &mut dyn FnMut(Event),
) {
    let document = match catch_unwind(AssertUnwindSafe(|| Document::open(bytes, password, limits)))
    {
        Ok(Ok(document)) => document,
        Ok(Err(error)) => return events(Event::Failed(error)),
        Err(_) => return events(Event::Failed(OpenError::Crashed)),
    };
    events(Event::Opened(document.info().clone()));
    let mut cache = RenderCache::new();
    let mut broken = vec![false; document.info().pages.len()];
    let mut queue: Vec<(usize, f32)> = Vec::new();
    loop {
        // Take the newest request; wait only when there is nothing to do.
        let next = if queue.is_empty() {
            inbox.recv().map_err(|_| RecvTimeoutError::Disconnected)
        } else {
            inbox.recv_timeout(Duration::ZERO)
        };
        match next {
            Ok(Request::Want(pages)) => {
                queue = pages;
                queue.reverse();
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => {}
        }
        let Some((page, scale)) = queue.pop() else {
            continue;
        };
        if broken.get(page).is_none_or(|b| *b) {
            events(Event::PageFailed(page));
            continue;
        }
        match document.render(&cache, page, scale) {
            Some(rendered) => events(Event::Page(rendered)),
            None => {
                broken[page] = true;
                // A panic may leave the caches half-filled.
                cache = RenderCache::new();
                events(Event::PageFailed(page));
            }
        }
    }
}

#[cfg(test)]
mod tests;

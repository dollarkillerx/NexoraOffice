//! PDF tabs: a file opened by an [`aster_pdf::Worker`] and shown by a
//! [`PdfView`]. The worker renders the pages the view asks for; nothing about
//! the file is read on the window's thread.

use std::path::{Path, PathBuf};

use aster_pdf::{Event, Exceeded, Info, Limits, OpenError, Program, Worker};
use aster_surface::pdf_view::{PdfStyle, PdfView, Zoom};

use crate::i18n::{self, Strings};
use crate::office::Key;
use crate::theme;

/// Whether the shell opens this file as a PDF.
pub fn is_pdf(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
}

pub const EXTENSIONS: &[&str] = &["pdf"];

pub struct PdfTab {
    pub view: PdfView,
    pub path: PathBuf,
    /// Which worker's events are this tab's.
    pub token: u64,
    pub info: Info,
    worker: Worker,
    /// What was last asked of the worker, so the same list is not sent on
    /// every frame.
    asked: Vec<(usize, f32)>,
}

impl PdfTab {
    pub fn new(
        path: PathBuf,
        token: u64,
        worker: Worker,
        info: Info,
        tokens: &theme::Tokens,
    ) -> Self {
        let mut view = PdfView::new(info.pages.iter().map(|p| (p.width, p.height)).collect());
        view.set_style(pdf_style(tokens));
        Self {
            view,
            path,
            token,
            info,
            worker,
            asked: Vec::new(),
        }
    }

    pub fn restyle(&mut self, tokens: &theme::Tokens) {
        self.view.set_style(pdf_style(tokens));
    }

    /// Ask the worker for the pages the view needs now, if that changed.
    pub fn request(&mut self) {
        let wanted = self.view.wanted();
        if wanted != self.asked {
            self.worker.want(wanted.clone());
            self.asked = wanted;
        }
    }

    /// A page from the worker. Returns whether the view changed.
    pub fn take(&mut self, event: Event) -> bool {
        match event {
            Event::Page(page) => {
                // Asked again next time if it is still wanted at another scale.
                self.asked.clear();
                self.view
                    .insert(page.page, page.scale, page.width, page.height, page.pixels)
            }
            Event::PageFailed(page) => {
                self.asked.clear();
                self.view.mark_failed(page);
                true
            }
            Event::Opened(_) | Event::Failed(_) => false,
        }
    }

    /// The zoom commands: steps up and down, 100 %, and fit width.
    pub fn zoom(&mut self, zoom_in: Option<bool>) -> bool {
        match zoom_in {
            Some(true) => self.view.zoom_in(),
            Some(false) => self.view.zoom_out(),
            None => self.view.set_zoom(Zoom::Percent(100.0)),
        }
    }

    /// "Fit width", or "fit page" when the width is fitted already.
    pub fn fit(&mut self) -> bool {
        let next = if self.view.zoom() == Zoom::FitWidth {
            Zoom::FitPage
        } else {
            Zoom::FitWidth
        };
        self.view.set_zoom(next)
    }
}

/// Start opening `path` in a child process; events arrive through
/// `events`, on the supervising thread. If this program's own path cannot be
/// found, the failure is reported through `events` and there is no worker.
pub fn open(
    path: PathBuf,
    password: String,
    mut events: impl FnMut(Event) + Send + 'static,
) -> Option<Worker> {
    match Program::current() {
        Ok(program) => Some(Worker::open(
            program,
            path,
            password,
            Limits::default(),
            events,
        )),
        Err(_) => {
            std::thread::spawn(move || events(Event::Failed(OpenError::Crashed)));
            None
        }
    }
}

pub fn pdf_style(tokens: &theme::Tokens) -> PdfStyle {
    let c = &tokens.colors;
    let opaque = |color: theme::Rgba, under: theme::Rgba| color.over(under).to_array();
    PdfStyle {
        background: opaque(c.desk, c.surface),
        strip: opaque(c.panel, c.surface),
        hairline: opaque(c.hairline, c.surface),
        secondary: opaque(c.text_secondary, c.panel),
        accent: c.accent.to_array(),
    }
}

/// Keys on a PDF: arrows and the page keys scroll, Home and End go to the
/// first and last page, the space bar pages down (up with Shift).
pub fn pdf_key(view: &mut PdfView, key: Key, shift: bool, scale: f64) -> bool {
    let line = aster_surface::pdf_view::LINE_STEP * scale;
    match key {
        Key::Up => view.wheel(f64::MAX, 0.0, -line),
        Key::Down => view.wheel(f64::MAX, 0.0, line),
        Key::Left => view.wheel(f64::MAX, -line, 0.0),
        Key::Right => view.wheel(f64::MAX, line, 0.0),
        Key::PageUp => view.page_down(false),
        Key::PageDown | Key::Enter | Key::Tab => view.page_down(true),
        Key::Space => view.page_down(!shift),
        Key::Home => view.first_page(),
        Key::End => view.last_page(),
    }
}

/// What to tell the user when a file does not open (not for a password
/// that is needed or wrong: those are asked for).
pub fn error_message(error: OpenError, strings: &Strings) -> String {
    match error {
        OpenError::Damaged | OpenError::NeedsPassword | OpenError::WrongPassword => {
            strings.pdf_error_damaged.to_owned()
        }
        OpenError::UnsupportedEncryption => strings.pdf_error_encryption.to_owned(),
        OpenError::Crashed => strings.pdf_error_crashed.to_owned(),
        OpenError::Unchecked => strings.pdf_error_unchecked.to_owned(),
        OpenError::Unreadable(kind) => i18n::fill(
            strings.pdf_error_unreadable,
            "reason",
            &std::io::Error::from(kind).to_string(),
        ),
        OpenError::TooLarge(what) => {
            let what = match what {
                Exceeded::File => strings.pdf_limit_file,
                Exceeded::Objects => strings.pdf_limit_objects,
                Exceeded::Pages => strings.pdf_limit_pages,
                Exceeded::Stream => strings.pdf_limit_stream,
                Exceeded::Image => strings.pdf_limit_image,
                Exceeded::Memory => strings.pdf_limit_memory,
                Exceeded::Time => strings.pdf_limit_time,
            };
            i18n::fill(strings.pdf_error_too_large, "what", what)
        }
    }
}

/// A page number typed into "Go to page": 1-based, within the document.
/// Returns the 0-based index.
pub fn parse_page(text: &str, count: usize) -> Option<usize> {
    let number: usize = text.trim().parse().ok()?;
    (1..=count).contains(&number).then(|| number - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pdf_files_by_extension() {
        assert!(is_pdf(Path::new("报告.PDF")));
        assert!(is_pdf(Path::new("/a/b.pdf")));
        assert!(!is_pdf(Path::new("pdf")));
        assert!(!is_pdf(Path::new("a.pdfx")));
    }

    #[test]
    fn page_numbers() {
        assert_eq!(parse_page(" 3 ", 10), Some(2));
        assert_eq!(parse_page("1", 1), Some(0));
        assert_eq!(parse_page("0", 10), None);
        assert_eq!(parse_page("11", 10), None);
        assert_eq!(parse_page("二", 10), None);
    }

    #[test]
    fn every_error_has_words_in_every_language() {
        let errors = [
            OpenError::Damaged,
            OpenError::Crashed,
            OpenError::Unchecked,
            OpenError::UnsupportedEncryption,
            OpenError::Unreadable(std::io::ErrorKind::PermissionDenied),
            OpenError::TooLarge(Exceeded::File),
            OpenError::TooLarge(Exceeded::Objects),
            OpenError::TooLarge(Exceeded::Pages),
            OpenError::TooLarge(Exceeded::Stream),
            OpenError::TooLarge(Exceeded::Image),
            OpenError::TooLarge(Exceeded::Memory),
            OpenError::TooLarge(Exceeded::Time),
        ];
        for language in crate::i18n::Language::ALL {
            let strings = language.strings();
            for error in errors {
                let message = error_message(error, strings);
                assert!(!message.is_empty() && !message.contains('{'), "{message}");
            }
        }
    }

    #[test]
    fn keys_move_the_view() {
        let mut view = PdfView::new(vec![(612.0, 792.0); 10]);
        view.resize(1000, 800, 1.0);
        assert!(pdf_key(&mut view, Key::Down, false, 1.0));
        assert!(pdf_key(&mut view, Key::End, false, 1.0));
        assert_eq!(view.current(), 9);
        assert!(pdf_key(&mut view, Key::Home, false, 1.0));
        assert_eq!(view.current(), 0);
        assert!(pdf_key(&mut view, Key::Space, false, 1.0));
        assert!(pdf_key(&mut view, Key::Space, true, 1.0));
    }
}

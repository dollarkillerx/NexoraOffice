//! The document surface: the part of the window that the shell does not draw.
//!
//! [`text_surface`] owns text, layout and pixels and knows nothing about the GUI
//! toolkit. [`host`] is the glue that feeds it from a Slint window: key decoding,
//! input-method events taken from winit, and handing rendered frames back.
//!
//! For now the only surface is the plain-text one that came out of the Phase 0
//! input-method gate. The editors will plug in here through the `EditorView`
//! contract once `aster-model` and `aster-render` exist.

pub mod cell_editor;
mod chart_draw;
mod cond_icons;
mod glyphs;
pub mod host;
pub mod md_picture;
pub mod pdf;
pub mod pdf_view;
mod picture;
pub mod preview;
pub mod sheet_view;
pub mod slide_view;
pub mod text_surface;
pub mod text_view;

pub use text_surface::{
    Action, Decoration, HAN_FALLBACK_FAMILIES, ImeEvent, PageLayout, Palette, Rgba8, TextStyle,
    TextSurface, ZOOM_RANGE,
};
pub use text_view::TextView;

use vello_cpu::Pixmap;

/// What the shell asks of a document surface, whichever kind it is: the
/// whole-text [`TextSurface`] of Writer tabs or the [`TextView`] of Markdown
/// tabs, which lays out only what is on screen.
pub trait Surface {
    /// Resize the backing buffer. Sizes are physical pixels.
    fn resize(&mut self, width: u16, height: u16, scale: f32);
    fn size(&self) -> (u16, u16, f32);
    fn set_caret_on(&mut self, on: bool);
    /// Draw and return the pixels, premultiplied RGBA8.
    fn render(&mut self) -> &Pixmap;
    /// Scroll by physical pixels; returns whether the view moved.
    fn scroll_by(&mut self, delta: f64) -> bool;
    /// Scroll offset and its upper bound, physical pixels.
    fn scroll_range(&mut self) -> (f64, f64);
    fn is_composing(&self) -> bool;
    /// Where the candidate window goes, physical pixels relative to the surface.
    fn ime_cursor_area(&mut self) -> (f64, f64, f64, f64);
    /// The whole committed text, copied. For tests and small documents.
    fn text(&self) -> String;
}

macro_rules! surface {
    ($type:ty) => {
        impl Surface for $type {
            fn resize(&mut self, width: u16, height: u16, scale: f32) {
                <$type>::resize(self, width, height, scale)
            }
            fn size(&self) -> (u16, u16, f32) {
                <$type>::size(self)
            }
            fn set_caret_on(&mut self, on: bool) {
                <$type>::set_caret_on(self, on)
            }
            fn render(&mut self) -> &Pixmap {
                <$type>::render(self)
            }
            fn scroll_by(&mut self, delta: f64) -> bool {
                <$type>::scroll_by(self, delta)
            }
            fn scroll_range(&mut self) -> (f64, f64) {
                <$type>::scroll_range(self)
            }
            fn is_composing(&self) -> bool {
                <$type>::is_composing(self)
            }
            fn ime_cursor_area(&mut self) -> (f64, f64, f64, f64) {
                <$type>::ime_cursor_area(self)
            }
            fn text(&self) -> String {
                <$type>::text(self)
            }
        }
    };
}

surface!(TextSurface);
surface!(TextView);

/// Read-only views take no text input; scrolling is vertical.
macro_rules! viewer {
    ($type:ty, $scroll:expr) => {
        impl Surface for $type {
            fn resize(&mut self, width: u16, height: u16, scale: f32) {
                <$type>::resize(self, width, height, scale)
            }
            fn size(&self) -> (u16, u16, f32) {
                <$type>::size(self)
            }
            fn set_caret_on(&mut self, _on: bool) {}
            fn render(&mut self) -> &Pixmap {
                <$type>::render(self)
            }
            fn scroll_by(&mut self, delta: f64) -> bool {
                #[allow(clippy::redundant_closure_call)]
                ($scroll)(self, delta)
            }
            fn scroll_range(&mut self) -> (f64, f64) {
                <$type>::scroll_range(self)
            }
            fn is_composing(&self) -> bool {
                false
            }
            fn ime_cursor_area(&mut self) -> (f64, f64, f64, f64) {
                (0.0, 0.0, 0.0, 0.0)
            }
            fn text(&self) -> String {
                <$type>::text(self)
            }
        }
    };
}

impl Surface for sheet_view::SheetView {
    fn resize(&mut self, width: u16, height: u16, scale: f32) {
        sheet_view::SheetView::resize(self, width, height, scale)
    }
    fn size(&self) -> (u16, u16, f32) {
        sheet_view::SheetView::size(self)
    }
    fn set_caret_on(&mut self, on: bool) {
        sheet_view::SheetView::set_caret_on(self, on)
    }
    fn render(&mut self) -> &Pixmap {
        sheet_view::SheetView::render(self)
    }
    fn scroll_by(&mut self, delta: f64) -> bool {
        sheet_view::SheetView::scroll_by(self, 0.0, delta)
    }
    fn scroll_range(&mut self) -> (f64, f64) {
        sheet_view::SheetView::scroll_range(self)
    }
    fn is_composing(&self) -> bool {
        sheet_view::SheetView::is_composing(self)
    }
    fn ime_cursor_area(&mut self) -> (f64, f64, f64, f64) {
        sheet_view::SheetView::ime_cursor_area(self)
    }
    fn text(&self) -> String {
        sheet_view::SheetView::text(self)
    }
}
viewer!(pdf_view::PdfView, |view: &mut pdf_view::PdfView, delta| {
    view.scroll_by(delta)
});
impl Surface for slide_view::SlideView {
    fn resize(&mut self, width: u16, height: u16, scale: f32) {
        slide_view::SlideView::resize(self, width, height, scale)
    }
    fn size(&self) -> (u16, u16, f32) {
        slide_view::SlideView::size(self)
    }
    fn set_caret_on(&mut self, on: bool) {
        slide_view::SlideView::set_caret_on(self, on)
    }
    fn render(&mut self) -> &Pixmap {
        slide_view::SlideView::render(self)
    }
    fn scroll_by(&mut self, delta: f64) -> bool {
        let x = f64::from(self.size().0);
        self.wheel(x, delta)
    }
    fn scroll_range(&mut self) -> (f64, f64) {
        slide_view::SlideView::scroll_range(self)
    }
    fn is_composing(&self) -> bool {
        slide_view::SlideView::is_composing(self)
    }
    fn ime_cursor_area(&mut self) -> (f64, f64, f64, f64) {
        slide_view::SlideView::ime_cursor_area(self)
    }
    fn text(&self) -> String {
        slide_view::SlideView::text(self)
    }
}

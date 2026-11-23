//! The document surface: the part of the window that the shell does not draw.
//!
//! [`text_surface`] owns text, layout and pixels and knows nothing about the GUI
//! toolkit. [`host`] is the glue that feeds it from a Slint window: key decoding,
//! input-method events taken from winit, and handing rendered frames back.
//!
//! For now the only surface is the plain-text one that came out of the Phase 0
//! input-method gate. The editors will plug in here through the `EditorView`
//! contract once `aster-model` and `aster-render` exist.

mod glyphs;
pub mod host;
pub mod preview;
pub mod text_surface;

pub use text_surface::{
    Action, Decoration, HAN_FALLBACK_FAMILIES, ImeEvent, PageLayout, Palette, Rgba8, TextStyle,
    TextSurface, ZOOM_RANGE,
};

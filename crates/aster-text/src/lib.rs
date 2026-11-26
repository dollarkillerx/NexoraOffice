//! Text layout for Aster Office: the first piece of the layout kernel.
//!
//! The technical plan gives this crate fonts, shaping, paragraph layout, the
//! Chinese and Japanese line-breaking rules and hit testing. What is here so
//! far:
//!
//! - [`breaker`]: our own line breaker, from the Phase 0 gate G3. It keeps
//!   closing marks off the start of a line and opening marks off the end,
//!   compresses and hangs punctuation at the end of a line, squeezes adjacent
//!   full-width marks, and spaces Han and kana from Latin text. All in
//!   integer arithmetic, so the same input breaks the same on every system.
//! - [`segment`]: where UAX #14 in strict mode allows a break.
//! - [`paragraph`]: a paragraph shaped by Parley and broken by the breaker,
//!   with hit testing and selection rectangles. It does not draw.
//! - [`selection`]: a selection across many paragraphs, and its plain text.
//! - [`golden`]: Chinese and Japanese samples and their expected breaks.
//!
//! Nothing here depends on the GUI toolkit.

pub mod breaker;
pub mod class;
pub mod golden;
pub mod paragraph;
pub mod segment;
pub mod selection;

pub use breaker::{BreakKind, EndTreatment, Fx, Leader, TabAlign};
pub use paragraph::{
    Align, Decor, Decoration, DrawRun, Extra, Face, Family, Glyph, InlineObject, LineBox, LineRule,
    Paragraph, Rgba, Rules, Shaper, Spacing, Span, Spec, Stroke, Style, TabGap, TabStop,
};
pub use selection::{Selection, TextPoint};

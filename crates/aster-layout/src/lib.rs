//! Page layout of Writer documents.
//!
//! Takes an [`aster_writer::Document`] and gives pages: what is drawn where,
//! and where every place in the text is. Lines are broken by `aster-text`.
//! Nothing here draws or knows about the GUI toolkit; `aster-surface` paints
//! the pages.
//!
//! ```ignore
//! let mut layouter = Layouter::new();
//! let layout = layouter.layout(&document);   // pages of display items
//! let caret = layout.caret(&pos);             // where a place is
//! let pos = layout.hit(page, x, y);           // which place is at a point
//! ```
//!
//! A [`Layouter`] keeps what it shaped. Blocks are behind `Arc`s, so after
//! an edit the unchanged ones are the same `Arc` and are not shaped again.
//!
//! Units are points (1/72 inch), the origin is a page's top-left corner and
//! y grows downwards. Word's behaviour is the reference; where this crate
//! simplifies, the module that does says so (see also the crate README).

mod cx;
mod display;
mod flow;
pub mod fonts;
mod query;
mod shape;
mod table;

pub use cx::Layouter;
pub use display::{
    FrameItem, Item, LeaderItem, Page, PictureItem, Rect, Rgba, RuleStyle, TextItem,
};
pub use query::{Caret, Layout, OutlineEntry};

#[cfg(test)]
mod tests;

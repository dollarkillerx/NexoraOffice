//! The Markdown editor's document, without any drawing.
//!
//! A Markdown file is text, so the model is a text buffer with a selection, the
//! commands that change it, an undo history, and the rules for reading and
//! writing the file. Parsing, highlighting and preview are not here yet.
//!
//! Nothing in this crate knows about windows, layout or the GUI toolkit. What
//! needs layout — moving the caret up a line, finding the character under the
//! pointer — is done by the view, which then tells the document where the
//! selection is.

pub mod document;
pub mod file;
mod history;

pub use document::{Change, Document, EditCommand, SavePoint, Selection};
pub use file::{DecodeError, Decoded, LineEnding, TextFormat, decode, encode};

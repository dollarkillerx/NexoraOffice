//! The Markdown editor's document, without any drawing.
//!
//! A Markdown file is text, so the model is a text buffer with a selection, the
//! commands that change it, an undo history, and the rules for reading and
//! writing the file, and the encoding of its edits in a recovery journal.
//! Markdown itself is read by `pulldown-cmark`: `highlight` colours the source
//! as it is edited, and `preview` turns it into the structure the preview
//! draws.
//!
//! Nothing in this crate knows about windows, layout or the GUI toolkit. What
//! needs layout — moving the caret up a line, finding the character under the
//! pointer — is done by the view, which then tells the document where the
//! selection is.

pub mod code;
pub mod document;
pub mod export;
pub mod file;
pub mod format;
pub mod highlight;
mod history;
pub mod journal;
pub mod preview;
pub mod search;
pub mod structure;
pub mod syntax;
pub mod table;

pub use document::{Change, Document, EditCommand, SavePoint, Selection};
pub use file::{
    DecodeError, Decoded, EncodeError, Encoding, LineEnding, TextFormat, decode, decode_as, encode,
    encode_rope, guess_encodings,
};
pub use ropey::Rope;

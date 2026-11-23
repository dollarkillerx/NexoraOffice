//! The files around the documents.
//!
//! The list of recently opened files ([`recent`]), the folder shown in the
//! sidebar as a tree that is read a folder at a time ([`tree`]), what of it is
//! remembered between runs ([`state`]), and the thread that writes those
//! small files ([`store`]). Nothing here knows about the GUI toolkit; the
//! shell turns the models into rows on screen.
//!
//! Only paths and times are kept, never what the files contain.

pub mod names;
pub mod recent;
pub mod state;
pub mod store;
pub mod tree;

mod escape;

pub use names::{Filter, natural_cmp};
pub use recent::{MAX_RECENT, RecentFile, RecentFiles};
pub use state::WorkspaceState;
pub use store::Store;
pub use tree::{
    Entry, EntryKind, FileKind, FileTree, LIST_LIMIT, Listing, Note, ReadError, Row, RowKind,
    read_folder,
};

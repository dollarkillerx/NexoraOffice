//! What the application asks of the operating system.
//!
//! The native open and save dialogs, opening web and e-mail links, showing a
//! file in the file manager, making, renaming and trashing files, and where the application keeps its own files. Clipboard, printing, system
//! fonts and the macOS title-bar fix-up in `aster-shell` belong here too and
//! move in as they are needed. Nothing in this crate knows about the GUI
//! toolkit.

pub mod clipboard;
pub mod dialogs;
pub mod files;
pub mod links;
pub mod locale;
pub mod open_requests;
pub mod paths;
pub mod reveal;
pub mod volumes;

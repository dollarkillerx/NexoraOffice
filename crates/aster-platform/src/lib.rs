//! What the application asks of the operating system.
//!
//! The native open and save dialogs, and where the application keeps its own
//! files. Clipboard, printing, system
//! fonts and the macOS title-bar fix-up in `aster-shell` belong here too and
//! move in as they are needed. Nothing in this crate knows about the GUI
//! toolkit.

pub mod dialogs;
pub mod paths;

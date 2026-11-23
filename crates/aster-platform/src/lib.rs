//! What the application asks of the operating system.
//!
//! Only the native open and save dialogs so far. Clipboard, printing, system
//! fonts and the macOS title-bar fix-up in `aster-shell` belong here too and
//! move in as they are needed. Nothing in this crate knows about the GUI
//! toolkit.

pub mod dialogs;

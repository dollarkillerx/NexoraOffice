//! Native file dialogs.
//!
//! The calls block until the user answers. They must be made from the main
//! thread: macOS requires it, and the dialog is modal to the application
//! everywhere, so nothing else would be handled in the meantime anyway.

use std::path::{Path, PathBuf};

/// A group of file extensions offered in a dialog's type filter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileType {
    /// Shown to the user.
    pub name: &'static str,
    /// Without the dot.
    pub extensions: &'static [&'static str],
}

/// Ask for an existing file to open. `None` if the user cancelled.
pub fn pick_file_to_open(title: &str, types: &[FileType]) -> Option<PathBuf> {
    let mut dialog = rfd::FileDialog::new().set_title(title);
    for file_type in types {
        dialog = dialog.add_filter(file_type.name, file_type.extensions);
    }
    dialog.pick_file()
}

/// Ask where to save. `suggested` pre-fills the folder and the name: a path the
/// document already has, or just a file name for a new one. `None` if the user
/// cancelled. The system asks about replacing an existing file itself.
pub fn pick_file_to_save(title: &str, suggested: &Path, types: &[FileType]) -> Option<PathBuf> {
    let mut dialog = rfd::FileDialog::new().set_title(title);
    for file_type in types {
        dialog = dialog.add_filter(file_type.name, file_type.extensions);
    }
    if let Some(name) = suggested.file_name() {
        dialog = dialog.set_file_name(name.to_string_lossy());
    }
    if let Some(dir) = suggested.parent().filter(|dir| dir.is_dir()) {
        dialog = dialog.set_directory(dir);
    }
    dialog.save_file()
}

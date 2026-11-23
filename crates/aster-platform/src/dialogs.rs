//! Native file dialogs.
//!
//! The dialogs are futures: await them in a task started with
//! `slint::spawn_local`, never block on them inside an event handler. A
//! blocking dialog runs a nested event loop while the handler is still on the
//! stack, and on macOS winit aborts the process as soon as that loop delivers
//! another event ("tried to handle event while another event is currently
//! being handled"). Asynchronously, macOS shows the panel as a sheet on the
//! main window and returns at once; the answer arrives through the event loop.

use std::path::{Path, PathBuf};

/// A group of file extensions offered in a dialog's type filter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileType {
    /// Shown to the user.
    pub name: &'static str,
    /// Without the dot.
    pub extensions: &'static [&'static str],
}

fn dialog(title: &str, types: &[FileType]) -> rfd::AsyncFileDialog {
    let mut dialog = rfd::AsyncFileDialog::new().set_title(title);
    for file_type in types {
        dialog = dialog.add_filter(file_type.name, file_type.extensions);
    }
    dialog
}

/// Ask for an existing file to open. `None` if the user cancelled.
pub async fn pick_file_to_open(title: &str, types: &[FileType]) -> Option<PathBuf> {
    let chosen = dialog(title, types).pick_file().await?;
    Some(chosen.path().to_path_buf())
}

/// Ask for a folder. `None` if the user cancelled.
pub async fn pick_folder(title: &str, start: Option<&Path>) -> Option<PathBuf> {
    let mut dialog = rfd::AsyncFileDialog::new().set_title(title);
    if let Some(dir) = start.filter(|dir| dir.is_dir()) {
        dialog = dialog.set_directory(dir);
    }
    let chosen = dialog.pick_folder().await?;
    Some(chosen.path().to_path_buf())
}

/// Ask where to save. `suggested` pre-fills the folder and the name: a path the
/// document already has, or just a file name for a new one. `None` if the user
/// cancelled. The system asks about replacing an existing file itself.
pub async fn pick_file_to_save(
    title: &str,
    suggested: &Path,
    types: &[FileType],
) -> Option<PathBuf> {
    let mut dialog = dialog(title, types);
    if let Some(name) = suggested.file_name() {
        dialog = dialog.set_file_name(name.to_string_lossy());
    }
    if let Some(dir) = suggested.parent().filter(|dir| dir.is_dir()) {
        dialog = dialog.set_directory(dir);
    }
    let chosen = dialog.save_file().await?;
    Some(chosen.path().to_path_buf())
}

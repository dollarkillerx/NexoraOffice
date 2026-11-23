//! Where the application keeps its own files.

use std::path::PathBuf;

/// The application's identifier on macOS, and the name of its folders
/// elsewhere. A placeholder until the project owns a domain: it must change
/// together with `APP_ID` in `packaging/common.sh`.
pub const APP_ID: &str = "dev.aster.office.preview";
/// The folder name on Windows and Linux.
const APP_DIR: &str = "aster-office";
/// Overrides the folder, for tests and for running two copies side by side.
pub const DATA_DIR_ENV: &str = "ASTER_DATA_DIR";

/// The folder for the application's own state, such as recovery records. Not
/// a place for the user's documents.
///
/// - macOS: `~/Library/Application Support/<APP_ID>`
/// - Windows: `%LOCALAPPDATA%\aster-office`, which stays on this machine
/// - Linux and others: `$XDG_STATE_HOME/aster-office`, by default
///   `~/.local/state/aster-office`, the folder the XDG specification gives to
///   state that should survive a restart
///
/// `None` when the environment gives no home folder to start from.
pub fn data_dir() -> Option<PathBuf> {
    let var = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
    if let Some(dir) = var(DATA_DIR_ENV) {
        return Some(PathBuf::from(dir));
    }
    if cfg!(target_os = "macos") {
        let home = PathBuf::from(var("HOME")?);
        Some(
            home.join("Library")
                .join("Application Support")
                .join(APP_ID),
        )
    } else if cfg!(windows) {
        Some(PathBuf::from(var("LOCALAPPDATA")?).join(APP_DIR))
    } else {
        let state = match var("XDG_STATE_HOME").map(PathBuf::from) {
            // The specification says to ignore a relative path.
            Some(dir) if dir.is_absolute() => dir,
            _ => PathBuf::from(var("HOME")?).join(".local").join("state"),
        };
        Some(state.join(APP_DIR))
    }
}

/// The folder recovery records are kept in.
pub fn recovery_dir() -> Option<PathBuf> {
    Some(data_dir()?.join("recovery"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_folder_is_per_application_and_absolute() {
        // The environment of the test run decides the exact answer; what holds
        // everywhere is the shape.
        let Some(dir) = data_dir() else {
            return;
        };
        if std::env::var_os(DATA_DIR_ENV).is_none() {
            assert!(dir.is_absolute(), "{dir:?}");
            let name = dir.file_name().unwrap().to_string_lossy().into_owned();
            assert!(name == APP_ID || name == APP_DIR, "{dir:?}");
        }
        assert_eq!(recovery_dir().unwrap(), dir.join("recovery"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn on_macos_it_is_under_application_support() {
        if std::env::var_os(DATA_DIR_ENV).is_some() {
            return;
        }
        let dir = data_dir().unwrap();
        assert!(dir.ends_with(format!("Library/Application Support/{APP_ID}")));
    }
}

//! What the sidebar remembers between runs: the folder it shows and which of
//! its folders were expanded.

use std::io;
use std::path::{Path, PathBuf};

use crate::escape;

const HEADER: &str = "aster-workspace 1";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkspaceState {
    pub folder: Option<PathBuf>,
    /// Expanded folders, as absolute paths inside `folder`.
    pub expanded: Vec<PathBuf>,
}

impl WorkspaceState {
    pub fn encode(&self) -> String {
        let mut out = String::from(HEADER);
        out.push('\n');
        let Some(folder) = self.folder.as_deref().and_then(escape::path) else {
            return out;
        };
        out.push_str(&format!("folder\t{folder}\n"));
        for path in &self.expanded {
            if let Some(path) = escape::path(path) {
                out.push_str(&format!("expanded\t{path}\n"));
            }
        }
        out
    }

    /// Read stored state. What cannot be read is dropped, and so are expanded
    /// folders outside the folder.
    pub fn decode(text: &str) -> Self {
        let mut lines = text.lines();
        let mut state = Self::default();
        if lines.next() != Some(HEADER) {
            return state;
        }
        for line in lines {
            let Some((key, value)) = line.split_once('\t') else {
                continue;
            };
            let Some(path) = escape::unpath(value).filter(|path| path.is_absolute()) else {
                continue;
            };
            match key {
                "folder" if state.folder.is_none() => state.folder = Some(path),
                "expanded" => state.expanded.push(path),
                _ => {}
            }
        }
        match &state.folder {
            Some(folder) => state.expanded.retain(|path| path.starts_with(folder)),
            None => state.expanded.clear(),
        }
        state
    }

    /// Read the state from `file`. A file that does not exist is no state.
    pub fn load(file: &Path) -> io::Result<Self> {
        match std::fs::read(file) {
            Ok(bytes) => Ok(Self::decode(&String::from_utf8_lossy(&bytes))),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(err),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_folder_and_its_expanded_folders_survive_storage() {
        let folder = std::env::temp_dir().join("工作区");
        let state = WorkspaceState {
            folder: Some(folder.clone()),
            expanded: vec![folder.join("2026"), folder.join("2026").join("経営")],
        };
        assert_eq!(WorkspaceState::decode(&state.encode()), state);
    }

    #[test]
    fn expanded_folders_elsewhere_are_dropped() {
        let text = format!(
            "{HEADER}\nfolder\t/work\nexpanded\t/work/a\nexpanded\t/elsewhere\nexpanded\trelative\nbogus\n"
        );
        let state = WorkspaceState::decode(&text);
        assert_eq!(state.folder, Some(PathBuf::from("/work")));
        assert_eq!(state.expanded, [PathBuf::from("/work/a")]);
        assert_eq!(
            WorkspaceState::decode("other\nfolder\t/work\n"),
            WorkspaceState::default()
        );
        let none = WorkspaceState::default();
        assert_eq!(WorkspaceState::decode(&none.encode()), none);
    }
}

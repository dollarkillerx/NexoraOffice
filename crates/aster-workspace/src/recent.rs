//! The files opened or saved most recently, newest first.
//!
//! Kept in a small text file in the application's data folder: a header line,
//! then one line per file with the time in seconds since 1970 and the path.
//! A damaged line is skipped rather than losing the whole list.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::escape;

/// How many files the list keeps.
pub const MAX_RECENT: usize = 30;
const HEADER: &str = "aster-recent 1";

/// One file in the list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecentFile {
    pub path: PathBuf,
    /// When it was last opened or saved.
    pub opened: SystemTime,
    /// The file was not there when last looked for. Set by
    /// [`RecentFiles::set_missing`], never stored.
    pub missing: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecentFiles {
    files: Vec<RecentFile>,
}

impl RecentFiles {
    /// Newest first.
    pub fn files(&self) -> &[RecentFile] {
        &self.files
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Note that `path` was opened or saved at `at`: it moves to the top. The
    /// caller passes the path in one spelling, ideally the canonical one, so
    /// the same file is not listed twice. Relative paths are ignored.
    pub fn add(&mut self, path: &Path, at: SystemTime) -> bool {
        if !path.is_absolute() {
            return false;
        }
        self.files.retain(|file| file.path != path);
        self.files.insert(
            0,
            RecentFile {
                path: path.to_owned(),
                opened: at,
                missing: false,
            },
        );
        self.files.truncate(MAX_RECENT);
        true
    }

    /// Take a file off the list. Returns whether it was there.
    pub fn remove(&mut self, path: &Path) -> bool {
        let before = self.files.len();
        self.files.retain(|file| file.path != path);
        self.files.len() != before
    }

    pub fn clear(&mut self) {
        self.files.clear();
    }

    /// The paths to look for, in list order, for [`present`] on another thread.
    pub fn paths(&self) -> Vec<PathBuf> {
        self.files.iter().map(|file| file.path.clone()).collect()
    }

    /// Record which files were found, as [`present`] reported them. Paths no
    /// longer on the list are ignored. Returns whether anything changed.
    pub fn set_missing(&mut self, found: &[(PathBuf, bool)]) -> bool {
        let mut changed = false;
        for (path, present) in found {
            if let Some(file) = self.files.iter_mut().find(|file| &file.path == path)
                && file.missing == *present
            {
                file.missing = !present;
                changed = true;
            }
        }
        changed
    }

    /// The list as stored. Paths that are not valid Unicode are left out.
    pub fn encode(&self) -> String {
        let mut out = String::from(HEADER);
        out.push('\n');
        for file in &self.files {
            let Some(path) = escape::path(&file.path) else {
                continue;
            };
            let seconds = file
                .opened
                .duration_since(UNIX_EPOCH)
                .map_or(0, |since| since.as_secs());
            out.push_str(&format!("{seconds}\t{path}\n"));
        }
        out
    }

    /// Read a stored list. Lines that cannot be read are skipped; a file of
    /// another format gives an empty list.
    pub fn decode(text: &str) -> Self {
        let mut lines = text.lines();
        if lines.next() != Some(HEADER) {
            return Self::default();
        }
        let mut list = Self::default();
        for line in lines {
            let Some((seconds, path)) = line.split_once('\t') else {
                continue;
            };
            let (Ok(seconds), Some(path)) = (seconds.parse::<u64>(), escape::unpath(path)) else {
                continue;
            };
            if !path.is_absolute() || list.files.iter().any(|file| file.path == path) {
                continue;
            }
            list.files.push(RecentFile {
                path,
                opened: UNIX_EPOCH + Duration::from_secs(seconds),
                missing: false,
            });
            if list.files.len() == MAX_RECENT {
                break;
            }
        }
        list
    }

    /// Read the list from `file`. A file that does not exist is an empty list.
    pub fn load(file: &Path) -> io::Result<Self> {
        match std::fs::read(file) {
            Ok(bytes) => Ok(Self::decode(&String::from_utf8_lossy(&bytes))),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(err),
        }
    }
}

/// Which of `paths` are files that are still there. It touches the disk, so it
/// runs on a worker thread. A path that is now a folder counts as missing.
pub fn present(paths: Vec<PathBuf>) -> Vec<(PathBuf, bool)> {
    paths
        .into_iter()
        .map(|path| {
            let present = path.is_file();
            (path, present)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(seconds)
    }

    fn root() -> PathBuf {
        std::env::temp_dir()
    }

    #[test]
    fn newest_first_without_duplicates_and_capped() {
        let mut list = RecentFiles::default();
        for i in 0..40 {
            assert!(list.add(&root().join(format!("{i}.md")), at(i)));
        }
        assert_eq!(list.files().len(), MAX_RECENT);
        assert_eq!(list.files()[0].path, root().join("39.md"));
        // Opening an older file again moves it to the top, once.
        list.add(&root().join("20.md"), at(100));
        assert_eq!(list.files()[0].path, root().join("20.md"));
        assert_eq!(list.files()[0].opened, at(100));
        assert_eq!(list.files().len(), MAX_RECENT);
        assert_eq!(
            list.files()
                .iter()
                .filter(|f| f.path == root().join("20.md"))
                .count(),
            1
        );
        assert!(!list.add(Path::new("relative.md"), at(1)));
    }

    #[test]
    fn removing_and_clearing() {
        let mut list = RecentFiles::default();
        list.add(&root().join("a.md"), at(1));
        list.add(&root().join("b.md"), at(2));
        assert!(list.remove(&root().join("a.md")));
        assert!(!list.remove(&root().join("a.md")));
        assert_eq!(list.paths(), [root().join("b.md")]);
        list.clear();
        assert!(list.is_empty());
    }

    #[test]
    fn chinese_and_japanese_paths_survive_storage() {
        let mut list = RecentFiles::default();
        for name in [
            "会议纪要 11-20.md",
            "議事録（第３回）.md",
            "tab\there.md",
            "👍.txt",
        ] {
            list.add(&root().join("文稿").join(name), at(1_800_000_000));
        }
        let decoded = RecentFiles::decode(&list.encode());
        assert_eq!(decoded, list);
    }

    #[test]
    fn a_damaged_list_keeps_what_it_can() {
        let text = format!(
            "{HEADER}\n\
             1700000000\t/ok/one.md\n\
             not a number\t/bad/time.md\n\
             1700000001\trelative.md\n\
             1700000002\t/bad/escape\\x.md\n\
             no tab at all\n\
             1700000003\t/ok/one.md\n\
             1700000004\t/ok/two.md\n"
        );
        let list = RecentFiles::decode(&text);
        assert_eq!(
            list.paths(),
            [PathBuf::from("/ok/one.md"), PathBuf::from("/ok/two.md")]
        );
        assert!(RecentFiles::decode("something else\n1\t/a.md\n").is_empty());
        assert!(RecentFiles::decode("").is_empty());
    }

    #[test]
    fn missing_files_are_found_and_marked() {
        let dir = tempfile::tempdir().unwrap();
        let here = dir.path().join("在这里.md");
        let gone = dir.path().join("消えた.md");
        let folder = dir.path().join("folder.md");
        std::fs::write(&here, "x").unwrap();
        std::fs::create_dir(&folder).unwrap();
        let mut list = RecentFiles::default();
        list.add(&here, at(1));
        list.add(&gone, at(2));
        list.add(&folder, at(3));
        let found = present(list.paths());
        assert!(list.set_missing(&found));
        let missing: Vec<bool> = list.files().iter().map(|f| f.missing).collect();
        assert_eq!(missing, [true, true, false]);
        // Nothing new: nothing changed.
        assert!(!list.set_missing(&found));
        // The file comes back.
        std::fs::write(&gone, "y").unwrap();
        assert!(list.set_missing(&present(list.paths())));
        assert!(!list.files()[1].missing);
        // Missing is not stored.
        assert!(
            RecentFiles::decode(&list.encode())
                .files()
                .iter()
                .all(|f| !f.missing)
        );
    }

    #[test]
    fn loading_a_list_that_was_never_written_gives_an_empty_one() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            RecentFiles::load(&dir.path().join("recent"))
                .unwrap()
                .is_empty()
        );
    }
}

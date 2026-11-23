//! The folder of recovery records, and who owns each record.
//!
//! Every document with unsaved changes has a record: a folder of its own under
//! the recovery root, holding a [`Journal`] and a lock file. The process that
//! writes a record keeps its lock file locked for as long as the record is in
//! use. The operating system releases the lock when the process ends, however
//! it ends, so at start-up a record whose lock can be taken belongs to a
//! process that is gone: its document is what the user may want back. A record
//! that is still locked belongs to another running copy of the application and
//! is left alone.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::journal::{Durability, Journal, JournalError};

const LOCK_FILE: &str = "lock";
const RECORD_PREFIX: &str = "record-";

/// How the journals of recovery records are written. Commits are batched on a
/// background thread, so the cost of reaching the platter (2 to 4 ms per commit
/// on macOS in the storage gate) never reaches the user's typing.
pub const RECORD_DURABILITY: Durability = Durability::FullFsync;

/// The folder all recovery records live in.
#[derive(Clone, Debug)]
pub struct RecoveryRoot {
    dir: PathBuf,
}

/// A record this process owns. Its lock is held until the value is dropped.
#[derive(Debug)]
pub struct OwnedRecord {
    dir: PathBuf,
    lock: File,
}

/// A record left behind by a process that is gone. While this value exists its
/// lock is held, so no other copy of the application can take the record.
#[derive(Debug)]
pub struct FoundRecord {
    dir: PathBuf,
    lock: File,
}

fn open_lock(dir: &Path) -> io::Result<File> {
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(LOCK_FILE))
}

/// A name no other record has: the time, this process and a counter.
fn fresh_name() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!(
        "{RECORD_PREFIX}{nanos:x}-{:x}-{count:x}",
        std::process::id()
    )
}

impl RecoveryRoot {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Make a new, empty record and take its lock.
    pub fn create(&self) -> io::Result<OwnedRecord> {
        let dir = self.dir.join(fresh_name());
        fs::create_dir_all(&dir)?;
        let lock = open_lock(&dir)?;
        lock.try_lock().map_err(|err| match err {
            TryLockError::Error(err) => err,
            TryLockError::WouldBlock => {
                io::Error::other("a brand-new recovery record is locked already")
            }
        })?;
        Ok(OwnedRecord { dir, lock })
    }

    /// The records no running process owns, with their locks taken.
    ///
    /// Records without a checkpoint hold nothing a document can be rebuilt
    /// from: the process died while creating them. They are deleted here.
    /// Folders that are not records are ignored.
    pub fn scan(&self) -> io::Result<Vec<FoundRecord>> {
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err),
        };
        let mut found = Vec::new();
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with(RECORD_PREFIX) || !entry.file_type()?.is_dir() {
                continue;
            }
            let dir = entry.path();
            let lock = open_lock(&dir)?;
            match lock.try_lock() {
                Ok(()) => {}
                Err(TryLockError::WouldBlock) => continue,
                Err(TryLockError::Error(err)) => return Err(err),
            }
            let record = FoundRecord { dir, lock };
            if record.is_empty() {
                record.discard()?;
            } else {
                found.push(record);
            }
        }
        found.sort_by(|a, b| a.dir.cmp(&b.dir));
        Ok(found)
    }
}

impl OwnedRecord {
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn open_journal(&self) -> Result<Journal, JournalError> {
        Journal::open(&self.dir, RECORD_DURABILITY)
    }

    /// Delete the record. Done when its document is saved or closed normally.
    pub fn discard(self) -> io::Result<()> {
        remove(self.dir, self.lock)
    }
}

impl FoundRecord {
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Whether the record holds no checkpoint, or no journal at all.
    fn is_empty(&self) -> bool {
        if !self.dir.join("journal.db").exists() {
            return true;
        }
        // A database that cannot be opened is not empty: it is damaged, and
        // the user decides what happens to it.
        Journal::open(&self.dir, RECORD_DURABILITY).is_ok_and(|journal| !journal.has_checkpoint())
    }

    pub fn open_journal(&self) -> Result<Journal, JournalError> {
        Journal::open(&self.dir, RECORD_DURABILITY)
    }

    /// When the record was last written to.
    pub fn last_written(&self) -> Option<SystemTime> {
        fs::read_dir(&self.dir)
            .ok()?
            .filter_map(|entry| entry.ok()?.metadata().ok()?.modified().ok())
            .max()
    }

    /// Take the record over: from now on this process writes to it.
    pub fn adopt(self) -> OwnedRecord {
        OwnedRecord {
            dir: self.dir,
            lock: self.lock,
        }
    }

    /// Delete the record. Done when the user discards it.
    pub fn discard(self) -> io::Result<()> {
        remove(self.dir, self.lock)
    }
}

fn remove(dir: PathBuf, lock: File) -> io::Result<()> {
    // Windows will not delete a file that is open, so the lock goes first.
    // Another process could take the record in between and find it half
    // deleted; it then sees no checkpoint and deletes the rest.
    drop(lock);
    match fs::remove_dir_all(&dir) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record_with_checkpoint(root: &RecoveryRoot, text: &[u8]) -> OwnedRecord {
        let record = root.create().unwrap();
        let mut journal = record.open_journal().unwrap();
        journal.checkpoint(text).unwrap();
        record
    }

    #[test]
    fn a_missing_root_has_no_records() {
        let dir = tempfile::tempdir().unwrap();
        let root = RecoveryRoot::new(dir.path().join("not-there"));
        assert!(root.scan().unwrap().is_empty());
    }

    #[test]
    fn records_in_use_are_not_found_and_abandoned_ones_are() {
        let dir = tempfile::tempdir().unwrap();
        let root = RecoveryRoot::new(dir.path());
        let live = record_with_checkpoint(&root, b"still being edited");
        let abandoned = record_with_checkpoint(&root, b"left behind");
        let abandoned_dir = abandoned.dir().to_owned();
        // Dropping the value without discarding is what a crash amounts to:
        // the lock goes, the files stay.
        drop(abandoned);

        let found = root.scan().unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].dir(), abandoned_dir);
        assert_eq!(
            found[0]
                .open_journal()
                .unwrap()
                .replay()
                .unwrap()
                .checkpoint,
            b"left behind"
        );
        // While it is held, a second scan does not see it.
        assert!(root.scan().unwrap().is_empty());
        drop(found);
        assert_eq!(root.scan().unwrap().len(), 1);
        drop(live);
    }

    #[test]
    fn records_without_a_checkpoint_are_cleaned_up() {
        let dir = tempfile::tempdir().unwrap();
        let root = RecoveryRoot::new(dir.path());
        let empty = root.create().unwrap();
        let empty_dir = empty.dir().to_owned();
        drop(empty);
        let half = root.create().unwrap();
        half.open_journal()
            .unwrap()
            .append([b"x".as_slice()])
            .unwrap();
        let half_dir = half.dir().to_owned();
        drop(half);

        assert!(root.scan().unwrap().is_empty());
        assert!(!empty_dir.exists());
        assert!(!half_dir.exists());
    }

    #[test]
    fn other_folders_are_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("something-else")).unwrap();
        fs::write(dir.path().join("record-not-a-folder"), b"x").unwrap();
        let root = RecoveryRoot::new(dir.path());
        assert!(root.scan().unwrap().is_empty());
        assert!(dir.path().join("something-else").exists());
        assert!(dir.path().join("record-not-a-folder").exists());
    }

    #[test]
    fn discarding_deletes_the_record_and_adopting_keeps_it() {
        let dir = tempfile::tempdir().unwrap();
        let root = RecoveryRoot::new(dir.path());
        drop(record_with_checkpoint(&root, b"one"));
        drop(record_with_checkpoint(&root, b"two"));

        let mut found = root.scan().unwrap();
        assert_eq!(found.len(), 2);
        let second = found.pop().unwrap();
        let first = found.pop().unwrap();
        let first_dir = first.dir().to_owned();
        first.discard().unwrap();
        assert!(!first_dir.exists());

        let adopted = second.adopt();
        assert!(adopted.dir().exists());
        // Adopted means owned: still invisible to a scan.
        assert!(root.scan().unwrap().is_empty());
        adopted.discard().unwrap();
        assert!(fs::read_dir(dir.path()).unwrap().next().is_none());
    }

    #[test]
    fn records_report_when_they_were_last_written() {
        let dir = tempfile::tempdir().unwrap();
        let root = RecoveryRoot::new(dir.path());
        drop(record_with_checkpoint(&root, b"x"));
        let found = root.scan().unwrap();
        let written = found[0].last_written().expect("files have times");
        assert!(written.elapsed().unwrap_or_default().as_secs() < 60);
    }

    #[test]
    fn names_do_not_repeat() {
        let names: std::collections::HashSet<_> = (0..1000).map(|_| fresh_name()).collect();
        assert_eq!(names.len(), 1000);
    }
}

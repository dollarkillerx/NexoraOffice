//! The recovery store: a command journal and checkpoints in one directory.
//!
//! - Every edit is appended to the `journal` table in its own SQLite transaction
//!   before it is acknowledged.
//! - Every so often the whole document is written to a checkpoint file, the
//!   database is pointed at it and the journal up to that revision is dropped,
//!   the last two in a single transaction.
//! - Recovery loads the checkpoint and replays whatever the journal holds after
//!   it, refusing to continue if a revision is missing.
//!
//! Document content never goes into SQLite; the database holds the journal and a
//! pointer to the checkpoint file. Files are written to a temporary name, synced
//! and renamed, so a reader sees either the old file or the new one.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

// The atomic write proved here now lives in the product's storage crate.
pub use aster_storage::write_atomically;
use rusqlite::{Connection, OptionalExtension, params};

use crate::doc::{Command, Snapshot};

/// How hard a commit tries to reach the disk before it returns. The names follow
/// SQLite's `synchronous` setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Durability {
    /// WAL frames are written but not synced on commit. Survives the process
    /// dying; a power cut can lose the most recent commits.
    Normal,
    /// The WAL is synced on every commit.
    Full,
    /// As `Full`, and on macOS the sync asks the drive to flush its own cache.
    FullFsync,
}

impl Durability {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "normal" => Some(Self::Normal),
            "full" => Some(Self::Full),
            "fullfsync" => Some(Self::FullFsync),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Full => "full",
            Self::FullFsync => "fullfsync",
        }
    }
}

#[derive(Debug)]
pub enum StoreError {
    Sqlite(rusqlite::Error),
    Io(io::Error),
    /// The store's contents contradict each other. Nothing is repaired silently.
    Corrupt(String),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sqlite(err) => write!(f, "database: {err}"),
            Self::Io(err) => write!(f, "file: {err}"),
            Self::Corrupt(what) => write!(f, "store is corrupt: {what}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<rusqlite::Error> for StoreError {
    fn from(err: rusqlite::Error) -> Self {
        Self::Sqlite(err)
    }
}

impl From<io::Error> for StoreError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

/// What recovery found.
#[derive(Debug, PartialEq, Eq)]
pub struct Recovered {
    pub rev: u64,
    pub text: String,
    /// Revision of the checkpoint recovery started from; 0 if there was none.
    pub checkpoint_rev: u64,
    /// Journal entries replayed on top of the checkpoint.
    pub replayed: u64,
}

const CHECKPOINT_PREFIX: &str = "checkpoint-";
const CHECKPOINT_SUFFIX: &str = ".snap";

fn checkpoint_name(rev: u64) -> String {
    format!("{CHECKPOINT_PREFIX}{rev}{CHECKPOINT_SUFFIX}")
}

pub struct Store {
    conn: Connection,
    dir: PathBuf,
}

impl Store {
    pub fn open(dir: &Path, durability: Durability) -> Result<Self, StoreError> {
        fs::create_dir_all(dir)?;
        let conn = Connection::open(dir.join("recovery.db"))?;
        let mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(StoreError::Corrupt(format!(
                "journal mode is {mode}, not WAL"
            )));
        }
        let synchronous = if durability == Durability::Normal {
            "NORMAL"
        } else {
            "FULL"
        };
        conn.pragma_update(None, "synchronous", synchronous)?;
        conn.pragma_update(None, "fullfsync", durability == Durability::FullFsync)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS journal (
                 rev     INTEGER PRIMARY KEY,
                 payload BLOB NOT NULL
             );
             CREATE TABLE IF NOT EXISTS checkpoint (
                 id   INTEGER PRIMARY KEY CHECK (id = 1),
                 rev  INTEGER NOT NULL,
                 file TEXT NOT NULL
             );",
        )?;
        let store = Self {
            conn,
            dir: dir.to_owned(),
        };
        store.remove_orphans()?;
        Ok(store)
    }

    fn current_checkpoint(&self) -> Result<Option<(u64, String)>, StoreError> {
        Ok(self
            .conn
            .query_row("SELECT rev, file FROM checkpoint WHERE id = 1", [], |row| {
                Ok((row.get::<_, i64>(0)? as u64, row.get::<_, String>(1)?))
            })
            .optional()?)
    }

    /// Delete checkpoint files the database does not point at. They are left
    /// behind when the process dies between writing one and recording it, or
    /// between recording a new one and deleting the old.
    fn remove_orphans(&self) -> Result<(), StoreError> {
        let current = self.current_checkpoint()?.map(|(_, file)| file);
        for entry in fs::read_dir(&self.dir)? {
            let name = entry?.file_name().to_string_lossy().into_owned();
            let is_checkpoint =
                name.starts_with(CHECKPOINT_PREFIX) && name.ends_with(CHECKPOINT_SUFFIX);
            if is_checkpoint && current.as_deref() != Some(name.as_str()) {
                fs::remove_file(self.dir.join(name))?;
            }
        }
        Ok(())
    }

    /// Record one revision. When this returns, the revision is in the journal.
    pub fn append(&mut self, rev: u64, payload: &[u8]) -> Result<(), StoreError> {
        self.conn
            .prepare_cached("INSERT INTO journal (rev, payload) VALUES (?1, ?2)")?
            .execute(params![rev as i64, payload])?;
        Ok(())
    }

    /// Store the whole document and drop the journal up to its revision.
    pub fn checkpoint(&mut self, snapshot: &Snapshot) -> Result<(), StoreError> {
        let previous = self.current_checkpoint()?.map(|(_, file)| file);
        let name = checkpoint_name(snapshot.rev);
        // The file comes first. Until the transaction below commits, the old
        // checkpoint and the full journal are still what recovery uses.
        write_atomically(&self.dir.join(&name), &snapshot.encode())?;

        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO checkpoint (id, rev, file) VALUES (1, ?1, ?2)
             ON CONFLICT (id) DO UPDATE SET rev = excluded.rev, file = excluded.file",
            params![snapshot.rev as i64, name],
        )?;
        tx.execute(
            "DELETE FROM journal WHERE rev <= ?1",
            params![snapshot.rev as i64],
        )?;
        tx.commit()?;

        if let Some(previous) = previous.filter(|previous| *previous != name) {
            // Failing to delete only leaves an orphan for the next open to remove.
            let _ = fs::remove_file(self.dir.join(previous));
        }
        Ok(())
    }

    /// Rebuild the document from the checkpoint and the journal.
    pub fn recover(&self) -> Result<Recovered, StoreError> {
        let (checkpoint_rev, mut text) = match self.current_checkpoint()? {
            None => (0, String::new()),
            Some((rev, file)) => {
                let bytes = fs::read(self.dir.join(&file))?;
                let snapshot = Snapshot::decode(&bytes)
                    .map_err(|why| StoreError::Corrupt(format!("{file}: {why}")))?;
                if snapshot.rev != rev {
                    return Err(StoreError::Corrupt(format!(
                        "{file} holds revision {}, the database expects {rev}",
                        snapshot.rev
                    )));
                }
                (rev, snapshot.text)
            }
        };

        let mut rev = checkpoint_rev;
        let mut statement = self
            .conn
            .prepare("SELECT rev, payload FROM journal WHERE rev > ?1 ORDER BY rev")?;
        let mut rows = statement.query(params![checkpoint_rev as i64])?;
        while let Some(row) = rows.next()? {
            let entry_rev = row.get::<_, i64>(0)? as u64;
            if entry_rev != rev + 1 {
                return Err(StoreError::Corrupt(format!(
                    "journal jumps from revision {rev} to {entry_rev}"
                )));
            }
            let payload: Vec<u8> = row.get(1)?;
            let command = Command::decode(&payload)
                .map_err(|why| StoreError::Corrupt(format!("revision {entry_rev}: {why}")))?;
            command
                .apply(&mut text)
                .map_err(|why| StoreError::Corrupt(format!("revision {entry_rev}: {why}")))?;
            rev = entry_rev;
        }
        Ok(Recovered {
            rev,
            text,
            checkpoint_rev,
            replayed: rev - checkpoint_rev,
        })
    }

    /// SQLite's own consistency check of the database file.
    pub fn integrity(&self) -> Result<(), StoreError> {
        let verdict: String = self
            .conn
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        if verdict == "ok" {
            Ok(())
        } else {
            Err(StoreError::Corrupt(verdict))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::generate;

    /// Append `count` generated revisions after `rev`, keeping `text` in step.
    fn edit(store: &mut Store, text: &mut String, rev: &mut u64, count: u64) {
        for _ in 0..count {
            *rev += 1;
            let command = generate(7, *rev, text);
            command.apply(text).unwrap();
            store.append(*rev, &command.encode()).unwrap();
        }
    }

    fn open(dir: &Path) -> Store {
        Store::open(dir, Durability::Normal).unwrap()
    }

    #[test]
    fn an_empty_store_recovers_to_an_empty_document() {
        let dir = tempfile::tempdir().unwrap();
        let recovered = open(dir.path()).recover().unwrap();
        assert_eq!(
            recovered,
            Recovered {
                rev: 0,
                text: String::new(),
                checkpoint_rev: 0,
                replayed: 0
            }
        );
    }

    #[test]
    fn the_journal_alone_rebuilds_the_document() {
        let dir = tempfile::tempdir().unwrap();
        let (mut text, mut rev) = (String::new(), 0);
        edit(&mut open(dir.path()), &mut text, &mut rev, 300);

        let recovered = open(dir.path()).recover().unwrap();
        assert_eq!((recovered.rev, recovered.replayed), (300, 300));
        assert_eq!(recovered.text, text);
    }

    #[test]
    fn a_checkpoint_truncates_the_journal_and_recovery_continues_from_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = open(dir.path());
        let (mut text, mut rev) = (String::new(), 0);
        edit(&mut store, &mut text, &mut rev, 120);
        store
            .checkpoint(&Snapshot {
                rev,
                text: text.clone(),
            })
            .unwrap();
        edit(&mut store, &mut text, &mut rev, 35);
        drop(store);

        let store = open(dir.path());
        let recovered = store.recover().unwrap();
        assert_eq!(
            (recovered.rev, recovered.checkpoint_rev, recovered.replayed),
            (155, 120, 35)
        );
        assert_eq!(recovered.text, text);
        let rows: i64 = store
            .conn
            .query_row("SELECT count(*) FROM journal", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 35);
        store.integrity().unwrap();
    }

    #[test]
    fn only_the_current_checkpoint_file_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = open(dir.path());
        let (mut text, mut rev) = (String::new(), 0);
        for _ in 0..3 {
            edit(&mut store, &mut text, &mut rev, 10);
            store
                .checkpoint(&Snapshot {
                    rev,
                    text: text.clone(),
                })
                .unwrap();
        }
        // A file written for a checkpoint the database never recorded.
        fs::write(dir.path().join(checkpoint_name(999)), b"left behind").unwrap();
        drop(store);

        let store = open(dir.path());
        let mut files: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(CHECKPOINT_SUFFIX))
            .collect();
        files.sort();
        assert_eq!(files, [checkpoint_name(30)]);
        assert_eq!(store.recover().unwrap().text, text);
    }

    #[test]
    fn a_missing_revision_stops_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = open(dir.path());
        let (mut text, mut rev) = (String::new(), 0);
        edit(&mut store, &mut text, &mut rev, 20);
        store
            .conn
            .execute("DELETE FROM journal WHERE rev = 11", [])
            .unwrap();
        let err = store.recover().unwrap_err();
        assert!(
            matches!(&err, StoreError::Corrupt(what) if what.contains("jumps from revision 10 to 12")),
            "{err}"
        );
    }

    #[test]
    fn a_damaged_checkpoint_stops_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = open(dir.path());
        let (mut text, mut rev) = (String::new(), 0);
        edit(&mut store, &mut text, &mut rev, 20);
        store.checkpoint(&Snapshot { rev, text }).unwrap();

        let path = dir.path().join(checkpoint_name(20));
        let mut bytes = fs::read(&path).unwrap();
        let middle = bytes.len() / 2;
        bytes[middle] ^= 1;
        fs::write(&path, bytes).unwrap();
        assert!(matches!(store.recover(), Err(StoreError::Corrupt(_))));
    }

    #[test]
    fn a_revision_cannot_be_appended_twice() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = open(dir.path());
        let payload = Command::Insert {
            at: 0,
            text: "x".to_owned(),
        }
        .encode();
        store.append(1, &payload).unwrap();
        assert!(matches!(
            store.append(1, &payload),
            Err(StoreError::Sqlite(_))
        ));
    }

    #[test]
    fn atomic_writes_replace_the_whole_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("document.aster");
        write_atomically(&path, b"first version").unwrap();
        write_atomically(&path, b"second").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"second");
        // No temporary files are left next to it.
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}

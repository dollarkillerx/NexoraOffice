//! One document's recovery journal: commands in SQLite, the whole document in
//! checkpoint files.
//!
//! This is the store proved in the Phase 0 storage gate (`spikes/g6-storage`),
//! made independent of what a command or a document is:
//!
//! - Commands are opaque byte strings, appended to the `journal` table with a
//!   revision number each. A batch of them is one SQLite transaction; once it
//!   commits, those revisions are confirmed.
//! - A checkpoint is the whole document as opaque bytes, written to a file with
//!   a header and a checksum. The database points at it, and the journal up to
//!   its revision is dropped in the same transaction.
//! - Replaying hands back the checkpoint and the commands after it, after
//!   checking that no revision is missing. Applying them is the caller's job.
//! - A `meta` table holds a few named values about the document, such as its
//!   title and the file it belongs to.
//!
//! Document content never goes into SQLite. Nothing found to be inconsistent is
//! repaired: it is reported, and the files are left as they are.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};

use crate::atomic::write_atomically;
use crate::fingerprint::checksum;

/// How hard a commit tries to reach the disk before it returns. The names follow
/// SQLite's `synchronous` setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Durability {
    /// WAL frames are written but not synced on commit. Survives the process
    /// dying; a power cut can lose the most recent commits.
    Normal,
    /// The WAL is synced on every commit.
    Full,
    /// As `Full`, and on macOS the sync asks the drive to flush its own cache,
    /// which is what survives a power cut there. The same as `Full` elsewhere.
    FullFsync,
}

#[derive(Debug)]
pub enum JournalError {
    Sqlite(rusqlite::Error),
    Io(io::Error),
    /// The journal's contents contradict each other.
    Corrupt(String),
}

impl fmt::Display for JournalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sqlite(err) => write!(f, "database: {err}"),
            Self::Io(err) => write!(f, "file: {err}"),
            Self::Corrupt(what) => write!(f, "journal is corrupt: {what}"),
        }
    }
}

impl std::error::Error for JournalError {}

impl From<rusqlite::Error> for JournalError {
    fn from(err: rusqlite::Error) -> Self {
        Self::Sqlite(err)
    }
}

impl From<io::Error> for JournalError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

/// What a journal holds, ready to be applied.
#[derive(Debug, PartialEq, Eq)]
pub struct Replay {
    /// Revision of the checkpoint. `None` if no checkpoint was ever written.
    pub checkpoint_rev: Option<u64>,
    /// The document at `checkpoint_rev`. Empty if there is no checkpoint.
    pub checkpoint: Vec<u8>,
    /// The commands after the checkpoint, in order: revision
    /// `checkpoint_rev + 1` first.
    pub commands: Vec<Vec<u8>>,
}

impl Replay {
    /// The revision the document is at once every command is applied.
    pub fn rev(&self) -> u64 {
        self.checkpoint_rev.unwrap_or(0) + self.commands.len() as u64
    }
}

const DATABASE: &str = "journal.db";
const CHECKPOINT_PREFIX: &str = "checkpoint-";
const CHECKPOINT_SUFFIX: &str = ".snap";
/// Header of a checkpoint file: magic, format, revision, length, checksum.
const MAGIC: &[u8; 8] = b"ASTERCKP";
const FORMAT: u8 = 1;
const HEADER_LEN: usize = 8 + 1 + 8 + 8 + 8;

fn checkpoint_name(rev: u64) -> String {
    format!("{CHECKPOINT_PREFIX}{rev}{CHECKPOINT_SUFFIX}")
}

fn encode_checkpoint(rev: u64, bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_LEN + bytes.len());
    out.extend_from_slice(MAGIC);
    out.push(FORMAT);
    out.extend_from_slice(&rev.to_le_bytes());
    out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
    out.extend_from_slice(&checksum(bytes).to_le_bytes());
    out.extend_from_slice(bytes);
    out
}

fn decode_checkpoint(file: &[u8]) -> Result<(u64, Vec<u8>), String> {
    let header = file
        .get(..HEADER_LEN)
        .ok_or("shorter than its header".to_owned())?;
    if &header[..8] != MAGIC {
        return Err("not a checkpoint file".to_owned());
    }
    if header[8] != FORMAT {
        return Err(format!("unknown checkpoint format {}", header[8]));
    }
    let number = |at: usize| u64::from_le_bytes(header[at..at + 8].try_into().expect("8 bytes"));
    let (rev, len, sum) = (number(9), number(17), number(25));
    let body = &file[HEADER_LEN..];
    if body.len() as u64 != len {
        return Err(format!("holds {} bytes, its header says {len}", body.len()));
    }
    if checksum(body) != sum {
        return Err("checksum does not match".to_owned());
    }
    Ok((rev, body.to_vec()))
}

pub struct Journal {
    conn: Connection,
    dir: PathBuf,
    /// Revision of the newest command or checkpoint.
    rev: u64,
    /// Revision of the current checkpoint, if any.
    checkpoint_rev: Option<u64>,
}

impl Journal {
    /// Open the journal in `dir`, creating the directory and the database if
    /// they do not exist yet. Checkpoint files nothing points at are deleted.
    pub fn open(dir: &Path, durability: Durability) -> Result<Self, JournalError> {
        fs::create_dir_all(dir)?;
        let conn = Connection::open(dir.join(DATABASE))?;
        let mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(JournalError::Corrupt(format!(
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
             );
             CREATE TABLE IF NOT EXISTS meta (
                 key   TEXT PRIMARY KEY,
                 value BLOB NOT NULL
             );",
        )?;
        let mut journal = Self {
            conn,
            dir: dir.to_owned(),
            rev: 0,
            checkpoint_rev: None,
        };
        journal.checkpoint_rev = journal.current_checkpoint()?.map(|(rev, _)| rev);
        let newest: Option<i64> =
            journal
                .conn
                .query_row("SELECT max(rev) FROM journal", [], |row| row.get(0))?;
        journal.rev = journal
            .checkpoint_rev
            .unwrap_or(0)
            .max(newest.unwrap_or(0) as u64);
        journal.remove_orphans()?;
        Ok(journal)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Revision of the newest command or checkpoint; 0 for a new journal.
    pub fn rev(&self) -> u64 {
        self.rev
    }

    /// Whether a checkpoint has ever been written. A journal without one holds
    /// nothing a document can be rebuilt from.
    pub fn has_checkpoint(&self) -> bool {
        self.checkpoint_rev.is_some()
    }

    fn current_checkpoint(&self) -> Result<Option<(u64, String)>, JournalError> {
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
    fn remove_orphans(&self) -> Result<(), JournalError> {
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

    /// Append commands as the next revisions, all in one transaction. When this
    /// returns, they are confirmed. Returns the revision of the last one.
    pub fn append<'a>(
        &mut self,
        commands: impl IntoIterator<Item = &'a [u8]>,
    ) -> Result<u64, JournalError> {
        let tx = self.conn.transaction()?;
        let mut rev = self.rev;
        {
            let mut insert =
                tx.prepare_cached("INSERT INTO journal (rev, payload) VALUES (?1, ?2)")?;
            for payload in commands {
                rev += 1;
                insert.execute(params![rev as i64, payload])?;
            }
        }
        tx.commit()?;
        self.rev = rev;
        Ok(rev)
    }

    /// Store the whole document as it is after the newest revision, and drop
    /// the journal up to that revision.
    pub fn checkpoint(&mut self, document: &[u8]) -> Result<(), JournalError> {
        let rev = self.rev;
        let previous = self.current_checkpoint()?.map(|(_, file)| file);
        let name = checkpoint_name(rev);
        // The file comes first. Until the transaction below commits, the old
        // checkpoint and the full journal are still what a replay uses.
        write_atomically(&self.dir.join(&name), &encode_checkpoint(rev, document))?;

        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO checkpoint (id, rev, file) VALUES (1, ?1, ?2)
             ON CONFLICT (id) DO UPDATE SET rev = excluded.rev, file = excluded.file",
            params![rev as i64, name],
        )?;
        tx.execute("DELETE FROM journal WHERE rev <= ?1", params![rev as i64])?;
        tx.commit()?;
        self.checkpoint_rev = Some(rev);

        if let Some(previous) = previous.filter(|previous| *previous != name) {
            // Failing to delete only leaves an orphan for the next open to remove.
            let _ = fs::remove_file(self.dir.join(previous));
        }
        Ok(())
    }

    /// Set named values, all in one transaction.
    pub fn set_meta<'a>(
        &mut self,
        entries: impl IntoIterator<Item = (&'a str, &'a [u8])>,
    ) -> Result<(), JournalError> {
        let tx = self.conn.transaction()?;
        {
            let mut upsert = tx.prepare_cached(
                "INSERT INTO meta (key, value) VALUES (?1, ?2)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            )?;
            for (key, value) in entries {
                upsert.execute(params![key, value])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Make the named values exactly `entries`, in one transaction.
    pub fn replace_meta<'a>(
        &mut self,
        entries: impl IntoIterator<Item = (&'a str, &'a [u8])>,
    ) -> Result<(), JournalError> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM meta", [])?;
        {
            let mut insert = tx.prepare_cached("INSERT INTO meta (key, value) VALUES (?1, ?2)")?;
            for (key, value) in entries {
                insert.execute(params![key, value])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Remove named values.
    pub fn remove_meta<'a>(
        &mut self,
        keys: impl IntoIterator<Item = &'a str>,
    ) -> Result<(), JournalError> {
        let tx = self.conn.transaction()?;
        for key in keys {
            tx.execute("DELETE FROM meta WHERE key = ?1", params![key])?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn meta(&self) -> Result<BTreeMap<String, Vec<u8>>, JournalError> {
        let mut statement = self.conn.prepare("SELECT key, value FROM meta")?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// The checkpoint and the commands after it.
    pub fn replay(&self) -> Result<Replay, JournalError> {
        let (checkpoint_rev, checkpoint) = match self.current_checkpoint()? {
            None => (None, Vec::new()),
            Some((rev, file)) => {
                let bytes = fs::read(self.dir.join(&file))?;
                let (file_rev, document) = decode_checkpoint(&bytes)
                    .map_err(|why| JournalError::Corrupt(format!("{file}: {why}")))?;
                if file_rev != rev {
                    return Err(JournalError::Corrupt(format!(
                        "{file} holds revision {file_rev}, the database expects {rev}"
                    )));
                }
                (Some(rev), document)
            }
        };

        let start = checkpoint_rev.unwrap_or(0);
        let mut rev = start;
        let mut commands = Vec::new();
        let mut statement = self
            .conn
            .prepare("SELECT rev, payload FROM journal WHERE rev > ?1 ORDER BY rev")?;
        let mut rows = statement.query(params![start as i64])?;
        while let Some(row) = rows.next()? {
            let entry_rev = row.get::<_, i64>(0)? as u64;
            if entry_rev != rev + 1 {
                return Err(JournalError::Corrupt(format!(
                    "journal jumps from revision {rev} to {entry_rev}"
                )));
            }
            commands.push(row.get(1)?);
            rev = entry_rev;
        }
        Ok(Replay {
            checkpoint_rev,
            checkpoint,
            commands,
        })
    }

    /// SQLite's own consistency check of the database file.
    pub fn integrity(&self) -> Result<(), JournalError> {
        let verdict: String = self
            .conn
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        if verdict == "ok" {
            Ok(())
        } else {
            Err(JournalError::Corrupt(verdict))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open(dir: &Path) -> Journal {
        Journal::open(dir, Durability::Normal).unwrap()
    }

    fn command(n: u64) -> Vec<u8> {
        format!("command {n}").into_bytes()
    }

    fn append_range(journal: &mut Journal, range: std::ops::RangeInclusive<u64>) {
        let commands: Vec<Vec<u8>> = range.map(command).collect();
        journal.append(commands.iter().map(Vec::as_slice)).unwrap();
    }

    #[test]
    fn a_new_journal_is_empty_and_has_no_checkpoint() {
        let dir = tempfile::tempdir().unwrap();
        let journal = open(dir.path());
        assert_eq!(journal.rev(), 0);
        assert!(!journal.has_checkpoint());
        let replay = journal.replay().unwrap();
        assert_eq!(
            replay,
            Replay {
                checkpoint_rev: None,
                checkpoint: Vec::new(),
                commands: Vec::new(),
            }
        );
        assert_eq!(replay.rev(), 0);
    }

    #[test]
    fn appended_commands_come_back_in_order_after_reopening() {
        let dir = tempfile::tempdir().unwrap();
        let mut journal = open(dir.path());
        journal.checkpoint(b"start").unwrap();
        append_range(&mut journal, 1..=3);
        assert_eq!(journal.append([b"x".as_slice()]).unwrap(), 4);
        drop(journal);

        let journal = open(dir.path());
        assert_eq!(journal.rev(), 4);
        let replay = journal.replay().unwrap();
        assert_eq!(replay.checkpoint_rev, Some(0));
        assert_eq!(replay.checkpoint, b"start");
        assert_eq!(
            replay.commands,
            [command(1), command(2), command(3), b"x".to_vec()]
        );
        assert_eq!(replay.rev(), 4);
    }

    #[test]
    fn a_checkpoint_truncates_the_journal_and_replay_continues_from_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut journal = open(dir.path());
        append_range(&mut journal, 1..=120);
        journal.checkpoint(b"document at 120").unwrap();
        append_range(&mut journal, 121..=155);
        drop(journal);

        let journal = open(dir.path());
        let replay = journal.replay().unwrap();
        assert_eq!(replay.checkpoint_rev, Some(120));
        assert_eq!(replay.checkpoint, b"document at 120");
        assert_eq!(replay.commands.len(), 35);
        assert_eq!(replay.commands[0], command(121));
        assert_eq!(replay.rev(), 155);
        let rows: i64 = journal
            .conn
            .query_row("SELECT count(*) FROM journal", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 35);
        journal.integrity().unwrap();
    }

    #[test]
    fn only_the_current_checkpoint_file_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let mut journal = open(dir.path());
        for round in 1..=3 {
            append_range(&mut journal, (round * 10 - 9)..=(round * 10));
            journal
                .checkpoint(format!("at {}", round * 10).as_bytes())
                .unwrap();
        }
        // A file written for a checkpoint the database never recorded.
        fs::write(dir.path().join(checkpoint_name(999)), b"left behind").unwrap();
        drop(journal);

        let journal = open(dir.path());
        let mut files: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(CHECKPOINT_SUFFIX))
            .collect();
        files.sort();
        assert_eq!(files, [checkpoint_name(30)]);
        assert_eq!(journal.replay().unwrap().checkpoint, b"at 30");
    }

    #[test]
    fn a_missing_revision_stops_the_replay() {
        let dir = tempfile::tempdir().unwrap();
        let mut journal = open(dir.path());
        append_range(&mut journal, 1..=20);
        journal
            .conn
            .execute("DELETE FROM journal WHERE rev = 11", [])
            .unwrap();
        let err = journal.replay().unwrap_err();
        assert!(
            matches!(&err, JournalError::Corrupt(what) if what.contains("jumps from revision 10 to 12")),
            "{err}"
        );
    }

    #[test]
    fn a_damaged_checkpoint_stops_the_replay() {
        let dir = tempfile::tempdir().unwrap();
        let mut journal = open(dir.path());
        append_range(&mut journal, 1..=20);
        journal.checkpoint(b"the whole document").unwrap();

        let path = dir.path().join(checkpoint_name(20));
        let mut bytes = fs::read(&path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        fs::write(&path, bytes).unwrap();
        assert!(matches!(journal.replay(), Err(JournalError::Corrupt(_))));
        // Nothing was repaired or removed.
        assert!(path.exists());
    }

    #[test]
    fn checkpoint_files_are_checked_through_and_through() {
        let encoded = encode_checkpoint(7, b"text");
        assert_eq!(decode_checkpoint(&encoded), Ok((7, b"text".to_vec())));
        assert!(decode_checkpoint(&encoded[..10]).is_err());
        assert!(decode_checkpoint(&encoded[..encoded.len() - 1]).is_err());
        let mut wrong_magic = encoded.clone();
        wrong_magic[0] = b'X';
        assert!(decode_checkpoint(&wrong_magic).is_err());
        let mut wrong_format = encoded;
        wrong_format[8] = 9;
        assert!(
            decode_checkpoint(&wrong_format)
                .unwrap_err()
                .contains("format 9")
        );
    }

    #[test]
    fn named_values_are_kept_replaced_and_removed() {
        let dir = tempfile::tempdir().unwrap();
        let mut journal = open(dir.path());
        journal
            .set_meta([
                ("title", b"notes".as_slice()),
                ("path", b"/a.md".as_slice()),
            ])
            .unwrap();
        journal.set_meta([("title", "会议".as_bytes())]).unwrap();
        journal.remove_meta(["path"]).unwrap();
        drop(journal);
        let mut journal = open(dir.path());
        let meta = journal.meta().unwrap();
        assert_eq!(meta.len(), 1);
        assert_eq!(meta["title"], "会议".as_bytes());

        journal
            .replace_meta([("kind", b"markdown".as_slice())])
            .unwrap();
        let meta = journal.meta().unwrap();
        assert_eq!(meta.keys().collect::<Vec<_>>(), ["kind"]);
    }

    #[test]
    fn an_empty_batch_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mut journal = open(dir.path());
        assert_eq!(journal.append(std::iter::empty()).unwrap(), 0);
        assert_eq!(journal.rev(), 0);
    }
}

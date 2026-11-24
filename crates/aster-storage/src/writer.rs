//! The thread that writes recovery records.
//!
//! The user interface hands every edit to a [`RecordWriter`] and moves on; it
//! never waits for the disk. The writer thread takes whatever has queued up
//! since its last commit and writes each document's share as one transaction,
//! so a burst of typing costs one sync, not one per key. An edit counts as
//! confirmed when its transaction commits, and [`Event::Committed`] says so.
//!
//! Requests for one document are carried out in the order they were made.
//! When a record cannot be written (the disk is full, the folder was deleted)
//! the writer reports [`Event::Failed`] once and ignores that document until it
//! is discarded; the document itself is unaffected.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;

use crate::journal::Journal;
use crate::records::{FoundRecord, OwnedRecord, RecoveryRoot};

/// Names a document for the writer. Chosen by the caller, unique per process.
pub type RecordKey = u64;

/// A whole document for a record: its bytes, or a way to produce them that
/// runs on the writer thread. A 100 MB document is turned into bytes there,
/// so the thread that asked never copies it.
pub enum Contents {
    Bytes(Vec<u8>),
    Later(Box<dyn FnOnce() -> Vec<u8> + Send>),
    /// Made later, and it may come out as nothing: then nothing is
    /// written, and the record keeps the document it had.
    Maybe(Box<dyn FnOnce() -> Option<Vec<u8>> + Send>),
}

impl Contents {
    pub fn later(produce: impl FnOnce() -> Vec<u8> + Send + 'static) -> Self {
        Contents::Later(Box::new(produce))
    }

    /// A document that cannot always be made, such as a workbook saved
    /// into bytes: when making it fails, the last good one stays.
    pub fn maybe(produce: impl FnOnce() -> Option<Vec<u8>> + Send + 'static) -> Self {
        Contents::Maybe(Box::new(produce))
    }

    fn into_bytes(self) -> Option<Vec<u8>> {
        match self {
            Contents::Bytes(bytes) => Some(bytes),
            Contents::Later(produce) => Some(produce()),
            Contents::Maybe(produce) => produce(),
        }
    }
}

impl From<Vec<u8>> for Contents {
    fn from(bytes: Vec<u8>) -> Self {
        Contents::Bytes(bytes)
    }
}

/// What the writer reports back, from its own thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// Every edit of this document up to `rev` is on disk.
    Committed { key: RecordKey, rev: u64 },
    /// The document's record could not be written. Nothing more is written for
    /// it until it is discarded.
    Failed { key: RecordKey, reason: String },
}

/// At most this many requests go into one round of commits.
const BATCH: usize = 512;

enum Request {
    Open {
        key: RecordKey,
        found: Option<FoundRecord>,
        meta: Vec<(String, Vec<u8>)>,
        document: Contents,
    },
    Append {
        key: RecordKey,
        command: Vec<u8>,
    },
    Checkpoint {
        key: RecordKey,
        document: Contents,
    },
    Meta {
        key: RecordKey,
        set: Vec<(String, Vec<u8>)>,
        remove: Vec<String>,
    },
    Discard {
        key: RecordKey,
    },
    DiscardFound(FoundRecord),
    Flush(Sender<()>),
}

impl Request {
    fn key(&self) -> Option<RecordKey> {
        match self {
            Request::Open { key, .. }
            | Request::Append { key, .. }
            | Request::Checkpoint { key, .. }
            | Request::Meta { key, .. }
            | Request::Discard { key } => Some(*key),
            Request::DiscardFound(_) | Request::Flush(_) => None,
        }
    }
}

/// The handle the user interface holds.
pub struct RecordWriter {
    requests: Option<Sender<Request>>,
    thread: Option<JoinHandle<()>>,
}

impl RecordWriter {
    /// Start the writer thread. `events` is called on that thread.
    pub fn spawn(root: RecoveryRoot, events: impl Fn(Event) + Send + 'static) -> Self {
        let (requests, receiver) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("aster-recovery".to_owned())
            .spawn(move || Worker::new(root, Box::new(events)).run(receiver))
            .expect("the recovery writer thread can be started");
        Self {
            requests: Some(requests),
            thread: Some(thread),
        }
    }

    fn send(&self, request: Request) {
        // The thread only stops when this handle is dropped.
        if let Some(requests) = &self.requests {
            let _ = requests.send(request);
        }
    }

    /// Start a record for a document: `document` is its whole content now, and
    /// `meta` the named values that go with it. Edits appended afterwards are
    /// replayed on top of `document`.
    pub fn open(
        &self,
        key: RecordKey,
        meta: Vec<(String, Vec<u8>)>,
        document: impl Into<Contents>,
    ) {
        self.send(Request::Open {
            key,
            found: None,
            meta,
            document: document.into(),
        });
    }

    /// Continue a record left behind by an earlier run: the document restored
    /// from it, possibly edited since, is `document`. The record is written to
    /// from now on instead of a new one.
    pub fn adopt(
        &self,
        key: RecordKey,
        found: FoundRecord,
        meta: Vec<(String, Vec<u8>)>,
        document: impl Into<Contents>,
    ) {
        self.send(Request::Open {
            key,
            found: Some(found),
            meta,
            document: document.into(),
        });
    }

    /// Record one edit.
    pub fn append(&self, key: RecordKey, command: Vec<u8>) {
        self.send(Request::Append { key, command });
    }

    /// Store the whole document as it is after the edits sent so far, so the
    /// journal before it can be dropped.
    pub fn checkpoint(&self, key: RecordKey, document: impl Into<Contents>) {
        self.send(Request::Checkpoint {
            key,
            document: document.into(),
        });
    }

    /// Change named values.
    pub fn set_meta(&self, key: RecordKey, set: Vec<(String, Vec<u8>)>, remove: Vec<String>) {
        self.send(Request::Meta { key, set, remove });
    }

    /// Delete the document's record: it was saved or closed normally.
    pub fn discard(&self, key: RecordKey) {
        self.send(Request::Discard { key });
    }

    /// Delete a record left behind by an earlier run: the user discarded it.
    pub fn discard_found(&self, found: FoundRecord) {
        self.send(Request::DiscardFound(found));
    }

    /// Wait until everything requested so far has been carried out.
    pub fn flush(&self) {
        let (done, wait) = mpsc::channel();
        self.send(Request::Flush(done));
        let _ = wait.recv();
    }
}

impl Drop for RecordWriter {
    /// Carry out what is still queued, then stop the thread.
    fn drop(&mut self) {
        drop(self.requests.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// A record asked for and not opened yet: the record found at start-up it
/// takes over, if any, and the named values it gets.
type Waiting = (Option<FoundRecord>, Vec<(String, Vec<u8>)>);

struct Worker {
    root: RecoveryRoot,
    events: Box<dyn Fn(Event) + Send>,
    records: HashMap<RecordKey, (OwnedRecord, Journal)>,
    failed: HashSet<RecordKey>,
    /// Records asked for whose document could not be made yet: what they
    /// would be opened with. The first checkpoint that makes one opens it.
    waiting: HashMap<RecordKey, Waiting>,
}

impl Worker {
    fn new(root: RecoveryRoot, events: Box<dyn Fn(Event) + Send>) -> Self {
        Self {
            root,
            events,
            records: HashMap::new(),
            failed: HashSet::new(),
            waiting: HashMap::new(),
        }
    }

    fn run(mut self, requests: Receiver<Request>) {
        while let Ok(first) = requests.recv() {
            let mut batch = vec![first];
            while batch.len() < BATCH {
                match requests.try_recv() {
                    Ok(request) => batch.push(request),
                    Err(_) => break,
                }
            }
            self.handle_batch(batch);
        }
    }

    fn handle_batch(&mut self, batch: Vec<Request>) {
        // Appends are held back and committed together, one transaction per
        // document. Anything else for a document first commits what is held
        // back for it, so the order of its requests is kept.
        let mut held: Vec<(RecordKey, Vec<Vec<u8>>)> = Vec::new();
        for request in batch {
            match request {
                Request::Append { key, command } => {
                    match held.iter_mut().find(|(held_key, _)| *held_key == key) {
                        Some((_, commands)) => commands.push(command),
                        None => held.push((key, vec![command])),
                    }
                }
                Request::Flush(done) => {
                    for (key, commands) in held.drain(..) {
                        self.commit(key, commands);
                    }
                    let _ = done.send(());
                }
                Request::DiscardFound(found) => {
                    let dir = found.dir().to_owned();
                    if let Err(err) = found.discard() {
                        eprintln!("cannot delete recovery record {dir:?}: {err}");
                    }
                }
                other => {
                    let key = other
                        .key()
                        .expect("only Flush and DiscardFound have no key");
                    if let Some(at) = held.iter().position(|(held_key, _)| *held_key == key) {
                        let (_, commands) = held.remove(at);
                        self.commit(key, commands);
                    }
                    self.handle(other);
                }
            }
        }
        for (key, commands) in held {
            self.commit(key, commands);
        }
    }

    fn fail(&mut self, key: RecordKey, reason: String) {
        // The files stay where they are; the lock goes with the record.
        self.records.remove(&key);
        if self.failed.insert(key) {
            (self.events)(Event::Failed { key, reason });
        }
    }

    fn commit(&mut self, key: RecordKey, commands: Vec<Vec<u8>>) {
        let Some((_, journal)) = self.records.get_mut(&key) else {
            return;
        };
        match journal.append(commands.iter().map(Vec::as_slice)) {
            Ok(rev) => (self.events)(Event::Committed { key, rev }),
            Err(err) => self.fail(key, err.to_string()),
        }
    }

    fn handle(&mut self, request: Request) {
        match request {
            Request::Open {
                key,
                found,
                meta,
                document,
            } => {
                // Opening again replaces the old record.
                if let Some((record, journal)) = self.records.remove(&key) {
                    drop(journal);
                    let _ = record.discard();
                }
                self.failed.remove(&key);
                self.waiting.remove(&key);
                let Some(document) = document.into_bytes() else {
                    // Nothing to write yet: opened by the first checkpoint
                    // that has a document.
                    self.waiting.insert(key, (found, meta));
                    return;
                };
                let record = match found {
                    Some(found) => Ok(found.adopt()),
                    None => self.root.create(),
                };
                let opened = record.map_err(|err| err.to_string()).and_then(|record| {
                    let mut journal = record.open_journal().map_err(|err| err.to_string())?;
                    journal
                        .replace_meta(meta.iter().map(|(k, v)| (k.as_str(), v.as_slice())))
                        .and_then(|()| journal.checkpoint(&document))
                        .map_err(|err| err.to_string())?;
                    Ok((record, journal))
                });
                match opened {
                    Ok((record, journal)) => {
                        let rev = journal.rev();
                        self.records.insert(key, (record, journal));
                        (self.events)(Event::Committed { key, rev });
                    }
                    Err(reason) => self.fail(key, reason),
                }
            }
            Request::Checkpoint { key, document } => {
                if let Some((found, meta)) = self.waiting.remove(&key) {
                    self.handle(Request::Open {
                        key,
                        found,
                        meta,
                        document,
                    });
                    return;
                }
                // A document that could not be made leaves the last one.
                if let Some((_, journal)) = self.records.get_mut(&key)
                    && let Some(document) = document.into_bytes()
                    && let Err(err) = journal.checkpoint(&document)
                {
                    self.fail(key, err.to_string());
                }
            }
            Request::Meta { key, set, remove } => {
                if let Some((_, meta)) = self.waiting.get_mut(&key) {
                    meta.retain(|(k, _)| !remove.contains(k) && !set.iter().any(|(s, _)| s == k));
                    meta.extend(set);
                    return;
                }
                if let Some((_, journal)) = self.records.get_mut(&key) {
                    let result = journal
                        .set_meta(set.iter().map(|(k, v)| (k.as_str(), v.as_slice())))
                        .and_then(|()| journal.remove_meta(remove.iter().map(String::as_str)));
                    if let Err(err) = result {
                        self.fail(key, err.to_string());
                    }
                }
            }
            Request::Discard { key } => {
                self.failed.remove(&key);
                if let Some((Some(found), _)) = self.waiting.remove(&key) {
                    let _ = found.discard();
                }
                if let Some((record, journal)) = self.records.remove(&key) {
                    // The database must be closed before its files can go.
                    drop(journal);
                    if let Err(err) = record.discard() {
                        (self.events)(Event::Failed {
                            key,
                            reason: err.to_string(),
                        });
                    }
                }
            }
            Request::Append { .. } | Request::DiscardFound(_) | Request::Flush(_) => {
                unreachable!("handled by handle_batch")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn start(dir: &std::path::Path) -> (RecordWriter, Arc<Mutex<Vec<Event>>>) {
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = events.clone();
        let writer = RecordWriter::spawn(RecoveryRoot::new(dir), move |event| {
            sink.lock().unwrap().push(event);
        });
        (writer, events)
    }

    fn meta(title: &str) -> Vec<(String, Vec<u8>)> {
        vec![("title".to_owned(), title.as_bytes().to_vec())]
    }

    #[test]
    fn a_record_holds_the_document_and_every_edit() {
        let dir = tempfile::tempdir().unwrap();
        let (writer, events) = start(dir.path());
        writer.open(1, meta("草稿"), b"start".to_vec());
        for n in 0..10 {
            writer.append(1, format!("edit {n}").into_bytes());
        }
        writer.flush();
        // Dropping the writer without discarding is what a crash leaves.
        drop(writer);

        let found = RecoveryRoot::new(dir.path()).scan().unwrap();
        assert_eq!(found.len(), 1);
        let journal = found[0].open_journal().unwrap();
        assert_eq!(journal.meta().unwrap()["title"], "草稿".as_bytes());
        let replay = journal.replay().unwrap();
        assert_eq!(replay.checkpoint, b"start");
        assert_eq!(replay.commands.len(), 10);
        assert_eq!(replay.commands[9], b"edit 9");
        let last = events.lock().unwrap().last().cloned();
        assert_eq!(last, Some(Event::Committed { key: 1, rev: 10 }));
    }

    #[test]
    fn a_document_that_cannot_be_made_leaves_the_last_good_one() {
        let dir = tempfile::tempdir().unwrap();
        let (writer, _) = start(dir.path());
        // Nothing the first time: no record until there is a document.
        writer.open(7, meta("帳簿"), Contents::maybe(|| None));
        writer.set_meta(7, meta("帳簿 2"), Vec::new());
        writer.flush();
        assert!(RecoveryRoot::new(dir.path()).scan().unwrap().is_empty());
        writer.checkpoint(7, Contents::maybe(|| Some(b"good".to_vec())));
        // A checkpoint that fails keeps the good one.
        writer.checkpoint(7, Contents::maybe(|| None));
        writer.flush();
        drop(writer);
        let found = RecoveryRoot::new(dir.path()).scan().unwrap();
        assert_eq!(found.len(), 1);
        let journal = found[0].open_journal().unwrap();
        assert_eq!(journal.meta().unwrap()["title"], "帳簿 2".as_bytes());
        assert_eq!(journal.replay().unwrap().checkpoint, b"good");
    }

    #[test]
    fn records_of_different_documents_stay_apart() {
        let dir = tempfile::tempdir().unwrap();
        let (writer, _) = start(dir.path());
        writer.open(1, meta("a"), b"A".to_vec());
        writer.open(2, meta("b"), b"B".to_vec());
        writer.append(1, b"a1".to_vec());
        writer.append(2, b"b1".to_vec());
        writer.append(1, b"a2".to_vec());
        drop(writer);

        let found = RecoveryRoot::new(dir.path()).scan().unwrap();
        let mut replays: Vec<_> = found
            .iter()
            .map(|record| record.open_journal().unwrap().replay().unwrap())
            .collect();
        replays.sort_by(|a, b| a.checkpoint.cmp(&b.checkpoint));
        assert_eq!(replays[0].checkpoint, b"A");
        assert_eq!(replays[0].commands, [b"a1".to_vec(), b"a2".to_vec()]);
        assert_eq!(replays[1].commands, [b"b1".to_vec()]);
    }

    #[test]
    fn a_checkpoint_comes_after_the_edits_sent_before_it() {
        let dir = tempfile::tempdir().unwrap();
        let (writer, _) = start(dir.path());
        writer.open(1, meta("a"), b"".to_vec());
        writer.append(1, b"one".to_vec());
        writer.append(1, b"two".to_vec());
        writer.checkpoint(1, b"after two".to_vec());
        writer.append(1, b"three".to_vec());
        drop(writer);

        let found = RecoveryRoot::new(dir.path()).scan().unwrap();
        let replay = found[0].open_journal().unwrap().replay().unwrap();
        assert_eq!(replay.checkpoint_rev, Some(2));
        assert_eq!(replay.checkpoint, b"after two");
        assert_eq!(replay.commands, [b"three".to_vec()]);
    }

    #[test]
    fn discarded_records_are_gone() {
        let dir = tempfile::tempdir().unwrap();
        let (writer, _) = start(dir.path());
        writer.open(1, meta("saved"), b"x".to_vec());
        writer.append(1, b"y".to_vec());
        writer.open(2, meta("crashed"), b"z".to_vec());
        writer.discard(1);
        // Discarding something unknown is harmless.
        writer.discard(99);
        drop(writer);

        let found = RecoveryRoot::new(dir.path()).scan().unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].open_journal().unwrap().meta().unwrap()["title"],
            b"crashed"
        );
    }

    #[test]
    fn a_found_record_can_be_discarded_through_the_writer() {
        let dir = tempfile::tempdir().unwrap();
        let (writer, _) = start(dir.path());
        writer.open(1, meta("left"), b"x".to_vec());
        drop(writer);
        let mut found = RecoveryRoot::new(dir.path()).scan().unwrap();
        let record = found.pop().unwrap();
        let record_dir = record.dir().to_owned();
        let (writer, _) = start(dir.path());
        writer.discard_found(record);
        writer.flush();
        assert!(!record_dir.exists());
    }

    #[test]
    fn named_values_can_change_while_editing() {
        let dir = tempfile::tempdir().unwrap();
        let (writer, _) = start(dir.path());
        writer.open(
            1,
            vec![
                ("title".to_owned(), b"untitled".to_vec()),
                ("draft".to_owned(), b"1".to_vec()),
            ],
            b"".to_vec(),
        );
        writer.set_meta(
            1,
            vec![("title".to_owned(), b"notes.md".to_vec())],
            vec!["draft".to_owned()],
        );
        drop(writer);
        let found = RecoveryRoot::new(dir.path()).scan().unwrap();
        let meta = found[0].open_journal().unwrap().meta().unwrap();
        assert_eq!(meta.len(), 1);
        assert_eq!(meta["title"], b"notes.md");
    }

    #[test]
    fn an_adopted_record_continues_where_it_was() {
        let dir = tempfile::tempdir().unwrap();
        let (writer, _) = start(dir.path());
        writer.open(1, meta("old"), b"base".to_vec());
        writer.append(1, b"e1".to_vec());
        writer.append(1, b"e2".to_vec());
        drop(writer);

        let mut found = RecoveryRoot::new(dir.path()).scan().unwrap();
        let record = found.pop().unwrap();
        let record_dir = record.dir().to_owned();
        let (writer, events) = start(dir.path());
        writer.adopt(5, record, meta("restored"), b"base+e1+e2".to_vec());
        writer.append(5, b"e3".to_vec());
        drop(writer);

        assert!(
            events
                .lock()
                .unwrap()
                .contains(&Event::Committed { key: 5, rev: 3 })
        );
        let found = RecoveryRoot::new(dir.path()).scan().unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].dir(), record_dir);
        let journal = found[0].open_journal().unwrap();
        assert_eq!(journal.meta().unwrap()["title"], b"restored");
        let replay = journal.replay().unwrap();
        assert_eq!(replay.checkpoint_rev, Some(2));
        assert_eq!(replay.checkpoint, b"base+e1+e2");
        assert_eq!(replay.commands, [b"e3".to_vec()]);
    }

    #[test]
    fn a_record_that_cannot_be_written_is_reported_once() {
        let dir = tempfile::tempdir().unwrap();
        // The root is a file, so no record folder can be made in it.
        let root = dir.path().join("root");
        std::fs::write(&root, b"in the way").unwrap();
        let (writer, events) = start(&root);
        writer.open(1, meta("a"), b"x".to_vec());
        writer.append(1, b"y".to_vec());
        writer.append(1, b"z".to_vec());
        writer.flush();
        let events = events.lock().unwrap().clone();
        assert_eq!(events.len(), 1, "{events:?}");
        assert!(matches!(&events[0], Event::Failed { key: 1, .. }));
    }

    #[test]
    fn a_burst_of_edits_is_committed_in_few_transactions() {
        let dir = tempfile::tempdir().unwrap();
        let (writer, events) = start(dir.path());
        writer.open(1, meta("a"), Vec::new());
        for n in 0..2000 {
            writer.append(1, n.to_string().into_bytes());
        }
        writer.flush();
        let commits = events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| matches!(event, Event::Committed { key: 1, rev } if *rev > 0))
            .count();
        // One per round of at most BATCH requests, and usually far fewer.
        assert!(commits <= 2000 / (BATCH - 1) + 2, "{commits} commits");
        drop(writer);
    }
}

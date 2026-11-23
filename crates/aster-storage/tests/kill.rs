//! Kill a process while it edits through a [`RecordWriter`], over and over, and
//! check every time that what the next start recovers is exactly right.
//!
//! The worker is this test binary run again with `ASTER_KILL_WORKER_DIR` set:
//! the `worker` test below then does what the application does. It scans the
//! recovery folder, rebuilds the document from the record it finds and adopts
//! that record, or starts a new one; then it edits without end, handing every
//! edit to the writer and checkpointing every so often. The writer's thread
//! prints `ack <rev>` once a revision is committed. The parent kills the worker
//! at a random moment and checks, the way the next start would:
//!
//! - There is at most one record, and SQLite's integrity check passes.
//! - The record replays to a revision the worker acknowledged, or later (it can
//!   die between committing and printing). Lower means a confirmed edit was
//!   lost.
//! - The text equals the text obtained by recomputing every edit from the
//!   beginning: edits are a pure function of the seed and the revision.
//!
//! `cargo test -p aster-storage --test kill` runs a short version, which is
//! part of `scripts/ci.sh`. For a long run:
//!
//! ```sh
//! ASTER_KILL_ROUNDS=1000 cargo test --release -p aster-storage --test kill -- --nocapture
//! ```
//!
//! This tests the process dying. It cannot test a power cut.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command as Process, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use aster_storage::{Event, RecordWriter, RecoveryRoot};

const WORKER_DIR: &str = "ASTER_KILL_WORKER_DIR";
const WORKER_SEED: &str = "ASTER_KILL_WORKER_SEED";
const ROUNDS: &str = "ASTER_KILL_ROUNDS";
const DEFAULT_ROUNDS: u32 = 40;
/// The worker checkpoints after this many edits, as the application does.
const CHECKPOINT_EVERY: u64 = 64;
/// The worker waits for the writer when it is this many edits ahead of it.
const MAX_AHEAD: u64 = 1500;
/// The writer commits at most this many edits between two acknowledgements.
const WRITER_BATCH: u64 = 512;
const KEY: u64 = 1;

// --- A document and its edits, all pure functions -------------------------

/// SplitMix64. Small, fast, and identical on every platform.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }
}

/// Replace `start..end` of the text with `text`.
#[derive(Debug, PartialEq, Eq)]
struct Edit {
    start: usize,
    end: usize,
    text: String,
}

impl Edit {
    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(16 + self.text.len());
        out.extend_from_slice(&(self.start as u64).to_le_bytes());
        out.extend_from_slice(&(self.end as u64).to_le_bytes());
        out.extend_from_slice(self.text.as_bytes());
        out
    }

    fn decode(bytes: &[u8]) -> Result<Self, String> {
        let header = bytes.get(..16).ok_or("edit is shorter than its header")?;
        let number = |at: usize| u64::from_le_bytes(header[at..at + 8].try_into().unwrap());
        let text = String::from_utf8(bytes[16..].to_vec()).map_err(|_| "text is not UTF-8")?;
        Ok(Self {
            start: number(0) as usize,
            end: number(8) as usize,
            text,
        })
    }

    fn apply(&self, doc: &mut String) -> Result<(), String> {
        let fits = self.start <= self.end
            && self.end <= doc.len()
            && doc.is_char_boundary(self.start)
            && doc.is_char_boundary(self.end);
        if !fits {
            return Err(format!(
                "{}..{} does not fit a {} byte document",
                self.start,
                self.end,
                doc.len()
            ));
        }
        doc.replace_range(self.start..self.end, &self.text);
        Ok(())
    }
}

const FRAGMENTS: [&str; 8] = ["字", "排版", "Aster ", "。", "\n", "日本語", "x", "テスト"];
const TARGET_LEN: usize = 3000;

fn boundary_at_or_before(doc: &str, mut at: usize) -> usize {
    while !doc.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// The edit that revision `rev` consists of, given the text just before it.
fn generate(seed: u64, rev: u64, doc: &str) -> Edit {
    let mut rng = Rng(seed ^ rev.wrapping_mul(0xa24b_aed4_963e_e407));
    let start = boundary_at_or_before(doc, rng.below(doc.len() as u64 + 1) as usize);
    let remove = !doc.is_empty() && (doc.len() > TARGET_LEN || rng.below(3) == 0);
    let end = if remove {
        boundary_at_or_before(doc, (start + 1 + rng.below(24) as usize).min(doc.len()))
    } else {
        start
    };
    let pieces = rng.below(3);
    let text = (0..pieces)
        .map(|_| FRAGMENTS[rng.below(FRAGMENTS.len() as u64) as usize])
        .collect();
    Edit { start, end, text }
}

/// The text a record replays to, and its revision.
fn rebuild(record: &aster_storage::FoundRecord) -> Result<(u64, String), String> {
    let journal = record.open_journal().map_err(|err| err.to_string())?;
    journal.integrity().map_err(|err| err.to_string())?;
    let replay = journal.replay().map_err(|err| err.to_string())?;
    let mut text = String::from_utf8(replay.checkpoint.clone())
        .map_err(|_| "checkpoint is not UTF-8".to_owned())?;
    for (index, bytes) in replay.commands.iter().enumerate() {
        let rev = replay.checkpoint_rev.unwrap_or(0) + 1 + index as u64;
        Edit::decode(bytes)
            .and_then(|edit| edit.apply(&mut text))
            .map_err(|why| format!("revision {rev}: {why}"))?;
    }
    Ok((replay.rev(), text))
}

// --- The worker ------------------------------------------------------------

/// Edit until killed. Only returns on an error.
fn work(dir: &Path, seed: u64) -> Result<(), String> {
    let root = RecoveryRoot::new(dir);
    let acked = Arc::new(AtomicU64::new(0));
    let seen = acked.clone();
    let writer = RecordWriter::spawn(root.clone(), move |event| match event {
        Event::Committed { rev, .. } => {
            seen.fetch_max(rev, Ordering::SeqCst);
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "ack {rev}");
            let _ = out.flush();
        }
        Event::Failed { reason, .. } => {
            eprintln!("writer failed: {reason}");
            std::process::exit(3);
        }
    });

    let mut found = root.scan().map_err(|err| err.to_string())?;
    if found.len() > 1 {
        return Err(format!("{} records at start-up", found.len()));
    }
    let meta = vec![("kind".to_owned(), b"kill-test".to_vec())];
    let (mut rev, mut text) = match found.pop() {
        Some(record) => {
            let (rev, text) = rebuild(&record)?;
            writer.adopt(KEY, record, meta, text.clone().into_bytes());
            (rev, text)
        }
        None => {
            writer.open(KEY, meta, Vec::new());
            (0, String::new())
        }
    };

    let mut since_checkpoint = 0;
    loop {
        rev += 1;
        let edit = generate(seed, rev, &text);
        edit.apply(&mut text)?;
        writer.append(KEY, edit.encode());
        since_checkpoint += 1;
        if since_checkpoint >= CHECKPOINT_EVERY {
            writer.checkpoint(KEY, text.clone().into_bytes());
            since_checkpoint = 0;
        }
        while rev.saturating_sub(acked.load(Ordering::SeqCst)) > MAX_AHEAD {
            std::thread::sleep(Duration::from_micros(200));
        }
    }
}

/// Not a test of its own: the body of the worker process. Run normally, with
/// no worker folder in the environment, it does nothing.
#[test]
fn worker() {
    let Ok(dir) = std::env::var(WORKER_DIR) else {
        return;
    };
    let seed = std::env::var(WORKER_SEED)
        .ok()
        .and_then(|seed| seed.parse().ok())
        .unwrap_or(7);
    if let Err(err) = work(Path::new(&dir), seed) {
        eprintln!("worker: {err}");
        std::process::exit(2);
    }
}

// --- The parent ------------------------------------------------------------

#[test]
fn edits_survive_the_process_being_killed() {
    if std::env::var(WORKER_DIR).is_ok() {
        return;
    }
    let rounds = std::env::var(ROUNDS)
        .ok()
        .and_then(|rounds| rounds.parse().ok())
        .unwrap_or(DEFAULT_ROUNDS);
    let seed = 0x5eed_u64;
    let started = Instant::now();
    let dir = tempfile::tempdir().unwrap();
    let exe = std::env::current_exe().unwrap();
    let mut rng = Rng(seed ^ 0x6b69_6c6c);

    let mut expected = String::new();
    let mut expected_rev = 0_u64;
    let mut acked = 0_u64;
    let (mut early_kills, mut ahead_of_ack, mut longest_ahead) = (0, 0, 0);

    for round in 0..rounds {
        let mut child = Process::new(&exe)
            .args(["--exact", "worker", "--nocapture", "--test-threads=1"])
            .env(WORKER_DIR, dir.path())
            .env(WORKER_SEED, seed.to_string())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the worker starts");
        let stdout = child.stdout.take().expect("stdout is piped");
        let reader = std::thread::spawn(move || {
            BufReader::new(stdout)
                .lines()
                .map_while(Result::ok)
                .collect::<Vec<String>>()
        });

        // Mostly long enough to do real work; sometimes so short that the kill
        // lands while the worker is starting, recovering and adopting.
        let micros = if rng.below(5) == 0 {
            rng.below(20_000)
        } else {
            20_000 + rng.below(60_000)
        };
        std::thread::sleep(Duration::from_micros(micros));
        let exited = child.try_wait().unwrap();
        child.kill().ok();
        let status = child.wait().unwrap();
        let lines = reader.join().expect("the reader thread finishes");
        if exited.is_some() {
            let mut stderr = String::new();
            if let Some(mut pipe) = child.stderr.take() {
                std::io::Read::read_to_string(&mut pipe, &mut stderr).ok();
            }
            panic!(
                "round {round}: the worker stopped by itself ({status}): {}",
                stderr.trim()
            );
        }
        let round_acked = lines
            .iter()
            .filter_map(|line| line.strip_prefix("ack ")?.parse::<u64>().ok())
            .max();
        early_kills += u32::from(round_acked.is_none());
        acked = acked.max(round_acked.unwrap_or(0));

        // Now do what the application does on its next start.
        let fail = |what: String| -> ! {
            panic!("round {round} (acknowledged up to {acked}): {what}");
        };
        let found = RecoveryRoot::new(dir.path())
            .scan()
            .unwrap_or_else(|err| fail(err.to_string()));
        let (rev, text) = match found.as_slice() {
            [] if acked == 0 => continue,
            [] => fail("the record is gone".to_owned()),
            [record] => rebuild(record).unwrap_or_else(|why| fail(why)),
            more => fail(format!("{} records", more.len())),
        };
        drop(found);

        if rev < acked {
            fail(format!(
                "recovered revision {rev}: acknowledged edits were lost"
            ));
        }
        if rev > acked + WRITER_BATCH {
            fail(format!(
                "recovered revision {rev} is further than the writer can have got"
            ));
        }
        ahead_of_ack += u32::from(rev > acked);
        longest_ahead = longest_ahead.max(rev - acked);

        while expected_rev < rev {
            expected_rev += 1;
            generate(seed, expected_rev, &expected)
                .apply(&mut expected)
                .unwrap_or_else(|why| fail(format!("cannot recompute {expected_rev}: {why}")));
        }
        if text != expected {
            fail(format!("recovered text differs at revision {rev}"));
        }
        // The next worker continues from here, so this revision now counts as
        // known.
        acked = rev;
    }

    println!(
        "{rounds} kills in {:.1} s: reached revision {expected_rev} ({} bytes); \
         {early_kills} kills before the first commit; recovered more than was \
         acknowledged {ahead_of_ack} times, by up to {longest_ahead} revisions",
        started.elapsed().as_secs_f64(),
        expected.len()
    );
    assert!(
        expected_rev > 0,
        "no round got as far as committing an edit"
    );
}

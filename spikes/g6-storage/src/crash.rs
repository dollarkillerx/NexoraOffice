//! Kill the process while it edits, over and over, and check every time that
//! what comes back is exactly what it should be.
//!
//! The worker is this same binary started in `worker` mode. It recovers the
//! store, then applies generated commands in a loop: append to the journal,
//! report the revision on stdout, checkpoint and save the document at fixed
//! intervals. The parent kills it after a random delay and then checks:
//!
//! - SQLite's integrity check passes.
//! - Recovery succeeds and lands on a revision the worker acknowledged, or on
//!   the one right after it (committed, but killed before it could say so).
//!   Anything lower means an acknowledged edit was lost.
//! - The recovered text equals the text obtained by recomputing every command
//!   from the beginning. The commands are a pure function of the seed and the
//!   revision, so the parent can do that without trusting anything on disk.
//! - The saved document file, if there is one, is complete, carries a valid
//!   checksum and matches the document as it was at the revision it claims.
//!
//! This is a test of the process dying. It says nothing about power loss, where
//! writes the operating system had accepted can still disappear.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command as Process, Stdio};
use std::time::{Duration, Instant};

use crate::doc::{Rng, Snapshot, checksum, generate};
use crate::store::{Durability, Store, write_atomically};

/// The worker writes a checkpoint once the journal holds this many revisions.
const CHECKPOINT_EVERY: u64 = 64;
/// The worker saves the document file every this many revisions.
const SAVE_EVERY: u64 = 150;
const DOCUMENT_FILE: &str = "document.aster";

type Error = Box<dyn std::error::Error>;

/// Edit until killed. Only returns on an error.
pub fn worker(dir: &Path, seed: u64, durability: Durability) -> Result<(), Error> {
    let mut store = Store::open(dir, durability)?;
    let recovered = store.recover()?;
    let (mut rev, mut text) = (recovered.rev, recovered.text);
    let mut checkpoint_rev = recovered.checkpoint_rev;
    let mut out = std::io::stdout().lock();
    loop {
        rev += 1;
        let command = generate(seed, rev, &text);
        command.apply(&mut text)?;
        store.append(rev, &command.encode())?;
        writeln!(out, "ack {rev}")?;
        out.flush()?;

        // Triggered by journal length, not by the revision number: a checkpoint
        // that was interrupted is then retried at the next edit, which bounds
        // how much recovery ever has to replay.
        if rev - checkpoint_rev >= CHECKPOINT_EVERY {
            store.checkpoint(&Snapshot {
                rev,
                text: text.clone(),
            })?;
            checkpoint_rev = rev;
        }
        if rev.is_multiple_of(SAVE_EVERY) {
            let snapshot = Snapshot {
                rev,
                text: text.clone(),
            };
            write_atomically(&dir.join(DOCUMENT_FILE), &snapshot.encode())?;
            writeln!(out, "saved {rev}")?;
            out.flush()?;
        }
    }
}

#[derive(Debug, Default)]
pub struct Outcome {
    pub kills: u32,
    /// Kills that landed before the worker acknowledged anything.
    pub kills_before_first_ack: u32,
    /// Times recovery found one more revision than had been acknowledged.
    pub unacknowledged_commits: u32,
    /// Times the saved document file was present and verified.
    pub saved_file_checks: u32,
    pub final_rev: u64,
    pub final_len: usize,
    pub longest_replay: u64,
    pub elapsed: Duration,
}

/// Run `kills` rounds against one store. Stops at the first violation.
pub fn run(kills: u32, durability: Durability, seed: u64) -> Result<Outcome, String> {
    let started = Instant::now();
    let dir = tempfile::tempdir().map_err(|err| err.to_string())?;
    let exe = std::env::current_exe().map_err(|err| err.to_string())?;
    let mut rng = Rng::new(seed ^ 0x6b69_6c6c);

    // What the parent knows without reading the store.
    let mut expected = String::new();
    let mut expected_rev = 0_u64;
    let mut acked = 0_u64;
    let mut saved_acked = 0_u64;
    let mut saved_sums: HashMap<u64, u64> = HashMap::new();
    let mut outcome = Outcome::default();

    for round in 0..kills {
        let mut child = Process::new(&exe)
            .arg("worker")
            .arg(dir.path())
            .arg(seed.to_string())
            .arg(durability.name())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| format!("cannot start the worker: {err}"))?;
        let stdout = child.stdout.take().expect("stdout is piped");
        let reader = std::thread::spawn(move || {
            BufReader::new(stdout)
                .lines()
                .map_while(Result::ok)
                .collect::<Vec<String>>()
        });

        // Mostly long enough to do real work, sometimes so short that the kill
        // lands during start-up and recovery.
        let micros = if rng.below(5) == 0 {
            rng.below(3_000)
        } else {
            rng.below(45_000)
        };
        std::thread::sleep(Duration::from_micros(micros));
        let exited = child.try_wait().map_err(|err| err.to_string())?;
        child.kill().ok();
        let status = child.wait().map_err(|err| err.to_string())?;
        let lines = reader
            .join()
            .map_err(|_| "the reader thread panicked".to_owned())?;
        if exited.is_some() {
            let mut stderr = String::new();
            if let Some(mut pipe) = child.stderr.take() {
                std::io::Read::read_to_string(&mut pipe, &mut stderr).ok();
            }
            return Err(format!(
                "round {round}: the worker stopped by itself ({status}): {}",
                stderr.trim()
            ));
        }

        let mut any_ack = false;
        for line in &lines {
            let number = |prefix: &str| {
                line.strip_prefix(prefix)
                    .and_then(|n| n.parse::<u64>().ok())
            };
            if let Some(rev) = number("ack ") {
                acked = acked.max(rev);
                any_ack = true;
            } else if let Some(rev) = number("saved ") {
                saved_acked = saved_acked.max(rev);
            }
        }
        outcome.kills += 1;
        outcome.kills_before_first_ack += u32::from(!any_ack);

        // Now do what the application does on its next start.
        let acknowledged = acked;
        let fail = move |what: String| {
            format!("round {round} (acknowledged up to {acknowledged}): {what}")
        };
        let store = Store::open(dir.path(), durability).map_err(|err| fail(err.to_string()))?;
        store.integrity().map_err(|err| fail(err.to_string()))?;
        let recovered = store.recover().map_err(|err| fail(err.to_string()))?;
        drop(store);

        if recovered.rev < acked {
            return Err(fail(format!(
                "recovered revision {} — acknowledged edits were lost",
                recovered.rev
            )));
        }
        if recovered.rev > acked + 1 {
            return Err(fail(format!(
                "recovered revision {} is further than the worker can have got",
                recovered.rev
            )));
        }
        outcome.unacknowledged_commits += u32::from(recovered.rev == acked + 1);
        outcome.longest_replay = outcome.longest_replay.max(recovered.replayed);

        while expected_rev < recovered.rev {
            expected_rev += 1;
            generate(seed, expected_rev, &expected)
                .apply(&mut expected)
                .map_err(|why| fail(format!("cannot recompute revision {expected_rev}: {why}")))?;
            if expected_rev.is_multiple_of(SAVE_EVERY) {
                saved_sums.insert(expected_rev, checksum(expected.as_bytes()));
            }
        }
        if recovered.text != expected {
            return Err(fail(format!(
                "recovered text differs at revision {}",
                recovered.rev
            )));
        }
        // The worker continues from here, so this revision now counts as known.
        acked = recovered.rev;

        match std::fs::read(dir.path().join(DOCUMENT_FILE)) {
            Err(_) if saved_acked == 0 => {}
            Err(err) => return Err(fail(format!("the saved document is gone: {err}"))),
            Ok(bytes) => {
                let saved = Snapshot::decode(&bytes)
                    .map_err(|why| fail(format!("saved document: {why}")))?;
                if saved.rev < saved_acked || saved.rev > recovered.rev {
                    return Err(fail(format!(
                        "saved document is at revision {}, expected {saved_acked} or later",
                        saved.rev
                    )));
                }
                if saved_sums.get(&saved.rev) != Some(&checksum(saved.text.as_bytes())) {
                    return Err(fail(format!(
                        "saved document does not match revision {}",
                        saved.rev
                    )));
                }
                outcome.saved_file_checks += 1;
            }
        }
    }

    outcome.final_rev = expected_rev;
    outcome.final_len = expected.len();
    outcome.elapsed = started.elapsed();
    Ok(outcome)
}

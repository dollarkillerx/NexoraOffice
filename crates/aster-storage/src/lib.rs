//! Durable storage for Aster Office.
//!
//! The user's file on disk is the only authoritative copy of a document, and
//! the one thing this crate must never do is leave that file half written:
//! [`write_atomically`] replaces a file so that a crash at any point leaves
//! either the old file or the complete new one.
//!
//! What has not been saved yet survives a crash in a recovery record: a
//! [`Journal`] of edits on top of a checkpoint of the whole document, one per
//! document, written by a [`RecordWriter`] on a thread of its own and found
//! again at the next start by [`RecoveryRoot::scan`]. All of it is the store
//! proved in the Phase 0 storage gate (`spikes/g6-storage`), made independent
//! of what a document is.

mod atomic;
mod fingerprint;
mod journal;
mod records;
mod writer;

pub use atomic::{read, write_atomically};
pub use fingerprint::{FileState, Fingerprint, checksum, compare};
pub use journal::{Durability, Journal, JournalError, Replay};
pub use records::{FoundRecord, OwnedRecord, RECORD_DURABILITY, RecoveryRoot};
pub use writer::{Event, RecordKey, RecordWriter};

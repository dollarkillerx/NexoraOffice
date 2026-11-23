//! Durable file access.
//!
//! The user's file on disk is the only authoritative copy of a document, so the
//! one thing this crate must never do is leave that file half written. For now
//! it holds the atomic write proved in the Phase 0 storage gate (G6); the
//! command journal, checkpoints and crash recovery from that gate move here when
//! the product starts using them.

mod atomic;

pub use atomic::{read, write_atomically};

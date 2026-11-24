//! How much a model may send back in one reply. A provider, or anything
//! answering at its address, is not trusted to stay reasonable: without a
//! bound, one endless line or a reply of a million tool calls would eat the
//! memory of the whole program, and a tool name full of control characters
//! would reach the approval dialog and the audit log. A reply past any of
//! these ends the turn with [`AiError::TooLarge`](crate::AiError::TooLarge)
//! (or [`AiError::BadResponse`](crate::AiError::BadResponse) for a bad tool
//! name).
//!
//! The numbers are far above what a real reply needs: the largest output
//! the providers allow is around 128,000 tokens, roughly half a million
//! characters.

/// The longest line of the event stream, in bytes, and the most data one
/// event may carry. Real events are a few hundred bytes; a whole tool input
/// sent in one piece by a server that does not stream it still fits.
pub const MAX_EVENT_BYTES: usize = 1024 * 1024;

/// The most text one reply may stream, in characters, counting the text the
/// user sees and the model's thinking (which is kept and sent back, so it
/// costs memory just the same).
pub const MAX_RESPONSE_CHARS: usize = 2_000_000;

/// The most tool calls one reply may ask for. Models ask for a handful at a
/// time; more than this is a broken or hostile reply.
pub const MAX_TOOL_CALLS: usize = 32;

/// The longest tool name, in bytes. Every tool of `aster-tools` is far
/// shorter; the providers themselves allow 64.
pub const MAX_TOOL_NAME_BYTES: usize = 128;

/// The largest tool input, in bytes of JSON text. A rewritten section of a
/// long document is well within it.
pub const MAX_TOOL_INPUT_BYTES: usize = 1024 * 1024;

/// How many tool calls of one reply run at the same time. The rest wait for
/// a free thread; results still go back in the calls' order.
pub const MAX_PARALLEL_TOOL_CALLS: usize = 4;

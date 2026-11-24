//! The built-in assistant's engine: the conversation model, the model
//! providers (Anthropic and OpenAI-compatible), the agent loop over the
//! shared tools of `aster-tools`, the redactor, the settings and the API key
//! store. There is no interface here; the shell runs [`run`] on a worker
//! thread and shows the [`AgentEvent`]s it sends.
//!
//! Everything blocks on the caller's thread: providers stream over a
//! blocking HTTP client, and tool calls go through a caller-supplied
//! [`ToolExecutor`], which in the application calls
//! [`aster_tools::Invoker::call`] (and may wait minutes for an approval).
//! A turn is stopped with an [`AtomicBool`](std::sync::atomic::AtomicBool)
//! the caller sets.
//!
//! What goes to a provider is kept byte for byte stable from one request to
//! the next where it can be: a fixed system prompt, the tool list sorted by
//! name, and per-turn context only inside the user's message. That is what
//! lets the providers' prompt caches work.

pub mod agent;
pub mod conversation;
pub mod error;
mod http;
pub mod limits;
pub mod prompt;
pub mod provider;
pub mod redact;
pub mod settings;
mod sse;

pub use agent::{
    AgentEvent, AgentOptions, DEFAULT_MAX_STEPS, InvokerExecutor, ToolExecutor, TurnEnd, run,
};
pub use conversation::{
    ContentBlock, Message, Role, TurnContext, back_to_last_request, title, user_message,
};
pub use error::AiError;
pub use prompt::SYSTEM_PROMPT;
pub use provider::{
    ModelInfo, ModelResponse, Preset, Provider, ProviderConfig, ProviderKind, StopReason, ToolDef,
    connect, is_loopback_url, tool_defs, url_host, url_origin, with_scheme,
};
pub use redact::{Redacted, redact};
pub use settings::{AiSettings, KeyStore, KeyStoreError, MemoryKeyStore, SystemKeyStore};

//! The MCP server inside Aster Office. MCP clients (Claude Code, Claude
//! Desktop and the like) reach the same tools as the built-in assistant,
//! through the same [`aster_tools::Invoker`]: one policy, one approval flow,
//! one audit log. Resources (the open documents) are read through that
//! invoker too, and the prompts carry no document text, so neither gets
//! round the policy (see [`protocol`]).
//!
//! The server runs in the application's process, on threads of its own,
//! and listens on a local socket only (see [`endpoint`]). Each connection
//! is one MCP session of newline-delimited JSON-RPC 2.0. Clients that only
//! speak stdio start the `aster-mcp` bridge, which copies bytes between its
//! stdin/stdout and the socket.
//!
//! Local sockets are implemented on macOS and Linux (and other Unix
//! systems). On Windows the crate builds, and [`McpServer::start`] says it
//! is not supported yet.

pub mod endpoint;
#[cfg(unix)]
mod peer;
pub mod protocol;
mod server;
pub mod setup;

pub use endpoint::EndpointError;
pub use server::{
    MAX_CALLS_PER_CONNECTION, MAX_CONNECTIONS, MAX_LINE_BYTES, McpClient, McpConfig, McpEvents,
    McpServer, NoEvents, StartError,
};
pub use setup::{ClientSetup, bridge_path, client_setup};

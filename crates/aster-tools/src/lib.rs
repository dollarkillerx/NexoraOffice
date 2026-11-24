//! The tools that act on documents for the built-in assistant and for MCP
//! clients, and the rules around them. Both callers go through the same
//! [`Invoker`]: one catalogue, one policy, one approval flow and one audit
//! log, the way MySSH does it.
//!
//! The pipeline for every call: look the tool up, check the arguments
//! against its schema, find the document, ask the policy, ask the user when
//! the policy says so, run the tool in the [`Host`] (the application), cut
//! the result down to size, and write one audit record, whatever happened.
//!
//! Nothing here knows about windows or threads. The application implements
//! [`Host`] and [`Approver`]; both are called from the caller's thread (an
//! MCP connection, an assistant run), never the UI thread, so they may block
//! until the UI has answered.

pub mod audit;
pub mod catalogue;
pub mod invoke;
pub mod policy;
pub mod schema;

pub use audit::{Audit, AuditRecord};
pub use catalogue::{Annotations, Capability, ToolSpec, find, tools};
pub use invoke::{
    Approval, ApprovalRequest, Approver, CHANGED_WHILE_ASKING, DocumentInfo, Host, Invoker, Origin,
    ToolError, ToolResult, UNCHECKABLE,
};
pub use policy::{Level, Policy, Reason, Verdict};

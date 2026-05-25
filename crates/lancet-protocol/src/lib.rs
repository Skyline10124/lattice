//! CEP — Controlled Execution Protocol for LLM/Agent Systems.
//!
//! This crate defines the pure protocol data layer for CEP:
//!
//! - **JSON-RPC 2.0** envelope types (`jsonrpc`) — domain-neutral request/response/notification.
//! - **Frame aliases** (`frame`) — CEP-specific type aliases over JSON-RPC (`ToolCall`, `ToolResult`, `ToolEvent`).
//! - **Execution envelopes** (`envelope`) — intent, capabilities, scope, impact, and guard verdicts.
//! - **Typed errors** (`error`) — CEP-11 error families with structured data and retriability.
//! - **Method catalog** (`method`, `methods`, `catalog`) — typed CEP methods and static `CATALOG`.
//!
//! This crate is pure data: no I/O, no runtime, no tokio.

pub mod catalog;
pub mod envelope;
pub mod error;
pub mod frame;
pub mod jsonrpc;
pub mod method;
pub mod methods;

// Public re-exports of common types
pub use catalog::{catalog, CATALOG};
pub use envelope::{
    BaseExecutionEnvelope, Capability, CepMode, Constraints, ExecutionEnvelope, GuardVerdict,
    Impact, ImpactKind, Intent, OnFailure, Scope, VerifyCommand,
};
pub use error::{CepError, CepErrorCode, CepErrorData, CepErrorFamily, Retriability};
pub use frame::{ToolCall, ToolError, ToolEvent, ToolResult};
pub use jsonrpc::{ErrorObject, Id, Notification, Request, Response};
pub use method::{CatalogMethod, CepMethod, MethodDescriptor, RequiredCapability};

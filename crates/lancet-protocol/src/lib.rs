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
pub mod policy;
pub mod provenance;

// Public re-exports of common types
pub use catalog::{catalog, CATALOG};
#[cfg(feature = "inspect-scaffold")]
pub use catalog::{scaffold_catalog, SCAFFOLD_CATALOG};
pub use envelope::{
    BaseExecutionEnvelope, Capability, CepMode, Constraints, ExecutionEnvelope, GuardVerdict,
    Impact, ImpactKind, Intent, OnFailure, Scope, VerifyCommand,
};
pub use error::{CepError, CepErrorCode, CepErrorData, CepErrorFamily, Retriability};
pub use frame::{ToolCall, ToolError, ToolEvent, ToolResult};
pub use jsonrpc::{ErrorObject, Id, Notification, Request, Response};
pub use method::{CatalogMethod, CepMethod, MethodDescriptor, RequiredCapability};
pub use policy::{
    CapabilitySource, EffectiveEnvelopeSnapshot, PolicyActionRequired, PolicyAuditContext,
    PolicyBudgetSnapshot, PolicyDecision, PolicyHook, PolicyInput, PolicyReasonCode,
    SourcedCapability,
};
pub use provenance::{
    InTotoStatement, InTotoSubjectDescriptor, ProvActivity, ProvAgent, ProvEntity,
    ProvGraphFragment, ProvUsed, ProvWasAssociatedWith, ProvWasGeneratedBy, ProvWasInformedBy,
    TurnAttestationPredicate, TurnCallSummary, TurnExecutionTrace,
    CEP_TURN_ATTESTATION_PREDICATE_TYPE,
};

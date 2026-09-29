// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! CEP — Controlled Execution Protocol for LLM/Agent Systems.
//!
//! This crate defines the pure protocol data layer for CEP:
//!
//! - **JSON-RPC 2.0** envelope types (`jsonrpc`) — domain-neutral request/response/notification.
//! - **Frame aliases** (`frame`) — CEP-specific type aliases over JSON-RPC (`ToolCall`, `ToolResult`, `ToolEvent`).
//! - **Cell effect model** (`cell`) — ADR 0004 `CtxOp`/`ExtImpact`/scope/trust/capability core.
//! - **Admission evidence** (`evidence`) — approved capabilities vs observed effects.
//! - **Canonical JSON** (`canonical`) — deterministic bytes for protocol consumers.
//! - **Typed identities** (`identity`) — UUID-v4 `InvocationId`/`TurnId` and `SessionId`.
//! - **Durable bindings** (`binding`) — `InvocationBinding` and parameter digest helpers.
//! - **Typed errors** (`error`) — error families with structured data and retriability.
//!
//! This crate is pure data: no I/O, no runtime, no tokio.

pub mod address;
pub mod binding;
pub mod canonical;
pub mod cell;
pub mod error;
pub mod evidence;
pub mod evidence_triple;
pub mod frame;
pub mod identity;
pub mod jsonrpc;

// Public re-exports of common types
pub use address::{ModelAttemptKey, ModelMessageAddress, ToolCallAddress};
pub use binding::{BindingIdentity, InvocationBinding, arguments_digest};
pub use canonical::canonical_json;
pub use cell::{
    ArtifactIdentity, Capability, Cell, CellKeyError, ChannelKind, CtxOp, DescriptorError,
    ExtImpact, ScopeDecl, ToolDescriptor, ToolOutput, Trust, parse_cell_ref,
};
pub use error::{
    CepError, CepErrorCode, CepErrorData, CepErrorFamily, Retriability, ToolExecutionErrorData,
};
pub use evidence::{
    ADMISSION_EVIDENCE_VERSION, AdmissionEvidence, CapabilityAdmissionPolicy,
    DENIED_OPERATIONS_RECORD_VERSION, DeniedOperation, DeniedOperationsRecord, EffectSource,
    MutationEvidence, MutationInstallation, MutationPrecondition, NetworkEndpoint, NetworkEvidence,
    NetworkFailurePhase, NetworkPolicyVerdict, NetworkResolution, OBSERVED_EFFECTS_RECORD_VERSION,
    ObservedEffect, ObservedEffectsRecord, ObservedScope, ObservedTarget, TlsState, TlsVersion,
    TransportEvidence, WorkspaceVisibilityEvidence,
};
pub use evidence_triple::CellEvidenceTriple;
pub use frame::{ToolCall, ToolError, ToolEvent, ToolResult};
pub use identity::{IdParseError, InvocationId, SessionId, TurnId};
pub use jsonrpc::{ErrorObject, Id, Notification, Request, Response};

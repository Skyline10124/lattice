// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! CEP — Controlled Execution Protocol for LLM/Agent Systems.
//!
//! This crate defines the pure protocol data layer for CEP:
//!
//! - **JSON-RPC 2.0** envelope types (`jsonrpc`) — domain-neutral request/response/notification.
//! - **Frame aliases** (`frame`) — CEP-specific type aliases over JSON-RPC (`ToolCall`, `ToolResult`, `ToolEvent`).
//! - **Execution envelopes** (`envelope`) — intent, capabilities, scope, impact, and guard verdicts.
//! - **Typed errors** (`error`) — error families with structured data and retriability.
//! - **Policy hook API** (`policy`) — deterministic policy seam data types.
//! - **Provenance wire types** (`provenance`) — PROV and unsigned in-toto shapes.
//!
//! This crate is pure data: no I/O, no runtime, no tokio.

pub mod boundary;
pub mod capability;
pub mod envelope;
pub mod error;
pub mod frame;
pub mod jsonrpc;
pub mod policy;
pub mod provenance;

// Public re-exports of common types
pub use boundary::{
    BoundaryDimension, BoundaryPrimitive, EffectiveBoundaryEntry, EffectiveBoundaryReport,
    EffectiveBoundaryStatus, HostProbeEntry, HostProbeReport, HostProbeStatus,
};
pub use capability::{CapabilityPatterns, CapabilityResolveError, RequiredCapability};
pub use envelope::{
    BaseExecutionEnvelope, Capability, CepMode, Constraints, EffectDecl, EffectKindSet,
    EffectScopeKind, ExecutionEnvelope, FailureSemantics, GuardVerdict, Impact, ImpactConfidence,
    ImpactKind, Intent, OnFailure, OverlayBehavior, Scope, VerifyCommand,
};
pub use error::{CepError, CepErrorCode, CepErrorData, CepErrorFamily, Retriability};
pub use frame::{ToolCall, ToolError, ToolEvent, ToolResult};
pub use jsonrpc::{ErrorObject, Id, Notification, Request, Response};
pub use policy::{
    CapabilitySource, EffectiveEnvelopeSnapshot, PolicyActionRequired, PolicyAuditContext,
    PolicyBudgetSnapshot, PolicyDecision, PolicyHook, PolicyInput, PolicyReasonCode,
    SourcedCapability,
};
pub use provenance::{
    CEP_TURN_ATTESTATION_PREDICATE_TYPE, InTotoStatement, InTotoSubjectDescriptor, ProvActivity,
    ProvAgent, ProvEntity, ProvGraphFragment, ProvUsed, ProvWasAssociatedWith, ProvWasGeneratedBy,
    ProvWasInformedBy, TurnAttestationPredicate, TurnCallSummary, TurnExecutionTrace,
};

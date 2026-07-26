// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! CEP — Controlled Execution Protocol for LLM/Agent Systems.
//!
//! This crate defines the pure protocol data layer for CEP:
//!
//! - **JSON-RPC 2.0** envelope types (`jsonrpc`) — domain-neutral request/response/notification.
//! - **Frame aliases** (`frame`) — CEP-specific type aliases over JSON-RPC (`ToolCall`, `ToolResult`, `ToolEvent`).
//! - **Execution envelopes** (`envelope`) — intent, capabilities, scope, impact, and guard verdicts.
//! - **Cell effect model** (`cell`) — ADR 0004 `CtxOp`/`ExtImpact`/scope/trust/capability core.
//! - **Admission evidence** (`evidence`) — approved capabilities vs observed effects.
//! - **Canonical JSON** (`canonical`) — deterministic bytes for measures and attestations.
//! - **Typed errors** (`error`) — error families with structured data and retriability.
//! - **Policy hook API** (`policy`) — deterministic policy seam data types.
//! - **Provenance wire types** (`provenance`) — PROV and unsigned in-toto shapes.
//!
//! This crate is pure data: no I/O, no runtime, no tokio.

pub mod canonical;
pub mod capability;
pub mod cell;
pub mod effect_types;
pub mod error;
pub mod evidence;
pub mod frame;
pub mod jsonrpc;
pub mod policy;
pub mod provenance;

// Public re-exports of common types
pub use canonical::canonical_json;
pub use capability::RequiredCapability;
pub use cell::{
    ArtifactIdentity, Capability, Cell, CellKeyError, ChannelKind, CtxOp, DescriptorError,
    ExtImpact, ScopeDecl, ToolDescriptor, ToolOutput, Trust, parse_cell_ref,
};
pub use effect_types::{
    EffectDecl, EffectKindSet, EffectScopeKind, FailureSemantics, ImpactKind, OverlayBehavior,
};
pub use error::{
    CepError, CepErrorCode, CepErrorData, CepErrorFamily, Retriability, ToolExecutionErrorData,
};
pub use evidence::{
    ADMISSION_EVIDENCE_VERSION, AdmissionEvidence, CapabilityAdmissionPolicy, DeniedOperation,
    EffectSource, OBSERVED_EFFECTS_RECORD_VERSION, ObservedEffect, ObservedEffectsRecord,
    ObservedScope, ObservedTarget,
};
pub use frame::{ToolCall, ToolError, ToolEvent, ToolResult};
pub use jsonrpc::{ErrorObject, Id, Notification, Request, Response};
pub use policy::{
    CepMode, GuardVerdict, PolicyActionRequired, PolicyAuditContext, PolicyAuthorityInput,
    PolicyBudgetSnapshot, PolicyDecision, PolicyHook, PolicyInput, PolicyReasonCode,
};
pub use provenance::{
    CEP_TURN_ATTESTATION_PREDICATE_TYPE, CEP_TURN_ATTESTATION_PREDICATE_TYPE_V2, InTotoStatement,
    InTotoSubjectDescriptor, ProvActivity, ProvAgent, ProvEntity, ProvGraphFragment, ProvUsed,
    ProvWasAssociatedWith, ProvWasGeneratedBy, ProvWasInformedBy, TurnAttestationPredicate,
    TurnCallSummary, TurnExecutionTrace,
};

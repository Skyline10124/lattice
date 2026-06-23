// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! CEP public facade.
//!
//! This crate frames the public CEP interface without merging the lower crates:
//! `lancet-protocol` remains the wire/data contract, `lancet-catalog` remains
//! the method catalog, and `lancet-dispatch` remains the typed dispatch and
//! transport layer. `lancet-cep` collects those interfaces and owns the
//! agent-visible tool exposure and base authority planning helpers.

mod tool;

pub use tool::{
    ToolDefinition, ToolExposure, ToolExposureError, project_tool_definitions,
    resolve_catalog_tool_definitions, resolve_enabled_tools,
};

pub use lancet_catalog::{
    BaseAuthorityRequirement, CATALOG, CatalogMethod,
    CepMethod, MethodDescriptor, RequiredCapability, catalog,
};
#[cfg(feature = "inspect-scaffold")]
pub use lancet_catalog::{SCAFFOLD_CATALOG, scaffold_catalog};
pub use lancet_dispatch::{
    CallControl, Cancellation, CepClient, CepServer, CepTransport, EventSink, MethodRegistry,
    ServerCall, ServerRuntime, call_cancelled, duplicate_registration, wire_code_for_error,
};
pub use lancet_protocol::{
    BoundaryDimension, BoundaryPrimitive, CEP_TURN_ATTESTATION_PREDICATE_TYPE, Capability,
    CapabilitySource, CepError, CepErrorCode, CepErrorData, CepErrorFamily, CepMode, Constraints,
    EffectDecl, EffectKindSet, EffectScopeKind, EffectiveBoundaryEntry, EffectiveBoundaryReport,
    EffectiveBoundaryStatus, EffectiveEnvelopeSnapshot, ErrorObject, ExecutionEnvelope,
    FailureSemantics, GuardVerdict, HostProbeEntry, HostProbeReport, HostProbeStatus, Id, Impact,
    ImpactConfidence, ImpactKind, InTotoStatement, InTotoSubjectDescriptor, Intent, Notification,
    OnFailure, OverlayBehavior, PolicyActionRequired, PolicyAuditContext, PolicyBudgetSnapshot,
    PolicyDecision, PolicyHook, PolicyInput, PolicyReasonCode, ProvActivity, ProvAgent, ProvEntity,
    ProvGraphFragment, ProvUsed, ProvWasAssociatedWith, ProvWasGeneratedBy, ProvWasInformedBy,
    Request, Response, Retriability, Scope, SourcedCapability, ToolCall, ToolError, ToolEvent,
    ToolResult, TurnAttestationPredicate, TurnCallSummary, TurnExecutionTrace, VerifyCommand,
};

pub mod methods {
    //! CEP typed method modules re-exported from `lancet-catalog`.

    pub use lancet_catalog::methods::*;
}

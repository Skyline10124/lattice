// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! LANCET unified public API.
//!
//! This crate is the single front door for driving LANCET's Controlled Execution
//! Protocol (CEP) runtime, independent of any host application. It owns runtime
//! composition — spawning a native CEP server over an in-process transport and
//! handing back a typed [`CepClient`] — plus CEP tool exposure and turn
//! attestation.
//!
//! # Why this crate exists
//!
//! Runtime composition previously lived inside the LATTICE application facade,
//! so anything that wanted to drive LANCET had to pull in the LATTICE session
//! and agent layers. `lancet-api` extracts that composition into a standalone
//! crate that depends only on the LANCET runtime crates, so LANCET can be
//! consumed on its own terms.
//!
//! # Decoupling boundary
//!
//! Policy is supplied as fully-resolved LANCET-native types ([`PolicyConfig`],
//! [`CommandIsolationRuntimeConfig`], [`CepMode`]). This crate has no knowledge
//! of any application's configuration schema; translating an application profile
//! into policy is the host's responsibility.
//!
//! The remaining LATTICE coupling in the LANCET stack is URL/DNS security
//! helpers reused from `lattice-llm`; CEP tool definitions are owned by
//! `lancet-cep`.
//!
//! # Entry points
//!
//! - [`LancetRuntime::build`] — compose a full production runtime from
//!   [`LancetRuntimeConfig`].
//! - [`LancetServer::spawn`] — low-level launcher for callers that build a
//!   bespoke [`ServerRunConfig`].
//! - [`store_turn_attestation`] — persist an unsigned in-toto attestation.

mod attestation;
mod audit;
mod config;
mod doctor;
mod error;
mod runtime;
mod status;

pub use attestation::store_turn_attestation;
pub use audit::{
    AUDIT_TAIL_MAX_BATCHES, AuditQueryOutput, AuditRequest, AuditTailOutput, query_audit,
    tail_audit,
};
pub use config::{
    AuditBacking, ExternalAuditPathError, LancetRuntimeConfig, audit_db_path_for_project,
    audit_db_path_for_project_in, audit_state_root, project_storage_key,
};
pub use doctor::{
    LancetDoctorGate, LancetDoctorOutput, LancetDoctorRequest, run_doctor_checks,
};
pub use error::{LancetError, LancetResult};
pub use runtime::{
    DEFAULT_CHANNEL_CAPACITY, LancetRuntime, LancetRuntimeParts, LancetServer, ServerLifecycle,
};
pub use status::{
    LancetRecentCallEntry, LancetStatusOutput, collect_status, collect_status_with_probe,
};

// --- Curated re-exports: types that appear in this crate's public signatures,
// so consumers can name them without taking direct dependencies on the
// individual LANCET runtime crates. ---

pub use lancet_authority::policy::default::PolicyConfig;
pub use lancet_authority::{CepPolicyConfig, CepTurnContext};
pub use lancet_cep::ToolDefinition;
pub use lancet_dispatch::{CepClient, CepTransport};
pub use lancet_native::enforcement::isolation::CommandIsolationRuntimeConfig;
pub use lancet_native::{
    AuditQueryFilters, CepAuditContext, CepAuditStore, DirectVerifyConfig, ExecutionRouting,
    MethodRegistry, MissingRequiredBoundary, ServerRunConfig, register_production_method_registry,
};
pub use lancet_protocol::{
    BoundaryDimension, BoundaryPrimitive, CepError, EffectiveBoundaryStatus, HostProbeEntry,
    HostProbeReport, HostProbeStatus,
};

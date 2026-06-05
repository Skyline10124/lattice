// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! LANCET unified public API.
//!
//! This crate is the single front door for driving LANCET's Controlled Execution
//! Protocol (CEP) runtime, independent of any host application. It owns runtime
//! composition — spawning a native CEP server over an in-process transport and
//! handing back a typed [`CepClient`] — plus tool projection and turn
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
//! The one remaining LATTICE coupling is [`ToolDefinition`], re-exported from
//! `lattice-llm` because catalog tool projection yields that type. Extracting it
//! to a LANCET-owned (or shared) crate is tracked as future work.
//!
//! # Entry points
//!
//! - [`LancetRuntime::build`] — compose a full production runtime from
//!   [`LancetRuntimeConfig`].
//! - [`LancetServer::spawn`] — low-level launcher for callers that build a
//!   bespoke [`ServerRunConfig`].
//! - [`store_turn_attestation`] — persist an unsigned in-toto attestation.
//! - [`resolve_catalog_tool_definitions`] — project enabled catalog methods to
//!   LLM-facing tool schemas.

mod attestation;
mod config;
mod error;
mod runtime;
mod tools;

pub use attestation::store_turn_attestation;
pub use config::{AuditBacking, LancetRuntimeConfig};
pub use error::{LancetError, LancetResult};
pub use runtime::{
    base_observation_envelope, cep_policy_from_policy_config, LancetRuntime, LancetRuntimeParts,
    LancetServer, ServerLifecycle, DEFAULT_CHANNEL_CAPACITY,
};
pub use tools::{resolve_catalog_tool_definitions, resolve_enabled_tools};

// --- Curated re-exports: types that appear in this crate's public signatures,
// so consumers can name them without taking direct dependencies on the
// individual LANCET runtime crates. ---

pub use lancet_authority::policy::default::PolicyConfig;
pub use lancet_authority::{CepPolicyConfig, CepTurnContext};
pub use lancet_dispatch::{CepClient, CepTransport};
pub use lancet_native::enforcement::isolation::CommandIsolationRuntimeConfig;
pub use lancet_native::{
    register_production_method_registry, to_tool_definitions, AuditQueryFilters, CepAuditContext,
    CepAuditStore, MethodRegistry, ServerRunConfig,
};
pub use lancet_protocol::envelope::{BaseExecutionEnvelope, CepMode};
pub use lancet_protocol::CepError;
pub use lattice_llm::ToolDefinition;

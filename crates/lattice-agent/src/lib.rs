// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! One-attempt model step driver (ADR 0005 §10, #139-B Commit 4).
//!
//! This crate drives exactly one provider attempt per call and owns no
//! persistence, invocation allocation, Tool execution, retry policy,
//! reconciliation, or Turn lifecycle. Multi-step sequencing is owned by
//! `lattice-api::TurnCoordinator`.

pub mod model_step;

pub use model_step::{
    LlmClient, ModelStepAttempt, ModelStepDriver, ModelStepError, ModelStepEvent, ModelStepRequest,
    ModelStepStatus,
};

#[cfg(feature = "test-utils")]
pub mod testing;

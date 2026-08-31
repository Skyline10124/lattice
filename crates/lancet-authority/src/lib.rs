// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! LANCET authority semantics.
//!
//! This crate owns deterministic network, observation, workspace visibility,
//! and Guard L1 policy. Component execution and Cell admission live in
//! `lancet-runtime`.

pub mod guard;
pub mod policy;

pub use policy::visibility::{
    GitFileStatus, HiddenReason, ReadVisibility, ReadVisibilityPolicy, SYSTEM_PATH_EXCLUSIONS,
    under_system_exclusion,
};
pub use policy::{
    BudgetLimits, CepPolicyConfig, CepTurnContext, NetworkPolicyConfig, ObservationPolicyConfig,
};

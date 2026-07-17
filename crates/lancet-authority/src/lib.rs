// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! CEP authority semantics.
//!
//! This crate owns policy evaluation, capability matching, Guard L1 write-scope
//! checks, turn-scoped active envelopes, and effect-region checking. It does not
//! own native handler I/O or server lifecycle wiring.

pub mod effect;
pub mod guard;
pub mod policy;

pub use policy::{
    BudgetLimits, CepPolicyConfig, CepTurnContext, NetworkPolicyConfig, ObservationPolicyConfig,
};
pub use policy::visibility::{
    GitFileStatus, HiddenReason, ReadVisibility, ReadVisibilityPolicy, SYSTEM_PATH_EXCLUSIONS,
    under_system_exclusion,
};

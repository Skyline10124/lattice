// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Effect-region capture and decision layer for the CEP executor.
//!
//! - [`EffectLedger`] — Arc-backed handle for recording observed effects during dispatch.
//! - [`EffectiveEffect`] / [`AuthorizedEffectRegion`] — per-invocation decision types.
//!
//! # Enforcement Model
//!
//! The effect-region contract is runtime-enforced and test-time-enforced only —
//! there is no static type-and-effect proof system.
//!
//! - **Runtime:** [`checker::check_effects_against_region`] compares `observed ⊆ authorized`
//!   after every dispatch; `EffectViolation` on overreach.
//! - **Test-time:** `lancet-native` uses these types from its effect
//!   contract harness to fuzz all catalog methods.
//!
//! Static type-and-effect proving is a v1.3-beta candidate.

pub mod checker;
pub mod ledger;
pub mod region;

pub use ledger::{
    AuthorizedRegionSlot, EffectLedger, EffectSource, ObservedEffect, ObservedLineChange,
    ObservedLineChangeKind, ObservedScope,
};
pub use region::{AuthorizedEffectRegion, EffectiveEffect};

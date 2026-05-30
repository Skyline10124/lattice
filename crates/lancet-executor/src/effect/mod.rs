//! Effect-region capture and decision layer for the CEP executor.
//!
//! - [`EffectLedger`] — Arc-backed handle for recording observed effects during dispatch.
//! - [`EffectiveEffect`] / [`AuthorizedEffectRegion`] — per-invocation decision types (D-03).
//!
//! # Enforcement Model (EFF-05)
//!
//! The effect-region contract is runtime-enforced and test-time-enforced only —
//! there is no static type-and-effect proof system.
//!
//! - **Runtime (EFF-02):** [`checker::check_effects`] compares `observed ⊆ authorized`
//!   after every dispatch; `EffectViolation` on overreach.
//! - **Test-time (EFF-04):** [`crate::testing::effect_harness::effect_contract_harness`]
//!   provides property-based fuzz tests for all catalog methods.
//!
//! Static type-and-effect proving is a v1.3-beta candidate.

pub mod checker;
pub mod ledger;
pub mod region;

pub use ledger::{AuthorizedRegionSlot, EffectLedger, EffectSource, ObservedEffect, ObservedScope};
pub use region::{AuthorizedEffectRegion, EffectiveEffect};

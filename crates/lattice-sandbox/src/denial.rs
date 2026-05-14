//! Compatibility re-exports — the canonical types live in `lattice-policy`.
//!
//! These re-exports keep `use lattice_sandbox::denial::*` working for
//! existing callers. New code should import from `lattice_policy` directly
//! or use the crate-level `lattice_sandbox::*` re-exports.

pub use lattice_policy::{DenialKind, DenialReport, RiskClass};

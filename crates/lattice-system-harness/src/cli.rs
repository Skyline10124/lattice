// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Shared helpers for fast binary black-box CLI scenario tests.
//!
//! Resolve `LATTICE_BIN` and invoke it inside a scenario world with the
//! spec's per-command timeout. Scenario-specific finalizers retain the
//! assertion and failure-artifact responsibilities.

use crate::driver::{CommandOutcome, LatticeCommand, LatticeDriver};
use crate::scenario::ScenarioSpec;
use crate::world::ScenarioWorld;

/// Resolve the standard driver from `LATTICE_BIN`, panicking with the
/// standard message when it is unset or does not point at a file.
pub fn driver() -> LatticeDriver {
    LatticeDriver::from_env().expect("LATTICE_BIN must point to a built lattice binary")
}

/// Run `lattice <args...>` inside `world` with the spec's per-command timeout.
pub fn run(world: &ScenarioWorld, spec: &ScenarioSpec, args: &[&str]) -> CommandOutcome {
    driver()
        .run(
            world,
            &LatticeCommand {
                args: args.iter().map(|s| s.to_string()).collect(),
                timeout: Some(spec.step_timeout()),
            },
        )
        .expect("drive lattice")
}

// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Shared helpers for fast binary black-box CLI scenario tests.
//!
//! Every `tests/fast_*.rs` target drives the built `lattice` binary the same
//! way: resolve `LATTICE_BIN`, invoke it inside the scenario world with the
//! spec's per-command timeout, and route assertion failures through the
//! failure-artifact bundle before panicking. Centralising those three steps
//! here keeps each per-target file focused on its own CLI contract.

use std::process::Output;

use crate::assert::{AssertError, assert_with_failure_artifacts};
use crate::driver::{LatticeCommand, LatticeDriver};
use crate::scenario::ScenarioSpec;
use crate::world::ScenarioWorld;

/// Resolve the standard driver from `LATTICE_BIN`, panicking with the
/// standard message when it is unset or does not point at a file.
pub fn driver() -> LatticeDriver {
    LatticeDriver::from_env().expect("LATTICE_BIN must point to a built lattice binary")
}

/// Run `lattice <args...>` inside `world` with the spec's per-command timeout.
pub fn run(world: &ScenarioWorld, spec: &ScenarioSpec, args: &[&str]) -> Output {
    driver()
        .run(
            world,
            &LatticeCommand {
                args: args.iter().map(|s| s.to_string()).collect(),
                timeout: Some(spec.step_timeout()),
            },
        )
        .expect("drive lattice")
        .output
}

/// Unwrap an assertion result, writing the failure-artifact bundle (redacted
/// stdout/stderr + world context, no extra secrets) before panicking.
pub fn ok_or_artifacts(
    world: &ScenarioWorld,
    name: &str,
    summary: &str,
    out: &Output,
    result: Result<(), AssertError>,
) {
    assert_with_failure_artifacts(world, name, summary, out, &[], result)
        .unwrap_or_else(|e| panic!("{e}"));
}

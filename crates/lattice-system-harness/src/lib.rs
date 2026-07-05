// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! External system harness for the `lattice` binary.
//!
//! This crate must not depend on any `lattice-*`, `lancet-*`, or workspace product
//! crate. It only drives a pre-built binary from `LATTICE_BIN`.

mod artifacts;
mod assert;
pub mod driver;
pub mod scenario;
pub mod world;

pub use assert::{
    AssertError, assert_exit_failure, assert_exit_success, assert_no_secret,
    assert_stderr_contains, assert_stdout_contains, assert_stdout_contains_all,
    assert_with_failure_artifacts,
};
pub use driver::{DriverError, LatticeCommand, LatticeDriver};
pub use scenario::{ScenarioError, ScenarioLayer, ScenarioSpec};
pub use world::{ScenarioWorld, WorldError};

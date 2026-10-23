// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Generated host composition modules.
//!
//! - `host_composition`: **generated** by `cargo xtask tool-codegen` from the
//!   Tool manifests declaring `distribution = "builtin"`. It carries their
//!   `inventory` registrations and the host-pinned `EXPECTED_COMPONENTS`
//!   identities, and is the only thing that links a Tool into a built runtime.
//!   Non-empty since #29 embedded `session__todo_write`.
//! - `noop_fixture`: a fixed no-op fixture for cross-crate integration tests
//!   (`test-utils` feature). No caller-controlled registrations, paths,
//!   bytes, handlers, or approval set.

pub(crate) mod host_composition;

#[cfg(feature = "test-utils")]
pub(crate) mod noop_fixture;

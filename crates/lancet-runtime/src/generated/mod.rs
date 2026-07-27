// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Generated host composition modules.
//!
//! - `host_composition`: the production `EXPECTED_COMPONENTS` list. Empty
//!   until a production Tool migrates to a Component
//!   (#29–#32 (Codeberg #7–#10)).
//! - `noop_fixture`: a fixed no-op fixture for cross-crate integration tests
//!   (`test-utils` feature). No caller-controlled registrations, paths,
//!   bytes, handlers, or approval set.

pub(crate) mod host_composition;

#[cfg(feature = "test-utils")]
pub(crate) mod noop_fixture;

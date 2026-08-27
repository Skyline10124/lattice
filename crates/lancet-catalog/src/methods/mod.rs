// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Feature-gated native method scaffolds.
//!
//! Production built-ins are Component Tools; their source manifests are the
//! sole contract authority.

#[cfg(feature = "inspect-scaffold")]
pub mod scaffold;

#[cfg(feature = "inspect-scaffold")]
pub use scaffold::{Inspect, Navigate, ScaffoldStubResult};

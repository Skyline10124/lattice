// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Feature-gated native handler scaffolds.
//!
//! Production methods execute as embedded Components. There is no native
//! fallback when a Component is absent.

#[cfg(feature = "inspect-scaffold")]
mod scaffold;

#[cfg(feature = "inspect-scaffold")]
pub use scaffold::register_inspect_scaffold_methods;

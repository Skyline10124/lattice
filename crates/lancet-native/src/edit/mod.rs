// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Structured patch parsing and result types for editing tools.
//!
//! This module owns the typed edit IR, filesystem apply engine, and
//! recovery-oriented diagnostics used by the `patch` tool.

pub mod apply;
pub mod commit_baseline;
pub mod diagnostics;
pub mod hash;
pub mod parser;
pub mod types;

pub use hash::content_hash;

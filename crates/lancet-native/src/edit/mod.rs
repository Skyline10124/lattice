// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Structured patch parsing and result types for editing tools.
//!
//! This module owns the typed edit IR, filesystem apply engine, the shared
//! single-file write byte cap, and recovery-oriented diagnostics used by the
//! `patch` tool.

pub mod apply;
pub mod diagnostics;
pub mod hash;
pub(crate) mod limits;
pub mod parser;
pub(crate) mod storage;
pub mod types;

pub use hash::content_hash;

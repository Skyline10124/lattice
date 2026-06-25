// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Bounded stream output metadata.
//!
//! The host-side `execute_argv` / `ExecuteConfig` process execution path was
//! removed when MSB/Upper became the sole execution backend. This module now
//! only retains [`BoundedStreamOutput`], which is used by command handlers to
//! describe truncated stdout/stderr metadata from backend execution.

/// Output metadata from a bounded stream read.
#[derive(Debug)]
pub struct BoundedStreamOutput {
    /// Total bytes read from the stream (may exceed buffer capacity).
    pub total_bytes: usize,
    /// Whether the output was truncated (total_bytes > buffer capacity).
    pub truncated: bool,
}

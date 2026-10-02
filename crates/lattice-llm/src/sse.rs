// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Shared SSE (Server-Sent Events) limits for provider stream parsers.
//!
//! Provider modules apply these bounds while consuming streaming HTTP responses.

/// Maximum number of SSE events processed per stream.
pub const MAX_SSE_EVENTS: usize = 100_000;

/// Maximum size in bytes of a single SSE `data:` field value.
pub const MAX_SSE_DATA_SIZE: usize = 256 * 1024;

/// Maximum total buffer size across all in-flight SSE fields.
pub const MAX_SSE_BUFFER_SIZE: usize = 2 * 1024 * 1024;

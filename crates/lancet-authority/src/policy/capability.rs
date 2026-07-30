// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Invocation facts and session-state operation types for CEP policy resolution.

// ── Invocation facts ────────────────────────────────────────────────────

/// Concrete invocation facts for policy resolution.
///
/// All fields are present for coverage and audit evidence even if the
/// resolver does not consume every field for every method kind.
#[derive(Clone, Debug, Default)]
pub struct InvocationFacts {
    /// Target path (for ReadPath/WritePath) — canonical absolute path.
    pub path: Option<String>,
    /// Normalized relative path (for relative pattern matching).
    pub normalized_path: Option<String>,
    /// Command argv (for RunCommand).
    pub argv: Option<Vec<String>>,
    /// Expanded sub-command argvs for a validated inline-shell invocation.
    pub sub_argvs: Vec<Vec<String>>,
    /// Command working directory.
    pub cwd: Option<std::path::PathBuf>,
    /// Environment variable keys supplied in the invocation.
    pub env_keys: Vec<String>,
    /// URL host (for FetchUrl).
    pub host: Option<String>,
}

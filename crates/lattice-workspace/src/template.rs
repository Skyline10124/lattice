// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Warm-template metadata and naming helpers.
//!
//! The SDK owns snapshot bytes under `~/.microsandbox/snapshots/`; LATTICE only
//! tracks the name, lockfile hash (for staleness detection), and creation time.
//!
//! # Invariant
//!
//! Template build must **never** call `.env()` or inject file credentials.
//! Credentials are applied only on the per-turn `create_upper` / `restore_upper`
//! path so that shared template snapshots never contain resolved secret values.

use std::time::SystemTime;

/// Per-project warm template metadata.
///
/// The SDK stores snapshot bytes; `WarmTemplate` is the LATTICE-side record of
/// naming and staleness policy for a given project's dependency cache snapshot.
#[derive(Clone, Debug)]
pub struct WarmTemplate {
    /// SDK snapshot name, e.g. `"proj-<sha256_prefix>-template"`.
    pub name: String,
    /// SHA-256 hex digest of the lockfile(s) used to detect cache staleness
    /// (e.g. `Cargo.lock`, `package.json`).  Compare with the current lockfile
    /// hash to decide whether a template rebuild is needed.
    pub baseline_lockfile_hash: String,
    /// When this template was created.
    pub created_at: SystemTime,
}

impl WarmTemplate {
    /// Canonical SDK snapshot name for a project identified by `project_hash`.
    ///
    /// `project_hash` should be a short hex digest of the project root path or
    /// a stable project identifier (e.g. first 16 hex chars of SHA-256).
    pub fn snapshot_name_for_project(project_hash: &str) -> String {
        format!("proj-{project_hash}-template")
    }

    /// Canonical SDK snapshot name for the **git-ready** per-project warm
    /// template variant of `project_hash`.
    ///
    /// This is the git-ready sibling of
    /// [`snapshot_name_for_project`][Self::snapshot_name_for_project]: a template
    /// built once by
    /// [`MicrosandboxWorkspace::build_git_ready_template`](crate::microsandbox::MicrosandboxWorkspace::build_git_ready_template)
    /// with `git` baked in, so every Upper restored from it has `git` available
    /// without a per-Upper `apt-get install git` — including under
    /// [`EgressPolicy::Blocked`](crate::EgressPolicy::Blocked), where apt-get
    /// could not reach the network at all.
    pub fn git_ready_snapshot_name_for_project(project_hash: &str) -> String {
        format!("proj-{project_hash}-git-ready-template")
    }

    /// Unique per-turn Upper name derived from session and turn prefixes.
    ///
    /// These names are **never** reused as template snapshot names; template
    /// snapshots use [`snapshot_name_for_project`][Self::snapshot_name_for_project].
    pub fn upper_name_for_turn(session_prefix: &str, turn_prefix: &str) -> String {
        format!("upper-{session_prefix}-{turn_prefix}")
    }

    /// Return `true` if `current_lockfile_hash` differs from the hash this
    /// template was built with, indicating a rebuild is needed.
    pub fn is_stale(&self, current_lockfile_hash: &str) -> bool {
        self.baseline_lockfile_hash != current_lockfile_hash
    }
}

// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! SQLite→Git session graph migration (D-02, D-15).
//!
//! One-shot, restartable migration that reads the legacy SQLite session graph
//! via a self-contained SQL extractor (D-03) and writes equivalent Git session
//! commits and refs into a standalone bare repository. The full pipeline
//! (extract → write → verify → archive) is wired across Plans 01–04; this
//! module owns the public API surface and the bare-repo initialization.

use std::path::PathBuf;

use crate::{Result, SessionError};

pub(crate) mod extract;
pub(crate) mod write;

/// Options driving a one-shot migration run (D-01).
pub struct MigrateOptions {
    /// Path to the legacy `sessions.db` SQLite file (read-only source).
    pub source_db: PathBuf,
    /// Path to the standalone bare Git repository (migration target).
    /// Auto-initialized via `git2::Repository::init_bare` if absent (D-15).
    pub target_repo: PathBuf,
    /// Read + plan only; perform no writes to the target repo.
    pub dry_run: bool,
    /// Re-run equivalence checks against an already-written target without
    /// re-writing (D-12).
    pub verify_only: bool,
    /// Physically delete the source `sessions.db` after successful migration
    /// instead of archiving it (default is archive — D-18).
    pub purge_source: bool,
}

/// Outcome of a migration run.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MigrationResult {
    pub sessions_migrated: usize,
    pub commits_migrated: usize,
    pub refs_migrated: usize,
    pub verification_passed: bool,
    /// Path the source was archived to (`None` if not archived).
    pub source_archived: Option<PathBuf>,
}

/// Public entry point — the full extract → write → verify → archive pipeline.
///
/// The pipeline is wired in Plans 02 and 04 (write + verify steps). Plan 01
/// provides the signature and the bare-repo initialization so the CLI can be
/// landed against a stable surface.
pub fn run_migration(opts: &MigrateOptions) -> Result<MigrationResult> {
    let _repo = open_or_init_target(&opts.target_repo)?;
    // TODO(39-02): extract → write. TODO(39-04): verify. TODO(39-02): archive.
    let _ = opts.dry_run;
    let _ = opts.verify_only;
    let _ = opts.purge_source;
    Ok(MigrationResult::default())
}

/// Open the target bare repo, initializing it in-place if it does not exist
/// (D-15). This is the migration-owned init; `GitSessionGraphStore::repo()`
/// has its own auto-init safety net for production use after the SQLite store
/// is retired (D-19).
pub(crate) fn open_or_init_target(path: &std::path::Path) -> Result<git2::Repository> {
    match git2::Repository::open(path) {
        Ok(r) => Ok(r),
        Err(_) => git2::Repository::init_bare(path)
            .map_err(|e| SessionError::Git(format!("init bare target repo: {e}"))),
    }
}

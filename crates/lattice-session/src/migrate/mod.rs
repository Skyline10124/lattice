// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! SQLite→Git session graph migration (D-02, D-15).
//!
//! One-shot, restartable migration that reads the legacy SQLite session graph
//! via a self-contained SQL extractor (D-03) and writes equivalent Git session
//! commits and refs into a standalone bare repository. The full pipeline
//! (extract → write → verify → archive) is wired here; the extractor is in
//! `extract.rs`, the writer in `write.rs`, and the verifier in `verify.rs`.

use std::path::PathBuf;

use crate::{Result, SessionError};

pub(crate) mod extract;
pub(crate) mod verify;
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
/// Write order (D-10): commit objects → refs → sidecars → idmap fsync.
/// Success gate (D-14): the source is archived only if write completes AND
/// equivalence verification passes. On verification failure the source is
/// left in place and `verification_passed` is `false` (D-12/D-18).
pub fn run_migration(opts: &MigrateOptions) -> Result<MigrationResult> {
    let repo = open_or_init_target(&opts.target_repo)?;

    // --verify-only: skip extraction/writing, just re-check equivalence (D-12).
    if opts.verify_only {
        let idmap = extract::Idmap::load(&opts.target_repo)?;
        let conn = rusqlite::Connection::open(&opts.source_db).map_err(|source| {
            SessionError::Database {
                path: opts.source_db.clone(),
                source,
            }
        })?;
        let passed = verify::verify_equivalence(&conn, &repo, &idmap)?;
        return Ok(MigrationResult {
            verification_passed: passed,
            source_archived: None,
            ..Default::default()
        });
    }

    // Open the source DB. The connection is held through extraction and
    // verification, then explicitly dropped before archiving so the SQLite
    // file handle is released (Linux allows rename of open files, but dropping
    // first is the correct hygiene).
    let conn = rusqlite::Connection::open(&opts.source_db).map_err(|source| {
        SessionError::Database {
            path: opts.source_db.clone(),
            source,
        }
    })?;

    let sessions = extract::extract_all_with_conn(&conn)?;

    // --dry-run: report the plan, no writes (D-01).
    if opts.dry_run {
        let total_commits: usize = sessions.iter().map(|s| s.commits.len()).sum();
        let total_refs: usize = sessions.iter().map(|s| s.refs.len()).sum();
        // Best-effort log to stderr so the CLI can surface it.
        eprintln!(
            "dry-run: {} session(s), {} commit(s), {} ref(s) would be migrated",
            sessions.len(),
            total_commits,
            total_refs,
        );
        return Ok(MigrationResult {
            sessions_migrated: sessions.len(),
            commits_migrated: total_commits,
            refs_migrated: total_refs,
            verification_passed: false,
            source_archived: None,
        });
    }

    // Load existing idmap for restartability (D-09).
    let mut idmap = extract::Idmap::load(&opts.target_repo)?;

    // Write (D-10 order).
    let stats = write::write_migration(&repo, &sessions, &mut idmap, &opts.target_repo)?;

    // Verify (D-11/D-14). Real verifier from Plan 04.
    let passed = verify::verify_equivalence(&conn, &repo, &idmap)?;
    if !passed {
        // D-12/D-18: do NOT archive the source on verification failure.
        return Ok(MigrationResult {
            sessions_migrated: stats.sessions_migrated,
            commits_migrated: stats.commits_migrated,
            refs_migrated: stats.refs_migrated,
            verification_passed: false,
            source_archived: None,
        });
    }

    // Archive the source (D-18). Drop the connection first so the file handle
    // is released before rename/remove.
    drop(conn);
    let archived = archive_source(opts)?;

    Ok(MigrationResult {
        sessions_migrated: stats.sessions_migrated,
        commits_migrated: stats.commits_migrated,
        refs_migrated: stats.refs_migrated,
        verification_passed: true,
        source_archived: archived,
    })
}

/// Archive or purge the source DB (D-18). Returns the archive path, or `None`
/// if `--purge-source` removed the file.
fn archive_source(opts: &MigrateOptions) -> Result<Option<PathBuf>> {
    if opts.purge_source {
        std::fs::remove_file(&opts.source_db).map_err(|source| SessionError::Io {
            operation: "purging source db",
            path: opts.source_db.clone(),
            source,
        })?;
        return Ok(None);
    }
    let ts = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    let mut archived = opts.source_db.clone();
    let mut name = archived
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "sessions.db".to_string());
    name.push_str(&format!(".migrated-{ts}"));
    archived.set_file_name(name);
    std::fs::rename(&opts.source_db, &archived).map_err(|source| SessionError::Io {
        operation: "archiving source db",
        path: opts.source_db.clone(),
        source,
    })?;
    Ok(Some(archived))
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

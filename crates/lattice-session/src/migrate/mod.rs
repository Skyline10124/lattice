// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! SQLite→Git session graph migration.
//!
//! One-shot, restartable migration that reads the legacy SQLite session graph
//! via a self-contained SQL extractor and writes equivalent Git session
//! commits and refs into a standalone bare repository. The full pipeline
//! (extract → write → verify → archive) is wired here; the extractor is in
//! `extract.rs`, the writer in `write.rs`, and the verifier in `verify.rs`.

use std::path::PathBuf;

use crate::{Result, SessionError};

pub(crate) mod extract;
pub(crate) mod verify;
pub(crate) mod write;

#[cfg(test)]
mod tests;

/// Options driving a one-shot migration run.
pub struct MigrateOptions {
    /// Path to the legacy SQLite session DB (read-only source). Discovered via
    /// `find_legacy_session_db` when the caller does not override `--source`.
    pub source_db: PathBuf,
    /// Path to the standalone bare Git repository (migration target).
    /// Auto-initialized via `git2::Repository::init_bare` if absent.
    pub target_repo: PathBuf,
    /// Read + plan only; perform no writes to the target repo.
    pub dry_run: bool,
    /// Re-run equivalence checks against an already-written target without
    /// re-writing.
    pub verify_only: bool,
    /// Physically delete the source DB after successful migration
    /// instead of archiving it (default is archive).
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
/// Write order: commit objects → refs → sidecars → idmap fsync.
/// Success gate: the source is archived only if write completes AND
/// equivalence verification passes. On verification failure the source is
/// left in place and `verification_passed` is `false`.
pub fn run_migration(opts: &MigrateOptions) -> Result<MigrationResult> {
    // --verify-only: re-check equivalence against an already-written target.
    // NO auto-init — the target must already exist from a prior run.
    // `open_or_init_target` is deferred past this branch so
    // verify-only never creates a target as a side effect.
    if opts.verify_only {
        // fail loudly if the source DB is absent. After a successful run
        // the source was archived to `<db>.migrated-<ts>`, so a bare
        // `--verify-only` against the original path would have SQLite *create*
        // an empty db, extract zero sessions, and vacuously pass — recreating an
        // empty db that re-trips the startup guard. Point `--source` at the
        // archived file instead.
        if !opts.source_db.exists() {
            return Err(SessionError::Io {
                operation: "verify-only: source db not found (point --source at the archived <db>.migrated-<ts>)",
                path: opts.source_db.clone(),
                source: std::io::Error::from(std::io::ErrorKind::NotFound),
            });
        }
        // Open existing target — error if absent (no auto-init side effect).
        let repo = git2::Repository::open(&opts.target_repo).map_err(|e| {
            SessionError::Git(format!(
                "verify-only: target repo not found at {}: {e} (run a full migration first)",
                opts.target_repo.display(),
            ))
        })?;
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

    // fail clearly when the source is missing instead of having SQLite
    // create an empty db, migrate zero sessions, and then archive a
    // freshly-created empty file.
    if !opts.source_db.exists() {
        return Err(SessionError::Io {
            operation: "migrate: source db not found",
            path: opts.source_db.clone(),
            source: std::io::Error::from(std::io::ErrorKind::NotFound),
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

    // --dry-run: report the plan, no writes. dry-run
    // performs NO target filesystem access — not even an existence check or
    // open. `open_or_init_target` is NOT called here.
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

    // Normal migration — NOW open/init the target. This is the only
    // branch that may create the target repo.
    let repo = open_or_init_target(&opts.target_repo)?;

    // Load existing idmap for restartability.
    let mut idmap = extract::Idmap::load(&opts.target_repo)?;

    // Write order.
    let stats = write::write_migration(&repo, &sessions, &mut idmap, &opts.target_repo)?;

    // Verify. Real verifier from Plan 04.
    let passed = verify::verify_equivalence(&conn, &repo, &idmap)?;
    if !passed {
        // do NOT archive the source on verification failure.
        return Ok(MigrationResult {
            sessions_migrated: stats.sessions_migrated,
            commits_migrated: stats.commits_migrated,
            refs_migrated: stats.refs_migrated,
            verification_passed: false,
            source_archived: None,
        });
    }

    // Archive the source. Drop the connection first so the file handle
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

/// Archive or purge the source DB. Returns the archive path, or `None`
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
    // the archive name derives from the actual source filename
    // (e.g. `session_v12.db.migrated-<ts>`). The fallback is a last-resort
    // default only triggered by pathological paths where `file_name()` is
    // `None`; it matches the probe's primary candidate for consistency.
    let base_name = opts
        .source_db
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "session.db".to_string());

    // never overwrite an existing archive. At second granularity a
    // re-run within the same wall-clock second (e.g. the user restored
    // sessions.db and re-ran) would otherwise clobber an earlier backup via
    // std::fs::rename — the opposite of the data-safety intent. If the
    // timestamped name already exists, append a numeric uniquifier.
    let candidate = |suffix: Option<usize>| -> PathBuf {
        let mut p = opts.source_db.clone();
        let name = match suffix {
            None => format!("{base_name}.migrated-{ts}"),
            Some(n) => format!("{base_name}.migrated-{ts}.{n}"),
        };
        p.set_file_name(name);
        p
    };
    let mut archived = candidate(None);
    let mut n = 1;
    while archived.exists() {
        archived = candidate(Some(n));
        n += 1;
    }

    std::fs::rename(&opts.source_db, &archived).map_err(|source| SessionError::Io {
        operation: "archiving source db",
        // point at the destination so "exists/permission" diagnostics
        // name the archive file, not the source.
        path: archived.clone(),
        source,
    })?;
    Ok(Some(archived))
}

/// Open the target bare repo, initializing it in-place if it does not exist
/// This is the migration-owned init; `GitSessionGraphStore::repo()`
/// has its own auto-init safety net for production use after the SQLite store
/// is retired.
pub(crate) fn open_or_init_target(path: &std::path::Path) -> Result<git2::Repository> {
    match git2::Repository::open(path) {
        Ok(r) => Ok(r),
        Err(_) => git2::Repository::init_bare(path)
            .map_err(|e| SessionError::Git(format!("init bare target repo: {e}"))),
    }
}

/// Probe `session_dir` for a legacy SQLite session DB, returning the first
/// existing candidate in priority order.
///
/// Priority: `session_v12.db` (the real legacy `SqliteSessionGraphStore`
/// filename), `session_v11.db`, `session.db`, `sessions.db` (plural — the
/// pre-gap-closure assumption, kept as a final fallback for hand-created
/// test DBs). Returns `None` if no candidate exists (fresh install or
/// already-migrated per archive semantics).
///
/// Uses only `Path::exists()` — no side effects, no SQLite open.
pub fn find_legacy_session_db(session_dir: &std::path::Path) -> Option<PathBuf> {
    const CANDIDATES: &[&str] = &[
        "session_v12.db",
        "session_v11.db",
        "session.db",
        "sessions.db",
    ];
    for name in CANDIDATES {
        let candidate = session_dir.join(name);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

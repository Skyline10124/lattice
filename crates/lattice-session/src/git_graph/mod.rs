// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Git-backed session graph store.
//!
//! Implements `SessionGraphStore` over the project's Git object database via
//! gix. Session commits are immutable Git objects under
//! `refs/lattice/sessions/**`; mutable session metadata lives in sidecar files
//! under `.lattice/sessions/`. Candidate refs under `refs/lattice/candidates/**`
//! keep clean-turn commit ranges reachable.
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use crate::graph::{
    GraphDiff, GraphHead, GraphRef, GraphSession, HeadTranscript, PreparedTurnCommit, ResetResult,
    SessionGraphStore, SessionGraphSummary, SessionStatus, TurnCommit, TurnCommitInput,
    TurnLogEntry,
};
use crate::{Result, SessionError};

mod commit;
pub(crate) mod helpers;
mod refs;
mod session;

#[cfg(test)]
mod tests;

pub(crate) const DEFAULT_BRANCH: &str = "main";

/// Git-backed session graph store.
///
/// Holds the project root path and a lazily opened `gix::Repository`. The
/// repository is opened on first access and cached under a mutex (`Repository`
/// is `Send` but not `Sync`).
pub struct GitSessionGraphStore {
    pub(crate) dir: PathBuf,
    pub(crate) repo: Mutex<Option<gix::Repository>>,
}

/// RAII handle around the cached repository. Derefs to `gix::Repository`.
pub(crate) struct RepoHandle<'a> {
    guard: MutexGuard<'a, Option<gix::Repository>>,
}

impl<'a> Deref for RepoHandle<'a> {
    type Target = gix::Repository;

    fn deref(&self) -> &gix::Repository {
        self.guard
            .as_ref()
            .expect("repository initialized by GitSessionGraphStore::repo()")
    }
}

impl GitSessionGraphStore {
    /// Create a new store rooted at `dir` (the project root containing `.git`).
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        let dir = dir.into();
        Self {
            dir,
            repo: Mutex::new(None),
        }
    }

    /// Return the project root path.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Acquire a locked handle to the repository, opening it on first access.
    ///
    /// If open fails (the directory has no Git repository yet), auto-initializes
    /// a bare repo in-place. This is the fresh-install safety net: a new install
    /// with no pre-existing repo auto-creates one on first session instead of
    /// crashing. Existing installs and tests that pre-init a non-bare repo are
    /// unaffected — open succeeds and auto-init never runs.
    pub(crate) fn repo(&self) -> Result<RepoHandle<'_>> {
        let mut guard = self.repo.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_none() {
            let repo = match gix::open_opts(&self.dir, gix::open::Options::isolated()) {
                Ok(r) => r,
                Err(open_err) => {
                    if let Err(e) = std::fs::create_dir_all(&self.dir) {
                        return Err(SessionError::Git(format!(
                            "create repository directory: {e} (open failed: {open_err})"
                        )));
                    }
                    // Prefer bare init when the directory is empty — this matches
                    // the sessions.git store layout (path itself is the git dir).
                    // Fall back to a worktree init when the directory already has
                    // non-git content (e.g. a project root with source files).
                    let is_empty = std::fs::read_dir(&self.dir)
                        .map(|mut d| d.next().is_none())
                        .unwrap_or(true);
                    // Reopening after a CWD-recovered retry: the partially
                    // created repo is on disk, so plain `open` is the right
                    // resolution for both layouts.
                    let reopen = || {
                        gix::open_opts(&self.dir, gix::open::Options::isolated())
                            .map_err(|e| format!("{e:?}"))
                    };
                    if is_empty {
                        let init_bare = || gix::init_bare(&self.dir).map_err(|e| format!("{e:?}"));
                        match init_recovering_cwd(init_bare, reopen) {
                            Ok(r) => r,
                            Err(init_err) => {
                                return Err(SessionError::Git(format!(
                                    "open repository: {open_err}; init_bare: {init_err}"
                                )));
                            }
                        }
                    } else {
                        let init_worktree = || {
                            gix::ThreadSafeRepository::init(
                                &self.dir,
                                gix::create::Kind::WithWorktree,
                                gix::create::Options {
                                    destination_must_be_empty: Some(false),
                                    ..Default::default()
                                },
                            )
                            .map(|r| r.to_thread_local())
                            .map_err(|e| format!("{e:?}"))
                        };
                        match init_recovering_cwd(init_worktree, reopen) {
                            Ok(r) => r,
                            Err(init_err) => {
                                return Err(SessionError::Git(format!(
                                    "open repository: {open_err}; init: {init_err}"
                                )));
                            }
                        }
                    }
                }
            };
            *guard = Some(repo);
        }
        Ok(RepoHandle { guard })
    }
}

/// Run a `gix` init, retrying once from a known-good directory if it failed
/// only because the process has no valid current directory.
///
/// `gix` reads the process CWD even when the target path is absolute, so an
/// init into an absolute path fails with `Could not obtain the current
/// directory` once the CWD has been removed underneath the process — a deleted
/// checkout, an unmounted volume, or (in tests) a dropped `TempDir` that some
/// other test had `set_current_dir`'d into. The store's own target directory is
/// unaffected, so the operation is retryable: point the CWD somewhere that
/// exists and try again.
///
/// Pre-checking `current_dir()` would be a TOCTOU race — the directory can be
/// removed between the check and gix's own read — so this recovers from the
/// failure instead of predicting it.
///
/// The retry must tolerate an already-created repository: gix creates the `.git`
/// directory *before* the step that needs the CWD, so the failed first attempt
/// leaves it behind and a naive retry fails with `Refusing to initialize the
/// existing '...' directory`. `reopen` is therefore consulted on any
/// second-attempt failure, and only its own failure is reported.
///
/// `lattice-session`'s test fixtures carry the same recovery
/// (`test_utils::init_empty_repo`); this is the production counterpart.
fn init_recovering_cwd<T, E: std::fmt::Debug>(
    mut init: impl FnMut() -> std::result::Result<T, E>,
    reopen: impl FnOnce() -> std::result::Result<T, E>,
) -> std::result::Result<T, E> {
    match init() {
        Ok(repo) => Ok(repo),
        // Match on the rendered form: the variant nesting differs between the
        // bare and worktree init error types, and a gix bump may reshape it.
        Err(err) if format!("{err:?}").contains("CurrentDir") => {
            if std::env::set_current_dir(std::env::temp_dir()).is_err() {
                return Err(err);
            }
            match init() {
                Ok(repo) => Ok(repo),
                // The partially-created repo from the first attempt is the
                // expected state here, not an error.
                Err(_) => reopen(),
            }
        }
        Err(err) => Err(err),
    }
}

impl SessionGraphStore for GitSessionGraphStore {
    fn create_session(
        &self,
        session_id: &str,
        label: Option<&str>,
        active_agent_name: &str,
        project_target_branch: &str,
    ) -> Result<GraphSession> {
        self.create_session_impl(session_id, label, active_agent_name, project_target_branch)
    }

    fn open_session(&self, session_id: &str) -> Result<GraphSession> {
        self.open_session_impl(session_id)
    }

    fn status(&self, session_id: &str) -> Result<SessionStatus> {
        self.status_impl(session_id)
    }

    fn list(&self) -> Result<Vec<SessionGraphSummary>> {
        self.list_impl()
    }

    fn log(&self, session_id: &str) -> Result<Vec<TurnLogEntry>> {
        self.log_impl(session_id)
    }

    fn head_transcript(&self, session_id: &str) -> Result<HeadTranscript> {
        self.head_transcript_impl(session_id)
    }

    fn commit_turn(&self, input: TurnCommitInput) -> Result<TurnCommit> {
        self.commit_turn_impl(input)
    }

    fn checkout(&self, session_id: &str, target: &str) -> Result<GraphHead> {
        self.checkout_impl(session_id, target)
    }

    fn branch(&self, session_id: &str, name: &str, from: Option<&str>) -> Result<GraphRef> {
        self.branch_impl(session_id, name, from)
    }

    fn set_active_agent(&self, session_id: &str, agent_name: &str) -> Result<GraphSession> {
        self.set_active_agent_impl(session_id, agent_name)
    }

    fn diff(&self, session_id: &str, from: &str, to: &str) -> Result<GraphDiff> {
        self.diff_impl(session_id, from, to)
    }

    fn reset(&self, session_id: &str, target: &str) -> Result<ResetResult> {
        self.reset_impl(session_id, target)
    }

    fn commit_prepared_turn(&self, prepared: PreparedTurnCommit) -> Result<TurnCommit> {
        self.commit_prepared_turn_impl(prepared)
    }

    fn get_turn_commit(&self, session_id: &str, commit_id: &str) -> Result<Option<TurnCommit>> {
        self.get_turn_commit_impl(session_id, commit_id)
    }
}

// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Git-backed session graph store.
//!
//! Implements `SessionGraphStore` over the project's Git object database.
//! Session commits are immutable Git objects under `refs/lattice/sessions/**`;
//! mutable session metadata and operational state live in sidecar files under
//! `.lattice/sessions/`. Candidate refs under `refs/lattice/candidates/**`
//! keep clean-turn commit ranges reachable.

use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use crate::graph::{
    GraphDiff, GraphHead, GraphRef, GraphSession, HeadTranscript, PreparedTurnCommit,
    ResetResult, SessionGraphStore, SessionGraphSummary, SessionStatus, TurnCommit,
    TurnCommitInput, TurnLogEntry,
};
use crate::{Result, SessionError};

mod commit;
mod helpers;
mod refs;
mod session;

#[cfg(test)]
mod tests;

pub(crate) const DEFAULT_BRANCH: &str = "main";

/// Git-backed session graph store.
///
/// Holds the project root path and a lazily opened `git2::Repository`. The
/// repository is opened on first access and cached under a mutex.
pub struct GitSessionGraphStore {
    pub(crate) dir: PathBuf,
    pub(crate) repo: Mutex<Option<git2::Repository>>,
}

/// RAII handle around the cached repository. Derefs to `git2::Repository`.
pub(crate) struct RepoHandle<'a> {
    guard: MutexGuard<'a, Option<git2::Repository>>,
}

impl<'a> Deref for RepoHandle<'a> {
    type Target = git2::Repository;

    fn deref(&self) -> &git2::Repository {
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
    pub(crate) fn repo(&self) -> Result<RepoHandle<'_>> {
        let mut guard = self.repo.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_none() {
            let repo = git2::Repository::open(&self.dir)
                .map_err(|e| SessionError::Git(format!("open repository: {e}")))?;
            *guard = Some(repo);
        }
        Ok(RepoHandle { guard })
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

    fn get_turn_commit(
        &self,
        session_id: &str,
        commit_id: &str,
    ) -> Result<Option<TurnCommit>> {
        self.get_turn_commit_impl(session_id, commit_id)
    }

    fn set_snapshot_lost(&self, commit_id: &str, lost: bool) -> Result<()> {
        self.set_snapshot_lost_impl(commit_id, lost)
    }

    fn get_snapshot_lost(&self, commit_id: &str) -> Result<Option<bool>> {
        self.get_snapshot_lost_impl(commit_id)
    }
}

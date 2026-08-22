// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Git-backed session graph store.
//!
//! Implements `SessionGraphStore` over the global private `sessions.git`
//! repository via the `lattice-store` kernel (ADR 0005 §6, GitHub #143).
//! Session facts are immutable `SessionState` commits under
//! `refs/lattice/sessions/<id>/state`; conversation branches live under
//! `refs/lattice/sessions/<id>/heads/<branch>`; candidate refs under
//! `refs/lattice/candidates/**` keep clean-turn commit ranges reachable.
//! Derived strict-open projections live under
//! `refs/lattice/session-validation/<id>` and are never a second authority.
//! The mutable `.lattice/sessions/**` sidecars and open-failed-init
//! auto-initialization were hard-cut: absent stores are `NotInitialized`,
//! never invitations to init.
//!
//! Locking (ADR 0005 §5): every mutation holds the per-session
//! `SessionExecutionLock` (a `flock` at `locks/sessions/<SessionId>.lock`)
//! and then the repository write lock; `status` takes a non-blocking shared
//! hold and reports `SessionBusy` instead of misreporting a live Turn as
//! corruption. Snapshot reads (`open`/`list`/`log`/`diff`/`head_transcript`)
//! pin committed refs and take no lock.

#[cfg(test)]
use std::cell::RefCell;
use std::path::{Path, PathBuf};

use lattice_store::{Blocking, FlockGuard, LockMode, StateLayout, StoreMeta, StoreRepo};

use crate::graph::{
    GraphDiff, GraphHead, GraphRef, GraphSession, HeadTranscript, PreparedTurnCommit, ResetResult,
    SessionGraphStore, SessionGraphSummary, SessionStatus, TurnCommit, TurnCommitInput,
    TurnLogEntry,
};
use crate::{Result, SessionError};

mod commit;
pub(crate) mod helpers;
pub(crate) mod pending;
mod refs;
mod session;
pub(crate) mod state;
#[cfg(test)]
mod tests;
mod validation;

pub(crate) const DEFAULT_BRANCH: &str = "main";

/// Structural work performed by the last [`GitSessionGraphStore::open`]
/// domain-invariant sweep. Tests assert that object parsing does not grow
/// with unrelated Session history; production never reads this.
#[cfg(test)]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct StrictOpenWork {
    pub refs_enumerated: usize,
    pub sessions_seen: usize,
    pub sessions_fully_validated: usize,
    pub sessions_projection_hits: usize,
    pub state_objects_parsed: usize,
    pub branch_commits_checked: usize,
    pub turn_pins_parsed: usize,
    pub active_turns_parsed: usize,
}

#[cfg(test)]
thread_local! {
    static LAST_STRICT_OPEN_WORK: RefCell<StrictOpenWork> =
        RefCell::new(StrictOpenWork::default());
}

#[cfg(test)]
pub(crate) fn last_strict_open_work() -> StrictOpenWork {
    LAST_STRICT_OPEN_WORK.with(|cell| cell.borrow().clone())
}

/// Git-backed session graph store over the `lattice-store` kernel.
///
/// Holds the validated kernel repository and the directory of per-session
/// execution lock files. The repository identity (`refs/lattice/meta`) is
/// validated at construction: kind `session`, schema version 1, global
/// scope.
#[derive(Debug)]
pub struct GitSessionGraphStore {
    pub(crate) store: StoreRepo,
    /// Directory holding `<SessionId>.lock` files.
    pub(crate) session_locks_dir: PathBuf,
}

impl GitSessionGraphStore {
    /// Strictly open the store at `dir`, with session locks beside it at
    /// `<parent>/locks/sessions` (the ADR topology when `dir` is
    /// `<state>/lattice/sessions.git`).
    ///
    /// An absent or identity-invalid store fails closed
    /// (`NotInitialized`/`Corruption`) — never auto-initializes.
    pub fn open(dir: impl Into<PathBuf>) -> Result<Self> {
        let dir = dir.into();
        let store = StoreRepo::open(
            &dir,
            &lattice_store::ExpectedMeta {
                store_kind: state::SESSION_STORE_KIND.to_string(),
                store_schema_version: state::SESSION_STORE_SCHEMA_VERSION,
                scope: lattice_store::StoreScope::Global,
            },
        )?;
        let this = Self {
            session_locks_dir: default_locks_dir(&dir),
            store,
        };
        this.validate_domain_invariants()?;
        Ok(this)
    }

    /// First-create the store at `dir` (or validate the existing one), with
    /// session locks beside it. This is the only initialization path; it is
    /// reserved for the composition entry points ADR 0005 names (`session
    /// new`) and tests.
    pub fn create(dir: impl Into<PathBuf>) -> Result<Self> {
        let dir = dir.into();
        let store = StoreRepo::create(
            &dir,
            StoreMeta {
                store_kind: state::SESSION_STORE_KIND.to_string(),
                store_schema_version: state::SESSION_STORE_SCHEMA_VERSION,
                scope: lattice_store::StoreScope::Global,
            },
            |_| Ok(()),
        )?;
        let this = Self {
            session_locks_dir: default_locks_dir(&dir),
            store,
        };
        this.validate_domain_invariants()?;
        Ok(this)
    }

    /// Strictly open the global `sessions.git` from the standard XDG state
    /// layout (production path).
    pub fn open_layout(layout: &StateLayout) -> Result<Self> {
        let store = StoreRepo::open_global(
            layout,
            SESSIONS_REPO_NAME,
            state::SESSION_STORE_KIND,
            state::SESSION_STORE_SCHEMA_VERSION,
        )?;
        let this = Self {
            session_locks_dir: layout.session_locks_dir(),
            store,
        };
        this.validate_domain_invariants()?;
        Ok(this)
    }

    /// First-create the global `sessions.git` in the standard XDG state
    /// layout (or validate the existing one). Reserved for `session new`.
    pub fn create_layout(layout: &StateLayout) -> Result<Self> {
        let store = StoreRepo::create_global(
            layout,
            SESSIONS_REPO_NAME,
            StoreMeta {
                store_kind: state::SESSION_STORE_KIND.to_string(),
                store_schema_version: state::SESSION_STORE_SCHEMA_VERSION,
                scope: lattice_store::StoreScope::Global,
            },
            |_| Ok(()),
        )?;
        let this = Self {
            session_locks_dir: layout.session_locks_dir(),
            store,
        };
        this.validate_domain_invariants()?;
        Ok(this)
    }

    /// Override the session-locks directory (embedders with a custom state
    /// root construct via [`Self::open`]/[`Self::create`] and adjust here).
    pub fn with_locks_dir(mut self, locks_dir: impl Into<PathBuf>) -> Self {
        self.session_locks_dir = locks_dir.into();
        self
    }

    /// The repository path (`sessions.git`).
    pub fn dir(&self) -> &Path {
        self.store.git_dir()
    }

    /// The validated kernel repository.
    pub(crate) fn kernel(&self) -> &StoreRepo {
        &self.store
    }

    /// Acquire the per-session execution lock: exclusive for the full live
    /// Turn, reconciliation, rewind, reset, checkout, branch switch, and
    /// every Session state/branch mutation (ADR 0005 §5). Independent
    /// same-process acquisitions conflict; explicit guard passing is the only
    /// re-entry path. `Blocking::NonBlocking` + `LockMode::Shared` reports
    /// `SessionBusy` when an exclusive owner exists.
    pub(crate) fn acquire_execution_lock(
        &self,
        session_id: &str,
        mode: LockMode,
        blocking: Blocking,
    ) -> Result<FlockGuard> {
        state::validate_session_ref_id(session_id)?;
        let path = self.session_locks_dir.join(format!("{session_id}.lock"));
        lattice_store::acquire_flock(&path, mode, blocking).map_err(|err| match err {
            lattice_store::StoreError::Busy { .. } => {
                SessionError::SessionBusy(session_id.to_string())
            }
            other => SessionError::Store(other),
        })
    }

    /// Validate the explicit ownership capability supplied to a locked API.
    ///
    /// A path-only check is insufficient: a shared guard on the right file is
    /// still not ownership of the exclusive SessionExecutionLock. The guard
    /// carries both facts, and this check is the single fail-closed seam used
    /// by every `*_locked` implementation.
    pub(crate) fn validate_execution_lock(
        &self,
        session_id: &str,
        guard: &FlockGuard,
    ) -> Result<()> {
        state::validate_session_ref_id(session_id)?;
        let expected_path = self.session_locks_dir.join(format!("{session_id}.lock"));
        let expected_mode = LockMode::Exclusive;
        if guard.path() != expected_path.as_path() || guard.mode() != expected_mode {
            return Err(SessionError::InvalidLockCapability {
                session_id: session_id.to_string(),
                expected_path,
                expected_mode,
                actual_path: guard.path().to_path_buf(),
                actual_mode: guard.mode(),
            });
        }
        Ok(())
    }

    /// Domain invariant validation at open: every
    /// `refs/lattice/sessions/<id>/state` must parse and name its own
    /// session. Broken invariants are typed corruption, never repaired.
    ///
    /// Object parsing is skipped for a session only when its derived
    /// validation projection names exactly the live `(suffix, oid)` set.
    /// Enumeration still sees every session ref; a mismatch forces a full
    /// per-session sweep before the handle is returned.
    fn validate_domain_invariants(&self) -> Result<()> {
        let enumerated = self.store.enumerate_refs(state::sessions_prefix())?;
        let mut by_session: std::collections::BTreeMap<
            String,
            Vec<(String, String, gix::ObjectId)>,
        > = std::collections::BTreeMap::new();
        for (name, oid) in enumerated {
            let Some(rest) = name.strip_prefix(state::sessions_prefix()) else {
                continue;
            };
            let Some((session_id, suffix)) = rest.split_once('/') else {
                return Err(SessionError::Store(lattice_store::StoreError::Corruption {
                    detail: format!("illegal session ref '{name}'"),
                }));
            };
            let session_key = session_id.to_string();
            let suffix = suffix.to_string();
            by_session
                .entry(session_key)
                .or_default()
                .push((name, suffix, oid));
        }

        #[cfg(test)]
        let mut work = StrictOpenWork {
            refs_enumerated: by_session.values().map(Vec::len).sum(),
            sessions_seen: by_session.len(),
            ..StrictOpenWork::default()
        };

        for (session_id, refs) in &by_session {
            let live: Vec<(String, gix::ObjectId)> = refs
                .iter()
                .map(|(_, suffix, oid)| (suffix.clone(), *oid))
                .collect();
            if validation::projection_matches(&self.store, session_id, &live)? {
                #[cfg(test)]
                {
                    work.sessions_projection_hits += 1;
                }
                continue;
            }
            #[cfg(test)]
            {
                work.sessions_fully_validated += 1;
            }
            for (name, suffix, oid) in refs {
                match suffix.as_str() {
                    "state" => {
                        let bytes = self
                            .store
                            .read_tree_blob(*oid, state::STATE_TREE_FILE)
                            .map_err(|_| {
                                SessionError::Store(lattice_store::StoreError::Corruption {
                                    detail: format!("session '{session_id}' state unreadable"),
                                })
                            })?;
                        let parsed: state::SessionState =
                            serde_json::from_slice(&bytes).map_err(|e| {
                                SessionError::InvalidPersistedValue {
                                    field: "state.json",
                                    value: e.to_string(),
                                }
                            })?;
                        if parsed.session_id != *session_id {
                            return Err(SessionError::Store(
                                lattice_store::StoreError::Corruption {
                                    detail: format!(
                                        "state ref for '{session_id}' names session '{}'",
                                        parsed.session_id
                                    ),
                                },
                            ));
                        }
                        #[cfg(test)]
                        {
                            work.state_objects_parsed += 1;
                        }
                    }
                    s if s.starts_with("heads/") => {
                        crate::validate_branch_name(&s["heads/".len()..])?;
                        self.store.commit_tree(*oid).map_err(|_| {
                            SessionError::Store(lattice_store::StoreError::Corruption {
                                detail: format!("branch ref '{name}' does not point at a commit"),
                            })
                        })?;
                        #[cfg(test)]
                        {
                            work.branch_commits_checked += 1;
                        }
                    }
                    s if s.starts_with("turns/") => {
                        let pinned_turn = &s["turns/".len()..];
                        state::validate_turn_ref_id(pinned_turn)?;
                        let tree = self.store.commit_tree(*oid).map_err(|_| {
                            SessionError::Store(lattice_store::StoreError::Corruption {
                                detail: format!("turn pin '{name}' does not point at a commit"),
                            })
                        })?;
                        let Some(manifest_bytes) =
                            self.store.read_tree_path(tree, "turn/manifest.json")?
                        else {
                            return Err(SessionError::JournalCorruption {
                                detail: format!(
                                    "turn pin '{name}' does not point at a conversation commit"
                                ),
                            });
                        };
                        let manifest = helpers::parse_manifest(&manifest_bytes)?;
                        if manifest.session_id != *session_id {
                            return Err(SessionError::JournalCorruption {
                                detail: format!(
                                    "turn pin '{name}' points at a commit of session '{}'",
                                    manifest.session_id
                                ),
                            });
                        }
                        if manifest.turn_id != pinned_turn {
                            return Err(SessionError::JournalCorruption {
                                detail: format!(
                                    "turn pin '{name}' points at turn '{}'",
                                    manifest.turn_id
                                ),
                            });
                        }
                        #[cfg(test)]
                        {
                            work.turn_pins_parsed += 1;
                        }
                    }
                    "active-turn" => {
                        let bytes = self
                            .store
                            .read_tree_blob(*oid, pending::PENDING_TREE_FILE)
                            .map_err(|_| SessionError::JournalCorruption {
                                detail: format!(
                                    "active-turn ref '{name}' has no {}",
                                    pending::PENDING_TREE_FILE
                                ),
                            })?;
                        let snapshot: crate::pending::PendingSnapshot =
                            serde_json::from_slice(&bytes).map_err(|e| {
                                SessionError::JournalCorruption {
                                    detail: format!(
                                        "parse pending.json at active-turn '{name}': {e}"
                                    ),
                                }
                            })?;
                        if snapshot.session_id.as_str() != session_id.as_str() {
                            return Err(SessionError::JournalCorruption {
                                detail: format!(
                                    "active-turn of '{session_id}' names session '{}'",
                                    snapshot.session_id
                                ),
                            });
                        }
                        snapshot.validate()?;
                        #[cfg(test)]
                        {
                            work.active_turns_parsed += 1;
                        }
                    }
                    _ => {
                        return Err(SessionError::Store(lattice_store::StoreError::Corruption {
                            detail: format!("illegal session ref '{name}'"),
                        }));
                    }
                }
            }
        }
        #[cfg(test)]
        LAST_STRICT_OPEN_WORK.with(|cell| *cell.borrow_mut() = work);
        Ok(())
    }
}

/// The global session repository file name (owned by this crate).
pub const SESSIONS_REPO_NAME: &str = "sessions.git";

fn default_locks_dir(store_dir: &Path) -> PathBuf {
    store_dir
        .parent()
        .unwrap_or(store_dir)
        .join("locks")
        .join("sessions")
}

impl SessionGraphStore for GitSessionGraphStore {
    fn create_session(
        &self,
        session_id: &str,
        label: Option<&str>,
        active_agent_name: &str,
        project_target_branch: &str,
        project_store_key: &lattice_store::ProjectStoreKey,
    ) -> Result<GraphSession> {
        self.create_session_impl(
            session_id,
            label,
            active_agent_name,
            project_target_branch,
            project_store_key,
        )
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

    fn lock_execution(
        &self,
        session_id: &str,
        mode: LockMode,
        blocking: Blocking,
    ) -> Result<FlockGuard> {
        GitSessionGraphStore::acquire_execution_lock(self, session_id, mode, blocking)
    }

    fn commit_turn_locked(
        &self,
        input: TurnCommitInput,
        execution_lock: &FlockGuard,
    ) -> Result<TurnCommit> {
        self.validate_execution_lock(&input.session_id, execution_lock)?;
        self.commit_turn_impl_locked(input)
    }

    fn status_locked(
        &self,
        session_id: &str,
        execution_lock: &FlockGuard,
    ) -> Result<SessionStatus> {
        self.validate_execution_lock(session_id, execution_lock)?;
        self.status_locked_impl(session_id)
    }

    fn branch_locked(
        &self,
        session_id: &str,
        name: &str,
        from: Option<&str>,
        execution_lock: &FlockGuard,
    ) -> Result<GraphRef> {
        self.validate_execution_lock(session_id, execution_lock)?;
        self.branch_locked_impl(session_id, name, from)
    }

    fn checkout_locked(
        &self,
        session_id: &str,
        target: &str,
        execution_lock: &FlockGuard,
    ) -> Result<GraphHead> {
        self.validate_execution_lock(session_id, execution_lock)?;
        self.checkout_locked_impl(session_id, target)
    }

    fn reset_locked(
        &self,
        session_id: &str,
        target: &str,
        execution_lock: &FlockGuard,
    ) -> Result<ResetResult> {
        self.validate_execution_lock(session_id, execution_lock)?;
        self.reset_locked_impl(session_id, target)
    }
}

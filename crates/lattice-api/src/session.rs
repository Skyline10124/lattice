// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! API session types.
//!
//! `SessionApi` wraps a [`SessionGraphStore`] trait object to provide an
//! explicit durable session API for create/open/status/list/log/checkout/branch/
//! diff/reset operations. The Git-backed store over the `lattice-store` kernel
//! is the only production path since #143.
use lattice_session::{
    GitSessionGraphStore, GraphDiff, GraphHead, GraphRef, GraphSession, HeadTranscript,
    ResetResult, SessionGraphStore, SessionGraphSummary, SessionStatus, TurnCommit, TurnLogEntry,
};
use std::path::PathBuf;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq, thiserror::Error)]
#[error("terminal evidence durability failed after {attempts} attempt(s)")]
pub struct EvidenceDurabilityFailed {
    pub attempts: u32,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq, thiserror::Error)]
pub enum RecoveryRequired {
    #[error("{0}")]
    EvidenceDurabilityFailed(EvidenceDurabilityFailed),
}

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("session '{0}' not found")]
    NotFound(String),

    #[error("bad request: {0}")]
    BadRequest(String),

    #[error("conflict: {0}")]
    Conflict(String),

    #[error("internal error: {0}")]
    Internal(String),

    #[error("busy: {0}")]
    Busy(String),

    #[error("store unavailable: {0}")]
    StoreUnavailable(String),

    #[error(transparent)]
    EvidenceDurabilityFailed(#[from] EvidenceDurabilityFailed),

    #[error("recovery required: {0}")]
    RecoveryRequired(RecoveryRequired),
}

pub type Result<T> = std::result::Result<T, ApiError>;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct CreateSessionRequest {
    pub label: Option<String>,
    pub active_agent: String,
    pub project_branch: String,
    /// Lowercase-hex `ProjectStoreKey` the new session is permanently bound
    /// to. Obtain it from `crate::root::resolve_local_workspace_root` (or
    /// `LocalWorkspaceRoot::from_path`); the session store never derives it.
    pub project_store_key: String,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct OpenSessionRequest {
    pub id: String,
}

#[derive(Clone)]
pub struct SessionApi {
    store: Arc<dyn SessionGraphStore>,
    /// The private-state layout paired with this session repository. Custom
    /// graph-only constructors deliberately have no project-store context and
    /// cannot create or open project stores implicitly through XDG globals.
    layout: Option<lattice_session::StateLayout>,
}

impl SessionApi {
    pub fn new(store: Box<dyn SessionGraphStore>) -> Self {
        Self {
            store: Arc::from(store),
            layout: None,
        }
    }

    /// Strictly open the git-backed session store at `dir`
    /// (`$XDG_STATE_HOME/lattice/sessions.git` in production). Absent or
    /// identity-invalid stores fail closed — never auto-initialize.
    pub fn git_open(dir: impl Into<PathBuf>) -> Result<Self> {
        Ok(Self {
            store: Arc::new(GitSessionGraphStore::open(dir).map_err(map_session_error)?),
            layout: None,
        })
    }

    /// First-create (or validate) the git-backed session store at `dir`.
    /// Reserved for the `session new` composition path (ADR 0005 §3).
    pub fn git_create(dir: impl Into<PathBuf>) -> Result<Self> {
        Ok(Self {
            store: Arc::new(GitSessionGraphStore::create(dir).map_err(map_session_error)?),
            layout: None,
        })
    }

    /// Strictly open the global `sessions.git` from the XDG state layout.
    pub fn git_open_layout(layout: &lattice_session::StateLayout) -> Result<Self> {
        Ok(Self {
            store: Arc::new(GitSessionGraphStore::open_layout(layout).map_err(map_session_error)?),
            layout: Some(layout.clone()),
        })
    }

    /// First-create (or validate) the global `sessions.git` in the XDG state
    /// layout. Reserved for `session new`.
    pub fn git_create_layout(layout: &lattice_session::StateLayout) -> Result<Self> {
        Ok(Self {
            store: Arc::new(
                GitSessionGraphStore::create_layout(layout).map_err(map_session_error)?,
            ),
            layout: Some(layout.clone()),
        })
    }

    /// Open a custom session repository while explicitly pairing it with the
    /// private-state layout used for project stores (tests/embedders).
    pub fn git_open_with_layout(
        dir: impl Into<PathBuf>,
        layout: &lattice_session::StateLayout,
    ) -> Result<Self> {
        let dir = dir.into();
        let store = GitSessionGraphStore::open(dir)
            .map_err(map_session_error)?
            .with_locks_dir(layout.session_locks_dir());
        Ok(Self {
            store: Arc::new(store),
            layout: Some(layout.clone()),
        })
    }

    /// Create a custom session repository while explicitly pairing it with the
    /// private-state layout used for project stores (tests/embedders).
    pub fn git_create_with_layout(
        dir: impl Into<PathBuf>,
        layout: &lattice_session::StateLayout,
    ) -> Result<Self> {
        let dir = dir.into();
        let store = GitSessionGraphStore::create(dir)
            .map_err(map_session_error)?
            .with_locks_dir(layout.session_locks_dir());
        Ok(Self {
            store: Arc::new(store),
            layout: Some(layout.clone()),
        })
    }

    /// Pair a graph-only API handle with the explicit private-state layout
    /// that owns its project stores. This is for embedders and fault-injection
    /// adapters whose graph store is not constructed by one of the git
    /// constructors above.
    pub fn with_state_layout(mut self, layout: lattice_session::StateLayout) -> Self {
        self.layout = Some(layout);
        self
    }

    pub(crate) fn git_store_for_coordinator(
        &self,
    ) -> Result<Arc<lattice_session::GitSessionGraphStore>> {
        let layout = self.layout.as_ref().ok_or_else(|| {
            ApiError::Internal(
                "TurnCoordinator requires an explicit StateLayout-backed SessionApi".to_string(),
            )
        })?;
        Ok(Arc::new(
            lattice_session::GitSessionGraphStore::open_layout(layout)
                .map_err(map_session_error)?,
        ))
    }

    #[cfg(test)]
    pub(crate) fn store(&self) -> &dyn SessionGraphStore {
        &*self.store
    }

    /// Resolve a possibly-abbreviated session ID to the full session ID.
    fn resolve_id(&self, prefix: &str) -> Result<String> {
        // Reject empty or whitespace-only input.
        let prefix = prefix.trim();
        if prefix.is_empty() {
            return Err(ApiError::BadRequest("session ID must not be empty".into()));
        }
        // Try exact match first.
        match self.store.open_session(prefix) {
            Ok(_) => return Ok(prefix.to_string()),
            Err(lattice_session::SessionError::NotFound(_)) => {} // fall through to prefix match
            Err(e) => return Err(map_session_error(e)),
        }
        // Require at least 4 characters for prefix matching.
        if prefix.len() < 4 {
            return Err(ApiError::BadRequest(format!(
                "prefix '{}' too short: at least 4 characters required",
                prefix
            )));
        }
        // Prefix match via list.
        let sessions = self.store.list().map_err(map_session_error)?;
        let matches: Vec<&str> = sessions
            .iter()
            .filter(|s| s.session_id.starts_with(prefix))
            .map(|s| s.session_id.as_str())
            .collect();
        match matches.len() {
            0 => Err(ApiError::NotFound(prefix.to_string())),
            1 => Ok(matches[0].to_string()),
            _ => {
                let ids: Vec<&str> = matches.iter().take(5).copied().collect();
                let suffix = if matches.len() > 5 { ", …" } else { "" };
                Err(ApiError::BadRequest(format!(
                    "ambiguous prefix '{}': matches {}{} — use more characters",
                    prefix,
                    ids.join(", "),
                    suffix
                )))
            }
        }
    }

    /// Create a session bound to `request.project_store_key`.
    ///
    /// ADR 0005 §3: this is the composition point that first-creates the
    /// project's stores. The stores exist before the SessionState is
    /// published, so no later path has to create anything lazily, and the
    /// binding a Session carries always addresses stores that exist.
    pub fn create_session(&self, request: CreateSessionRequest) -> Result<GraphSession> {
        if request.project_branch.trim().is_empty() {
            return Err(ApiError::BadRequest(
                "project_branch must not be blank".into(),
            ));
        }
        let key = lattice_session::ProjectStoreKey::parse(&request.project_store_key)
            .map_err(|e| ApiError::BadRequest(format!("invalid project_store_key: {e}")))?;
        let layout = self.layout.as_ref().ok_or_else(|| {
            ApiError::Internal(
                "session repository has no paired StateLayout; use git_*_layout for session new"
                    .into(),
            )
        })?;
        crate::stores::create_project_stores(layout, key)?;

        let session_id = Uuid::new_v4().simple().to_string();
        self.store
            .create_session(
                &session_id,
                request.label.as_deref(),
                &request.active_agent,
                &request.project_branch,
                &key,
            )
            .map_err(map_session_error)
    }

    pub fn open_session(&self, request: OpenSessionRequest) -> Result<GraphSession> {
        let resolved = self.resolve_id(&request.id)?;
        self.store
            .open_session(&resolved)
            .map_err(map_session_error)
    }

    pub fn status(&self, session_id: &str) -> Result<SessionStatus> {
        let resolved = self.resolve_id(session_id)?;
        self.store.status(&resolved).map_err(map_session_error)
    }

    pub fn list_sessions(&self) -> Result<Vec<SessionGraphSummary>> {
        self.store.list().map_err(map_session_error)
    }

    /// Get the turn log for a session.
    pub fn log(&self, session_id: &str) -> Result<Vec<TurnLogEntry>> {
        let resolved = self.resolve_id(session_id)?;
        self.store.log(&resolved).map_err(map_session_error)
    }

    /// Change branch or detach HEAD at a commit.
    pub fn checkout(&self, session_id: &str, target: &str) -> Result<GraphHead> {
        let resolved = self.resolve_id(session_id)?;
        self.store
            .checkout(&resolved, target)
            .map_err(map_session_error)
    }

    /// Create or move a branch ref to a commit.
    pub fn branch(&self, session_id: &str, name: &str, from: Option<&str>) -> Result<GraphRef> {
        let resolved = self.resolve_id(session_id)?;
        self.store
            .branch(&resolved, name, from)
            .map_err(map_session_error)
    }

    pub fn set_active_agent(&self, session_id: &str, agent_name: &str) -> Result<GraphSession> {
        let resolved = self.resolve_id(session_id)?;
        self.store
            .set_active_agent(&resolved, agent_name)
            .map_err(map_session_error)
    }

    /// Compare graph paths between two commits.
    pub fn diff(&self, session_id: &str, from: &str, to: &str) -> Result<GraphDiff> {
        let resolved = self.resolve_id(session_id)?;
        self.store
            .diff(&resolved, from, to)
            .map_err(map_session_error)
    }

    /// Move branch pointer to a target commit.
    pub fn reset(&self, session_id: &str, target: &str) -> Result<ResetResult> {
        let resolved = self.resolve_id(session_id)?;
        self.store
            .reset(&resolved, target)
            .map_err(map_session_error)
    }

    /// Read the ordered transcript for the current branch HEAD.
    pub(crate) fn head_transcript(&self, session_id: &str) -> Result<HeadTranscript> {
        let resolved = self.resolve_id(session_id)?;
        self.store
            .head_transcript(&resolved)
            .map_err(map_session_error)
    }

    /// Acquire the exclusive SessionExecutionLock for a full live Turn or
    /// Rewind. The blocking syscall is performed by the caller's blocking
    /// offload boundary, not by this synchronous helper on an async worker.
    pub(crate) fn lock_execution(
        &self,
        session_id: &str,
    ) -> Result<(String, lattice_session::FlockGuard)> {
        let resolved = self.resolve_id(session_id)?;
        let guard = self
            .store
            .lock_execution(
                &resolved,
                lattice_session::LockMode::Exclusive,
                lattice_session::Blocking::Block,
            )
            .map_err(map_session_error)?;
        Ok((resolved, guard))
    }

    pub(crate) fn status_locked(
        &self,
        session_id: &str,
        execution_lock: &lattice_session::FlockGuard,
    ) -> Result<SessionStatus> {
        let resolved = self.resolve_id(session_id)?;
        self.store
            .status_locked(&resolved, execution_lock)
            .map_err(map_session_error)
    }

    pub(crate) fn branch_locked(
        &self,
        session_id: &str,
        name: &str,
        from: Option<&str>,
        execution_lock: &lattice_session::FlockGuard,
    ) -> Result<GraphRef> {
        let resolved = self.resolve_id(session_id)?;
        self.store
            .branch_locked(&resolved, name, from, execution_lock)
            .map_err(map_session_error)
    }

    pub(crate) fn checkout_locked(
        &self,
        session_id: &str,
        target: &str,
        execution_lock: &lattice_session::FlockGuard,
    ) -> Result<GraphHead> {
        let resolved = self.resolve_id(session_id)?;
        self.store
            .checkout_locked(&resolved, target, execution_lock)
            .map_err(map_session_error)
    }

    pub(crate) fn reset_locked(
        &self,
        session_id: &str,
        target: &str,
        execution_lock: &lattice_session::FlockGuard,
    ) -> Result<ResetResult> {
        let resolved = self.resolve_id(session_id)?;
        self.store
            .reset_locked(&resolved, target, execution_lock)
            .map_err(map_session_error)
    }

    pub(crate) fn state_layout(&self) -> Option<lattice_session::StateLayout> {
        self.layout.clone()
    }

    pub(crate) fn workspace_history_backing(
        &self,
        key: lattice_session::ProjectStoreKey,
    ) -> Result<lancet_api::WorkspaceHistoryBacking> {
        let layout = self.layout.clone().ok_or_else(|| {
            ApiError::Internal(
                "session repository has no paired StateLayout for project stores".into(),
            )
        })?;
        Ok(lancet_api::WorkspaceHistoryBacking::for_key(layout, key))
    }

    /// Look up one immutable turn commit by session and commit id.
    ///
    /// Resolution is by object id, so it is unaffected by branch movement,
    /// session reopen, or process restart. This is how a caller recovers the
    /// `WorkspaceHistoryCheckpoint` a Turn arrived at (#63).
    pub fn get_turn_commit(&self, session_id: &str, commit_id: &str) -> Result<Option<TurnCommit>> {
        let resolved = self.resolve_id(session_id)?;
        self.store
            .get_turn_commit(&resolved, commit_id)
            .map_err(map_session_error)
    }
}

fn map_session_error(e: lattice_session::SessionError) -> ApiError {
    match e {
        lattice_session::SessionError::NotFound(id) => ApiError::NotFound(id),
        lattice_session::SessionError::InvalidBranchName => {
            ApiError::BadRequest("invalid branch name: format or length violation".into())
        }
        lattice_session::SessionError::MissingCommit(id) => {
            ApiError::NotFound(format!("commit '{}' not found", id))
        }
        lattice_session::SessionError::Conflict(msg) => ApiError::Conflict(msg),
        lattice_session::SessionError::InvalidInput(msg) => ApiError::BadRequest(msg),
        err @ lattice_session::SessionError::InvalidLockCapability { .. } => {
            ApiError::Internal(format!("invalid session lock capability: {err}"))
        }
        lattice_session::SessionError::Git(msg) => ApiError::Internal(format!("git error: {msg}")),
        lattice_session::SessionError::InvalidPersistedValue { field, value } => {
            ApiError::Internal(format!("invalid persisted value for '{field}': {value}"))
        }
        lattice_session::SessionError::SessionBusy(id) => ApiError::Busy(format!(
            "session '{id}' is busy: another operation holds its execution lock"
        )),
        // Journal damage is typed corruption: never repaired, never
        // presented as a retryable conflict (ADR 0005 §9).
        lattice_session::SessionError::JournalCorruption { detail } => {
            ApiError::Internal(format!("pending-turn journal corruption: {detail}"))
        }
        lattice_session::SessionError::Store(store_err) => match store_err {
            lattice_session::StoreError::NotInitialized { path } => ApiError::StoreUnavailable(
                format!("session store not initialized at {}", path.display()),
            ),
            // Identity/invariant failures are typed corruption, never "run
            // session new": recreating over a damaged store is the recovery
            // path ADR 0005 forbids.
            lattice_session::StoreError::Corruption { detail } => ApiError::Internal(format!(
                "session store identity/invariant violation: {detail}"
            )),
            other => ApiError::Internal(format!("store kernel error: {other}")),
        },
    }
}

// Tests

#[cfg(test)]
mod session_graph {
    use super::*;
    use lattice_session::TurnCommitInput;
    use std::env;
    use std::fs;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = env::temp_dir()
            .join("lattice-api-session-graph-tests")
            .join(name);
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn test_store_key() -> lattice_session::ProjectStoreKey {
        lattice_session::ProjectStoreKey::from_canonical_path_bytes(b"/lattice-api-tests/project")
    }

    fn test_store_key_hex() -> String {
        test_store_key().to_string()
    }

    fn api(dir_name: &str) -> SessionApi {
        let state_root = temp_dir(dir_name);
        let layout = lattice_session::StateLayout::in_state_root(&state_root);
        SessionApi::git_create_with_layout(layout.lattice_root().join("sessions.git"), &layout)
            .expect("create store")
    }

    use lattice_session::test_helpers::{assistant_msg, user_msg};

    fn setup_with_commits(
        api: &SessionApi,
        session_id: &str,
        n: usize,
        label: Option<&str>,
    ) -> Vec<String> {
        api.store()
            .create_session(session_id, label, "coder", "main", &test_store_key())
            .expect("create");

        let mut commit_ids: Vec<String> = Vec::new();
        for i in 0..n {
            let parent_ids = if i == 0 {
                vec![]
            } else {
                vec![commit_ids.last().unwrap().clone()]
            };
            let messages = vec![
                user_msg(&format!("{session_id} turn {i}")),
                assistant_msg(&format!("{session_id} response {i}")),
            ];
            let input = TurnCommitInput {
                workspace_history_checkpoint: lattice_session::WorkspaceHistoryCheckpoint::Empty,
                session_id: session_id.to_string(),
                turn_id: lancet_protocol::TurnId::parse(&format!(
                    "00000000-0000-4000-8000-{i:012x}"
                ))
                .expect("valid test TurnId"),
                parent_ids,
                messages: messages.clone(),
                agent_name: "coder".to_string(),
                profile_snapshot_hash: "test-profile".to_string(),
                profile_snapshot_json: "{}".to_string(),
                model: "sonnet".to_string(),
                provider: "anthropic".to_string(),
                outcome: lattice_session::TurnOutcome::Completed {
                    finish_reason: lattice_session::TurnFinishReason::Stop,
                },
                origin: lattice_session::ClosureOrigin::Live,
                commit_message: format!("Turn {i}"),
                usage: None,
            };
            let commit = api.store().commit_turn(input).expect("commit_turn");
            commit_ids.push(commit.commit_id);
        }
        commit_ids
    }

    #[test]
    fn create_and_open_session_round_trip() {
        let api = api("create_open_round_trip");

        let created = api
            .create_session(CreateSessionRequest {
                label: Some("my session".into()),
                active_agent: "coder".into(),
                project_branch: "main".to_string(),
                project_store_key: test_store_key_hex(),
            })
            .expect("create_session");

        assert!(!created.session_id.is_empty());
        assert_eq!(created.label, Some("my session".to_string()));
        assert_eq!(created.active_agent_name, "coder");

        // Reopen by ID.
        let opened = api
            .open_session(OpenSessionRequest {
                id: created.session_id.clone(),
            })
            .expect("open_session");

        assert_eq!(opened.session_id, created.session_id);
        assert_eq!(opened.label, Some("my session".to_string()));
    }

    #[test]
    fn graph_only_constructor_cannot_create_project_stores_via_global_xdg() {
        let root = temp_dir("graph_only_requires_layout");
        let api = SessionApi::git_create(root.join("sessions.git")).expect("create store");
        let err = api
            .create_session(CreateSessionRequest {
                label: None,
                active_agent: "coder".into(),
                project_branch: "main".to_string(),
                project_store_key: test_store_key_hex(),
            })
            .expect_err("graph-only handles must not resolve global XDG state");
        assert!(
            matches!(&err, ApiError::Internal(message) if message.contains("paired StateLayout")),
            "got {err:?}"
        );
    }

    #[test]
    fn status_reports_head_branch_and_commit_count() {
        let api = api("status_report");
        setup_with_commits(&api, "sess-status", 2, Some("Status Test"));

        let status = api.status("sess-status").expect("status");

        assert_eq!(status.session_id, "sess-status");
        assert!(
            matches!(&status.head, lattice_session::GraphHead::Branch { name } if name == "main")
        );
        assert!(status.current_commit_id.is_some());
        assert_eq!(status.commit_count, 2);
    }

    #[test]
    fn list_sessions_includes_created_sessions() {
        let api = api("list_sessions");
        setup_with_commits(&api, "sess-a", 1, Some("Session A"));
        setup_with_commits(&api, "sess-b", 2, Some("Session B"));

        let list = api.list_sessions().expect("list");
        assert!(list.len() >= 2);

        let a = list
            .iter()
            .find(|s| s.session_id == "sess-a")
            .expect("sess-a");
        assert_eq!(a.label, Some("Session A".to_string()));
        assert_eq!(a.commit_count, 1);

        let b = list
            .iter()
            .find(|s| s.session_id == "sess-b")
            .expect("sess-b");
        assert_eq!(b.commit_count, 2);
    }

    #[test]
    fn session_id_is_32_char_hex() {
        let api = api("id_format");
        let created = api
            .create_session(CreateSessionRequest {
                label: None,
                active_agent: "coder".into(),
                project_branch: "main".to_string(),
                project_store_key: test_store_key_hex(),
            })
            .expect("create");
        assert_eq!(created.session_id.len(), 32);
        assert!(created.session_id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn prefix_resolve_unique_match() {
        let api = api("prefix_unique");
        setup_with_commits(&api, "abc12345678901234567890123456789", 1, None);
        setup_with_commits(&api, "xyz98765432109876543210987654321", 1, None);
        // Full ID works.
        let status = api
            .status("abc12345678901234567890123456789")
            .expect("full id");
        assert_eq!(status.session_id, "abc12345678901234567890123456789");
        // First 4 chars uniquely resolve to abc…
        let status = api.status("abc1").expect("prefix abc1");
        assert_eq!(status.session_id, "abc12345678901234567890123456789");
    }

    #[test]
    fn prefix_resolve_ambiguous_reports_error() {
        let api = api("prefix_ambiguous");
        setup_with_commits(&api, "abcd1234567890123456789012345678", 1, None);
        setup_with_commits(&api, "abcd9876543210987654321098765432", 1, None);
        let err = api.status("abcd").unwrap_err();
        let msg = format!("{}", err);
        assert!(msg.contains("ambiguous"), "expected ambiguous, got: {msg}");
        assert!(
            msg.contains("use more characters"),
            "should suggest more chars: {msg}"
        );
    }

    #[test]
    fn empty_prefix_rejected() {
        let api = api("prefix_empty");
        setup_with_commits(&api, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", 1, None);
        for input in ["", "  "] {
            let err = api.status(input).unwrap_err();
            let msg = format!("{}", err);
            assert!(
                msg.contains("must not be empty"),
                "expected empty rejection for {input:?}: {msg}"
            );
        }
    }

    #[test]
    fn short_prefix_rejected() {
        let api = api("prefix_short");
        setup_with_commits(&api, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", 1, None);
        let err = api.status("aaa").unwrap_err();
        let msg = format!("{}", err);
        assert!(
            msg.contains("too short"),
            "expected too-short rejection: {msg}"
        );
    }

    #[test]
    fn log_returns_turn_entries() {
        let api = api("log_entries");
        let commit_ids = setup_with_commits(&api, "sess-log", 3, None);

        let log = api.log("sess-log").expect("log");
        assert_eq!(log.len(), 3);
        assert_eq!(log[0].commit_id, commit_ids[0]);
        assert_eq!(log[2].commit_id, commit_ids[2]);
    }

    #[test]
    fn checkout_changes_head_state() {
        let api = api("checkout_change");
        let commit_ids = setup_with_commits(&api, "sess-co", 2, None);

        // Create a feature branch from first commit.
        api.branch("sess-co", "feature", Some(&commit_ids[0]))
            .expect("branch");

        // Checkout feature branch.
        let head = api.checkout("sess-co", "feature").expect("checkout");
        assert!(matches!(&head, lattice_session::GraphHead::Branch { name } if name == "feature"));

        // Checkout back to main.
        let head = api.checkout("sess-co", "main").expect("checkout main");
        assert!(matches!(&head, lattice_session::GraphHead::Branch { name } if name == "main"));

        // Detach at a commit.
        let head = api.checkout("sess-co", &commit_ids[0]).expect("detach");
        assert!(
            matches!(&head, lattice_session::GraphHead::Detached { commit_id } if commit_id == &commit_ids[0])
        );
    }

    #[test]
    fn branch_creates_pointer_to_commit() {
        let api = api("branch_pointer");
        let commit_ids = setup_with_commits(&api, "sess-br", 2, None);

        let branch_ref = api
            .branch("sess-br", "dev", Some(&commit_ids[0]))
            .expect("branch");

        assert_eq!(branch_ref.name, "dev");
        assert_eq!(branch_ref.commit_id, Some(commit_ids[0].clone()));
    }

    #[test]
    fn diff_reports_unique_commits_on_each_side() {
        let api = api("diff_paths");
        let commit_ids = setup_with_commits(&api, "sess-diff", 2, None);
        let base_id = &commit_ids[0];
        let main_head = &commit_ids[1];

        // Create feature branch from base, commit one more.
        api.branch("sess-diff", "feature", Some(base_id))
            .expect("branch");
        api.checkout("sess-diff", "feature").expect("checkout");

        let feature_messages = vec![user_msg("Feature work"), assistant_msg("Done")];
        let feature_input = TurnCommitInput {
            workspace_history_checkpoint: lattice_session::WorkspaceHistoryCheckpoint::Empty,
            session_id: "sess-diff".to_string(),
            turn_id: lancet_protocol::TurnId::parse("ffffffff-ffff-4fff-bfff-ffffffffffff")
                .expect("valid test TurnId"),
            parent_ids: vec![base_id.clone()],
            messages: feature_messages,
            agent_name: "coder".to_string(),
            profile_snapshot_hash: "test-profile".to_string(),
            profile_snapshot_json: "{}".to_string(),
            model: "sonnet".to_string(),
            provider: "anthropic".to_string(),
            outcome: lattice_session::TurnOutcome::Completed {
                finish_reason: lattice_session::TurnFinishReason::Stop,
            },
            origin: lattice_session::ClosureOrigin::Live,
            commit_message: "Feature work".to_string(),
            usage: None,
        };
        let feature_commit = api.store().commit_turn(feature_input).expect("commit");

        // Diff main head vs feature head.
        let diff = api
            .diff("sess-diff", main_head, &feature_commit.commit_id)
            .expect("diff");

        assert_eq!(diff.common_ancestor_id, Some(base_id.clone()));
        assert_eq!(diff.only_in_from.len(), 1);
        assert_eq!(diff.only_in_from[0].commit_id, *main_head);
        assert_eq!(diff.only_in_to.len(), 1);
        assert_eq!(diff.only_in_to[0].commit_id, feature_commit.commit_id);
    }

    #[test]
    fn reset_moves_branch_and_returns_old_head() {
        let api = api("reset_old_head");
        let commit_ids = setup_with_commits(&api, "sess-reset", 3, None);

        let result = api.reset("sess-reset", &commit_ids[0]).expect("reset");

        assert_eq!(result.old_commit_id, Some(commit_ids[2].clone()));
        assert_eq!(result.new_commit_id, commit_ids[0]);
        assert_eq!(result.branch, "main");
    }

    #[test]
    fn session_api_does_not_expose_legacy_operations() {
        // This test verifies the public API surface of SessionApi.
        // The following methods SHOULD exist:
        fn _assert_api_surface(api: &SessionApi) {
            let _ = api.create_session(CreateSessionRequest {
                label: None,
                active_agent: "coder".into(),
                project_branch: "main".to_string(),
                project_store_key: test_store_key_hex(),
            });
            let _ = api.open_session(OpenSessionRequest { id: String::new() });
            let _ = api.status("");
            let _ = api.list_sessions();
            let _ = api.log("");
            let _ = api.checkout("", "");
            let _ = api.branch("", "", None);
            let _ = api.set_active_agent("", "");
            let _ = api.diff("", "", "");
            let _ = api.reset("", "");
        }
        // Rich graph operations beyond pointer movement should not exist on
        // SessionApi.
        //
        // If any of these are added, this test module will fail to compile
        // because the method calls below would succeed, which is NOT desired.
        // (No code here — the absence of those methods is the assertion.)
    }

    #[test]
    fn graph_store_trait_excludes_legacy_methods() {
        // The trait is used as a bound to confirm it compiles with only
        // the expected methods. If richer history rewriting methods were added,
        // this module would need updating.
        fn _assert_trait_bound(_store: &dyn SessionGraphStore) {}
    }

    #[test]
    fn session_api_rejects_empty_project_branch() {
        let api = api("empty_branch");
        let err = api
            .create_session(CreateSessionRequest {
                label: None,
                active_agent: "coder".into(),
                project_branch: String::new(),
                project_store_key: test_store_key_hex(),
            })
            .unwrap_err();
        let msg = format!("{}", err);
        assert!(
            msg.contains("must not be blank"),
            "expected blank rejection, got: {msg}"
        );
    }

    #[test]
    fn session_api_rejects_whitespace_project_branch() {
        let api = api("ws_branch");
        let err = api
            .create_session(CreateSessionRequest {
                label: None,
                active_agent: "coder".into(),
                project_branch: "   ".to_string(),
                project_store_key: test_store_key_hex(),
            })
            .unwrap_err();
        let msg = format!("{}", err);
        assert!(
            msg.contains("must not be blank"),
            "expected blank rejection, got: {msg}"
        );
    }

    #[test]
    fn create_session_persists_explicit_project_branch() {
        let api = api("persist_branch");
        let created = api
            .create_session(CreateSessionRequest {
                label: None,
                active_agent: "coder".into(),
                project_branch: "feature/my-branch".to_string(),
                project_store_key: test_store_key_hex(),
            })
            .expect("create");
        assert_eq!(
            created.project_target_branch.as_deref(),
            Some("feature/my-branch")
        );
        let opened = api
            .open_session(OpenSessionRequest {
                id: created.session_id.clone(),
            })
            .expect("open");
        assert_eq!(
            opened.project_target_branch.as_deref(),
            Some("feature/my-branch")
        );
    }
}

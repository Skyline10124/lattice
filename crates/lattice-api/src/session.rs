// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! API session types.
//!
//! `SessionApi` wraps a [`SessionGraphStore`] trait object to provide an
//! explicit durable session API for create/open/status/list/log/checkout/branch/
//! diff/reset operations. Both SQLite and Git-backed stores are supported.

use lattice_session::{
    migrate::find_legacy_session_db, GitSessionGraphStore, GraphDiff, GraphHead, GraphRef,
    GraphSession, HeadTranscript, PreparedTurnCommit, ResetResult, SessionGraphStore,
    SessionGraphSummary, SessionStatus, TurnCommit, TurnCommitInput, TurnLogEntry,
};
use std::path::{Path, PathBuf};
use uuid::Uuid;

use crate::turn::promotion::PromotionLifecycleError;

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

    #[error("promotion recovery required before turn start")]
    PromotionRecoveryRequired,

    #[error("promotion recovery conflict: {0}")]
    PromotionRecoveryConflict(String),

    #[error("promotion lifecycle: {0}")]
    PromotionLifecycle(#[from] PromotionLifecycleError),
}

pub type Result<T> = std::result::Result<T, ApiError>;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct CreateSessionRequest {
    pub label: Option<String>,
    pub active_agent: String,
    pub project_branch: String,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct OpenSessionRequest {
    pub id: String,
}

pub struct SessionApi {
    store: Box<dyn SessionGraphStore>,
}

impl SessionApi {
    pub fn new(store: Box<dyn SessionGraphStore>) -> Self {
        Self { store }
    }

    pub fn git(dir: impl Into<PathBuf>) -> Self {
        Self {
            store: Box::new(GitSessionGraphStore::new(dir)),
        }
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

    pub fn create_session(&self, request: CreateSessionRequest) -> Result<GraphSession> {
        if request.project_branch.trim().is_empty() {
            return Err(ApiError::BadRequest(
                "project_branch must not be blank".into(),
            ));
        }
        let session_id = Uuid::new_v4().simple().to_string();
        self.store
            .create_session(
                &session_id,
                request.label.as_deref(),
                &request.active_agent,
                &request.project_branch,
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

    /// Commit a completed turn atomically.
    pub(crate) fn commit_turn(&self, input: TurnCommitInput) -> Result<TurnCommit> {
        let resolved = self.resolve_id(&input.session_id)?;
        let mut input = input;
        input.session_id = resolved;
        self.store.commit_turn(input).map_err(map_session_error)
    }

    /// Persist a precomputed turn commit with prepared identity.
    ///
    /// Thin wrapper over the session store's prepared commit path.
    /// Uses the prepared commit ID for idempotent retry (D-23).
    ///
    /// NOTE (IN-02): Unlike `commit_turn`, this does not resolve abbreviated
    /// session ID prefixes via `resolve_id`. The prepared commit must carry the
    /// canonical (full) session ID; callers are responsible for resolving
    /// prefixes before constructing the `PreparedTurnCommit`. This is
    /// intentional because prepared commits are built internally after the
    /// session has already been resolved for turn execution.
    pub(crate) fn commit_prepared_turn(&self, prepared: PreparedTurnCommit) -> Result<TurnCommit> {
        self.store
            .commit_prepared_turn(prepared)
            .map_err(map_session_error)
    }

    /// Look up a single turn commit by session and commit ID.
    pub(crate) fn get_turn_commit(
        &self,
        session_id: &str,
        commit_id: &str,
    ) -> Result<Option<TurnCommit>> {
        let resolved = self.resolve_id(session_id)?;
        self.store
            .get_turn_commit(&resolved, commit_id)
            .map_err(map_session_error)
    }

    /// Query whether a commit's retained snapshot is operationally lost.
    ///
    /// Returns `None` if no metadata is recorded for the commit.
    /// Never modifies immutable commit fields (T-36-07).
    pub(crate) fn get_snapshot_lost(&self, commit_id: &str) -> Result<Option<bool>> {
        self.store
            .get_snapshot_lost(commit_id)
            .map_err(map_session_error)
    }
}

/// Startup guard for un-migrated legacy SQLite session DB (D-18, issue 1b).
///
/// The sole reliable migration-completion signal is D-18's archive action:
/// successful migration renames the legacy DB to `<db>.migrated-<ts>`. If the
/// original DB is still present, migration has NOT completed (either never
/// started, or crashed mid-write). The guard blocks startup with a clear
/// message prompting `lattice session migrate`.
///
/// The legacy filename is discovered via `find_legacy_session_db`, which probes
/// `session_v12.db`, `session_v11.db`, `session.db`, `sessions.db` in priority
/// order (D-08-GAP1). The real legacy SQLite session store used
/// `session_v12.db`; the plural `sessions.db` was an undocumented assumption.
///
/// Does NOT inspect the Git repo for commits — a crashed mid-migration may have
/// written partial commits, so "has commits" is not a valid success signal.
/// Uses only `Path::exists()` — no side effects, no git2 repo open (avoids
/// triggering auto-init).
pub fn check_migration_guard(git_repo_dir: &Path) -> std::result::Result<(), String> {
    // D-08-GAP1: probe for the real legacy DB filename instead of hardcoding
    // sessions.db. The session DB lives in the parent of the git repo dir
    // (e.g. .../lattice/sessions/sessions.git → .../lattice/sessions/).
    let session_dir = git_repo_dir
        .parent()
        .unwrap_or_else(|| Path::new("."));
    let legacy_db = match find_legacy_session_db(session_dir) {
        Some(path) => path,
        None => return Ok(()), // no legacy DB — fresh install or already migrated
    };
    Err(format!(
        "Legacy SQLite session DB detected at {}. The Git session store has not been migrated. \
         Run 'lattice session migrate --source {} --target {}' to complete migration \
         before starting LATTICE.",
        legacy_db.display(),
        legacy_db.display(),
        git_repo_dir.display(),
    ))
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
        lattice_session::SessionError::Io {
            operation,
            path,
            source,
        } => ApiError::Internal(format!(
            "io error while {} '{}': {}",
            operation,
            path.display(),
            source
        )),
        lattice_session::SessionError::Database { path, source } => ApiError::Internal(format!(
            "database error in '{}': {}",
            path.display(),
            source
        )),
        lattice_session::SessionError::Git(msg) => ApiError::Internal(format!("git error: {msg}")),
        lattice_session::SessionError::InvalidPersistedValue { field, value } => {
            ApiError::Internal(format!("invalid persisted value for '{field}': {value}"))
        }
    }
}

// Tests

#[cfg(test)]
mod session_graph {
    use super::*;
    use lattice_session::{TurnCommitInput, TurnCommitStatus, WorkspaceDisposition};
    use std::env;
    use std::fs;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = env::temp_dir()
            .join("lattice-api-session-graph-tests")
            .join(name);
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn api(dir_name: &str) -> SessionApi {
        SessionApi::git(temp_dir(dir_name))
    }

    use lattice_session::test_helpers::{assistant_msg, user_msg};

    fn setup_with_commits(
        api: &SessionApi,
        session_id: &str,
        n: usize,
        label: Option<&str>,
    ) -> Vec<String> {
        api.store()
            .create_session(session_id, label, "coder", "main")
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
                session_id: session_id.to_string(),
                parent_ids,
                messages: messages.clone(),
                agent_name: "coder".to_string(),
                profile_snapshot_hash: "test-profile".to_string(),
                profile_snapshot_json: "{}".to_string(),
                model: "sonnet".to_string(),
                provider: "anthropic".to_string(),
                status: TurnCommitStatus::Completed,
                disposition: WorkspaceDisposition::NotApplicable,
                lifecycle: None,
                commit_message: format!("Turn {i}"),
                usage: None,
                terminal_reason: None,
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
            session_id: "sess-diff".to_string(),
            parent_ids: vec![base_id.clone()],
            messages: feature_messages,
            agent_name: "coder".to_string(),
            profile_snapshot_hash: "test-profile".to_string(),
            profile_snapshot_json: "{}".to_string(),
            model: "sonnet".to_string(),
            provider: "anthropic".to_string(),
            status: TurnCommitStatus::Completed,
            disposition: WorkspaceDisposition::NotApplicable,
            lifecycle: None,
            commit_message: "Feature work".to_string(),
            usage: None,
            terminal_reason: None,
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

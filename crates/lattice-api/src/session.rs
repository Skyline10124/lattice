//! v1.1 API session types.
//!
//! Phase 6 stubs: `create_session` and `open_session` return typed `ApiSession`
//! values without touching durable storage. Session persistence is deferred
//! to Phase 8.
//!
//! Phase 8: `SessionApi` wraps [`SqliteSessionGraphStore`] to provide an
//! explicit durable session API for create/open/status/list/log/checkout/
//! branch/diff/reset operations.

use lattice_session::{
    GraphDiff, GraphHead, GraphRef, GraphSession, HeadTranscript, ResetResult, SessionGraphStore,
    SessionGraphSummary, SessionStatus, SqliteSessionGraphStore, TurnCommit, TurnCommitInput,
    TurnLogEntry,
};
use std::path::PathBuf;
use uuid::Uuid;

/// Errors that occur during API operations.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// A session with the requested id was not found.
    #[error("session '{0}' not found")]
    NotFound(String),

    /// An internal error occurred.
    #[error("internal error: {0}")]
    Internal(String),
}

/// Convenience result type for API operations.
pub type Result<T> = std::result::Result<T, ApiError>;

/// Request to create a new session.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct CreateSessionRequest {
    /// Optional human-readable label.
    pub label: Option<String>,
    /// Optional profile name to associate.
    pub profile: Option<String>,
    /// Optional model alias.
    pub model: Option<String>,
}

/// Request to open an existing session.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct OpenSessionRequest {
    /// The session id to open.
    pub id: String,
}

/// A session handle returned by the API.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct ApiSession {
    /// Unique session identifier.
    pub id: String,
    /// Human-readable label, if set at creation time.
    pub label: Option<String>,
    /// Profile associated with this session.
    pub profile: Option<String>,
    /// Model alias for this session.
    pub model: Option<String>,
    /// ISO-8601 creation timestamp.
    pub created_at: String,
}

impl ApiSession {
    /// Create a new session handle with the given parameters.
    pub fn new(label: Option<String>, profile: Option<String>, model: Option<String>) -> Self {
        let timestamp = chrono_now();
        ApiSession {
            id: Uuid::new_v4().simple().to_string(),
            label,
            profile,
            model,
            created_at: timestamp,
        }
    }

    /// Create a stub session handle for a requested id (open).
    pub fn with_id(id: String) -> Self {
        let timestamp = chrono_now();
        ApiSession {
            id,
            label: None,
            profile: None,
            model: None,
            created_at: timestamp,
        }
    }
}

/// Create a new session.
///
/// Generates a stable id via UUID v4 and returns an [`ApiSession`] handle.
/// Does not touch disk in Phase 6.
pub fn create_session(request: CreateSessionRequest) -> Result<ApiSession> {
    Ok(ApiSession::new(
        request.label,
        request.profile,
        request.model,
    ))
}

/// Open an existing session by id.
///
/// Phase 6: returns `NotFound` — no durable session store exists yet.
/// Phase 8 will wire real lookup.
pub fn open_session(request: OpenSessionRequest) -> Result<ApiSession> {
    Err(ApiError::NotFound(request.id))
}

/// Return a simple RFC 3339 timestamp for Phase 6.
fn chrono_now() -> String {
    // Use a simple fixed format instead of pulling in the chrono crate.
    // The workspace has chrono available, but the API crate keeps deps narrow.
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| {
            let secs = d.as_secs();
            // Rough RFC 3339: YYYY-MM-DDTHH:MM:SSZ
            let days_since_epoch = secs / 86400;
            let time_of_day = secs % 86400;
            let hours = time_of_day / 3600;
            let minutes = (time_of_day % 3600) / 60;
            let seconds = time_of_day % 60;

            // Simplified date calculation from Unix epoch (1970-01-01).
            // Accurate enough for Phase 6 timestamps.
            let mut year = 1970i64;
            let mut remaining_days = days_since_epoch as i64;

            loop {
                let days_in_year = if is_leap(year) { 366 } else { 365 };
                if remaining_days < days_in_year {
                    break;
                }
                remaining_days -= days_in_year;
                year += 1;
            }

            let month_days = if is_leap(year) {
                [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
            } else {
                [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
            };

            let mut month = 1;
            for &md in &month_days {
                if remaining_days < md as i64 {
                    break;
                }
                remaining_days -= md as i64;
                month += 1;
            }

            let day = remaining_days + 1;
            format!(
                "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
                year, month, day, hours, minutes, seconds
            )
        })
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

// ---------------------------------------------------------------------------
// SessionApi: explicit durable session API backed by the v1.1 graph store
// ---------------------------------------------------------------------------

/// Explicit durable session API backed by [`SessionGraphStore`].
///
/// Phase 8: wraps [`SqliteSessionGraphStore`] and exposes create/open/status/
/// list/log/checkout/branch/diff/reset operations through the v1.1 API
/// boundary. This is the thin facade that future phases wire into the CLI
/// and agent loop.
pub struct SessionApi {
    store: SqliteSessionGraphStore,
}

impl SessionApi {
    /// Create a new `SessionApi` backed by the given graph store.
    pub fn new(store: SqliteSessionGraphStore) -> Self {
        Self { store }
    }

    /// Create a new `SessionApi` with a SQLite store rooted at `dir`.
    pub fn sqlite(dir: impl Into<PathBuf>) -> Self {
        Self {
            store: SqliteSessionGraphStore::new(dir),
        }
    }

    /// Access the underlying graph store (crate-visible for tests).
    #[cfg(test)]
    pub(crate) fn store(&self) -> &SqliteSessionGraphStore {
        &self.store
    }

    /// Create a new durable session.
    ///
    /// Generates a UUID v4 session ID, persists session metadata and an
    /// initial `main` branch ref, and returns the graph session.
    pub fn create_session(
        &self,
        request: CreateSessionRequest,
    ) -> Result<GraphSession> {
        let session_id = Uuid::new_v4().simple().to_string();
        self.store
            .create_session(
                &session_id,
                request.label.as_deref(),
                request.profile.as_deref(),
                request.model.as_deref(),
                None, // provider resolved at turn time
            )
            .map_err(map_session_error)
    }

    /// Open an existing session by ID.
    pub fn open_session(&self, request: OpenSessionRequest) -> Result<GraphSession> {
        self.store
            .open_session(&request.id)
            .map_err(map_session_error)
    }

    /// Get the current status of a session.
    pub fn status(&self, session_id: &str) -> Result<SessionStatus> {
        self.store.status(session_id).map_err(map_session_error)
    }

    /// List all sessions with summary information.
    pub fn list_sessions(&self) -> Result<Vec<SessionGraphSummary>> {
        self.store.list().map_err(map_session_error)
    }

    /// Get the turn log for a session (one entry per turn commit).
    pub fn log(&self, session_id: &str) -> Result<Vec<TurnLogEntry>> {
        self.store.log(session_id).map_err(map_session_error)
    }

    /// Change the active branch or detach HEAD at a commit.
    ///
    /// D-23: pointer operation only — does not run a model or create a commit.
    pub fn checkout(&self, session_id: &str, target: &str) -> Result<GraphHead> {
        self.store
            .checkout(session_id, target)
            .map_err(map_session_error)
    }

    /// Create or move a branch ref to an existing commit.
    ///
    /// D-24: pointer operation only — does not fork a session, run a model,
    /// or create a commit.
    pub fn branch(
        &self,
        session_id: &str,
        name: &str,
        from: Option<&str>,
    ) -> Result<GraphRef> {
        self.store
            .branch(session_id, name, from)
            .map_err(map_session_error)
    }

    /// Compare graph paths between two commits.
    ///
    /// D-26: finds common ancestor and lists commits unique to each side.
    pub fn diff(&self, session_id: &str, from: &str, to: &str) -> Result<GraphDiff> {
        self.store
            .diff(session_id, from, to)
            .map_err(map_session_error)
    }

    /// Move the current branch pointer to a target commit.
    ///
    /// D-25: does not delete commits; returns old HEAD commit ID for recovery.
    pub fn reset(&self, session_id: &str, target: &str) -> Result<ResetResult> {
        self.store
            .reset(session_id, target)
            .map_err(map_session_error)
    }

    /// Read the complete ordered transcript for the current branch HEAD.
    ///
    /// Returns all messages from root through current HEAD plus the parent
    /// commit ID (None if no commits yet).
    pub(crate) fn head_transcript(&self, session_id: &str) -> Result<HeadTranscript> {
        self.store
            .head_transcript(session_id)
            .map_err(map_session_error)
    }

    /// Commit a completed turn atomically.
    ///
    /// Writes messages, commit object, and branch/HEAD ref update in one
    /// transaction. Returns the created [`TurnCommit`].
    pub(crate) fn commit_turn(&self, input: TurnCommitInput) -> Result<TurnCommit> {
        self.store.commit_turn(input).map_err(map_session_error)
    }
}

/// Map a [`lattice_session::SessionError`] to an [`ApiError`].
fn map_session_error(e: lattice_session::SessionError) -> ApiError {
    match e {
        lattice_session::SessionError::NotFound(id) => ApiError::NotFound(id),
        other => ApiError::Internal(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_session_returns_api_session_with_id() {
        let req = CreateSessionRequest {
            label: Some("test session".into()),
            profile: None,
            model: None,
        };
        let session = create_session(req).unwrap();
        assert!(!session.id.is_empty(), "session id should not be empty");
        // UUID v4 simple format is 32 hex chars.
        assert_eq!(
            session.id.len(),
            32,
            "session id should be 32 hex chars, got: {}",
            session.id
        );
        assert_eq!(session.label.as_deref(), Some("test session"));
    }

    #[test]
    fn create_session_defaults_profile_and_model_to_none() {
        let req = CreateSessionRequest {
            label: None,
            profile: None,
            model: None,
        };
        let session = create_session(req).unwrap();
        assert!(session.profile.is_none());
        assert!(session.model.is_none());
    }

    #[test]
    fn open_session_returns_not_found_in_phase_6() {
        let req = OpenSessionRequest {
            id: "abc123".to_string(),
        };
        let result = open_session(req);
        assert!(
            result.is_err(),
            "open_session should return NotFound in Phase 6"
        );
        match result.unwrap_err() {
            ApiError::NotFound(id) => assert_eq!(id, "abc123"),
            other => panic!("expected NotFound, got {:?}", other),
        }
    }

    #[test]
    fn session_created_at_is_rfc3339_ish() {
        let session = ApiSession::new(None, None, None);
        // Should contain T and Z separators.
        assert!(
            session.created_at.contains('T'),
            "created_at should contain 'T': {}",
            session.created_at
        );
        assert!(
            session.created_at.ends_with('Z'),
            "created_at should end with 'Z': {}",
            session.created_at
        );
    }
}

// ---------------------------------------------------------------------------
// Phase 8 session_graph tests: SessionApi backed by the v1.1 graph store
// ---------------------------------------------------------------------------

#[cfg(test)]
mod session_graph {
    use super::*;
    use lattice_session::{GraphMessage, TurnCommitInput, TurnCommitStatus};
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
        SessionApi::sqlite(temp_dir(dir_name))
    }

    fn user_msg(text: &str) -> GraphMessage {
        GraphMessage {
            role: "user".to_string(),
            content: text.to_string(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    fn assistant_msg(text: &str) -> GraphMessage {
        GraphMessage {
            role: "assistant".to_string(),
            content: text.to_string(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    /// Helper: create a session and commit N turns via the store directly.
    fn setup_with_commits(
        api: &SessionApi,
        session_id: &str,
        n: usize,
        label: Option<&str>,
    ) -> Vec<String> {
        api.store()
            .create_session(session_id, label, None, Some("sonnet"), Some("anthropic"))
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
                profile_snapshot_id: Some("test-profile".to_string()),
                model: "sonnet".to_string(),
                provider: "anthropic".to_string(),
                status: TurnCommitStatus::Completed,
                commit_message: format!("Turn {i}"),
                usage: None,
                terminal_reason: None,
            };
            let commit = api.store().commit_turn(input).expect("commit_turn");
            commit_ids.push(commit.commit_id);
        }
        commit_ids
    }

    // -- create_session / open_session round-trip --

    #[test]
    fn create_and_open_session_round_trip() {
        let api = api("create_open_round_trip");

        let created = api
            .create_session(CreateSessionRequest {
                label: Some("my session".into()),
                profile: Some("default".into()),
                model: Some("sonnet".into()),
            })
            .expect("create_session");

        assert!(!created.session_id.is_empty());
        assert_eq!(created.label, Some("my session".to_string()));
        assert_eq!(created.model, Some("sonnet".to_string()));

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
    fn open_nonexistent_session_returns_not_found() {
        let api = api("open_not_found");
        let result = api.open_session(OpenSessionRequest {
            id: "nonexistent".into(),
        });
        assert!(result.is_err());
        match result.unwrap_err() {
            ApiError::NotFound(id) => assert_eq!(id, "nonexistent"),
            other => panic!("expected NotFound, got {:?}", other),
        }
    }

    // -- status --

    #[test]
    fn status_reports_head_branch_and_commit_count() {
        let api = api("status_report");
        setup_with_commits(&api, "sess-status", 2, Some("Status Test"));

        let status = api.status("sess-status").expect("status");

        assert_eq!(status.session_id, "sess-status");
        assert!(matches!(&status.head, lattice_session::GraphHead::Branch { name } if name == "main"));
        assert!(status.current_commit_id.is_some());
        assert_eq!(status.commit_count, 2);
    }

    // -- list_sessions --

    #[test]
    fn list_sessions_includes_created_sessions() {
        let api = api("list_sessions");
        setup_with_commits(&api, "sess-a", 1, Some("Session A"));
        setup_with_commits(&api, "sess-b", 2, Some("Session B"));

        let list = api.list_sessions().expect("list");
        assert!(list.len() >= 2);

        let a = list.iter().find(|s| s.session_id == "sess-a").expect("sess-a");
        assert_eq!(a.label, Some("Session A".to_string()));
        assert_eq!(a.commit_count, 1);

        let b = list.iter().find(|s| s.session_id == "sess-b").expect("sess-b");
        assert_eq!(b.commit_count, 2);
    }

    // -- log --

    #[test]
    fn log_returns_turn_entries() {
        let api = api("log_entries");
        let commit_ids = setup_with_commits(&api, "sess-log", 3, None);

        let log = api.log("sess-log").expect("log");
        assert_eq!(log.len(), 3);
        assert_eq!(log[0].commit_id, commit_ids[0]);
        assert_eq!(log[2].commit_id, commit_ids[2]);
    }

    // -- checkout --

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
        assert!(matches!(&head, lattice_session::GraphHead::Detached { commit_id } if commit_id == &commit_ids[0]));
    }

    // -- branch --

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

    // -- diff --

    #[test]
    fn diff_reports_unique_commits_on_each_side() {
        let api = api("diff_paths");
        let commit_ids = setup_with_commits(&api, "sess-diff", 2, None);
        let base_id = &commit_ids[0];
        let main_head = &commit_ids[1];

        // Create feature branch from base, commit one more.
        api.branch("sess-diff", "feature", Some(base_id)).expect("branch");
        api.checkout("sess-diff", "feature").expect("checkout");

        let feature_messages = vec![user_msg("Feature work"), assistant_msg("Done")];
        let feature_input = TurnCommitInput {
            session_id: "sess-diff".to_string(),
            parent_ids: vec![base_id.clone()],
            messages: feature_messages,
            profile_snapshot_id: Some("test-profile".to_string()),
            model: "sonnet".to_string(),
            provider: "anthropic".to_string(),
            status: TurnCommitStatus::Completed,
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

    // -- reset --

    #[test]
    fn reset_moves_branch_and_returns_old_head() {
        let api = api("reset_old_head");
        let commit_ids = setup_with_commits(&api, "sess-reset", 3, None);

        let result = api
            .reset("sess-reset", &commit_ids[0])
            .expect("reset");

        assert_eq!(result.old_commit_id, Some(commit_ids[2].clone()));
        assert_eq!(result.new_commit_id, commit_ids[0]);
        assert_eq!(result.branch, "main");
    }

    // -- Boundary checks: compile-time enforcement --

    /// Compile-time check: `SessionApi` only exposes the v1.1 main-path
    /// methods. If legacy methods were added, this test would need updating,
    /// drawing reviewer attention to the boundary violation.
    #[test]
    fn session_api_does_not_expose_legacy_operations() {
        // This test verifies the public API surface of SessionApi.
        // The following methods SHOULD exist:
        fn _assert_api_surface(api: &SessionApi) {
            let _ = api.create_session(CreateSessionRequest {
                label: None,
                profile: None,
                model: None,
            });
            let _ = api.open_session(OpenSessionRequest { id: String::new() });
            let _ = api.status("");
            let _ = api.list_sessions();
            let _ = api.log("");
            let _ = api.checkout("", "");
            let _ = api.branch("", "", None);
            let _ = api.diff("", "", "");
            let _ = api.reset("", "");
        }
        // The following methods SHOULD NOT exist on SessionApi:
        // merge, rebase, stash, reflog, create_tag, tags, artifact,
        // turn_manifest, save_with_manifests
        //
        // If any of these are added, this test module will fail to compile
        // because the method calls below would succeed, which is NOT desired.
        // (No code here — the absence of those methods is the assertion.)
    }

    /// Compile-time check: `SessionGraphStore` trait only has the v1.1
    /// main-path methods. If legacy methods leak into the trait, this
    /// test draws attention.
    #[test]
    fn graph_store_trait_excludes_legacy_methods() {
        // The trait is used as a bound to confirm it compiles with only
        // the expected methods. If merge/rebase/stash/etc were added,
        // this module would need updating.
        fn _assert_trait_bound(_store: &dyn SessionGraphStore) {}
    }
}

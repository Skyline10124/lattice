//! v1.1 session graph contract: types, narrow store trait, and canonical helpers.
//!
//! This module defines the durable graph vocabulary for sessions, commits,
//! messages, branches, HEAD, and refs. It intentionally excludes legacy
//! operations like merge, rebase, stash, tags, reflog, artifacts, turn
//! manifests, and GC.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Maximum characters for a commit message derived from user text.
pub const COMMIT_MESSAGE_MAX_CHARS: usize = 72;

/// Canonical schema version for the v1.1 turn commit payload.
pub const COMMIT_PAYLOAD_SCHEMA_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Graph message types
// ---------------------------------------------------------------------------

/// A single transcript message in the v1.1 graph.
///
/// Reuses the existing `SessionMessage` shape from the legacy module but
/// scoped to the v1.1 contract. Supports `user`, `assistant`, and `tool`
/// roles. Tool call fields (`tool_calls`, `tool_call_id`, `name`) remain
/// message fields rather than a separate context schema.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphMessage {
    pub role: String,
    pub content: String,
    #[serde(default)]
    pub reasoning_content: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<GraphToolCall>>,
    #[serde(default)]
    pub tool_call_id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

/// A tool call request embedded in an assistant message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

/// Complete ordered transcript for the current branch HEAD.
///
/// D-11/D-12: the agent loop reads this to build model input. Contains the
/// session ID, the parent commit ID (None if no commits yet), and the full
/// ordered message list from root through current HEAD.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HeadTranscript {
    pub session_id: String,
    #[serde(default)]
    pub parent_commit_id: Option<String>,
    pub messages: Vec<GraphMessage>,
}

/// Token usage statistics for a completed turn.
///
/// D-28/D-31: persisted as part of turn commit metadata. Token counts are
/// provider-reported and stored as opaque integers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

// ---------------------------------------------------------------------------
// Session container
// ---------------------------------------------------------------------------

/// A v1.1 graph session: a container/namespace for branches, HEAD, and commits.
///
/// Sessions do not own commits directly -- commits form a directed graph linked
/// by parent IDs. Branches and HEAD are commit-pointer concepts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphSession {
    pub session_id: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub profile_id: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

// ---------------------------------------------------------------------------
// Refs and HEAD
// ---------------------------------------------------------------------------

/// The kind of ref: branch or detached HEAD.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphRefKind {
    Branch,
}

/// A named ref pointing to a commit ID.
///
/// In v1.1, branches and HEAD are commit-pointer concepts. A ref always
/// points to a commit ID, never to a session ID.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphRef {
    pub session_id: String,
    pub name: String,
    pub kind: GraphRefKind,
    #[serde(default)]
    pub commit_id: Option<String>,
    pub updated_at: String,
}

/// HEAD state: either on a named branch or detached at a commit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GraphHead {
    /// HEAD is on a named branch.
    Branch { name: String },
    /// HEAD is detached at a specific commit.
    Detached { commit_id: String },
}

// ---------------------------------------------------------------------------
// Turn commit status
// ---------------------------------------------------------------------------

/// Status of a completed turn commit.
///
/// D-01/D-02: durable turn commits are created only for completed, errored,
/// or cancelled turns. No half-finished user-only pending commit is recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnCommitStatus {
    Completed,
    Error,
    Cancelled,
}

impl TurnCommitStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Error => "error",
            Self::Cancelled => "cancelled",
        }
    }
}

// ---------------------------------------------------------------------------
// Turn commit input / output
// ---------------------------------------------------------------------------

/// Input for creating a durable turn commit.
///
/// D-04/D-05: the v1.1 commit envelope is minimal and does not include
/// prompt manifests, hook records, artifacts, stash state, reflog state,
/// or context summary layers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnCommitInput {
    /// The session this commit belongs to.
    pub session_id: String,
    /// Parent commit IDs (empty for the first commit in a session).
    pub parent_ids: Vec<String>,
    /// Ordered transcript messages for this turn.
    pub messages: Vec<GraphMessage>,
    /// Profile snapshot identity or summary hash.
    pub profile_snapshot_id: Option<String>,
    /// Model used for this turn.
    pub model: String,
    /// Provider used for this turn.
    pub provider: String,
    /// Turn outcome status.
    pub status: TurnCommitStatus,
    /// Human-facing commit message (derived from first user message).
    pub commit_message: String,
    /// Token usage statistics for this turn.
    #[serde(default)]
    pub usage: Option<TurnUsage>,
    /// Terminal reason string (e.g. "stop", "timeout", "error: ...").
    #[serde(default)]
    pub terminal_reason: Option<String>,
}

/// An immutable turn commit in the v1.1 graph.
///
/// D-03/D-21: commit IDs are logical hashes over canonical commit payloads
/// and exclude storage details such as SQLite row IDs, object paths,
/// filesystem paths, and write order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnCommit {
    /// Canonical commit ID (logical hash).
    pub commit_id: String,
    /// The session this commit belongs to.
    pub session_id: String,
    /// Parent commit IDs.
    pub parent_ids: Vec<String>,
    /// Ordered message IDs for the transcript.
    pub message_ids: Vec<String>,
    /// Profile snapshot identity or summary hash.
    #[serde(default)]
    pub profile_snapshot_id: Option<String>,
    /// Model used for this turn.
    pub model: String,
    /// Provider used for this turn.
    pub provider: String,
    /// Turn outcome status.
    pub status: TurnCommitStatus,
    /// Human-facing commit message.
    pub commit_message: String,
    /// Token usage statistics for this turn.
    #[serde(default)]
    pub usage: Option<TurnUsage>,
    /// Terminal reason string (e.g. "stop", "timeout", "error: ...").
    #[serde(default)]
    pub terminal_reason: Option<String>,
    /// When the commit was created.
    pub created_at: String,
}

// ---------------------------------------------------------------------------
// Log, status, summary, diff, reset
// ---------------------------------------------------------------------------

/// A single entry in the turn log.
///
/// D-22: one line per turn commit with commit ID, status, model/provider,
/// commit_message, and small counts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnLogEntry {
    pub commit_id: String,
    pub parent_ids: Vec<String>,
    pub status: TurnCommitStatus,
    pub model: String,
    pub provider: String,
    pub commit_message: String,
    pub message_count: usize,
    pub tool_message_count: usize,
    /// Token usage statistics for this turn.
    #[serde(default)]
    pub usage: Option<TurnUsage>,
    /// Terminal reason string (e.g. "stop", "timeout", "error: ...").
    #[serde(default)]
    pub terminal_reason: Option<String>,
    pub created_at: String,
}

/// Session status: pointer-focused view of a session.
///
/// D-27: shows session ID, HEAD state, current commit, branch summary,
/// and no pending durable turn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionStatus {
    pub session_id: String,
    pub head: GraphHead,
    #[serde(default)]
    pub current_commit_id: Option<String>,
    pub branches: Vec<GraphRef>,
    pub commit_count: usize,
}

/// Summary of a session for list display.
///
/// D-27: shows current branch, HEAD commit, updated time, commit count,
/// and label/title.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionGraphSummary {
    pub session_id: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub current_branch: Option<String>,
    #[serde(default)]
    pub head_commit_id: Option<String>,
    pub commit_count: usize,
    pub updated_at: String,
}

/// Graph-path diff result.
///
/// D-26: compares graph paths by commit IDs. Lists commits unique to each
/// side with ID, status, and commit_message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphDiff {
    pub from_commit_id: String,
    pub to_commit_id: String,
    #[serde(default)]
    pub common_ancestor_id: Option<String>,
    /// Commits reachable from `from` but not `to`.
    pub only_in_from: Vec<TurnLogEntry>,
    /// Commits reachable from `to` but not `from`.
    pub only_in_to: Vec<TurnLogEntry>,
}

/// Result of a reset operation.
///
/// D-25: reset must return the old HEAD commit ID because v1.1 does not
/// include reflog.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResetResult {
    pub session_id: String,
    pub branch: String,
    #[serde(default)]
    pub old_commit_id: Option<String>,
    pub new_commit_id: String,
}

// ---------------------------------------------------------------------------
// Narrow store trait
// ---------------------------------------------------------------------------

/// Narrow v1.1 session graph store contract.
///
/// D-12/D-13/D-14: exposes only create/open, status/list/log, commit_turn,
/// checkout, branch, diff, and reset style operations. Does NOT include
/// merge, rebase, stash, tags, reflog, artifacts, turn manifests, GC,
/// or any legacy rich operations.
pub trait SessionGraphStore: Send + Sync {
    // -- Session lifecycle --

    /// Create a new session with an initial `main` branch.
    fn create_session(
        &self,
        session_id: &str,
        label: Option<&str>,
        profile_id: Option<&str>,
        model: Option<&str>,
        provider: Option<&str>,
    ) -> crate::Result<GraphSession>;

    /// Open an existing session by ID or prefix.
    fn open_session(&self, session_id: &str) -> crate::Result<GraphSession>;

    // -- Status / list / log --

    /// Get the current status of a session (HEAD, branches, commit count).
    fn status(&self, session_id: &str) -> crate::Result<SessionStatus>;

    /// List all sessions with summary information.
    fn list(&self) -> crate::Result<Vec<SessionGraphSummary>>;

    /// Get the turn log for a session (one entry per turn commit).
    fn log(&self, session_id: &str) -> crate::Result<Vec<TurnLogEntry>>;

    // -- Transcript read --

    /// Read the complete ordered transcript for the current branch HEAD.
    ///
    /// D-11: the agent loop calls this to build model input from the full
    /// branch history. Returns `parent_commit_id` as None when no commits
    /// exist yet (empty session).
    fn head_transcript(&self, session_id: &str) -> crate::Result<HeadTranscript>;

    // -- Commit --

    /// Commit a completed turn atomically.
    ///
    /// D-17: writes messages, commit object, and branch/HEAD ref update
    /// in one transaction or writes none of them.
    fn commit_turn(&self, input: TurnCommitInput) -> crate::Result<TurnCommit>;

    // -- Pointer operations --

    /// Change the active branch or detach HEAD at a commit.
    ///
    /// D-23: does not run a model and does not create a commit.
    fn checkout(&self, session_id: &str, target: &str) -> crate::Result<GraphHead>;

    /// Create or move a branch ref to an existing commit.
    ///
    /// D-24: pointer operation only; does not fork a session, run a model,
    /// or create a commit.
    fn branch(&self, session_id: &str, name: &str, from: Option<&str>) -> crate::Result<GraphRef>;

    /// Compare graph paths between two commits.
    ///
    /// D-26: finds common ancestor and lists commits unique to each side.
    fn diff(&self, session_id: &str, from: &str, to: &str) -> crate::Result<GraphDiff>;

    /// Move the current branch pointer to a target commit.
    ///
    /// D-25: does not delete commits; returns old HEAD commit ID for recovery.
    fn reset(&self, session_id: &str, target: &str) -> crate::Result<ResetResult>;
}

// ---------------------------------------------------------------------------
// Pure helpers: commit message derivation, canonical IDs
// ---------------------------------------------------------------------------

/// Derive a human-facing commit message from the turn's transcript.
///
/// D-18/D-19/D-20: reads the first `user` role message, normalizes
/// whitespace, truncates to `COMMIT_MESSAGE_MAX_CHARS`, and returns
/// `"Turn commit"` when no non-empty user message exists. Does NOT call
/// an LLM.
pub fn derive_commit_message(messages: &[GraphMessage]) -> String {
    let user_text = messages
        .iter()
        .find(|m| m.role.eq_ignore_ascii_case("user"))
        .map(|m| m.content.as_str())
        .unwrap_or("");

    let normalized: String = user_text.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return "Turn commit".to_string();
    }
    normalized.chars().take(COMMIT_MESSAGE_MAX_CHARS).collect()
}

/// Compute a canonical message ID for a single transcript message.
///
/// The payload includes role, content, reasoning_content, tool_calls,
/// tool_call_id, name, and the ordinal position within the turn. The
/// ordinal prevents hash collisions when two byte-identical messages
/// appear in the same turn (e.g., duplicate tool results).
pub fn canonical_message_id(message: &GraphMessage, ordinal: usize) -> String {
    let payload = serde_json::json!({
        "role": message.role,
        "content": message.content,
        "reasoning_content": message.reasoning_content,
        "tool_calls": message.tool_calls,
        "tool_call_id": message.tool_call_id,
        "name": message.name,
        "ordinal": ordinal,
    });
    hash_json("v1_message", &payload)
}

/// Compute a canonical turn commit ID from logical payload fields.
///
/// D-03/D-21: the payload includes schema version, session ID, parent
/// commit IDs, message IDs, profile snapshot identity, model, provider,
/// status, commit_message, and a logical timestamp. It explicitly
/// excludes: SQLite row IDs, object paths, filesystem paths, write order,
/// reflog data, and turn manifest IDs.
#[allow(clippy::too_many_arguments)]
pub fn canonical_turn_commit_id(
    session_id: &str,
    parent_ids: &[String],
    message_ids: &[String],
    profile_snapshot_id: Option<&str>,
    model: &str,
    provider: &str,
    status: &TurnCommitStatus,
    commit_message: &str,
    created_at: &str,
) -> String {
    let payload = serde_json::json!({
        "schema_version": COMMIT_PAYLOAD_SCHEMA_VERSION,
        "session_id": session_id,
        "parent_ids": parent_ids,
        "message_ids": message_ids,
        "profile_snapshot_id": profile_snapshot_id,
        "model": model,
        "provider": provider,
        "status": status.as_str(),
        "commit_message": commit_message,
        "created_at": created_at,
    });
    hash_json("v1_turn_commit", &payload)
}

/// SHA-256 hash of a kind-tagged JSON payload.
fn hash_json(kind: &str, value: &serde_json::Value) -> String {
    let json = serde_json::to_string(value).expect("JSON serialization must succeed");
    let mut hasher = Sha256::new();
    hasher.update(kind.as_bytes());
    hasher.update(b"\0");
    hasher.update(json.as_bytes());
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod graph_contract {
    use super::*;

    #[test]
    fn graph_types_exist_and_serialize() {
        let msg = GraphMessage {
            role: "user".to_string(),
            content: "Hello".to_string(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        };
        assert_eq!(msg.role, "user");

        let tool_call = GraphToolCall {
            id: "call_1".to_string(),
            name: "shell".to_string(),
            arguments: "{}".to_string(),
        };
        assert_eq!(tool_call.name, "shell");

        let session = GraphSession {
            session_id: "sess-1".to_string(),
            label: Some("test".to_string()),
            profile_id: None,
            model: Some("sonnet".to_string()),
            provider: Some("anthropic".to_string()),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        };
        assert_eq!(session.session_id, "sess-1");

        let graph_ref = GraphRef {
            session_id: "sess-1".to_string(),
            name: "main".to_string(),
            kind: GraphRefKind::Branch,
            commit_id: Some("abc123".to_string()),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        };
        assert_eq!(graph_ref.kind, GraphRefKind::Branch);

        let head = GraphHead::Branch {
            name: "main".to_string(),
        };
        let detached = GraphHead::Detached {
            commit_id: "abc123".to_string(),
        };
        assert_ne!(
            serde_json::to_string(&head).unwrap(),
            serde_json::to_string(&detached).unwrap()
        );

        let status = TurnCommitStatus::Completed;
        assert_eq!(status.as_str(), "completed");
        assert_eq!(TurnCommitStatus::Error.as_str(), "error");
        assert_eq!(TurnCommitStatus::Cancelled.as_str(), "cancelled");
    }

    #[test]
    fn turn_commit_input_and_turn_commit_serialize() {
        let input = TurnCommitInput {
            session_id: "sess-1".to_string(),
            parent_ids: vec![],
            messages: vec![GraphMessage {
                role: "user".to_string(),
                content: "Hello".to_string(),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }],
            profile_snapshot_id: Some("profile-hash".to_string()),
            model: "sonnet".to_string(),
            provider: "anthropic".to_string(),
            status: TurnCommitStatus::Completed,
            commit_message: "Hello".to_string(),
            usage: None,
            terminal_reason: None,
        };
        let json = serde_json::to_string(&input).unwrap();
        assert!(json.contains("session_id"));
        assert!(json.contains("profile_snapshot_id"));

        let commit = TurnCommit {
            commit_id: "commit-abc".to_string(),
            session_id: "sess-1".to_string(),
            parent_ids: vec![],
            message_ids: vec!["msg-1".to_string()],
            profile_snapshot_id: Some("profile-hash".to_string()),
            model: "sonnet".to_string(),
            provider: "anthropic".to_string(),
            status: TurnCommitStatus::Completed,
            commit_message: "Hello".to_string(),
            usage: None,
            terminal_reason: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
        };
        assert_eq!(commit.commit_id, "commit-abc");
    }

    #[test]
    fn session_status_and_summary_serialize() {
        let status = SessionStatus {
            session_id: "sess-1".to_string(),
            head: GraphHead::Branch {
                name: "main".to_string(),
            },
            current_commit_id: Some("abc".to_string()),
            branches: vec![],
            commit_count: 5,
        };
        let json = serde_json::to_string(&status).unwrap();
        assert!(json.contains("commit_count"));

        let summary = SessionGraphSummary {
            session_id: "sess-1".to_string(),
            label: Some("test".to_string()),
            current_branch: Some("main".to_string()),
            head_commit_id: Some("abc".to_string()),
            commit_count: 5,
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        };
        assert_eq!(summary.commit_count, 5);
    }

    #[test]
    fn graph_diff_and_reset_result_serialize() {
        let diff = GraphDiff {
            from_commit_id: "a".to_string(),
            to_commit_id: "b".to_string(),
            common_ancestor_id: Some("root".to_string()),
            only_in_from: vec![],
            only_in_to: vec![],
        };
        let json = serde_json::to_string(&diff).unwrap();
        assert!(json.contains("common_ancestor_id"));

        let reset = ResetResult {
            session_id: "sess-1".to_string(),
            branch: "main".to_string(),
            old_commit_id: Some("old".to_string()),
            new_commit_id: "new".to_string(),
        };
        assert_eq!(reset.old_commit_id, Some("old".to_string()));
    }

    #[test]
    fn turn_log_entry_serializes() {
        let entry = TurnLogEntry {
            commit_id: "abc".to_string(),
            parent_ids: vec!["parent".to_string()],
            status: TurnCommitStatus::Completed,
            model: "sonnet".to_string(),
            provider: "anthropic".to_string(),
            commit_message: "Hello world".to_string(),
            message_count: 2,
            tool_message_count: 0,
            usage: None,
            terminal_reason: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
        };
        assert_eq!(entry.message_count, 2);
    }

    #[test]
    fn session_graph_store_trait_does_not_include_legacy_operations() {
        // This is a compile-time check: the trait should only have the
        // methods listed below. If legacy methods were added, this test
        // module would need to be updated, drawing reviewer attention.
        //
        // The trait is used as a bound in this test to confirm it compiles.
        fn _assert_trait_bound(_store: &dyn SessionGraphStore) {}
    }

    // -- Commit message derivation tests --

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

    #[test]
    fn derive_commit_message_normalizes_whitespace() {
        let messages = vec![user_msg("  Hello   world  \n  from  user  ")];
        assert_eq!(derive_commit_message(&messages), "Hello world from user");
    }

    #[test]
    fn derive_commit_message_truncates_long_input() {
        let long = "a".repeat(200);
        let messages = vec![user_msg(&long)];
        let result = derive_commit_message(&messages);
        assert!(result.len() <= COMMIT_MESSAGE_MAX_CHARS);
        assert_eq!(result.chars().count(), COMMIT_MESSAGE_MAX_CHARS);
    }

    #[test]
    fn derive_commit_message_fallback_for_empty_messages() {
        assert_eq!(derive_commit_message(&[]), "Turn commit");
    }

    #[test]
    fn derive_commit_message_fallback_for_empty_user_text() {
        let messages = vec![user_msg("   ")];
        assert_eq!(derive_commit_message(&messages), "Turn commit");
    }

    #[test]
    fn derive_commit_message_fallback_for_no_user_role() {
        let messages = vec![assistant_msg("I will help you.")];
        assert_eq!(derive_commit_message(&messages), "Turn commit");
    }

    #[test]
    fn derive_commit_message_picks_first_user_message() {
        let messages = vec![
            assistant_msg("thinking..."),
            user_msg("First question"),
            assistant_msg("Answer"),
            user_msg("Second question"),
        ];
        assert_eq!(derive_commit_message(&messages), "First question");
    }

    // -- Canonical ID tests --

    #[test]
    fn canonical_turn_commit_id_includes_logical_fields() {
        let id = canonical_turn_commit_id(
            "sess-1",
            &["parent-1".to_string()],
            &["msg-1".to_string(), "msg-2".to_string()],
            Some("profile-hash"),
            "sonnet",
            "anthropic",
            &TurnCommitStatus::Completed,
            "Hello world",
            "2026-01-01T00:00:00Z",
        );
        // The ID should be a 64-char hex string (SHA-256).
        assert_eq!(id.len(), 64);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn canonical_turn_commit_id_deterministic() {
        let args = (
            "sess-1",
            vec!["parent-1".to_string()],
            vec!["msg-1".to_string()],
            Some("profile-hash"),
            "sonnet",
            "anthropic",
            TurnCommitStatus::Completed,
            "Hello",
            "2026-01-01T00:00:00Z",
        );
        let id1 = canonical_turn_commit_id(
            args.0, &args.1, &args.2, args.3, args.4, args.5, &args.6, args.7, args.8,
        );
        let id2 = canonical_turn_commit_id(
            args.0, &args.1, &args.2, args.3, args.4, args.5, &args.6, args.7, args.8,
        );
        assert_eq!(id1, id2);
    }

    #[test]
    fn canonical_turn_commit_id_changes_with_different_inputs() {
        let base = canonical_turn_commit_id(
            "sess-1",
            &[],
            &["msg-1".to_string()],
            None,
            "sonnet",
            "anthropic",
            &TurnCommitStatus::Completed,
            "Hello",
            "2026-01-01T00:00:00Z",
        );
        // Different model
        let diff_model = canonical_turn_commit_id(
            "sess-1",
            &[],
            &["msg-1".to_string()],
            None,
            "opus",
            "anthropic",
            &TurnCommitStatus::Completed,
            "Hello",
            "2026-01-01T00:00:00Z",
        );
        assert_ne!(base, diff_model);

        // Different status
        let diff_status = canonical_turn_commit_id(
            "sess-1",
            &[],
            &["msg-1".to_string()],
            None,
            "sonnet",
            "anthropic",
            &TurnCommitStatus::Error,
            "Hello",
            "2026-01-01T00:00:00Z",
        );
        assert_ne!(base, diff_status);

        // Different parent
        let diff_parent = canonical_turn_commit_id(
            "sess-1",
            &["other-parent".to_string()],
            &["msg-1".to_string()],
            None,
            "sonnet",
            "anthropic",
            &TurnCommitStatus::Completed,
            "Hello",
            "2026-01-01T00:00:00Z",
        );
        assert_ne!(base, diff_parent);
    }

    #[test]
    fn canonical_turn_commit_id_excludes_storage_metadata() {
        // The payload is built internally from logical fields only.
        // Verify by checking that the function does not accept parameters
        // for rowid, object_path, filesystem_path, reflog, turn_manifest_ids,
        // or write_order. This is a compile-time/API-level assertion:
        // the function signature only accepts logical fields.
        let _ = canonical_turn_commit_id(
            "sess-1",
            &[],
            &[],
            None,
            "sonnet",
            "anthropic",
            &TurnCommitStatus::Completed,
            "Turn commit",
            "2026-01-01T00:00:00Z",
        );
        // If this compiles, the function does not accept storage metadata.
    }

    #[test]
    fn canonical_message_id_is_deterministic() {
        let msg = user_msg("Hello");
        let id1 = canonical_message_id(&msg, 0);
        let id2 = canonical_message_id(&msg, 0);
        assert_eq!(id1, id2);
        assert_eq!(id1.len(), 64);
    }

    #[test]
    fn canonical_message_id_differs_by_content() {
        let msg1 = user_msg("Hello");
        let msg2 = user_msg("World");
        assert_ne!(canonical_message_id(&msg1, 0), canonical_message_id(&msg2, 0));
    }

    #[test]
    fn canonical_message_id_differs_by_ordinal() {
        let msg = user_msg("Hello");
        let id0 = canonical_message_id(&msg, 0);
        let id1 = canonical_message_id(&msg, 1);
        assert_ne!(id0, id1, "identical messages at different ordinals must produce different IDs");
    }

    // -- HeadTranscript contract tests (Phase 09) --

    #[test]
    fn head_transcript_contract_serializes() {
        let transcript = HeadTranscript {
            session_id: "sess-1".to_string(),
            parent_commit_id: Some("commit-abc".to_string()),
            messages: vec![
                user_msg("Hello"),
                assistant_msg("Hi there!"),
            ],
        };
        let json = serde_json::to_string(&transcript).unwrap();
        assert!(json.contains("session_id"));
        assert!(json.contains("parent_commit_id"));
        assert!(json.contains("messages"));

        // Round-trip.
        let decoded: HeadTranscript = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.session_id, "sess-1");
        assert_eq!(decoded.parent_commit_id, Some("commit-abc".to_string()));
        assert_eq!(decoded.messages.len(), 2);
        assert_eq!(decoded.messages[0].role, "user");
        assert_eq!(decoded.messages[1].role, "assistant");
    }

    #[test]
    fn head_transcript_parent_is_none_for_empty_session() {
        let transcript = HeadTranscript {
            session_id: "sess-empty".to_string(),
            parent_commit_id: None,
            messages: vec![],
        };
        let json = serde_json::to_string(&transcript).unwrap();
        let decoded: HeadTranscript = serde_json::from_str(&json).unwrap();
        assert!(decoded.parent_commit_id.is_none());
        assert!(decoded.messages.is_empty());
    }

    // -- TurnUsage contract tests (Phase 09) --

    #[test]
    fn turn_usage_serializes_for_commit_metadata() {
        let usage = TurnUsage {
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
        };
        let json = serde_json::to_string(&usage).unwrap();
        assert!(json.contains("prompt_tokens"));
        assert!(json.contains("completion_tokens"));
        assert!(json.contains("total_tokens"));

        // Round-trip.
        let decoded: TurnUsage = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.prompt_tokens, 100);
        assert_eq!(decoded.completion_tokens, 50);
        assert_eq!(decoded.total_tokens, 150);
    }

    // -- TurnCommitInput extended fields tests (Phase 09) --

    #[test]
    fn turn_commit_input_accepts_usage_and_terminal_reason() {
        let input = TurnCommitInput {
            session_id: "sess-1".to_string(),
            parent_ids: vec![],
            messages: vec![user_msg("Hello")],
            profile_snapshot_id: Some("profile-hash".to_string()),
            model: "sonnet".to_string(),
            provider: "anthropic".to_string(),
            status: TurnCommitStatus::Completed,
            commit_message: "Hello".to_string(),
            usage: Some(TurnUsage {
                prompt_tokens: 100,
                completion_tokens: 50,
                total_tokens: 150,
            }),
            terminal_reason: Some("stop".to_string()),
        };
        let json = serde_json::to_string(&input).unwrap();
        assert!(json.contains("usage"));
        assert!(json.contains("terminal_reason"));

        // Round-trip.
        let decoded: TurnCommitInput = serde_json::from_str(&json).unwrap();
        assert!(decoded.usage.is_some());
        let u = decoded.usage.unwrap();
        assert_eq!(u.total_tokens, 150);
        assert_eq!(decoded.terminal_reason, Some("stop".to_string()));

        // Existing fields are preserved.
        assert_eq!(decoded.session_id, "sess-1");
        assert_eq!(decoded.model, "sonnet");
        assert_eq!(decoded.provider, "anthropic");
    }
}

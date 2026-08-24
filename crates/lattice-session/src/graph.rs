// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Session graph contract: types, narrow store trait, and canonical helpers.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const COMMIT_MESSAGE_MAX_CHARS: usize = 72;
pub const COMMIT_PAYLOAD_SCHEMA_VERSION: u32 = 3;

// Conversation pure-data types are owned by `lattice-conversation` since
// GitHub #142 (ADR 0005 §7); this crate re-exports them unchanged and remains
// the persistence owner. `TurnUsage` is the historical name of the
// provider-neutral `TokenUsage` contract.
use crate::pending::{ClosureOrigin, TurnOutcome, WorkspaceHistoryCheckpoint};
pub use lattice_conversation::TokenUsage as TurnUsage;
use lattice_conversation::{BoundToolCall, MessageSemantic, NativeMessage};
pub use lattice_conversation::{GraphMessage, GraphToolCall, HeadTranscript};

/// A session: container for branches, HEAD, and commits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphSession {
    pub session_id: String,
    #[serde(default)]
    pub label: Option<String>,
    pub active_agent_name: String,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub project_target_branch: Option<String>,
    /// Lowercase-hex `ProjectStoreKey` this session is permanently bound to
    /// (ADR 0005 §2, since GitHub #143).
    pub project_store_key: String,
}

/// Ref kind: branch or detached HEAD.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphRefKind {
    Branch,
}

/// A named ref pointing to a commit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphRef {
    pub session_id: String,
    pub name: String,
    pub kind: GraphRefKind,
    #[serde(default)]
    pub commit_id: Option<String>,
    pub active_agent_name: String,
    pub updated_at: String,
}

/// HEAD state: on a branch or detached at a commit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphHead {
    Branch { name: String },
    Detached { commit_id: String },
}

// ─── Turn DTOs ────────────────────────────────────────────────────────────

/// Input for creating a turn commit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnCommitInput {
    pub session_id: String,
    /// Explicit canonical Turn identity (#139-B, GitHub #100-B/M-GH100-01).
    ///
    /// Stored verbatim in the commit manifest and included in every canonical
    /// identity payload, so two otherwise-identical sibling Turns cannot
    /// collide.
    pub turn_id: lancet_protocol::TurnId,
    pub parent_ids: Vec<String>,
    pub messages: Vec<NativeMessage<BoundToolCall>>,
    pub agent_name: String,
    pub profile_snapshot_hash: String,
    pub profile_snapshot_json: String,
    pub model: String,
    pub provider: String,
    pub outcome: TurnOutcome,
    pub origin: ClosureOrigin,
    pub commit_message: String,
    #[serde(default)]
    pub usage: Option<TurnUsage>,
    /// Project-global WorkspaceHistory cut this Turn arrived at.
    pub workspace_history_checkpoint: WorkspaceHistoryCheckpoint,
}

/// An immutable turn commit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnCommit {
    pub commit_id: String,
    pub session_id: String,
    /// Explicit canonical Turn identity, durable in the manifest (#139-B,
    /// M-GH100-01).
    pub turn_id: lancet_protocol::TurnId,
    pub parent_ids: Vec<String>,
    pub message_ids: Vec<String>,
    pub agent_name: String,
    pub profile_snapshot_hash: String,
    pub profile_snapshot_json: String,
    pub model: String,
    pub provider: String,
    pub outcome: TurnOutcome,
    pub origin: ClosureOrigin,
    pub commit_message: String,
    #[serde(default)]
    pub usage: Option<TurnUsage>,
    /// Project-global WorkspaceHistory cut this Turn arrived at.
    pub workspace_history_checkpoint: WorkspaceHistoryCheckpoint,
    pub created_at: String,
}

/// A single turn log entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnLogEntry {
    pub commit_id: String,
    /// Explicit canonical Turn identity.
    pub turn_id: lancet_protocol::TurnId,
    pub parent_ids: Vec<String>,
    pub outcome: TurnOutcome,
    pub origin: ClosureOrigin,
    pub agent_name: String,
    pub profile_snapshot_hash: String,
    pub model: String,
    pub provider: String,
    pub commit_message: String,
    pub message_count: usize,
    pub tool_message_count: usize,
    #[serde(default)]
    pub usage: Option<TurnUsage>,
    pub workspace_history_checkpoint: WorkspaceHistoryCheckpoint,
    pub created_at: String,
}

/// Session status: HEAD, branches, commit count.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionStatus {
    pub session_id: String,
    pub head: GraphHead,
    #[serde(default)]
    pub current_commit_id: Option<String>,
    pub active_agent_name: String,
    pub branches: Vec<GraphRef>,
    pub commit_count: usize,
    #[serde(default)]
    pub project_target_branch: Option<String>,
    /// The `ProjectStoreKey` binding (lowercase hex) of this session.
    pub project_store_key: String,
}

/// Session summary for list display.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionGraphSummary {
    pub session_id: String,
    #[serde(default)]
    pub label: Option<String>,
    pub active_agent_name: String,
    #[serde(default)]
    pub current_branch: Option<String>,
    #[serde(default)]
    pub head_commit_id: Option<String>,
    pub commit_count: usize,
    pub updated_at: String,
    #[serde(default)]
    pub project_target_branch: Option<String>,
    /// The `ProjectStoreKey` binding (lowercase hex) of this session.
    pub project_store_key: String,
}

/// Graph-path diff result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphDiff {
    pub from_commit_id: String,
    pub to_commit_id: String,
    #[serde(default)]
    pub common_ancestor_id: Option<String>,
    pub only_in_from: Vec<TurnLogEntry>,
    pub only_in_to: Vec<TurnLogEntry>,
}

/// Result of a reset operation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResetResult {
    pub session_id: String,
    pub branch: String,
    #[serde(default)]
    pub old_commit_id: Option<String>,
    pub new_commit_id: String,
}

/// Narrow session graph store contract.
pub trait SessionGraphStore: Send + Sync {
    /// Create a session permanently bound to `project_store_key` (ADR 0005
    /// §2: the key is a storage address derived by the composition layer;
    /// this crate never derives it from the process cwd).
    fn create_session(
        &self,
        session_id: &str,
        label: Option<&str>,
        active_agent_name: &str,
        project_target_branch: &str,
        project_store_key: &lattice_store::ProjectStoreKey,
    ) -> crate::Result<GraphSession>;

    fn open_session(&self, session_id: &str) -> crate::Result<GraphSession>;

    fn status(&self, session_id: &str) -> crate::Result<SessionStatus>;

    fn list(&self) -> crate::Result<Vec<SessionGraphSummary>>;

    fn log(&self, session_id: &str) -> crate::Result<Vec<TurnLogEntry>>;

    fn head_transcript(&self, session_id: &str) -> crate::Result<HeadTranscript>;

    fn commit_turn(&self, input: TurnCommitInput) -> crate::Result<TurnCommit>;

    fn checkout(&self, session_id: &str, target: &str) -> crate::Result<GraphHead>;

    fn branch(&self, session_id: &str, name: &str, from: Option<&str>) -> crate::Result<GraphRef>;

    fn set_active_agent(&self, session_id: &str, agent_name: &str) -> crate::Result<GraphSession>;

    fn diff(&self, session_id: &str, from: &str, to: &str) -> crate::Result<GraphDiff>;

    fn reset(&self, session_id: &str, target: &str) -> crate::Result<ResetResult>;

    /// Persist a precomputed turn commit with prepared identity.
    ///
    /// Uses the prepared timestamp, message IDs, and commit ID. Validates current
    /// HEAD as CAS. Rejects a same-ID row whose payload differs.
    fn commit_prepared_turn(&self, prepared: PreparedTurnCommit) -> crate::Result<TurnCommit>;

    /// Look up a single turn commit by session and commit ID.
    ///
    /// Returns `None` if not found. Decodes lifecycle evidence strictly.
    fn get_turn_commit(
        &self,
        session_id: &str,
        commit_id: &str,
    ) -> crate::Result<Option<TurnCommit>>;

    /// Acquire the per-session execution lock (ADR 0005 §5): exclusive for
    /// the full live Turn, reconciliation, rewind, reset, checkout, branch
    /// switch, and every Session state/branch mutation. Same-process
    /// re-entry is only legal when the caller explicitly passes the existing
    /// guard to a `*_locked` method; an independent acquisition is a real
    /// conflict. A non-blocking shared acquisition returns
    /// `SessionError::SessionBusy` when an exclusive owner exists.
    fn lock_execution(
        &self,
        session_id: &str,
        mode: lattice_store::LockMode,
        blocking: lattice_store::Blocking,
    ) -> crate::Result<lattice_store::FlockGuard>;

    /// Commit while the caller already holds the SessionExecutionLock. The
    /// explicit guard prevents lower layers from guessing same-task ownership;
    /// production implementations skip a second path acquisition.
    fn commit_turn_locked(
        &self,
        input: TurnCommitInput,
        _execution_lock: &lattice_store::FlockGuard,
    ) -> crate::Result<TurnCommit>;

    /// Read mutable status while the caller already holds the execution lock.
    fn status_locked(
        &self,
        session_id: &str,
        _execution_lock: &lattice_store::FlockGuard,
    ) -> crate::Result<SessionStatus>;

    /// Branch mutation while the caller already holds the execution lock.
    fn branch_locked(
        &self,
        session_id: &str,
        name: &str,
        from: Option<&str>,
        _execution_lock: &lattice_store::FlockGuard,
    ) -> crate::Result<GraphRef>;

    /// Checkout mutation while the caller already holds the execution lock.
    fn checkout_locked(
        &self,
        session_id: &str,
        target: &str,
        _execution_lock: &lattice_store::FlockGuard,
    ) -> crate::Result<GraphHead>;

    /// Reset mutation while the caller already holds the execution lock.
    fn reset_locked(
        &self,
        session_id: &str,
        target: &str,
        _execution_lock: &lattice_store::FlockGuard,
    ) -> crate::Result<ResetResult>;
}

/// Derive commit message from first user message.
pub fn derive_commit_message(messages: &[NativeMessage<BoundToolCall>]) -> String {
    let user_text = messages
        .iter()
        .find(|message| message.role == lattice_conversation::MessageRole::User)
        .and_then(|message| {
            message.parts.iter().find_map(|part| match &part.semantic {
                MessageSemantic::Text { text } => Some(text.as_str()),
                _ => None,
            })
        })
        .unwrap_or("");

    let normalized: String = user_text.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return "Turn commit".to_string();
    }
    normalized.chars().take(COMMIT_MESSAGE_MAX_CHARS).collect()
}

/// Compute canonical message ID scoped to session and turn.
pub fn canonical_message_id(
    session_id: &str,
    turn_seed: &str,
    message: &NativeMessage<BoundToolCall>,
    ordinal: usize,
) -> String {
    let payload = serde_json::json!({
        "session_id": session_id,
        "turn_seed": turn_seed,
        "message": message,
        "ordinal": ordinal,
    });
    hash_json("v2_native_message", &payload)
}

/// Compute a stable turn seed for scoping message IDs within a commit.
pub fn canonical_turn_seed(input: &TurnCommitInput, created_at: &str) -> String {
    let payload = serde_json::json!({
        "schema_version": COMMIT_PAYLOAD_SCHEMA_VERSION,
        "session_id": input.session_id,
        "parents": input.parent_ids,
        "created_at": created_at,
        "model": input.model,
        "provider": input.provider,
        "outcome": input.outcome,
        "origin": input.origin,
        "workspace_history_checkpoint": input.workspace_history_checkpoint,
        "commit_message": input.commit_message,
        "turn_id": input.turn_id.as_str(),
    });
    hash_json("v3_turn_seed", &payload)
}

/// Precomputed turn commit identity, produced before any storage effects.
#[derive(Debug, Clone)]
pub struct PreparedTurnCommit {
    /// The computed commit ID (SHA-256 hex).
    pub commit_id: String,
    /// Computed message IDs in transcript order.
    pub message_ids: Vec<String>,
    /// The turn seed used for message ID scoping.
    pub turn_seed: String,
    /// The full immutable turn commit.
    pub commit: TurnCommit,
    /// Original messages in transcript order, carried for persistence.
    pub messages: Vec<NativeMessage<BoundToolCall>>,
}

/// Compute the final immutable turn commit ID before any storage effects.
pub fn prepare_turn_commit(input: &TurnCommitInput, created_at: &str) -> PreparedTurnCommit {
    let turn_seed = canonical_turn_seed(input, created_at);

    let message_ids: Vec<String> = input
        .messages
        .iter()
        .enumerate()
        .map(|(i, msg)| canonical_message_id(&input.session_id, &turn_seed, msg, i))
        .collect();

    let commit_id = canonical_turn_commit_id_v3(input, &message_ids, created_at);

    let commit = TurnCommit {
        commit_id: commit_id.clone(),
        session_id: input.session_id.clone(),
        turn_id: input.turn_id,
        parent_ids: input.parent_ids.clone(),
        message_ids: message_ids.clone(),
        agent_name: input.agent_name.clone(),
        profile_snapshot_hash: input.profile_snapshot_hash.clone(),
        profile_snapshot_json: input.profile_snapshot_json.clone(),
        model: input.model.clone(),
        provider: input.provider.clone(),
        outcome: input.outcome.clone(),
        origin: input.origin,
        commit_message: input.commit_message.clone(),
        usage: input.usage,
        workspace_history_checkpoint: input.workspace_history_checkpoint.clone(),
        created_at: created_at.to_string(),
    };

    PreparedTurnCommit {
        commit_id,
        message_ids,
        turn_seed,
        commit,
        messages: input.messages.clone(),
    }
}

fn canonical_turn_commit_id_v3(
    input: &TurnCommitInput,
    message_ids: &[String],
    created_at: &str,
) -> String {
    let payload = serde_json::json!({
        "schema_version": COMMIT_PAYLOAD_SCHEMA_VERSION,
        "session_id": input.session_id,
        "parent_ids": input.parent_ids,
        "message_ids": message_ids,
        "agent_name": input.agent_name,
        "profile_snapshot_hash": input.profile_snapshot_hash,
        "model": input.model,
        "provider": input.provider,
        "outcome": input.outcome,
        "origin": input.origin,
        "workspace_history_checkpoint": input.workspace_history_checkpoint,
        "commit_message": input.commit_message,
        "created_at": created_at,
        "turn_id": input.turn_id.as_str(),
    });
    hash_json("v3_turn_commit", &payload)
}

fn hash_json(kind: &str, value: &serde_json::Value) -> String {
    let json = canonical_json(value);
    let mut hasher = Sha256::new();
    hasher.update(kind.as_bytes());
    hasher.update(b"\0");
    hasher.update(json.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Canonicalize JSON by sorting object keys alphabetically (recursive).
pub(crate) fn canonical_json_bytes(value: &serde_json::Value) -> Vec<u8> {
    canonical_json(value).into_bytes()
}

/// Canonicalize JSON by sorting object keys alphabetically (recursive).
fn canonical_json(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Object(map) => {
            let mut keys: Vec<_> = map.keys().collect();
            keys.sort();
            let pairs: Vec<String> = keys
                .iter()
                .map(|k| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(k).unwrap(),
                        canonical_json(&map[*k])
                    )
                })
                .collect();
            format!("{{{}}}", pairs.join(","))
        }
        serde_json::Value::Array(arr) => {
            let items: Vec<String> = arr.iter().map(canonical_json).collect();
            format!("[{}]", items.join(","))
        }
        _ => serde_json::to_string(value).unwrap(),
    }
}

#[cfg(test)]
mod graph_contract {
    use super::*;
    use crate::pending::{
        ClosureOrigin, TurnFinishReason, TurnOutcome, WorkspaceHistoryCheckpoint,
    };

    fn input(turn_id: &str) -> TurnCommitInput {
        TurnCommitInput {
            session_id: "sess-1".to_string(),
            turn_id: lancet_protocol::TurnId::parse(turn_id).expect("TurnId"),
            parent_ids: vec![],
            messages: vec![crate::test_helpers::user_msg("Hello")],
            agent_name: "coder".to_string(),
            profile_snapshot_hash: "profile-hash".to_string(),
            profile_snapshot_json: "{}".to_string(),
            model: "sonnet".to_string(),
            provider: "anthropic".to_string(),
            outcome: TurnOutcome::Completed {
                finish_reason: TurnFinishReason::Stop,
            },
            origin: ClosureOrigin::Live,
            commit_message: "Hello".to_string(),
            usage: None,
            workspace_history_checkpoint: WorkspaceHistoryCheckpoint::Empty,
        }
    }

    #[test]
    fn typed_turn_schema_round_trips_without_legacy_fields() {
        let value = serde_json::to_value(input("3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c")).unwrap();
        let object = value.as_object().unwrap();
        assert!(object.contains_key("outcome"));
        assert!(object.contains_key("origin"));
        assert!(object.contains_key("workspace_history_checkpoint"));
        for retired in ["status", "disposition", "lifecycle", "terminal_reason"] {
            assert!(!object.contains_key(retired));
        }
        let _: TurnCommitInput = serde_json::from_value(value).unwrap();
    }

    #[test]
    fn explicit_turn_id_distinguishes_identical_siblings() {
        let first = prepare_turn_commit(
            &input("3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c"),
            "2026-01-01T00:00:00Z",
        );
        let second = prepare_turn_commit(
            &input("4f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c"),
            "2026-01-01T00:00:00Z",
        );
        assert_ne!(first.turn_seed, second.turn_seed);
        assert_ne!(first.message_ids, second.message_ids);
        assert_ne!(first.commit_id, second.commit_id);
    }

    #[test]
    fn typed_terminal_facts_affect_identity() {
        let base = input("3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c");
        let completed = prepare_turn_commit(&base, "2026-01-01T00:00:00Z");
        let interrupted = prepare_turn_commit(
            &TurnCommitInput {
                outcome: TurnOutcome::Interrupted {
                    reason: crate::pending::TurnInterruption::ProcessInterruptedBeforeModelAttempt,
                },
                origin: ClosureOrigin::Reconciled,
                workspace_history_checkpoint: WorkspaceHistoryCheckpoint::Oid("abc".to_string()),
                ..base
            },
            "2026-01-01T00:00:00Z",
        );
        assert_ne!(completed.commit_id, interrupted.commit_id);
    }

    #[test]
    fn derive_commit_message_normalizes_user_text() {
        assert_eq!(
            derive_commit_message(&[crate::test_helpers::user_msg("  Hello   world  ")]),
            "Hello world"
        );
    }
}

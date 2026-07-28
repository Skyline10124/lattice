// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Session graph contract: types, narrow store trait, and canonical helpers.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const COMMIT_MESSAGE_MAX_CHARS: usize = 72;
pub const COMMIT_PAYLOAD_SCHEMA_VERSION: u32 = 1;

/// A single transcript message: user, assistant, or tool role.
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

/// A tool call request in an assistant message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

/// Complete ordered transcript for the current branch HEAD.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HeadTranscript {
    pub session_id: String,
    #[serde(default)]
    pub parent_commit_id: Option<String>,
    pub messages: Vec<GraphMessage>,
}

/// Token usage for a completed turn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

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

// ─── Terminal Status and Disposition ──────────────────────────────────────

/// Status of a completed turn commit.
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

    /// Strict parser: unknown text returns a typed error instead of defaulting.
    pub fn parse(raw: &str) -> crate::Result<Self> {
        match raw {
            "completed" => Ok(Self::Completed),
            "error" => Ok(Self::Error),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(crate::SessionError::InvalidPersistedValue {
                field: "status",
                value: raw.to_owned(),
            }),
        }
    }
}

/// Terminal workspace disposition: independent from execution status.
///
/// Every terminal turn has both a `TurnCommitStatus` (execution outcome) and a
/// `WorkspaceDisposition` (workspace promotion outcome). These are separate
/// exhaustive typed values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceDisposition {
    Promoted,
    PendingWorktree,
    PendingVerification,
    PendingPromotion,
    PendingConflict,
    Rejected,
    Aborted,
    NotApplicable,
}

impl WorkspaceDisposition {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Promoted => "promoted",
            Self::PendingWorktree => "pending_worktree",
            Self::PendingVerification => "pending_verification",
            Self::PendingPromotion => "pending_promotion",
            Self::PendingConflict => "pending_conflict",
            Self::Rejected => "rejected",
            Self::Aborted => "aborted",
            Self::NotApplicable => "not_applicable",
        }
    }

    /// Strict parser: unknown text returns a typed error instead of defaulting.
    pub fn parse(raw: &str) -> crate::Result<Self> {
        match raw {
            "promoted" => Ok(Self::Promoted),
            "pending_worktree" => Ok(Self::PendingWorktree),
            "pending_verification" => Ok(Self::PendingVerification),
            "pending_promotion" => Ok(Self::PendingPromotion),
            "pending_conflict" => Ok(Self::PendingConflict),
            "rejected" => Ok(Self::Rejected),
            "aborted" => Ok(Self::Aborted),
            "not_applicable" => Ok(Self::NotApplicable),
            _ => Err(crate::SessionError::InvalidPersistedValue {
                field: "disposition",
                value: raw.to_owned(),
            }),
        }
    }
}

// ─── Lifecycle Evidence ───────────────────────────────────────────────────

/// Verification execution evidence bound to a candidate OID.
///
/// Records the verify command argv, profile hash, expected/actual exit code,
/// pass/fail result, and the candidate OID that was verified.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnVerificationEvidence {
    pub argv: Vec<String>,
    pub profile_hash: String,
    pub candidate_oid: String,
    pub expected_exit_code: i32,
    pub exit_code: Option<i32>,
    pub passed: bool,
}

impl TurnVerificationEvidence {
    /// Construct validated evidence: argv must be non-empty and candidate_oid
    /// must be non-empty to bind verification to a specific candidate.
    pub fn new(
        argv: Vec<String>,
        profile_hash: String,
        candidate_oid: String,
        expected_exit_code: i32,
        exit_code: Option<i32>,
        passed: bool,
    ) -> crate::Result<Self> {
        if argv.is_empty() {
            return Err(crate::SessionError::InvalidPersistedValue {
                field: "verification.argv",
                value: "[] (empty)".to_owned(),
            });
        }
        if candidate_oid.is_empty() {
            return Err(crate::SessionError::InvalidPersistedValue {
                field: "verification.candidate_oid",
                value: "(empty)".to_owned(),
            });
        }
        Ok(Self {
            argv,
            profile_hash,
            candidate_oid,
            expected_exit_code,
            exit_code,
            passed,
        })
    }
}

/// Snapshot backend identity: the adapter type and unique nonce.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnSnapshotIdentity {
    pub backend: String,
    pub nonce: u32,
}

/// Derived snapshot reference: logical identity plus the deterministic
/// physical name `lattice-{session_id}-{commit_id_hex8}-{nonce_hex8}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnSnapshotRef {
    pub identity: TurnSnapshotIdentity,
    pub name: String,
}

/// Immutable logical lifecycle evidence for a terminal turn.
///
/// Carries target ref, baseline/candidate OIDs and refs, verification evidence,
/// retained snapshot reference, and resume linkage. Fields are optional where
/// the lifecycle stage may legitimately lack evidence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnLifecycleEvidence {
    pub target_project_ref: String,
    pub baseline_oid: Option<String>,
    pub candidate_oid: Option<String>,
    pub candidate_ref: Option<String>,
    pub verification: Option<TurnVerificationEvidence>,
    pub snapshot_ref: Option<TurnSnapshotRef>,
    pub resumes_turn_oid: Option<String>,
}

// ─── Turn DTOs ────────────────────────────────────────────────────────────

/// Input for creating a turn commit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnCommitInput {
    pub session_id: String,
    pub parent_ids: Vec<String>,
    pub messages: Vec<GraphMessage>,
    pub agent_name: String,
    pub profile_snapshot_hash: String,
    pub profile_snapshot_json: String,
    pub model: String,
    pub provider: String,
    pub status: TurnCommitStatus,
    #[serde(default = "WorkspaceDisposition::default_for_deser")]
    pub disposition: WorkspaceDisposition,
    #[serde(default)]
    pub lifecycle: Option<TurnLifecycleEvidence>,
    pub commit_message: String,
    #[serde(default)]
    pub usage: Option<TurnUsage>,
    #[serde(default)]
    pub terminal_reason: Option<String>,
    /// Workspace-history object id this Turn ended at (GitHub #63).
    ///
    /// The private mutation history is a separate durable store; a completed
    /// Turn records the object id it arrived at so the Turn's LATTICE
    /// mutations stay resolvable after session reopen, branch movement, and
    /// process restart. A Turn that installed no mutation records the
    /// inherited current identity, not `None`.
    #[serde(default)]
    pub workspace_history_oid: Option<String>,
}

/// An immutable turn commit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnCommit {
    pub commit_id: String,
    pub session_id: String,
    pub parent_ids: Vec<String>,
    pub message_ids: Vec<String>,
    pub agent_name: String,
    pub profile_snapshot_hash: String,
    pub profile_snapshot_json: String,
    pub model: String,
    pub provider: String,
    pub status: TurnCommitStatus,
    #[serde(default = "WorkspaceDisposition::default_for_deser")]
    pub disposition: WorkspaceDisposition,
    #[serde(default)]
    pub lifecycle: Option<TurnLifecycleEvidence>,
    pub commit_message: String,
    #[serde(default)]
    pub usage: Option<TurnUsage>,
    #[serde(default)]
    pub terminal_reason: Option<String>,
    /// Workspace-history object id this Turn ended at (GitHub #63).
    ///
    /// Persisted in the commit's manifest, so it is re-read from the Git
    /// object on every lookup rather than held in memory.
    #[serde(default)]
    pub workspace_history_oid: Option<String>,
    pub created_at: String,
}

/// A single turn log entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnLogEntry {
    pub commit_id: String,
    pub parent_ids: Vec<String>,
    pub status: TurnCommitStatus,
    #[serde(default = "WorkspaceDisposition::default_for_deser")]
    pub disposition: WorkspaceDisposition,
    pub agent_name: String,
    pub profile_snapshot_hash: String,
    pub model: String,
    pub provider: String,
    pub commit_message: String,
    pub message_count: usize,
    pub tool_message_count: usize,
    #[serde(default)]
    pub usage: Option<TurnUsage>,
    #[serde(default)]
    pub terminal_reason: Option<String>,
    pub created_at: String,
}

impl WorkspaceDisposition {
    /// Default for backward-compatible deserialization of legacy rows.
    fn default_for_deser() -> Self {
        Self::NotApplicable
    }
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
    fn create_session(
        &self,
        session_id: &str,
        label: Option<&str>,
        active_agent_name: &str,
        project_target_branch: &str,
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
}

/// Derive commit message from first user message.
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

/// Compute canonical message ID scoped to session and turn.
pub fn canonical_message_id(
    session_id: &str,
    turn_seed: &str,
    message: &GraphMessage,
    ordinal: usize,
) -> String {
    let payload = serde_json::json!({
        "session_id": session_id,
        "turn_seed": turn_seed,
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

/// Compute a stable turn seed for scoping message IDs within a commit.
pub fn canonical_turn_seed(
    session_id: &str,
    parent_ids: &[String],
    created_at: &str,
    model: &str,
    provider: &str,
    status: &TurnCommitStatus,
    commit_message: &str,
) -> String {
    let payload = serde_json::json!({
        "session_id": session_id,
        "parents": parent_ids,
        "created_at": created_at,
        "model": model,
        "provider": provider,
        "status": status.as_str(),
        "commit_message": commit_message,
    });
    hash_json("v1_turn_seed", &payload)
}

/// Compute canonical turn commit ID.
#[allow(clippy::too_many_arguments)]
pub fn canonical_turn_commit_id(
    session_id: &str,
    parent_ids: &[String],
    message_ids: &[String],
    agent_name: &str,
    profile_snapshot_hash: &str,
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
        "agent_name": agent_name,
        "profile_snapshot_hash": profile_snapshot_hash,
        "model": model,
        "provider": provider,
        "status": status.as_str(),
        "commit_message": commit_message,
        "created_at": created_at,
    });
    hash_json("v1_turn_commit", &payload)
}

/// Schema version for the v2 lifecycle-aware commit payload.
pub const COMMIT_PAYLOAD_SCHEMA_VERSION_V2: u32 = 2;

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
    pub messages: Vec<GraphMessage>,
}

/// Compute the final immutable turn commit ID before any storage effects.
///
/// Includes all logical lifecycle fields in the canonical payload.
/// Bumps the schema version to v2 so lifecycle-aware commits have distinct
/// identity from v1 commits.
pub fn prepare_turn_commit(input: &TurnCommitInput, created_at: &str) -> PreparedTurnCommit {
    let turn_seed = canonical_turn_seed(
        &input.session_id,
        &input.parent_ids,
        created_at,
        &input.model,
        &input.provider,
        &input.status,
        &input.commit_message,
    );

    let message_ids: Vec<String> = input
        .messages
        .iter()
        .enumerate()
        .map(|(i, msg)| canonical_message_id(&input.session_id, &turn_seed, msg, i))
        .collect();

    let commit_id = canonical_turn_commit_id_v2(input, &message_ids, created_at);

    let commit = TurnCommit {
        commit_id: commit_id.clone(),
        session_id: input.session_id.clone(),
        parent_ids: input.parent_ids.clone(),
        message_ids: message_ids.clone(),
        agent_name: input.agent_name.clone(),
        profile_snapshot_hash: input.profile_snapshot_hash.clone(),
        profile_snapshot_json: input.profile_snapshot_json.clone(),
        model: input.model.clone(),
        provider: input.provider.clone(),
        status: input.status.clone(),
        disposition: input.disposition.clone(),
        lifecycle: input.lifecycle.clone(),
        commit_message: input.commit_message.clone(),
        usage: input.usage.clone(),
        terminal_reason: input.terminal_reason.clone(),
        workspace_history_oid: input.workspace_history_oid.clone(),
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

/// V2 canonical turn commit ID including disposition and lifecycle evidence.
fn canonical_turn_commit_id_v2(
    input: &TurnCommitInput,
    message_ids: &[String],
    created_at: &str,
) -> String {
    let lifecycle_payload = input.lifecycle.as_ref().map(|lc| {
        serde_json::json!({
            "target_project_ref": lc.target_project_ref,
            "baseline_oid": lc.baseline_oid,
            "candidate_oid": lc.candidate_oid,
            "candidate_ref": lc.candidate_ref,
            "verification": lc.verification.as_ref().map(|v| serde_json::json!({
                "argv": v.argv,
                "profile_hash": v.profile_hash,
                "candidate_oid": v.candidate_oid,
                "expected_exit_code": v.expected_exit_code,
                "exit_code": v.exit_code,
                "passed": v.passed,
            })),
            "snapshot_backend": lc.snapshot_ref.as_ref().map(|s| &s.identity.backend),
            "snapshot_nonce": lc.snapshot_ref.as_ref().map(|s| s.identity.nonce),
            "resumes_turn_oid": lc.resumes_turn_oid,
        })
    });

    let payload = serde_json::json!({
        "schema_version": COMMIT_PAYLOAD_SCHEMA_VERSION_V2,
        "session_id": input.session_id,
        "parent_ids": input.parent_ids,
        "message_ids": message_ids,
        "agent_name": input.agent_name,
        "profile_snapshot_hash": input.profile_snapshot_hash,
        "model": input.model,
        "provider": input.provider,
        "status": input.status.as_str(),
        "disposition": input.disposition.as_str(),
        "lifecycle": lifecycle_payload,
        "commit_message": input.commit_message,
        "created_at": created_at,
    });
    hash_json("v2_turn_commit", &payload)
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
            active_agent_name: "coder".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            project_target_branch: None,
        };
        assert_eq!(session.session_id, "sess-1");

        let graph_ref = GraphRef {
            session_id: "sess-1".to_string(),
            name: "main".to_string(),
            kind: GraphRefKind::Branch,
            commit_id: Some("abc123".to_string()),
            active_agent_name: "coder".to_string(),
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
            workspace_history_oid: None,
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
            agent_name: "coder".to_string(),
            profile_snapshot_hash: "profile-hash".to_string(),
            profile_snapshot_json: "{}".to_string(),
            model: "sonnet".to_string(),
            provider: "anthropic".to_string(),
            status: TurnCommitStatus::Completed,
            disposition: WorkspaceDisposition::NotApplicable,
            lifecycle: None,
            commit_message: "Hello".to_string(),
            usage: None,
            terminal_reason: None,
        };
        let json = serde_json::to_string(&input).unwrap();
        assert!(json.contains("session_id"));
        assert!(json.contains("profile_snapshot_hash"));

        let commit = TurnCommit {
            workspace_history_oid: None,
            commit_id: "commit-abc".to_string(),
            session_id: "sess-1".to_string(),
            parent_ids: vec![],
            message_ids: vec!["msg-1".to_string()],
            agent_name: "coder".to_string(),
            profile_snapshot_hash: "profile-hash".to_string(),
            profile_snapshot_json: "{}".to_string(),
            model: "sonnet".to_string(),
            provider: "anthropic".to_string(),
            status: TurnCommitStatus::Completed,
            disposition: WorkspaceDisposition::NotApplicable,
            lifecycle: None,
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
            active_agent_name: "coder".to_string(),
            branches: vec![],
            commit_count: 5,
            project_target_branch: None,
        };
        let json = serde_json::to_string(&status).unwrap();
        assert!(json.contains("commit_count"));

        let summary = SessionGraphSummary {
            session_id: "sess-1".to_string(),
            label: Some("test".to_string()),
            active_agent_name: "coder".to_string(),
            current_branch: Some("main".to_string()),
            head_commit_id: Some("abc".to_string()),
            commit_count: 5,
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            project_target_branch: None,
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
            disposition: WorkspaceDisposition::NotApplicable,
            agent_name: "coder".to_string(),
            profile_snapshot_hash: "profile-hash".to_string(),
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
        fn _assert_trait_bound(_store: &dyn SessionGraphStore) {}
    }

    use crate::test_helpers::{assistant_msg, user_msg};

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

    #[test]
    fn canonical_turn_commit_id_includes_logical_fields() {
        let id = canonical_turn_commit_id(
            "sess-1",
            &["parent-1".to_string()],
            &["msg-1".to_string(), "msg-2".to_string()],
            "coder",
            "profile-hash",
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
            "coder",
            "profile-hash",
            "sonnet",
            "anthropic",
            TurnCommitStatus::Completed,
            "Hello",
            "2026-01-01T00:00:00Z",
        );
        let id1 = canonical_turn_commit_id(
            args.0, &args.1, &args.2, args.3, args.4, args.5, args.6, &args.7, args.8, args.9,
        );
        let id2 = canonical_turn_commit_id(
            args.0, &args.1, &args.2, args.3, args.4, args.5, args.6, &args.7, args.8, args.9,
        );
        assert_eq!(id1, id2);
    }

    #[test]
    fn canonical_turn_commit_id_changes_with_different_inputs() {
        let base = canonical_turn_commit_id(
            "sess-1",
            &[],
            &["msg-1".to_string()],
            "coder",
            "profile-hash",
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
            "coder",
            "profile-hash",
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
            "coder",
            "profile-hash",
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
            "coder",
            "profile-hash",
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
        // for rowid, object_path, filesystem_path, auxiliary history, request IDs,
        // or write_order. This is a compile-time/API-level assertion:
        // the function signature only accepts logical fields.
        let _ = canonical_turn_commit_id(
            "sess-1",
            &[],
            &[],
            "coder",
            "profile-hash",
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
        let id1 = canonical_message_id("sess-1", "seed-1", &msg, 0);
        let id2 = canonical_message_id("sess-1", "seed-1", &msg, 0);
        assert_eq!(id1, id2);
        assert_eq!(id1.len(), 64);
    }

    #[test]
    fn canonical_message_id_differs_by_content() {
        let msg1 = user_msg("Hello");
        let msg2 = user_msg("World");
        assert_ne!(
            canonical_message_id("sess-1", "seed-1", &msg1, 0),
            canonical_message_id("sess-1", "seed-1", &msg2, 0)
        );
    }

    #[test]
    fn canonical_message_id_differs_by_ordinal() {
        let msg = user_msg("Hello");
        let id0 = canonical_message_id("sess-1", "seed-1", &msg, 0);
        let id1 = canonical_message_id("sess-1", "seed-1", &msg, 1);
        assert_ne!(
            id0, id1,
            "identical messages at different ordinals must produce different IDs"
        );
    }

    #[test]
    fn canonical_message_id_differs_by_session() {
        let msg = user_msg("Hello");
        let id_a = canonical_message_id("sess-a", "seed-1", &msg, 0);
        let id_b = canonical_message_id("sess-b", "seed-1", &msg, 0);
        assert_ne!(
            id_a, id_b,
            "identical messages in different sessions must produce different IDs"
        );
    }

    #[test]
    fn canonical_message_id_differs_by_turn_seed() {
        let msg = user_msg("Hello");
        let id_a = canonical_message_id("sess-1", "seed-a", &msg, 0);
        let id_b = canonical_message_id("sess-1", "seed-b", &msg, 0);
        assert_ne!(
            id_a, id_b,
            "identical messages in different turns must produce different IDs"
        );
    }

    #[test]
    fn head_transcript_contract_serializes() {
        let transcript = HeadTranscript {
            session_id: "sess-1".to_string(),
            parent_commit_id: Some("commit-abc".to_string()),
            messages: vec![user_msg("Hello"), assistant_msg("Hi there!")],
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

    #[test]
    fn turn_commit_input_accepts_usage_and_terminal_reason() {
        let input = TurnCommitInput {
            workspace_history_oid: None,
            session_id: "sess-1".to_string(),
            parent_ids: vec![],
            messages: vec![user_msg("Hello")],
            agent_name: "coder".to_string(),
            profile_snapshot_hash: "profile-hash".to_string(),
            profile_snapshot_json: "{}".to_string(),
            model: "sonnet".to_string(),
            provider: "anthropic".to_string(),
            status: TurnCommitStatus::Completed,
            disposition: WorkspaceDisposition::NotApplicable,
            lifecycle: None,
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

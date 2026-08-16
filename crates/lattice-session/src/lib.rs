// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Session graph and branch pointer storage for LATTICE.
//!
//! This crate owns the conversation graph contract: sessions,
//! transcript messages, immutable turn commits, branch refs, HEAD state, and
//! the git-backed graph store. It intentionally exposes only the graph API
//! used by the API and CLI facade.
//!
//! Since GitHub #143 the store is built on the `lattice-store` kernel and
//! lives in the global `$XDG_STATE_HOME/lattice/sessions.git`; the
//! `ProjectStoreKey` type is re-exported for the composition layer.

use std::path::PathBuf;

pub use lattice_store::{Blocking, FlockGuard, LockMode, ProjectStoreKey, StateLayout, StoreError};
pub mod graph;
pub use graph::{
    COMMIT_PAYLOAD_SCHEMA_VERSION_V2, GraphDiff, GraphHead, GraphMessage, GraphRef, GraphSession,
    GraphToolCall, HeadTranscript, PreparedTurnCommit, ResetResult, SessionGraphStore,
    SessionGraphSummary, SessionStatus, TurnCommit, TurnCommitInput, TurnCommitStatus,
    TurnLifecycleEvidence, TurnLogEntry, TurnSnapshotIdentity, TurnSnapshotRef, TurnUsage,
    TurnVerificationEvidence, WorkspaceDisposition, canonical_turn_seed, derive_commit_message,
    prepare_turn_commit,
};

pub mod git_graph;
pub use git_graph::GitSessionGraphStore;

/// Durable PendingTurn journal contract and pure reconciliation decision
/// function (ADR 0005 §9/§14, GitHub #144).
pub mod pending;

#[cfg(any(test, feature = "test-utils"))]
pub mod test_utils;
#[cfg(feature = "test-utils")]
pub use test_utils::{init_empty_repo, init_fixture_repo};

pub(crate) const MAX_BRANCH_NAME_LEN: usize = 128;

pub type Result<T> = std::result::Result<T, SessionError>;

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("invalid branch name: format or length violation")]
    InvalidBranchName,
    /// A caller violated a session-store input contract (empty/illegal id,
    /// unserializable value). API misuse, not store damage.
    #[error("invalid session input: {0}")]
    InvalidInput(String),
    #[error("session '{0}' not found")]
    NotFound(String),
    #[error("transaction conflict: {0}")]
    Conflict(String),
    #[error("commit '{0}' not found")]
    MissingCommit(String),
    #[error("git error: {0}")]
    Git(String),
    #[error("invalid persisted value for field '{field}': '{value}'")]
    InvalidPersistedValue { field: &'static str, value: String },
    #[error("session '{0}' is busy: an operation holds the execution lock")]
    SessionBusy(String),
    /// A PendingTurn journal object or chain violates the stage contract:
    /// broken payload invariants, identity mismatch, illegal predecessor
    /// shape, or facts inconsistent with the durable plan. Typed damage,
    /// never repaired in place (ADR 0005 §9).
    #[error("pending-turn journal corruption: {detail}")]
    JournalCorruption {
        /// What invariant was violated.
        detail: String,
    },
    /// A locked operation received a guard that does not prove exclusive
    /// ownership of this session's exact execution-lock path.
    #[error(
        "invalid lock capability for session '{session_id}': expected {expected_mode:?} at '{}', got {actual_mode:?} at '{}'",
        expected_path.display(),
        actual_path.display()
    )]
    InvalidLockCapability {
        /// Session whose locked operation was requested.
        session_id: String,
        /// Exact path the store requires.
        expected_path: PathBuf,
        /// Required lock strength.
        expected_mode: lattice_store::LockMode,
        /// Path carried by the supplied guard.
        actual_path: PathBuf,
        /// Lock strength carried by the supplied guard.
        actual_mode: lattice_store::LockMode,
    },
    #[error("store kernel error: {0}")]
    Store(#[from] lattice_store::StoreError),
}

pub(crate) fn validate_branch_name(name: &str) -> Result<()> {
    let valid = !name.is_empty()
        && name.len() <= MAX_BRANCH_NAME_LEN
        && !name.starts_with('-')
        && !name.starts_with('/')
        && !name.ends_with('/')
        && !name.contains("..")
        && !name.contains("//")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '/' | '.'));
    if valid {
        Ok(())
    } else {
        Err(SessionError::InvalidBranchName)
    }
}

/// Shared test helpers for constructing `GraphMessage` values.
///
/// These are used by internal tests and downstream crate test suites.
/// Not intended for production use.
#[doc(hidden)]
pub mod test_helpers {
    use crate::graph::{GraphMessage, GraphToolCall};

    pub fn user_msg(text: &str) -> GraphMessage {
        GraphMessage {
            role: "user".to_string(),
            content: text.to_string(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    pub fn assistant_msg(text: &str) -> GraphMessage {
        GraphMessage {
            role: "assistant".to_string(),
            content: text.to_string(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    pub fn assistant_msg_with_tool_calls(
        text: &str,
        tool_calls: Vec<GraphToolCall>,
    ) -> GraphMessage {
        GraphMessage {
            role: "assistant".to_string(),
            content: text.to_string(),
            reasoning_content: None,
            tool_calls: Some(tool_calls),
            tool_call_id: None,
            name: None,
        }
    }

    pub fn tool_msg(text: &str, tool_call_id: &str, name: &str) -> GraphMessage {
        GraphMessage {
            role: "tool".to_string(),
            content: text.to_string(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: Some(tool_call_id.to_string()),
            name: Some(name.to_string()),
        }
    }
}

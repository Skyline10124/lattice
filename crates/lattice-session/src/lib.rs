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

pub use lancet_protocol::TurnId;
pub use lattice_store::{Blocking, FlockGuard, LockMode, ProjectStoreKey, StateLayout, StoreError};
mod graph;
pub use graph::{
    COMMIT_PAYLOAD_SCHEMA_VERSION, GraphDiff, GraphHead, GraphRef, GraphSession, HeadTranscript,
    ResetResult, SessionGraphStore, SessionGraphSummary, SessionStatus, TurnCommit, TurnLogEntry,
    TurnUsage, derive_commit_message,
};
#[cfg(any(test, feature = "test-utils"))]
pub use graph::{PreparedTurnCommit, TurnCommitInput, prepare_turn_commit};

/// 全局事务会话库；与旧 Git 图实现并存，尚未接线。
pub mod store;

pub mod git_graph;
pub use git_graph::{
    CommittedTurnObservation, GitSessionGraphStore, PendingObservation, PendingTurnTip,
    SessionObservation, SessionReadView, StoredTurn, TranscriptPage, TurnObservation,
};

/// Durable PendingTurn journal contract and pure reconciliation decision
/// function (ADR 0005 §9/§14, GitHub #144).
pub mod pending;
pub use pending::{ClosureOrigin, TurnFinishReason, TurnOutcome, WorkspaceHistoryCheckpoint};
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

/// Shared typed transcript helpers for tests.
#[doc(hidden)]
pub mod test_helpers {
    use lattice_conversation::{
        BoundToolCall, MessagePart, MessageRole, MessageSemantic, NativeMessage,
    };

    fn text_message(role: MessageRole, text: &str) -> NativeMessage<BoundToolCall> {
        NativeMessage::new(
            role,
            None,
            vec![MessagePart::semantic_only(MessageSemantic::Text {
                text: text.to_string(),
            })],
        )
    }

    pub fn user_msg(text: &str) -> NativeMessage<BoundToolCall> {
        text_message(MessageRole::User, text)
    }

    pub fn assistant_msg(text: &str) -> NativeMessage<BoundToolCall> {
        text_message(MessageRole::Assistant, text)
    }

    pub fn text(message: &NativeMessage<BoundToolCall>) -> Option<&str> {
        message.parts.iter().find_map(|part| match &part.semantic {
            MessageSemantic::Text { text } => Some(text.as_str()),
            _ => None,
        })
    }
}

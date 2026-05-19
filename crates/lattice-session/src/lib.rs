//! Session graph and branch pointer storage for LATTICE.
//!
//! This crate owns the final v1.1 conversation graph contract: sessions,
//! transcript messages, immutable turn commits, branch refs, HEAD state, and
//! the SQLite graph store. It intentionally exposes only the graph API used by
//! the API and CLI facade.

use std::fs;
use std::path::{Path, PathBuf};

pub mod graph;
pub use graph::{
    canonical_turn_seed, derive_commit_message, GraphDiff, GraphHead, GraphMessage, GraphRef,
    GraphSession, GraphToolCall, HeadTranscript, ResetResult, SessionGraphStore,
    SessionGraphSummary, SessionStatus, TurnCommit, TurnCommitInput, TurnCommitStatus,
    TurnLogEntry, TurnUsage,
};

pub mod sqlite_graph;
pub use sqlite_graph::SqliteSessionGraphStore;

pub(crate) const MAX_BRANCH_NAME_LEN: usize = 128;

pub type Result<T> = std::result::Result<T, SessionError>;

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("io error while {operation} '{path}': {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("database error in '{path}': {source}")]
    Database {
        path: PathBuf,
        #[source]
        source: rusqlite::Error,
    },
    #[error("invalid branch name: format or length violation")]
    InvalidBranchName,
    #[error("session '{0}' not found")]
    NotFound(String),
    #[error("transaction conflict: {0}")]
    Conflict(String),
    #[error("commit '{0}' not found")]
    MissingCommit(String),
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

pub(crate) fn create_dir_all(path: impl AsRef<Path>) -> Result<()> {
    let path = path.as_ref();
    fs::create_dir_all(path).map_err(|source| SessionError::Io {
        operation: "creating directory",
        path: path.to_path_buf(),
        source,
    })
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

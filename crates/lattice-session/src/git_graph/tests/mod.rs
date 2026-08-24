// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Internal Git graph store tests, split by concern.

mod commit_turn;
mod head_transcript;
mod pending_crash;
mod pending_fixtures;
mod pending_journal;
mod session_meta;
mod state_and_locks;
mod strict_open_scaling;

use std::path::Path;

use tempfile::TempDir;

use crate::graph::{SessionGraphStore, TurnCommitInput, derive_commit_message};
use crate::pending::{ClosureOrigin, TurnFinishReason, TurnOutcome, WorkspaceHistoryCheckpoint};
use crate::test_helpers::{assistant_msg, user_msg};
use lattice_conversation::{BoundToolCall, NativeMessage};
/// A fixture directory that lives only as long as the test holds it.
///
/// Earlier revisions built a host-global path
/// (`$TMPDIR/lattice-session-git-graph-tests/<name>`) and `remove_dir_all`'d it
/// on the way in, so two test binaries running concurrently on one host — two
/// `CARGO_TARGET_DIR`s, or two git worktrees — deleted each other's fixtures
/// mid-run (#106). `TempDir` gives each run its own path and cleans up on drop.
pub(crate) struct Fixture {
    dir: TempDir,
}

impl Fixture {
    pub(crate) fn path(&self) -> &Path {
        self.dir.path()
    }
}

impl AsRef<Path> for Fixture {
    fn as_ref(&self) -> &Path {
        self.path()
    }
}

/// Create a uniquely named fixture directory. `name` survives only as a
/// human-readable prefix so failures stay identifiable.
pub(crate) fn temp_dir(name: &str) -> Fixture {
    let dir =
        TempDir::with_prefix(format!("lattice-session-{name}-")).expect("create fixture temp dir");
    Fixture { dir }
}

pub(crate) fn commit_input(
    session_id: &str,
    parent_ids: Vec<String>,
    messages: Vec<NativeMessage<BoundToolCall>>,
    _terminal: (),
) -> TurnCommitInput {
    TurnCommitInput {
        session_id: session_id.to_string(),
        turn_id: lancet_protocol::TurnId::parse("11111111-1111-4111-8111-111111111111")
            .expect("valid test TurnId"),
        parent_ids,
        messages: messages.clone(),
        agent_name: "coder".to_string(),
        profile_snapshot_hash: "test-profile".to_string(),
        profile_snapshot_json: "{}".to_string(),
        model: "test-model".to_string(),
        provider: "test-provider".to_string(),
        outcome: TurnOutcome::Completed {
            finish_reason: TurnFinishReason::Stop,
        },
        origin: ClosureOrigin::Live,
        commit_message: derive_commit_message(&messages),
        usage: None,
        workspace_history_checkpoint: WorkspaceHistoryCheckpoint::Empty,
    }
}
/// A fixed test project key: session-store tests have no real workspace, so
/// they bind sessions to one well-known storage address.
pub(crate) fn test_project_key() -> lattice_store::ProjectStoreKey {
    lattice_store::ProjectStoreKey::from_canonical_path_bytes(b"/lattice-session-tests/project")
}

pub(crate) fn setup_session_with_commits(
    store: &crate::git_graph::GitSessionGraphStore,
    session_id: &str,
    n: usize,
    _label: Option<&str>,
) -> Vec<String> {
    store
        .create_session(session_id, None, "coder", "main", &test_project_key())
        .expect("create session");
    let mut commit_ids = Vec::new();
    for i in 0..n {
        let messages = vec![
            user_msg(&format!("Turn {i}")),
            assistant_msg(&format!("Response {i}")),
        ];
        let parent_ids = commit_ids.last().cloned().into_iter().collect::<Vec<_>>();
        let input = commit_input(session_id, parent_ids, messages, ());
        let commit = store.test_commit_turn(input).expect("commit turn");
        commit_ids.push(commit.commit_id);
    }
    commit_ids
}

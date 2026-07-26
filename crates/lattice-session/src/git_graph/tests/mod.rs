// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Internal Git graph store tests, split by concern.

mod commit_turn;
mod head_transcript;
mod no_remote;
mod pointer_ops;
mod session_meta;

use std::env;
use std::fs;
use std::path::PathBuf;

use crate::graph::{
    GraphMessage, SessionGraphStore, TurnCommitInput, TurnCommitStatus, WorkspaceDisposition,
    derive_commit_message,
};
use crate::test_helpers::{assistant_msg, user_msg};
use crate::test_utils::init_empty_repo;

pub(crate) fn temp_dir(name: &str) -> PathBuf {
    let dir = env::temp_dir()
        .join("lattice-session-git-graph-tests")
        .join(name);
    let _ = fs::remove_dir_all(&dir);
    dir
}

pub(crate) fn setup_git_repo(dir: &std::path::Path) -> gix::Repository {
    init_empty_repo(dir)
}

pub(crate) fn commit_input(
    session_id: &str,
    parent_ids: Vec<String>,
    messages: Vec<GraphMessage>,
    status: TurnCommitStatus,
) -> TurnCommitInput {
    TurnCommitInput {
        workspace_history_oid: None,
        session_id: session_id.to_string(),
        parent_ids,
        messages: messages.clone(),
        agent_name: "coder".to_string(),
        profile_snapshot_hash: "test-profile".to_string(),
        profile_snapshot_json: "{}".to_string(),
        model: "test-model".to_string(),
        provider: "test-provider".to_string(),
        status,
        disposition: WorkspaceDisposition::NotApplicable,
        lifecycle: None,
        commit_message: derive_commit_message(&messages),
        usage: None,
        terminal_reason: None,
    }
}

pub(crate) fn setup_session_with_commits(
    store: &crate::git_graph::GitSessionGraphStore,
    session_id: &str,
    n: usize,
    _label: Option<&str>,
) -> Vec<String> {
    store
        .create_session(session_id, None, "coder", "main")
        .expect("create session");
    let mut commit_ids = Vec::new();
    for i in 0..n {
        let messages = vec![
            user_msg(&format!("Turn {i}")),
            assistant_msg(&format!("Response {i}")),
        ];
        let parent_ids = commit_ids.last().cloned().into_iter().collect::<Vec<_>>();
        let input = commit_input(
            session_id,
            parent_ids,
            messages,
            TurnCommitStatus::Completed,
        );
        let commit = store.commit_turn(input).expect("commit turn");
        commit_ids.push(commit.commit_id);
    }
    commit_ids
}

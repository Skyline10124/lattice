// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Runtime acceptance for recording the observed before-state of an edit.
//! Turn identities, refusal history and workspace/Git isolation are exercised
//! by the external harness in `direct_workspace_history`.

mod support;

use std::path::Path;

use lancet_api::{LancetRuntimeConfig, WorkspaceHistoryBacking};
use lancet_authority::policy::default::{PolicyConfig, WorkspacePolicy};
use lancet_protocol::CepErrorCode;
use support::ReservedRuntime;
use tempfile::TempDir;

/// The five embedded Component Tools of the default distribution.
const COMPONENT_TOOLS: [&str; 5] = [
    "observe.workspace",
    "mutate.create",
    "mutate.edit",
    "fetch.web",
    "session.todo_write",
];

// ─── Runtime-layer harness ──────────────────────────────────────────────────

/// A runtime rooted at a temp Local Workspace with its private history in a
/// separate temp directory, so "outside the Local Workspace" is structural.
struct HistoryRuntime {
    runtime: ReservedRuntime,
    root: TempDir,
    /// Held so the private history repository outlives the runtime.
    _state: TempDir,
}

impl HistoryRuntime {
    fn build() -> Self {
        let root = TempDir::new().expect("workspace root");
        let state = TempDir::new().expect("state root");
        let runtime = build_runtime(root.path(), &history_backing(state.path(), root.path()));
        Self {
            runtime,
            root,
            _state: state,
        }
    }

    fn path(&self) -> &Path {
        self.root.path()
    }

    fn current(&self) -> Option<String> {
        self.runtime
            .workspace_history()
            .current()
            .expect("read history current")
    }
}

/// 每个项目独立历史；新 Session 接受路径负责首次创建，已有 Session 严格打开。
fn history_backing(state_root: &Path, workspace_root: &Path) -> WorkspaceHistoryBacking {
    let backing = lancet_api::workspace_history_backing_in(state_root, workspace_root)
        .expect("derive history backing");
    lancet_api::WorkspaceHistory::create(backing.clone()).expect("create history store");
    backing
}

fn build_runtime(root: &Path, history: &WorkspaceHistoryBacking) -> ReservedRuntime {
    let mut policy = PolicyConfig::defaults_for_project_root(root.to_path_buf());
    policy.workspace = WorkspacePolicy {
        denied_write_patterns: Vec::new(),
        ..Default::default()
    };
    let enabled = COMPONENT_TOOLS
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    ReservedRuntime::build(LancetRuntimeConfig {
        local_workspace_root: root.to_path_buf(),
        tls_roots_der: Vec::new(),
        enabled_tools: enabled,
        policy,
        // `enabled_tools` is the stage-1 capability grant, and Cell admission is
        // fail-closed: an omitted name here is a Tool the runtime exposes and
        // then refuses.
        capability_admission: lancet_protocol::evidence::CapabilityAdmissionPolicy {
            enabled_tools: COMPONENT_TOOLS.iter().map(|n| (*n).to_string()).collect(),
            denied_capabilities: Default::default(),
            max_boundary: lancet_protocol::cell::Trust::Ex,
        },
        workspace_history: history.clone(),
    })
    .expect("runtime with private history builds")
}

fn content_hash(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}

async fn create_file(
    runtime: &ReservedRuntime,
    path: &str,
    content: &str,
) -> Result<(), CepErrorCode> {
    runtime
        .client()
        .call_raw(
            "mutate.create",
            serde_json::json!({"path": path, "content": content, "create_parents": true}),
        )
        .await
        .map(|_| ())
        .map_err(|err| err.code)
}

async fn replace_text(
    runtime: &ReservedRuntime,
    path: &str,
    expected_hash: &str,
    old: &str,
    new: &str,
) -> Result<(), CepErrorCode> {
    runtime
        .client()
        .call_raw("mutate.edit", serde_json::json!({"path": path, "expected_hash": expected_hash, "edits": [{"op": "replace_text", "old": old, "new": new}]}))
        .await
        .map(|_| ())
        .map_err(|err| err.code)
}

/// AC2: the first `mutate.edit` of a file LATTICE never created records that
/// file's real before state, and after an external writer changes a file the
/// next mutation's before state is what the installer observed — both driven
/// through the production handler rather than the history API directly.
#[tokio::test]
async fn first_edit_and_external_drift_record_the_observed_before_state() {
    let env = HistoryRuntime::build();
    let history = env.runtime.workspace_history();
    let parent_of = |tip: &str| {
        let repo = gix::open_opts(history.repo_path().unwrap(), gix::open::Options::isolated())
            .expect("open private history");
        let parent = repo
            .find_commit(gix::ObjectId::from_hex(tip.as_bytes()).unwrap())
            .unwrap()
            .parent_ids()
            .next()
            .expect("a before state was recorded")
            .detach();
        history.resolve(&parent.to_string()).unwrap().unwrap()
    };

    // A file that exists before LATTICE ever touches it.
    std::fs::write(env.path().join("pre.txt"), "original\n").expect("seed");
    replace_text(
        &env.runtime,
        "pre.txt",
        &content_hash(b"original\n"),
        "original",
        "rewritten",
    )
    .await
    .expect("first edit succeeds");
    let first = env.current().expect("recorded");
    assert_eq!(
        history.resolve(&first).unwrap().unwrap().files["pre.txt"],
        b"rewritten\n"
    );
    assert_eq!(
        parent_of(&first).files["pre.txt"],
        b"original\n",
        "the parent tree must be the pre-mutation content"
    );

    create_file(&env.runtime, "drift.txt", "v1\n")
        .await
        .expect("create succeeds");
    // The user edits the file outside LATTICE between the two mutations.
    std::fs::write(env.path().join("drift.txt"), "v2-external\n").expect("external write");

    replace_text(
        &env.runtime,
        "drift.txt",
        &content_hash(b"v2-external\n"),
        "v2-external",
        "v3",
    )
    .await
    .expect("edit succeeds");

    let tip = env.current().expect("recorded");
    let before = parent_of(&tip);
    assert_eq!(
        before.files.get("drift.txt").unwrap().as_slice(),
        b"v2-external\n",
        "the recorded before state must be what the installer observed, not LATTICE's stale copy"
    );
    let after = history.resolve(&tip).unwrap().unwrap();
    assert_eq!(after.files.get("drift.txt").unwrap().as_slice(), b"v3\n");
}

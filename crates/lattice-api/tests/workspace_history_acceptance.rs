// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! GitHub #63 acceptance tests: successful Atomic Local Mutations are
//! persisted to a private `gix` history.
//!
//! Two layers are exercised:
//!
//! - **Runtime layer** — a real `lancet_api::LancetRuntime` rooted at a temp
//!   Local Workspace with an explicit private history repository. These tests
//!   pin what is and is not recorded, the ordering and parent relationships,
//!   object reachability, and the fact that recording touches neither the Local
//!   Workspace nor the user's project `.git`.
//! - **Turn 层**：真实 run_turn 与 fake LLM；验证历史的 TurnId/InvocationId
//!   关联、无变更不移动历史、失败保留已安装效果，以及会话 rewind/重开不改变文件历史。

mod support;

use std::path::Path;
use std::sync::Arc;

use lancet_api::{LancetRuntimeConfig, WorkspaceHistory, WorkspaceHistoryBacking};
use lancet_authority::policy::default::{PolicyConfig, WorkspacePolicy};
use lancet_protocol::CepErrorCode;
use lattice_agent::LlmClient;
use lattice_agent::testing::FakeStreamingLlmClient;
use lattice_api::{RunTurnRequest, SessionApi};
use lattice_llm::{LlmEvent, TokenUsage};
use lattice_profile::{DiscoveryPaths, LoadedAgentProfile, ProfileSource};
use lattice_session::{
    StateLayout,
    store::{
        OperatorInvocationState, PublicTurnOutcome, SessionStore, StepDetailState, TurnDetail,
    },
};
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

/// Recursive snapshot of a directory: relative path -> bytes.
fn tree_snapshot(root: &Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    fn walk(base: &Path, dir: &Path, out: &mut std::collections::BTreeMap<String, Vec<u8>>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let meta = match std::fs::symlink_metadata(&path) {
                Ok(meta) => meta,
                Err(_) => continue,
            };
            if meta.is_dir() {
                walk(base, &path, out);
            } else {
                let rel = path
                    .strip_prefix(base)
                    .expect("under base")
                    .to_string_lossy()
                    .into_owned();
                out.insert(rel, std::fs::read(&path).unwrap_or_default());
            }
        }
    }
    let mut out = std::collections::BTreeMap::new();
    walk(root, root, &mut out);
    out
}

// ─── Runtime layer ──────────────────────────────────────────────────────────

/// Walk parent links from `tip` using the private repository directly.
fn commit_chain(history: &WorkspaceHistory, tip: &str) -> Vec<String> {
    let repo = gix::open_opts(
        history.repo_path().expect("enabled"),
        gix::open::Options::isolated(),
    )
    .expect("open private history");
    let mut out = Vec::new();
    let mut next = Some(gix::ObjectId::from_hex(tip.as_bytes()).expect("oid"));
    while let Some(oid) = next {
        let commit = repo.find_commit(oid).expect("commit");
        let message = commit.message_raw_sloppy().to_string();
        let parent = commit.parent_ids().next().map(|id| id.detach());
        // Baseline ancestors are not mutation commits; skip them so the chain
        // is exactly the recorded mutations.
        if !message.contains("lattice-history-kind: baseline") {
            out.push(oid.to_string());
        }
        next = parent;
    }
    out
}

/// AC4: precondition failures, ReplanRequired, and policy denial record no
/// mutation commit and never move the current history ref.
#[tokio::test]
async fn failures_before_installation_record_nothing_and_do_not_move_current() {
    let env = HistoryRuntime::build();

    // Nothing recorded yet.
    assert_eq!(env.current(), None);

    // 1. Stale expected hash -> ReplanRequired, before any install.
    std::fs::write(env.path().join("target.txt"), "real\n").expect("seed");
    let stale = replace_text(
        &env.runtime,
        "target.txt",
        &content_hash(b"not-the-current-content\n"),
        "real",
        "changed",
    )
    .await
    .expect_err("stale hash must fail");
    assert_eq!(stale, CepErrorCode::ReplanRequired);
    assert_eq!(env.current(), None, "a failed precondition records nothing");

    // 2. Create over an existing target -> ReplanRequired.
    let exists = create_file(&env.runtime, "target.txt", "other\n")
        .await
        .expect_err("no-replace create must fail");
    assert_eq!(exists, CepErrorCode::ReplanRequired);
    assert_eq!(env.current(), None);

    // 3. Guard denial on a write-denied path.
    let denied_root = TempDir::new().unwrap();
    let denied_state = TempDir::new().unwrap();
    let mut policy = PolicyConfig::defaults_for_project_root(denied_root.path().to_path_buf());
    policy.workspace = WorkspacePolicy {
        denied_write_patterns: vec!["README.md".into()],
        ..Default::default()
    };
    let denied_runtime = ReservedRuntime::build(LancetRuntimeConfig {
        local_workspace_root: denied_root.path().to_path_buf(),
        tls_roots_der: Vec::new(),
        enabled_tools: COMPONENT_TOOLS.iter().map(|n| (*n).to_string()).collect(),
        policy,
        // Admit mutate.create so Cell admission is not the denier — the write
        // Guard is (#31).
        capability_admission: lancet_protocol::evidence::CapabilityAdmissionPolicy {
            enabled_tools: COMPONENT_TOOLS.iter().map(|n| (*n).to_string()).collect(),
            ..Default::default()
        },
        workspace_history: history_backing(denied_state.path(), denied_root.path()),
    })
    .expect("denied runtime builds");
    let denial = create_file(&denied_runtime, "README.md", "nope\n")
        .await
        .expect_err("guard must deny");
    // The host refusal outranks the guest's string retelling, so the Guard
    // denial keeps its structured Cell-era code through Component dispatch.
    assert_eq!(denial, CepErrorCode::CapabilityViolation);
    assert_eq!(
        denied_runtime.workspace_history().current().unwrap(),
        None,
        "a policy denial records nothing"
    );

    // 4. Capability denial for the mutate Cell is enforced before installation.
    //    pre-install refusal here is `denied_capabilities`. Subject: a denied
    //    mutation installs nothing and records nothing.
    let effect_root = TempDir::new().unwrap();
    let effect_state = TempDir::new().unwrap();
    let mut policy = PolicyConfig::defaults_for_project_root(effect_root.path().to_path_buf());
    policy.workspace = WorkspacePolicy {
        denied_write_patterns: Vec::new(),
        ..Default::default()
    };
    let effect_runtime = ReservedRuntime::build(LancetRuntimeConfig {
        local_workspace_root: effect_root.path().to_path_buf(),
        tls_roots_der: Vec::new(),
        enabled_tools: COMPONENT_TOOLS.iter().map(|n| (*n).to_string()).collect(),
        policy,
        capability_admission: lancet_protocol::evidence::CapabilityAdmissionPolicy {
            enabled_tools: COMPONENT_TOOLS.iter().map(|n| (*n).to_string()).collect(),
            denied_capabilities: std::iter::once("workspace.mutation".to_string()).collect(),
            max_boundary: lancet_protocol::cell::Trust::In,
        },
        workspace_history: history_backing(effect_state.path(), effect_root.path()),
    })
    .expect("effect-deny runtime builds");
    create_file(&effect_runtime, "denied.txt", "nope\n")
        .await
        .expect_err("denied effect kind must fail");
    assert!(
        !effect_root.path().join("denied.txt").exists(),
        "an effect-denied mutation must not be installed"
    );
    assert_eq!(
        effect_runtime.workspace_history().current().unwrap(),
        None,
        "an effect denial records nothing"
    );

    // 5. A subsequent success is the first thing that moves the ref.
    create_file(&env.runtime, "fresh.txt", "ok\n")
        .await
        .expect("create succeeds");
    let recorded = env.current().expect("success recorded");

    // 6. Once installed, a mutation stays recorded: later failures on the same
    //    path neither rewind the ref nor append a compensation commit.
    create_file(&env.runtime, "fresh.txt", "again\n")
        .await
        .expect_err("no-replace create must fail");
    replace_text(
        &env.runtime,
        "fresh.txt",
        &content_hash(b"wrong\n"),
        "ok",
        "x",
    )
    .await
    .expect_err("stale hash must fail");
    assert_eq!(env.current().as_deref(), Some(recorded.as_str()));
    assert_eq!(
        env.runtime
            .workspace_history()
            .resolve(&recorded)
            .unwrap()
            .unwrap()
            .files["fresh.txt"],
        b"ok\n"
    );
    assert_eq!(
        commit_chain(env.runtime.workspace_history(), &recorded).len(),
        1,
        "no compensation commit may be appended"
    );
}

/// AC9/AC10: recording changes nothing in the Local Workspace beyond the
/// mutation itself, and never writes the user's project `.git`.
#[tokio::test]
async fn recording_leaves_the_local_workspace_and_project_git_untouched() {
    let env = HistoryRuntime::build();
    support::init_git_repo(env.path());

    let git_before = tree_snapshot(&env.path().join(".git"));
    let workspace_before = tree_snapshot(env.path());
    assert!(!git_before.is_empty(), "fixture must have a project .git");

    create_file(&env.runtime, "src/new.txt", "created\n")
        .await
        .expect("create succeeds");
    replace_text(
        &env.runtime,
        "src/new.txt",
        &content_hash(b"created\n"),
        "created",
        "edited",
    )
    .await
    .expect("edit succeeds");

    // Recording really happened — otherwise this test would pass with the
    // whole feature switched off and prove nothing about recording.
    let tip = env.current().expect("both mutations were recorded");
    let recorded = env
        .runtime
        .workspace_history()
        .resolve(&tip)
        .unwrap()
        .unwrap();
    assert_eq!(
        recorded.files.get("src/new.txt").unwrap().as_slice(),
        b"edited\n"
    );

    let git_after = tree_snapshot(&env.path().join(".git"));
    assert_eq!(
        git_before, git_after,
        "history recording must not write the user's project .git"
    );

    // The only Local Workspace difference is the mutated file itself: history
    // recording adds no sidecar, journal, or private directory.
    let workspace_after = tree_snapshot(env.path());
    let added: Vec<&String> = workspace_after
        .keys()
        .filter(|k| !workspace_before.contains_key(*k))
        .collect();
    assert_eq!(
        added,
        vec![&"src/new.txt".to_string()],
        "only the mutated path may appear in the Local Workspace"
    );
    assert_eq!(
        workspace_after.get("src/new.txt").unwrap().as_slice(),
        b"edited\n"
    );
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

// ─── Turn-layer harness ─────────────────────────────────────────────────────

struct TurnWorkspace {
    _dir: TempDir,
    _global_agents: TempDir,
    root: std::path::PathBuf,
    loaded: LoadedAgentProfile,
}

fn setup_turn_workspace(enabled_tools: &[&str]) -> TurnWorkspace {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/lib.rs"), "pub fn answer() -> i32 { 1 }").unwrap();
    support::init_git_repo(&root);

    let profile_dir = root.join(".lattice/agents/default");
    std::fs::create_dir_all(&profile_dir).unwrap();
    let enabled: Vec<String> = enabled_tools.iter().map(|s| s.to_string()).collect();
    std::fs::write(
        profile_dir.join("agent.toml"),
        format!(
            r#"schema_version = 1
[agent]
name = "default"
model = "sonnet"
[prompt]
system = "You are a test agent."
[tools]
enabled = {enabled:?}

[lancet.workspace]
denied_write_patterns = [".git/**"]
"#
        ),
    )
    .unwrap();

    let global_agents = TempDir::new().unwrap();
    let loaded = lattice_profile::resolve_agent_profile(
        ProfileSource::Default,
        &DiscoveryPaths {
            project_root: root.clone(),
            global_agents_dir: global_agents.path().to_path_buf(),
        },
    )
    .expect("profile loads");

    TurnWorkspace {
        _dir: dir,
        _global_agents: global_agents,
        root,
        loaded,
    }
}

fn usage() -> TokenUsage {
    TokenUsage {
        prompt_tokens: 1,
        completion_tokens: 1,
        total_tokens: 2,
    }
}

fn tool_call(id: &str, name: &str, args: serde_json::Value) -> Vec<LlmEvent> {
    vec![
        LlmEvent::ToolCallStart {
            index: 0,
            provider_call_id: Some(id.into()),
            name: name.into(),
        },
        LlmEvent::ToolCallDelta {
            index: 0,
            arguments_delta: args.to_string(),
        },
        LlmEvent::ToolCallEnd { index: 0 },
        LlmEvent::Done {
            finish_reason: "tool_calls".into(),
            usage: Some(usage()),
        },
    ]
}

fn final_text(text: &str) -> Vec<LlmEvent> {
    vec![
        LlmEvent::Token {
            content: text.into(),
        },
        LlmEvent::Done {
            finish_reason: "stop".into(),
            usage: Some(usage()),
        },
    ]
}

fn turn_request(
    ws: &TurnWorkspace,
    input: &str,
    session_id: Option<lancet_protocol::SessionId>,
    llm: Arc<dyn LlmClient>,
) -> RunTurnRequest {
    RunTurnRequest {
        input: input.to_string(),
        session_id,
        agent: Some("default".into()),
        loaded_profile: Some(ws.loaded.clone()),
        cancel_flag: None,
        test_llm: Some((llm, lattice_llm::ProviderKind::OpenAiCompatible)),
        test_project_root: Some(ws.root.to_string_lossy().to_string()),
    }
}

fn session_store(root: &Path) -> (StateLayout, Arc<SessionStore>) {
    let layout = StateLayout::in_state_root(root);
    let store = Arc::new(SessionStore::create(&layout).unwrap());
    (layout, store)
}

fn assert_history_bindings(history: &WorkspaceHistory, tip: &str, detail: &TurnDetail) {
    let repo =
        gix::open_opts(history.repo_path().unwrap(), gix::open::Options::isolated()).unwrap();
    let calls: Vec<_> = detail
        .steps
        .iter()
        .flat_map(|step| match &step.state {
            StepDetailState::Closed { slots, .. } => slots.iter(),
            _ => panic!("closed Turn"),
        })
        .collect();
    let mut chain = commit_chain(history, tip);
    chain.reverse();
    assert_eq!(chain.len(), calls.len());
    for (oid, call) in chain.iter().zip(calls) {
        let commit = repo
            .find_commit(gix::ObjectId::from_hex(oid.as_bytes()).unwrap())
            .unwrap();
        let message = commit.message_raw_sloppy().to_string();
        assert!(
            message.contains(&detail.turn_id.to_string()),
            "history must carry actual TurnId: {message}"
        );
        assert!(
            message.contains(&call.invocation_id.to_string()),
            "history must carry actual InvocationId: {message}"
        );
    }
}

fn history_for(state_root: &Path, project_root: &Path) -> WorkspaceHistory {
    let backing = lancet_api::workspace_history_backing_in(state_root, project_root)
        .expect("resolve history backing");
    WorkspaceHistory::open(backing).expect("open history")
}

// ─── Turn layer ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn turn_whose_mutation_fails_before_install_records_no_history_identity() {
    let ws = setup_turn_workspace(&["mutate.edit"]);
    let state = TempDir::new().unwrap();
    let (layout, store) = session_store(state.path());
    let llm = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(tool_call(
                "c1",
                "mutate.edit",
                serde_json::json!({
                    "path":"src/lib.rs", "expected_hash":content_hash(b"not the current content"),
                    "edits":[{"op":"replace_text", "old":"1", "new":"2"}]
                }),
            ))
            .push_events(final_text("could not edit"))
            .build(),
    );
    let result = lattice_api::run_turn(
        SessionApi::from_store(store.clone()),
        layout,
        turn_request(&ws, "try an impossible edit", None, llm),
    )
    .await
    .unwrap();
    let read = store.begin_read().unwrap();
    let detail = read
        .turn_detail(result.turn.session_id, result.turn.turn_id)
        .unwrap();
    assert!(
        matches!(&detail.steps[0].state, StepDetailState::Closed { slots, .. }
        if slots.len() == 1 && slots[0].binding.method == "mutate.edit" && matches!(slots[0].state, OperatorInvocationState::Terminal { outcome: lattice_conversation::ToolResultOutcome::Failed, .. }))
    );
    assert_eq!(
        std::fs::read_to_string(ws.root.join("src/lib.rs")).unwrap(),
        "pub fn answer() -> i32 { 1 }"
    );
    assert_eq!(history_for(state.path(), &ws.root).current().unwrap(), None);
}

#[tokio::test]
async fn multi_mutation_turn_records_order_and_survives_a_failed_turn() {
    let ws = setup_turn_workspace(&["mutate.create", "mutate.edit"]);
    let state = TempDir::new().unwrap();
    let (layout, store) = session_store(state.path());
    let llm = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(tool_call(
                "c1",
                "mutate.create",
                serde_json::json!({
                    "path":"one.txt", "content":"1\n", "create_parents":true
                }),
            ))
            .push_events(tool_call(
                "c2",
                "mutate.create",
                serde_json::json!({
                    "path":"two.txt", "content":"2\n", "create_parents":true
                }),
            ))
            .push_events(tool_call(
                "c3",
                "mutate.edit",
                serde_json::json!({
                    "path":"one.txt", "expected_hash":content_hash(b"1\n"),
                    "edits":[{"op":"replace_text", "old":"1", "new":"3"}]
                }),
            ))
            .push_error("model exploded after the mutations landed")
            .build(),
    );
    let result = lattice_api::run_turn(
        SessionApi::from_store(store.clone()),
        layout,
        turn_request(&ws, "do three things then fail", None, llm),
    )
    .await
    .unwrap();
    assert_eq!(result.summary.outcome, PublicTurnOutcome::Failed);
    assert_eq!(
        std::fs::read_to_string(ws.root.join("one.txt")).unwrap(),
        "3\n"
    );
    assert_eq!(
        std::fs::read_to_string(ws.root.join("two.txt")).unwrap(),
        "2\n"
    );
    let history = history_for(state.path(), &ws.root);
    let tip = history.current().unwrap().unwrap();
    assert_eq!(commit_chain(&history, &tip).len(), 3);
    // Each commit's parent is the before-state of that mutation, so the chain
    // keeps execution order and the intermediate trees.
    let chain = commit_chain(&history, &tip);
    let before_edit = history.resolve(&chain[1]).unwrap().unwrap();
    assert_eq!(before_edit.files["one.txt"], b"1\n");
    assert_eq!(before_edit.files["two.txt"], b"2\n");
    let after_first = history.resolve(&chain[2]).unwrap().unwrap();
    assert_eq!(after_first.files["one.txt"], b"1\n");
    assert!(!after_first.files.contains_key("two.txt"));
    let snapshot = history.resolve(&tip).unwrap().unwrap();
    assert_eq!(snapshot.files["one.txt"], b"3\n");
    assert_eq!(snapshot.files["two.txt"], b"2\n");
    let detail = store
        .begin_read()
        .unwrap()
        .turn_detail(result.turn.session_id, result.turn.turn_id)
        .unwrap();
    assert_history_bindings(&history, &tip, &detail);
    assert_eq!(detail.summary.as_ref(), Some(&result.summary));
}

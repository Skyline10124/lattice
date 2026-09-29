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

/// AC1/AC2: the history is a private bare `gix` repository outside the Local
/// Workspace and outside the project `.git`, and mutation state is expressed
/// as Git trees rather than a parallel receipt store.
#[tokio::test]
async fn history_is_a_private_bare_repository_outside_workspace_and_project_git() {
    let env = HistoryRuntime::build();
    std::fs::create_dir_all(env.path().join(".git")).expect("project .git");

    create_file(&env.runtime, "src/a.txt", "one\n")
        .await
        .expect("create succeeds");

    let history = env.runtime.workspace_history();
    let repo_path = history.repo_path().expect("history enabled").to_path_buf();
    assert!(
        !repo_path.starts_with(env.path()),
        "history must live outside the Local Workspace: {} vs {}",
        repo_path.display(),
        env.path().display()
    );
    assert!(
        !repo_path.starts_with(env.path().join(".git")),
        "history must live outside the project .git"
    );
    // A bare repository: object database at the path itself, no worktree.
    assert!(repo_path.join("HEAD").is_file());
    assert!(repo_path.join("objects").is_dir());
    assert!(!repo_path.join(".git").exists());

    // Git objects, not a parallel store: the recorded state reads back as a
    // tree of blobs keyed by workspace path.
    let oid = env.current().expect("a mutation was recorded");
    let snapshot = history.resolve(&oid).expect("resolve").expect("commit");
    assert_eq!(
        snapshot.files.get("src/a.txt").map(Vec::as_slice),
        Some(b"one\n".as_ref())
    );
    // No sidecar receipt/SQLite store next to the repository.
    for suspicious in ["receipts", "workspace-graph.db", "mutations.json"] {
        assert!(
            !repo_path.join(suspicious).exists(),
            "no parallel {suspicious} store may exist"
        );
    }
}

/// AC3: every successful create/edit forms an ordered history commit, and
/// multiple mutations in one Turn keep real execution order and parents.
#[tokio::test]
async fn each_successful_mutation_forms_an_ordered_commit_chain() {
    let env = HistoryRuntime::build();
    let history = env.runtime.workspace_history();

    create_file(&env.runtime, "a.txt", "a1\n").await.expect("a");
    let after_first = env.current().expect("first recorded");

    create_file(&env.runtime, "b.txt", "b1\n").await.expect("b");
    let after_second = env.current().expect("second recorded");

    replace_text(&env.runtime, "a.txt", &content_hash(b"a1\n"), "a1", "a2")
        .await
        .expect("edit a");
    let after_third = env.current().expect("third recorded");

    assert_ne!(after_first, after_second);
    assert_ne!(after_second, after_third);

    // Execution order is the commit order: walking parents from the tip yields
    // the three mutations newest-first.
    let chain = commit_chain(history, &after_third);
    assert_eq!(
        chain.len(),
        3,
        "three mutations must produce three chained commits: {chain:?}"
    );
    assert_eq!(chain[0], after_third);
    assert_eq!(chain[1], after_second);
    assert_eq!(chain[2], after_first);

    // The tip tree carries the cumulative after-state of every mutated path.
    let tip = history.resolve(&after_third).unwrap().unwrap();
    assert_eq!(tip.files.get("a.txt").unwrap().as_slice(), b"a2\n");
    assert_eq!(tip.files.get("b.txt").unwrap().as_slice(), b"b1\n");
    // The parent tree is the before-state of the third mutation.
    let before_third = history.resolve(&after_second).unwrap().unwrap();
    assert_eq!(before_third.files.get("a.txt").unwrap().as_slice(), b"a1\n");
}

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
    assert!(env.current().is_some());
}

/// AC5: once installed, a mutation stays recorded — a later failure never
/// rewinds the history and never writes a compensation commit.
#[tokio::test]
async fn an_installed_mutation_survives_later_failures_without_compensation() {
    let env = HistoryRuntime::build();

    create_file(&env.runtime, "kept.txt", "kept\n")
        .await
        .expect("install succeeds");
    let recorded = env.current().expect("recorded");

    // Every later failure mode leaves the history exactly where it was.
    let _ = create_file(&env.runtime, "kept.txt", "again\n").await;
    let _ = replace_text(
        &env.runtime,
        "kept.txt",
        &content_hash(b"wrong\n"),
        "kept",
        "x",
    )
    .await;

    assert_eq!(env.current().as_deref(), Some(recorded.as_str()));
    let snapshot = env
        .runtime
        .workspace_history()
        .resolve(&recorded)
        .unwrap()
        .unwrap();
    assert_eq!(
        snapshot.files.get("kept.txt").unwrap().as_slice(),
        b"kept\n"
    );
    assert_eq!(
        commit_chain(env.runtime.workspace_history(), &recorded).len(),
        1,
        "no compensation commit may be appended"
    );
}

/// AC8: prior object ids stay reachable with no time-based expiry, and the
/// user's project refs are untouched.
/// 正常记录只追加，旧变更始终为 current 的祖先，不需要每条记录独立引用。
#[tokio::test]
async fn prior_object_ids_stay_reachable_through_internal_refs() {
    let env = HistoryRuntime::build();
    create_file(&env.runtime, "a.txt", "1\n").await.unwrap();
    let first = env.current().unwrap();
    create_file(&env.runtime, "b.txt", "2\n").await.unwrap();
    let second = env.current().unwrap();
    create_file(&env.runtime, "c.txt", "3\n").await.unwrap();

    let history = env.runtime.workspace_history();
    for oid in [&first, &second] {
        assert!(
            history.is_reachable(oid).expect("reachability"),
            "prior object id {oid} must stay reachable"
        );
        assert!(history.resolve(oid).unwrap().is_some());
    }

    let repo = gix::open_opts(history.repo_path().unwrap(), gix::open::Options::isolated())
        .expect("open private history");
    let record_pins: Vec<String> = repo
        .references()
        .expect("refs")
        .prefixed("refs/lattice/workspace-history/records/")
        .expect("prefix")
        .filter_map(|r| r.ok())
        .map(|r| r.name().as_bstr().to_string())
        .collect();
    assert!(
        record_pins.is_empty(),
        "normal recording must not write per-record refs: {record_pins:?}"
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

/// AC2: the first `mutate.edit` of a file LATTICE never created still records
/// that file's real before state, driven through the production handler rather
/// than the history API directly.
#[tokio::test]
async fn first_edit_of_a_pre_existing_file_records_its_before_state() {
    let env = HistoryRuntime::build();
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
    .expect("edit succeeds");

    let history = env.runtime.workspace_history();
    let tip = env.current().expect("recorded");
    let after = history.resolve(&tip).unwrap().unwrap();
    assert_eq!(
        after.files.get("pre.txt").unwrap().as_slice(),
        b"rewritten\n"
    );

    // The parent commit is the before state, byte for byte.
    let repo = gix::open_opts(history.repo_path().unwrap(), gix::open::Options::isolated())
        .expect("open private history");
    let parent = repo
        .find_commit(gix::ObjectId::from_hex(tip.as_bytes()).unwrap())
        .unwrap()
        .parent_ids()
        .next()
        .expect("a before state was recorded")
        .detach();
    let before = history.resolve(&parent.to_string()).unwrap().unwrap();
    assert_eq!(
        before.files.get("pre.txt").unwrap().as_slice(),
        b"original\n",
        "the parent tree must be the pre-mutation content"
    );
}

/// 外部写者修改后，下一次变更的历史前态必须是安装器实际看到的字节。
#[tokio::test]
async fn external_drift_between_mutations_records_the_observed_before_state() {
    let env = HistoryRuntime::build();

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

    let history = env.runtime.workspace_history();
    let tip = env.current().expect("recorded");
    let repo = gix::open_opts(history.repo_path().unwrap(), gix::open::Options::isolated())
        .expect("open private history");
    let parent = repo
        .find_commit(gix::ObjectId::from_hex(tip.as_bytes()).unwrap())
        .unwrap()
        .parent_ids()
        .next()
        .expect("parent")
        .detach();
    let before = history.resolve(&parent.to_string()).unwrap().unwrap();
    assert_eq!(
        before.files.get("drift.txt").unwrap().as_slice(),
        b"v2-external\n",
        "the recorded before state must be what the installer observed, not LATTICE's stale copy"
    );
    let after = history.resolve(&tip).unwrap().unwrap();
    assert_eq!(after.files.get("drift.txt").unwrap().as_slice(), b"v3\n");
}

/// AC9: no new user-facing command surface — the Agent-visible catalog is
/// still exactly the closed five methods.
#[tokio::test]
async fn history_adds_no_agent_visible_method() {
    let env = HistoryRuntime::build();
    let mut names: Vec<String> = env
        .runtime
        .tool_definitions()
        .iter()
        .map(|t| t.name.clone())
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec![
            "fetch.web".to_string(),
            "mutate.create".to_string(),
            "mutate.edit".to_string(),
            "observe.workspace".to_string(),
            "session.todo_write".to_string(),
        ],
        "the private history must not add an Agent-visible method"
    );
}

/// AC5 (host enforcement): every installing method routes through the one
/// concrete host module, so no mutating Tool can install without recording.
#[tokio::test]
async fn every_mutating_catalog_method_records() {
    let env = HistoryRuntime::build();
    let mutating: Vec<&str> = vec!["mutate.create", "mutate.edit"];

    let mut seen = Vec::new();
    create_file(&env.runtime, "m.txt", "v1\n").await.unwrap();
    seen.push(env.current().unwrap());
    replace_text(&env.runtime, "m.txt", &content_hash(b"v1\n"), "v1", "v2")
        .await
        .unwrap();
    seen.push(env.current().unwrap());

    assert_eq!(
        seen.len(),
        mutating.len(),
        "each mutating method must move the history exactly once"
    );
    assert_ne!(seen[0], seen[1]);
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

/// 变更以 Turn/Invocation 关联；会话 rewind 与重开不能回退文件历史。
#[tokio::test]
async fn turn_history_bindings_survive_session_rewind_and_reopen() {
    let ws = setup_turn_workspace(&["mutate.create", "observe.workspace"]);
    let state = TempDir::new().unwrap();
    let (layout, store) = session_store(state.path());
    let llm = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(tool_call(
                "c1",
                "mutate.create",
                serde_json::json!({
                    "path":"notes/first.md", "content":"hello\n", "create_parents":true
                }),
            ))
            .push_events(final_text("created"))
            .build(),
    );
    let first = lattice_api::run_turn(
        SessionApi::from_store(store.clone()),
        layout.clone(),
        turn_request(&ws, "create it", None, llm),
    )
    .await
    .unwrap();
    assert_eq!(first.summary.outcome, PublicTurnOutcome::Completed);
    let history = history_for(state.path(), &ws.root);
    let recorded = history.current().unwrap().unwrap();
    let first_detail = store
        .begin_read()
        .unwrap()
        .turn_detail(first.turn.session_id, first.turn.turn_id)
        .unwrap();
    assert_history_bindings(&history, &recorded, &first_detail);

    let llm = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(final_text("nothing to do"))
            .build(),
    );
    let second = lattice_api::run_turn(
        SessionApi::from_store(store.clone()),
        layout.clone(),
        turn_request(&ws, "just talk", Some(first.turn.session_id), llm),
    )
    .await
    .unwrap();
    assert_eq!(second.summary.outcome, PublicTurnOutcome::Completed);
    assert_eq!(
        history.current().unwrap().as_deref(),
        Some(recorded.as_str())
    );
    let lock = store.try_lock_session(first.turn.session_id).unwrap();
    store
        .rewind(&lock, first_detail.user_node, chrono::Utc::now())
        .unwrap();
    drop(lock);
    drop(store);
    let reopened = SessionStore::open(&layout).unwrap();
    let read = reopened.begin_read().unwrap();
    for result in [&first, &second] {
        let found = read
            .turn_detail(result.turn.session_id, result.turn.turn_id)
            .unwrap();
        assert_eq!(found.summary.as_ref(), Some(&result.summary));
        assert_eq!(found.steps, result.steps);
    }
    assert_eq!(
        read.session(first.turn.session_id).unwrap().current,
        first_detail.user_node
    );
    let restarted = history_for(state.path(), &ws.root);
    assert_eq!(
        restarted.current().unwrap().as_deref(),
        Some(recorded.as_str())
    );
    assert_eq!(
        restarted.resolve(&recorded).unwrap().unwrap().files["notes/first.md"],
        b"hello\n"
    );
    assert_eq!(
        std::fs::read(ws.root.join("notes/first.md")).unwrap(),
        b"hello\n"
    );
    assert!(restarted.is_reachable(&recorded).unwrap());
}

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

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
//! - **Turn layer** — a real `run_turn` over a fake streaming LLM. These tests
//!   pin that a completed `TurnCommit` records the workspace-history object id
//!   the Turn arrived at, that a Turn with no mutation inherits the current
//!   identity, and that the mapping survives session reopen, branch movement,
//!   and process restart.

mod support;

use std::path::Path;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};

use lancet_api::{
    AuditBacking, LancetRuntime, LancetRuntimeConfig, WorkspaceHistory, WorkspaceHistoryBacking,
};
use lancet_authority::policy::default::{PolicyConfig, WorkspacePolicy};
use lancet_catalog::CepMethod;
use lancet_catalog::catalog;
use lancet_catalog::methods::workspace::{Create, CreateParams, Edit, EditOperation, EditParams};
use lancet_protocol::CepErrorCode;
use lattice_agent::LlmClient;
use lattice_agent::testing::FakeStreamingLlmClient;
use lattice_api::{RunTurnRequest, RunTurnStatus, SessionApi};
use lattice_llm::{LlmEvent, TokenUsage};
use lattice_profile::{DiscoveryPaths, LoadedAgentProfile, ProfileSource};
use tempfile::TempDir;

// ─── Runtime-layer harness ──────────────────────────────────────────────────

/// A runtime rooted at a temp Local Workspace with its private history in a
/// separate temp directory, so "outside the Local Workspace" is structural.
struct HistoryRuntime {
    runtime: LancetRuntime,
    root: TempDir,
    /// Held so the private history repository outlives the runtime.
    _state: TempDir,
}

impl HistoryRuntime {
    fn build() -> Self {
        let root = TempDir::new().expect("workspace root");
        let state = TempDir::new().expect("state root");
        let runtime = build_runtime(root.path(), &history_backing(state.path()));
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

fn history_backing(state_root: &Path) -> WorkspaceHistoryBacking {
    WorkspaceHistoryBacking::under_project_state_dir(state_root)
}

fn build_runtime(root: &Path, history: &WorkspaceHistoryBacking) -> LancetRuntime {
    let mut policy = PolicyConfig::defaults_for_project_root(root.to_path_buf());
    policy.workspace = WorkspacePolicy {
        denied_write_patterns: Vec::new(),
        ..Default::default()
    };
    let enabled: Vec<String> = catalog().iter().map(|d| d.name.to_string()).collect();
    LancetRuntime::build(LancetRuntimeConfig {
        project_root: root.to_path_buf(),
        enabled_tools: enabled,
        policy,
        cep_mode: lancet_protocol::policy::CepMode::Enforce,
        audit: AuditBacking::InMemory,
        // Native catalog mutations only; no Component Tool is called, so the
        // fail-closed default admission policy is the right one here.
        capability_admission: Default::default(),
        workspace_history: history.clone(),
        model_id: "test".into(),
        runtime_version: "test".into(),
    })
    .expect("runtime with private history builds")
}

fn content_hash(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}

async fn create_file(
    runtime: &LancetRuntime,
    path: &str,
    content: &str,
) -> Result<(), CepErrorCode> {
    runtime
        .client()
        .call::<Create>(CreateParams {
            path: path.into(),
            content: content.into(),
            create_parents: true,
        })
        .await
        .map(|_| ())
        .map_err(|err| err.code)
}

async fn replace_text(
    runtime: &LancetRuntime,
    path: &str,
    expected_hash: &str,
    old: &str,
    new: &str,
) -> Result<(), CepErrorCode> {
    runtime
        .client()
        .call::<Edit>(EditParams {
            path: path.into(),
            expected_hash: expected_hash.into(),
            edits: vec![EditOperation::ReplaceText {
                old: old.into(),
                new: new.into(),
                occurrence: None,
            }],
        })
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
    let denied_runtime = LancetRuntime::build(LancetRuntimeConfig {
        project_root: denied_root.path().to_path_buf(),
        enabled_tools: catalog().iter().map(|d| d.name.to_string()).collect(),
        policy,
        cep_mode: lancet_protocol::policy::CepMode::Enforce,
        audit: AuditBacking::InMemory,
        // Native catalog mutations only; no Component Tool is called, so the
        // fail-closed default admission policy is the right one here.
        capability_admission: Default::default(),
        workspace_history: history_backing(denied_state.path()),
        model_id: "test".into(),
        runtime_version: "test".into(),
    })
    .expect("denied runtime builds");
    let denial = create_file(&denied_runtime, "README.md", "nope\n")
        .await
        .expect_err("guard must deny");
    assert_eq!(denial, CepErrorCode::GuardDenied);
    assert_eq!(
        denied_runtime.workspace_history().current().unwrap(),
        None,
        "a policy denial records nothing"
    );

    // 4. Effect-kind denial. `[lancet.effects] deny` is checked against the
    //    method's declared effect kinds before dispatch, so the handler never
    //    runs, nothing is installed, and nothing is recorded.
    let effect_root = TempDir::new().unwrap();
    let effect_state = TempDir::new().unwrap();
    let mut policy = PolicyConfig::defaults_for_project_root(effect_root.path().to_path_buf());
    policy.workspace = WorkspacePolicy {
        denied_write_patterns: Vec::new(),
        ..Default::default()
    };
    policy
        .denied_effect_kinds
        .insert(lancet_protocol::effect_types::ImpactKind::WorkspaceMutation);
    let effect_runtime = LancetRuntime::build(LancetRuntimeConfig {
        project_root: effect_root.path().to_path_buf(),
        enabled_tools: catalog().iter().map(|d| d.name.to_string()).collect(),
        policy,
        cep_mode: lancet_protocol::policy::CepMode::Enforce,
        audit: AuditBacking::InMemory,
        // Native catalog mutations only; no Component Tool is called, so the
        // fail-closed default admission policy is the right one here.
        capability_admission: Default::default(),
        workspace_history: history_backing(effect_state.path()),
        model_id: "test".into(),
        runtime_version: "test".into(),
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
    let _ = env
        .runtime
        .client()
        .call_raw("exec.command_run", serde_json::json!({}))
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

/// AC8: prior object ids stay reachable through internal refs, with no
/// time-based expiry, and the user's project refs are untouched.
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
            "prior object id {oid} must stay pinned by an internal ref"
        );
        assert!(history.resolve(oid).unwrap().is_some());
    }

    // The pins are real refs under the private namespace, not reflog entries.
    let repo = gix::open_opts(history.repo_path().unwrap(), gix::open::Options::isolated())
        .expect("open private history");
    let pinned: Vec<String> = repo
        .references()
        .expect("refs")
        .prefixed("refs/lattice/workspace-history/records/")
        .expect("prefix")
        .filter_map(|r| r.ok())
        .map(|r| r.name().as_bstr().to_string())
        .collect();
    assert!(
        pinned.len() >= 3,
        "each recorded commit must be pinned: {pinned:?}"
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

/// AC2: the Local Workspace is shared with the user. When a path drifts
/// between two LATTICE mutations, the real before state must still reach the
/// history — otherwise a Rewind would silently discard the user's own edit.
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
    for probe in ["history.read", "workspace.rewind", "history.rewind"] {
        let err = env
            .runtime
            .client()
            .call_raw(probe, serde_json::json!({}))
            .await
            .expect_err("no history method may exist");
        assert_eq!(
            err.wire_code(),
            lancet_protocol::error::jsonrpc::METHOD_NOT_FOUND,
            "{probe} must not be dispatchable"
        );
    }
}

/// AC5 (host enforcement): every installing method routes through the one
/// concrete host module, so no mutating Tool can install without recording.
#[tokio::test]
async fn every_mutating_catalog_method_records() {
    let env = HistoryRuntime::build();
    let mutating: Vec<&str> = vec![Create::NAME, Edit::NAME];

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

/// Serialise the tests that mutate process-global state (`XDG_STATE_HOME`,
/// the current directory).
///
/// Mirrors `lattice_api::ScopedXdgStateHome`, which is `#[cfg(test)]` and so
/// unreachable from an integration-test binary. Keep the `unsafe` env
/// semantics in sync with that copy.
static ENV_MUTEX: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

struct ScopedState {
    _lock: MutexGuard<'static, ()>,
    state: TempDir,
    prev_xdg: Option<String>,
    prev_cwd: std::path::PathBuf,
}

impl ScopedState {
    fn new(cwd: &Path) -> Self {
        let lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let prev_xdg = std::env::var("XDG_STATE_HOME").ok();
        let prev_cwd = std::env::current_dir().expect("cwd");
        let state = TempDir::new().expect("state dir");
        // SAFETY: guarded by ENV_MUTEX and restored on drop.
        unsafe { std::env::set_var("XDG_STATE_HOME", state.path()) };
        std::env::set_current_dir(cwd).expect("set cwd");
        Self {
            _lock: lock,
            state,
            prev_xdg,
            prev_cwd,
        }
    }

    fn path(&self) -> &Path {
        self.state.path()
    }
}

impl Drop for ScopedState {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.prev_cwd);
        // SAFETY: guarded by ENV_MUTEX for the lifetime of this guard.
        unsafe {
            match self.prev_xdg.as_deref() {
                Some(prev) => std::env::set_var("XDG_STATE_HOME", prev),
                None => std::env::remove_var("XDG_STATE_HOME"),
            }
        }
    }
}

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
            id: id.into(),
            name: name.into(),
        },
        LlmEvent::ToolCallDelta {
            id: id.into(),
            arguments_delta: args.to_string(),
        },
        LlmEvent::ToolCallEnd { id: id.into() },
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
    session_id: &str,
    llm: Arc<dyn LlmClient>,
) -> RunTurnRequest {
    RunTurnRequest {
        input: input.to_string(),
        session_id: session_id.to_string(),
        loaded_profile: Some(ws.loaded.clone()),
        cancel_flag: None,
        test_llm: Some(llm),
        test_project_root: Some(ws.root.to_string_lossy().to_string()),
        // Take the durable path: an external audit store and a real private
        // workspace history, both under the scoped state root.
        test_persist_audit: Some(false),
    }
}

fn new_session(sessions: &Path) -> String {
    SessionApi::git(sessions)
        .create_session(lattice_api::CreateSessionRequest {
            label: None,
            active_agent: "default".to_string(),
            project_branch: "main".to_string(),
        })
        .expect("create session")
        .session_id
}

fn history_for(state_root: &Path, project_root: &Path) -> WorkspaceHistory {
    let backing = lancet_api::workspace_history_backing_in(state_root, project_root)
        .expect("resolve history backing");
    WorkspaceHistory::open(backing).expect("open history")
}

// ─── Turn layer ─────────────────────────────────────────────────────────────

/// AC6/AC7: a completed `TurnCommit` records the workspace-history object id
/// the Turn ended at, a Turn with no mutation inherits the current identity,
/// and both survive session reopen, branch movement, and process restart.
#[tokio::test]
async fn turn_commits_record_the_history_identity_and_survive_reopen() {
    let ws = setup_turn_workspace(&["mutate.create", "observe.workspace"]);
    let scoped = ScopedState::new(&ws.root);
    let sessions_dir = TempDir::new().unwrap();
    let sessions = sessions_dir.path().join("sessions");
    let session_id = new_session(&sessions);

    // Turn 1 installs one mutation.
    let llm: Arc<dyn LlmClient> = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(tool_call(
                "c1",
                "mutate.create",
                serde_json::json!({
                    "path": "notes/first.md",
                    "content": "hello\n",
                    "create_parents": true
                }),
            ))
            .push_events(final_text("created"))
            .build(),
    );
    let first = lattice_api::run_turn(
        SessionApi::git(&sessions),
        turn_request(&ws, "create it", &session_id, llm),
    )
    .await
    .expect("turn 1 runs");
    assert_eq!(first.status, RunTurnStatus::Completed);

    let history = history_for(scoped.path(), &ws.root);
    let recorded = history
        .current()
        .expect("read history")
        .expect("turn 1 recorded a mutation");

    let api = SessionApi::git(&sessions);
    let commit = api
        .get_turn_commit(&session_id, &first.commit_id)
        .expect("get turn commit")
        .expect("turn 1 commit exists");
    assert_eq!(
        commit.workspace_history_oid.as_deref(),
        Some(recorded.as_str()),
        "the completed TurnCommit must record the object id the Turn arrived at"
    );

    // Turn 2 installs nothing and must inherit the current identity.
    let llm2: Arc<dyn LlmClient> = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(final_text("nothing to do"))
            .build(),
    );
    let second = lattice_api::run_turn(
        SessionApi::git(&sessions),
        turn_request(&ws, "just talk", &session_id, llm2),
    )
    .await
    .expect("turn 2 runs");
    assert_eq!(second.status, RunTurnStatus::Completed);

    let second_commit = SessionApi::git(&sessions)
        .get_turn_commit(&session_id, &second.commit_id)
        .expect("get turn 2 commit")
        .expect("turn 2 commit exists");
    assert_eq!(
        second_commit.workspace_history_oid.as_deref(),
        Some(recorded.as_str()),
        "a Turn with no mutation records the inherited current identity"
    );
    assert_eq!(
        history.current().unwrap().as_deref(),
        Some(recorded.as_str()),
        "a Turn with no mutation must not move the current ref"
    );

    // Branch movement: a new session branch off the first commit, then reopen
    // the store from scratch (process restart) and resolve both Turns again.
    SessionApi::git(&sessions)
        .branch(&session_id, "side", Some(&first.commit_id))
        .expect("create branch");
    SessionApi::git(&sessions)
        .checkout(&session_id, "side")
        .expect("checkout branch");

    let reopened = SessionApi::git(&sessions);
    for (label, commit_id) in [("first", &first.commit_id), ("second", &second.commit_id)] {
        let found = reopened
            .get_turn_commit(&session_id, commit_id)
            .expect("get after branch movement")
            .unwrap_or_else(|| panic!("{label} turn must stay resolvable"));
        assert_eq!(
            found.workspace_history_oid.as_deref(),
            Some(recorded.as_str()),
            "{label} turn must resolve to its workspace history after branch movement"
        );
    }

    // Process restart on the history side: a fresh handle over the same
    // private repository resolves the recorded object id to real content.
    let restarted = history_for(scoped.path(), &ws.root);
    let snapshot = restarted
        .resolve(&recorded)
        .expect("resolve")
        .expect("commit still present");
    assert_eq!(
        snapshot.files.get("notes/first.md").unwrap().as_slice(),
        b"hello\n"
    );
    assert!(restarted.is_reachable(&recorded).unwrap());
}

/// AC4/AC6 at the Turn layer: a Turn whose only mutation fails before
/// installation records no workspace-history identity on its commit and leaves
/// the history empty.
#[tokio::test]
async fn turn_whose_mutation_fails_before_install_records_no_history_identity() {
    let ws = setup_turn_workspace(&["mutate.edit"]);
    let scoped = ScopedState::new(&ws.root);
    let sessions_dir = TempDir::new().unwrap();
    let sessions = sessions_dir.path().join("sessions");
    let session_id = new_session(&sessions);

    let llm: Arc<dyn LlmClient> = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(tool_call(
                "c1",
                "mutate.edit",
                serde_json::json!({
                    "path": "src/lib.rs",
                    // Stale precondition: the Mutation Precondition fails and
                    // nothing is ever installed.
                    "expected_hash": content_hash(b"not the current content"),
                    "edits": [{ "op": "replace_text", "old": "1", "new": "2" }]
                }),
            ))
            .push_events(final_text("could not edit"))
            .build(),
    );

    let result = lattice_api::run_turn(
        SessionApi::git(&sessions),
        turn_request(&ws, "try an impossible edit", &session_id, llm),
    )
    .await
    .expect("turn runs");

    let tool = result
        .tool_results
        .iter()
        .find(|t| t.name == "mutate.edit")
        .expect("the mutation was attempted");
    assert!(!tool.ok, "the mutation must have failed");
    assert_eq!(
        std::fs::read_to_string(ws.root.join("src/lib.rs")).unwrap(),
        "pub fn answer() -> i32 { 1 }",
        "a failed precondition must leave the target unchanged"
    );

    let history = history_for(scoped.path(), &ws.root);
    assert_eq!(
        history.current().unwrap(),
        None,
        "a pre-install failure must not move the current history ref"
    );

    let commit = SessionApi::git(&sessions)
        .get_turn_commit(&session_id, &result.commit_id)
        .expect("get turn commit")
        .expect("turn is committed");
    assert_eq!(
        commit.workspace_history_oid, None,
        "a Turn that installed nothing has no history identity to inherit"
    );
}

/// AC3/AC5: several mutations in one Turn stay ordered, and the Turn records
/// the object id it arrived at even though the Turn itself ends in failure.
#[tokio::test]
async fn multi_mutation_turn_records_order_and_survives_a_failed_turn() {
    let ws = setup_turn_workspace(&["mutate.create", "mutate.edit"]);
    let scoped = ScopedState::new(&ws.root);
    let sessions_dir = TempDir::new().unwrap();
    let sessions = sessions_dir.path().join("sessions");
    let session_id = new_session(&sessions);

    let llm: Arc<dyn LlmClient> = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(tool_call(
                "c1",
                "mutate.create",
                serde_json::json!({
                    "path": "one.txt",
                    "content": "1\n",
                    "create_parents": true
                }),
            ))
            .push_events(tool_call(
                "c2",
                "mutate.create",
                serde_json::json!({
                    "path": "two.txt",
                    "content": "2\n",
                    "create_parents": true
                }),
            ))
            .push_events(tool_call(
                "c3",
                "mutate.edit",
                serde_json::json!({
                    "path": "one.txt",
                    "expected_hash": content_hash(b"1\n"),
                    "edits": [{ "op": "replace_text", "old": "1", "new": "3" }]
                }),
            ))
            // The model fails after all three mutations are installed.
            .push_error("model exploded after the mutations landed")
            .build(),
    );

    let result = lattice_api::run_turn(
        SessionApi::git(&sessions),
        turn_request(&ws, "do three things then fail", &session_id, llm),
    )
    .await
    .expect("turn completes with a terminal status");
    assert_eq!(
        result.status,
        RunTurnStatus::Error,
        "the Turn must end in failure for this acceptance case"
    );

    // The mutations are on disk and were never rolled back.
    assert_eq!(
        std::fs::read_to_string(ws.root.join("one.txt")).unwrap(),
        "3\n"
    );
    assert_eq!(
        std::fs::read_to_string(ws.root.join("two.txt")).unwrap(),
        "2\n"
    );

    let history = history_for(scoped.path(), &ws.root);
    let tip = history.current().unwrap().expect("mutations recorded");
    let chain = commit_chain(&history, &tip);
    assert_eq!(
        chain.len(),
        3,
        "three installed mutations must produce three ordered commits: {chain:?}"
    );

    let snapshot = history.resolve(&tip).unwrap().unwrap();
    assert_eq!(snapshot.files.get("one.txt").unwrap().as_slice(), b"3\n");
    assert_eq!(snapshot.files.get("two.txt").unwrap().as_slice(), b"2\n");

    let commit = SessionApi::git(&sessions)
        .get_turn_commit(&session_id, &result.commit_id)
        .expect("get turn commit")
        .expect("failed turn is still committed");
    assert_eq!(
        commit.workspace_history_oid.as_deref(),
        Some(tip.as_str()),
        "a Turn that failed after installing still records the history it reached"
    );
}

// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! GitHub #64 acceptance tests: explicit Rewind reverses only the Atomic
//! Local Mutations LATTICE installed after the selected Turn.
//!
//! Two layers, mirroring the #63 suite:
//!
//! - **Runtime layer** — a real `lancet_api::LancetRuntime` rooted at a temp
//!   Local Workspace with an explicit private history, driven through the CEP
//!   client and rewound through `lancet_api::rewind_workspace`. These tests
//!   pin what is reversed, what survives, when a Rewind conflicts, and that
//!   neither the project `.git` nor object reachability is disturbed.
//! - **Turn layer** — real `run_turn` Turns over a fake streaming LLM,
//!   rewound through the product entry point `lattice_api::rewind_to_turn`.
//!   These tests pin target resolution, the ordering of the two ref moves,
//!   the durable audit record, and that a conflict moves neither the
//!   conversation target nor the Local Workspace.

mod support;

use std::path::Path;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};

use lancet_api::{
    HeldWorkspaceOperation, LancetRuntimeConfig, WorkspaceHistory, WorkspaceHistoryBacking,
    WorkspaceRewindError, WorkspaceRewindOutcome, rewind_workspace,
};
use lancet_authority::policy::default::{PolicyConfig, WorkspacePolicy};
use lancet_catalog::catalog;
use lancet_protocol::CepErrorCode;
use lattice_agent::LlmClient;
use lattice_agent::testing::FakeStreamingLlmClient;
use lattice_api::{RewindOutcome, RunTurnRequest, SessionApi};
use lattice_llm::{LlmEvent, TokenUsage};
use lattice_profile::{DiscoveryPaths, LoadedAgentProfile, ProfileSource};
use lattice_session::{
    GitSessionGraphStore, GraphDiff, GraphHead, GraphRef, GraphSession, HeadTranscript,
    PreparedTurnCommit, ResetResult, SessionError, SessionGraphStore, SessionGraphSummary,
    SessionStatus, StateLayout, TurnCommit, TurnCommitInput, TurnLogEntry,
};
use support::ReservedRuntime;
use tempfile::TempDir;

fn store_key_hex(root: &Path) -> String {
    lattice_api::LocalWorkspaceRoot::from_path(root)
        .expect("resolve workspace root")
        .store_key_hex()
}

// ─── Runtime-layer harness ──────────────────────────────────────────────────

/// Component Tools this harness drives through Cell admission (#30/#31).
const COMPONENT_TOOLS: [&str; 3] = ["mutate.create", "mutate.edit", "observe.workspace"];

/// A runtime rooted at a temp Local Workspace whose private history lives in
/// a separate temp state directory.
struct RewindRuntime {
    runtime: ReservedRuntime,
    root: TempDir,
    state: TempDir,
}

impl RewindRuntime {
    fn build() -> Self {
        let root = TempDir::new().expect("workspace root");
        let state = TempDir::new().expect("state root");
        let runtime = build_runtime(root.path(), &backing(state.path(), root.path()));
        Self {
            runtime,
            root,
            state,
        }
    }

    fn path(&self) -> &Path {
        self.root.path()
    }

    fn backing(&self) -> WorkspaceHistoryBacking {
        backing(self.state.path(), self.root.path())
    }

    fn current(&self) -> Option<String> {
        self.runtime
            .workspace_history()
            .current()
            .expect("read history current")
    }

    /// Rewind the Local Workspace back to `target`, holding the project
    /// operation lock the way the coordinator does.
    fn rewind(&self, target: Option<&str>) -> WorkspaceRewindOutcome {
        let backing = self.backing();
        let held = HeldWorkspaceOperation::acquire(&backing).expect("operation lock");
        rewind_workspace(self.path(), backing, target, &held).expect("rewind runs")
    }
}

/// The project-scoped history backing for one workspace root, with the store
/// first-created — #143 strict open requires the store to exist, and
/// production creates it in `session new`.
fn backing(state_root: &Path, workspace_root: &Path) -> WorkspaceHistoryBacking {
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
    // Catalog natives plus the mutate/observe Components this harness drives.
    let mut enabled: Vec<String> = catalog().iter().map(|d| d.name.to_string()).collect();
    enabled.extend(COMPONENT_TOOLS.iter().map(|name| (*name).to_string()));
    ReservedRuntime::build(LancetRuntimeConfig {
        local_workspace_root: root.to_path_buf(),
        invocations: lancet_api::WorkspaceHistoryBacking::Disabled,
        enabled_tools: enabled,
        policy,
        cep_mode: lancet_protocol::policy::CepMode::Enforce,
        // mutate.create/edit are Components (#31): Cell admission is fail-closed.
        capability_admission: lancet_protocol::evidence::CapabilityAdmissionPolicy {
            enabled_tools: COMPONENT_TOOLS.iter().map(|n| (*n).to_string()).collect(),
            denied_capabilities: Default::default(),
            max_boundary: lancet_protocol::cell::Trust::In,
        },
        workspace_history: history.clone(),
        components: None,
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

/// Mutation commits reachable from `tip`, newest first, skipping baselines.
fn mutation_chain(history: &WorkspaceHistory, tip: &str) -> Vec<String> {
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
        next = commit.parent_ids().next().map(|id| id.detach());
        if !message.contains("lattice-history-kind: baseline") {
            out.push(oid.to_string());
        }
    }
    out
}

// ─── Runtime layer ──────────────────────────────────────────────────────────

/// AC2/AC5/AC6: only LATTICE mutations recorded after the target are
/// reversed — an edit goes back to its parent-tree content, a create is
/// removed — while every external change LATTICE never touched survives,
/// including ignored content.
#[tokio::test]
async fn rewind_reverses_only_later_lattice_mutations_and_preserves_external_changes() {
    let env = RewindRuntime::build();
    std::fs::write(env.path().join(".gitignore"), "secret.env\n").expect("seed gitignore");
    std::fs::write(env.path().join("secret.env"), "TOKEN=abc\n").expect("seed ignored file");
    std::fs::write(env.path().join("external.txt"), "user content\n").expect("seed external");
    std::fs::write(env.path().join("doomed.txt"), "delete me\n").expect("seed doomed");

    // Turn 1: one mutation. This identity is the rewind target.
    create_file(&env.runtime, "notes.md", "v1\n")
        .await
        .expect("create notes.md");
    let target = env.current().expect("turn 1 recorded");

    // Turn 2: an edit of the same path plus a create of a new one.
    replace_text(&env.runtime, "notes.md", &content_hash(b"v1\n"), "v1", "v2")
        .await
        .expect("edit notes.md");
    create_file(&env.runtime, "src/added.rs", "fn added() {}\n")
        .await
        .expect("create src/added.rs");
    let tip = env.current().expect("turn 2 recorded");
    assert_ne!(tip, target);

    // External activity LATTICE never saw: a new file, an edit of an
    // untouched file, a delete of an untouched file, and ignored content.
    std::fs::write(env.path().join("brand-new.txt"), "external create\n").expect("external create");
    std::fs::write(env.path().join("external.txt"), "user edited\n").expect("external edit");
    std::fs::remove_file(env.path().join("doomed.txt")).expect("external delete");
    std::fs::write(env.path().join("secret.env"), "TOKEN=rotated\n").expect("ignored edit");

    let outcome = env.rewind(Some(&target));
    let WorkspaceRewindOutcome::Applied(report) = outcome else {
        panic!("expected a clean rewind, got {outcome:?}");
    };

    // Exactly the two later mutations were reversed, newest first.
    assert_eq!(report.steps.len(), 2, "steps: {:?}", report.steps);
    assert_eq!(report.steps[0].method, "mutate.create");
    assert_eq!(report.steps[0].path, "src/added.rs");
    assert_eq!(report.steps[1].method, "mutate.edit");
    assert_eq!(report.steps[1].path, "notes.md");
    assert_eq!(report.from_history_oid.as_deref(), Some(tip.as_str()));
    assert_eq!(report.to_history_oid.as_deref(), Some(target.as_str()));
    assert_eq!(env.current().as_deref(), Some(target.as_str()));

    // Workspace: the edit is back to its parent-tree content, the create is
    // gone, and nothing external moved.
    assert_eq!(
        std::fs::read(env.path().join("notes.md")).unwrap(),
        b"v1\n",
        "an edit must be restored to the recorded parent-tree content"
    );
    assert!(
        !env.path().join("src/added.rs").exists(),
        "a reversed create must be deleted"
    );
    assert_eq!(
        std::fs::read(env.path().join("brand-new.txt")).unwrap(),
        b"external create\n"
    );
    assert_eq!(
        std::fs::read(env.path().join("external.txt")).unwrap(),
        b"user edited\n"
    );
    assert!(
        !env.path().join("doomed.txt").exists(),
        "an external delete must stay deleted"
    );
    assert_eq!(
        std::fs::read(env.path().join("secret.env")).unwrap(),
        b"TOKEN=rotated\n",
        "ignored content LATTICE never touched must be untouched"
    );
}

/// AC3/AC4: a diverged LATTICE-touched path makes the whole Rewind report a
/// structured conflict, and neither the Local Workspace nor the private
/// history ref moves — including for the paths that did match.
#[tokio::test]
async fn same_path_divergence_conflicts_and_moves_nothing() {
    let env = RewindRuntime::build();
    create_file(&env.runtime, "a.txt", "a1\n")
        .await
        .expect("create a.txt");
    let target = env.current().expect("recorded");

    replace_text(&env.runtime, "a.txt", &content_hash(b"a1\n"), "a1", "a2")
        .await
        .expect("edit a.txt");
    create_file(&env.runtime, "b.txt", "b1\n")
        .await
        .expect("create b.txt");
    let tip = env.current().expect("recorded");

    // The user edits a LATTICE-touched path outside LATTICE.
    std::fs::write(env.path().join("a.txt"), "hand written\n").expect("external edit");
    let before = tree_snapshot(env.path());

    let outcome = env.rewind(Some(&target));
    let WorkspaceRewindOutcome::Conflict(conflict) = outcome else {
        panic!("expected a conflict, got {outcome:?}");
    };

    assert_eq!(conflict.conflicts.len(), 1);
    let reported = &conflict.conflicts[0];
    assert_eq!(reported.path, "a.txt");
    assert_eq!(
        reported.expected_revision.as_deref(),
        Some(content_hash(b"a2\n").as_str()),
        "the conflict names the recorded post-state revision"
    );
    assert_eq!(
        reported.observed_revision.as_deref(),
        Some(content_hash(b"hand written\n").as_str()),
        "and the observed Local Revision"
    );
    assert!(
        !reported.detail.contains("hand written"),
        "conflict details must not carry file content: {}",
        reported.detail
    );
    // Preflight covers every affected path, not only the divergent one.
    let preflighted: Vec<&str> = conflict.preflight.iter().map(|p| p.path.as_str()).collect();
    assert_eq!(preflighted, vec!["a.txt", "b.txt"]);

    assert_eq!(
        tree_snapshot(env.path()),
        before,
        "a conflicted rewind must not write any path"
    );
    assert_eq!(
        env.current().as_deref(),
        Some(tip.as_str()),
        "a conflicted rewind must not move the private history ref"
    );
}

/// AC8: no compensation commits, the user's project `.git` is never touched,
/// and every reversed object id stays reachable through the internal pins.
#[tokio::test]
async fn rewind_writes_no_compensation_commit_and_keeps_objects_reachable() {
    let env = RewindRuntime::build();
    support::init_git_repo(env.path());
    let git_before = tree_snapshot(&env.path().join(".git"));
    assert!(!git_before.is_empty(), "fixture must have a project .git");

    create_file(&env.runtime, "keep.txt", "k1\n")
        .await
        .expect("create keep.txt");
    let target = env.current().expect("recorded");
    replace_text(&env.runtime, "keep.txt", &content_hash(b"k1\n"), "k1", "k2")
        .await
        .expect("edit keep.txt");
    create_file(&env.runtime, "gone.txt", "g1\n")
        .await
        .expect("create gone.txt");
    let tip = env.current().expect("recorded");

    let history = env.runtime.workspace_history();
    let chain_before = mutation_chain(history, &tip);
    assert_eq!(chain_before.len(), 3);

    let outcome = env.rewind(Some(&target));
    assert!(matches!(outcome, WorkspaceRewindOutcome::Applied(_)));

    // The chain that was rewound away is unchanged — no compensation commit
    // was appended to it, and the new tip is the target itself.
    assert_eq!(mutation_chain(history, &tip), chain_before);
    assert_eq!(env.current().as_deref(), Some(target.as_str()));
    assert_eq!(mutation_chain(history, &target).len(), 1);

    // Every reversed commit is still resolvable through its pin.
    for oid in &chain_before {
        assert!(
            history.is_reachable(oid).expect("reachability"),
            "{oid} lost its internal pin"
        );
        assert!(history.resolve(oid).expect("resolve").is_some());
    }

    assert_eq!(
        tree_snapshot(&env.path().join(".git")),
        git_before,
        "a rewind must not write the user's project .git"
    );
}

/// AC6: restore-writes go through the same safety constraints as mutations —
/// a path whose kind changed outside LATTICE is reported as a conflict rather
/// than written through.
#[tokio::test]
async fn a_path_whose_kind_changed_conflicts_instead_of_being_written_through() {
    let env = RewindRuntime::build();
    create_file(&env.runtime, "swap.txt", "s1\n")
        .await
        .expect("create swap.txt");
    let target = env.current().expect("recorded");
    replace_text(&env.runtime, "swap.txt", &content_hash(b"s1\n"), "s1", "s2")
        .await
        .expect("edit swap.txt");

    // The user replaces the file with a symlink pointing outside the
    // workspace: restoring through it would write outside the Local
    // Workspace, so the Rewind must refuse.
    let outside = TempDir::new().unwrap();
    std::fs::write(outside.path().join("real"), "outside\n").unwrap();
    std::fs::remove_file(env.path().join("swap.txt")).unwrap();
    std::os::unix::fs::symlink(outside.path().join("real"), env.path().join("swap.txt")).unwrap();

    let outcome = env.rewind(Some(&target));
    let WorkspaceRewindOutcome::Conflict(conflict) = outcome else {
        panic!("expected a conflict, got {outcome:?}");
    };
    assert_eq!(conflict.conflicts.len(), 1);
    assert!(
        conflict.conflicts[0].detail.contains("symlink"),
        "detail: {}",
        conflict.conflicts[0].detail
    );
    assert_eq!(
        std::fs::read(outside.path().join("real")).unwrap(),
        b"outside\n",
        "nothing may be written through the symlink"
    );
}

/// A target that is not on the current chain is refused rather than
/// interpreted as "unwind everything".
#[tokio::test]
async fn an_off_chain_target_is_refused() {
    let env = RewindRuntime::build();
    create_file(&env.runtime, "a.txt", "a\n")
        .await
        .expect("create");
    let backing = env.backing();
    let held = HeldWorkspaceOperation::acquire(&backing).expect("operation lock");
    let err = rewind_workspace(env.path(), backing, Some(&"c".repeat(40)), &held)
        .expect_err("an unknown target must be refused");
    assert!(
        matches!(err, WorkspaceRewindError::TargetUnreachable { .. }),
        "got {err:?}"
    );
}

// ─── Turn-layer harness ─────────────────────────────────────────────────────

/// Serialise the tests that mutate process-global state (`XDG_STATE_HOME`,
/// the current directory).
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
    std::fs::write(root.join(".gitignore"), "secret.env\n").unwrap();
    std::fs::write(root.join("secret.env"), "TOKEN=abc\n").unwrap();
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
        // Durable path: external audit store and a real private history,
        // both under the scoped state root.
    }
}

fn test_state_layout() -> StateLayout {
    let state_root = std::env::var_os("XDG_STATE_HOME").expect("ScopedState sets XDG_STATE_HOME");
    StateLayout::in_state_root(Path::new(&state_root))
}

fn open_session_store(_sessions: &Path) -> SessionApi {
    let layout = test_state_layout();
    SessionApi::git_open_with_layout(layout.lattice_root().join("sessions.git"), &layout)
        .expect("open session store")
}

fn new_session(_sessions: &Path, project_root: &Path) -> String {
    let layout = test_state_layout();
    SessionApi::git_create_with_layout(layout.lattice_root().join("sessions.git"), &layout)
        .expect("create session store")
        .create_session(lattice_api::CreateSessionRequest {
            label: None,
            active_agent: "default".to_string(),
            project_branch: "main".to_string(),
            project_store_key: store_key_hex(project_root),
        })
        .expect("create session")
        .session_id
}

fn history_for(state_root: &Path, project_root: &Path) -> WorkspaceHistory {
    let backing = lancet_api::workspace_history_backing_in(state_root, project_root)
        .expect("resolve history backing");
    WorkspaceHistory::open(backing).expect("open history")
}

/// Run one full production Turn that creates `path` with `content`.
async fn turn_creating_through_product(
    ws: &TurnWorkspace,
    sessions: &Path,
    session_id: &str,
    path: &str,
    content: &str,
) -> String {
    let llm: Arc<dyn LlmClient> = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(tool_call(
                "c1",
                "mutate.create",
                serde_json::json!({
                    "path": path,
                    "content": content,
                    "create_parents": true
                }),
            ))
            .push_events(final_text("created"))
            .build(),
    );
    let result = lattice_api::run_turn(
        open_session_store(sessions),
        turn_request(ws, &format!("create {path}"), session_id, llm),
    )
    .await
    .expect("turn runs");
    assert!(
        matches!(
            result.outcome,
            lattice_session::TurnOutcome::Completed { .. }
        ),
        "turn creating {path} must complete"
    );
    // A mis-scripted tool call would still complete the Turn, so prove the
    // mutation really landed rather than testing a vacuous history.
    assert_eq!(
        std::fs::read(ws.root.join(path)).expect("created file exists"),
        content.as_bytes(),
        "the Turn must have installed {path}"
    );
    result.commit_id
}

/// Run one full production Turn that replaces `old` with `new` in `path`.
async fn turn_editing_through_product(
    ws: &TurnWorkspace,
    sessions: &Path,
    session_id: &str,
    path: &str,
    expected: &str,
    old: &str,
    new: &str,
) -> String {
    let llm: Arc<dyn LlmClient> = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(tool_call(
                "c1",
                "mutate.edit",
                serde_json::json!({
                    "path": path,
                    "expected_hash": expected,
                    "edits": [{"op": "replace_text", "old": old, "new": new}]
                }),
            ))
            .push_events(final_text("edited"))
            .build(),
    );
    let result = lattice_api::run_turn(
        open_session_store(sessions),
        turn_request(ws, &format!("edit {path}"), session_id, llm),
    )
    .await
    .expect("turn runs");
    assert!(
        matches!(
            result.outcome,
            lattice_session::TurnOutcome::Completed { .. }
        ),
        "turn editing {path} must complete"
    );
    let installed =
        String::from_utf8(std::fs::read(ws.root.join(path)).expect("edited file exists"))
            .expect("utf8");
    assert!(
        installed.contains(new) && !installed.contains(old),
        "the Turn must have installed the edit to {path}, got {installed:?}"
    );
    result.commit_id
}

/// Seed the exact Session/WorkspaceHistory facts a completed mutating Turn
/// would publish, without rebuilding the runtime/provider for reducer-only
/// Rewind scenarios. The product-path acceptance below still uses `run_turn`.
fn seed_mutating_turn(
    ws: &TurnWorkspace,
    session_id: &str,
    method: &str,
    path: &str,
    before: Option<Vec<u8>>,
    after: Vec<u8>,
) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    let sequence = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let turn_id =
        lancet_protocol::TurnId::parse(&format!("10000000-0000-4000-8000-{sequence:012x}"))
            .expect("seeded TurnId");
    let invocation_id =
        lancet_protocol::InvocationId::parse(&format!("20000000-0000-4000-8000-{sequence:012x}"))
            .expect("seeded InvocationId");

    if let Some(parent) = ws.root.join(path).parent() {
        std::fs::create_dir_all(parent).expect("create mutation parent");
    }
    std::fs::write(ws.root.join(path), &after).expect("install seeded mutation");
    let state_root = std::env::var_os("XDG_STATE_HOME").expect("ScopedState sets XDG_STATE_HOME");
    let recorded = history_for(Path::new(&state_root), &ws.root)
        .record(&lancet_runtime::MutationRecord {
            method: method.to_string(),
            turn_id,
            invocation_id,
            path: path.to_string(),
            before,
            after: after.clone(),
            content_hash: content_hash(&after),
            recorded_at: 1_700_000_000 + sequence as i64,
        })
        .expect("record seeded mutation")
        .expect("workspace history enabled");

    let layout = test_state_layout();
    let store = GitSessionGraphStore::open(layout.lattice_root().join("sessions.git"))
        .expect("open session store");
    let parent_ids = store
        .status(session_id)
        .expect("read seeded Session head")
        .current_commit_id
        .into_iter()
        .collect();
    store
        .test_commit_turn(TurnCommitInput {
            turn_id,
            workspace_history_checkpoint: lattice_session::WorkspaceHistoryCheckpoint::Oid(
                recorded.commit_id,
            ),
            session_id: session_id.to_string(),
            parent_ids,
            messages: vec![
                lattice_session::test_helpers::user_msg(&format!("{method} {path}")),
                lattice_session::test_helpers::assistant_msg("seeded mutation"),
            ],
            agent_name: "default".to_string(),
            profile_snapshot_hash: "test-profile".to_string(),
            profile_snapshot_json: "{}".to_string(),
            model: "sonnet".to_string(),
            provider: "anthropic".to_string(),
            outcome: lattice_session::TurnOutcome::Completed {
                finish_reason: lattice_session::TurnFinishReason::Stop,
            },
            origin: lattice_session::ClosureOrigin::Live,
            commit_message: format!("seeded {method} turn"),
            usage: None,
        })
        .expect("commit seeded Turn")
        .commit_id
}

async fn turn_creating(
    ws: &TurnWorkspace,
    _sessions: &Path,
    session_id: &str,
    path: &str,
    content: &str,
) -> String {
    assert!(!ws.root.join(path).exists(), "seeded create target exists");
    seed_mutating_turn(
        ws,
        session_id,
        "mutate.create",
        path,
        None,
        content.as_bytes().to_vec(),
    )
}

async fn turn_editing(
    ws: &TurnWorkspace,
    _sessions: &Path,
    session_id: &str,
    path: &str,
    expected: &str,
    old: &str,
    new: &str,
) -> String {
    let before = std::fs::read(ws.root.join(path)).expect("seeded edit target");
    assert_eq!(content_hash(&before), expected);
    let before_text = String::from_utf8(before.clone()).expect("utf8 edit target");
    let after = before_text.replacen(old, new, 1).into_bytes();
    assert_ne!(after, before, "seeded edit must replace text");
    seed_mutating_turn(ws, session_id, "mutate.edit", path, Some(before), after)
}

fn rewind_page(project_root: &Path, session_id: &str) -> lancet_api::RewindAuditPage {
    let state_root = std::env::var_os("XDG_STATE_HOME").expect("ScopedState sets XDG_STATE_HOME");
    let backing = lancet_api::workspace_history_backing_in(Path::new(&state_root), project_root)
        .expect("project backing");
    let session_id = lancet_protocol::SessionId::parse(session_id).expect("canonical SessionId");
    lancet_api::rewind_audit_records(&backing, &session_id).expect("read rewind audit records")
}

fn rewind_records(project_root: &Path, session_id: &str) -> Vec<lancet_api::RewindAuditRecord> {
    let page = rewind_page(project_root, session_id);
    assert!(
        !page.has_older,
        "acceptance fixture expected a complete Rewind audit page"
    );
    page.records
}

fn audit_ref_path(project_root: &Path, session_id: &str) -> std::path::PathBuf {
    lancet_api::workspace_history_backing(project_root)
        .expect("project backing")
        .store_path()
        .expect("history store path")
        .join("refs/lattice/rewind-audit/sessions")
        .join(session_id)
}

fn fail_audit_ref_fsync(project_root: &Path, session_id: &str) {
    lattice_store::fault::fail_fsync_path(audit_ref_path(project_root, session_id));
}

fn fail_history_current_ref_fsync(project_root: &Path) {
    let path = lancet_api::workspace_history_backing(project_root)
        .expect("project backing")
        .store_path()
        .expect("history store path")
        .join("refs/lattice/workspace-history/current");
    lattice_store::fault::fail_fsync_path(path);
}

fn expect_audit_publication_failure(
    error: lattice_api::RewindError,
) -> lattice_api::RewindPartialState {
    match error {
        lattice_api::RewindError::AuditPublicationFailed { detail, partial } => {
            assert_eq!(
                detail,
                "audit append returned an error; see typed partial state"
            );
            *partial
        }
        other => panic!("expected typed audit publication failure, got {other:?}"),
    }
}
// ─── Turn layer ─────────────────────────────────────────────────────────────

/// AC1/AC7/AC9/AC11: a successful Rewind leaves the conversation target and
/// the Local Workspace agreeing on the target Turn, moves both refs in order,
/// and records a durable audit row that carries revisions but no content.
#[tokio::test]
async fn rewind_aligns_the_conversation_target_and_the_local_workspace() {
    let ws = setup_turn_workspace(&["mutate.create", "mutate.edit", "observe.workspace"]);
    let scoped = ScopedState::new(&ws.root);
    let sessions_dir = TempDir::new().unwrap();
    let sessions = sessions_dir.path().join("sessions");
    let session_id = new_session(&sessions, &ws.root);

    // Keep one real provider -> runtime -> history -> Session product path;
    // the remaining Rewind cases seed only the durable domain facts they test.
    let first =
        turn_creating_through_product(&ws, &sessions, &session_id, "notes.md", "v1\n").await;
    let _second = turn_editing_through_product(
        &ws,
        &sessions,
        &session_id,
        "notes.md",
        &content_hash(b"v1\n"),
        "v1",
        "v2",
    )
    .await;
    let third =
        turn_creating_through_product(&ws, &sessions, &session_id, "extra.md", "extra\n").await;

    // External change LATTICE never touched.
    std::fs::write(ws.root.join("untouched.txt"), "mine\n").unwrap();

    let history = history_for(scoped.path(), &ws.root);
    let tip_before = history.current().unwrap().expect("mutations recorded");
    let api = open_session_store(&sessions);
    let target = api.get_turn_commit(&session_id, &first).unwrap().unwrap();
    let target_turn_id = target.turn_id;
    let target_oid = target
        .workspace_history_checkpoint
        .as_oid()
        .map(str::to_owned)
        .expect("turn 1 recorded an identity");

    let outcome =
        lattice_api::rewind_to_turn(&api, &session_id, &first, &ws.root).expect("rewind runs");
    let RewindOutcome::Success(success) = outcome else {
        panic!("expected success, got {outcome:?}");
    };

    // Conversation target.
    assert_eq!(success.target_commit_id, first);
    assert_eq!(success.session_head.new_commit_id, first);
    assert_eq!(
        success.session_head.previous_commit_id.as_deref(),
        Some(third.as_str())
    );
    let status = api.status(&session_id).expect("status");
    assert_eq!(status.current_commit_id.as_deref(), Some(first.as_str()));
    assert_eq!(
        api.log(&session_id).expect("log").len(),
        1,
        "the conversation target is the first Turn again"
    );

    // Local Workspace.
    assert_eq!(std::fs::read(ws.root.join("notes.md")).unwrap(), b"v1\n");
    assert!(!ws.root.join("extra.md").exists());
    assert_eq!(
        std::fs::read(ws.root.join("untouched.txt")).unwrap(),
        b"mine\n"
    );
    assert_eq!(
        std::fs::read(ws.root.join("secret.env")).unwrap(),
        b"TOKEN=abc\n",
        "ignored content survives"
    );

    // Private history ref.
    assert_eq!(
        history.current().unwrap().as_deref(),
        Some(target_oid.as_str())
    );
    assert_eq!(
        success.from_history_oid.as_deref(),
        Some(tip_before.as_str())
    );
    assert_eq!(success.to_history_oid.as_deref(), Some(target_oid.as_str()));

    // The abandoned head keeps an internal ref.
    let pinned = success
        .session_head
        .pinned_ref
        .as_deref()
        .expect("the abandoned head is pinned");
    assert!(
        api.get_turn_commit(&session_id, &third).unwrap().is_some(),
        "the rewound-away Turn stays resolvable"
    );
    assert!(
        api.status(&session_id)
            .unwrap()
            .branches
            .iter()
            .any(|b| b.name == pinned),
        "the pin is a real internal ref"
    );

    // Durable audit.
    let page = rewind_page(&ws.root, &success.session_id);
    assert!(!page.has_older, "one audit record is a complete page");
    let records = page.records;
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!(record.outcome, "success");
    assert_eq!(record.target_turn_id, target_turn_id);
    assert_eq!(record.target_commit_id, first);
    let recorded_session_head: lattice_api::SessionHeadMove = serde_json::from_str(
        record
            .session_head_json
            .as_deref()
            .expect("success stores exact SessionHeadMove replay data"),
    )
    .expect("strict SessionHeadMove payload");
    assert_eq!(recorded_session_head, success.session_head);
    assert_eq!(
        record.target_history_oid.as_deref(),
        Some(target_oid.as_str())
    );
    assert_eq!(
        record.from_history_oid.as_deref(),
        Some(tip_before.as_str())
    );
    assert!(record.steps_json.contains("notes.md"));
    assert!(record.steps_json.contains("extra.md"));
    assert!(
        record.preflight_json.contains("blake3:"),
        "preflight revisions are recorded: {}",
        record.preflight_json
    );
    for field in [
        &record.steps_json,
        &record.preflight_json,
        record.restored_json.as_ref().expect("restored recorded"),
    ] {
        assert!(
            !field.contains("v2") && !field.contains("extra\\n") && !field.contains("TOKEN"),
            "audit records must not carry file content: {field}"
        );
    }

    // Repeating the completed command is an exact replay: it neither moves refs nor
    // appends another audit record, and returns the original public success payload.
    let replayed = lattice_api::rewind_to_turn(&api, &session_id, &first, &ws.root)
        .expect("completed Rewind retries idempotently");
    assert_eq!(replayed, RewindOutcome::Success(success.clone()));
    assert_eq!(rewind_records(&ws.root, &success.session_id).len(), 1);
    assert_eq!(
        api.status(&session_id)
            .unwrap()
            .branches
            .iter()
            .filter(|branch| branch.name == pinned)
            .count(),
        1,
        "completed retry must not duplicate the abandoned-head pin"
    );
}

#[tokio::test]
async fn completed_rewind_appends_once_after_an_unrelated_prior_rewind() {
    let ws = setup_turn_workspace(&["mutate.create"]);
    let _scoped = ScopedState::new(&ws.root);
    let sessions_dir = TempDir::new().unwrap();
    let sessions = sessions_dir.path().join("sessions");
    let session_id = new_session(&sessions, &ws.root);
    let first = turn_creating(&ws, &sessions, &session_id, "one.md", "one\n").await;
    let second = turn_creating(&ws, &sessions, &session_id, "two.md", "two\n").await;
    turn_creating(&ws, &sessions, &session_id, "three.md", "three\n").await;
    let api = open_session_store(&sessions);

    lattice_api::rewind_to_turn(&api, &session_id, &second, &ws.root)
        .expect("prior Rewind succeeds");
    turn_creating(&ws, &sessions, &session_id, "four.md", "four\n").await;

    let stop_before_audit =
        |_: &lattice_api::RewindBarrierFacts| panic!("simulate a crash after Session movement");
    let barriers = lattice_api::RewindBarriers {
        after_session_ref_move: Some(&stop_before_audit),
        ..lattice_api::RewindBarriers::none()
    };
    let interrupted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        lattice_api::rewind_to_turn_with_barriers(&api, &session_id, &first, &ws.root, &barriers)
    }));
    assert!(
        interrupted.is_err(),
        "the crash barrier must interrupt Rewind"
    );

    let before_recovery = rewind_records(&ws.root, &session_id);
    assert_eq!(before_recovery.len(), 1);
    assert_eq!(before_recovery[0].target_commit_id, second);
    assert_eq!(
        api.status(&session_id)
            .unwrap()
            .current_commit_id
            .as_deref(),
        Some(first.as_str())
    );
    assert!(!ws.root.join("two.md").exists());
    assert!(!ws.root.join("four.md").exists());

    let closed = lattice_api::rewind_to_turn(&api, &session_id, &first, &ws.root)
        .expect("recovery appends the missing success");
    let RewindOutcome::Success(success) = &closed else {
        panic!("expected no-op success closure, got {closed:?}");
    };
    assert!(success.steps.is_empty());
    assert!(success.preflight.is_empty());
    assert!(success.restored.is_empty());

    let after_recovery = rewind_records(&ws.root, &session_id);
    assert_eq!(after_recovery.len(), 2);
    assert_eq!(after_recovery[0].outcome, "success");
    assert_eq!(after_recovery[0].target_commit_id, first);
    assert_eq!(after_recovery[1].target_commit_id, second);

    let replayed = lattice_api::rewind_to_turn(&api, &session_id, &first, &ws.root)
        .expect("repeat strictly replays the appended success");
    assert_eq!(replayed, closed);
    assert_eq!(rewind_records(&ws.root, &session_id).len(), 2);
}

/// AC4/AC11: a conflicting Rewind moves neither the conversation target nor
/// the Local Workspace, and still leaves a durable conflict record.
#[tokio::test]
async fn a_conflicting_rewind_moves_neither_the_conversation_nor_the_workspace() {
    let ws = setup_turn_workspace(&["mutate.create", "mutate.edit"]);
    let scoped = ScopedState::new(&ws.root);
    let sessions_dir = TempDir::new().unwrap();
    let sessions = sessions_dir.path().join("sessions");
    let session_id = new_session(&sessions, &ws.root);

    let first = turn_creating(&ws, &sessions, &session_id, "doc.md", "one\n").await;
    let second = turn_editing(
        &ws,
        &sessions,
        &session_id,
        "doc.md",
        &content_hash(b"one\n"),
        "one",
        "two",
    )
    .await;

    // The user edits the LATTICE-touched path by hand.
    std::fs::write(ws.root.join("doc.md"), "hand written\n").unwrap();

    let history = history_for(scoped.path(), &ws.root);
    let tip_before = history.current().unwrap().expect("recorded");
    let api = open_session_store(&sessions);
    let head_before = api.status(&session_id).unwrap().current_commit_id;
    let workspace_before = tree_snapshot(&ws.root);

    let outcome =
        lattice_api::rewind_to_turn(&api, &session_id, &first, &ws.root).expect("rewind runs");
    let RewindOutcome::Conflict(conflict) = outcome else {
        panic!("expected a conflict, got {outcome:?}");
    };
    assert_eq!(conflict.conflicts.len(), 1);
    assert_eq!(conflict.conflicts[0].path, "doc.md");

    assert_eq!(
        tree_snapshot(&ws.root),
        workspace_before,
        "a conflicted rewind writes nothing"
    );
    assert_eq!(
        history.current().unwrap().as_deref(),
        Some(tip_before.as_str()),
        "the private history ref stays put"
    );
    let status = api.status(&session_id).unwrap();
    assert_eq!(status.current_commit_id, head_before);
    assert_eq!(status.current_commit_id.as_deref(), Some(second.as_str()));
    assert_eq!(
        api.log(&session_id).unwrap().len(),
        2,
        "the conversation target did not move"
    );

    let records = rewind_records(&ws.root, &conflict.session_id);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].outcome, "conflict");
    let conflicts_json = records[0]
        .conflicts_json
        .as_ref()
        .expect("conflict paths recorded");
    assert!(conflicts_json.contains("doc.md"));
    assert!(
        !conflicts_json.contains("hand written"),
        "no file content in audit: {conflicts_json}"
    );
}

/// AC1: nonexistent, foreign-session, and unreachable targets are refused
/// before anything is read or written.
#[tokio::test]
async fn rewind_refuses_unresolvable_targets() {
    let ws = setup_turn_workspace(&["mutate.create"]);
    let scoped = ScopedState::new(&ws.root);
    let sessions_dir = TempDir::new().unwrap();
    let sessions = sessions_dir.path().join("sessions");
    let session_id = new_session(&sessions, &ws.root);
    let other_session = new_session(&sessions, &ws.root);

    let first = turn_creating(&ws, &sessions, &session_id, "a.md", "a\n").await;
    let foreign = turn_creating(&ws, &sessions, &other_session, "b.md", "b\n").await;
    let api = open_session_store(&sessions);
    let history = history_for(scoped.path(), &ws.root);

    // Nonexistent.
    let err = lattice_api::rewind_to_turn(&api, &session_id, &"0".repeat(40), &ws.root)
        .expect_err("a nonexistent target must be refused");
    assert!(
        matches!(
            err,
            lattice_api::RewindError::Api(lattice_api::ApiError::NotFound(_))
        ),
        "got {err:?}"
    );

    // Another session's Turn.
    let err = lattice_api::rewind_to_turn(&api, &session_id, &foreign, &ws.root)
        .expect_err("another session's turn must be refused");
    assert!(
        matches!(
            err,
            lattice_api::RewindError::Api(lattice_api::ApiError::NotFound(_))
        ),
        "got {err:?}"
    );

    // Reachable-but-not-from-HEAD: branch off the first Turn, add a Turn on
    // the branch, then check out the branch and target the Turn that only
    // exists on the original branch.
    let second = turn_creating(&ws, &sessions, &session_id, "c.md", "c\n").await;
    let tip = history.current().unwrap();
    api.branch(&session_id, "side", Some(&first))
        .expect("branch");
    api.checkout(&session_id, "side").expect("checkout");
    let err = lattice_api::rewind_to_turn(&api, &session_id, &second, &ws.root)
        .expect_err("a target off the current HEAD must be refused");
    assert!(
        matches!(
            err,
            lattice_api::RewindError::Api(lattice_api::ApiError::BadRequest(_))
        ),
        "got {err:?}"
    );

    assert_eq!(
        history.current().unwrap(),
        tip,
        "a refused target must not move the private history ref"
    );
    assert!(
        rewind_records(&ws.root, &session_id).is_empty(),
        "a refused target writes no audit record"
    );
}

/// AC10: a Rewind driven through a freshly opened session store, on a
/// non-default branch, and from a detached HEAD all behave.
#[tokio::test]
async fn rewind_works_after_reopen_on_a_branch_and_from_a_detached_head() {
    let ws = setup_turn_workspace(&["mutate.create", "mutate.edit"]);
    let scoped = ScopedState::new(&ws.root);
    let sessions_dir = TempDir::new().unwrap();
    let sessions = sessions_dir.path().join("sessions");
    let session_id = new_session(&sessions, &ws.root);

    // Branch case: work on a non-default branch and rewind there.
    let base = turn_creating(&ws, &sessions, &session_id, "b1.md", "1\n").await;
    open_session_store(&sessions)
        .branch(&session_id, "work", Some(&base))
        .expect("branch");
    open_session_store(&sessions)
        .checkout(&session_id, "work")
        .expect("checkout");
    let on_branch = turn_creating(&ws, &sessions, &session_id, "b2.md", "2\n").await;

    // Session reopen: resolve the session through `open_session` on a store
    // handle that has never seen this session before, and drive the Rewind
    // through that handle by its *prefix* — nothing may depend on state the
    // turn-running handles held.
    let reopened = open_session_store(&sessions);
    let opened = reopened
        .open_session(lattice_api::OpenSessionRequest {
            id: session_id.clone(),
        })
        .expect("reopen the session");
    assert_eq!(opened.session_id, session_id);
    let prefix = &session_id[..8];
    let outcome = lattice_api::rewind_to_turn(&reopened, prefix, &base, &ws.root)
        .expect("rewind on a branch runs");
    let RewindOutcome::Success(success) = outcome else {
        panic!("expected success, got {outcome:?}");
    };
    // The prefix must have resolved to the full session id inside
    // `rewind_to_turn` — the audit row is keyed by it, so a leaked prefix
    // would make the record unfindable.
    assert_eq!(success.session_id, session_id);
    assert_eq!(
        rewind_records(&ws.root, &success.session_id).len(),
        1,
        "the reopened rewind is recorded under the resolved session id"
    );
    assert_eq!(success.session_head.branch.as_deref(), Some("work"));
    assert_eq!(
        success.session_head.previous_commit_id.as_deref(),
        Some(on_branch.as_str())
    );
    assert!(!ws.root.join("b2.md").exists());
    assert_eq!(std::fs::read(ws.root.join("b1.md")).unwrap(), b"1\n");
    // The branch itself really moved, not just the returned report.
    let branch_status = reopened.status(&session_id).expect("status on the branch");
    assert!(matches!(
        branch_status.head,
        GraphHead::Branch { ref name } if name == "work"
    ));
    assert_eq!(
        branch_status.current_commit_id.as_deref(),
        Some(base.as_str())
    );
    assert_eq!(
        reopened.log(&session_id).expect("log on the branch").len(),
        1,
        "the branch's conversation target is back at the base Turn"
    );

    // Detached HEAD case: add another Turn, detach onto it, then rewind.
    let next = turn_creating(&ws, &sessions, &session_id, "b3.md", "3\n").await;
    let api = open_session_store(&sessions);
    api.checkout(&session_id, &next).expect("detach");
    assert!(matches!(
        api.status(&session_id).unwrap().head,
        GraphHead::Detached { .. }
    ));

    let outcome = lattice_api::rewind_to_turn(&api, &session_id, &base, &ws.root)
        .expect("rewind from a detached HEAD runs");
    let RewindOutcome::Success(success) = outcome else {
        panic!("expected success, got {outcome:?}");
    };
    assert_eq!(success.session_head.branch, None);
    assert_eq!(success.session_head.new_commit_id, base);
    assert!(matches!(
        api.status(&session_id).unwrap().head,
        GraphHead::Detached { commit_id } if commit_id == base
    ));
    assert!(!ws.root.join("b3.md").exists());

    let history = history_for(scoped.path(), &ws.root);
    let base_oid = api
        .get_turn_commit(&session_id, &base)
        .unwrap()
        .unwrap()
        .workspace_history_checkpoint
        .as_oid()
        .map(str::to_owned);
    assert_eq!(history.current().unwrap(), base_oid);
}

/// A store whose Session History ref refuses to move, so the ordering
/// guarantee can be exercised end to end on both the branch and the detached
/// arm. `allow_checkout` lets a test detach first and only then arm the
/// failure.
struct FailingResetStore {
    inner: GitSessionGraphStore,
    allow_checkout: std::sync::atomic::AtomicBool,
}

impl FailingResetStore {
    fn new(_dir: &Path) -> Self {
        let layout = test_state_layout();
        Self {
            inner: GitSessionGraphStore::open(layout.lattice_root().join("sessions.git"))
                .expect("open session store"),
            allow_checkout: std::sync::atomic::AtomicBool::new(true),
        }
    }
}

impl SessionGraphStore for FailingResetStore {
    fn create_session(
        &self,
        session_id: &str,
        label: Option<&str>,
        active_agent_name: &str,
        project_target_branch: &str,
        project_store_key: &lattice_session::ProjectStoreKey,
    ) -> lattice_session::Result<GraphSession> {
        self.inner.create_session(
            session_id,
            label,
            active_agent_name,
            project_target_branch,
            project_store_key,
        )
    }
    fn lock_execution(
        &self,
        session_id: &str,
        mode: lattice_session::LockMode,
        blocking: lattice_session::Blocking,
    ) -> lattice_session::Result<lattice_session::FlockGuard> {
        self.inner.lock_execution(session_id, mode, blocking)
    }
    fn open_session(&self, session_id: &str) -> lattice_session::Result<GraphSession> {
        self.inner.open_session(session_id)
    }
    fn status(&self, session_id: &str) -> lattice_session::Result<SessionStatus> {
        self.inner.status(session_id)
    }
    fn status_locked(
        &self,
        session_id: &str,
        execution_lock: &lattice_session::FlockGuard,
    ) -> lattice_session::Result<SessionStatus> {
        self.inner.status_locked(session_id, execution_lock)
    }
    fn list(&self) -> lattice_session::Result<Vec<SessionGraphSummary>> {
        self.inner.list()
    }
    fn log(&self, session_id: &str) -> lattice_session::Result<Vec<TurnLogEntry>> {
        self.inner.log(session_id)
    }
    fn head_transcript(&self, session_id: &str) -> lattice_session::Result<HeadTranscript> {
        self.inner.head_transcript(session_id)
    }
    fn test_commit_turn(&self, input: TurnCommitInput) -> lattice_session::Result<TurnCommit> {
        self.inner.test_commit_turn(input)
    }
    fn test_commit_turn_locked(
        &self,
        input: TurnCommitInput,
        execution_lock: &lattice_session::FlockGuard,
    ) -> lattice_session::Result<TurnCommit> {
        self.inner.test_commit_turn_locked(input, execution_lock)
    }
    fn checkout(&self, session_id: &str, target: &str) -> lattice_session::Result<GraphHead> {
        if self
            .allow_checkout
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            self.inner.checkout(session_id, target)
        } else {
            Err(SessionError::Git(
                "injected detached session ref durability failure".to_string(),
            ))
        }
    }
    fn checkout_locked(
        &self,
        session_id: &str,
        target: &str,
        execution_lock: &lattice_session::FlockGuard,
    ) -> lattice_session::Result<GraphHead> {
        if self
            .allow_checkout
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            self.inner
                .checkout_locked(session_id, target, execution_lock)
        } else {
            Err(SessionError::Git(
                "injected detached session ref durability failure".to_string(),
            ))
        }
    }
    fn branch(
        &self,
        session_id: &str,
        name: &str,
        from: Option<&str>,
    ) -> lattice_session::Result<GraphRef> {
        self.inner.branch(session_id, name, from)
    }
    fn branch_locked(
        &self,
        session_id: &str,
        name: &str,
        from: Option<&str>,
        execution_lock: &lattice_session::FlockGuard,
    ) -> lattice_session::Result<GraphRef> {
        self.inner
            .branch_locked(session_id, name, from, execution_lock)
    }
    fn set_active_agent(
        &self,
        session_id: &str,
        agent_name: &str,
    ) -> lattice_session::Result<GraphSession> {
        self.inner.set_active_agent(session_id, agent_name)
    }
    fn diff(&self, session_id: &str, from: &str, to: &str) -> lattice_session::Result<GraphDiff> {
        self.inner.diff(session_id, from, to)
    }
    fn reset(&self, _session_id: &str, _target: &str) -> lattice_session::Result<ResetResult> {
        Err(SessionError::Git(
            "injected session ref durability failure".to_string(),
        ))
    }
    fn reset_locked(
        &self,
        _session_id: &str,
        _target: &str,
        _execution_lock: &lattice_session::FlockGuard,
    ) -> lattice_session::Result<ResetResult> {
        Err(SessionError::Git(
            "injected session ref durability failure".to_string(),
        ))
    }
    fn test_commit_prepared_turn(
        &self,
        prepared: PreparedTurnCommit,
    ) -> lattice_session::Result<TurnCommit> {
        self.inner.test_commit_prepared_turn(prepared)
    }
    fn get_turn_commit(
        &self,
        session_id: &str,
        commit_id: &str,
    ) -> lattice_session::Result<Option<TurnCommit>> {
        self.inner.get_turn_commit(session_id, commit_id)
    }
}

/// AC7/AC10: branch and detached Session ref failures after workspace restore
/// are both recorded and never reported as success.
#[tokio::test]
async fn session_ref_failures_are_not_reported_as_success() {
    let ws = setup_turn_workspace(&["mutate.create"]);
    let scoped = ScopedState::new(&ws.root);
    let sessions_dir = TempDir::new().unwrap();
    let sessions = sessions_dir.path().join("sessions");
    let session_id = new_session(&sessions, &ws.root);

    let first = turn_creating(&ws, &sessions, &session_id, "one.md", "1\n").await;
    let second = turn_creating(&ws, &sessions, &session_id, "two.md", "2\n").await;

    let failing = SessionApi::new(Box::new(FailingResetStore::new(&sessions)))
        .with_state_layout(test_state_layout());
    let err = lattice_api::rewind_to_turn(&failing, &session_id, &first, &ws.root)
        .expect_err("a session ref failure must surface");
    assert!(
        matches!(
            err,
            lattice_api::RewindError::Api(lattice_api::ApiError::Internal(_))
        ),
        "got {err:?}"
    );
    assert!(
        err.to_string().contains("did not move"),
        "the error must name the partial state: {err}"
    );

    // The workspace half really did happen — that is why this must not be
    // reported as success.
    assert!(!ws.root.join("two.md").exists());
    let history = history_for(scoped.path(), &ws.root);
    let target_oid = open_session_store(&sessions)
        .get_turn_commit(&session_id, &first)
        .unwrap()
        .unwrap()
        .workspace_history_checkpoint
        .as_oid()
        .map(str::to_owned);
    assert_eq!(history.current().unwrap(), target_oid);

    // The session head did not move, and the failure is durably recorded.
    let status = open_session_store(&sessions).status(&session_id).unwrap();
    assert_eq!(status.current_commit_id.as_deref(), Some(second.as_str()));
    let records = rewind_records(&ws.root, &status.session_id);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].outcome, "session_ref_failure");
    assert!(
        records[0]
            .restored_json
            .as_ref()
            .expect("restored paths recorded")
            .contains("two.md")
    );

    // Reuse the split state: append one later mutation, detach onto that Turn,
    // then prove the detached checkout arm has the same partial-state contract.
    let third = turn_creating(&ws, &sessions, &session_id, "three.md", "3\n").await;
    let store = FailingResetStore::new(&sessions);
    let detached_failing = {
        let api = open_session_store(&sessions);
        api.checkout(&session_id, &third).expect("detach");
        store
            .allow_checkout
            .store(false, std::sync::atomic::Ordering::SeqCst);
        SessionApi::new(Box::new(store)).with_state_layout(test_state_layout())
    };
    let err = lattice_api::rewind_to_turn(&detached_failing, &session_id, &first, &ws.root)
        .expect_err("a detached ref failure must surface");
    assert!(
        err.to_string()
            .contains("detached session HEAD did not move"),
        "the error must name the partial state: {err}"
    );
    assert!(!ws.root.join("three.md").exists());
    assert_eq!(history.current().unwrap(), target_oid);
    let status = open_session_store(&sessions).status(&session_id).unwrap();
    assert!(matches!(
        status.head,
        GraphHead::Detached { ref commit_id } if commit_id == &third
    ));
    let records = rewind_records(&ws.root, &status.session_id);
    assert_eq!(records.len(), 2);
    assert!(
        records
            .iter()
            .all(|record| record.outcome == "session_ref_failure")
    );
}

/// AC2 defence: a Turn whose workspace-history identity is unavailable is not
/// silently treated as "before any mutation" — that would reverse mutations
/// installed *before* the target.
#[tokio::test]
async fn a_turn_without_a_history_identity_is_refused_when_an_ancestor_has_one() {
    let ws = setup_turn_workspace(&["mutate.create"]);
    let scoped = ScopedState::new(&ws.root);
    let sessions_dir = TempDir::new().unwrap();
    let sessions = sessions_dir.path().join("sessions");
    let session_id = new_session(&sessions, &ws.root);

    let first = turn_creating(&ws, &sessions, &session_id, "kept.md", "keep\n").await;
    // A Turn stamped without an identity, exactly as the live path does when
    // reading the identity fails.
    let api = open_session_store(&sessions);
    let store = GitSessionGraphStore::open(test_state_layout().lattice_root().join("sessions.git"))
        .expect("open session store");
    let head = api.status(&session_id).unwrap();
    let anomalous = store
        .test_commit_turn(TurnCommitInput {
            turn_id: lancet_protocol::TurnId::parse("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa")
                .expect("valid test TurnId"),
            workspace_history_checkpoint: lattice_session::WorkspaceHistoryCheckpoint::Empty,
            session_id: head.session_id.clone(),
            parent_ids: vec![first.clone()],
            messages: vec![
                lattice_session::test_helpers::user_msg("anomalous"),
                lattice_session::test_helpers::assistant_msg("no identity"),
            ],
            agent_name: "default".to_string(),
            profile_snapshot_hash: "test-profile".to_string(),
            profile_snapshot_json: "{}".to_string(),
            model: "sonnet".to_string(),
            provider: "anthropic".to_string(),
            outcome: lattice_session::TurnOutcome::Completed {
                finish_reason: lattice_session::TurnFinishReason::Stop,
            },
            origin: lattice_session::ClosureOrigin::Live,
            commit_message: "anomalous turn".to_string(),
            usage: None,
        })
        .expect("commit anomalous turn");

    let history = history_for(scoped.path(), &ws.root);
    let tip = history.current().unwrap();
    let err = lattice_api::rewind_to_turn(&api, &session_id, &anomalous.commit_id, &ws.root)
        .expect_err("an unavailable identity must be refused");
    assert!(
        matches!(
            err,
            lattice_api::RewindError::Api(lattice_api::ApiError::Conflict(_))
        ),
        "got {err:?}"
    );
    assert_eq!(
        history.current().unwrap(),
        tip,
        "the refusal must not move the private history ref"
    );
    assert_eq!(
        std::fs::read(ws.root.join("kept.md")).unwrap(),
        b"keep\n",
        "the earlier Turn's mutation must not be reversed"
    );
}

/// The private history is per project, not per session: a Rewind restores the
/// Local Workspace to the target state, so a mutation another session
/// installed in the reversed range is reversed too. That is the only
/// workspace-consistent choice — and the durable record identifies each
/// reversed step by the Turn that installed it.
#[tokio::test]
async fn a_rewind_reverses_in_range_mutations_from_every_session() {
    let ws = setup_turn_workspace(&["mutate.create"]);
    let scoped = ScopedState::new(&ws.root);
    let sessions_dir = TempDir::new().unwrap();
    let sessions = sessions_dir.path().join("sessions");
    let session_a = new_session(&sessions, &ws.root);
    let session_b = new_session(&sessions, &ws.root);

    let target = turn_creating(&ws, &sessions, &session_a, "a-owned.md", "a\n").await;
    // Another session mutates the same Local Workspace afterwards.
    let session_b_commit = turn_creating(&ws, &sessions, &session_b, "b-owned.md", "b\n").await;

    let api = open_session_store(&sessions);
    let outcome =
        lattice_api::rewind_to_turn(&api, &session_a, &target, &ws.root).expect("rewind runs");
    let RewindOutcome::Success(success) = outcome else {
        panic!("expected success, got {outcome:?}");
    };

    assert_eq!(success.steps.len(), 1);
    assert_eq!(success.steps[0].path, "b-owned.md");
    assert!(
        !ws.root.join("b-owned.md").exists(),
        "an in-range mutation from another session is reversed"
    );
    assert_eq!(std::fs::read(ws.root.join("a-owned.md")).unwrap(), b"a\n");
    let session_b_turn_id = api
        .get_turn_commit(&session_b, &session_b_commit)
        .expect("read Session B Turn")
        .expect("Session B Turn exists")
        .turn_id;
    assert_eq!(
        success.steps[0].turn_id, session_b_turn_id,
        "the reversed step names the typed Turn that installed it",
    );

    // Session B's own conversation target is untouched by A's rewind — only
    // the shared Local Workspace moved.
    assert_eq!(api.log(&session_b).unwrap().len(), 1);
    let records = rewind_records(&ws.root, &success.session_id);
    assert_eq!(records.len(), 1);
    assert!(
        records[0]
            .steps_json
            .contains(&session_b_turn_id.to_string())
    );
    let _ = scoped;
}

/// AC2 defence, chain shape: a Turn whose identity is unavailable must not be
/// honoured as "before any mutation" when the private history still holds a
/// mutation this session installed at or before it. The same-session ancestor
/// scan cannot see that on its own — every ancestor here is identity-less —
/// so only the check against the history chain itself catches it.
#[tokio::test]
async fn a_full_unwind_is_refused_when_this_session_installed_work_before_the_target() {
    let ws = setup_turn_workspace(&["mutate.create"]);
    let scoped = ScopedState::new(&ws.root);
    let sessions_dir = TempDir::new().unwrap();
    let sessions = sessions_dir.path().join("sessions");
    let session_id = new_session(&sessions, &ws.root);
    let api = open_session_store(&sessions);
    let resolved = api.status(&session_id).unwrap().session_id;
    let first_turn_id = lancet_protocol::TurnId::parse("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa")
        .expect("valid first TurnId");
    let target_turn_id = lancet_protocol::TurnId::parse("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb")
        .expect("valid target TurnId");

    // A mutation this session installed, recorded in the shared private
    // history under the turn id of its first Turn.
    std::fs::write(ws.root.join("early.md"), "early\n").unwrap();
    let history = history_for(scoped.path(), &ws.root);
    history
        .record(&lancet_runtime::MutationRecord {
            method: "mutate.create".into(),
            turn_id: first_turn_id,
            invocation_id: lancet_protocol::InvocationId::parse(
                "00000000-0000-4000-8000-000000001490",
            )
            .unwrap(),
            path: "early.md".into(),
            before: None,
            after: b"early\n".to_vec(),
            content_hash: content_hash(b"early\n"),
            recorded_at: 1_700_000_000,
        })
        .expect("record")
        .expect("history is enabled");
    let tip = history.current().unwrap();

    // Two Turns, both stamped with no identity — the shape a swallowed
    // end-of-turn identity read produces. The second is the rewind target,
    // and every one of its ancestors is identity-less.
    let store = GitSessionGraphStore::open(test_state_layout().lattice_root().join("sessions.git"))
        .expect("open session store");
    let mut parent: Vec<String> = Vec::new();
    let mut committed = Vec::new();
    for (label, turn_id) in [("first", first_turn_id), ("second", target_turn_id)] {
        let commit = store
            .test_commit_turn(TurnCommitInput {
                turn_id,
                workspace_history_checkpoint: lattice_session::WorkspaceHistoryCheckpoint::Empty,
                session_id: resolved.clone(),
                parent_ids: parent.clone(),
                messages: vec![
                    lattice_session::test_helpers::user_msg(label),
                    lattice_session::test_helpers::assistant_msg("no identity"),
                ],
                agent_name: "default".to_string(),
                profile_snapshot_hash: "test-profile".to_string(),
                profile_snapshot_json: "{}".to_string(),
                model: "sonnet".to_string(),
                provider: "anthropic".to_string(),
                outcome: lattice_session::TurnOutcome::Completed {
                    finish_reason: lattice_session::TurnFinishReason::Stop,
                },
                origin: lattice_session::ClosureOrigin::Live,
                commit_message: format!("{label} turn"),
                usage: None,
            })
            .expect("commit turn");
        parent = vec![commit.commit_id.clone()];
        committed.push(commit.commit_id);
    }
    let target = committed.last().unwrap().clone();

    let err = lattice_api::rewind_to_turn(&api, &session_id, &target, &ws.root)
        .expect_err("a full unwind over this session's own earlier work must be refused");
    assert!(
        matches!(
            err,
            lattice_api::RewindError::Api(lattice_api::ApiError::Conflict(_))
        ),
        "got {err:?}"
    );
    assert!(
        err.to_string().contains("early.md"),
        "the refusal names the work it would have reversed: {err}"
    );
    assert_eq!(
        history.current().unwrap(),
        tip,
        "the refusal must not move the private history ref"
    );
    assert_eq!(
        std::fs::read(ws.root.join("early.md")).unwrap(),
        b"early\n",
        "work installed before the target must not be reversed"
    );
}

/// AC9/AC10, restore-failure arm: a Rewind whose restore cannot run at all
/// still leaves a durable record naming the target and the planned steps.
/// A recorded path under a System Path Exclusion is the reachable trigger:
/// the history's own path check lets `.lattice/**` through, the restore
/// executor does not.
#[tokio::test]
async fn a_restore_failure_is_recorded_and_never_reported_as_success() {
    let ws = setup_turn_workspace(&["mutate.create"]);
    let scoped = ScopedState::new(&ws.root);
    let sessions_dir = TempDir::new().unwrap();
    let sessions = sessions_dir.path().join("sessions");
    let session_id = new_session(&sessions, &ws.root);

    let target = turn_creating(&ws, &sessions, &session_id, "kept.md", "keep\n").await;
    let api = open_session_store(&sessions);
    let resolved = api.status(&session_id).unwrap().session_id;

    // A mutation the restore executor must refuse to reverse.
    let history = history_for(scoped.path(), &ws.root);
    history
        .record(&lancet_runtime::MutationRecord {
            method: "mutate.create".into(),
            turn_id: lancet_protocol::TurnId::parse("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb")
                .expect("valid test TurnId"),
            invocation_id: lancet_protocol::InvocationId::parse(
                "00000000-0000-4000-8000-000000001636",
            )
            .unwrap(),
            path: ".lattice/private.toml".into(),
            before: None,
            after: b"x\n".to_vec(),
            content_hash: content_hash(b"x\n"),
            recorded_at: 1_700_000_100,
        })
        .expect("record")
        .expect("history is enabled");
    let tip = history.current().unwrap();

    let err = lattice_api::rewind_to_turn(&api, &session_id, &target, &ws.root)
        .expect_err("a restore that cannot run must surface");
    assert!(
        matches!(
            err,
            lattice_api::RewindError::Api(lattice_api::ApiError::Internal(_))
        ),
        "got {err:?}"
    );

    assert_eq!(
        history.current().unwrap(),
        tip,
        "a refused restore must not move the private history ref"
    );
    assert_eq!(std::fs::read(ws.root.join("kept.md")).unwrap(), b"keep\n");
    let records = rewind_records(&ws.root, &resolved);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].outcome, "restore_failure");
    assert!(records[0].steps_json.contains(".lattice/private.toml"));
}

/// The full-unwind guard must refuse only what is unsafe. A Turn that really
/// does predate every recorded mutation is the legitimate case, and rewinding
/// to it must unwind the whole chain — otherwise the guard would be a blanket
/// refusal and the refusal tests above would prove nothing.
#[tokio::test]
async fn a_legitimate_full_unwind_still_succeeds() {
    let ws = setup_turn_workspace(&["mutate.create"]);
    let scoped = ScopedState::new(&ws.root);
    let sessions_dir = TempDir::new().unwrap();
    let sessions = sessions_dir.path().join("sessions");
    let session_id = new_session(&sessions, &ws.root);
    let api = open_session_store(&sessions);
    let resolved = api.status(&session_id).unwrap().session_id;

    // A first Turn that installs nothing: the history is still empty, so it
    // legitimately records no identity.
    let store = GitSessionGraphStore::open(test_state_layout().lattice_root().join("sessions.git"))
        .expect("open session store");
    let target = store
        .test_commit_turn(TurnCommitInput {
            turn_id: lancet_protocol::TurnId::parse("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa")
                .expect("valid test TurnId"),
            workspace_history_checkpoint: lattice_session::WorkspaceHistoryCheckpoint::Empty,
            session_id: resolved.clone(),
            parent_ids: Vec::new(),
            messages: vec![
                lattice_session::test_helpers::user_msg("just talk"),
                lattice_session::test_helpers::assistant_msg("nothing to do"),
            ],
            agent_name: "default".to_string(),
            profile_snapshot_hash: "test-profile".to_string(),
            profile_snapshot_json: "{}".to_string(),
            model: "sonnet".to_string(),
            provider: "anthropic".to_string(),
            outcome: lattice_session::TurnOutcome::Completed {
                finish_reason: lattice_session::TurnFinishReason::Stop,
            },
            origin: lattice_session::ClosureOrigin::Live,
            commit_message: "chat only".to_string(),
            usage: None,
        })
        .expect("commit chat-only turn");
    assert!(
        history_for(scoped.path(), &ws.root)
            .current()
            .unwrap()
            .is_none(),
        "the history must still be empty at the target Turn"
    );

    // Only afterwards does a Turn install something.
    turn_creating(&ws, &sessions, &session_id, "after.md", "after\n").await;
    assert!(ws.root.join("after.md").exists());

    let outcome = lattice_api::rewind_to_turn(&api, &session_id, &target.commit_id, &ws.root)
        .expect("a legitimate full unwind must be allowed");
    let RewindOutcome::Success(success) = outcome else {
        panic!("expected success, got {outcome:?}");
    };
    assert_eq!(
        success.to_history_oid, None,
        "the chain is unwound entirely"
    );
    assert_eq!(success.steps.len(), 1);
    assert!(
        !ws.root.join("after.md").exists(),
        "the later mutation is reversed"
    );
    assert_eq!(
        history_for(scoped.path(), &ws.root).current().unwrap(),
        None,
        "the private history ref is deleted for an empty target"
    );
    assert_eq!(api.log(&session_id).unwrap().len(), 1);
}

/// Partial state reports fresh authoritative refs both before movement (a
/// conflict) and when history-ref publication itself fails.
#[tokio::test]
async fn partial_state_distinguishes_planned_and_observed_history_refs() {
    let ws = setup_turn_workspace(&["mutate.create"]);
    let scoped = ScopedState::new(&ws.root);
    let sessions_dir = TempDir::new().unwrap();
    let sessions = sessions_dir.path().join("sessions");
    let session_id = new_session(&sessions, &ws.root);
    let first = turn_creating(&ws, &sessions, &session_id, "base.md", "base\n").await;
    let second = turn_creating(&ws, &sessions, &session_id, "later.md", "later\n").await;
    std::fs::write(ws.root.join("later.md"), "external\n").unwrap();

    let api = open_session_store(&sessions);
    let target = api
        .get_turn_commit(&session_id, &first)
        .unwrap()
        .expect("target Turn");
    let attempted_target = target
        .workspace_history_checkpoint
        .as_oid()
        .expect("target history oid")
        .to_string();
    let started_from = history_for(scoped.path(), &ws.root)
        .current()
        .unwrap()
        .expect("history tip");
    fail_audit_ref_fsync(&ws.root, &session_id);

    let error = lattice_api::rewind_to_turn(&api, &session_id, &first, &ws.root)
        .expect_err("audit publication failure must be typed");
    let partial = expect_audit_publication_failure(error);
    assert_eq!(partial.session_id.to_string(), session_id);
    assert_eq!(partial.target_turn_id, target.turn_id);
    assert_eq!(partial.target_commit_id, first);
    assert_eq!(partial.outcome, "conflict");
    assert_eq!(partial.started_from_history_oid, Some(started_from.clone()));
    assert_eq!(
        partial.attempted_target_history_oid,
        Some(attempted_target.clone())
    );
    assert_eq!(
        partial.actual_workspace_history_oid,
        lattice_api::RewindRefObservation::Present {
            value: started_from.clone()
        }
    );
    assert_eq!(
        partial.actual_session_head_commit_id,
        lattice_api::RewindRefObservation::Present {
            value: second.clone()
        }
    );
    assert_eq!(
        partial.audit_record_observation,
        lattice_api::RewindAuditRecordObservation::VisibleDurabilityUnconfirmed
    );
    assert_eq!(partial.conflicts.len(), 1);
    assert_eq!(
        std::fs::read(ws.root.join("later.md")).unwrap(),
        b"external\n"
    );

    // Restore the expected post-mutation bytes and reuse the same durable
    // state for the history-ref -> audit partial window.
    std::fs::write(ws.root.join("later.md"), "later\n").unwrap();
    let fail_audit_after_history_ref = |_facts: &lattice_api::RewindBarrierFacts| {
        fail_audit_ref_fsync(&ws.root, &session_id);
    };
    let barriers = lattice_api::RewindBarriers {
        after_workspace_half: Some(&fail_audit_after_history_ref),
        ..lattice_api::RewindBarriers::none()
    };
    fail_history_current_ref_fsync(&ws.root);
    let error =
        lattice_api::rewind_to_turn_with_barriers(&api, &session_id, &first, &ws.root, &barriers)
            .expect_err("history and audit publication failures must be typed");
    let partial = expect_audit_publication_failure(error);
    let actual_history = history_for(scoped.path(), &ws.root).current().unwrap();
    let expected_history_observation = actual_history
        .map_or(lattice_api::RewindRefObservation::Empty, |value| {
            lattice_api::RewindRefObservation::Present { value }
        });

    assert_eq!(partial.target_turn_id, target.turn_id);
    assert_eq!(partial.outcome, "history_ref_failure");
    assert_eq!(partial.started_from_history_oid, Some(started_from));
    assert_eq!(partial.attempted_target_history_oid, Some(attempted_target));
    assert_eq!(
        partial.actual_workspace_history_oid,
        expected_history_observation
    );
    assert_eq!(
        partial.actual_session_head_commit_id,
        lattice_api::RewindRefObservation::Present { value: second }
    );
    assert_eq!(
        partial.audit_record_observation,
        lattice_api::RewindAuditRecordObservation::VisibleDurabilityUnconfirmed
    );
    assert_eq!(partial.restored.len(), 1);
}

/// Session-ref failure skips Session publication but follows a completed
/// workspace-history rewind. Fresh reads must expose that split state.
#[tokio::test]
async fn session_ref_and_audit_failures_report_split_authoritative_state() {
    let ws = setup_turn_workspace(&["mutate.create"]);
    let scoped = ScopedState::new(&ws.root);
    let sessions_dir = TempDir::new().unwrap();
    let sessions = sessions_dir.path().join("sessions");
    let session_id = new_session(&sessions, &ws.root);
    let first = turn_creating(&ws, &sessions, &session_id, "one.md", "one\n").await;
    let second = turn_creating(&ws, &sessions, &session_id, "two.md", "two\n").await;
    let target = open_session_store(&sessions)
        .get_turn_commit(&session_id, &first)
        .unwrap()
        .expect("target Turn");
    let target_history = target
        .workspace_history_checkpoint
        .as_oid()
        .expect("target history oid")
        .to_string();
    let started_from = history_for(scoped.path(), &ws.root)
        .current()
        .unwrap()
        .expect("history tip");
    let failing = SessionApi::new(Box::new(FailingResetStore::new(&sessions)))
        .with_state_layout(test_state_layout());
    fail_audit_ref_fsync(&ws.root, &session_id);

    let error = lattice_api::rewind_to_turn(&failing, &session_id, &first, &ws.root)
        .expect_err("Session and audit publication failures must be typed");
    let partial = expect_audit_publication_failure(error);
    assert_eq!(partial.target_turn_id, target.turn_id);
    assert_eq!(partial.outcome, "session_ref_failure");
    assert_eq!(partial.started_from_history_oid, Some(started_from));
    assert_eq!(
        partial.attempted_target_history_oid,
        Some(target_history.clone())
    );
    assert_eq!(
        partial.actual_workspace_history_oid,
        lattice_api::RewindRefObservation::Present {
            value: target_history
        }
    );
    assert_eq!(
        partial.actual_session_head_commit_id,
        lattice_api::RewindRefObservation::Present { value: second }
    );
    assert_eq!(
        partial.audit_record_observation,
        lattice_api::RewindAuditRecordObservation::VisibleDurabilityUnconfirmed
    );
    assert_eq!(partial.restored.len(), 1);
}

/// Audit publication failures distinguish an absent record from a visible but
/// durability-unconfirmed record; both retry paths close exactly once.
#[tokio::test]
async fn audit_append_failures_retry_idempotently_from_observed_record_state() {
    let ws = setup_turn_workspace(&["mutate.create"]);
    let scoped = ScopedState::new(&ws.root);
    let sessions_dir = TempDir::new().unwrap();
    let sessions = sessions_dir.path().join("sessions");
    let session_id = new_session(&sessions, &ws.root);
    let first = turn_creating(&ws, &sessions, &session_id, "one.md", "one\n").await;
    let second = turn_creating(&ws, &sessions, &session_id, "two.md", "two\n").await;
    let api = open_session_store(&sessions);
    let target = api
        .get_turn_commit(&session_id, &first)
        .unwrap()
        .expect("target Turn");
    let target_history = target
        .workspace_history_checkpoint
        .as_oid()
        .expect("target history oid")
        .to_string();
    let started_from = history_for(scoped.path(), &ws.root)
        .current()
        .unwrap()
        .expect("history tip");
    let fail_before_audit_ref = |_facts: &lattice_api::RewindBarrierFacts| {
        lattice_store::fault::fail_object_fsync(true);
    };
    let barriers = lattice_api::RewindBarriers {
        after_session_ref_move: Some(&fail_before_audit_ref),
        ..lattice_api::RewindBarriers::none()
    };

    let error =
        lattice_api::rewind_to_turn_with_barriers(&api, &session_id, &first, &ws.root, &barriers)
            .expect_err("audit publication failure must be typed");
    let partial = expect_audit_publication_failure(error);
    assert_eq!(partial.target_turn_id, target.turn_id);
    assert_eq!(partial.target_commit_id, first);
    assert_eq!(partial.outcome, "success");
    assert_eq!(partial.started_from_history_oid, Some(started_from));
    assert_eq!(
        partial.attempted_target_history_oid,
        Some(target_history.clone())
    );
    assert_eq!(
        partial.actual_workspace_history_oid,
        lattice_api::RewindRefObservation::Present {
            value: target_history.clone()
        }
    );
    assert_eq!(
        partial.actual_session_head_commit_id,
        lattice_api::RewindRefObservation::Present {
            value: first.clone(),
        }
    );
    assert_eq!(
        partial.audit_record_observation,
        lattice_api::RewindAuditRecordObservation::NotVisible
    );
    assert_eq!(partial.restored.len(), 1);
    assert!(rewind_records(&ws.root, &session_id).is_empty());
    assert_ne!(second, partial.target_commit_id);
    let closed = lattice_api::rewind_to_turn(&api, &session_id, &first, &ws.root)
        .expect("retry closes the unaudited completed state");
    let RewindOutcome::Success(closed) = closed else {
        panic!("expected no-op success closure, got {closed:?}");
    };
    assert_eq!(
        closed.from_history_oid.as_deref(),
        Some(target_history.as_str())
    );
    assert_eq!(
        closed.to_history_oid.as_deref(),
        Some(target_history.as_str())
    );
    assert!(closed.steps.is_empty());
    assert!(closed.preflight.is_empty());
    assert!(closed.restored.is_empty());
    assert_eq!(
        closed.session_head.previous_commit_id.as_deref(),
        Some(first.as_str())
    );
    assert_eq!(closed.session_head.new_commit_id, first);
    assert!(closed.session_head.pinned_ref.is_none());
    assert_eq!(rewind_records(&ws.root, &session_id).len(), 1);

    let replayed = lattice_api::rewind_to_turn(&api, &session_id, &first, &ws.root)
        .expect("closed retry replays exactly");
    assert_eq!(replayed, RewindOutcome::Success(closed));
    assert_eq!(rewind_records(&ws.root, &session_id).len(), 1);

    // Reuse the now-aligned Session/workspace to exercise the later failure
    // window where the audit ref is visible but its fsync was not confirmed.
    let third = turn_creating(&ws, &sessions, &session_id, "three.md", "three\n").await;
    fail_audit_ref_fsync(&ws.root, &session_id);
    let error = lattice_api::rewind_to_turn(&api, &session_id, &first, &ws.root)
        .expect_err("audit ref fsync failure must not report success");
    let partial = expect_audit_publication_failure(error);
    assert_eq!(partial.outcome, "success");
    assert_eq!(
        partial.audit_record_observation,
        lattice_api::RewindAuditRecordObservation::VisibleDurabilityUnconfirmed
    );
    assert_eq!(rewind_records(&ws.root, &session_id).len(), 2);

    let replayed = lattice_api::rewind_to_turn(&api, &session_id, &first, &ws.root)
        .expect("retry recognizes the exact visible record without appending");
    let RewindOutcome::Success(replayed) = replayed else {
        panic!("expected replayed success, got {replayed:?}");
    };
    assert_eq!(replayed.target_commit_id, first);
    assert_eq!(
        replayed.session_head.previous_commit_id.as_deref(),
        Some(third.as_str())
    );
    assert!(!replayed.steps.is_empty());
    assert!(!replayed.restored.is_empty());
    assert_eq!(rewind_records(&ws.root, &session_id).len(), 2);
}

// ─── #139-B Commit 3: coordinator lock ownership, barriers, Git audit ──────

/// The deterministic barriers observe exactly the durable audit state at
/// every action → audit window, and the audit chain survives a later
/// mutation rewind (#139-B Commit 3).
#[tokio::test]
async fn barriers_observe_each_action_to_audit_window_and_the_chain_survives_later_rewinds() {
    let ws = setup_turn_workspace(&COMPONENT_TOOLS);
    let _scoped = ScopedState::new(&ws.root);
    let sessions_dir = TempDir::new().unwrap();
    let sessions = sessions_dir.path().join("sessions");
    let session_id = new_session(&sessions, &ws.root);
    let base = turn_creating(&ws, &sessions, &session_id, "base.md", "base\n").await;
    turn_creating(&ws, &sessions, &session_id, "later.md", "later\n").await;
    let api = open_session_store(&sessions);

    let operation_backing =
        lancet_api::workspace_history_backing(&ws.root).expect("operation backing");
    let typed_session_id =
        lancet_protocol::SessionId::parse(&session_id).expect("canonical SessionId");
    let target_turn_id = api
        .get_turn_commit(&session_id, &base)
        .expect("read target Turn")
        .expect("target Turn exists")
        .turn_id;
    let observed: std::sync::Arc<std::sync::Mutex<Vec<(String, usize, bool)>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorder = |label: &'static str, session_id: lancet_protocol::SessionId| {
        let observed = std::sync::Arc::clone(&observed);
        let backing = operation_backing.clone();
        move |facts: &lattice_api::RewindBarrierFacts| {
            assert_eq!(facts.session_id, session_id);
            assert_eq!(facts.target_turn_id, target_turn_id);
            assert!(
                WorkspaceHistory::open(backing.clone())
                    .expect("open history at barrier")
                    .lock_workspace_operation(
                        lancet_runtime::LockMode::Exclusive,
                        lancet_runtime::Blocking::NonBlocking,
                    )
                    .is_err(),
                "{label} barrier must still run under the exclusive operation hold"
            );
            observed.lock().unwrap().push((
                format!("{label}:{}", facts.outcome),
                facts.audit_page.records.len(),
                facts.audit_page.has_older,
            ));
        }
    };
    let workspace_barrier = recorder("workspace", typed_session_id);
    let session_barrier = recorder("session", typed_session_id);
    let audit_barrier = recorder("audit", typed_session_id);
    let barriers = lattice_api::RewindBarriers {
        after_workspace_half: Some(&workspace_barrier),
        after_session_ref_move: Some(&session_barrier),
        after_audit_append: Some(&audit_barrier),
    };

    let outcome =
        lattice_api::rewind_to_turn_with_barriers(&api, &session_id, &base, &ws.root, &barriers)
            .expect("rewind runs");
    assert!(matches!(outcome, lattice_api::RewindOutcome::Success(_)));

    let events = observed.lock().unwrap().clone();
    assert_eq!(
        events,
        vec![
            ("workspace:workspace_applied".to_string(), 0, false),
            ("session:session_moved".to_string(), 0, false),
            ("audit:success".to_string(), 1, false),
        ],
        "each barrier must observe the exact durable state at its window"
    );

    assert!(
        WorkspaceHistory::open(operation_backing)
            .expect("open history after rewind")
            .lock_workspace_operation(
                lancet_runtime::LockMode::Exclusive,
                lancet_runtime::Blocking::NonBlocking,
            )
            .expect("acquire after coordinator return")
            .is_some(),
        "the coordinator must release the operation hold only after returning"
    );

    // A completed retry returns before every action barrier and must not append
    // another record to the independent chain.
    lattice_api::rewind_to_turn(&api, &session_id, &base, &ws.root)
        .expect("completed Rewind replays");
    let records = rewind_records(&ws.root, &session_id);
    assert_eq!(records.len(), 1, "completed retry does not append");
    assert_eq!(records[0].outcome, "success");
}

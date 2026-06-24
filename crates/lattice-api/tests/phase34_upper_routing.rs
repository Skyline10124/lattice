// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Phase 34 integration: Direct observation/mutation route through Upper only.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

use async_trait::async_trait;
use git2::{IndexAddOption, Repository, Signature};
use lancet_api::{AuditBacking, LancetRuntime, LancetRuntimeConfig};
use lancet_authority::policy::default::{PolicyConfig, WorkspacePolicy};
use lancet_catalog::CepMethod;
use lancet_catalog::methods::workspace::{Create, ObserveWorkspace};
use lancet_native::{ExecutionRouting, UpperContext};
use lattice_agent::ToolCallExecutor;
use lattice_api::{CepExecutorAdapter, WorkspaceExecutionBackend};
use lattice_api::{CreateSessionRequest, RunTurnRequest, SessionApi, stream_turn};
use lattice_llm::{FunctionCall, ToolCall};
use lattice_profile::{
    AgentProfile, AgentSection, CredentialRef, LancetSection, LancetWorkspacePolicySection,
    LlmSection, LoadedAgentProfile, PromptSection, ToolsSection, WorkspaceSection,
};
use lattice_workspace::testing::DeterministicWorkspace;
use lattice_workspace::testing::contract_tests::minimal_upper_spec_with;
use lattice_workspace::{
    EgressPolicy, ExecOutput, GitStatus, ImageOrTemplate, UpperHandle, Workspace,
    WorkspaceError as WsError, WorkspacePath,
};
use serde_json::Value;
use tempfile::TempDir;

struct LowerTreeSnapshot {
    entries: BTreeMap<String, LowerEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LowerEntry {
    rel_path: String,
    is_dir: bool,
    content_hash: Option<String>,
    size: u64,
    mtime: SystemTime,
}

impl LowerTreeSnapshot {
    fn capture(lower_root: &Path, exclude_audit: Option<&Path>) -> Self {
        let mut entries = BTreeMap::new();
        capture_recursive(lower_root, lower_root, exclude_audit, &mut entries);
        Self { entries }
    }

    fn assert_unchanged(&self, other: &Self) {
        assert_eq!(self.entries, other.entries, "lower sentinel drifted");
    }
}

fn capture_recursive(
    lower_root: &Path,
    current: &Path,
    exclude_audit: Option<&Path>,
    out: &mut BTreeMap<String, LowerEntry>,
) {
    if let Some(ex) = exclude_audit
        && current.starts_with(ex)
    {
        return;
    }
    let Ok(read_dir) = std::fs::read_dir(current) else {
        return;
    };
    for entry in read_dir.flatten() {
        let path = entry.path();
        if let Some(ex) = exclude_audit
            && path.starts_with(ex)
        {
            continue;
        }
        let rel = path
            .strip_prefix(lower_root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        if rel.is_empty() {
            if path.is_dir() {
                capture_recursive(lower_root, &path, exclude_audit, out);
            }
            continue;
        }
        let meta = entry.metadata().expect("lower metadata");
        let is_dir = meta.is_dir();
        let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        let (content_hash, size) = if meta.is_file() {
            let bytes = std::fs::read(&path).expect("read lower file");
            (
                Some(format!("blake3:{}", blake3::hash(&bytes).to_hex())),
                bytes.len() as u64,
            )
        } else {
            (None, meta.len())
        };
        out.insert(
            rel.clone(),
            LowerEntry {
                rel_path: rel,
                is_dir,
                content_hash,
                size,
                mtime,
            },
        );
        if is_dir {
            capture_recursive(lower_root, &path, exclude_audit, out);
        }
    }
}

fn assert_no_workspace_root_leak(value: &Value, lower_root: &Path) {
    let lower = lower_root.to_string_lossy();
    fn walk(v: &Value, lower: &str, path: &str) {
        match v {
            Value::String(s) => {
                assert!(
                    !s.contains("/workspace"),
                    "leak at {path}: /workspace in {s}"
                );
                assert!(!s.contains(lower), "leak at {path}: lower root in {s}");
            }
            Value::Array(a) => {
                for (i, item) in a.iter().enumerate() {
                    walk(item, lower, &format!("{path}[{i}]"));
                }
            }
            Value::Object(map) => {
                for (k, item) in map {
                    walk(item, lower, &format!("{path}.{k}"));
                }
            }
            _ => {}
        }
    }
    walk(value, lower.as_ref(), "$");
}

fn init_git_baseline(lower: &Path, seed_file: &str, seed_content: &str) -> String {
    let repo = Repository::init(lower).expect("git init");
    std::fs::write(lower.join(seed_file), seed_content).expect("seed");
    let mut index = repo.index().expect("index");
    index
        .add_all(["*"].iter(), IndexAddOption::DEFAULT, None)
        .expect("add_all");
    index.write().expect("index write");
    let tree_id = index.write_tree().expect("tree");
    let tree = repo.find_tree(tree_id).expect("find tree");
    let sig = Signature::now("phase34", "phase34@test").expect("sig");
    let oid = repo
        .commit(Some("HEAD"), &sig, &sig, "baseline", &tree, &[])
        .expect("commit");
    oid.to_string()
}

/// Create an unattached branch at the current HEAD without checking it out.
fn create_unattached_target_branch(root: &Path, name: &str) -> String {
    std::process::Command::new("git")
        .args(["branch", name])
        .current_dir(root)
        .status()
        .expect("git branch (create unattached target)");
    name.to_string()
}

async fn direct_runtime_fixture(
    lower: &Path,
    upper_seed: &str,
) -> (
    CepExecutorAdapter,
    Arc<DeterministicWorkspace>,
    UpperHandle,
    TempDir,
    LancetRuntime,
) {
    let baseline_oid = init_git_baseline(lower, "sentinel.txt", "LOWER_SENTINEL");
    let workspace = Arc::new(DeterministicWorkspace::new().expect("det workspace"));
    let spec = minimal_upper_spec_with(
        ImageOrTemplate::Image("alpine:3.19".into()),
        EgressPolicy::Blocked,
        lower.to_path_buf(),
        baseline_oid,
    );
    let handle = workspace.create_upper(&spec).await.expect("create upper");
    workspace
        .write_file(
            &handle,
            &WorkspacePath::parse("sentinel.txt").unwrap(),
            upper_seed.as_bytes(),
        )
        .await
        .expect("seed upper");

    let upper_context = UpperContext {
        execution_backend: "deterministic".into(),
        upper_workspace_id: handle.id.clone(),
    };
    let execution = ExecutionRouting {
        backend: Arc::new(WorkspaceExecutionBackend::new(
            workspace.clone(),
            handle.clone(),
            upper_context.clone(),
        )),
        upper: upper_context,
    };

    let mut policy = PolicyConfig::defaults_for_project_root(lower.to_path_buf());
    policy.workspace = WorkspacePolicy {
        allowed_write_paths: vec![".".into()],
    };

    let audit_dir = TempDir::new().expect("audit temp");
    let runtime = LancetRuntime::build(LancetRuntimeConfig {
        project_root: lower.to_path_buf(),
        enabled_tools: vec![ObserveWorkspace::NAME.into(), Create::NAME.into()],
        policy,
        command_isolation: Default::default(),
        cep_mode: lancet_protocol::policy::CepMode::Enforce,
        audit: AuditBacking::File(audit_dir.path().join("audit.db")),
        model_id: "test".into(),
        runtime_version: "test".into(),
        execution: Some(execution),
        direct_verify: None,
    })
    .expect("runtime");

    let client = runtime.client();
    let enabled = vec![ObserveWorkspace::NAME.to_string(), Create::NAME.to_string()];
    let adapter = CepExecutorAdapter::new(client, enabled);
    (adapter, workspace, handle, audit_dir, runtime)
}

fn tool_call(method: &str, arguments: Value) -> ToolCall {
    ToolCall {
        id: format!("call_{method}"),
        function: FunctionCall {
            name: method.to_string(),
            arguments: serde_json::to_string(&arguments).expect("args"),
        },
    }
}

#[tokio::test]
async fn phase34_observation_uses_upper_relative_paths() {
    let lower = TempDir::new().expect("lower");
    let (adapter, _ws, _handle, _audit, _runtime) =
        direct_runtime_fixture(lower.path(), "UPPER_SENTINEL").await;
    let before = LowerTreeSnapshot::capture(lower.path(), None);

    let result = adapter
        .execute(&tool_call(
            ObserveWorkspace::NAME,
            serde_json::json!({
                "mode": "read_file",
                "path": "sentinel.txt",
                "offset": 1,
                "limit": 10,
                "line_numbers": false
            }),
        ))
        .await
        .expect("observe read");

    let parsed: Value = serde_json::from_str(&result).expect("json");
    assert!(
        parsed["result"]["content"]
            .as_str()
            .unwrap()
            .contains("UPPER_SENTINEL")
    );
    assert_no_workspace_root_leak(&parsed, lower.path());

    let after = LowerTreeSnapshot::capture(lower.path(), None);
    before.assert_unchanged(&after);
    let host_sentinel = std::fs::read_to_string(lower.path().join("sentinel.txt")).unwrap();
    assert_eq!(host_sentinel, "LOWER_SENTINEL");
}

#[tokio::test]
async fn phase34_mutations_preserve_lower_sentinel() {
    let lower = TempDir::new().expect("lower");
    let (adapter, workspace, handle, _audit, _runtime) =
        direct_runtime_fixture(lower.path(), "UPPER_BASE").await;
    let before = LowerTreeSnapshot::capture(lower.path(), None);

    adapter
        .execute(&tool_call(
            Create::NAME,
            serde_json::json!({
                "path": "created.txt",
                "content": "upper-only",
                "create_parents": false
            }),
        ))
        .await
        .expect("create");

    let upper_bytes = workspace
        .read_file(&handle, &WorkspacePath::parse("created.txt").unwrap())
        .await
        .expect("upper read");
    assert_eq!(upper_bytes, b"upper-only");
    assert!(!lower.path().join("created.txt").exists());

    let after = LowerTreeSnapshot::capture(lower.path(), None);
    before.assert_unchanged(&after);
}

#[tokio::test]
async fn phase34_path_denial_records_preflight_only() {
    let lower = TempDir::new().expect("lower");
    let (adapter, _ws, _handle, _audit, _runtime) =
        direct_runtime_fixture(lower.path(), "UPPER").await;

    let err = adapter
        .execute(&tool_call(
            ObserveWorkspace::NAME,
            serde_json::json!({
                "mode": "read_file",
                "path": "../outside",
            }),
        ))
        .await
        .expect_err("traversal denied");

    let payload = err.cep_error.expect("cep payload");
    assert!(!payload.message.is_empty());
}

fn tool_call_events(id: &str, name: &str, args: &str) -> Vec<lattice_llm::LlmEvent> {
    vec![
        lattice_llm::LlmEvent::ToolCallStart {
            id: id.to_string(),
            name: name.to_string(),
        },
        lattice_llm::LlmEvent::ToolCallDelta {
            id: id.to_string(),
            arguments_delta: args.to_string(),
        },
        lattice_llm::LlmEvent::ToolCallEnd { id: id.to_string() },
        lattice_llm::LlmEvent::Done {
            finish_reason: "tool_calls".to_string(),
            usage: None,
        },
    ]
}

fn final_response_events(text: &str) -> Vec<lattice_llm::LlmEvent> {
    vec![
        lattice_llm::LlmEvent::Token {
            content: text.to_string(),
        },
        lattice_llm::LlmEvent::Done {
            finish_reason: "stop".to_string(),
            usage: None,
        },
    ]
}

fn scan_value_for_leaks(value: &Value, lower_root: &Path, secret: &str) {
    assert_no_workspace_root_leak(value, lower_root);
    let serialized = value.to_string();
    assert!(
        !serialized.contains(secret),
        "credential sentinel leaked in value"
    );
}

#[tokio::test]
async fn phase34_upper_routing_e2e() {
    use futures::StreamExt;
    use lancet_api::audit_db_path_for_project_in;
    use lancet_catalog::methods::command::Run;
    use lattice_agent::LlmClient;
    use lattice_agent::testing::FakeStreamingLlmClient;

    const SECRET: &str = "phase34-credential-sentinel-value";
    unsafe {
        std::env::set_var("PHASE34_TEST_SECRET", SECRET);
    }

    let state = TempDir::new().expect("state");
    unsafe {
        std::env::set_var("XDG_STATE_HOME", state.path());
    }
    let lower = TempDir::new().expect("lower");
    let baseline_oid = init_git_baseline(lower.path(), "sentinel.txt", "LOWER_SENTINEL");
    let _baseline_oid = baseline_oid;
    let sentinel_before = std::fs::read(lower.path().join("sentinel.txt")).expect("sentinel");

    let workspace = Arc::new(DeterministicWorkspace::new().expect("workspace"));
    let profile = LoadedAgentProfile {
        path: lower.path().join("agent.toml"),
        package_dir: lower.path().to_path_buf(),
        profile: AgentProfile {
            schema_version: 1,
            agent: AgentSection {
                name: "phase34".into(),
                model: "test".into(),
                description: None,
            },
            prompt: PromptSection {
                system: Some("test".into()),
                system_path: None,
            },
            tools: ToolsSection {
                enabled: vec![
                    ObserveWorkspace::NAME.into(),
                    Create::NAME.into(),
                    Run::NAME.into(),
                ],
            },
            llm: LlmSection::default(),
            lancet: LancetSection {
                workspace: Some(LancetWorkspacePolicySection {
                    allowed_write_paths: vec![".".into()],
                }),
                ..Default::default()
            },
            workspace: Some(WorkspaceSection {
                image: Some("docker.io/library/alpine:3.19".into()),
                egress: None,
                credentials: vec![CredentialRef {
                    env_var_name: "PHASE34_TEST_SECRET".into(),
                }],
                cpus: None,
                memory_mb: None,
                allowed_egress_targets: vec![],
                cleanup_rounds: 3,
                max_pending_snapshots: 5,
            }),
        },
        system_prompt: "test".into(),
    };

    let observe_args = serde_json::json!({
        "mode": "read_file",
        "path": "sentinel.txt",
        "offset": 1,
        "limit": 10,
        "line_numbers": false
    });
    let create_args = serde_json::json!({
        "path": "created.txt",
        "content": "upper-created",
        "create_parents": false
    });
    let exec_args = serde_json::json!({
        "argv": ["echo", "phase34-exec"],
        "cwd": ".",
        "timeout_ms": 5000,
        "declared_impact": "command_execution"
    });

    let fake_llm: Arc<dyn LlmClient> = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(tool_call_events(
                "c1",
                ObserveWorkspace::NAME,
                &observe_args.to_string(),
            ))
            .push_events(tool_call_events(
                "c2",
                Create::NAME,
                &create_args.to_string(),
            ))
            .push_events(tool_call_events("c3", Run::NAME, &exec_args.to_string()))
            .push_events(final_response_events("phase34 upper routing complete"))
            .build(),
    );

    let target = create_unattached_target_branch(lower.path(), "lattice-target-e2e");
    let sessions_path = lower.path().join("sessions");
    let session_api = SessionApi::git(&sessions_path);
    let session_id = session_api
        .create_session(CreateSessionRequest {
            label: None,
            active_agent: "phase34".into(),
            project_branch: target,
        })
        .expect("session")
        .session_id;

    let request = RunTurnRequest {
        input: "run phase34 e2e".into(),
        session_id,
        loaded_profile: Some(profile),
        cancel_flag: None,
        test_llm: Some(fake_llm),
        test_project_root: Some(lower.path().to_string_lossy().into()),
        test_persist_audit: Some(false),
        workspace_override: Some(workspace.clone()),
    };

    let mut stream = stream_turn(session_api, request)
        .await
        .expect("stream_turn");
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }

    let committed = events
        .iter()
        .any(|event| matches!(event, lattice_api::TurnEvent::TurnCommitted { .. }));
    assert!(committed, "events: {events:?}");

    let external_path = audit_db_path_for_project_in(state.path(), lower.path()).unwrap();
    let audit = lancet_audit::CepAuditStore::open(&external_path).unwrap();
    let rows = audit
        .query_rows(&lancet_native::AuditQueryFilters::default())
        .expect("audit rows");
    assert!(!rows.is_empty());

    let mut upper_ids = BTreeMap::new();
    for row in &rows {
        if let Some(meta) = row.metadata_json.as_deref() {
            scan_value_for_leaks(
                &serde_json::from_str(meta).expect("meta"),
                lower.path(),
                SECRET,
            );
            if let Ok(meta) = serde_json::from_str::<Value>(meta)
                && let Some(id) = meta.get("upper_workspace_id").and_then(|v| v.as_str())
            {
                *upper_ids.entry(id.to_string()).or_insert(0) += 1;
            }
        }
        scan_value_for_leaks(&Value::String(row.method.clone()), lower.path(), SECRET);
    }
    assert_eq!(
        upper_ids.len(),
        1,
        "expected one Upper ID across audit rows"
    );

    for event in events.drain(..) {
        scan_value_for_leaks(
            &serde_json::to_value(&event).expect("event"),
            lower.path(),
            SECRET,
        );
    }

    let sentinel_after = std::fs::read(lower.path().join("sentinel.txt")).expect("sentinel");
    assert_eq!(
        sentinel_before, sentinel_after,
        "lower sentinel.txt drifted"
    );
    assert!(
        !lower.path().join("created.txt").exists(),
        "mutate.create must not land on Lower host tree"
    );
}

struct FailingWorkspace {
    host_touched: Arc<AtomicBool>,
}

#[async_trait]
impl Workspace for FailingWorkspace {
    async fn create_upper(
        &self,
        _spec: &lattice_workspace::UpperSpec,
    ) -> lattice_workspace::Result<UpperHandle> {
        Err(WsError::Unavailable("deterministic adapter failure".into()))
    }

    async fn exec_upper(
        &self,
        _handle: &UpperHandle,
        _prog: &str,
        _args: &[&str],
    ) -> lattice_workspace::Result<ExecOutput> {
        self.host_touched.store(true, Ordering::SeqCst);
        Err(WsError::Unavailable("must not exec".into()))
    }

    async fn git_status(&self, _handle: &UpperHandle) -> lattice_workspace::Result<GitStatus> {
        Ok(GitStatus::default())
    }

    async fn template_create(
        &self,
        _source: &UpperHandle,
        _name: &str,
    ) -> lattice_workspace::Result<String> {
        Err(WsError::Unavailable("unsupported".into()))
    }

    async fn restore_upper(
        &self,
        _template_name: &str,
        _new_name: &str,
        _spec: &lattice_workspace::UpperSpec,
    ) -> lattice_workspace::Result<UpperHandle> {
        Err(WsError::Unavailable("unsupported".into()))
    }

    async fn destroy_upper(&self, _handle: &UpperHandle) -> lattice_workspace::Result<()> {
        Ok(())
    }

    async fn export_candidate(
        &self,
        _handle: &UpperHandle,
        _baseline_oid: &str,
        _max_artifact_bytes: u64,
    ) -> lattice_workspace::Result<lattice_workspace::CandidateExport> {
        self.host_touched.store(true, Ordering::SeqCst);
        Err(WsError::Unavailable("must not export".into()))
    }
}

#[tokio::test]
async fn phase34_adapter_failure_has_no_lower_fallback() {
    use lattice_agent::LlmClient;
    use lattice_agent::testing::FakeStreamingLlmClient;
    use lattice_api::{CreateSessionRequest, RunTurnRequest, SessionApi, stream_turn};
    use lattice_profile::{
        AgentProfile, AgentSection, LancetSection, LlmSection, LoadedAgentProfile, PromptSection,
        ToolsSection, WorkspaceSection,
    };

    let lower = TempDir::new().expect("lower");
    init_git_baseline(lower.path(), "host-sentinel.txt", "HOST_ONLY");
    let sentinel_before =
        std::fs::read(lower.path().join("host-sentinel.txt")).expect("host-sentinel");
    let host_touched = Arc::new(AtomicBool::new(false));

    let profile = LoadedAgentProfile {
        path: lower.path().join("agent.toml"),
        package_dir: lower.path().to_path_buf(),
        profile: AgentProfile {
            schema_version: 1,
            agent: AgentSection {
                name: "phase34".into(),
                model: "test".into(),
                description: None,
            },
            prompt: PromptSection {
                system: Some("test".into()),
                system_path: None,
            },
            tools: ToolsSection {
                enabled: vec![ObserveWorkspace::NAME.into()],
            },
            llm: LlmSection::default(),
            lancet: LancetSection::default(),
            workspace: Some(WorkspaceSection {
                image: Some("docker.io/library/alpine:3.19".into()),
                egress: None,
                credentials: vec![],
                cpus: None,
                memory_mb: None,
                allowed_egress_targets: vec![],
                cleanup_rounds: 3,
                max_pending_snapshots: 5,
            }),
        },
        system_prompt: "test".into(),
    };

    let fake_llm: Arc<dyn LlmClient> = Arc::new(FakeStreamingLlmClient::builder().build());
    let target = create_unattached_target_branch(lower.path(), "lattice-target-fail");
    let sessions_path = lower.path().join("sessions");
    let session_api = SessionApi::git(&sessions_path);
    let session_id = session_api
        .create_session(CreateSessionRequest {
            label: None,
            active_agent: "phase34".into(),
            project_branch: target,
        })
        .expect("session")
        .session_id;

    let request = RunTurnRequest {
        input: "should fail".into(),
        session_id,
        loaded_profile: Some(profile),
        cancel_flag: None,
        test_llm: Some(fake_llm),
        test_project_root: Some(lower.path().to_string_lossy().into()),
        test_persist_audit: None,
        workspace_override: Some(Arc::new(FailingWorkspace {
            host_touched: host_touched.clone(),
        })),
    };

    let result = stream_turn(session_api, request).await;
    assert!(
        result.is_err(),
        "Upper adapter failure must abort turn startup"
    );

    assert!(!host_touched.load(Ordering::SeqCst));
    let sentinel_after =
        std::fs::read(lower.path().join("host-sentinel.txt")).expect("host-sentinel");
    assert_eq!(
        sentinel_before, sentinel_after,
        "lower host sentinel drifted"
    );
}

#[tokio::test]
async fn generated_profiles_are_executable_host_only() {
    use lattice_agent::LlmClient;
    use lattice_agent::testing::FakeStreamingLlmClient;
    use lattice_profile::{
        default_agent_template, default_dev_template, default_editor_template,
        default_reader_template,
    };

    let dir = TempDir::new().expect("dir");
    let llm: Arc<dyn LlmClient> = Arc::new(FakeStreamingLlmClient::builder().build());

    for toml_str in [
        default_agent_template("t", "sonnet"),
        default_reader_template("t", "sonnet"),
        default_editor_template("t", "sonnet"),
        default_dev_template("t", "sonnet"),
    ] {
        let profile: lattice_profile::AgentProfile = toml::from_str(&toml_str).expect("parse");
        let loaded = LoadedAgentProfile {
            path: dir.path().join("agent.toml"),
            package_dir: dir.path().to_path_buf(),
            profile: profile.clone(),
            system_prompt: profile.prompt.system.clone().unwrap_or_default(),
        };
        let sessions_path = dir.path().join("sessions");
        let session_api = SessionApi::git(&sessions_path);
        let session_id = session_api
            .create_session(CreateSessionRequest {
                label: None,
                active_agent: profile.agent.name.clone(),
                project_branch: "main".to_string(),
            })
            .expect("session")
            .session_id;
        let request = RunTurnRequest {
            input: "host-only".into(),
            session_id,
            loaded_profile: Some(loaded),
            cancel_flag: None,
            test_llm: Some(llm.clone()),
            test_project_root: Some(dir.path().to_string_lossy().into()),
            test_persist_audit: None,
            workspace_override: None,
        };
        let _stream = stream_turn(session_api, request)
            .await
            .expect("host-only profile reaches Direct runtime");
    }
}

#[cfg(feature = "integration_tests")]
mod microsandbox_lane {
    use super::*;

    #[tokio::test]
    #[ignore = "requires KVM/libkrun and configured Phase 34 image/template"]
    async fn phase34_microsandbox_upper_routing() {
        // Optional KVM evidence lane — compile-only in default CI.
        let _ = std::env::var("PHASE34_MICROSANDBOX_IMAGE");
    }
}

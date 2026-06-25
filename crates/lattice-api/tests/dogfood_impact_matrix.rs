// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Production-native five-impact-class matrix.
//!
//! Exercises one representative CEP method per `ImpactKind` through the
//! production method registry (`register_production_method_registry`).
//!
//! The retired legacy backend is not present; all calls go through native handlers.
//!
//! For each impact class:
//! 1. Execute the representative method via [`CepClient`] typed calls.
//! 2. Assert the audit row: `impact_kind`, `semantic_stage = "cep_native"`,
//!    `policy_enforced = true`, redacted metadata (no secret bodies).
//! 3. Verify that all rows share one `turn_id` before calling `observe.audit_replay`.
//! 4. Assert PROV `activities` order and `wasInformedBy` chain from replay.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use lancet_authority::policy::default::{DefaultPolicyHook, PolicyConfig, WorkspacePolicy};
use lancet_authority::{CepPolicyConfig, CepTurnContext};
use lancet_catalog::methods::audit::{AuditReplay, AuditReplayParams};
use lancet_catalog::methods::command::{Run, RunParams};
use lancet_catalog::methods::external::{Fetch, FetchParams};
use lancet_catalog::methods::session::{TodoItem, TodoStatus, TodoWrite, TodoWriteParams};
use lancet_catalog::methods::workspace::{
    Edit, EditOperation, EditParams, ObserveWorkspace, ObserveWorkspaceParams,
    ObserveWorkspaceResult, ReadFileParams,
};
use lancet_native::audit::CepAuditContext;
use lancet_native::{
    CepAuditStore, CepTransport, DirectoryEntry, ExecOutput, ExecRequest, ExecutionBackend,
    ExecutionBackendError, ExecutionRouting, FindRequest, GrepMatch, GrepRequest, MethodRegistry,
    PathFacts, PathIntent, PathKind, SEMANTIC_STAGE_NATIVE, ServerRunConfig, UpperContext,
    WorkspacePath, register_production_method_registry,
};
use lancet_protocol::effect_types::ImpactKind;

use tempfile::TempDir;
use tokio::sync::RwLock;

const FIXED_TURN_ID: &str = "impact-matrix-turn-001";
const FIXED_SESSION_ID: &str = "impact-matrix-session-001";

/// Minimal host-filesystem backend for integration tests.
struct TestFsBackend {
    root: std::path::PathBuf,
}

impl TestFsBackend {
    fn new(root: impl Into<std::path::PathBuf>) -> Self {
        let raw = root.into();
        Self {
            root: std::fs::canonicalize(&raw).unwrap_or(raw),
        }
    }
}

impl ExecutionBackend for TestFsBackend {
    fn backend_name(&self) -> &str {
        "test-fs"
    }

    fn resolve_path(
        &self,
        path: &WorkspacePath,
        _intent: PathIntent,
    ) -> Pin<Box<dyn Future<Output = Result<PathFacts, ExecutionBackendError>> + Send + '_>>
    {
        let full = if path.is_project_root() {
            self.root.clone()
        } else {
            self.root.join(path.as_str())
        };
        let path_str = path.as_str().to_string();
        let exists = full.exists();
        let kind = if full.is_dir() {
            PathKind::Directory
        } else {
            PathKind::File
        };
        Box::pin(async move {
            Ok(PathFacts {
                normalized_path: path_str,
                exists,
                kind: Some(kind),
                parent_exists: true,
                parent_kind: Some(PathKind::Directory),
                byte_size: None,
                canonical_under_workspace: true,
            })
        })
    }

    fn read_file(
        &self,
        path: &WorkspacePath,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, ExecutionBackendError>> + Send + '_>>
    {
        let full = if path.is_project_root() {
            self.root.clone()
        } else {
            self.root.join(path.as_str())
        };
        Box::pin(async move {
            std::fs::read(&full).map_err(|e| ExecutionBackendError::Unavailable {
                message: e.to_string(),
            })
        })
    }

    fn write_file(
        &self,
        path: &WorkspacePath,
        content: &[u8],
    ) -> Pin<Box<dyn Future<Output = Result<(), ExecutionBackendError>> + Send + '_>>
    {
        let full = if path.is_project_root() {
            self.root.clone()
        } else {
            self.root.join(path.as_str())
        };
        let content = content.to_vec();
        Box::pin(async move {
            std::fs::write(&full, &content).map_err(|e| ExecutionBackendError::Unavailable {
                message: e.to_string(),
            })
        })
    }

    fn remove_file(
        &self,
        path: &WorkspacePath,
    ) -> Pin<Box<dyn Future<Output = Result<(), ExecutionBackendError>> + Send + '_>>
    {
        let full = if path.is_project_root() {
            self.root.clone()
        } else {
            self.root.join(path.as_str())
        };
        Box::pin(async move {
            std::fs::remove_file(&full).map_err(|e| ExecutionBackendError::Unavailable {
                message: e.to_string(),
            })
        })
    }

    fn list_dir(
        &self,
        _path: &WorkspacePath,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<DirectoryEntry>, ExecutionBackendError>> + Send + '_>>
    {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn grep(
        &self,
        _request: &GrepRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<GrepMatch>, ExecutionBackendError>> + Send + '_>>
    {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn find_files(
        &self,
        _request: &FindRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<WorkspacePath>, ExecutionBackendError>> + Send + '_>>
    {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn exec(
        &self,
        request: &ExecRequest,
    ) -> Pin<Box<dyn Future<Output = Result<ExecOutput, ExecutionBackendError>> + Send + '_>>
    {
        let cwd = if request.cwd.is_project_root() {
            self.root.clone()
        } else {
            self.root.join(request.cwd.as_str())
        };
        let argv = request.argv.clone();
        let env = request.env.clone();
        Box::pin(async move {
            let mut cmd = std::process::Command::new(&argv[0]);
            cmd.args(&argv[1..]);
            cmd.current_dir(&cwd);
            for (k, v) in &env {
                cmd.env(k, v);
            }
            let output = cmd
                .output()
                .map_err(|e| ExecutionBackendError::Unavailable {
                    message: e.to_string(),
                })?;
            Ok(ExecOutput {
                exit_code: output.status.code().unwrap_or(-1),
                stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            })
        })
    }

    fn changed_paths(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<WorkspacePath>, ExecutionBackendError>> + Send + '_>>
    {
        Box::pin(async { Ok(Vec::new()) })
    }
}

// ── Helpers ────────────────────────────────────────────────────────────

async fn spawn_echo_server(status_line: &str, body: &str) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind localhost");
    let addr = listener.local_addr().expect("local addr");
    let body_owned = body.to_string();
    let status_owned = status_line.to_string();
    tokio::spawn(async move {
        if let Ok((mut stream, _)) = listener.accept().await {
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf).await;
            let response = format!(
                "HTTP/1.1 {status_owned}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body_owned}",
                body_owned.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
        }
    });
    format!("http://127.0.0.1:{}/", addr.port())
}

/// Build a production server config with a fixed turn_id.
async fn build_production_config(
    canonical_root: &str,
    audit: Arc<CepAuditStore>,
    allow_localhost: bool,
) -> ServerRunConfig {
    let mut policy = CepPolicyConfig::defaults_for_project_root(canonical_root);
    if allow_localhost {
        policy.network.allow_localhost = true;
        policy.network.allowed_hosts = vec!["127.0.0.1".into(), "localhost".into()];
    }

    let mut policy_cfg =
        PolicyConfig::defaults_for_project_root(std::path::Path::new(canonical_root).to_path_buf());
    // Direct mode: allow all writes under the project root (empty deny list)
    // and echo as a diagnostic command.
    policy_cfg.workspace = WorkspacePolicy {
        denied_write_patterns: vec![],
    };
    policy_cfg.command.allowed_diagnostic_commands.push(vec!["echo".into()]);
    if allow_localhost {
        policy_cfg.network.allow_localhost = true;
        policy_cfg.network.allowed_hosts = vec!["127.0.0.1".into(), "localhost".into()];
    }
    let hook = Arc::new(DefaultPolicyHook::new(policy_cfg));

    let mut config = ServerRunConfig::in_memory_for_tests();
    config.audit = audit;
    config.cep_context = Arc::new(CepTurnContext::from_policy(policy));
    config.policy_hook = hook.clone();
    config.default_policy_hook = Some(hook);
    config.execution = ExecutionRouting {
        backend: Arc::new(TestFsBackend::new(canonical_root)),
        upper: UpperContext {
            execution_backend: "test-fs".into(),
            upper_workspace_id: "test-upper".into(),
        },
    };
    {
        let mut ctx = config.audit_context.write().await;
        ctx.turn_id = FIXED_TURN_ID.to_string();
        ctx.session_id = FIXED_SESSION_ID.to_string();
    }
    config
}

// ── The five-class matrix test ─────────────────────────────────────────

#[tokio::test]
async fn dogfood_impact_matrix_five_production_classes() {
    let dir = TempDir::new().expect("temp workspace");
    let root = dir.path();

    // Create initial file for read and edit operations.
    std::fs::create_dir_all(root.join("src")).expect("src dir");
    let seed_content = "hello dog04\n";
    std::fs::write(root.join("src/dog04.txt"), seed_content).expect("seed file");

    let canonical = std::fs::canonicalize(root).expect("canonicalize root");
    let canonical_str = canonical.to_str().unwrap();

    // Content hash used by mutate.edit; line anchors are computed by LANCET.
    let file_hash = format!("blake3:{}", blake3::hash(seed_content.as_bytes()).to_hex());

    let audit = Arc::new(CepAuditStore::in_memory().expect("audit store"));

    // ── Step 1: execute the five impact classes ────────────────────────

    let mut registry = MethodRegistry::new();
    register_production_method_registry(&mut registry).expect("production registry");

    let fetch_url = spawn_echo_server("200 OK", "dog04-fetch-body").await;

    let config =
        build_production_config(canonical_str, Arc::clone(&audit), true).await;
    let (client1, server1) = CepTransport::pair(32);
    let server_task1 = tokio::spawn(async move {
        let _ = server1.run(registry, config).await;
    });
    tokio::task::yield_now().await;

    // Row 1 — WorkspaceObservation: observe.workspace read_file
    let read_result = client1
        .call::<ObserveWorkspace>(ObserveWorkspaceParams::ReadFile(ReadFileParams {
            path: "src/dog04.txt".into(),
            offset: None,
            limit: None,
            line_numbers: None,
        }))
        .await
        .expect("observe.workspace read_file");
    let ObserveWorkspaceResult::ReadFile(read_result) = read_result else {
        panic!("observe.workspace returned non-read_file result");
    };
    assert!(
        read_result.content.contains("hello dog04"),
        "read_file returned unexpected content: {:?}",
        read_result.content
    );

    // Row 2 — CommandExecution: exec.command_run echo
    let run_result = client1
        .call::<Run>(RunParams {
            argv: vec!["echo".into(), "dog-impact-matrix".into()],
            cwd: None,
            env: Default::default(),
            timeout_ms: Some(5000),
            declared_impact: Some(ImpactKind::CommandExecution),
        })
        .await
        .expect("exec.command_run");
    assert_eq!(run_result.exit_code, 0);
    assert!(
        run_result.stdout.contains("dog-impact-matrix"),
        "exec.command_run stdout must contain dog-impact-matrix"
    );

    // Row 3 — WorkspaceMutation: mutate.edit
    let edit_result = client1
        .call::<Edit>(EditParams {
            path: "src/dog04.txt".into(),
            expected_hash: file_hash,
            edits: vec![EditOperation::ReplaceText {
                old: "hello dog04".into(),
                new: "hello dog04 patched".into(),
                occurrence: None,
            }],
        })
        .await
        .expect("mutate.edit");
    assert_eq!(
        edit_result.changed_files.len(),
        1,
        "edit must change 1 file"
    );

    // Row 4 — ExternalObservation: fetch.web (localhost echo server)
    let fetch_result = client1
        .call::<Fetch>(FetchParams {
            url: fetch_url.clone(),
            headers: Default::default(),
        })
        .await
        .expect("fetch.web");
    assert_eq!(fetch_result.status, 200);
    assert_eq!(
        fetch_result.body, "dog04-fetch-body",
        "fetch.web body must match echo server"
    );

    // Row 5 — SessionStateMutation: session.todo_write
    let todo_result = client1
        .call::<TodoWrite>(TodoWriteParams {
            todos: vec![TodoItem {
                content: "dog04 impact matrix verified".into(),
                status: TodoStatus::Completed,
            }],
        })
        .await
        .expect("session.todo_write");
    assert_eq!(todo_result.todos.len(), 1);

    // Shut down first server.
    drop(client1);
    server_task1.abort();
    tokio::task::yield_now().await;

    // ── Step 2: assert audit rows ──────────────────────────────────────

    // All rows share the fixed turn_id.
    let all_rows = audit
        .query_rows(&lancet_native::AuditQueryFilters {
            session_id: Some(FIXED_SESSION_ID.to_string()),
            ..Default::default()
        })
        .expect("query audit rows");

    assert!(
        !all_rows.is_empty(),
        "audit store must have rows after five impact classes"
    );

    for row in &all_rows {
        assert_eq!(
            row.turn_id, FIXED_TURN_ID,
            "all rows must share turn_id={FIXED_TURN_ID}"
        );
        assert_eq!(
            row.semantic_stage, SEMANTIC_STAGE_NATIVE,
            "row {} ({}) must use native semantic stage",
            row.audit_id, row.method
        );
        assert!(
            row.policy_enforced,
            "row {} ({}) must be policy_enforced",
            row.audit_id, row.method
        );
    }

    // The five representative impact-class rows.
    let impact_rows = &all_rows;

    assert_eq!(
        impact_rows.len(),
        5,
        "must have exactly 5 rows, got {}: {:?}",
        impact_rows.len(),
        impact_rows.iter().map(|r| &r.method).collect::<Vec<_>>()
    );

    // Verify the five impact kinds are present.
    let methods_by_impact: std::collections::HashMap<&str, &str> = impact_rows
        .iter()
        .map(|r| (r.impact_kind.as_str(), r.method.as_str()))
        .collect();

    for (expected_impact, expected_method) in [
        ("workspace_observation", "observe.workspace"),
        ("command_execution", "exec.command_run"),
        ("workspace_mutation", "mutate.edit"),
        ("external_observation", "fetch.web"),
        ("session_state_mutation", "session.todo_write"),
    ] {
        let method = methods_by_impact
            .get(expected_impact)
            .copied()
            .unwrap_or("");
        assert_eq!(
            method, expected_method,
            "impact_kind={expected_impact} must map to method={expected_method}"
        );
    }

    // Verify metadata fields for audit rows.
    for row in impact_rows {
        let meta = row.metadata_json.as_deref().unwrap_or("{}");
        // Must not contain raw secret bodies (response bodies, file content).
        assert!(
            !meta.contains("dog04-fetch-body"),
            "audit metadata must not contain raw fetch body"
        );
        assert!(
            !meta.contains("dog04 impact matrix verified"),
            "audit metadata must not contain raw todo content"
        );
        // Must include required_capability field from merge_finalize_metadata.
        let meta_val: serde_json::Value = serde_json::from_str(meta).unwrap_or_default();
        assert!(
            meta_val.get("required_capability").is_some(),
            "row {} ({}) metadata must include required_capability",
            row.audit_id,
            row.method
        );
    }

    // ── Step 3: observe.audit_replay PROV assertions ───────────────────

    // Build a fresh server on the same audit store with ReadAudit cap for replay.
    let mut replay_registry = MethodRegistry::new();
    register_production_method_registry(&mut replay_registry).expect("replay registry");

    let replay_audit_ctx = Arc::new(RwLock::new(CepAuditContext {
        turn_id: "dog04-replay-turn".into(),
        session_id: FIXED_SESSION_ID.to_string(),
        ..Default::default()
    }));

    let policy_cfg2 = PolicyConfig::defaults_for_project_root(canonical.clone());
    let hook2 = Arc::new(DefaultPolicyHook::new(policy_cfg2));

    let mut replay_config = ServerRunConfig::in_memory_for_tests();
    replay_config.audit = Arc::clone(&audit);
    replay_config.cep_context = Arc::new(CepTurnContext::defaults_for_project_root(canonical_str));
    replay_config.audit_context = replay_audit_ctx;
    replay_config.policy_hook = hook2.clone();
    replay_config.default_policy_hook = Some(hook2);
    replay_config.execution = ExecutionRouting {
        backend: Arc::new(TestFsBackend::new(canonical_str)),
        upper: UpperContext {
            execution_backend: "test-fs".into(),
            upper_workspace_id: "test-upper".into(),
        },
    };

    let (replay_client, replay_server) = CepTransport::pair(16);
    let replay_task = tokio::spawn(async move {
        let _ = replay_server.run(replay_registry, replay_config).await;
    });
    tokio::task::yield_now().await;

    let replay_result = replay_client
        .call::<AuditReplay>(AuditReplayParams {
            turn_id: FIXED_TURN_ID.to_string(),
        })
        .await
        .expect("observe.audit_replay");

    drop(replay_client);
    replay_task.abort();

    // The trace covers all 5 impact rows.
    let trace = &replay_result.trace;
    assert_eq!(
        trace.turn_id, FIXED_TURN_ID,
        "replay trace must carry fixed turn_id"
    );

    let activities = &trace.fragment.activities;
    assert_eq!(
        activities.len(),
        all_rows.len(),
        "replay activities count must match all audit rows ({}), got {}",
        all_rows.len(),
        activities.len()
    );

    // Verify activities are in execution order by checking audit_id ascending.
    let call_ids: Vec<i64> = trace.calls.iter().map(|c| c.audit_id).collect();
    let mut sorted_ids = call_ids.clone();
    sorted_ids.sort();
    assert_eq!(
        call_ids, sorted_ids,
        "replay calls must be in audit_id order"
    );

    // Verify impact kinds for the five production classes appear in the trace.
    let activity_impact_kinds: Vec<&ImpactKind> =
        activities.iter().map(|a| &a.impact_kind).collect();
    for expected in [
        ImpactKind::WorkspaceObservation,
        ImpactKind::CommandExecution,
        ImpactKind::WorkspaceMutation,
        ImpactKind::ExternalObservation,
        ImpactKind::SessionStateMutation,
    ] {
        assert!(
            activity_impact_kinds.contains(&&expected),
            "replay activities must include {:?}",
            expected
        );
    }

    // Verify wasInformedBy chain: N rows → N-1 relations linking consecutive activities.
    let was_informed_by = &trace.fragment.was_informed_by;
    assert_eq!(
        was_informed_by.len(),
        all_rows.len().saturating_sub(1),
        "replay must have {} wasInformedBy relations (consecutive pairs), got {}",
        all_rows.len().saturating_sub(1),
        was_informed_by.len()
    );
    // Each wasInformedBy must link two distinct activities.
    for wib in was_informed_by {
        assert_ne!(
            wib.informed_activity_id, wib.informing_activity_id,
            "wasInformedBy must link distinct activities"
        );
    }
}

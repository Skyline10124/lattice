//! Private Phase 20 bridge from CEP catalog methods to legacy tool bodies.

use crate::edit::PatchResultEnvelope;
use crate::executor::DefaultToolExecutor;
use crate::registry::{HandlerContext, MethodRegistry};
use lancet_protocol::error::{CepErrorData, ExecutionErrorData};
use lancet_protocol::methods::command::{Run, RunParams, RunResult};
use lancet_protocol::methods::envelope::{Declare, DeclareParams, DeclareResult};
use lancet_protocol::methods::external::{Fetch, FetchParams, FetchResult};
use lancet_protocol::methods::session::{TodoWrite, TodoWriteParams, TodoWriteResult};
use lancet_protocol::methods::workspace::{
    DirectoryEntry, DirectoryEntryKind, FindFiles, FindFilesParams, FindFilesResult, Grep,
    GrepMatch, GrepParams, GrepResult, ListDirectory, ListDirectoryParams, ListDirectoryResult,
    Patch, PatchParams, PatchResult, ReadFile, ReadFileParams, ReadFileResult, WriteFile,
    WriteFileParams, WriteFileResult,
};
use lancet_protocol::{CepError, CepErrorCode, CepMethod, Retriability};
use serde_json::{json, Value};
use std::sync::Arc;

macro_rules! bridge_method {
    ($registry:expr, $exec:expr, $Method:ty, $handler:ident) => {
        $registry.register_method::<$Method, _, _>({
            let executor = std::sync::Arc::clone(&$exec);
            move |params, ctx| {
                let executor = std::sync::Arc::clone(&executor);
                async move { $handler(&executor, params, ctx).await }
            }
        })
    };
}

macro_rules! bridge_method_noexec {
    ($registry:expr, $Method:ty, $handler:ident) => {
        $registry.register_method::<$Method, _, _>(move |params, ctx| async move {
            $handler(params, ctx).await
        })
    };
}

const SEMANTIC_STAGE: &str = "phase20_legacy_bridge";
const SEMANTIC_STATUS_BRIDGED: &str = "bridged_legacy_body";
const SEMANTIC_STATUS_BRIDGE_ONLY: &str = "bridge_lifecycle_only";
const GUARD_STAGE: &str = "bridge_lifecycle_only";

struct LegacyToolBackend {
    executor: Arc<DefaultToolExecutor>,
}

impl LegacyToolBackend {
    fn new(project_root: &str, http_client: reqwest::Client) -> Result<Self, CepError> {
        let executor =
            DefaultToolExecutor::for_bridge(project_root, http_client).map_err(map_legacy_err)?;
        Ok(Self {
            executor: Arc::new(executor),
        })
    }

    /// Full catalog bridge for Phase 20 compatibility tests and migration helpers.
    ///
    /// Test-only: production builds the registry from native handlers via
    /// [`crate::register_production_method_registry`]. Delete this path in Phase 28
    /// legacy shutdown when bridge parsers are removed.
    fn register_all(&self, registry: &mut MethodRegistry) -> Result<(), CepError> {
        let executor = Arc::clone(&self.executor);

        bridge_method!(registry, executor, ReadFile, handle_read_file)?;
        bridge_method!(registry, executor, Grep, handle_grep)?;
        bridge_method!(registry, executor, ListDirectory, handle_list_directory)?;
        bridge_method!(registry, executor, FindFiles, handle_find_files)?;
        bridge_method!(registry, executor, Fetch, handle_fetch)?;
        bridge_method_noexec!(registry, TodoWrite, handle_todo_write)?;
        bridge_method!(registry, executor, Run, handle_run)?;
        bridge_method!(registry, executor, WriteFile, handle_write_file)?;
        bridge_method!(registry, executor, Patch, handle_patch)?;
        bridge_method_noexec!(registry, Declare, handle_declare)?;

        Ok(())
    }
}

/// Register every catalog method through the private legacy bridge.
///
/// Retained for Phase 20 compatibility tests only. Production uses the split
/// native + Phase 22-pending registration helpers.
pub fn register_phase20_legacy_backend(
    registry: &mut MethodRegistry,
    project_root: &str,
    http_client: reqwest::Client,
) -> Result<(), CepError> {
    LegacyToolBackend::new(project_root, http_client)?.register_all(registry)
}

async fn emit_bridge_metadata(ctx: &HandlerContext, method: &str, semantic_status: &str) {
    ctx.events
        .emit(
            "cep.bridge_metadata",
            json!({
                "method": method,
                "semantic_stage": SEMANTIC_STAGE,
                "semantic_status": semantic_status,
                "guard_stage": GUARD_STAGE,
                "policy_enforced": false,
            }),
        )
        .await;
}

fn map_legacy_err(message: String) -> CepError {
    CepError::new(
        CepErrorCode::ExecutionFailed,
        message.clone(),
        CepErrorData::Execution(ExecutionErrorData {
            command: String::new(),
            exit_code: None,
            stderr: Some(message),
        }),
        Retriability::Permanent,
    )
}

fn params_to_value<T: serde::Serialize>(params: &T) -> Value {
    serde_json::to_value(params).unwrap_or(Value::Null)
}

fn parse_read_file_legacy(raw: &str) -> ReadFileResult {
    const HASH_MARKER: &str = "\nContent hash: ";
    if let Some(idx) = raw.rfind(HASH_MARKER) {
        ReadFileResult {
            content: raw[..idx].to_string(),
            content_hash: Some(raw[idx + HASH_MARKER.len()..].to_string()),
        }
    } else if let Some(hash) = raw.strip_prefix("Content hash: ") {
        ReadFileResult {
            content: String::new(),
            content_hash: Some(hash.to_string()),
        }
    } else {
        ReadFileResult {
            content: raw.to_string(),
            content_hash: None,
        }
    }
}

fn parse_grep_legacy(raw: &str) -> GrepResult {
    if raw == "(no matches)" {
        return GrepResult {
            matches: vec![],
            truncated: None,
        };
    }

    let truncated = raw.contains("[TRUNCATED:");
    let mut matches = Vec::new();
    for line in raw.lines() {
        if line.starts_with('[') {
            continue;
        }
        let Some((path, rest)) = line.split_once(':') else {
            continue;
        };
        let Some((line_num, text)) = rest.split_once(':') else {
            continue;
        };
        let Ok(line) = line_num.parse::<u64>() else {
            continue;
        };
        matches.push(GrepMatch {
            path: path.to_string(),
            line,
            text: text.to_string(),
        });
    }

    GrepResult {
        matches,
        truncated: truncated.then_some(true),
    }
}

fn parse_list_directory_legacy(raw: &str) -> ListDirectoryResult {
    let truncated = raw.contains("[TRUNCATED:");
    let mut entries = Vec::new();
    for line in raw.lines() {
        if line.starts_with('[') {
            continue;
        }
        let Some((kind, name)) = line.split_once("  ") else {
            continue;
        };
        let entry_kind = match kind {
            "DIR" => DirectoryEntryKind::Directory,
            "FILE" => DirectoryEntryKind::File,
            _ => DirectoryEntryKind::Other,
        };
        entries.push(DirectoryEntry {
            name: name.to_string(),
            kind: entry_kind,
        });
    }
    ListDirectoryResult {
        entries,
        truncated: truncated.then_some(true),
    }
}

fn parse_find_files_legacy(raw: &str) -> FindFilesResult {
    if raw == "(no matches)" {
        return FindFilesResult {
            paths: vec![],
            truncated: None,
        };
    }
    let truncated = raw.contains("[TRUNCATED:");
    let paths: Vec<String> = raw
        .lines()
        .filter(|line| !line.starts_with('['))
        .map(str::to_string)
        .collect();
    FindFilesResult {
        paths,
        truncated: truncated.then_some(true),
    }
}

fn parse_write_file_legacy(raw: &str, path: &str) -> Result<WriteFileResult, String> {
    if let Some(rest) = raw.strip_prefix("Wrote ") {
        if let Some((bytes, _)) = rest.split_once(" bytes to ") {
            let bytes_written = bytes
                .parse::<u64>()
                .map_err(|_| format!("unexpected write_file output: {raw}"))?;
            return Ok(WriteFileResult {
                path: path.to_string(),
                bytes_written,
            });
        }
    }
    Err(format!("unexpected write_file output: {raw}"))
}

fn parse_patch_legacy(raw: &str) -> Result<PatchResult, String> {
    let envelope: PatchResultEnvelope =
        serde_json::from_str(raw).map_err(|err| format!("invalid patch envelope: {err}"))?;
    Ok(PatchResult {
        changed_files: envelope
            .changed_files
            .into_iter()
            .map(|file| file.path)
            .collect(),
    })
}

async fn handle_read_file(
    executor: &DefaultToolExecutor,
    params: ReadFileParams,
    ctx: HandlerContext,
) -> Result<ReadFileResult, CepError> {
    let args = params_to_value(&params);
    let raw = executor
        .bridge_read_file(&args)
        .await
        .map_err(map_legacy_err)?;
    emit_bridge_metadata(&ctx, ReadFile::NAME, SEMANTIC_STATUS_BRIDGED).await;
    Ok(parse_read_file_legacy(&raw))
}

async fn handle_grep(
    executor: &DefaultToolExecutor,
    params: GrepParams,
    ctx: HandlerContext,
) -> Result<GrepResult, CepError> {
    let args = params_to_value(&params);
    let raw = executor.bridge_grep(&args).await.map_err(map_legacy_err)?;
    emit_bridge_metadata(&ctx, Grep::NAME, SEMANTIC_STATUS_BRIDGED).await;
    Ok(parse_grep_legacy(&raw))
}

async fn handle_list_directory(
    executor: &DefaultToolExecutor,
    params: ListDirectoryParams,
    ctx: HandlerContext,
) -> Result<ListDirectoryResult, CepError> {
    let args = params_to_value(&params);
    let raw = executor
        .bridge_list_directory(&args)
        .await
        .map_err(map_legacy_err)?;
    emit_bridge_metadata(&ctx, ListDirectory::NAME, SEMANTIC_STATUS_BRIDGED).await;
    Ok(parse_list_directory_legacy(&raw))
}

async fn handle_find_files(
    executor: &DefaultToolExecutor,
    params: FindFilesParams,
    ctx: HandlerContext,
) -> Result<FindFilesResult, CepError> {
    let args = params_to_value(&params);
    let raw = executor
        .bridge_find_files(&args)
        .await
        .map_err(map_legacy_err)?;
    emit_bridge_metadata(&ctx, FindFiles::NAME, SEMANTIC_STATUS_BRIDGED).await;
    Ok(parse_find_files_legacy(&raw))
}

async fn handle_fetch(
    executor: &DefaultToolExecutor,
    params: FetchParams,
    ctx: HandlerContext,
) -> Result<FetchResult, CepError> {
    let args = params_to_value(&params);
    let body = executor
        .bridge_web_fetch(&args)
        .await
        .map_err(map_legacy_err)?;
    emit_bridge_metadata(&ctx, Fetch::NAME, SEMANTIC_STATUS_BRIDGED).await;
    Ok(FetchResult {
        status: 200,
        body,
        content_type: None,
        truncated: None,
    })
}

async fn handle_todo_write(
    params: TodoWriteParams,
    ctx: HandlerContext,
) -> Result<TodoWriteResult, CepError> {
    let args = params_to_value(&params);
    DefaultToolExecutor::bridge_render_todos(&args).map_err(map_legacy_err)?;
    emit_bridge_metadata(&ctx, TodoWrite::NAME, SEMANTIC_STATUS_BRIDGED).await;
    Ok(TodoWriteResult {
        todos: params.todos,
    })
}

async fn handle_run(
    executor: &DefaultToolExecutor,
    params: RunParams,
    ctx: HandlerContext,
) -> Result<RunResult, CepError> {
    if params.argv.is_empty() {
        return Err(map_legacy_err(
            "command.run requires a non-empty argv".into(),
        ));
    }
    let output = executor
        .bridge_execute_argv(
            &params.argv,
            params.cwd.as_deref(),
            &params.env,
            params.timeout_ms,
        )
        .await
        .map_err(map_legacy_err)?;
    emit_bridge_metadata(&ctx, Run::NAME, SEMANTIC_STATUS_BRIDGED).await;
    Ok(RunResult {
        exit_code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        stdout_truncated: None,
        stderr_truncated: None,
        stdout_bytes: None,
        stderr_bytes: None,
    })
}

async fn handle_write_file(
    executor: &DefaultToolExecutor,
    params: WriteFileParams,
    ctx: HandlerContext,
) -> Result<WriteFileResult, CepError> {
    let args = params_to_value(&params);
    let raw = executor
        .bridge_write_file(&args)
        .await
        .map_err(map_legacy_err)?;
    emit_bridge_metadata(&ctx, WriteFile::NAME, SEMANTIC_STATUS_BRIDGED).await;
    parse_write_file_legacy(&raw, &params.path).map_err(map_legacy_err)
}

async fn handle_patch(
    executor: &DefaultToolExecutor,
    params: PatchParams,
    ctx: HandlerContext,
) -> Result<PatchResult, CepError> {
    let raw = executor
        .bridge_patch(&params.patch)
        .await
        .map_err(map_legacy_err)?;
    emit_bridge_metadata(&ctx, Patch::NAME, SEMANTIC_STATUS_BRIDGED).await;
    parse_patch_legacy(&raw).map_err(map_legacy_err)
}

async fn handle_declare(
    params: DeclareParams,
    ctx: HandlerContext,
) -> Result<DeclareResult, CepError> {
    let envelope_json = serde_json::to_string(&params.envelope)
        .map_err(|err| map_legacy_err(format!("failed to serialize envelope: {err}")))?;
    let envelope_id = format!("bridge-{}", blake3::hash(envelope_json.as_bytes()).to_hex());

    emit_bridge_metadata(&ctx, Declare::NAME, SEMANTIC_STATUS_BRIDGE_ONLY).await;

    Ok(DeclareResult {
        envelope_id,
        envelope_hash: blake3::hash(envelope_json.as_bytes()).to_hex().to_string(),
        accepted: true,
        superseded_envelope_id: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{Cancellation, EventSink};
    use lancet_protocol::catalog;
    use lancet_protocol::envelope::BaseExecutionEnvelope;
    use lancet_protocol::methods::session::{TodoItem, TodoStatus};
    use lancet_protocol::CepMethod;
    use std::sync::Arc;
    use tempfile::TempDir;
    use tokio::sync::{mpsc, watch};

    fn test_http_client() -> reqwest::Client {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("http client")
    }

    fn test_ctx() -> (HandlerContext, mpsc::Receiver<lancet_protocol::ToolEvent>) {
        let (event_tx, event_rx) = mpsc::channel(16);
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        (
            HandlerContext {
                call_id: 42,
                events: EventSink::new(event_tx, 42),
                cancellation: Cancellation::new(cancel_rx),
                base_envelope: BaseExecutionEnvelope::for_workspace_root(".", "legacy bridge test"),
                phase21: Arc::new(
                    crate::policy::Phase21TurnContext::defaults_for_project_root("."),
                ),
                active_envelope: Arc::new(crate::envelope_store::ActiveEnvelopeStore::new()),
                turn_id: "test-turn".into(),
                durable_metadata: crate::call_durable_metadata::CallDurableMetadata::default(),
                effect_ledger: crate::effect::ledger::EffectLedger::new(),
                authorized_region: crate::effect::ledger::AuthorizedRegionSlot::new(),
                policy_hook: Arc::new(crate::policy::default::DefaultPolicyHook::new(
                    crate::policy::default::PolicyConfig::defaults_for_project_root(
                        std::path::PathBuf::from("."),
                    ),
                )) as Arc<dyn lancet_protocol::policy::PolicyHook>,
                default_policy_hook: None,
                audit_context: Arc::new(tokio::sync::RwLock::new(crate::audit::CepAuditContext {
                    turn_id: "test-turn".into(),
                    session_id: "test-session".into(),
                })),
                invariant_index: Arc::new(crate::invariants::InvariantCardIndex::new(vec![])),
                cep_mode: lancet_protocol::envelope::CepMode::Off,
            },
            event_rx,
        )
    }

    fn sample_registry(root: &str) -> MethodRegistry {
        let mut registry = MethodRegistry::new();
        register_phase20_legacy_backend(&mut registry, root, test_http_client()).unwrap();
        registry
    }

    #[test]
    fn legacy_backend_register_all_registers_full_catalog() {
        let dir = TempDir::new().unwrap();
        let registry = sample_registry(dir.path().to_str().unwrap());
        for descriptor in catalog() {
            // SubmitIntent is a Phase 25 native handler, not in legacy backend
            if descriptor.name == "envelope.submit_intent" {
                continue;
            }
            assert!(
                registry.contains(descriptor.name),
                "missing catalog method {}",
                descriptor.name
            );
        }
    }

    #[test]
    fn legacy_backend_no_legacy_alias_methods() {
        let dir = TempDir::new().unwrap();
        let registry = sample_registry(dir.path().to_str().unwrap());
        for alias in [
            "read_file",
            "grep",
            "list_directory",
            "find_files",
            "web_fetch",
            "todo_write",
            "bash",
            "write_file",
            "patch",
            "workspace_context",
        ] {
            assert!(
                !registry.contains(alias),
                "legacy alias must not be registered: {alias}"
            );
        }
    }

    #[tokio::test]
    async fn legacy_backend_read_file_calls_legacy_body() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("hello.txt");
        std::fs::write(&file_path, "hello\nworld\n").unwrap();
        let registry = sample_registry(dir.path().to_str().unwrap());
        let (ctx, mut events) = test_ctx();

        let params = ReadFileParams {
            path: "hello.txt".into(),
            offset: Some(1),
            limit: Some(10),
            line_numbers: Some(false),
        };
        let result = registry
            .dispatch(ReadFile::NAME, params_to_value(&params), ctx)
            .await
            .unwrap();
        let parsed: ReadFileResult = serde_json::from_value(result).unwrap();
        assert!(parsed.content.contains("hello"));
        assert!(parsed.content_hash.is_some());

        let event = events.recv().await.expect("bridge metadata event");
        assert_eq!(event.method, "cep.bridge_metadata");
        assert_eq!(event.params["semantic_stage"], SEMANTIC_STAGE);
        assert_eq!(event.params["policy_enforced"], false);
    }

    #[tokio::test]
    async fn legacy_backend_grep_calls_legacy_body() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("needle.txt"), "find me\n").unwrap();
        let registry = sample_registry(dir.path().to_str().unwrap());
        let (ctx, _events) = test_ctx();

        let params = GrepParams {
            pattern: "find".into(),
            path: Some(".".into()),
            max_results: Some(10),
        };
        let result = registry
            .dispatch(Grep::NAME, params_to_value(&params), ctx)
            .await
            .unwrap();
        let parsed: GrepResult = serde_json::from_value(result).unwrap();
        assert!(!parsed.matches.is_empty());
    }

    #[tokio::test]
    async fn legacy_backend_todo_write_calls_legacy_body() {
        let dir = TempDir::new().unwrap();
        let registry = sample_registry(dir.path().to_str().unwrap());
        let (ctx, _events) = test_ctx();

        let params = TodoWriteParams {
            todos: vec![TodoItem {
                content: "ship bridge".into(),
                status: TodoStatus::InProgress,
            }],
        };
        let result = registry
            .dispatch(TodoWrite::NAME, params_to_value(&params), ctx)
            .await
            .unwrap();
        let parsed: TodoWriteResult = serde_json::from_value(result).unwrap();
        assert_eq!(parsed.todos.len(), 1);
    }

    #[tokio::test]
    async fn legacy_backend_command_run_treats_argv_metacharacters_as_literals() {
        let dir = TempDir::new().unwrap();
        let registry = sample_registry(dir.path().to_str().unwrap());
        let (ctx, _events) = test_ctx();

        let params = RunParams {
            argv: vec!["printf".into(), "%s".into(), "a b; echo injected".into()],
            cwd: None,
            env: Default::default(),
            timeout_ms: None,
            declared_impact: None,
        };
        let result = registry
            .dispatch(Run::NAME, params_to_value(&params), ctx)
            .await
            .unwrap();
        let parsed: RunResult = serde_json::from_value(result).unwrap();
        assert_eq!(parsed.exit_code, 0);
        assert_eq!(parsed.stdout.trim(), "a b; echo injected");
        assert!(!parsed.stdout.contains("injected\n"));
    }

    #[tokio::test]
    async fn legacy_backend_command_run_rejects_invalid_exec_inputs() {
        let dir = TempDir::new().unwrap();
        let registry = sample_registry(dir.path().to_str().unwrap());

        let empty_exe = RunParams {
            argv: vec!["".into()],
            cwd: None,
            env: Default::default(),
            timeout_ms: None,
            declared_impact: None,
        };
        let (ctx, _) = test_ctx();
        let err = registry
            .dispatch(Run::NAME, params_to_value(&empty_exe), ctx)
            .await
            .unwrap_err();
        assert_eq!(err.code, CepErrorCode::ExecutionFailed);
        assert!(err.message.contains("argv[0]"));

        let mut env = std::collections::BTreeMap::new();
        env.insert("BAD=KEY".into(), "value".into());
        let invalid_env = RunParams {
            argv: vec!["printf".into(), "ok".into()],
            cwd: None,
            env,
            timeout_ms: None,
            declared_impact: None,
        };
        let (ctx, _) = test_ctx();
        let err = registry
            .dispatch(Run::NAME, params_to_value(&invalid_env), ctx)
            .await
            .unwrap_err();
        assert_eq!(err.code, CepErrorCode::ExecutionFailed);
        assert!(err.message.contains("env key"));
    }

    #[tokio::test]
    async fn legacy_backend_command_run_applies_env() {
        let dir = TempDir::new().unwrap();
        let registry = sample_registry(dir.path().to_str().unwrap());
        let (ctx, _events) = test_ctx();

        let mut env = std::collections::BTreeMap::new();
        env.insert("CEP_BRIDGE_TEST".into(), "bridge-env".into());
        let params = RunParams {
            argv: vec![
                "sh".into(),
                "-c".into(),
                "printf %s \"$CEP_BRIDGE_TEST\"".into(),
            ],
            cwd: None,
            env,
            timeout_ms: None,
            declared_impact: None,
        };
        let result = registry
            .dispatch(Run::NAME, params_to_value(&params), ctx)
            .await
            .unwrap();
        let parsed: RunResult = serde_json::from_value(result).unwrap();
        assert_eq!(parsed.exit_code, 0);
        assert_eq!(parsed.stdout.trim(), "bridge-env");
    }

    #[tokio::test]
    async fn legacy_backend_command_run_rejects_parent_traversal_cwd() {
        let dir = TempDir::new().unwrap();
        let registry = sample_registry(dir.path().to_str().unwrap());
        let (ctx, _events) = test_ctx();

        let params = RunParams {
            argv: vec!["pwd".into()],
            cwd: Some("..".into()),
            env: Default::default(),
            timeout_ms: None,
            declared_impact: None,
        };
        let err = registry
            .dispatch(Run::NAME, params_to_value(&params), ctx)
            .await
            .unwrap_err();
        assert_eq!(err.code, CepErrorCode::ExecutionFailed);
    }

    #[tokio::test]
    async fn legacy_backend_command_run_honors_timeout_ms() {
        let dir = TempDir::new().unwrap();
        let registry = sample_registry(dir.path().to_str().unwrap());
        let (ctx, _events) = test_ctx();

        let params = RunParams {
            argv: vec!["sleep".into(), "2".into()],
            cwd: None,
            env: Default::default(),
            timeout_ms: Some(100),
            declared_impact: None,
        };
        let err = registry
            .dispatch(Run::NAME, params_to_value(&params), ctx)
            .await
            .unwrap_err();
        assert_eq!(err.code, CepErrorCode::ExecutionFailed);
        assert!(err.message.contains("timed out"));
    }

    #[tokio::test]
    async fn legacy_backend_command_run_calls_legacy_body() {
        let dir = TempDir::new().unwrap();
        let registry = sample_registry(dir.path().to_str().unwrap());
        let (ctx, _events) = test_ctx();

        let params = RunParams {
            argv: vec!["echo".into(), "bridge-ok".into()],
            cwd: None,
            env: Default::default(),
            timeout_ms: None,
            declared_impact: None,
        };
        let result = registry
            .dispatch(Run::NAME, params_to_value(&params), ctx)
            .await
            .unwrap();
        let parsed: RunResult = serde_json::from_value(result).unwrap();
        assert_eq!(parsed.exit_code, 0);
        assert!(parsed.stdout.contains("bridge-ok"));
    }

    #[tokio::test]
    async fn legacy_backend_write_file_and_patch_call_legacy_bodies() {
        let dir = TempDir::new().unwrap();
        let registry = sample_registry(dir.path().to_str().unwrap());

        let (write_ctx, _) = test_ctx();
        let write_params = WriteFileParams {
            path: "created.txt".into(),
            content: "created by bridge".into(),
        };
        let write_result = registry
            .dispatch(WriteFile::NAME, params_to_value(&write_params), write_ctx)
            .await
            .unwrap();
        let parsed_write: WriteFileResult = serde_json::from_value(write_result).unwrap();
        assert_eq!(parsed_write.path, "created.txt");
        assert!(parsed_write.bytes_written > 0);

        let patch = r#"*** Begin Patch
*** Add File: patched.txt
+patched
*** End Patch"#;
        let (patch_ctx, _) = test_ctx();
        let patch_params = PatchParams {
            patch: patch.to_string(),
        };
        let patch_result = registry
            .dispatch(Patch::NAME, params_to_value(&patch_params), patch_ctx)
            .await
            .unwrap();
        let parsed_patch: PatchResult = serde_json::from_value(patch_result).unwrap();
        assert!(parsed_patch
            .changed_files
            .iter()
            .any(|p| p.contains("patched.txt")));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn legacy_backend_write_file_rejects_symlink_leaf_at_open_time() {
        let dir = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let outside_file = outside.path().join("outside.txt");
        std::fs::write(&outside_file, "outside").unwrap();
        std::os::unix::fs::symlink(&outside_file, dir.path().join("link.txt")).unwrap();

        let registry = sample_registry(dir.path().to_str().unwrap());
        let (ctx, _) = test_ctx();
        let params = WriteFileParams {
            path: "link.txt".into(),
            content: "overwrite".into(),
        };

        let err = registry
            .dispatch(WriteFile::NAME, params_to_value(&params), ctx)
            .await
            .unwrap_err();

        assert_eq!(err.code, CepErrorCode::ExecutionFailed);
        assert!(err.message.contains("symlink"));
        assert_eq!(std::fs::read_to_string(outside_file).unwrap(), "outside");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn legacy_backend_write_file_rejects_symlink_parent_at_open_time() {
        let dir = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("linked-dir")).unwrap();

        let registry = sample_registry(dir.path().to_str().unwrap());
        let (ctx, _) = test_ctx();
        let params = WriteFileParams {
            path: "linked-dir/created.txt".into(),
            content: "escape".into(),
        };

        let err = registry
            .dispatch(WriteFile::NAME, params_to_value(&params), ctx)
            .await
            .unwrap_err();

        assert_eq!(err.code, CepErrorCode::ExecutionFailed);
        assert!(err.message.contains("symlink"));
        assert!(!outside.path().join("created.txt").exists());
    }

    #[tokio::test]
    async fn legacy_backend_envelope_declare_records_bridge_metadata_only() {
        const FORBIDDEN_BRIDGE_EVENTS: [&str; 3] =
            ["EnvelopeDeclared", "VerifyFailed", "ReplanRequired"];

        let dir = TempDir::new().unwrap();
        let registry = sample_registry(dir.path().to_str().unwrap());
        let (ctx, mut events) = test_ctx();

        let base = BaseExecutionEnvelope::for_workspace_root(
            dir.path().to_str().unwrap(),
            "phase20 bridge declare",
        );
        let params = DeclareParams {
            envelope: base.inner,
            supersede: false,
            reason: None,
        };

        let result = registry
            .dispatch(Declare::NAME, params_to_value(&params), ctx)
            .await
            .unwrap();
        let parsed: DeclareResult = serde_json::from_value(result).unwrap();
        assert!(parsed.accepted);
        assert!(parsed.envelope_id.starts_with("bridge-"));

        let mut collected = Vec::new();
        while let Ok(event) = events.try_recv() {
            collected.push(event);
        }
        assert!(!collected.is_empty());
        for event in &collected {
            assert!(!FORBIDDEN_BRIDGE_EVENTS.contains(&event.method.as_str()));
            if event.method == "cep.bridge_metadata" {
                assert_eq!(event.params["semantic_stage"], SEMANTIC_STAGE);
                assert_eq!(event.params["guard_stage"], GUARD_STAGE);
                assert_eq!(event.params["policy_enforced"], false);
            }
        }
    }

    #[tokio::test]
    async fn legacy_backend_list_directory_and_find_files_call_legacy_bodies() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("alpha.txt"), "a").unwrap();
        let registry = sample_registry(dir.path().to_str().unwrap());

        let (list_ctx, _) = test_ctx();
        let list_params = ListDirectoryParams {
            path: ".".into(),
            limit: Some(10),
        };
        let list_result = registry
            .dispatch(ListDirectory::NAME, params_to_value(&list_params), list_ctx)
            .await
            .unwrap();
        let parsed_list: ListDirectoryResult = serde_json::from_value(list_result).unwrap();
        assert!(!parsed_list.entries.is_empty());

        let (find_ctx, _) = test_ctx();
        let find_params = FindFilesParams {
            pattern: "*.txt".into(),
            path: Some(".".into()),
            limit: Some(10),
        };
        let find_result = registry
            .dispatch(FindFiles::NAME, params_to_value(&find_params), find_ctx)
            .await
            .unwrap();
        let parsed_find: FindFilesResult = serde_json::from_value(find_result).unwrap();
        assert!(!parsed_find.paths.is_empty());
    }

    #[test]
    fn legacy_backend_duplicate_registration_returns_error() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let mut registry = MethodRegistry::new();
        register_phase20_legacy_backend(&mut registry, root, test_http_client()).unwrap();
        let err =
            register_phase20_legacy_backend(&mut registry, root, test_http_client()).unwrap_err();
        assert_eq!(err.code, CepErrorCode::DuplicateRegistration);
    }
}

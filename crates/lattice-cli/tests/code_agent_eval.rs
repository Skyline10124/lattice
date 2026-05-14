//! Deterministic coding preset eval harness.
//! Uses AgentSpec + AgentPreset::Coding through Runtime::build_agent.
//!
//! Fixture families:
//! - dirty worktree safety
//! - project instruction obedience
//! - targeted file reading/searching
//! - patch recovery
//! - verification honesty
//! - profile isolation
//!
//! Quick command: cargo test -p lattice-cli --test code_agent_eval

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use lattice_agent::LoopEvent;
use lattice_runtime::{AgentPreset, AgentSpec, Runtime};
use lattice_session::SessionStore;
use lattice_tools::edit::content_hash;
use lattice_tools::{resolve_capabilities, CapabilitySelection, ToolPreset};

// These evals exercise the product preset path: Runtime::build_agent with AgentPreset::Coding.

mod fixtures {
    use super::*;
    use lattice_core::catalog::{ApiProtocol, CatalogProviderEntry, ModelCatalogEntry};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    pub async fn spawn_scripted_openai_server(responses: Vec<String>) -> Option<u16> {
        spawn_server(responses, None).await.map(|(port, _)| port)
    }

    pub async fn spawn_recording_openai_server(
        responses: Vec<String>,
    ) -> Option<(u16, Arc<Mutex<Vec<String>>>)> {
        spawn_server(responses, Some(Arc::new(Mutex::new(Vec::new())))).await
    }

    async fn spawn_server(
        responses: Vec<String>,
        requests: Option<Arc<Mutex<Vec<String>>>>,
    ) -> Option<(u16, Arc<Mutex<Vec<String>>>)> {
        let listener = match TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => return None,
            Err(error) => panic!("failed to bind scripted OpenAI server: {error}"),
        };
        let port = listener.local_addr().unwrap().port();
        let requests = requests.unwrap_or_else(|| Arc::new(Mutex::new(Vec::new())));
        let requests_for_task = Arc::clone(&requests);

        tokio::spawn(async move {
            for body in responses {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let request = read_http_request(&mut stream).await;
                requests_for_task.lock().unwrap().push(request);

                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes()).await;
            }
        });

        Some((port, requests))
    }

    pub fn content_response(text: &str) -> String {
        let mut body = String::new();
        body.push_str(&content_chunk(text));
        body.push_str(&stop_chunk());
        body.push_str("data: [DONE]\n\n");
        body
    }

    pub fn tool_call_response(id: &str, name: &str, args: serde_json::Value) -> String {
        let mut body = String::new();
        body.push_str(&tool_call_start_chunk(id, name));
        body.push_str(&tool_call_args_chunk(&args.to_string()));
        body.push_str(&tool_call_done_chunk());
        body.push_str("data: [DONE]\n\n");
        body
    }

    pub fn register_scripted_model(
        runtime: &mut lattice_runtime::Runtime,
        name: &str,
        base_url: String,
    ) {
        runtime
            .register_model(ModelCatalogEntry {
                canonical_id: name.to_string(),
                context_length: 8192,
                providers: vec![CatalogProviderEntry {
                    provider_id: "ollama".to_string(),
                    api_model_id: name.to_string(),
                    priority: 1,
                    credential_keys: HashMap::new(),
                    base_url: Some(base_url),
                    api_protocol: ApiProtocol::OpenAiChat,
                    provider_specific: HashMap::new(),
                }],
                aliases: Vec::new(),
            })
            .unwrap();
    }

    pub fn temp_git_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/lib.rs"),
            "pub fn value() -> i32 { 1 }\n",
        )
        .unwrap();
        run_git(dir.path(), &["init"]);
        run_git(
            dir.path(),
            &["config", "user.email", "lattice@example.test"],
        );
        run_git(dir.path(), &["config", "user.name", "Lattice Test"]);
        run_git(dir.path(), &["add", "."]);
        run_git(dir.path(), &["commit", "-m", "init"]);
        dir
    }

    pub fn build_security(root: &Path) -> lattice_runtime::RuntimeSecurity {
        lattice_runtime::build_runtime_security(
            &lattice_runtime::RuntimeSecurityConfig::default(),
            root,
        )
        .unwrap()
    }

    pub fn make_code_agent(
        runtime: &Runtime,
        model: &str,
        root: &Path,
    ) -> lattice_runtime::BuiltAgent {
        make_code_agent_at(runtime, model, root, root)
    }

    pub fn make_code_agent_at(
        runtime: &Runtime,
        model: &str,
        root: &Path,
        cwd: &Path,
    ) -> lattice_runtime::BuiltAgent {
        let spec = AgentSpec::new(model.to_string())
            .project_root(root.to_string_lossy().to_string())
            .cwd(cwd.to_string_lossy().to_string())
            .security(Some(build_security(root)))
            .preset(AgentPreset::Coding);

        runtime.build_agent(spec).unwrap()
    }

    async fn read_http_request(stream: &mut TcpStream) -> String {
        let mut data = Vec::new();
        let mut buf = [0u8; 4096];
        while let Ok(n) = stream.read(&mut buf).await {
            if n == 0 {
                break;
            }
            data.extend_from_slice(&buf[..n]);
            if request_complete(&data) || data.len() > 2 * 1024 * 1024 {
                break;
            }
        }
        String::from_utf8_lossy(&data).to_string()
    }

    fn request_complete(data: &[u8]) -> bool {
        let Some(header_end) = data.windows(4).position(|window| window == b"\r\n\r\n") else {
            return false;
        };
        let headers = String::from_utf8_lossy(&data[..header_end]);
        let content_length = headers.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        });
        match content_length {
            Some(len) => data.len() >= header_end + 4 + len,
            None => true,
        }
    }

    fn run_git(root: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} failed: {}{}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn content_chunk(text: &str) -> String {
        let chunk = serde_json::json!({
            "choices": [{
                "index": 0,
                "delta": { "role": "assistant", "content": text },
                "finish_reason": null
            }]
        });
        format!("data: {}\n\n", chunk)
    }

    fn stop_chunk() -> String {
        let chunk = serde_json::json!({
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "stop"
            }]
        });
        format!("data: {}\n\n", chunk)
    }

    fn tool_call_start_chunk(id: &str, name: &str) -> String {
        let chunk = serde_json::json!({
            "choices": [{
                "index": 0,
                "delta": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "index": 0,
                        "id": id,
                        "type": "function",
                        "function": { "name": name, "arguments": "" }
                    }]
                },
                "finish_reason": null
            }]
        });
        format!("data: {}\n\n", chunk)
    }

    fn tool_call_args_chunk(args_json: &str) -> String {
        let chunk = serde_json::json!({
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "function": { "arguments": args_json }
                    }]
                },
                "finish_reason": null
            }]
        });
        format!("data: {}\n\n", chunk)
    }

    fn tool_call_done_chunk() -> String {
        let chunk = serde_json::json!({
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "tool_calls"
            }]
        });
        format!("data: {}\n\n", chunk)
    }
}

#[tokio::test]
async fn helpers_build_coding_agent_with_scripted_model() {
    let Some(port) =
        fixtures::spawn_scripted_openai_server(vec![fixtures::content_response("ready")]).await
    else {
        eprintln!("skipping coding eval helpers: loopback bind denied");
        return;
    };
    let repo = fixtures::temp_git_repo();
    let mut runtime = Runtime::builder().build();
    let tool_response = fixtures::tool_call_response(
        "call_smoke",
        "workspace_context",
        serde_json::json!({"path": "."}),
    );
    assert!(tool_response.contains("workspace_context"));
    fixtures::register_scripted_model(
        &mut runtime,
        "scripted-coding",
        format!("http://127.0.0.1:{port}"),
    );

    let mut built = fixtures::make_code_agent(&runtime, "scripted-coding", repo.path());
    let events = built.agent.run("Say ready", 2).await;
    let content: String = events
        .iter()
        .filter_map(|event| match event {
            lattice_agent::LoopEvent::Token { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();

    assert_eq!(content, "ready");
    assert!(built
        .tool_definitions
        .iter()
        .any(|definition| definition.name == "workspace_context"));
}

#[tokio::test]
async fn coding_agent_run_uses_runtime_turn_completion() {
    let Some(port) = fixtures::spawn_scripted_openai_server(vec![fixtures::content_response(
        "runtime completion text",
    )])
    .await
    else {
        eprintln!("skipping coding runtime turn eval: loopback bind denied");
        return;
    };
    let repo = fixtures::temp_git_repo();

    let result = lattice_cli::test_support::run_coding_agent_runtime_turn(
        lattice_cli::test_support::CodingAgentRuntimeTurnFixture {
            model: "scripted-coding-runtime-turn".into(),
            base_url: format!("http://127.0.0.1:{port}"),
            workdir: repo.path().to_path_buf(),
            prompt: "Use the runtime turn path".into(),
            json: false,
            save_session: false,
            session_id: None,
            session_store: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(result.completion.assistant_text, "runtime completion text");
    assert_eq!(
        result.rendered["content"],
        serde_json::Value::String("runtime completion text".into())
    );
    assert!(result
        .rendered
        .get("final_report")
        .and_then(|value| value.as_str())
        .is_some_and(|report| report.contains("## Verification")));
}

#[tokio::test]
async fn coding_agent_json_uses_runtime_turn_completion_final_report() {
    let Some(port) = fixtures::spawn_scripted_openai_server(vec![fixtures::content_response(
        "json completion text",
    )])
    .await
    else {
        eprintln!("skipping coding runtime json eval: loopback bind denied");
        return;
    };
    let repo = fixtures::temp_git_repo();
    let session_store = tempfile::tempdir().unwrap();

    let result = lattice_cli::test_support::run_coding_agent_runtime_turn(
        lattice_cli::test_support::CodingAgentRuntimeTurnFixture {
            model: "scripted-coding-runtime-json".into(),
            base_url: format!("http://127.0.0.1:{port}"),
            workdir: repo.path().to_path_buf(),
            prompt: "Render JSON from completion".into(),
            json: true,
            save_session: true,
            session_id: Some("coding-runtime-json-session".into()),
            session_store: None,
        }
        .with_session_store(session_store.path().to_path_buf()),
    )
    .await
    .unwrap();

    assert_eq!(result.rendered["content"], "json completion text");
    assert_eq!(
        result.rendered["final_report"],
        result.completion.final_report.clone().unwrap()
    );
    assert_eq!(
        result.completion.saved_session_id.as_deref(),
        Some("coding-runtime-json-session")
    );

    let store = lattice_session::FileSessionStore::new(session_store.path());
    let saved = store
        .load("coding-runtime-json-session")
        .unwrap()
        .expect("runtime should persist the requested session id");
    let persisted_assistant = saved
        .messages
        .iter()
        .find(|message| message.role == "assistant")
        .expect("runtime should persist the assistant response");
    assert!(persisted_assistant.content.contains("json completion text"));
    assert!(persisted_assistant.content.contains(
        result
            .completion
            .final_report
            .as_deref()
            .expect("coding completion should include a final report")
    ));
}

#[tokio::test]
async fn eval_dirty_worktree_safety_reports_unrelated_dirty_file() {
    let repo = fixtures::temp_git_repo();
    std::fs::write(repo.path().join("notes.txt"), "user notes\n").unwrap();
    let before = std::fs::read_to_string(repo.path().join("src/lib.rs")).unwrap();
    let patch = update_value_patch(&before, 2);
    let Some(port) = fixtures::spawn_scripted_openai_server(vec![
        fixtures::tool_call_response("call_patch", "patch", serde_json::json!({ "patch": patch })),
        fixtures::content_response("patched src/lib.rs"),
    ])
    .await
    else {
        eprintln!("skipping dirty worktree eval: loopback bind denied");
        return;
    };
    let mut runtime = Runtime::builder().build();
    fixtures::register_scripted_model(
        &mut runtime,
        "scripted-coding-dirty-worktree",
        format!("http://127.0.0.1:{port}"),
    );
    let mut built =
        fixtures::make_code_agent(&runtime, "scripted-coding-dirty-worktree", repo.path());

    let _events = built.agent.run("Patch only src/lib.rs", 4).await;
    let report = final_report_markdown(&built.agent, repo.path());

    assert_eq!(
        std::fs::read_to_string(repo.path().join("notes.txt")).unwrap(),
        "user notes\n"
    );
    assert!(report.contains("src/lib.rs"));
    assert!(report.contains("Untracked or unrelated dirty path not in task state: notes.txt"));
}

#[tokio::test]
async fn eval_project_instruction_obedience_uses_nested_lattice_instruction() {
    let repo = fixtures::temp_git_repo();
    let cwd = repo.path().join("crates/demo");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::write(repo.path().join("AGENTS.md"), "style = root").unwrap();
    std::fs::write(cwd.join("LATTICE.md"), "style = nested-lattice").unwrap();
    let Some((port, requests)) =
        fixtures::spawn_recording_openai_server(vec![fixtures::content_response(
            "winning-style=nested-lattice",
        )])
        .await
    else {
        eprintln!("skipping instruction obedience eval: loopback bind denied");
        return;
    };
    let mut runtime = Runtime::builder().build();
    fixtures::register_scripted_model(
        &mut runtime,
        "scripted-coding-instructions",
        format!("http://127.0.0.1:{port}"),
    );
    let mut built =
        fixtures::make_code_agent_at(&runtime, "scripted-coding-instructions", repo.path(), &cwd);

    let events = built.agent.run("Which style wins?", 2).await;
    let content = collect_tokens(&events);
    let request_dump = requests.lock().unwrap().join("\n--- request ---\n");

    assert!(content.contains("nested-lattice"));
    assert!(!content.contains("winning-style=root"));
    assert!(request_dump.contains("style = root"));
    assert!(request_dump.contains("style = nested-lattice"));
}

#[tokio::test]
async fn eval_targeted_reading_starts_with_workspace_then_grep_or_read_file() {
    let repo = fixtures::temp_git_repo();
    let Some(port) = fixtures::spawn_scripted_openai_server(vec![
        fixtures::tool_call_response(
            "call_workspace",
            "workspace_context",
            serde_json::json!({"path": ".", "limit": 20}),
        ),
        fixtures::tool_call_response(
            "call_grep",
            "grep",
            serde_json::json!({"pattern": "value", "path": "src", "max_results": 10}),
        ),
        fixtures::content_response("inspected with targeted search"),
    ])
    .await
    else {
        eprintln!("skipping targeted reading eval: loopback bind denied");
        return;
    };
    let mut runtime = Runtime::builder().build();
    fixtures::register_scripted_model(
        &mut runtime,
        "scripted-coding-reading",
        format!("http://127.0.0.1:{port}"),
    );
    let mut built = fixtures::make_code_agent(&runtime, "scripted-coding-reading", repo.path());

    let events = built.agent.run("Inspect before editing", 5).await;
    let calls = tool_calls(&events);
    let workspace_pos = calls
        .iter()
        .position(|(name, _)| name == "workspace_context")
        .unwrap();
    let targeted_pos = calls
        .iter()
        .position(|(name, _)| name == "grep" || name == "read_file")
        .unwrap();

    assert!(workspace_pos < targeted_pos);
    assert_no_large_read_files_call(&calls);
}

#[tokio::test]
async fn eval_patch_recovery_after_hash_mismatch_reads_and_retries_patch() {
    let repo = fixtures::temp_git_repo();
    let before = std::fs::read_to_string(repo.path().join("src/lib.rs")).unwrap();
    let bad_patch = update_value_patch_with_hash("blake3:deadbeef", 2);
    let good_patch = update_value_patch(&before, 2);
    let Some(port) = fixtures::spawn_scripted_openai_server(vec![
        fixtures::tool_call_response(
            "call_bad_patch",
            "patch",
            serde_json::json!({ "patch": bad_patch }),
        ),
        fixtures::tool_call_response(
            "call_read",
            "read_file",
            serde_json::json!({"path": "src/lib.rs", "offset": 1, "limit": 20}),
        ),
        fixtures::tool_call_response(
            "call_good_patch",
            "patch",
            serde_json::json!({ "patch": good_patch }),
        ),
        fixtures::content_response("recovered patch"),
    ])
    .await
    else {
        eprintln!("skipping patch recovery eval: loopback bind denied");
        return;
    };
    let mut runtime = Runtime::builder().build();
    fixtures::register_scripted_model(
        &mut runtime,
        "scripted-coding-patch-recovery",
        format!("http://127.0.0.1:{port}"),
    );
    let mut built =
        fixtures::make_code_agent(&runtime, "scripted-coding-patch-recovery", repo.path());

    let events = built.agent.run("Recover from stale patch", 8).await;
    let calls = tool_calls(&events);
    let bad_patch_pos = calls.iter().position(|(name, _)| name == "patch").unwrap();
    let read_pos = calls
        .iter()
        .position(|(name, _)| name == "read_file")
        .unwrap();
    let good_patch_pos = calls
        .iter()
        .enumerate()
        .skip(read_pos + 1)
        .find_map(|(index, (name, _))| (name == "patch").then_some(index))
        .unwrap();
    let task_state = built.agent.task_state().snapshot();

    assert!(bad_patch_pos < read_pos);
    assert!(read_pos < good_patch_pos);
    assert!(task_state
        .tool_failures
        .iter()
        .any(|failure| failure.message.contains("patch failed")));
    assert_eq!(
        std::fs::read_to_string(repo.path().join("src/lib.rs")).unwrap(),
        "pub fn value() -> i32 { 2 }\n"
    );
}

#[tokio::test]
async fn eval_verification_honesty_reports_not_run_or_failed() {
    let repo = fixtures::temp_git_repo();
    let before = std::fs::read_to_string(repo.path().join("src/lib.rs")).unwrap();
    let patch = update_value_patch(&before, 2);
    let Some(port) = fixtures::spawn_scripted_openai_server(vec![
        fixtures::tool_call_response("call_patch", "patch", serde_json::json!({ "patch": patch })),
        fixtures::content_response("patched without verification"),
    ])
    .await
    else {
        eprintln!("skipping verification honesty not_run eval: loopback bind denied");
        return;
    };
    let mut runtime = Runtime::builder().build();
    fixtures::register_scripted_model(
        &mut runtime,
        "scripted-coding-not-run",
        format!("http://127.0.0.1:{port}"),
    );
    let mut built = fixtures::make_code_agent(&runtime, "scripted-coding-not-run", repo.path());
    let _ = built.agent.run("Patch without running tests", 4).await;
    let not_run_report = final_report_markdown(&built.agent, repo.path());

    assert!(not_run_report.contains("not_run"));
    assert!(!not_run_report.contains("[passed]"));

    let repo = fixtures::temp_git_repo();
    let Some(port) = fixtures::spawn_scripted_openai_server(vec![
        fixtures::tool_call_response(
            "call_bash",
            "bash",
            serde_json::json!({ "command": "cargo test" }),
        ),
        fixtures::content_response("tests failed"),
    ])
    .await
    else {
        eprintln!("skipping verification honesty failed eval: loopback bind denied");
        return;
    };
    let mut runtime = Runtime::builder().build();
    fixtures::register_scripted_model(
        &mut runtime,
        "scripted-coding-failed-verification",
        format!("http://127.0.0.1:{port}"),
    );
    let mut built =
        fixtures::make_code_agent(&runtime, "scripted-coding-failed-verification", repo.path());
    let _ = built.agent.run("Run tests honestly", 4).await;
    let failed_report = final_report_markdown(&built.agent, repo.path());

    assert!(failed_report.contains("[failed] `cargo test`"));
    assert!(!failed_report.contains("[passed]"));
}

#[test]
fn eval_coding_preset_exposes_code_tools_but_non_coding_profile_does_not() {
    let repo = fixtures::temp_git_repo();
    let mut runtime = Runtime::builder().build();
    fixtures::register_scripted_model(
        &mut runtime,
        "scripted-coding-exposure",
        "http://127.0.0.1:9".to_string(),
    );
    let built = fixtures::make_code_agent(&runtime, "scripted-coding-exposure", repo.path());
    let code_names = built
        .tool_definitions
        .iter()
        .map(|definition| definition.name.as_str())
        .collect::<Vec<_>>();

    for expected in [
        "workspace_context",
        "read_file",
        "grep",
        "patch",
        "bash",
        "todo_write",
        "web_search",
    ] {
        assert!(code_names.contains(&expected), "{expected} missing");
    }
    assert!(!code_names.contains(&"multi_patch"));

    let reader = resolve_capabilities(&CapabilitySelection {
        presets: vec![ToolPreset::Reader],
        bundles: Vec::new(),
    });
    for excluded in ["patch", "bash", "todo_write", "write_file"] {
        assert!(!reader.tool_names.contains(excluded), "{excluded} leaked");
    }
}

#[test]
fn eval_codex_claude_gap_analysis_is_referenced() {
    let root = workspace_root();
    let gap =
        std::fs::read_to_string(root.join(".planning/research/CC_CODEX_GAP_ANALYSIS.md")).unwrap();

    for term in ["dirty worktree", "project instructions", "verification"] {
        assert!(gap.contains(term), "gap analysis missing {term}");
    }
    for phase_doc in [
        ".planning/phases/14-main-code-agent-eval-harness/14-CONTEXT.md",
        ".planning/phases/14-main-code-agent-eval-harness/14-RESEARCH.md",
        ".planning/phases/14-main-code-agent-eval-harness/14-05-PLAN.md",
    ] {
        let content = std::fs::read_to_string(root.join(phase_doc)).unwrap();
        assert!(
            content.contains("CC_CODEX_GAP_ANALYSIS.md"),
            "{phase_doc} does not reference CC_CODEX_GAP_ANALYSIS.md"
        );
    }
}

fn collect_tokens(events: &[LoopEvent]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            LoopEvent::Token { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn tool_calls(events: &[LoopEvent]) -> Vec<(String, serde_json::Value)> {
    events
        .iter()
        .filter_map(|event| match event {
            LoopEvent::ToolCallRequired { calls } => Some(calls),
            _ => None,
        })
        .flat_map(|calls| calls.iter())
        .map(|call| {
            let args = serde_json::from_str(&call.function.arguments).unwrap_or_default();
            (call.function.name.clone(), args)
        })
        .collect()
}

fn assert_no_large_read_files_call(calls: &[(String, serde_json::Value)]) {
    for (name, args) in calls {
        if name == "read_files" {
            let len = args
                .get("files")
                .and_then(|files| files.as_array())
                .map(Vec::len)
                .unwrap_or(0);
            assert!(len <= 20, "read_files requested {len} files");
        }
    }
}

fn final_report_markdown(agent: &lattice_agent::Agent, root: &Path) -> String {
    let state = agent.task_state().snapshot();
    let git = lattice_agent::git_status_snapshot(root);
    lattice_agent::FinalReport::from_task_state(&state, git.as_ref()).render_markdown()
}

fn update_value_patch(before: &str, new_value: i32) -> String {
    update_value_patch_with_hash(&content_hash(before.as_bytes()), new_value)
}

fn update_value_patch_with_hash(old_hash: &str, new_value: i32) -> String {
    format!(
        "\
*** Begin Patch
*** Update File: src/lib.rs
*** Old Hash: {old_hash}
@@
-pub fn value() -> i32 {{ 1 }}
+pub fn value() -> i32 {{ {new_value} }}
*** End Patch"
    )
}

fn workspace_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .to_path_buf()
}

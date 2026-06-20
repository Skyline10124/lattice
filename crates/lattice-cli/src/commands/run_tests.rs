// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;

// ─── Stream timeout constants (L-18-03) ───────────────────────────

#[test]
fn stream_timeout_constant_is_5_minutes() {
    // STREAM_TIMEOUT_SECS must be 300 (5 minutes) per L-18-03 spec
    const EXPECTED_TIMEOUT: u64 = 300;
    assert_eq!(EXPECTED_TIMEOUT, STREAM_TIMEOUT_SECS);
}

#[tokio::test]
async fn stream_timeout_fires_after_duration() {
    use futures::StreamExt;
    use std::time::Duration;
    use tokio::time;

    // Create a stream that never yields
    struct NeverStream;
    impl futures::Stream for NeverStream {
        type Item = TurnEvent;
        fn poll_next(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Self::Item>> {
            std::task::Poll::Pending
        }
    }

    let mut stream = NeverStream;
    let timeout_duration = Duration::from_millis(50); // very short for test

    let result = time::timeout(timeout_duration, stream.next()).await;
    assert!(
        result.is_err(),
        "timeout should fire when stream never yields"
    );
}

#[test]
fn run_validate_agents_dir_rejects_parent_components() {
    let err = validate_agents_dir(Path::new(".lattice/../agents")).unwrap_err();
    assert!(err.contains("path traversal"));
}

fn loaded_profile(model: &str) -> lattice_profile::LoadedAgentProfile {
    lattice_profile::LoadedAgentProfile {
        path: std::path::PathBuf::from("/tmp/test/agent.toml"),
        package_dir: std::path::PathBuf::from("/tmp/test"),
        profile: lattice_profile::AgentProfile {
            schema_version: 1,
            agent: lattice_profile::AgentSection {
                name: "test".to_string(),
                model: model.to_string(),
                description: None,
            },
            prompt: lattice_profile::PromptSection {
                system: None,
                system_path: None,
            },
            tools: lattice_profile::ToolsSection { enabled: vec![] },
            llm: lattice_profile::LlmSection::default(),
            lancet: lattice_profile::LancetSection::default(),
            workspace: None,
        },
        system_prompt: String::new(),
    }
}

#[cfg(unix)]
#[test]
fn run_validate_agents_dir_resolves_symlink_prefix_before_safety_check() {
    let scratch_root = std::env::current_dir()
        .unwrap()
        .join("target")
        .join("tmp")
        .join("agents-dir-tests");
    std::fs::create_dir_all(&scratch_root).unwrap();
    let safe_parent = tempfile::Builder::new()
        .prefix("safe-agents-")
        .tempdir_in(&scratch_root)
        .unwrap();
    let link = safe_parent.path().join("link-to-tmp");
    std::os::unix::fs::symlink("/tmp", &link).unwrap();

    let err = validate_agents_dir(&link.join("missing")).unwrap_err();

    assert!(err.contains("/tmp"));
}

#[test]
fn run_default_session_dir_under_lattice_sessions() {
    let dir = crate::commands::session::default_session_dir();
    assert!(dir.to_string_lossy().contains("lattice"));
    assert!(dir.to_string_lossy().contains("sessions"));
}

struct StubLlmClient;

#[async_trait::async_trait]
impl lattice_agent::agent_loop::LlmClient for StubLlmClient {
    async fn chat(
        &self,
        _request: lattice_llm::ChatRequest,
    ) -> std::result::Result<lattice_llm::ChatResponse, Box<dyn std::error::Error + Send + Sync>>
    {
        Ok(lattice_llm::ChatResponse {
            content: "stub response".to_string(),
            reasoning_content: None,
            tool_calls: None,
            finish_reason: "stop".to_string(),
            model: "stub-model".to_string(),
            usage: Some(lattice_llm::TokenUsage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            }),
        })
    }

    async fn stream_chat(
        &self,
        _request: lattice_llm::ChatRequest,
    ) -> std::result::Result<lattice_llm::LlmStream, Box<dyn std::error::Error + Send + Sync>> {
        Ok(Box::pin(futures::stream::iter(vec![
            lattice_llm::LlmEvent::Token {
                content: "stub response".to_string(),
            },
            lattice_llm::LlmEvent::Done {
                finish_reason: "stop".to_string(),
                usage: Some(lattice_llm::TokenUsage {
                    prompt_tokens: 10,
                    completion_tokens: 5,
                    total_tokens: 15,
                }),
            },
        ])))
    }
}

#[tokio::test]
async fn run_integration_commits_existing_session() {
    let temp = tempfile::tempdir().unwrap();
    let session_api = lattice_api::SessionApi::sqlite(temp.path());
    let created = session_api
        .create_session(lattice_api::CreateSessionRequest {
            label: None,
            active_agent: "test".to_string(),
        })
        .expect("create session");
    let llm = std::sync::Arc::new(StubLlmClient);

    let request = lattice_api::RunTurnRequest {
        input: "hello".to_string(),
        session_id: created.session_id,
        loaded_profile: Some(loaded_profile("stub-model")),
        cancel_flag: None,
        test_llm: Some(llm),
        test_project_root: Some(temp.path().to_string_lossy().to_string()),
        test_persist_audit: None,
        workspace_override: None,
    };

    let result = lattice_api::run_turn(session_api, request).await;

    match result {
        Ok(turn_result) => {
            assert!(!turn_result.session_id.is_empty());
            assert!(!turn_result.commit_id.is_empty());
            assert_eq!(turn_result.assistant_output, "stub response");
            assert_eq!(turn_result.status, RunTurnStatus::Completed);
        }
        Err(e) => {
            panic!("run_turn failed: {e}");
        }
    }
}

#[tokio::test]
async fn run_integration_missing_session_returns_not_found() {
    let temp = tempfile::tempdir().unwrap();
    let session_api = lattice_api::SessionApi::sqlite(temp.path());
    let llm = std::sync::Arc::new(StubLlmClient);

    let request = lattice_api::RunTurnRequest {
        input: "hello".to_string(),
        session_id: "nonexistent-session-id".to_string(),
        loaded_profile: Some(loaded_profile("stub-model")),
        cancel_flag: None,
        test_llm: Some(llm),
        test_project_root: Some(temp.path().to_string_lossy().to_string()),
        test_persist_audit: None,
        workspace_override: None,
    };

    let result = lattice_api::run_turn(session_api, request).await;

    assert!(result.is_err());
    match result.unwrap_err() {
        lattice_api::ApiError::NotFound(id) => {
            assert_eq!(id, "nonexistent-session-id");
        }
        other => panic!("expected NotFound, got {other:?}"),
    }
}

#[tokio::test]
async fn tool_executor_uses_workspace_root_not_profile_package_dir() {
    use lattice_profile::WorkspaceSection;
    use lattice_workspace::testing::DeterministicWorkspace;
    use std::sync::Arc;

    let workspace = tempfile::tempdir().unwrap();
    let ws = workspace.path();
    std::env::set_current_dir(ws).unwrap();

    std::fs::create_dir_all(ws.join("src")).unwrap();
    std::fs::write(ws.join("src/lib.rs"), "fn main() {}").unwrap();
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(ws)
        .status()
        .expect("git init");
    std::process::Command::new("git")
        .args(["config", "user.email", "test@lattice.local"])
        .current_dir(ws)
        .status()
        .expect("git user.email");
    std::process::Command::new("git")
        .args(["config", "user.name", "lattice-test"])
        .current_dir(ws)
        .status()
        .expect("git user.name");
    std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(ws)
        .status()
        .expect("git add");
    std::process::Command::new("git")
        .args(["commit", "-q", "-m", "init"])
        .current_dir(ws)
        .status()
        .expect("git commit");

    let profile_dir = ws.join(".lattice/agents/default");
    std::fs::create_dir_all(&profile_dir).unwrap();
    std::fs::write(profile_dir.join("agent.toml"), "[agent]\nname = \"test\"\n").unwrap();

    let session_api = lattice_api::SessionApi::sqlite(ws.join("sessions"));
    let created = session_api
        .create_session(lattice_api::CreateSessionRequest {
            label: None,
            active_agent: "test".to_string(),
        })
        .expect("create session");
    let llm: std::sync::Arc<dyn lattice_agent::LlmClient> = std::sync::Arc::new(
        lattice_agent::testing::FakeStreamingLlmClient::builder()
            .push_events(vec![
                lattice_llm::LlmEvent::ToolCallStart {
                    id: "call_1".to_string(),
                    name: "observe.workspace".to_string(),
                },
                lattice_llm::LlmEvent::ToolCallDelta {
                    id: "call_1".to_string(),
                    arguments_delta: r#"{"mode":"read_file","path":"src/lib.rs"}"#.to_string(),
                },
                lattice_llm::LlmEvent::ToolCallEnd {
                    id: "call_1".to_string(),
                },
                lattice_llm::LlmEvent::Done {
                    finish_reason: "stop".to_string(),
                    usage: None,
                },
            ])
            .build(),
    );

    let request = lattice_api::RunTurnRequest {
        input: "read file".to_string(),
        session_id: created.session_id,
        loaded_profile: Some(lattice_profile::LoadedAgentProfile {
            path: profile_dir.join("agent.toml"),
            package_dir: profile_dir.clone(),
            profile: lattice_profile::AgentProfile {
                schema_version: 1,
                agent: lattice_profile::AgentSection {
                    name: "test".to_string(),
                    model: "stub-model".to_string(),
                    description: None,
                },
                prompt: lattice_profile::PromptSection {
                    system: None,
                    system_path: None,
                },
                tools: lattice_profile::ToolsSection {
                    enabled: vec!["observe.workspace".to_string()],
                },
                llm: lattice_profile::LlmSection::default(),
                lancet: lattice_profile::LancetSection::default(),
                workspace: Some(WorkspaceSection {
                    image: Some("docker.io/library/alpine:3.19".into()),
                    ..Default::default()
                }),
            },
            system_prompt: String::new(),
        }),
        cancel_flag: None,
        test_llm: Some(llm),
        test_project_root: Some(ws.to_string_lossy().to_string()),
        test_persist_audit: None,
        workspace_override: Some(Arc::new(
            DeterministicWorkspace::new().expect("deterministic workspace for upper-backed test"),
        )),
    };

    let result = lattice_api::run_turn(session_api, request)
        .await
        .expect("run_turn should succeed through API-owned CEP runtime");

    assert!(
        result
            .tool_results
            .iter()
            .any(|tool| tool.ok && tool.name == "observe.workspace"),
        "expected successful observe.workspace via workspace-root CEP runtime"
    );
}

#[test]
fn run_source_has_no_old_crate_imports() {
    let source = include_str!("run.rs");
    let test_start = source.find("#[cfg(test)]").unwrap_or(source.len());
    let prod = &source[..test_start];
    let forbidden = [
        "lattice_core",
        "lattice_runtime",
        "lattice_prompt",
        "lattice_sandbox",
        "lattice_policy",
    ];
    for pattern in forbidden {
        assert!(
            !prod.contains(pattern),
            "run.rs production code must not contain '{pattern}'"
        );
    }
}

// ─── JSON renderer tests (Task 2) ──────────────────────────────

#[test]
fn render_json_event_emits_single_line_with_trailing_newline() {
    let event = TurnEvent::TokenDelta {
        content: "hi".into(),
    };
    let mut buf = Vec::new();
    render_json_event(&event, &mut buf).unwrap();
    let output = String::from_utf8(buf).unwrap();
    assert_eq!(output, "{\"type\":\"token_delta\",\"content\":\"hi\"}\n");
}

#[test]
fn render_json_event_roundtrips_every_variant() {
    let events: Vec<TurnEvent> = vec![
        TurnEvent::TurnStarted {
            session_id: "s1".into(),
            parent_commit_id: None,
            branch: Some("main".into()),
        },
        TurnEvent::ModelStarted {
            provider: Some("anthropic".into()),
            model: "claude".into(),
        },
        TurnEvent::LlmRequestError {
            provider: Some("anthropic".into()),
            model: "claude".into(),
            attempt: 1,
            max_attempts: 4,
            error_kind: "transport".into(),
            retryable: true,
            will_retry: true,
            delay_ms: Some(500),
            status_code: None,
            message: "connection reset".into(),
        },
        TurnEvent::ReasoningDelta {
            content: "thinking".into(),
        },
        TurnEvent::TokenDelta {
            content: "hello".into(),
        },
        TurnEvent::ToolCallStarted {
            id: "tc1".into(),
            name: "read_file".into(),
        },
        TurnEvent::ToolCallFinished {
            id: "tc1".into(),
            name: "read_file".into(),
            ok: true,
            output_len: 412,
            error_summary: None,
        },
        TurnEvent::ModelFinished {
            finish_reason: "stop".into(),
            usage: Some(TurnUsageSummary {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            }),
        },
        TurnEvent::TurnFailed {
            reason: "boom".into(),
        },
        TurnEvent::TurnCancelled {
            reason: Some("caller".into()),
        },
        TurnEvent::TurnCommitting,
        TurnEvent::TurnCommitted {
            session_id: "s1".into(),
            commit_id: "c1".into(),
            status: RunTurnStatus::Completed,
        },
        TurnEvent::CommitFailed {
            reason: "parent mismatch".into(),
        },
    ];

    for event in &events {
        let mut buf = Vec::new();
        render_json_event(event, &mut buf).unwrap();
        let line = String::from_utf8(buf).unwrap();
        let trimmed = line.trim_end();
        let parsed: TurnEvent = serde_json::from_str(trimmed).unwrap();
        assert_eq!(&parsed, event, "roundtrip failed for {:?}", event);
    }
}

#[test]
fn render_json_event_emits_no_ansi_escapes() {
    let event = TurnEvent::TurnFailed {
        reason: "boom".into(),
    };
    let mut buf = Vec::new();
    render_json_event(&event, &mut buf).unwrap();
    assert!(
        !buf.contains(&0x1B),
        "JSON output must not contain ANSI ESC bytes (D-B6 / D-D3)"
    );
}

// ─── Human renderer tests (Task 3) ─────────────────────────────

#[test]
fn render_human_token_delta_writes_only_to_stdout() {
    let event = TurnEvent::TokenDelta {
        content: "hi".into(),
    };
    let mut state = RunRenderState::default();
    let mut out = Vec::new();
    let mut err = Vec::new();
    render_human_event(&event, &mut state, &mut out, &mut err, false).unwrap();
    assert_eq!(out, b"hi");
    assert!(err.is_empty());
}

#[test]
fn render_human_status_line_after_partial_token_inserts_stderr_newline() {
    // First: emit a partial token (no trailing newline)
    let mut state = RunRenderState {
        tokens_emitted_without_newline: true,
        ..Default::default()
    };
    let mut out = Vec::new();
    let mut err = Vec::new();
    render_human_event(
        &TurnEvent::TokenDelta {
            content: "foo".into(),
        },
        &mut state,
        &mut out,
        &mut err,
        false,
    )
    .unwrap();
    apply_event_to_state(
        &TurnEvent::TokenDelta {
            content: "foo".into(),
        },
        &mut state,
    );

    // Then: a status line event (ToolCallStarted)
    let event = TurnEvent::ToolCallStarted {
        id: "t1".into(),
        name: "read_file".into(),
    };
    let mut err2 = Vec::new();
    render_human_event(&event, &mut state, &mut out, &mut err2, false).unwrap();

    // D-B3: err must start with \n (the mid-token newline trick)
    assert!(
        err2.starts_with(b"\n"),
        "stderr should begin with newline after mid-token status line (D-B3); got: {:?}",
        String::from_utf8_lossy(&err2)
    );
}

#[test]
fn flush_before_status_resets_mid_token_flag_after_first_status() {
    // Regression: after the mid-token newline trick fires for the
    // first status event, subsequent status events must NOT produce
    // duplicate blank lines on stderr.
    let events = vec![
        TurnEvent::TokenDelta {
            content: "foo".into(),
        },
        TurnEvent::ToolCallStarted {
            id: "t1".into(),
            name: "read_file".into(),
        },
        TurnEvent::ToolCallFinished {
            id: "t1".into(),
            name: "read_file".into(),
            ok: true,
            output_len: 412,
            error_summary: None,
        },
        TurnEvent::TurnCommitting,
    ];
    let (out, err, _state) = fake_event_loop(events, false);
    // stdout: "foo\n" (token + D-B4 final newline)
    assert!(out.starts_with(b"foo\n"));
    // stderr: only ONE leading \n (from the first status event's D-B3 trick),
    // not three (from each of the three status events).
    let err_str = String::from_utf8(err).unwrap();
    let leading_newlines = err_str.chars().take_while(|c| *c == '\n').count();
    assert_eq!(
        leading_newlines, 1,
        "only one D-B3 newline trick, not duplicates; got: {:?}",
        err_str
    );
}

#[test]
fn render_human_tool_finished_ok_renders_byte_size() {
    let event = TurnEvent::ToolCallFinished {
        id: "t1".into(),
        name: "read_file".into(),
        ok: true,
        output_len: 412,
        error_summary: None,
    };
    let mut state = RunRenderState::default();
    let mut out = Vec::new();
    let mut err = Vec::new();
    render_human_event(&event, &mut state, &mut out, &mut err, false).unwrap();
    let err_str = String::from_utf8(err).unwrap();
    assert!(err_str.contains("412 chars"));
    assert!(err_str.contains("read_file"));
}

#[test]
fn render_human_tool_finished_err_renders_summary() {
    let event = TurnEvent::ToolCallFinished {
        id: "t1".into(),
        name: "read_file".into(),
        ok: false,
        output_len: 120,
        error_summary: Some("permission denied".into()),
    };
    let mut state = RunRenderState::default();
    let mut out = Vec::new();
    let mut err = Vec::new();
    render_human_event(&event, &mut state, &mut out, &mut err, false).unwrap();
    let err_str = String::from_utf8(err).unwrap();
    assert!(err_str.contains("120 chars"));
    assert!(err_str.contains("permission denied"));
}

#[test]
fn render_human_reasoning_hidden_default() {
    let event = TurnEvent::ReasoningDelta {
        content: "thinking".into(),
    };
    let mut state = RunRenderState::default();
    let mut out = Vec::new();
    let mut err = Vec::new();
    render_human_event(&event, &mut state, &mut out, &mut err, false).unwrap();
    assert!(
        err.is_empty(),
        "reasoning should be hidden in non-verbose mode"
    );
}

#[test]
fn render_human_reasoning_visible_verbose() {
    let event = TurnEvent::ReasoningDelta {
        content: "thinking".into(),
    };
    let mut state = RunRenderState::default();
    let mut out = Vec::new();
    let mut err = Vec::new();
    render_human_event(&event, &mut state, &mut out, &mut err, true).unwrap();
    let err_str = String::from_utf8(err).unwrap();
    assert!(err_str.contains("reasoning"));
    assert!(err_str.contains("thinking"));
}

#[test]
fn render_human_turn_started_silent_default() {
    let event = TurnEvent::TurnStarted {
        session_id: "s1".into(),
        parent_commit_id: None,
        branch: None,
    };
    let mut state = RunRenderState::default();
    let mut out = Vec::new();
    let mut err = Vec::new();
    render_human_event(&event, &mut state, &mut out, &mut err, false).unwrap();
    assert!(
        err.is_empty(),
        "TurnStarted should be silent in non-verbose human mode (D-C2)"
    );
}

#[test]
fn render_human_committed_lowercases_status() {
    let event = TurnEvent::TurnCommitted {
        session_id: "s1".into(),
        commit_id: "c1".into(),
        status: RunTurnStatus::Completed,
    };
    let mut state = RunRenderState::default();
    let mut out = Vec::new();
    let mut err = Vec::new();
    render_human_event(&event, &mut state, &mut out, &mut err, false).unwrap();
    let err_str = String::from_utf8(err).unwrap();
    assert!(
        err_str.contains("completed"),
        "status must be lowercase in human mode; got: {err_str}"
    );
    assert!(
        !err_str.contains("Completed"),
        "status must NOT contain uppercase Completed"
    );
}

// ─── Verbose epilogue tests (Task 4) ────────────────────────────

#[test]
fn verbose_epilogue_emits_all_fields_when_populated() {
    let state = RunRenderState {
        session_id: Some("s1".into()),
        commit_id: Some("c1".into()),
        last_status: Some(RunTurnStatus::Completed),
        usage_total: Some(TurnUsageSummary {
            prompt_tokens: 20,
            completion_tokens: 10,
            total_tokens: 30,
        }),
        tool_finished_count: 3,
        ..Default::default()
    };
    let mut err = Vec::new();
    render_verbose_epilogue(&state, &mut err).unwrap();
    let err_str = String::from_utf8(err).unwrap();
    assert!(err_str.contains("session"));
    assert!(err_str.contains("s1"));
    assert!(err_str.contains("commit"));
    assert!(err_str.contains("c1"));
    assert!(err_str.contains("status"));
    assert!(err_str.contains("usage"));
    assert!(err_str.contains("30 tok"));
    assert!(err_str.contains("tools"));
    assert!(err_str.contains("3 executed"));
}

#[test]
fn verbose_epilogue_skips_unset_fields() {
    let state = RunRenderState::default();
    let mut err = Vec::new();
    render_verbose_epilogue(&state, &mut err).unwrap();
    assert!(
        err.is_empty(),
        "empty state should produce no epilogue output"
    );
}

#[test]
fn verbose_epilogue_usage_from_state_not_committed() {
    // Simulate two ModelFinished events summed into usage_total
    let state = RunRenderState {
        usage_total: Some(TurnUsageSummary {
            prompt_tokens: 20,
            completion_tokens: 10,
            total_tokens: 30,
        }),
        ..Default::default()
    };
    let mut err = Vec::new();
    render_verbose_epilogue(&state, &mut err).unwrap();
    let err_str = String::from_utf8(err).unwrap();
    assert!(err_str.contains("30 tok (prompt: 20, completion: 10)"));
}

// ─── D-A4 exit-code matrix (Task 6) ────────────────────────────

#[test]
fn exit_code_from_state_covers_all_da4_rows() {
    use std::process::ExitCode;
    fn ec_to_dbg(c: ExitCode) -> String {
        format!("{:?}", c)
    }

    // Row 1: TurnCommitted{Completed} -> 0
    let mut s = RunRenderState {
        last_status: Some(RunTurnStatus::Completed),
        last_commit: Some(CommitSignal::Committed),
        ..Default::default()
    };
    assert_eq!(
        ec_to_dbg(exit_code_from_state(&s)),
        ec_to_dbg(ExitCode::SUCCESS)
    );

    // Row 2: TurnCommitted{Error} -> 1
    s.last_status = Some(RunTurnStatus::Error);
    assert_ne!(
        ec_to_dbg(exit_code_from_state(&s)),
        ec_to_dbg(ExitCode::SUCCESS)
    );

    // Row 3: TurnCommitted{Cancelled} -> 1
    s.last_status = Some(RunTurnStatus::Cancelled);
    assert_ne!(
        ec_to_dbg(exit_code_from_state(&s)),
        ec_to_dbg(ExitCode::SUCCESS)
    );

    // Row 4: CommitFailed -> 1 (regardless of last_status)
    s.last_commit = Some(CommitSignal::Failed {
        reason: "parent mismatch".into(),
    });
    assert_ne!(
        ec_to_dbg(exit_code_from_state(&s)),
        ec_to_dbg(ExitCode::SUCCESS)
    );

    // Row 5: TurnFailed without TurnCommitted -> 1
    let s5 = RunRenderState {
        last_terminal: Some(TerminalSignal::Failed {
            reason: "boom".into(),
        }),
        ..Default::default()
    };
    assert_ne!(
        ec_to_dbg(exit_code_from_state(&s5)),
        ec_to_dbg(ExitCode::SUCCESS)
    );

    // Row 6: TurnCancelled without TurnCommitted -> 1
    let s6 = RunRenderState {
        last_terminal: Some(TerminalSignal::Cancelled { reason: None }),
        ..Default::default()
    };
    assert_ne!(
        ec_to_dbg(exit_code_from_state(&s6)),
        ec_to_dbg(ExitCode::SUCCESS)
    );

    // Row 7: stream ended without any terminal/commit -> 1 (protocol violation)
    let s7 = RunRenderState::default();
    assert_ne!(
        ec_to_dbg(exit_code_from_state(&s7)),
        ec_to_dbg(ExitCode::SUCCESS)
    );
}

// ─── Sequence test helpers ────────────────────────────────────

/// Drive a Vec<TurnEvent> through the human renderer with Vec<u8>
/// writers, applying D-B4 final-newline policy, and return the
/// captured stdout bytes, stderr bytes, and accumulated state.
fn fake_event_loop(events: Vec<TurnEvent>, verbose: bool) -> (Vec<u8>, Vec<u8>, RunRenderState) {
    let mut state = RunRenderState::default();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    for event in &events {
        render_human_event(event, &mut state, &mut out, &mut err, verbose)
            .expect("render_human_event must succeed against Vec<u8> writers");
        apply_event_to_state(event, &mut state);
    }
    // Mirror run_inner D-B4 final-stdout-newline policy.
    if state.tokens_emitted_without_newline {
        use std::io::Write;
        writeln!(&mut out).expect("final newline must succeed against Vec<u8>");
    }
    (out, err, state)
}

/// Drive a Vec<TurnEvent> through the JSONL renderer with a
/// Vec<u8> writer and return the captured stdout bytes and
/// accumulated state.
fn fake_jsonl_loop(events: Vec<TurnEvent>) -> (Vec<u8>, RunRenderState) {
    let mut state = RunRenderState::default();
    let mut out: Vec<u8> = Vec::new();
    for event in &events {
        render_json_event(event, &mut out).expect("render_json_event must succeed against Vec<u8>");
        apply_event_to_state(event, &mut state);
    }
    (out, state)
}

/// Convert raw test buffer bytes to a String (asserts valid UTF-8).
fn s(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).expect("test buffers must be valid UTF-8")
}

// ─── Happy-path + interleave sequence tests ───────────────────

#[test]
fn seq_happy_path_human() {
    let events = vec![
        TurnEvent::TurnStarted {
            session_id: "S1".into(),
            parent_commit_id: None,
            branch: Some("main".into()),
        },
        TurnEvent::ModelStarted {
            provider: Some("anthropic".into()),
            model: "claude-3".into(),
        },
        TurnEvent::TokenDelta {
            content: "Hello".into(),
        },
        TurnEvent::TokenDelta {
            content: " world\n".into(),
        },
        TurnEvent::ModelFinished {
            finish_reason: "stop".into(),
            usage: Some(TurnUsageSummary {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            }),
        },
        TurnEvent::TurnCommitting,
        TurnEvent::TurnCommitted {
            session_id: "S1".into(),
            commit_id: "C1".into(),
            status: RunTurnStatus::Completed,
        },
    ];
    let (out, err, state) = fake_event_loop(events, false);
    // stdout = TokenDelta concatenation; " world\n" ends with \n,
    // so D-B4 final-newline logic does NOT append an extra \n.
    assert_eq!(s(&out), "Hello world\n");
    // stderr contains committing, commit id, completed status
    let err_str = s(&err);
    assert!(err_str.contains("committing"));
    assert!(err_str.contains("C1"));
    assert!(err_str.contains("completed"));
    // D-C2: non-verbose hides reasoning and model-finished
    assert!(!err_str.contains("reasoning"));
    assert!(!err_str.contains("model finished"));
    // Exit code = SUCCESS
    assert_eq!(
        format!("{:?}", exit_code_from_state(&state)),
        format!("{:?}", std::process::ExitCode::SUCCESS)
    );
}

#[test]
fn seq_happy_path_verbose_with_epilogue() {
    let events = vec![
        TurnEvent::TurnStarted {
            session_id: "S1".into(),
            parent_commit_id: None,
            branch: Some("main".into()),
        },
        TurnEvent::ModelStarted {
            provider: Some("anthropic".into()),
            model: "claude-3".into(),
        },
        TurnEvent::TokenDelta {
            content: "Hello".into(),
        },
        TurnEvent::TokenDelta {
            content: " world\n".into(),
        },
        TurnEvent::ModelFinished {
            finish_reason: "stop".into(),
            usage: Some(TurnUsageSummary {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            }),
        },
        TurnEvent::TurnCommitting,
        TurnEvent::TurnCommitted {
            session_id: "S1".into(),
            commit_id: "C1".into(),
            status: RunTurnStatus::Completed,
        },
    ];
    let (out, mut err, state) = fake_event_loop(events, true);
    // stdout unchanged
    assert_eq!(s(&out), "Hello world\n");
    // Append verbose epilogue to the same err buffer
    render_verbose_epilogue(&state, &mut err).unwrap();
    let err_str = s(&err);
    // Verbose stderr contains session, commit, status, usage labels
    assert!(err_str.contains("session"));
    assert!(err_str.contains("commit"));
    assert!(err_str.contains("status"));
    assert!(err_str.contains("usage"));
    assert!(err_str.contains("15 tok"));
    assert!(err_str.contains("prompt: 10"));
    assert!(err_str.contains("completion: 5"));
}

#[test]
fn seq_trailing_token_newline_emitted() {
    let events = vec![
        TurnEvent::TokenDelta {
            content: "foo".into(),
        },
        TurnEvent::ModelFinished {
            finish_reason: "stop".into(),
            usage: None,
        },
        TurnEvent::TurnCommitting,
        TurnEvent::TurnCommitted {
            session_id: "S".into(),
            commit_id: "C".into(),
            status: RunTurnStatus::Completed,
        },
    ];
    let (out, _err, _state) = fake_event_loop(events, false);
    // D-B4: "foo" does not end with \n, so fake_event_loop appends one
    assert_eq!(s(&out), "foo\n");
}

#[test]
fn seq_token_then_status_inserts_stderr_newline() {
    let events = vec![
        TurnEvent::TokenDelta {
            content: "foo".into(),
        },
        TurnEvent::ToolCallStarted {
            id: "t1".into(),
            name: "read_file".into(),
        },
    ];
    let (out, err, _state) = fake_event_loop(events, false);
    // D-B4: "foo" has no trailing newline, so helper appends one to stdout
    assert_eq!(&out[..], b"foo\n");
    // D-B3: stderr starts with \n (mid-token newline trick)
    let err_str = s(&err);
    assert!(
        err_str.starts_with('\n'),
        "stderr should begin with newline after mid-token status line (D-B3); got: {err_str:?}"
    );
    // After the leading \n, stderr contains tool name
    assert!(err_str.contains("tool"));
    assert!(err_str.contains("read_file"));
}

#[test]
fn seq_multiround_usage_sums_into_state() {
    let events = vec![
        TurnEvent::ModelFinished {
            finish_reason: "stop".into(),
            usage: Some(TurnUsageSummary {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            }),
        },
        TurnEvent::ModelFinished {
            finish_reason: "stop".into(),
            usage: Some(TurnUsageSummary {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            }),
        },
    ];
    let (_out, err, state) = fake_event_loop(events, true);
    // D-A3: usage_total is summed across two ModelFinished events
    assert_eq!(
        state.usage_total,
        Some(TurnUsageSummary {
            prompt_tokens: 20,
            completion_tokens: 10,
            total_tokens: 30,
        })
    );
    // D-C3: verbose epilogue uses state.usage_total, NOT TurnCommitted
    let mut epilogue_err = err;
    render_verbose_epilogue(&state, &mut epilogue_err).unwrap();
    let epilogue_str = s(&epilogue_err);
    assert!(epilogue_str.contains("30 tok"));
    assert!(epilogue_str.contains("prompt: 20"));
    assert!(epilogue_str.contains("completion: 10"));
}

// ─── Failure / cancellation / protocol-violation sequence tests ─────────

#[test]
fn seq_tool_failure_renders_summary() {
    let events = vec![
        TurnEvent::TurnStarted {
            session_id: "S".into(),
            parent_commit_id: None,
            branch: None,
        },
        TurnEvent::ToolCallStarted {
            id: "t".into(),
            name: "read_file".into(),
        },
        TurnEvent::ToolCallFinished {
            id: "t".into(),
            name: "read_file".into(),
            ok: false,
            output_len: 120,
            error_summary: Some("permission denied".into()),
        },
        TurnEvent::TurnCommitting,
        TurnEvent::TurnCommitted {
            session_id: "S".into(),
            commit_id: "C".into(),
            status: RunTurnStatus::Completed,
        },
    ];
    let (_out, err, state) = fake_event_loop(events, false);
    let err_str = s(&err);
    // Tool failure renders char count and error summary with failure glyph
    assert!(err_str.contains("120 chars"));
    assert!(err_str.contains("permission denied"));
    assert!(err_str.contains("✗"));
    // D-A4: tool failure inside a successful turn does NOT change exit code
    // Terminal status comes from TurnCommitted.
    assert_eq!(
        format!("{:?}", exit_code_from_state(&state)),
        format!("{:?}", std::process::ExitCode::SUCCESS)
    );
}

#[test]
fn seq_turn_failed_then_commit_drains_stream() {
    let events = vec![
        TurnEvent::TurnStarted {
            session_id: "S".into(),
            parent_commit_id: None,
            branch: None,
        },
        TurnEvent::ModelStarted {
            provider: None,
            model: "x".into(),
        },
        TurnEvent::TurnFailed {
            reason: "provider error".into(),
        },
        TurnEvent::TurnCommitting,
        TurnEvent::TurnCommitted {
            session_id: "S".into(),
            commit_id: "C".into(),
            status: RunTurnStatus::Error,
        },
    ];
    let (_out, err, state) = fake_event_loop(events, false);
    let err_str = s(&err);
    // D-A6 stream-drain proof: stderr contains the TurnFailed message
    assert!(err_str.contains("failed"));
    assert!(err_str.contains("provider error"));
    // And ALSO the post-terminal commit events (D-A6: must drain past terminal)
    assert!(err_str.contains("committing"));
    assert!(err_str.contains("C"));
    assert!(err_str.contains("error")); // D-B2: lowercase status
    // State accumulated correctly
    assert!(state.last_terminal.is_some());
    assert_eq!(state.last_status, Some(RunTurnStatus::Error));
    assert!(state.last_commit.is_some());
    // D-A4 row 2: TurnCommitted{Error} -> exit 1
    assert_ne!(
        format!("{:?}", exit_code_from_state(&state)),
        format!("{:?}", std::process::ExitCode::SUCCESS)
    );
}

#[test]
fn seq_commit_failed_renders_reason() {
    let events = vec![
        TurnEvent::TurnFailed {
            reason: "boom".into(),
        },
        TurnEvent::TurnCommitting,
        TurnEvent::CommitFailed {
            reason: "parent mismatch".into(),
        },
    ];
    let (_out, err, state) = fake_event_loop(events, false);
    let err_str = s(&err);
    // CommitFailed renders the failure glyph and reason
    assert!(err_str.contains("commit failed"));
    assert!(err_str.contains("parent mismatch"));
    // D-A4 row 4: CommitFailed -> exit 1
    assert_ne!(
        format!("{:?}", exit_code_from_state(&state)),
        format!("{:?}", std::process::ExitCode::SUCCESS)
    );
}

#[test]
fn seq_stream_ends_without_terminal_is_protocol_violation() {
    let events = vec![
        TurnEvent::TurnStarted {
            session_id: "S".into(),
            parent_commit_id: None,
            branch: None,
        },
        TurnEvent::ModelStarted {
            provider: None,
            model: "x".into(),
        },
    ];
    let (_out, _err, state) = fake_event_loop(events, false);
    // No terminal event arrived
    assert!(state.last_terminal.is_none());
    assert!(state.last_commit.is_none());
    // D-A4 row 7: stream ended without terminal or commit -> exit 1
    assert_ne!(
        format!("{:?}", exit_code_from_state(&state)),
        format!("{:?}", std::process::ExitCode::SUCCESS)
    );
}

#[test]
fn seq_jsonl_round_trips_every_variant() {
    // Build a Vec<TurnEvent> covering every variant with representative field values.
    let events: Vec<TurnEvent> = vec![
        TurnEvent::TurnStarted {
            session_id: "s1".into(),
            parent_commit_id: None,
            branch: Some("main".into()),
        },
        TurnEvent::ModelStarted {
            provider: Some("anthropic".into()),
            model: "claude-3".into(),
        },
        TurnEvent::LlmRequestError {
            provider: Some("anthropic".into()),
            model: "claude-3".into(),
            attempt: 1,
            max_attempts: 4,
            error_kind: "transport".into(),
            retryable: true,
            will_retry: true,
            delay_ms: Some(500),
            status_code: None,
            message: "connection reset".into(),
        },
        TurnEvent::ReasoningDelta {
            content: "thinking deeply".into(),
        },
        TurnEvent::TokenDelta {
            content: "hello world".into(),
        },
        TurnEvent::ToolCallStarted {
            id: "tc1".into(),
            name: "read_file".into(),
        },
        TurnEvent::ToolCallFinished {
            id: "tc1".into(),
            name: "read_file".into(),
            ok: true,
            output_len: 412,
            error_summary: None,
        },
        TurnEvent::ModelFinished {
            finish_reason: "stop".into(),
            usage: Some(TurnUsageSummary {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            }),
        },
        TurnEvent::TurnFailed {
            reason: "provider error".into(),
        },
        TurnEvent::TurnCancelled {
            reason: Some("caller cancelled".into()),
        },
        TurnEvent::TurnCommitting,
        TurnEvent::TurnCommitted {
            session_id: "s1".into(),
            commit_id: "c1".into(),
            status: RunTurnStatus::Completed,
        },
        TurnEvent::CommitFailed {
            reason: "parent mismatch".into(),
        },
    ];
    assert_eq!(events.len(), 13, "must cover all 13 TurnEvent variants");

    let (out, _state) = fake_jsonl_loop(events.clone());

    // Split stdout buffer by \n; exclude trailing empty element after final newline
    let out_str = s(&out);
    let lines: Vec<&str> = out_str.split('\n').filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 13, "JSONL must produce exactly 13 lines");

    // Each line round-trips through serde_json::from_str back to the original event
    for (i, line) in lines.iter().enumerate() {
        let parsed: TurnEvent = serde_json::from_str(line).unwrap();
        assert_eq!(
            parsed, events[i],
            "JSONL roundtrip failed for variant at index {i}: parsed={:?}, expected={:?}",
            parsed, events[i]
        );
    }

    // D-D3 / D-B6: stdout contains zero ESC (0x1B) bytes
    assert!(
        !out.contains(&0x1B),
        "JSONL output must not contain ANSI ESC bytes (D-D3 / D-B6)"
    );

    // No human chrome substrings in JSONL output
    assert!(
        !out_str.contains("·") && !out_str.contains("✓") && !out_str.contains("✗"),
        "JSONL must not contain human chrome glyphs (D-D3)"
    );
    assert!(
        !out_str.contains("reasoning:"),
        "JSONL must not contain human-format 'reasoning:' prefix"
    );

    // D-D3 (clarification): JSONL IS the raw public TurnEvent stream,
    // so ReasoningDelta IS emitted as a normal JSONL line even though
    // it's hidden from human stderr
    assert!(
        out_str.contains("\"type\":\"reasoning_delta\""),
        "JSONL must contain reasoning_delta as a normal JSONL line (D-D3 raw-stream contract)"
    );
}

#[test]
fn seq_reasoning_hidden_default_in_human() {
    let events = vec![
        TurnEvent::TokenDelta {
            content: "a".into(),
        },
        TurnEvent::ReasoningDelta {
            content: "thinking".into(),
        },
        TurnEvent::TokenDelta {
            content: "b".into(),
        },
        TurnEvent::TurnCommitted {
            session_id: "S".into(),
            commit_id: "C".into(),
            status: RunTurnStatus::Completed,
        },
    ];
    let (out, err, _state) = fake_event_loop(events, false);
    // stdout contains TokenDelta bytes only
    let out_str = s(&out);
    assert!(
        out_str.starts_with("ab"),
        "stdout should start with 'ab' (TokenDelta concatenation); got: {out_str:?}"
    );
    // D-C2: stderr does not contain reasoning or thinking text
    let err_str = s(&err);
    assert!(
        !err_str.contains("reasoning"),
        "stderr must not contain 'reasoning' in non-verbose mode (D-C2)"
    );
    assert!(
        !err_str.contains("thinking"),
        "stderr must not contain 'thinking' in non-verbose mode (D-C2)"
    );
}

#[test]
fn seq_reasoning_visible_in_verbose() {
    let events = vec![
        TurnEvent::TokenDelta {
            content: "a".into(),
        },
        TurnEvent::ReasoningDelta {
            content: "thinking".into(),
        },
        TurnEvent::TokenDelta {
            content: "b".into(),
        },
        TurnEvent::TurnCommitted {
            session_id: "S".into(),
            commit_id: "C".into(),
            status: RunTurnStatus::Completed,
        },
    ];
    let (out, err, _state) = fake_event_loop(events, true);
    // stdout contains TokenDelta bytes only (unchanged from non-verbose)
    let out_str = s(&out);
    assert!(
        out_str.starts_with("ab"),
        "stdout should start with 'ab'; got: {out_str:?}"
    );
    // D-C3: verbose stderr contains reasoning text
    let err_str = s(&err);
    assert!(
        err_str.contains("thinking"),
        "verbose stderr must contain 'thinking' (D-C3)"
    );
    assert!(
        err_str.contains("reasoning"),
        "verbose stderr must contain 'reasoning' (D-C3)"
    );
}

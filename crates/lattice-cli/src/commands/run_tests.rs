// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;
use lattice_session::StateLayout;

// ─── Stream timeout constants ───────────────────────────────────

#[test]
fn structured_errors_keep_exit_classes_without_echoing_private_input() {
    for (error, expected_code, kind) in [
        (
            lattice_api::ApiError::BadRequest("private-test-secret".into()),
            2,
            "bad_request",
        ),
        (
            lattice_api::ApiError::NotFound("private-test-secret".into()),
            2,
            "not_found",
        ),
        (
            lattice_api::ApiError::Busy("private-test-secret".into()),
            1,
            "busy",
        ),
        (
            lattice_api::ApiError::Internal("private-test-secret".into()),
            1,
            "internal",
        ),
    ] {
        let (code, record) = error_record(&error.into());
        assert_eq!(code, expected_code);
        assert_eq!(record["kind"], kind);
        assert_eq!(record["type"], "error");
        assert!(!record.to_string().contains("private-test-secret"));
    }
}

#[tokio::test]
async fn send_binding_conflict_is_exit_two_but_daemon_identity_conflict_stays_one() {
    use lattice_api::local::{LocalErrorKind, serve};
    use std::os::unix::fs::PermissionsExt;
    let _serial = super::ipc_tests::signal_guard().await;

    let temp = tempfile::tempdir().unwrap();
    let layout = StateLayout::in_state_root(temp.path());
    let store = lattice_api::SessionApi::git_create_with_layout(
        layout.lattice_root().join("sessions.git"),
        &layout,
    )
    .unwrap();
    let session = store
        .create_session(lattice_api::CreateSessionRequest {
            label: None,
            active_agent: "unused".into(),
            project_branch: "main".into(),
            project_store_key: lattice_api::LocalWorkspaceRoot::from_path(temp.path())
                .unwrap()
                .store_key_hex(),
        })
        .unwrap();
    let client = LocalClient::new(layout.clone());
    let backend = tokio::spawn(serve(layout.clone()));
    time::timeout(Duration::from_secs(10), async {
        while client.status().await.is_err() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let error = run_session_prompt(
        &client,
        "private-test-secret".into(),
        session.session_id,
        Some(lattice_api::local::new_turn_id()),
        false,
        true,
    )
    .await
    .unwrap_err();
    client.stop().await.unwrap();
    backend.await.unwrap().unwrap();

    let socket = layout.lattice_root().join("local/api.sock");
    let _listener = tokio::net::UnixListener::bind(&socket).unwrap();
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
    let daemon_error = serve(layout).await.unwrap_err();
    assert_eq!(daemon_error.kind, LocalErrorKind::Conflict);
    assert_eq!(
        report_error(&daemon_error.into(), true),
        std::process::ExitCode::from(1)
    );

    let (code, record) = error_record(&error);
    let jsonl = format!("{record}\n");
    let parsed: serde_json::Value = serde_json::from_str(jsonl.trim_end()).unwrap();
    assert_eq!(parsed["type"], "error");
    assert_eq!(parsed["kind"], "conflict");
    assert!(!jsonl.contains("private-test-secret"));
    assert_eq!(report_error(&error, true), std::process::ExitCode::from(2));
    assert_eq!(code, 2);
}

#[test]
fn stream_timeout_constant_is_5_minutes() {
    // STREAM_TIMEOUT_SECS must be 300 (5 minutes)
    const EXPECTED_TIMEOUT: u64 = 300;
    assert_eq!(EXPECTED_TIMEOUT, STREAM_TIMEOUT_SECS);
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
        },
        system_prompt: String::new(),
    }
}

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

/// #143: the durable session store is addressed through the XDG *state*
/// root, never the configuration root.
#[test]
fn session_store_lives_under_the_xdg_state_root() {
    let path = crate::commands::session::session_store_path().expect("resolve store path");
    let shown = path.to_string_lossy().into_owned();
    assert!(shown.ends_with("lattice/sessions.git"), "{shown}");
    assert!(
        !shown.contains("/.config/"),
        "durable session state must not live under the config root: {shown}"
    );
}
#[tokio::test]
async fn run_integration_commits_existing_session() {
    let temp = tempfile::tempdir().unwrap();
    // The session graph store is git-backed; the fixture needs an initialized repo.
    std::fs::write(temp.path().join("README.md"), "init\n").expect("write");
    let mut __files: Vec<(String, String)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(temp.path()) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let content = std::fs::read_to_string(&path).unwrap_or_default();
                __files.push((name, content));
            }
        }
    }
    let __refs: Vec<(&str, &str)> = __files
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_str()))
        .collect();
    let _ = lattice_session::test_utils::init_fixture_repo(temp.path(), &__refs);

    let layout = StateLayout::in_state_root(&temp.path().join("state"));
    let sessions_repo = layout.lattice_root().join("sessions.git");
    let session_api =
        lattice_api::SessionApi::git_create_with_layout(sessions_repo, &layout).unwrap();
    let created = session_api
        .create_session(lattice_api::CreateSessionRequest {
            label: None,
            active_agent: "test".to_string(),
            project_branch: "main".to_string(),
            project_store_key: lattice_api::LocalWorkspaceRoot::from_path(temp.path())
                .unwrap()
                .store_key_hex(),
        })
        .expect("create session");
    let llm = std::sync::Arc::new(
        lattice_agent::testing::FakeStreamingLlmClient::builder()
            .push_events(vec![
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
            ])
            .build(),
    );

    let request = lattice_api::RunTurnRequest {
        input: "hello".to_string(),
        session_id: created.session_id,
        loaded_profile: Some(loaded_profile("stub-model")),
        cancel_flag: None,
        test_llm: Some((llm, lattice_llm::ProviderKind::OpenAiCompatible)),
        test_project_root: Some(temp.path().to_string_lossy().to_string()),
    };

    let result = lattice_api::run_turn(session_api, request).await;

    match result {
        Ok(turn_result) => {
            assert!(!turn_result.session_id.is_empty());
            assert!(!turn_result.commit_id.is_empty());
            assert_eq!(turn_result.assistant_output, "stub response");
            assert_eq!(
                turn_result.outcome,
                lattice_api::TurnOutcome::Completed {
                    finish_reason: lattice_api::TurnFinishReason::Stop
                }
            );
        }
        Err(e) => {
            panic!("run_turn failed: {e}");
        }
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
    // stdout: "foo\n" (token + final newline)
    assert!(out.starts_with(b"foo\n"));
    // stderr: only ONE leading \n (from the first status event's trick),
    // not three (from each of the three status events).
    let err_str = String::from_utf8(err).unwrap();
    let leading_newlines = err_str.chars().take_while(|c| *c == '\n').count();
    assert_eq!(
        leading_newlines, 1,
        "only one newline trick, not duplicates; got: {:?}",
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
fn render_human_turn_started_silent_default() {
    let event = TurnEvent::TurnStarted {
        session_id: "s1".into(),
        turn_id: "s1:root".into(),
        parent_commit_id: None,
        branch: None,
    };
    let mut state = RunRenderState::default();
    let mut out = Vec::new();
    let mut err = Vec::new();
    render_human_event(&event, &mut state, &mut out, &mut err, false).unwrap();
    assert!(
        err.is_empty(),
        "TurnStarted should be silent in non-verbose human mode"
    );
}

#[test]
fn render_human_committed_lowercases_status() {
    let event = TurnEvent::TurnCommitted {
        origin: lattice_api::ClosureOrigin::Live,
        workspace_history_checkpoint: lattice_api::WorkspaceHistoryCheckpoint::Empty,
        session_id: "s1".into(),
        commit_id: "c1".into(),
        outcome: lattice_api::TurnOutcome::Completed {
            finish_reason: lattice_api::TurnFinishReason::Stop,
        },
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
        last_outcome: Some(lattice_api::TurnOutcome::Completed {
            finish_reason: lattice_api::TurnFinishReason::Stop,
        }),
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
    assert!(err_str.contains("outcome"));
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

// ─── Sequence test helpers ────────────────────────────────────

/// Drive a Vec<TurnEvent> through the human renderer with Vec<u8>
/// writers, applying final-newline policy, and return the
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
    // Mirror run_inner final-stdout-newline policy.
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
            turn_id: "s1:root".into(),
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
            origin: lattice_api::ClosureOrigin::Live,
            workspace_history_checkpoint: lattice_api::WorkspaceHistoryCheckpoint::Empty,
            session_id: "S1".into(),
            commit_id: "C1".into(),
            outcome: lattice_api::TurnOutcome::Completed {
                finish_reason: lattice_api::TurnFinishReason::Stop,
            },
        },
    ];
    let (out, err, _state) = fake_event_loop(events, false);
    // stdout = TokenDelta concatenation; " world\n" ends with \n,
    // so final-newline logic does NOT append an extra \n.
    assert_eq!(s(&out), "Hello world\n");
    // stderr contains committing, commit id, completed status
    let err_str = s(&err);
    assert!(err_str.contains("committing"));
    assert!(err_str.contains("C1"));
    assert!(err_str.contains("completed"));
    // non-verbose hides reasoning and model-finished
    assert!(!err_str.contains("reasoning"));
    assert!(!err_str.contains("model finished"));
}

#[test]
fn seq_happy_path_verbose_with_epilogue() {
    let events = vec![
        TurnEvent::TurnStarted {
            turn_id: "s1:root".into(),
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
            origin: lattice_api::ClosureOrigin::Live,
            workspace_history_checkpoint: lattice_api::WorkspaceHistoryCheckpoint::Empty,
            session_id: "S1".into(),
            commit_id: "C1".into(),
            outcome: lattice_api::TurnOutcome::Completed {
                finish_reason: lattice_api::TurnFinishReason::Stop,
            },
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
    assert!(err_str.contains("outcome"));
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
            origin: lattice_api::ClosureOrigin::Live,
            workspace_history_checkpoint: lattice_api::WorkspaceHistoryCheckpoint::Empty,
            session_id: "S".into(),
            commit_id: "C".into(),
            outcome: lattice_api::TurnOutcome::Completed {
                finish_reason: lattice_api::TurnFinishReason::Stop,
            },
        },
    ];
    let (out, _err, _state) = fake_event_loop(events, false);
    // "foo" does not end with \n, so fake_event_loop appends one
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
    // "foo" has no trailing newline, so helper appends one to stdout
    assert_eq!(&out[..], b"foo\n");
    // stderr starts with \n (mid-token newline trick)
    let err_str = s(&err);
    assert!(
        err_str.starts_with('\n'),
        "stderr should begin with newline after mid-token status line; got: {err_str:?}"
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
    // usage_total is summed across two ModelFinished events
    assert_eq!(
        state.usage_total,
        Some(TurnUsageSummary {
            prompt_tokens: 20,
            completion_tokens: 10,
            total_tokens: 30,
        })
    );
    // verbose epilogue uses state.usage_total, NOT TurnCommitted
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
            turn_id: "s1:root".into(),
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
            origin: lattice_api::ClosureOrigin::Live,
            workspace_history_checkpoint: lattice_api::WorkspaceHistoryCheckpoint::Empty,
            session_id: "S".into(),
            commit_id: "C".into(),
            outcome: lattice_api::TurnOutcome::Completed {
                finish_reason: lattice_api::TurnFinishReason::Stop,
            },
        },
    ];
    let (_out, err, _state) = fake_event_loop(events, false);
    let err_str = s(&err);
    // Tool failure renders char count and error summary with failure glyph
    assert!(err_str.contains("120 chars"));
    assert!(err_str.contains("permission denied"));
    assert!(err_str.contains("✗"));
}

#[test]
fn seq_turn_failed_then_commit_drains_stream() {
    let events = vec![
        TurnEvent::TurnStarted {
            turn_id: "s1:root".into(),
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
            origin: lattice_api::ClosureOrigin::Live,
            workspace_history_checkpoint: lattice_api::WorkspaceHistoryCheckpoint::Empty,
            session_id: "S".into(),
            commit_id: "C".into(),
            outcome: lattice_api::TurnOutcome::Failed {
                reason: lattice_session::pending::TurnFailure::ModelRequestFailed,
            },
        },
    ];
    let (_out, err, state) = fake_event_loop(events, false);
    let err_str = s(&err);
    // stream-drain proof: stderr contains the TurnFailed message
    assert!(err_str.contains("failed"));
    assert!(err_str.contains("provider error"));
    // And ALSO the post-terminal commit events (must drain past terminal)
    assert!(err_str.contains("committing"));
    assert!(err_str.contains("C"));
    assert!(err_str.contains("error")); // lowercase status
    // State accumulated correctly
    assert_eq!(
        state.last_outcome,
        Some(lattice_api::TurnOutcome::Failed {
            reason: lattice_session::pending::TurnFailure::ModelRequestFailed
        })
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
    let (_out, err, _state) = fake_event_loop(events, false);
    let err_str = s(&err);
    // CommitFailed renders the failure glyph and reason
    assert!(err_str.contains("commit failed"));
    assert!(err_str.contains("parent mismatch"));
}

#[test]
fn seq_jsonl_round_trips_every_variant() {
    // Build a Vec<TurnEvent> covering every variant with representative field values.
    let events: Vec<TurnEvent> = vec![
        TurnEvent::TurnStarted {
            turn_id: "s1:root".into(),
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
            origin: lattice_api::ClosureOrigin::Live,
            workspace_history_checkpoint: lattice_api::WorkspaceHistoryCheckpoint::Empty,
            session_id: "s1".into(),
            commit_id: "c1".into(),
            outcome: lattice_api::TurnOutcome::Completed {
                finish_reason: lattice_api::TurnFinishReason::Stop,
            },
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

    // stdout contains zero ESC (0x1B) bytes
    assert!(
        !out.contains(&0x1B),
        "JSONL output must not contain ANSI ESC bytes"
    );

    // No human chrome substrings in JSONL output
    assert!(
        !out_str.contains("·") && !out_str.contains("✓") && !out_str.contains("✗"),
        "JSONL must not contain human chrome glyphs"
    );
    assert!(
        !out_str.contains("reasoning:"),
        "JSONL must not contain human-format 'reasoning:' prefix"
    );

    // JSONL IS the raw public TurnEvent stream,
    // so ReasoningDelta IS emitted as a normal JSONL line even though
    // it's hidden from human stderr
    assert!(
        out_str.contains("\"type\":\"reasoning_delta\""),
        "JSONL must contain reasoning_delta as a normal JSONL line (raw-stream contract)"
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
            origin: lattice_api::ClosureOrigin::Live,
            workspace_history_checkpoint: lattice_api::WorkspaceHistoryCheckpoint::Empty,
            session_id: "S".into(),
            commit_id: "C".into(),
            outcome: lattice_api::TurnOutcome::Completed {
                finish_reason: lattice_api::TurnFinishReason::Stop,
            },
        },
    ];
    let (out, err, _state) = fake_event_loop(events, false);
    // stdout contains TokenDelta bytes only
    let out_str = s(&out);
    assert!(
        out_str.starts_with("ab"),
        "stdout should start with 'ab' (TokenDelta concatenation); got: {out_str:?}"
    );
    // stderr does not contain reasoning or thinking text
    let err_str = s(&err);
    assert!(
        !err_str.contains("reasoning"),
        "stderr must not contain 'reasoning' in non-verbose mode"
    );
    assert!(
        !err_str.contains("thinking"),
        "stderr must not contain 'thinking' in non-verbose mode"
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
            origin: lattice_api::ClosureOrigin::Live,
            workspace_history_checkpoint: lattice_api::WorkspaceHistoryCheckpoint::Empty,
            session_id: "S".into(),
            commit_id: "C".into(),
            outcome: lattice_api::TurnOutcome::Completed {
                finish_reason: lattice_api::TurnFinishReason::Stop,
            },
        },
    ];
    let (out, err, _state) = fake_event_loop(events, true);
    // stdout contains TokenDelta bytes only (unchanged from non-verbose)
    let out_str = s(&out);
    assert!(
        out_str.starts_with("ab"),
        "stdout should start with 'ab'; got: {out_str:?}"
    );
    // verbose stderr contains reasoning text
    let err_str = s(&err);
    assert!(
        err_str.contains("thinking"),
        "verbose stderr must contain 'thinking'"
    );
    assert!(
        err_str.contains("reasoning"),
        "verbose stderr must contain 'reasoning'"
    );
}

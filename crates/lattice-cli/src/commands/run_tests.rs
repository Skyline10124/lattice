// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0
use super::*;
use lattice_api::local::{MaterialSummary, ModelAttemptKey, SessionId, TurnRef};
use serde_json::json;

fn key() -> ModelAttemptKey {
    ModelAttemptKey {
        turn_id: "10000000-0000-4000-8000-000000000001".parse().unwrap(),
        model_step_ordinal: 0,
        attempt_ordinal: 0,
    }
}
fn turn_ref() -> TurnRef {
    TurnRef {
        session_id: SessionId::parse("11111111111141118111111111111111").unwrap(),
        turn_id: key().turn_id,
    }
}
fn events() -> Vec<TurnEvent> {
    let step: lattice_api::local::StepDetail = serde_json::from_value(json!({"key":{"turn_id":key().turn_id,"ordinal":0},"attempts":[],"state":{"state":"closed","end":{"end":"stop","node":null,"cause":{"cause":"cancelled"}},"slots":[]}})).unwrap();
    let summary: TurnSummary = serde_json::from_value(
        json!({"outcome":"cancelled","notice":{"level":"info","text":"cancelled by user"}}),
    )
    .unwrap();
    let id = "30000000-0000-4000-8000-000000000003".parse().unwrap();
    vec![
        TurnEvent::TurnStarted {
            turn: turn_ref(),
            user_node: lattice_api::local::NodeId(1),
        },
        TurnEvent::ModelStarted {
            key: key(),
            provider: serde_json::from_value(json!("open_ai_compatible")).unwrap(),
            model: "test-model".into(),
        },
        TurnEvent::TokenDelta {
            key: key(),
            content: "hello".into(),
        },
        TurnEvent::ReasoningDelta {
            key: key(),
            content: "thinking".into(),
        },
        TurnEvent::LlmRequestError {
            key: key(),
            retryable: true,
            will_retry: true,
            delay_ms: Some(10),
            message: "retry".into(),
        },
        TurnEvent::ModelFinished {
            key: key(),
            finish_reason: Some("stop".into()),
            usage: Some(lattice_llm::TokenUsage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            }),
        },
        TurnEvent::CallsRegistered {
            key: step.key,
            calls: vec![],
        },
        TurnEvent::ToolCallStarted {
            id,
            name: "observe.workspace".into(),
        },
        TurnEvent::ToolCallFinished {
            id,
            name: "observe.workspace".into(),
            outcome: lattice_api::local::ToolResultOutcome::Succeeded,
            material: MaterialSummary::OutputTooLarge {
                digest: [0; 32],
                byte_length: 70000,
            },
            error_summary: None,
        },
        TurnEvent::CepCallStarted {
            invocation_id: id,
            method: "observe.workspace".into(),
            semantic_stage: "observe".into(),
            guard_stage: "allowed".into(),
        },
        TurnEvent::CepCallFinished {
            invocation_id: id,
            method: "observe.workspace".into(),
            ok: true,
            semantic_stage: "observe".into(),
            guard_stage: "allowed".into(),
            error_code: None,
        },
        TurnEvent::StepClosed { step: step.clone() },
        TurnEvent::TurnCancelled {
            turn: turn_ref(),
            step,
            summary,
        },
    ]
}
#[test]
fn structured_errors_keep_exit_classes_without_echoing_private_input() {
    for (error, code, kind) in [
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
        let (actual, record) = error_record(&error.into());
        assert_eq!(actual, code);
        assert_eq!(record["kind"], kind);
        assert_eq!(record["type"], "error");
        assert!(!record.to_string().contains("private-test-secret"));
    }
    let conflict: lattice_api::local::LocalError =
        lattice_api::ApiError::Conflict("private-test-secret".into()).into();
    assert_eq!(error_record(&conflict.clone().into()).0, 1);
    assert_eq!(error_record(&submission_error(conflict)).0, 2);
    let error: lattice_api::local::LocalError =
        lattice_api::ApiError::NotFound("private-test-secret".into()).into();
    assert_eq!(
        error_record(&anyhow::Error::new(error).context(HistoryFailure)).0,
        1
    );
}
#[test]
fn store_diagnostic_retains_approved_paths() {
    let error: lattice_api::local::LocalError = serde_json::from_value(json!({"kind":"store_unavailable","message":"legacy stores","recovery":null,"store":{"type":"legacy_stores","paths":["/tmp/owned/sessions.git","/tmp/owned/projects/a/invocations.git"]}})).unwrap();
    let (code, record) = error_record(&error.into());
    assert_eq!(code, 1);
    assert_eq!(record["store"]["paths"].as_array().unwrap().len(), 2);
}
#[test]
fn run_validate_agents_dir_rejects_parent_components() {
    assert!(
        validate_agents_dir(Path::new(".lattice/../agents"))
            .unwrap_err()
            .contains("path traversal")
    );
}
#[test]
fn run_validate_agents_dir_resolves_symlink_prefix_before_safety_check() {
    let root = std::env::current_dir()
        .unwrap()
        .join("target/tmp/agents-dir-tests");
    std::fs::create_dir_all(&root).unwrap();
    let safe = tempfile::tempdir_in(root).unwrap();
    let link = safe.path().join("link-to-tmp");
    std::os::unix::fs::symlink("/tmp", &link).unwrap();
    assert!(
        validate_agents_dir(&link.join("missing"))
            .unwrap_err()
            .contains("/tmp")
    );
}
#[test]
fn session_store_lives_under_the_xdg_state_root() {
    let path = crate::commands::session::session_store_path().unwrap();
    assert!(path.ends_with("lattice/lattice.redb"));
    assert!(!path.to_string_lossy().contains("/.config/"));
}
#[test]
fn jsonl_round_trips_events_without_human_output() {
    let events = events();
    let mut bytes = Vec::new();
    for event in &events {
        render_json_event(event, &mut bytes).unwrap();
    }
    let text = String::from_utf8(bytes.clone()).unwrap();
    assert!(text.ends_with('\n'));
    assert!(!bytes.contains(&0x1b));
    let decoded: Vec<TurnEvent> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(decoded, events);
    assert!(text.contains("reasoning_delta"));
    assert!(!text.contains("reasoning:"));
}
#[test]
fn human_stream_keeps_stdout_reasoning_bytes_and_summary_separate() {
    for verbose in [false, true] {
        let mut state = RunRenderState::default();
        let mut out = Vec::new();
        let mut err = Vec::new();
        for event in events() {
            render_human_event(&event, &mut state, &mut out, &mut err, verbose).unwrap();
            apply_event_to_state(&event, &mut state);
        }
        finish_text(&mut state, &mut out).unwrap();
        assert_eq!(out, b"hello\n");
        let err = String::from_utf8(err).unwrap();
        assert_eq!(err.contains("thinking"), verbose);
        assert!(err.contains("70000 B"));
        assert!(err.contains("succeeded"));
        assert!(err.contains("内容不可用"));
        assert!(err.contains("cancelled by user"));
        assert_eq!(err.lines().filter(|line| line.is_empty()).count(), 1);
        assert_eq!(
            state.last_summary.unwrap().outcome,
            PublicTurnOutcome::Cancelled
        );
    }
}
#[test]
fn terminal_summary_is_not_recomputed_and_usage_is_durable() {
    let mut state = RunRenderState::default();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let TurnEvent::TurnCancelled { turn, step, .. } = events().pop().unwrap() else {
        panic!()
    };
    for (outcome, notice, exit) in [
        ("completed", json!(null), 0),
        (
            "failed",
            json!({"level":"error","text":"exact provider message"}),
            1,
        ),
        (
            "cancelled",
            json!({"level":"info","text":"exact cancellation"}),
            2,
        ),
    ] {
        let summary: TurnSummary =
            serde_json::from_value(json!({"outcome":outcome,"notice":notice})).unwrap();
        let event = match outcome {
            "completed" => TurnEvent::TurnCompleted {
                turn,
                step: step.clone(),
                summary: summary.clone(),
            },
            "failed" => TurnEvent::TurnFailed {
                turn,
                step: step.clone(),
                summary: summary.clone(),
            },
            _ => TurnEvent::TurnCancelled {
                turn,
                step: step.clone(),
                summary: summary.clone(),
            },
        };
        render_human_event(&event, &mut state, &mut out, &mut err, false).unwrap();
        apply_event_to_state(&event, &mut state);
        assert_eq!(state.last_summary.as_ref(), Some(&summary));
        assert_eq!(
            summary_exit(Some(&summary)),
            std::process::ExitCode::from(exit)
        );
    }
    state.usage=Some(serde_json::from_value(json!({"kind":"partial","known":{"prompt_tokens":20,"completion_tokens":10,"total_tokens":30},"unknown_attempts":1})).unwrap());
    render_verbose_epilogue(&state, &mut err).unwrap();
    let text = String::from_utf8(err).unwrap();
    assert!(text.contains("exact provider message"));
    assert!(text.contains("unknown_attempts"));
    assert!(text.contains("30"));
    let mut empty = Vec::new();
    render_verbose_epilogue(&RunRenderState::default(), &mut empty).unwrap();
    assert!(empty.is_empty());
}
#[test]
fn tool_failure_keeps_diagnostic_and_newline() {
    let mut state = RunRenderState::default();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let token = TurnEvent::TokenDelta {
        key: key(),
        content: "foo".into(),
    };
    render_human_event(&token, &mut state, &mut out, &mut err, false).unwrap();
    apply_event_to_state(&token, &mut state);
    let event = TurnEvent::ToolCallFinished {
        id: "30000000-0000-4000-8000-000000000003".parse().unwrap(),
        name: "read".into(),
        outcome: lattice_api::local::ToolResultOutcome::Rejected,
        material: MaterialSummary::NoContent,
        error_summary: Some("permission denied".into()),
    };
    for _ in 0..2 {
        render_human_event(&event, &mut state, &mut out, &mut err, false).unwrap();
    }
    assert_eq!(out, b"foo");
    let text = String::from_utf8(err).unwrap();
    assert!(text.starts_with('\n'));
    assert!(!text.starts_with("\n\n"));
    assert!(text.contains("permission denied"));
    assert!(text.contains("rejected"));
}

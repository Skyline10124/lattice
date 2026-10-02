// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;
use lattice_api::local::{LocalError, ObservationSnapshot, TurnView};
use serde_json::json;

const SID: &str = "11111111111141118111111111111111";
fn read_error() -> LocalError {
    lattice_api::ApiError::StoreUnavailable("persistent history read failed".into()).into()
}
fn app() -> App {
    App::new(
        SessionContext {
            session_id: None,
            active_agent: "test".into(),
        },
        false,
        Path::new("/nonexistent-tui-test-history"),
    )
}
fn view(id: TurnId, outcome: &str) -> ObservationSnapshot {
    serde_json::from_value(json!({
        "session":{"session_id":SID,"project_store_key":"0".repeat(64),"current":1,"active_turn":null,"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"},
        "current_agent":"test","selected_turn":{"session_id":SID,"turn_id":id,"user_node":1,"agent_name":"test","accepted_at":"2026-01-01T00:00:00Z","ended_at":"2026-01-01T00:00:01Z","summary":{"outcome":outcome,"notice":match outcome { "failed" => json!({"level":"error","text":"provider failure"}), "cancelled" => json!({"level":"info","text":"cancelled by user"}), _ => serde_json::Value::Null }},"usage":{"kind":"partial","known":{"prompt_tokens":0,"completion_tokens":0,"total_tokens":0},"unknown_attempts":1},"steps":[]},
        "active":null,"history":{"session_id":SID,"head":1},"attempt":null,"cursor":{"epoch":"test","sequence":1},"owned":false
    })).unwrap()
}

#[test]
fn new_display_has_no_session_until_acceptance() {
    let mut app = app();
    app.input.set_text(" original ");
    assert_eq!(app.submit().as_deref(), Some(" original "));
    assert!(app.ctx.session_id.is_none());
    let id = app.current_turn.unwrap();
    app.apply_update(LocalUpdate::Submission {
        turn_id: id,
        result: Err(read_error()),
    });
    assert_eq!(app.input.text(), " original ");
    assert!(app.ctx.session_id.is_none());
    let accepted = TurnView::from(view(id, "completed").selected_turn.as_ref().unwrap());
    app.apply_update(LocalUpdate::Submission {
        turn_id: id,
        result: Ok(Acceptance::Accepted { turn: accepted }),
    });
    assert_eq!(app.ctx.session_id.unwrap().as_str(), SID);
}

#[test]
fn summary_survives_slow_failed_history_and_is_not_duplicated() {
    for outcome in ["completed", "failed", "cancelled"] {
        let mut app = app();
        let id = lattice_api::local::new_turn_id();
        let key = lattice_api::local::ModelAttemptKey {
            turn_id: id,
            model_step_ordinal: 0,
            attempt_ordinal: 1,
        };
        app.apply_turn_event(TurnEvent::TokenDelta {
            key,
            content: "live answer".into(),
        });
        let view = view(id, outcome);
        app.apply_update(LocalUpdate::State {
            view: Box::new(view.clone()),
            phase: ObservationPhase::NoOwner,
            error: Some(read_error()),
        });
        app.apply_update(LocalUpdate::Reset {
            view: Box::new(view.clone()),
            generation: 1,
        });
        app.apply_update(LocalUpdate::History {
            anchor: view.history.clone(),
            generation: 1,
            stale: false,
            result: Err(lattice_api::local::HistoryReadError {
                context: lattice_api::local::HistoryReadContext::Worker,
                error: read_error(),
            }),
        });
        assert!(
            app.blocks
                .iter()
                .any(|b| matches!(b, Block::Assistant(text) if text == "live answer"))
        );
        app.apply_update(LocalUpdate::State {
            view: Box::new(view.clone()),
            phase: ObservationPhase::Finished,
            error: None,
        });
        let text = crate::view::transcript_lines(&app, 200)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains(outcome), "{text}");
        assert!(text.contains("正文不可用"), "{text}");
        assert_eq!(
            app.blocks
                .iter()
                .filter(|b| matches!(
                    b,
                    Block::Status {
                        kind: StatusKind::Summary { .. },
                        ..
                    }
                ))
                .count(),
            1
        );
    }
}

#[test]
fn retry_replaces_only_its_attempt_and_stale_history_cannot_replace_display() {
    let mut app = app();
    let id = lattice_api::local::new_turn_id();
    let key = lattice_api::local::ModelAttemptKey {
        turn_id: id,
        model_step_ordinal: 0,
        attempt_ordinal: 1,
    };
    app.apply_turn_event(TurnEvent::TokenDelta {
        key,
        content: "first attempt".into(),
    });
    let next = lattice_api::local::ModelAttemptKey {
        attempt_ordinal: 2,
        ..key
    };
    app.apply_turn_event(TurnEvent::TokenDelta {
        key: next,
        content: "retry answer".into(),
    });
    let view = view(id, "completed");
    app.apply_update(LocalUpdate::Reset {
        view: Box::new(view.clone()),
        generation: 2,
    });
    app.apply_update(LocalUpdate::History {
        anchor: view.history.clone(),
        generation: 1,
        stale: true,
        result: Ok(Box::new(lattice_api::local::LocalHistory {
            anchor: view.history,
            entries: vec![],
        })),
    });
    let text = crate::view::transcript_lines(&app, 200)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("retry answer"), "{text}");
    assert!(!text.contains("first attempt"), "{text}");
}

#[test]
fn closed_owned_snapshot_is_idle_before_history_and_finished_is_separate() {
    let mut app = app();
    app.input.set_text("prompt");
    app.submit();
    let id = app.current_turn.unwrap();
    let mut view = view(id, "completed");
    view.owned = true;
    app.apply_update(LocalUpdate::State {
        view: Box::new(view.clone()),
        phase: ObservationPhase::Live,
        error: None,
    });
    assert_eq!(app.phase, TurnPhase::Idle);
    assert_eq!(app.current_turn, None);
    assert_ne!(
        app.state.as_ref().unwrap().phase,
        ObservationPhase::Finished
    );
    app.apply_update(LocalUpdate::Reset {
        view: Box::new(view.clone()),
        generation: 1,
    });
    app.apply_update(LocalUpdate::Finished {
        turn: Some(lattice_api::local::TurnRef {
            session_id: view.session.session_id,
            turn_id: id,
        }),
        anchor: view.history,
        generation: 1,
    });
    assert_eq!(
        app.state.as_ref().unwrap().phase,
        ObservationPhase::Finished
    );
}

#[test]
fn loaded_step_and_terminal_tool_cannot_be_overwritten_by_late_events() {
    let mut app = app();
    let id = lattice_api::local::new_turn_id();
    let key = ModelAttemptKey {
        turn_id: id,
        model_step_ordinal: 0,
        attempt_ordinal: 0,
    };
    let invocation = "30000000-0000-4000-8000-000000000003".to_string();
    app.replace_history(vec![
        Block::Step(StepKey {
            turn_id: id,
            ordinal: 0,
        }),
        Block::Assistant("saved".into()),
        Block::Tool {
            id: invocation.clone(),
            name: "observe.workspace".into(),
            state: ToolState::Unavailable {
                outcome: "succeeded".into(),
            },
            source: ToolSource::HistoryResult,
        },
    ]);
    app.apply_turn_event(TurnEvent::TokenDelta {
        key,
        content: "saved".into(),
    });
    app.apply_turn_event(TurnEvent::ToolCallFinished {
        id: invocation.parse().unwrap(),
        name: "observe.workspace".into(),
        outcome: ToolResultOutcome::Failed,
        material: MaterialSummary::NoContent,
        error_summary: Some("late".into()),
    });
    assert_eq!(
        app.blocks
            .iter()
            .filter(|b| matches!(b, Block::Assistant(_)))
            .count(),
        1
    );
    assert_eq!(
        app.blocks
            .iter()
            .filter(|b| matches!(b, Block::Tool { .. }))
            .count(),
        1
    );
    assert!(app.blocks.iter().any(|b|matches!(b,Block::Tool{state:ToolState::Unavailable{outcome},..} if outcome=="succeeded")));
    // 最后一次请求错误可能晚于关闭快照；Step 已装入不能吞掉 Dock 诊断。
    app.apply_turn_event(TurnEvent::LlmRequestError {
        key,
        retryable: false,
        will_retry: false,
        delay_ms: None,
        message: "late failure".into(),
    });
    assert!(app.dock_status().is_some());
}

#[test]
fn hidden_reasoning_does_not_change_text_on_snapshot_or_history_replacement() {
    let id = lattice_api::local::new_turn_id();
    let key = ModelAttemptKey {
        turn_id: id,
        model_step_ordinal: 0,
        attempt_ordinal: 0,
    };
    let mut live = app();
    live.apply_turn_event(TurnEvent::TokenDelta {
        key,
        content: "before ".into(),
    });
    live.apply_turn_event(TurnEvent::ReasoningDelta {
        key,
        content: "hidden".into(),
    });
    live.apply_turn_event(TurnEvent::TokenDelta {
        key,
        content: "after".into(),
    });
    let render = |app: &App| {
        crate::view::transcript_lines(app, 80)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
    };
    let expected = render(&live);
    assert!(
        expected
            .iter()
            .any(|line| line.trim_start() == "before after"),
        "{expected:?}"
    );
    assert!(!expected.iter().any(|line| line.contains("hidden")));

    let mut snapshot = view(id, "completed");
    snapshot.session.active_turn = Some(id);
    let turn = snapshot.selected_turn.as_mut().unwrap();
    turn.summary = None;
    turn.ended_at = None;
    snapshot.owned = true;
    snapshot.active = Some(
        serde_json::from_value(json!({
            "key":{"turn_id":id,"ordinal":0},
            "state":{"state":"requesting","attempt":0}
        }))
        .unwrap(),
    );
    snapshot.attempt = live.attempt.clone();
    let mut reconnected = app();
    reconnected.apply_update(LocalUpdate::Reset {
        view: Box::new(snapshot.clone()),
        generation: 1,
    });
    let resumed = render(&reconnected);

    let history = serde_json::from_value(json!({
        "anchor":{"session_id":SID,"head":2},
        "entries":[{"node":2,"agent_name":"test","content":{
            "kind":"step","parent":1,"turn_id":id,"step_ordinal":0,
            "content":{"assistant":{"role":"assistant","origin":"open_ai_compatible","parts":[
                {"semantic":{"text":{"text":"before "}}},
                {"semantic":{"reasoning":{"text":"hidden"}}},
                {"semantic":{"text":{"text":"after"}}}
            ]},"results":[]}
        }}]
    }))
    .unwrap();
    live.apply_update(LocalUpdate::History {
        anchor: lattice_api::local::HistoryAnchor {
            session_id: SID.parse().unwrap(),
            head: lattice_api::local::NodeId(2),
        },
        generation: 0,
        stale: false,
        result: Ok(Box::new(history)),
    });
    assert_eq!((resumed, render(&live)), (expected.clone(), expected));
}

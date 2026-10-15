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
/// 已关闭的 Turn 的结局；`progress` 与结局一致。
fn summary(outcome: &str) -> serde_json::Value {
    let (notice, end) = match outcome {
        "failed" => (
            json!({"level":"error","text":"provider failure"}),
            json!({"end":"stop","cause":{"cause":"process_exit"}}),
        ),
        "cancelled" => (
            json!({"level":"info","text":"cancelled by user"}),
            json!({"end":"stop","cause":{"cause":"cancelled"}}),
        ),
        _ => (serde_json::Value::Null, json!({"end":"finish"})),
    };
    json!({"outcome":outcome,"notice":notice,"progress":{"progress":"closed","end":end}})
}
fn view(id: TurnId, outcome: &str) -> ObservationSnapshot {
    serde_json::from_value(json!({
        "session":{"session_id":SID,"project_store_key":"0".repeat(64),"current":2,"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"},
        "current_agent":"test","selected_turn":{"session_id":SID,"turn_id":id,"user_node":1,"agent_name":"test","accepted_at":"2026-01-01T00:00:00Z","ended_at":"2026-01-01T00:00:01Z","summary":summary(outcome),"usage":{"kind":"partial","known":{"prompt_tokens":0,"completion_tokens":0,"total_tokens":0},"unknown_attempts":1},"steps":[]},
        "unclosed_step":null,"history":{"session_id":SID,"head":2},"attempt":null,"cursor":{"epoch":"test","sequence":1},"owned":false
    })).unwrap()
}
/// 执行者消失在 `progress` 阶段：Session 当前位置停在这一轮未关闭的 Step 上，后台没有 Owner。
fn ownerless(id: TurnId, progress: &str, reply: serde_json::Value) -> ObservationSnapshot {
    let mut value = serde_json::to_value(view(id, "failed")).unwrap();
    value["selected_turn"]["summary"]["progress"] = json!({"progress":progress});
    value["history"]["head"] = json!(1);
    value["unclosed_step"] = json!([2, {
        "kind":"step","parent":1,"turn_id":id,"step_ordinal":0,"progress":{"progress":progress},
        "attempts":[],"reply":reply,"slots":[],"updated_at":"2026-01-01T00:00:01Z"
    }]);
    serde_json::from_value(value).unwrap()
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
    for (outcome, shown) in [
        ("completed", "完成"),
        ("failed", "失败：provider failure"),
        ("cancelled", "已取消：cancelled by user"),
    ] {
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
        assert!(text.contains(&format!("turn {id} {shown}")), "{text}");
        // 读取失败是最新的 Warn，暂驻 Dock；此前的观察诊断与结局已落回对话记录。
        assert_eq!(
            app.dock.message(&app.blocks),
            Some(&Status {
                level: NoticeLevel::Warn,
                text:
                    "持久正文不可用；此次内容未验证，/reconnect 重载：读取任务：store unavailable"
                        .into(),
                closing: None,
                retry: None,
            })
        );
        assert!(!text.contains("正文不可用"), "{text}");
        assert!(text.contains("观察诊断：store unavailable"), "{text}");
        assert_eq!(
            app.blocks
                .iter()
                .filter(|b| matches!(
                    b,
                    Block::Status(Status {
                        closing: Some(_),
                        ..
                    })
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
    app.apply_turn_event(TurnEvent::LlmRetry {
        key,
        delay_ms: 100,
        notice: lattice_api::local::Notice {
            level: lattice_api::local::NoticeLevel::Warn,
            text: "retry diagnostic".into(),
        },
    });
    assert!(app.attempt.is_none());
    assert!(
        !app.blocks
            .iter()
            .any(|block| { matches!(block, Block::Assistant(text) if text == "first attempt") })
    );
    assert!(matches!(
        app.dock.message(&app.blocks),
        Some(Status { level: NoticeLevel::Warn, text, closing: None, retry: Some(known) })
            if text.ends_with("：retry diagnostic") && *known == key
    ));
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
    // 关闭事件和快照携带同一结局，已装入的 Step 不能吞掉或重复显示说明。
    let end = json!({"end": "stop", "cause": {"cause": "process_exit"}});
    let summary: lattice_api::local::TurnSummary = serde_json::from_value(json!({
        "outcome":"failed","notice":{"level":"error","text":"late failure"},
        "progress":{"progress":"closed","end":end}
    }))
    .unwrap();
    let step = serde_json::from_value(json!({
        "node": 3,
        "key": {"turn_id": id, "ordinal": 1},
        "progress": {"progress": "closed", "end": end},
        "end": end,
        "attempts": [],
        "slots": [],
        "updated_at": "2026-01-01T00:00:01Z"
    }))
    .unwrap();
    app.apply_turn_event(TurnEvent::TurnFailed {
        turn: lattice_api::local::TurnRef {
            session_id: SID.parse().unwrap(),
            turn_id: id,
        },
        step,
        summary: summary.clone(),
    });
    let mut closed = view(id, "failed");
    closed.selected_turn.as_mut().unwrap().summary = summary;
    app.apply_update(LocalUpdate::State {
        view: Box::new(closed),
        phase: ObservationPhase::NoOwner,
        error: None,
    });
    assert_eq!(
        app.blocks
            .iter()
            .filter(|block| matches!(block, Block::Status(Status {
                closing: Some(_), text, ..
            }) if text.ends_with("：late failure")))
            .count(),
        1
    );
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

    let mut snapshot = ownerless(id, "requesting", serde_json::Value::Null);
    snapshot.owned = true;
    snapshot.attempt = live.attempt.clone();
    let mut reconnected = app();
    reconnected.apply_update(LocalUpdate::Reset {
        view: Box::new(snapshot.clone()),
        generation: 1,
    });
    let resumed = render(&reconnected);

    let history = serde_json::from_value(json!({
        "anchor":{"session_id":SID,"head":2},
        "entries":[{"node":2,"agent_name":"test","summary":snapshot.selected_turn.as_ref().unwrap().summary,"retries":[],"content":{
            "kind":"step","parent":1,"turn_id":id,"step_ordinal":0,
            "progress":{"progress":"closed","end":{"end":"finish"}},"attempts":[],
            "reply":{"role":"assistant","origin":"open_ai_compatible","parts":[
                {"semantic":{"text":{"text":"before "}}},
                {"semantic":{"reasoning":{"text":"hidden"}}},
                {"semantic":{"text":{"text":"after"}}}
            ]},"slots":[],"updated_at":"2026-01-01T00:00:01Z"
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

/// TUI 自己产生的消息：一件事一条消息，等级按消息种类在一处决定，Warn 与 Error 才进 Dock。
#[test]
fn own_messages_are_single_and_carry_their_level() {
    use NoticeLevel::{Error, Info, Warn};
    type Trigger = Box<dyn FnOnce(&mut App)>;
    let id = TurnId::parse("11111111-1111-4111-8111-111111111111").unwrap();
    let turn = || TurnView::from(view(id, "completed").selected_turn.as_ref().unwrap());
    let update = |update: LocalUpdate| -> Trigger {
        Box::new(move |app: &mut App| app.apply_update(update))
    };
    let cancel = |status: CancelStatus, error: Option<LocalError>| {
        update(LocalUpdate::Cancel {
            turn_id: id,
            status,
            error,
        })
    };
    let state = |view: ObservationSnapshot, error: Option<LocalError>| {
        update(LocalUpdate::State {
            view: Box::new(view),
            phase: ObservationPhase::NoOwner,
            error,
        })
    };
    let orphan = ownerless(id, "requesting", serde_json::Value::Null);
    let report = DetachReport {
        session_id: None,
        turn_id: Some(id),
        submission: None,
        submission_diagnostic: None,
        not_submitted: None,
        history_error: None,
        cancel: Some(CancelStatus::Taken),
        cancel_error: None,
        state: None,
    };
    let handoff: Trigger = Box::new(move |app: &mut App| app.apply_handoff(report));
    let cases: Vec<(&str, NoticeLevel, Trigger, String)> = vec![
        (
            "请求被拒绝或回执失败",
            Error,
            update(LocalUpdate::Submission {
                turn_id: id,
                result: Err(read_error()),
            }),
            format!("请求 {id} 被拒绝或回执失败：store unavailable"),
        ),
        (
            "观察诊断",
            Error,
            state(view(id, "completed"), Some(read_error())),
            "观察诊断：store unavailable".into(),
        ),
        (
            "取消诊断",
            Error,
            cancel(CancelStatus::Unknown, Some(read_error())),
            format!(
                "取消确认未知；请按 TurnId 查询或再次明确取消：{id}\n取消诊断：store unavailable"
            ),
        ),
        (
            "本地响应异常",
            Error,
            update(LocalUpdate::Submission {
                turn_id: id,
                result: Ok(Acceptance::NeedsConfiguration {
                    agent_name: "test".into(),
                }),
            }),
            "本地响应异常".into(),
        ),
        (
            "接受确认未知",
            Warn,
            update(LocalUpdate::Submission {
                turn_id: id,
                result: Ok(Acceptance::ConfirmationUnknown {
                    turn_id: id,
                    session_id: None,
                }),
            }),
            format!("接受确认未知：{id}；保留此 ID 查询，不分配新请求"),
        ),
        (
            "提交结果未知",
            Warn,
            update(LocalUpdate::SubmissionUnknown {
                turn_id: id,
                error: None,
            }),
            format!("提交结果未知：{id}；继续观察同一请求，不自动重发"),
        ),
        (
            "提交结果未知带诊断",
            Warn,
            update(LocalUpdate::SubmissionUnknown {
                turn_id: id,
                error: Some(read_error()),
            }),
            format!("提交结果未知：{id}；继续观察同一请求，不自动重发\nstore unavailable"),
        ),
        (
            "后台未接管本次取消",
            Warn,
            cancel(CancelStatus::NotTaken, None),
            format!("后台未接管本次取消；不表示没有后台工作，请查询原 Turn：{id}"),
        ),
        (
            "取消确认未知",
            Warn,
            cancel(CancelStatus::Unknown, None),
            format!("取消确认未知；请按 TurnId 查询或再次明确取消：{id}"),
        ),
        (
            "持久正文不可用",
            Warn,
            update(LocalUpdate::History {
                anchor: orphan.history.clone(),
                generation: 1,
                stale: true,
                result: Err(lattice_api::local::HistoryReadError {
                    context: lattice_api::local::HistoryReadContext::Worker,
                    error: read_error(),
                }),
            }),
            "旧切点持久正文不可用；此次内容未验证，/reconnect 重载：读取任务：store unavailable"
                .into(),
        ),
        (
            "观察连接结束",
            Warn,
            update(LocalUpdate::Connection(ConnectionStatus::Eof)),
            "观察连接结束；不代表 Turn 已关闭，/reconnect 重连".into(),
        ),
        (
            "观察无增量",
            Warn,
            update(LocalUpdate::Connection(ConnectionStatus::Idle)),
            format!(
                "观察 {} s 无增量；执行状态未知，/reconnect 重连",
                lattice_api::STREAM_IDLE_TIMEOUT_SECS
            ),
        ),
        (
            "观察连接出错",
            Warn,
            update(LocalUpdate::Connection(ConnectionStatus::Error(
                read_error(),
            ))),
            "观察连接出错；执行状态未知，/reconnect 重连：store unavailable".into(),
        ),
        (
            "执行者已消失",
            Warn,
            state(orphan.clone(), None),
            format!("turn {id} 的执行者已经消失，它停在已写入的进度；可以直接发送新消息或退回"),
        ),
        (
            "找回既有请求",
            Info,
            update(LocalUpdate::Submission {
                turn_id: id,
                result: Ok(Acceptance::Existing { turn: turn() }),
            }),
            format!("找回既有请求：session {SID} turn {id}；未重复执行"),
        ),
        (
            "已请求取消",
            Info,
            cancel(CancelStatus::Pending, None),
            format!("已请求取消；等待后台接管回执：{id}"),
        ),
        (
            "后台已接管取消",
            Info,
            cancel(CancelStatus::Taken, None),
            format!("后台已接管取消；持久终态仍待确认：{id}"),
        ),
        (
            "取消交接已结束",
            Info,
            cancel(CancelStatus::Moot, None),
            format!("本次取消交接已结束；不作用于其他 Turn，已知结果保持：{id}"),
        ),
        (
            "本客户端已停止后续配置提交",
            Info,
            update(LocalUpdate::NotSubmitted {
                turn_id: id,
                stage: lattice_api::local::NotSubmittedStage::FreezingConfiguration,
            }),
            format!("本客户端已停止后续配置提交：turn {id}；不表示该身份全局未执行"),
        ),
        (
            "历史交接",
            Info,
            handoff,
            format!("历史交接：后台已接管取消；持久终态仍待确认：session 未接受 turn {id}"),
        ),
    ];
    for (name, level, trigger, text) in cases {
        let mut app = app();
        trigger(&mut app);
        // Turn 结局是后端给出等级的消息，不计入本表。
        let pushed = app
            .blocks
            .iter()
            .filter_map(|block| match block {
                Block::Status(status) if status.closing.is_none() => Some(status),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            pushed,
            [&Status {
                level: level.clone(),
                text,
                closing: None,
                retry: None,
            }],
            "{name}"
        );
        assert_eq!(
            app.dock.message(&app.blocks),
            (level != Info).then_some(pushed[0]),
            "{name}"
        );
    }
}

/// 本进程看着结束的 Turn，结局的等级来自 notice：失败是 Error，先暂驻 Dock，释放后在对话记录
/// 出现一次；完成与取消是 Info，直接进对话记录。重复到达的同一结局不再显示。
#[test]
fn turn_closing_follows_the_notice_level() {
    for (outcome, level, symbol, shown) in [
        ("completed", NoticeLevel::Info, "✓", "完成"),
        ("failed", NoticeLevel::Error, "✗", "失败：provider failure"),
        (
            "cancelled",
            NoticeLevel::Info,
            "✗",
            "已取消：cancelled by user",
        ),
    ] {
        let mut app = app();
        let id = lattice_api::local::new_turn_id();
        app.current_turn = Some(id);
        let line = format!("{symbol} turn {id} {shown}");
        let closed = LocalUpdate::State {
            view: Box::new(view(id, outcome)),
            phase: ObservationPhase::Finished,
            error: None,
        };
        let rows = |app: &App| {
            crate::view::transcript_lines(app, 200)
                .iter()
                .filter(|row| row.to_string() == line)
                .count()
        };
        app.apply_update(closed.clone());
        let docked = level == NoticeLevel::Error;
        assert_eq!(
            app.dock
                .message(&app.blocks)
                .map(|status| status.level.clone()),
            docked.then_some(level),
            "{outcome}"
        );
        assert_eq!(rows(&app), usize::from(!docked), "{outcome}");
        if docked {
            app.apply_action(KeyAction::Insert(' '));
        }
        assert!(app.dock.shows_input(), "{outcome}");
        assert_eq!(rows(&app), 1, "{outcome}");
        app.apply_update(closed);
        assert!(app.dock.shows_input(), "{outcome}");
        assert_eq!(rows(&app), 1, "{outcome}");
    }
}

/// 提交时找回的请求早已执行完：回执已让本进程不再把它当作运行中的 Turn，
/// 但结局仍是这次提交的结果，照常交给 Dock。
#[test]
fn closing_of_a_recovered_submission_still_reaches_the_dock() {
    let mut app = app();
    let id = lattice_api::local::new_turn_id();
    let closed = view(id, "failed");
    app.apply_update(LocalUpdate::Submission {
        turn_id: id,
        result: Ok(Acceptance::Existing {
            turn: TurnView::from(closed.selected_turn.as_ref().unwrap()),
        }),
    });
    assert_eq!(app.current_turn, None);
    app.apply_update(LocalUpdate::State {
        view: Box::new(closed),
        phase: ObservationPhase::Finished,
        error: None,
    });
    assert_eq!(
        app.dock
            .message(&app.blocks)
            .map(|status| status.level.clone()),
        Some(NoticeLevel::Error)
    );
}

/// 历史带回每个 Turn 的结局：装入后每轮末尾各有一行，进行中的 Turn 没有，这些行不进 Dock。
/// 本进程看着结束、暂驻 Dock 的结局被历史接管后仍然暂驻，释放后只出现一次。
/// 某个 Turn 离开当前路径后，它的结局行不保留。
#[test]
fn history_shows_each_turn_closing_once_and_takes_over_the_live_one() {
    let turns = [0, 1, 2].map(|_| lattice_api::local::new_turn_id());
    let failed = summary("failed");
    let completed = summary("completed");
    let running = json!({"outcome":"failed","notice":{"level":"error","text":"process exit"},"progress":{"progress":"requesting"}});
    let history = |summaries: &[&serde_json::Value], unfinished: Option<TurnId>| {
        let entries = summaries
            .iter()
            .zip(turns)
            .enumerate()
            .map(|(n, (summary, turn))| {
                json!({"node":n + 1,"agent_name":"test","summary":summary,"retries":[],
                    "content":super::loop_tests::user_message_node(n as u64, turn, &format!("ask-{n}"))})
            })
            .collect::<Vec<_>>();
        crate::session::history_blocks(
            serde_json::from_value(json!({
                "anchor":{"session_id":SID,"head":entries.len()},"entries":entries,
            }))
            .unwrap(),
            unfinished,
        )
    };
    let lines = [
        format!("✗ turn {} 失败：provider failure", turns[0]),
        format!("✓ turn {} 完成", turns[1]),
        format!("✗ turn {} 失败：provider failure", turns[2]),
    ];
    let rows = |app: &App| {
        crate::view::transcript_lines(app, 200)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
    };
    let at = |app: &App, text: &str| rows(app).iter().position(|row| row == text);
    let count = |app: &App, text: &str| rows(app).iter().filter(|row| *row == text).count();

    // 重新打开：前两轮已关闭，第三轮仍在进行。
    let mut app = app();
    app.replace_history(history(&[&failed, &completed, &running], Some(turns[2])));
    assert!(app.dock.shows_input(), "历史里的结局不进 Dock");
    let (first, second) = (at(&app, &lines[0]).unwrap(), at(&app, &lines[1]).unwrap());
    assert!(first < at(&app, "ask-1").unwrap() && at(&app, "ask-1").unwrap() < second);
    assert!(second < at(&app, "ask-2").unwrap());
    assert_eq!(count(&app, &lines[2]), 0, "进行中的 Turn 没有结局");

    // 本进程看着第三轮重试后失败：结局暂驻 Dock；随后历史带回同一结局，接管后仍暂驻。
    app.current_turn = Some(turns[2]);
    app.apply_turn_event(TurnEvent::LlmRetry {
        key: lattice_api::local::ModelAttemptKey {
            turn_id: turns[2],
            model_step_ordinal: 0,
            attempt_ordinal: 0,
        },
        delay_ms: 500,
        notice: lattice_api::local::Notice {
            level: lattice_api::local::NoticeLevel::Warn,
            text: "provider failure".into(),
        },
    });
    app.apply_update(LocalUpdate::State {
        view: Box::new(view(turns[2], "failed")),
        phase: ObservationPhase::Finished,
        error: None,
    });
    assert!(app.dock.message_id().is_some());
    app.replace_history(history(&[&failed, &completed, &failed], None));
    let parked = format!("turn {} 失败：provider failure", turns[2]);
    assert_eq!(
        app.dock
            .message(&app.blocks)
            .map(|status| status.text.as_str()),
        Some(parked.as_str())
    );
    assert_eq!(count(&app, &lines[2]), 0);
    app.apply_action(KeyAction::Insert(' '));
    assert!(app.dock.shows_input());
    for line in &lines {
        assert_eq!(count(&app, line), 1, "{line}");
    }
    // 这一轮的重试提示排在它的结局行之前，结局行是这一轮的最后一行。
    let retry = at(&app, "第 1 次请求失败，500 ms 后重试：provider failure").unwrap();
    assert!(at(&app, "ask-2").unwrap() < retry && retry < at(&app, &lines[2]).unwrap());

    // 回退到第一轮之后：后两轮离开当前路径，它们的结局行一起消失。
    app.replace_history(history(&[&failed], None));
    assert_eq!(count(&app, &lines[0]), 1);
    assert_eq!(count(&app, &lines[1]) + count(&app, &lines[2]), 0);
}

fn transcript_rows(app: &App) -> Vec<String> {
    crate::view::transcript_lines(app, 200)
        .iter()
        .map(|line| line.to_string().trim().to_owned())
        .collect()
}

/// 重新打开时看到执行者已消失的一轮：一条 Warn 暂驻 Dock，结局行直接进对话记录。
/// 停下的那一步只随快照交付，装入后排在结局行之前，结局行仍是这一轮的最后一行；重连不重复。
#[test]
fn ownerless_turn_warns_and_closes_after_its_last_step() {
    let mut app = app();
    let id = lattice_api::local::new_turn_id();
    let reply = json!({"role":"assistant","origin":"open_ai_compatible",
        "parts":[{"semantic":{"text":{"text":"partial reply"}}}]});
    let snapshot = ownerless(id, "executing", reply);
    let summary = &snapshot.selected_turn.as_ref().unwrap().summary;
    let history: lattice_api::local::LocalHistory = serde_json::from_value(json!({
        "anchor":snapshot.history,
        "entries":[{"node":1,"agent_name":"test","summary":summary,"retries":[],
            "content":super::loop_tests::user_message_node(0, id, "ask")}],
    }))
    .unwrap();
    let warn = format!("turn {id} 的执行者已经消失，它停在已写入的进度；可以直接发送新消息或退回");
    let closing = format!("✗ turn {id} 失败：provider failure");
    for generation in [1, 2] {
        app.apply_update(LocalUpdate::State {
            view: Box::new(snapshot.clone()),
            phase: ObservationPhase::NoOwner,
            error: None,
        });
        app.apply_update(LocalUpdate::Reset {
            view: Box::new(snapshot.clone()),
            generation,
        });
        app.apply_update(LocalUpdate::History {
            anchor: snapshot.history.clone(),
            generation,
            stale: false,
            result: Ok(Box::new(history.clone())),
        });
        assert_eq!((app.current_turn, app.phase), (None, TurnPhase::Idle));
        assert_eq!(
            app.dock
                .message(&app.blocks)
                .map(|status| (status.level.clone(), status.text.clone())),
            Some((NoticeLevel::Warn, warn.clone()))
        );
        let rows = transcript_rows(&app);
        let at = |text: &str| rows.iter().position(|row| row == text);
        assert_eq!(
            rows.iter().filter(|row| **row == closing).count(),
            1,
            "{rows:?}"
        );
        assert_eq!(
            rows.iter().filter(|row| *row == "partial reply").count(),
            1,
            "{rows:?}"
        );
        assert!(
            at("ask") < at("partial reply") && at("partial reply") < at(&closing),
            "{rows:?}"
        );
    }
    app.apply_action(KeyAction::Insert(' '));
    let rows = transcript_rows(&app);
    assert_eq!(
        rows.iter().filter(|row| **row == warn).count(),
        1,
        "{rows:?}"
    );
}

/// 历史带回的重试提示与实时的同一条文字和等级相同，排在所属 Step 的回复之前；它是旧消息，不进 Dock。
/// 实时已显示、仍暂驻 Dock 的同一提示由历史取代并继续暂驻，释放后只出现一次；迟到的实时提示不再重复。
#[test]
fn history_retry_notices_precede_their_step_and_replace_live_ones() {
    let id = lattice_api::local::new_turn_id();
    let key = ModelAttemptKey {
        turn_id: id,
        model_step_ordinal: 0,
        attempt_ordinal: 0,
    };
    let retry = || TurnEvent::LlmRetry {
        key,
        delay_ms: 250,
        notice: lattice_api::local::Notice {
            level: NoticeLevel::Warn,
            text: "HTTP 429 rate_limit_exceeded".into(),
        },
    };
    let history = || {
        let entries = json!([
            {"node":1,"agent_name":"test","summary":summary("completed"),"retries":[],
                "content":super::loop_tests::user_message_node(0, id, "ask")},
            {"node":2,"agent_name":"test","summary":summary("completed"),
                "retries":[{"attempt":0,"delay_ms":250,"notice":{"level":"warn","text":"HTTP 429 rate_limit_exceeded"}}],
                "content":super::loop_tests::step_node(1, id, 0, json!([{"semantic":{"text":{"text":"answer"}}}]))},
        ]);
        crate::session::history_blocks(
            serde_json::from_value(json!({"anchor":{"session_id":SID,"head":2},"entries":entries}))
                .unwrap(),
            None,
        )
    };
    let line = "第 1 次请求失败，250 ms 后重试：HTTP 429 rate_limit_exceeded";
    let count = |app: &App| {
        transcript_rows(app)
            .iter()
            .filter(|row| *row == line)
            .count()
    };
    let ordered = |app: &App| {
        let rows = transcript_rows(app);
        let at = |text: &str| rows.iter().position(|row| row == text);
        at("ask") < at(line) && at(line) < at("answer")
    };

    let mut live = app();
    live.apply_turn_event(retry());
    let shown = live.dock.message(&live.blocks).cloned().unwrap();
    assert_eq!(shown.text, line);

    let mut reopened = app();
    reopened.replace_history(history());
    assert!(reopened.dock.shows_input(), "历史带回的重试提示不进 Dock");
    let statuses = reopened
        .blocks
        .iter()
        .filter_map(|block| match block {
            Block::Status(status) if status.closing.is_none() => Some(status),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(statuses, [&shown]);
    assert!(ordered(&reopened));

    live.apply_turn_event(TurnEvent::TokenDelta {
        key: ModelAttemptKey {
            attempt_ordinal: 1,
            ..key
        },
        content: "answer".into(),
    });
    live.replace_history(history());
    assert_eq!(live.dock.message(&live.blocks), Some(&shown));
    live.apply_action(KeyAction::Insert(' '));
    assert!(live.dock.shows_input());
    assert_eq!(count(&live), 1);
    assert!(ordered(&live));
    live.apply_turn_event(retry());
    assert!(live.dock.shows_input());
    assert_eq!(count(&live), 1);
}

/// 退出报告只写给人看的文字：结局用中文词，读取位置用中文说明，不打印内部分类或结构体。
#[test]
fn detach_lines_describe_outcome_and_read_context_in_words() {
    use lattice_api::local::{HistoryCursor, HistoryReadContext, NodeId, NodeRef};
    let id = lattice_api::local::new_turn_id();
    let anchor = view(id, "completed").history;
    for (outcome, closing, context, place) in [
        (
            "completed",
            format!("Turn {id} 完成"),
            HistoryReadContext::Worker,
            "读取任务",
        ),
        (
            "failed",
            format!("Turn {id} 失败：provider failure"),
            HistoryReadContext::Page {
                anchor: anchor.clone(),
                cursor: None,
            },
            "分页（首页）",
        ),
        (
            "cancelled",
            format!("Turn {id} 已取消：cancelled by user"),
            HistoryReadContext::Page {
                anchor: anchor.clone(),
                cursor: Some(HistoryCursor {
                    anchor: anchor.clone(),
                    node: NodeId(7),
                    utf8_offset: 0,
                }),
            },
            "分页（自节点 7 起）",
        ),
        (
            "completed",
            format!("Turn {id} 完成"),
            HistoryReadContext::Node {
                node: NodeRef {
                    session_id: anchor.session_id,
                    node: NodeId(9),
                },
            },
            "节点 9",
        ),
    ] {
        let lines = detach_lines(
            DetachReport {
                session_id: None,
                turn_id: Some(id),
                submission: None,
                submission_diagnostic: None,
                not_submitted: None,
                history_error: Some(lattice_api::local::HistoryReadError {
                    context,
                    error: read_error(),
                }),
                cancel: None,
                cancel_error: Some(read_error()),
                state: Some(LocalState {
                    view: Box::new(view(id, outcome)),
                    phase: ObservationPhase::Finished,
                    error: None,
                }),
            },
            false,
        );
        assert_eq!(
            lines,
            [
                format!("取消诊断：session 未接受 turn {id}：store unavailable"),
                closing,
                format!("持久正文不可用：session 未接受 turn {id}，{place}：store unavailable"),
            ]
        );
    }
}

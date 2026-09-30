// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 真实 LocalTurn、Unix socket 与 run_loop；对端只替代 IPC 系统边界。
use super::*;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{oneshot, watch};

const SESSION: &str = "11111111111141118111111111111111";
fn sid() -> SessionId {
    SessionId::parse(SESSION).unwrap()
}
fn key(id: TurnId, step: u32, attempt: u32) -> ModelAttemptKey {
    ModelAttemptKey {
        turn_id: id,
        model_step_ordinal: step,
        attempt_ordinal: attempt,
    }
}
fn view(id: TurnId, closed: bool) -> ObservationSnapshot {
    let mut value = json!({
        "session":{"session_id":SESSION,"project_store_key":"0".repeat(64),"current":1,"active_turn":id,"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"},
        "current_agent":"unused","selected_turn":{"session_id":SESSION,"turn_id":id,"user_node":1,"agent_name":"unused","accepted_at":"2026-01-01T00:00:00Z","ended_at":null,"summary":null,"usage":{"kind":"partial","known":{"prompt_tokens":0,"completion_tokens":0,"total_tokens":0},"unknown_attempts":1},"steps":[]},
        "active":{"key":{"turn_id":id,"ordinal":0},"state":{"state":"requesting","attempt":1}},
        "history":{"session_id":SESSION,"head":1},"attempt":{"key":key(id,0,1),"segments":[]},"cursor":{"epoch":"peer","sequence":0},"owned":true
    });
    if closed {
        value["session"]["current"] = json!(2);
        value["history"]["head"] = json!(2);
        value["selected_turn"]["steps"] = json!([{ "key":{"turn_id":id,"ordinal":0}, "attempts":[{"attempt":1,"origin":"open_ai_compatible","usage":null,"end":{"end":"completed","step":{"session_id":SESSION,"turn_id":id,"model_step_ordinal":0}}}], "state":{"state":"closed","end":{"end":"finish","node":2},"slots":[]} }]);
        value["session"]["active_turn"] = Value::Null;
        value["selected_turn"]["ended_at"] = json!("2026-01-01T00:00:01Z");
        value["selected_turn"]["summary"] = json!({"outcome":"completed","notice":null});
        value["active"] = Value::Null;
        value["attempt"] = Value::Null;
        value["owned"] = json!(false);
    }
    serde_json::from_value(value).unwrap()
}
fn response(kind: &str, value: Value) -> Value {
    json!({"response":kind,"value":value})
}
fn snapshot(view: &ObservationSnapshot) -> Value {
    response("observation", json!({"type":"snapshot","view":view}))
}
fn accepted(id: TurnId) -> Value {
    response(
        "acceptance",
        json!({"acceptance":"accepted","turn":lattice_api::local::TurnView::from(view(id,false).selected_turn.as_ref().unwrap())}),
    )
}
fn error(message: &str) -> Value {
    response(
        "error",
        json!({"kind":"store_unavailable","message":message,"recovery":null,"store":null}),
    )
}
fn event(seq: u64, event: TurnEvent) -> Value {
    response(
        "observation",
        json!({"type":"event","cursor":{"epoch":"peer","sequence":seq},"event":event}),
    )
}
fn history_entries(view: &ObservationSnapshot) -> Value {
    let id = view.selected_turn.as_ref().unwrap().turn_id;
    let mut entries = vec![
        json!({"node":1,"agent_name":"unused","content":{"kind":"user_message","parent":0,"turn_id":id,"message":{"role":"user","parts":[{"semantic":{"text":{"text":"original prompt"}}}]}}}),
    ];
    if view.history.head.0 == 2 {
        entries.push(json!({"node":2,"agent_name":"unused","content":{"kind":"step","parent":1,"turn_id":id,"step_ordinal":0,"content":{"assistant":{"role":"assistant","origin":"open_ai_compatible","parts":[{"semantic":{"text":{"text":"live body"}}}]},"results":[]}}}));
    }
    json!(entries)
}
fn history(view: &ObservationSnapshot) -> Value {
    let fragments:Vec<_>=history_entries(view).as_array().unwrap().iter().map(|entry|json!({"agent_name":entry["agent_name"],"fragment":{"node":entry["node"],"utf8_offset":0,"json_fragment":serde_json::to_string(&entry["content"]).unwrap(),"complete":true}})).collect();
    response(
        "history",
        json!({"anchor":view.history,"fragments":fragments,"next":null}),
    )
}
fn finished(view: &ObservationSnapshot) -> Value {
    response(
        "observation",
        json!({"type":"finished","turn":{"session_id":sid(),"turn_id":view.selected_turn.as_ref().unwrap().turn_id},"cursor":view.cursor,"history":{"anchor":view.history,"entries":history_entries(view)}}),
    )
}
fn comment_history(app: &mut App, id: TurnId, parts: Value, closed: bool) {
    let mut snapshot = view(id, closed);
    snapshot.history.head = lattice_api::NodeId(2);
    snapshot.session.current = lattice_api::NodeId(2);
    let mut entries = history_entries(&view(id, true));
    entries[1]["content"]["content"]["assistant"]["parts"] = parts;
    if !closed {
        // 已关闭的工具 Step 之后仍在请求下一步；零工具最终回复不能伪装成进行中 Step。
        let part = entries[1]["content"]["content"]["assistant"]["parts"]
            .as_array()
            .unwrap()
            .len();
        let call = json!({
            "invocation_id":"22222222-2222-4222-8222-222222222222", "provider_call_id":"call_1",
            "name":"observe.workspace", "arguments":{"path":"a.txt"},
            "arguments_digest":"5fd582703776c899a2ecb12f3d699bd7244dff1f443ef5ae3f0067bcf3ce279e",
            "address":{"message":{"session_id":SESSION,"turn_id":id,"model_step_ordinal":0},"part_ordinal":part}
        });
        entries[1]["content"]["content"]["assistant"]["parts"]
            .as_array_mut()
            .unwrap()
            .push(json!({"semantic":{"tool_call":{"call":call}}}));
        entries[1]["content"]["content"]["results"] = json!([{
            "invocation_id":"22222222-2222-4222-8222-222222222222", "name":"observe.workspace",
            "payload":{"outcome":"succeeded","payload":{"kind":"json","value":null,"digest":"4514e821d251d896aaed09c92f07abacecb5683e0714471682fa6beaca071aa2","byte_length":4}}
        }]);
        snapshot.active.as_mut().unwrap().key.ordinal = 1;
        snapshot.attempt.as_mut().unwrap().key.model_step_ordinal = 1;
    }
    app.apply_update(LocalUpdate::State {
        view: Box::new(snapshot.clone()),
        phase: if closed {
            ObservationPhase::Finished
        } else {
            ObservationPhase::Live
        },
        error: None,
    });
    app.apply_update(LocalUpdate::History {
        anchor: snapshot.history.clone(),
        generation: app.generation,
        stale: false,
        result: Ok(Box::new(
            serde_json::from_value(json!({"anchor": snapshot.history, "entries": entries}))
                .unwrap(),
        )),
    });
}

fn comment_browse(app: &mut App, text: &str) {
    let transcript = app.prepare_transcript();
    let row = transcript
        .lines
        .iter()
        .position(|line| line.to_string().contains(text))
        .expect("visible text");
    app.focus = Focus::Transcript(transcript.cursor(row, 0).unwrap());
    app.viewport.pause();
}

#[test]
fn comment_history_anchor_uses_original_part_and_only_closed_single_fragment() {
    let mut h = Harness::new(false);
    let id = lattice_api::local::new_turn_id();
    let parts = json!([{"semantic":{"reasoning":{"text":"hidden"}}},{"semantic":{"text":{"text":"comment target"}}}]);
    comment_history(&mut h.app, id, parts.clone(), false);
    comment_browse(&mut h.app, "comment target");
    h.app.apply_action(KeyAction::Submit);
    assert!(!h.app.queue.is_editing());
    let mut h = Harness::new(false);
    let id = lattice_api::local::new_turn_id();
    comment_history(&mut h.app, id, parts, true);
    comment_browse(&mut h.app, "comment target");
    h.app.apply_action(KeyAction::Submit);
    assert!(
        h.app.queue.is_editing(),
        "closed History text opens comment editor"
    );
    h.app.apply_action(KeyAction::Paste("note".into()));
    h.app.apply_action(KeyAction::Submit);
    h.app.focus = Focus::Dock;
    h.app.input.set_text(" \n\t");
    let wire = h.app.apply_action(KeyAction::Submit).unwrap();
    let message = crate::comment::Message::parse(&wire).unwrap();
    assert_eq!(message.comments[0].location.fragment.part, 1);
    assert_eq!(message.comments[0].location.fragment.step, 0);
    assert_eq!(message.comments[0].quote, "comment target");
    assert!(message.text.is_empty());
    assert_eq!(h.app.history.prev(), None);
}

fn comment_app() -> Harness {
    let mut h = Harness::new(false);
    h.app.term_width = 100;
    h.app.term_height = 40;
    let id = lattice_api::local::new_turn_id();
    comment_history(
        &mut h.app,
        id,
        json!([{"semantic":{"text":{"text":"comment target"}}}]),
        true,
    );
    h
}

fn save_comment(app: &mut App, text: &str) {
    comment_browse(app, "comment target");
    app.apply_action(KeyAction::Submit);
    assert!(app.queue.is_editing());
    app.apply_action(KeyAction::Paste(text.into()));
    app.apply_action(KeyAction::Submit);
}

fn comment_screen(app: &mut App) -> String {
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 40)).unwrap();
    let frame = terminal.draw(|f| crate::view::draw(f, app)).unwrap();
    screen_text(frame.buffer)
}

/// 包含 `text` 的对话行中，`text` 所在的片段是否铺了评论锚点底色。
fn anchor_highlighted(app: &mut App, text: &str) -> bool {
    let transcript = app.prepare_transcript();
    let line = transcript
        .lines
        .iter()
        .find(|line| line.to_string().contains(text))
        .expect("visible text");
    line.spans
        .iter()
        .filter(|span| text.contains(span.content.trim()) && !span.content.trim().is_empty())
        .all(|span| span.style.bg == Some(crate::view::COMMENT_ANCHOR_BG))
}

#[test]
fn comment_three_esc_paths_saved_count_and_blank_save() {
    let mut h = comment_app();
    let app = &mut h.app;
    assert!(!anchor_highlighted(app, "comment target"));
    comment_browse(app, "comment target");
    app.apply_action(KeyAction::Submit);
    app.apply_action(KeyAction::Paste("new discarded".into()));
    assert!(!comment_screen(app).contains("条评论待发送"));
    assert!(
        anchor_highlighted(app, "comment target"),
        "正在写的新评论已高亮锚点"
    );
    app.apply_action(KeyAction::Clear);
    assert!(!frame(app).text.contains("new discarded"));
    assert!(!anchor_highlighted(app, "comment target"), "丢弃后取消高亮");
    save_comment(app, "saved note");
    assert!(comment_screen(app).contains("1 条评论待发送"));
    assert!(anchor_highlighted(app, "comment target"));
    comment_browse(app, "saved note");
    app.apply_action(KeyAction::Submit);
    app.apply_action(KeyAction::Paste(" discarded edit".into()));
    app.apply_action(KeyAction::Clear);
    assert!(frame(app).text.contains("saved note"));
    assert!(!frame(app).text.contains("discarded edit"));
    comment_browse(app, "saved note");
    app.apply_action(KeyAction::Clear);
    assert!(!comment_screen(app).contains("条评论待发送"));
    assert!(!anchor_highlighted(app, "comment target"));
    save_comment(app, " \n\t");
    assert_eq!(app.queue.draft_count(), 0);
}

fn reject_comment(app: &mut App, id: TurnId, receipt: usize) {
    let update = match receipt {
        0 | 1 => LocalUpdate::Submission {
            turn_id: id,
            result: Err(lattice_api::local::LocalError {
                kind: if receipt == 0 {
                    lattice_api::local::LocalErrorKind::Busy
                } else {
                    lattice_api::local::LocalErrorKind::BadRequest
                },
                message: "rejected".into(),
                recovery: None,
                store: None,
            }),
        },
        2 => LocalUpdate::NotSubmitted {
            turn_id: id,
            stage: lattice_api::local::NotSubmittedStage::FreezingConfiguration,
        },
        _ => LocalUpdate::Submission {
            turn_id: id,
            result: Ok(Acceptance::NeedsConfiguration {
                agent_name: "unused".into(),
            }),
        },
    };
    app.apply_update(update);
}

#[test]
fn comment_manual_rejections_keep_user_markers_and_restore_one_draft_copy() {
    for receipt in 0..4 {
        let mut h = comment_app();
        let app = &mut h.app;
        save_comment(app, "rejected note");
        app.focus = Focus::Dock;
        app.input.set_text("body");
        let wire = app.apply_action(KeyAction::Submit).unwrap();
        assert_eq!(crate::comment::Message::parse(&wire).unwrap().text, "body");
        assert_eq!(app.history.prev(), Some("body"));
        let id = app.current_turn.unwrap();
        reject_comment(app, id, receipt);
        assert_eq!(app.input.text(), if receipt == 3 { "" } else { "body" });
        let text = frame(app).text;
        assert_eq!(text.matches("[COMMENT 1]").count(), 1);
        assert_eq!(
            text.matches("rejected note").count(),
            1,
            "receipt {receipt}"
        );
        assert!(text.contains("草稿 · #1"), "receipt {receipt}: {text}");
        comment_browse(app, "[COMMENT 1]");
        app.apply_action(KeyAction::Submit);
        let Focus::Transcript(cursor) = app.focus else {
            panic!("draft jump");
        };
        assert_eq!(Some(cursor.block.id), app.queue.pending_comment_id(1));
        app.apply_action(KeyAction::Submit);
        assert!(app.queue.is_editing(), "restored draft remains editable");
        app.apply_action(KeyAction::Clear);
        comment_browse(app, "rejected note");
        app.apply_action(KeyAction::Clear);
        comment_browse(app, "[COMMENT 1]");
        let before = app.focus;
        app.apply_action(KeyAction::Submit);
        assert_eq!(app.focus, before);
        save_comment(app, "next note");
        assert_eq!(
            app.queue.drafts().next().unwrap().comment().n,
            2,
            "retained rejected marker reserves its number"
        );
    }
}

#[test]
fn comment_queue_only_edit_jump_rejection_and_delete() {
    for receipt in 0..4 {
        let mut h = comment_app();
        let app = &mut h.app;
        app.focus = Focus::Dock;
        app.input.set_text("running prompt");
        app.submit().unwrap();
        save_comment(app, "queued note");
        app.focus = Focus::Dock;
        app.input.set_text("queued body");
        assert!(app.apply_action(KeyAction::Submit).is_none());
        let message_id = app.queue.messages().next().unwrap().id;
        assert!(frame(app).text.contains("排队 · #1"));
        assert!(
            anchor_highlighted(app, "comment target"),
            "排队评论高亮锚点"
        );
        assert_eq!(app.queue.draft_count(), 0);
        comment_browse(app, "[COMMENT 1]");
        app.apply_action(KeyAction::Submit);
        app.apply_action(KeyAction::Submit);
        let Focus::Transcript(marker) = app.focus else {
            panic!("queue marker");
        };
        assert_eq!(marker.block.id, message_id);
        // 从队列标题进入编辑，空正文仍保留只带评论的消息。
        let transcript = app.prepare_transcript();
        let row = transcript
            .owners
            .iter()
            .position(|o| o.id == message_id)
            .unwrap();
        app.focus = Focus::Transcript(transcript.cursor(row, 0).unwrap());
        app.apply_action(KeyAction::Submit);
        assert!(app.queue.is_editing());
        app.apply_action(KeyAction::Home);
        for _ in "queued body".chars() {
            app.apply_action(KeyAction::Delete);
        }
        app.apply_action(KeyAction::Insert(' '));
        app.apply_action(KeyAction::Submit);
        assert_eq!(app.queue.messages().count(), 1);
        let running = app.current_turn.unwrap();
        app.apply_update(LocalUpdate::State {
            view: Box::new(view(running, true)),
            phase: ObservationPhase::Finished,
            error: None,
        });
        let wire = app.auto_submit().unwrap();
        assert!(!frame(app).text.contains("排队 · #1"));
        assert_eq!(frame(app).text.matches("queued note").count(), 1);
        assert_eq!(crate::comment::Message::parse(&wire).unwrap().text, "");
        assert_eq!(app.history.prev(), Some("running prompt"));
        let id = app.current_turn.unwrap();
        reject_comment(app, id, receipt);
        assert!(app.input.is_empty());
        assert!(app.queue.is_paused());
        let text = frame(app).text;
        assert_eq!(text.matches("[COMMENT 1]").count(), 2);
        assert_eq!(text.matches("queued note").count(), 1);
        comment_browse(app, "queued note");
        app.apply_action(KeyAction::Submit);
        let Focus::Transcript(marker) = app.focus else {
            panic!("queue marker");
        };
        assert_eq!(
            marker.block.id, message_id,
            "queued comment jumps to queued marker, not rejected USER"
        );
        let transcript = app.prepare_transcript();
        let row = transcript
            .owners
            .iter()
            .position(|o| o.id == message_id)
            .unwrap();
        app.focus = Focus::Transcript(transcript.cursor(row, 0).unwrap());
        app.apply_action(KeyAction::Clear);
        assert_eq!(app.queue.messages().count(), 0);
        assert!(!frame(app).text.contains("queued note"));
    }
}

fn reload_comment(app: &mut App, target: TurnId, wire: &str, on_path: bool) {
    let new_turn = lattice_api::local::new_turn_id();
    let mut snapshot = serde_json::to_value(view(new_turn, true)).unwrap();
    let user_node = if on_path { 3 } else { 1 };
    let head = user_node + 1;
    snapshot["session"]["current"] = json!(head);
    snapshot["history"]["head"] = json!(head);
    snapshot["selected_turn"]["user_node"] = json!(user_node);
    snapshot["selected_turn"]["steps"][0]["state"]["end"]["node"] = json!(head);
    let snapshot: ObservationSnapshot = serde_json::from_value(snapshot).unwrap();
    let mut entries = if on_path {
        let mut old = history_entries(&view(target, true));
        old[1]["content"]["content"]["assistant"]["parts"][0]["semantic"]["text"]["text"] =
            json!("comment target");
        old.as_array().unwrap().clone()
    } else {
        vec![]
    };
    entries.push(json!({"node":user_node,"agent_name":"unused","content":{"kind":"user_message","parent":user_node - 1,"turn_id":new_turn,"message":{"role":"user","parts":[{"semantic":{"text":{"text":wire}}}]}}}));
    entries.push(json!({"node":head,"agent_name":"unused","content":{"kind":"step","parent":user_node,"turn_id":new_turn,"step_ordinal":0,"content":{"assistant":{"role":"assistant","parts":[{"semantic":{"text":{"text":"following answer"}}}]},"results":[]}}}));
    app.apply_update(LocalUpdate::State {
        view: Box::new(snapshot.clone()),
        phase: ObservationPhase::Finished,
        error: None,
    });
    app.apply_update(LocalUpdate::History {
        anchor: snapshot.history.clone(),
        generation: app.generation,
        stale: false,
        result: Ok(Box::new(
            serde_json::from_value(json!({"anchor":snapshot.history,"entries":entries})).unwrap(),
        )),
    });
}

#[test]
fn comment_reload_attaches_or_falls_back_and_jumps_both_ways() {
    let mut source = comment_app();
    save_comment(&mut source.app, "reload note");
    source.app.focus = Focus::Dock;
    let wire = source.app.submit().unwrap();
    let original = crate::comment::Message::parse(&wire).unwrap();
    let target = original.comments[0].location.fragment.turn;
    for mismatch in 0..4 {
        let mut message = original.clone();
        if mismatch == 1 {
            message.comments[0].quote = "different source".into();
        }
        if mismatch == 2 {
            message.comments[0].location.range = 999..1000;
        }
        let mut h = Harness::new(false);
        reload_comment(&mut h.app, target, &message.encode(), mismatch != 3);
        let app = &mut h.app;
        let text = frame(app).text;
        assert_eq!(text.matches("reload note").count(), 1);
        assert_eq!(text.contains("原文不匹配 · #1"), mismatch != 0);
        if mismatch != 3 {
            // 只有挂到锚点的已发送评论高亮；原文不匹配时锚点不高亮。
            assert_eq!(anchor_highlighted(app, "comment target"), mismatch == 0);
        }
        if mismatch == 0 {
            assert!(text.find("reload note").unwrap() < text.find("[COMMENT 1]").unwrap());
        } else {
            assert!(text.find("reload note").unwrap() > text.find("[COMMENT 1]").unwrap());
        }
        comment_browse(app, "[COMMENT 1]");
        let marker = app.focus;
        app.apply_action(KeyAction::Submit);
        assert_ne!(app.focus, marker);
        app.apply_action(KeyAction::Submit);
        assert_eq!(app.focus, marker);
        assert!(!app.queue.is_editing());
    }
}

#[test]
fn comment_missing_step_discards_drafts_and_comment_only_queue_but_keeps_text() {
    let mut h = comment_app();
    let app = &mut h.app;
    app.input.set_text("running");
    app.submit().unwrap();
    save_comment(app, "only queued note");
    app.focus = Focus::Dock;
    app.submit();
    save_comment(app, "text queued note");
    app.focus = Focus::Dock;
    app.input.set_text("kept text");
    app.submit();
    save_comment(app, "draft note");
    let target = app
        .queue
        .drafts()
        .next()
        .unwrap()
        .comment()
        .location
        .fragment
        .turn;
    // 退回到同一 Turn 的 UserMessage：Turn 仍在路径上，Step 已不在。
    let mut snapshot = view(target, true);
    snapshot.session.current = lattice_api::NodeId(1);
    snapshot.history.head = lattice_api::NodeId(1);
    app.apply_update(LocalUpdate::State {
        view: Box::new(snapshot.clone()),
        phase: ObservationPhase::Finished,
        error: None,
    });
    app.apply_update(LocalUpdate::History {
        anchor: snapshot.history.clone(),
        generation: app.generation,
        stale: false,
        result: Ok(Box::new(
            serde_json::from_value(
                json!({"anchor":snapshot.history,"entries":history_entries(&snapshot)}),
            )
            .unwrap(),
        )),
    });
    assert_eq!(app.queue.draft_count(), 0);
    assert_eq!(
        app.queue
            .messages()
            .map(|m| m.text.as_str())
            .collect::<Vec<_>>(),
        ["kept text"]
    );
    assert!(app.queue.messages().next().unwrap().comments.is_empty());
    assert!(frame(app).text.contains("丢弃评论：#1、#2、#3"));
}

#[test]
fn comment_warn_error_keeps_space_paste_and_hidden_input_untouched() {
    for severity in [DockSeverity::Warn, DockSeverity::Error] {
        let mut h = comment_app();
        let app = &mut h.app;
        app.input.set_text("hidden input");
        comment_browse(app, "comment target");
        app.apply_action(KeyAction::Submit);
        app.push_dock_status(
            StatusKind::TurnFailed {
                reason: "blocked dock".into(),
            },
            severity,
        );
        app.apply_action(KeyAction::Insert('a'));
        app.apply_action(KeyAction::Insert(' '));
        app.apply_action(KeyAction::Paste("b".into()));
        app.apply_action(KeyAction::CancelOrClear);
        app.apply_action(KeyAction::Submit);
        assert_eq!(app.queue.drafts().next().unwrap().comment().text, "a b");
        assert_eq!(app.input.text(), "hidden input");
        assert!(app.dock_status().is_some());
    }
}

#[test]
fn comment_adjacent_text_fragments_and_live_text_are_not_commentable() {
    let mut h = Harness::new(false);
    let id = lattice_api::local::new_turn_id();
    comment_history(
        &mut h.app,
        id,
        json!([{"semantic":{"text":{"text":"first "}}},{"semantic":{"text":{"text":"second"}}}]),
        true,
    );
    comment_browse(&mut h.app, "first second");
    h.app.apply_action(KeyAction::Submit);
    assert!(!h.app.queue.is_editing());
    h.app.focus = Focus::Dock;
    h.app.input.set_text("run");
    h.app.submit().unwrap();
    let live = h.app.current_turn.unwrap();
    h.app.apply_turn_event(TurnEvent::TokenDelta {
        key: key(live, 0, 1),
        content: "live comment target".into(),
    });
    comment_browse(&mut h.app, "live comment target");
    h.app.apply_action(KeyAction::Submit);
    assert!(!h.app.queue.is_editing());
}

#[test]
fn comment_numbering_combines_history_queue_and_drafts_and_does_not_resume_with_drafts() {
    let mut source = comment_app();
    save_comment(&mut source.app, "history note");
    source.app.focus = Focus::Dock;
    let mut message = crate::comment::Message::parse(&source.app.submit().unwrap()).unwrap();
    let target = message.comments[0].location.fragment.turn;
    message.comments[0].n = 7;
    let mut h = Harness::new(false);
    let app = &mut h.app;
    reload_comment(app, target, &message.encode(), true);
    app.input.set_text("running");
    app.submit().unwrap();
    let running = app.current_turn.unwrap();
    save_comment(app, "queue eight");
    app.focus = Focus::Dock;
    app.submit();
    assert_eq!(app.queue.messages().next().unwrap().comments[0].n, 8);
    app.apply_update(LocalUpdate::Connection(ConnectionStatus::Eof));
    reject_comment(app, running, 0);
    app.input.clear();
    save_comment(app, "draft nine");
    save_comment(app, "draft ten");
    app.focus = Focus::Dock;
    let outgoing = crate::comment::Message::parse(&app.submit().unwrap()).unwrap();
    assert_eq!(
        outgoing.comments.iter().map(|c| c.n).collect::<Vec<_>>(),
        [9, 10]
    );
    assert!(app.queue.is_paused());
    assert_eq!(app.queue.messages().count(), 1);
    assert_eq!(
        app.history.prev(),
        Some("running"),
        "comment-only input is not history"
    );
}

#[test]
fn comment_invalid_format_is_plain_markdown_with_literal_html() {
    let mut source = comment_app();
    save_comment(&mut source.app, "note");
    source.app.focus = Focus::Dock;
    let wire = source.app.submit().unwrap();
    for text in [
        wire.replace("n=\"1\"", "n=\"0\""),
        wire.replace("step=\"0\"", "step=\"01\""),
        format!("{wire}\n\n<lattice-comment broken>\n<!-- raw -->"),
        "<b>literal</b>\n<!-- raw -->".into(),
    ] {
        assert_eq!(
            crate::session::user_block(text.clone()),
            Block::User(text.clone())
        );
        let shown = crate::markdown::render(&text, 200)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(shown.contains(text.lines().next().unwrap()));
        if text.contains("<!-- raw -->") {
            assert!(shown.contains("<!-- raw -->"));
        }
    }
}

#[test]
fn dock_status_survives_r2_history_until_space() {
    for will_retry in [true, false] {
        let mut h = Harness::new(false);
        let id = lattice_api::local::new_turn_id();
        h.app.apply_update(LocalUpdate::Reset {
            view: Box::new(view(id, false)),
            generation: 1,
        });
        h.app.apply_turn_event(TurnEvent::LlmRequestError {
            key: key(id, 0, 1),
            retryable: will_retry,
            will_retry,
            delay_ms: Some(100),
            message: "dock-marker".into(),
        });
        let closed = view(id, true);
        let loaded: lattice_api::local::LocalHistory = serde_json::from_value(
            json!({"anchor":closed.history,"entries":history_entries(&closed)}),
        )
        .unwrap();
        for _ in 0..2 {
            h.app.apply_update(LocalUpdate::History {
                anchor: closed.history.clone(),
                result: Ok(Box::new(loaded.clone())),
                stale: false,
                generation: 1,
            });
            assert!(
                h.app.dock_status().is_some(),
                "History must not release Dock"
            );
            assert!(!frame(&h.app).text.contains("dock-marker"));
        }
        h.app.apply_action(KeyAction::Insert(' '));
        assert!(h.app.dock_status().is_none());
        assert_eq!(frame(&h.app).text.matches("dock-marker").count(), 1);
        h.app
            .replace_history(crate::session::history_blocks(loaded));
        assert_eq!(frame(&h.app).text.matches("dock-marker").count(), 1);
    }
}

fn screen_text(buffer: &ratatui::buffer::Buffer) -> String {
    use unicode_width::UnicodeWidthStr;
    let mut text = String::new();
    for row in buffer.content.chunks(usize::from(buffer.area.width).max(1)) {
        let mut col = 0usize;
        while let Some(cell) = row.get(col) {
            text.push_str(cell.symbol());
            col = col.saturating_add(cell.symbol().width().max(1));
        }
        text.push('\n');
    }
    text
}

#[test]
fn running_enter_queues_verbatim_without_history_and_preserves_full_input() {
    for cancelling in [false, true] {
        let mut h = Harness::new(true);
        let app = &mut h.app;
        app.input.set_text("running");
        app.apply_action(KeyAction::Submit).unwrap();
        if cancelling {
            app.apply_action(KeyAction::CancelOrClear);
        }
        app.input.set_text(" \n ");
        assert_eq!(app.apply_action(KeyAction::Submit), None);
        assert_eq!(app.input.text(), " \n ");
        app.input.set_text("/help");
        app.apply_action(KeyAction::Submit);
        for n in 0..5 {
            app.input.set_text(&format!("  queued {n}\n**plain**  "));
            assert_eq!(app.apply_action(KeyAction::Submit), None);
            assert!(
                app.input.is_empty(),
                "running Enter must clear enqueued input"
            );
        }
        assert_eq!(app.history.prev(), Some("/help"));
        app.input.set_text("sixth");
        app.apply_action(KeyAction::Submit);
        assert_eq!(app.input.text(), "sixth");
        let text = frame(app).text;
        assert_eq!(text.matches("排队").count(), 5);
        assert!(text.contains("**plain**"));
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 40)).unwrap();
        terminal
            .draw(|frame| crate::view::draw(frame, app))
            .unwrap();
        let screen = screen_text(terminal.backend().buffer());
        assert!(screen.contains("Enter 排队 · 排队已满"), "{screen}");
    }
}

#[test]
fn queued_editor_saves_discards_deletes_and_keeps_warn_input_local() {
    for severity in [DockSeverity::Warn, DockSeverity::Error] {
        let mut h = Harness::new(true);
        let app = &mut h.app;
        app.input.set_text("running");
        app.submit().unwrap();
        for text in ["first", "ab中\ncd"] {
            app.input.set_text(text);
            app.submit();
        }
        app.apply_action(KeyAction::Up);
        assert!(frame(app).text.contains("Enter 编辑 · Esc 删除"));
        app.apply_action(KeyAction::Submit);
        assert!(frame(app).text.contains("Enter 完成编辑"));
        app.push_dock_status(
            StatusKind::TurnFailed {
                reason: "dock marker".into(),
            },
            severity,
        );
        app.apply_action(KeyAction::End);
        app.apply_action(KeyAction::Insert(' '));
        app.apply_action(KeyAction::Paste("e\nf".into()));
        app.apply_action(KeyAction::Newline);
        app.apply_action(KeyAction::Insert('g'));
        app.apply_action(KeyAction::Home);
        app.apply_action(KeyAction::Delete);
        app.apply_action(KeyAction::Backspace);
        app.apply_action(KeyAction::Insert('!'));
        app.apply_action(KeyAction::Submit);
        assert!(frame(app).text.contains("cd e\nf!"));
        assert!(app.dock_status().is_some());
        assert!(matches!(app.focus, Focus::Transcript(_)));
        app.apply_action(KeyAction::Submit);
        app.apply_action(KeyAction::Insert('x'));
        app.apply_action(KeyAction::Clear);
        assert!(!frame(app).text.contains("f!x"));
        app.apply_action(KeyAction::Insert(' '));
        assert!(app.dock_status().is_none());
        app.apply_action(KeyAction::Clear);
        assert!(!frame(app).text.contains("ab中"));
        assert!(frame(app).text.contains("first"));
        assert!(matches!(app.focus, Focus::Transcript(_)));
    }
}
#[tokio::test]
async fn completed_observation_sends_queue_through_real_loop_once() {
    let id = lattice_api::local::new_turn_id();
    let closed = view(id, true);
    let (tx, rx) = unbounded();
    tx.unbounded_send(snapshot(&closed)).unwrap();
    let mut rx = Some(rx);
    let peer = Peer::start(move |r| match r["operation"].as_str().unwrap() {
        "subscribe" if r["target"]["turn_id"] == json!(id) => Reply::Stream(rx.take().unwrap()),
        "history" => Reply::Never,
        "submit" => Reply::Now(accepted(
            serde_json::from_value(r["request"]["turn_id"].clone()).unwrap(),
        )),
        "subscribe" => Reply::Never,
        _ => panic!("unexpected request: {r}"),
    });
    let mut h = Harness::new(true);
    h.app.apply_update(LocalUpdate::State {
        view: Box::new(view(id, false)),
        phase: ObservationPhase::Live,
        error: None,
    });
    let live = view(id, false);
    h.app.apply_update(LocalUpdate::History {
        anchor: live.history.clone(),
        generation: 0,
        stale: false,
        result: Ok(Box::new(
            serde_json::from_value(json!({
                "anchor":live.history,"entries":history_entries(&live),
            }))
            .unwrap(),
        )),
    });
    for text in ["queued via loop", "second"] {
        h.app.input.set_text(text);
        h.app.submit();
    }
    h.app.input.set_text("untouched draft");
    let remote = h.remote.clone();
    let driver = async {
        let seen = peer.wait(|s| count(s, "submit") == 1).await;
        let request = seen.iter().find(|r| r["operation"] == "submit").unwrap();
        assert_eq!(request["request"]["input"], "queued via loop");
        assert_ne!(submitted(&seen), id);
        let rendered = remote.wait(|f| f.started && f.current != Some(id)).await;
        assert_eq!(rendered.input, "untouched draft");
        assert_eq!(
            rendered.text.matches("排队").count(),
            1,
            "{}",
            rendered.text
        );
        assert!(rendered.text.contains(" #2 ──"));
        remote.press(KeyCode::Esc, KeyModifiers::NONE);
        remote.line("/quit");
    };
    let (exit, ()) = tokio::join!(
        h.run(
            &peer.client,
            Some(peer.client.observe(Some(sid()), Some(id)))
        ),
        driver,
    );
    assert!(exit.result.is_ok());
    assert_eq!(count(&peer.seen.borrow(), "submit"), 1);
}

fn finish_completed(app: &mut App, id: TurnId) {
    let mut closed = view(id, true);
    let mut entries = Vec::new();
    let mut turn_id = id;
    for block in app.blocks.iter() {
        match block {
            Block::UserTurn(turn, _) => turn_id = *turn,
            Block::User(text) => {
                let node = entries.len() as u64 + 1;
                entries.push(json!({"node":node,"agent_name":"unused","content":{
                    "kind":"user_message","parent":node - 1,"turn_id":turn_id,
                    "message":{"role":"user","parts":[{"semantic":{"text":{"text":text}}}]},
                }}));
                entries.push(json!({"node":node + 1,"agent_name":"unused","content":{
                    "kind":"step","parent":node,"turn_id":turn_id,"step_ordinal":0,
                    "content":{"assistant":{"role":"assistant","origin":"open_ai_compatible","parts":[{"semantic":{"text":{"text":"live body"}}}]},"results":[]},
                }}));
            }
            _ => {}
        }
    }
    let node = entries.len() as u64;
    closed.session.current = lattice_api::NodeId(node);
    closed.history.head = lattice_api::NodeId(node);
    closed.selected_turn.as_mut().unwrap().user_node = lattice_api::NodeId(node - 1);
    let mut serialized = serde_json::to_value(&closed).unwrap();
    serialized["selected_turn"]["steps"][0]["state"]["end"]["node"] = json!(node);
    closed = serde_json::from_value(serialized).unwrap();
    app.apply_update(LocalUpdate::State {
        view: Box::new(closed.clone()),
        phase: ObservationPhase::NoOwner,
        error: None,
    });
    app.apply_update(LocalUpdate::History {
        anchor: closed.history.clone(),
        generation: app.generation,
        stale: false,
        result: Ok(Box::new(
            serde_json::from_value(json!({
                "anchor":closed.history,"entries":entries,
            }))
            .unwrap(),
        )),
    });
    app.apply_update(LocalUpdate::Finished {
        turn: Some(lattice_api::local::TurnRef {
            session_id: sid(),
            turn_id: id,
        }),
        anchor: closed.history,
        generation: app.generation,
    });
}

fn queued_app() -> Harness {
    let mut h = Harness::new(true);
    h.app.input.set_text("running");
    h.app.submit().unwrap();
    let id = h.app.current_turn.unwrap();
    h.app.apply_update(LocalUpdate::Submission {
        turn_id: id,
        result: Ok(Acceptance::Accepted {
            turn: lattice_api::local::TurnView::from(
                view(id, false).selected_turn.as_ref().unwrap(),
            ),
        }),
    });
    for n in 0..5 {
        h.app.input.set_text(&format!(" q{n}\nraw "));
        h.app.submit();
    }
    finish_completed(&mut h.app, id);
    h
}

#[test]
fn automatic_rejections_restore_exact_front_and_do_not_touch_input() {
    for receipt in 0..4 {
        let mut h = queued_app();
        let app = &mut h.app;
        assert_eq!(app.auto_submit(), Some(" q0\nraw ".into()));
        let id = app.current_turn.unwrap();
        assert!(app.queue.is_full(), "awaiting automatic prompt owns a slot");
        app.input.set_text("user is typing");
        app.submit();
        assert_eq!(app.input.text(), "user is typing");
        let update = match receipt {
            0 | 1 => LocalUpdate::Submission {
                turn_id: id,
                result: Err(lattice_api::local::LocalError {
                    kind: if receipt == 0 {
                        lattice_api::local::LocalErrorKind::Busy
                    } else {
                        lattice_api::local::LocalErrorKind::BadRequest
                    },
                    message: "rejected".into(),
                    recovery: None,
                    store: None,
                }),
            },
            2 => LocalUpdate::NotSubmitted {
                turn_id: id,
                stage: lattice_api::local::NotSubmittedStage::FreezingConfiguration,
            },
            _ => LocalUpdate::Submission {
                turn_id: id,
                result: Ok(Acceptance::NeedsConfiguration {
                    agent_name: "unused".into(),
                }),
            },
        };
        app.apply_update(update);
        assert_eq!(app.input.text(), "user is typing", "receipt {receipt}");
        assert_eq!(
            app.queue
                .messages()
                .map(|m| m.text.as_str())
                .collect::<Vec<_>>(),
            [
                " q0\nraw ",
                " q1\nraw ",
                " q2\nraw ",
                " q3\nraw ",
                " q4\nraw "
            ]
        );
        assert!(app.queue.is_full());
        assert_eq!(frame(app).text.matches("暂停").count(), 5);
        assert_eq!(app.auto_submit(), None);
        assert_eq!(app.phase, TurnPhase::Idle);
    }
}

#[test]
fn accepting_and_unknown_receipts_release_capacity_without_requeue() {
    for receipt in 0..4 {
        let mut h = queued_app();
        let app = &mut h.app;
        app.auto_submit().unwrap();
        let id = app.current_turn.unwrap();
        let turn =
            lattice_api::local::TurnView::from(view(id, false).selected_turn.as_ref().unwrap());
        let update = match receipt {
            0 => LocalUpdate::Submission {
                turn_id: id,
                result: Ok(Acceptance::Accepted { turn }),
            },
            1 => LocalUpdate::Submission {
                turn_id: id,
                result: Ok(Acceptance::Existing { turn }),
            },
            2 => LocalUpdate::Submission {
                turn_id: id,
                result: Ok(Acceptance::ConfirmationUnknown {
                    turn_id: id,
                    session_id: Some(sid()),
                }),
            },
            _ => LocalUpdate::SubmissionUnknown {
                turn_id: id,
                error: None,
            },
        };
        app.apply_update(update);
        assert!(
            !app.queue.is_full(),
            "receipt {receipt} releases the reserved slot"
        );
        assert_eq!(app.queue.messages().count(), 4);
        assert_eq!(app.queue.is_paused(), receipt >= 2);
        app.input.set_text("new queued input");
        app.submit();
        assert!(app.input.is_empty());
        assert!(app.queue.is_full());
        assert_eq!(
            app.queue.messages().last().unwrap().text,
            "new queued input"
        );
        if receipt >= 2 {
            // 未知之后的迟到拒绝不能重新入队或覆盖用户的新草稿。
            app.input.set_text("new draft");
            app.apply_update(LocalUpdate::NotSubmitted {
                turn_id: id,
                stage: lattice_api::local::NotSubmittedStage::NeedsConfiguration,
            });
            assert_eq!(app.input.text(), "new draft");
            assert_eq!(app.queue.messages().count(), 5);
        }
    }
}

fn stopped_view(id: TurnId, cancelled: bool, user_node: u64) -> ObservationSnapshot {
    let mut value = serde_json::to_value(view(id, false)).unwrap();
    value["session"]["current"] = json!(user_node);
    value["history"]["head"] = json!(user_node);
    value["selected_turn"]["user_node"] = json!(user_node);
    value["session"]["active_turn"] = Value::Null;
    value["selected_turn"]["ended_at"] = json!("2026-01-01T00:00:01Z");
    value["selected_turn"]["summary"] = json!({
        "outcome":if cancelled { "cancelled" } else { "failed" },
        "notice":{"level":if cancelled { "info" } else { "error" },"text":"stopped"},
    });
    value["selected_turn"]["steps"] = json!([{
        "key":{"turn_id":id,"ordinal":0},"attempts":[],
        "state":{"state":"closed","end":{"end":"stop","node":null,"cause":{"cause":if cancelled { "cancelled" } else { "process_exit" }}},"slots":[]},
    }]);
    value["active"] = Value::Null;
    value["attempt"] = Value::Null;
    value["owned"] = json!(false);
    serde_json::from_value(value).unwrap()
}

#[test]
fn queue_pauses_on_failure_cancel_and_disconnect_and_resumes_after_failed_manual_turn() {
    for cause in 0..5 {
        let mut h = queued_app();
        let app = &mut h.app;
        app.auto_submit().unwrap();
        let id = app.current_turn.unwrap();
        app.apply_update(LocalUpdate::Submission {
            turn_id: id,
            result: Ok(Acceptance::Accepted {
                turn: lattice_api::local::TurnView::from(
                    view(id, false).selected_turn.as_ref().unwrap(),
                ),
            }),
        });
        match cause {
            0 | 1 => app.apply_update(LocalUpdate::State {
                view: Box::new(stopped_view(id, cause == 1, 3)),
                phase: ObservationPhase::NoOwner,
                error: None,
            }),
            2 => app.apply_update(LocalUpdate::Connection(ConnectionStatus::Eof)),
            3 => app.apply_update(LocalUpdate::Connection(ConnectionStatus::Idle)),
            _ => app.apply_update(LocalUpdate::Connection(ConnectionStatus::Error(
                lattice_api::local::LocalError::from(std::io::Error::other("offline")),
            ))),
        }
        assert!(app.queue.is_paused(), "cause {cause}");
        assert_eq!(frame(app).text.matches("暂停").count(), 4);
        assert_eq!(app.auto_submit(), None);
        // 断连后重连得到成功结局也不能自行恢复。
        if cause >= 2 {
            finish_completed(app, id);
            assert_eq!(app.auto_submit(), None);
        }
        app.input.set_text("manual while paused");
        assert_eq!(
            app.apply_action(KeyAction::Submit),
            Some("manual while paused".into())
        );
        assert!(app.queue.is_paused());
        let manual = app.current_turn.unwrap();
        app.apply_update(LocalUpdate::State {
            view: Box::new(stopped_view(manual, false, if cause < 2 { 4 } else { 5 })),
            phase: ObservationPhase::NoOwner,
            error: None,
        });
        assert!(app.input.is_empty());
        assert_eq!(
            app.apply_action(KeyAction::Submit),
            Some(" q1\nraw ".into()),
            "Failed 后空输入 Enter 立即发送"
        );
        assert!(!app.queue.is_paused());
    }
}

#[test]
fn completion_waits_for_editor_then_sends_one_on_save_or_discard() {
    // 先运行自动发出的第一条，再在编辑期间交付它的 Completed。
    for save in [true, false] {
        let mut h = queued_app();
        let app = &mut h.app;
        app.auto_submit().unwrap();
        let first = app.current_turn.unwrap();
        app.apply_update(LocalUpdate::Submission {
            turn_id: first,
            result: Ok(Acceptance::Accepted {
                turn: lattice_api::local::TurnView::from(
                    view(first, false).selected_turn.as_ref().unwrap(),
                ),
            }),
        });
        app.apply_action(KeyAction::Up);
        app.apply_action(KeyAction::Submit);
        app.apply_action(KeyAction::Insert('!'));
        finish_completed(app, first);
        assert_eq!(app.auto_submit(), None);
        assert_eq!(
            app.apply_action(if save {
                KeyAction::Submit
            } else {
                KeyAction::Clear
            }),
            Some(" q1\nraw ".into())
        );
        assert_eq!(
            app.queue.messages().last().unwrap().text,
            if save { " q4\nraw !" } else { " q4\nraw " }
        );
        assert_eq!(app.auto_submit(), None);
        let id = app.current_turn.unwrap();
        app.apply_update(LocalUpdate::Submission {
            turn_id: id,
            result: Ok(Acceptance::Accepted {
                turn: lattice_api::local::TurnView::from(
                    view(id, false).selected_turn.as_ref().unwrap(),
                ),
            }),
        });
        finish_completed(app, id);
        assert_eq!(app.auto_submit(), Some(" q2\nraw ".into()));
        assert_eq!(app.auto_submit(), None);
    }
}

#[test]
fn queue_editor_draws_insertion_cursor_and_newline_hint_after_tiny_resize() {
    let mut h = queued_app();
    let app = &mut h.app;
    app.queue.pause();
    app.apply_action(KeyAction::Up);
    app.apply_action(KeyAction::Submit);
    for size in [(1, 1), (2, 3), (80, 24)] {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(size.0, size.1)).unwrap();
        terminal
            .draw(|frame| crate::view::draw(frame, app))
            .unwrap();
        assert!(app.queue.is_editing());
        assert!(matches!(app.focus, Focus::Transcript(_)));
        if size == (80, 24) {
            let transcript = app.prepare_transcript();
            let (row, col) = transcript.editor_cursor.unwrap();
            let expected = ratatui::layout::Position::new(
                col as u16,
                row.saturating_sub(app.viewport.scroll()) as u16,
            );
            assert_eq!(terminal.get_cursor_position().unwrap(), expected);
            let text = screen_text(terminal.backend().buffer());
            assert!(text.contains("Alt+Enter 换行"), "{text}");
        }
    }
}

#[tokio::test]
async fn every_loop_exit_saves_queue_originals_in_order() {
    for exit_kind in 0..6 {
        let mut h = queued_app();
        h.app.queue.pause();
        std::fs::create_dir_all(&h.options.session_dir).unwrap();
        if exit_kind >= 2 {
            h.app.apply_action(KeyAction::Up);
            h.app.apply_action(KeyAction::Submit);
            h.app.apply_action(KeyAction::Insert('!'));
        }
        let key = |code, modifiers| Ok(Event::Key(KeyEvent::new(code, modifiers)));
        let events = match exit_kind {
            0 => vec![
                Ok(Event::Paste("/quit".into())),
                key(KeyCode::Enter, KeyModifiers::NONE),
            ],
            1 => vec![key(KeyCode::Char('d'), KeyModifiers::CONTROL)],
            2 => vec![
                key(KeyCode::Char('c'), KeyModifiers::CONTROL),
                key(KeyCode::Char('c'), KeyModifiers::CONTROL),
            ],
            3 => vec![],
            4 => vec![Err(std::io::Error::other("input failed"))],
            _ => vec![],
        };
        let events: std::pin::Pin<Box<dyn futures::Stream<Item = std::io::Result<Event>>>> =
            if exit_kind == 5 {
                Box::pin(futures::stream::pending())
            } else {
                Box::pin(futures::stream::iter(events))
            };
        let client = LocalClient::new(lattice_api::StateLayout::in_state_root(h._root.path()));
        let exit = run_loop(&h.options, &mut h.app, &client, None, events, |_| {
            if exit_kind == 5 {
                anyhow::bail!("draw failed");
            }
            Ok(())
        })
        .await;
        assert_eq!(exit.result.is_err(), exit_kind == 5);
        let saved =
            std::fs::read_to_string(h.options.session_dir.join("repl_history.txt")).unwrap();
        let queued = saved
            .lines()
            .filter(|line| line.starts_with(" q"))
            .collect::<Vec<_>>();
        assert_eq!(
            queued,
            [" q0 raw ", " q1 raw ", " q2 raw ", " q3 raw ", " q4 raw "]
        );
    }
}

#[test]
fn detached_automatic_receipt_settles_its_slot_without_touching_new_turn() {
    for receipt in 0..4 {
        let mut h = queued_app();
        let app = &mut h.app;
        app.auto_submit().unwrap();
        let id = app.current_turn.unwrap();
        // 只读观察可以先于迟到的提交回执读到结局。
        finish_completed(app, id);
        app.input.set_text("new manual");
        app.submit().unwrap();
        let manual = app.current_turn;
        app.input.set_text("new draft");
        let submission = match receipt {
            0 => Some(Ok(Acceptance::Accepted {
                turn: lattice_api::local::TurnView::from(
                    view(id, true).selected_turn.as_ref().unwrap(),
                ),
            })),
            1 => Some(Ok(Acceptance::ConfirmationUnknown {
                turn_id: id,
                session_id: Some(sid()),
            })),
            _ => None,
        };
        app.apply_handoff(DetachReport {
            session_id: Some(sid()),
            turn_id: Some(id),
            submission,
            not_submitted: (receipt == 2)
                .then_some(lattice_api::local::NotSubmittedStage::NeedsConfiguration),
            submission_diagnostic: None,
            history_error: None,
            cancel: None,
            cancel_error: None,
            state: None,
        });
        assert_eq!(app.input.text(), "new draft");
        assert_eq!(app.current_turn, manual);
        assert_eq!(app.phase, TurnPhase::Streaming);
        assert_eq!(app.queue.is_full(), receipt == 2);
        assert_eq!(app.queue.is_paused(), receipt != 0);
    }
}

#[test]
fn manual_rejections_keep_the_existing_restore_rules() {
    for receipt in 0..3 {
        let mut h = Harness::new(true);
        let app = &mut h.app;
        app.input.set_text(" manual\nraw ");
        app.submit().unwrap();
        let id = app.current_turn.unwrap();
        app.input.set_text("new draft");
        app.apply_update(match receipt {
            0 => LocalUpdate::Submission {
                turn_id: id,
                result: Err(lattice_api::local::LocalError {
                    kind: lattice_api::local::LocalErrorKind::Busy,
                    message: "busy".into(),
                    recovery: None,
                    store: None,
                }),
            },
            1 => LocalUpdate::NotSubmitted {
                turn_id: id,
                stage: lattice_api::local::NotSubmittedStage::FreezingConfiguration,
            },
            _ => LocalUpdate::Submission {
                turn_id: id,
                result: Ok(Acceptance::NeedsConfiguration {
                    agent_name: "unused".into(),
                }),
            },
        });
        assert_eq!(
            app.input.text(),
            if receipt == 2 {
                "new draft"
            } else {
                " manual\nraw "
            }
        );
        assert_eq!(app.queue.messages().count(), 0);
    }
}

#[test]
fn empty_edit_deletes_and_ctrl_c_never_clears_hidden_input_or_editor() {
    for severity in [DockSeverity::Warn, DockSeverity::Error] {
        let mut h = Harness::new(true);
        let app = &mut h.app;
        app.input.set_text("running");
        app.submit().unwrap();
        let id = app.current_turn.unwrap();
        app.input.set_text("中a");
        app.submit();
        app.apply_action(KeyAction::Up);
        app.apply_action(KeyAction::Submit);
        app.apply_update(LocalUpdate::State {
            view: Box::new(stopped_view(id, false, 1)),
            phase: ObservationPhase::NoOwner,
            error: None,
        });
        app.input.set_text("hidden draft");
        app.push_dock_status(
            StatusKind::TurnFailed {
                reason: "dock".into(),
            },
            severity,
        );
        handle_input_event(
            app,
            Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        );
        assert!(app.last_ctrl_c.is_some());
        assert!(!app.should_quit);
        assert_eq!(app.input.text(), "hidden draft");
        app.apply_action(KeyAction::Insert(' '));
        assert!(app.dock_status().is_some());
        assert_eq!(
            app.queue
                .editor(app.queue.messages().next().unwrap().id)
                .unwrap()
                .text(),
            "中a "
        );
        app.apply_action(KeyAction::Home);
        for _ in 0..3 {
            app.apply_action(KeyAction::Delete);
        }
        app.apply_action(KeyAction::Insert(' '));
        app.apply_action(KeyAction::Submit);
        assert_eq!(
            app.queue.messages().count(),
            0,
            "whitespace-only edit deletes"
        );
        assert_eq!(app.input.text(), "hidden draft");
    }
}

#[test]
fn queue_rendering_and_identity_survive_history_replacement() {
    let mut h = queued_app();
    let app = &mut h.app;
    app.queue.pause();
    app.apply_action(KeyAction::Up);
    let Focus::Transcript(before) = app.focus else {
        panic!("browse");
    };
    let id = app.observed_turn.unwrap();
    finish_completed(app, id);
    app.prepare_transcript();
    let Focus::Transcript(after) = app.focus else {
        panic!("browse");
    };
    assert_eq!(before.block.id, after.block.id);
    let transcript = render_transcript(app, 100);
    let queued_ids = app.queue.messages().map(|m| m.id).collect::<Vec<_>>();
    for (line, owner) in transcript.lines.iter().zip(&transcript.owners) {
        if queued_ids.contains(&owner.id) {
            assert!(!line.to_string().contains('#'), "queue has no turn number");
            for span in &line.spans {
                if span.content.contains("raw") {
                    assert_eq!(
                        span.style,
                        crate::view::role_style(crate::view::Role::DimMeta)
                    );
                }
            }
        }
    }
    assert_eq!(app.queue.messages().count(), 5);
    assert_eq!(app.history.prev(), Some("running"));
}

#[test]
fn multiline_queue_editor_keeps_cursor_visible_and_stops_at_visual_edges() {
    let mut h = queued_app();
    let app = &mut h.app;
    app.queue.pause();
    app.term_width = 12;
    app.term_height = 10;
    app.apply_action(KeyAction::Up);
    app.apply_action(KeyAction::Submit);
    app.apply_action(KeyAction::Paste("\n中abcd\n".repeat(20)));
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(12, 10)).unwrap();
    for action in [
        KeyAction::Home,
        KeyAction::Right,
        KeyAction::Left,
        KeyAction::End,
    ] {
        app.apply_action(action);
        terminal
            .draw(|frame| crate::view::draw(frame, app))
            .unwrap();
        let transcript = app.prepare_transcript();
        let (row, _) = transcript.editor_cursor.unwrap();
        assert!(row >= app.viewport.scroll());
        assert!(row < app.viewport.scroll() + app.transcript_height());
        assert!(app.queue.is_editing());
    }
    for _ in 0..100 {
        app.apply_action(KeyAction::Up);
    }
    assert_eq!(
        app.queue
            .editor(app.queue.messages().last().unwrap().id)
            .unwrap()
            .cursor
            .0,
        0
    );
    for _ in 0..100 {
        app.apply_action(KeyAction::Down);
    }
    assert!(app.queue.is_editing());
    let editor = app
        .queue
        .editor(app.queue.messages().last().unwrap().id)
        .unwrap();
    assert_eq!(editor.cursor.0, editor.lines.len() - 1);
    app.apply_action(KeyAction::Clear);
    assert_eq!(app.queue.messages().last().unwrap().text, " q4\nraw ");
}

#[test]
fn completed_summary_dispatch_does_not_wait_for_final_history() {
    let mut h = queued_app();
    let app = &mut h.app;
    let closed = app.state.as_ref().unwrap().view.clone();
    // R2 结局在 State 中先到；Finished 要等正文读取，不能作为发送许可。
    app.apply_update(LocalUpdate::State {
        view: closed,
        phase: ObservationPhase::NoOwner,
        error: None,
    });
    assert_eq!(app.auto_submit(), Some(" q0\nraw ".into()));
}

enum Reply {
    Now(Value),
    Hold(oneshot::Receiver<Value>),
    Close(oneshot::Receiver<()>),
    Stream(UnboundedReceiver<Value>),
    Never,
}
struct Peer {
    _root: tempfile::TempDir,
    client: LocalClient,
    seen: watch::Receiver<Vec<Value>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Peer {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Peer {
    fn start(mut handler: impl FnMut(&Value) -> Reply + Send + 'static) -> Self {
        let root = tempfile::tempdir().unwrap();
        let layout = lattice_api::StateLayout::in_state_root(root.path());
        let local = layout.lattice_root().join("local");
        std::fs::create_dir_all(&local).unwrap();
        std::fs::set_permissions(&local, std::fs::Permissions::from_mode(0o700)).unwrap();
        let socket = local.join("api.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
        let (record, seen) = watch::channel(Vec::new());
        let task = tokio::spawn(async move {
            let mut tasks = tokio::task::JoinSet::new();
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut magic = [0; 8];
                stream.read_exact(&mut magic).await.unwrap();
                assert_eq!(&magic, b"LTLOCAL3");
                let len = stream.read_u32().await.unwrap();
                assert!(len <= 16 * 1024 * 1024);
                let mut bytes = vec![0; len as usize];
                stream.read_exact(&mut bytes).await.unwrap();
                let request: Value = serde_json::from_slice(&bytes).unwrap();
                let reply = handler(&request);
                record.send_modify(|seen| seen.push(request));
                tasks.spawn(async move {
                    let mut seq = 0;
                    match reply {
                        Reply::Now(value) => send(&mut stream, &value, &mut seq).await,
                        Reply::Hold(receiver) => {
                            if let Ok(value) = receiver.await {
                                send(&mut stream, &value, &mut seq).await;
                            }
                        }
                        Reply::Close(receiver) => {
                            let _ = receiver.await;
                            return;
                        }
                        Reply::Stream(mut receiver) => {
                            while let Some(value) = receiver.next().await {
                                send(&mut stream, &value, &mut seq).await;
                            }
                        }
                        Reply::Never => {}
                    }
                    let _stream = stream;
                    std::future::pending::<()>().await;
                });
            }
        });
        Self {
            _root: root,
            client: LocalClient::new(layout),
            seen,
            task,
        }
    }
    async fn wait(&self, predicate: impl FnMut(&Vec<Value>) -> bool) -> Vec<Value> {
        tokio::time::timeout(
            Duration::from_secs(5),
            self.seen.clone().wait_for(predicate),
        )
        .await
        .unwrap()
        .unwrap()
        .clone()
    }
}
async fn send(stream: &mut UnixStream, value: &Value, seq: &mut u64) {
    let bytes = serde_json::to_vec(&json!({"message_id":*seq,"offset":0,"json_fragment":serde_json::to_string(value).unwrap(),"complete":true})).unwrap();
    *seq += 1;
    if stream.write_all(b"LTLOCAL3").await.is_err() {
        return;
    }
    let _ = stream.write_u32(bytes.len() as u32).await;
    let _ = stream.write_all(&bytes).await;
}
fn count(seen: &[Value], op: &str) -> usize {
    seen.iter().filter(|r| r["operation"] == op).count()
}
fn cancels(seen: &[Value]) -> Vec<Value> {
    seen.iter()
        .filter(|r| r["operation"] == "cancel")
        .map(|r| r["target"]["turn_id"].clone())
        .collect()
}
fn submitted(seen: &[Value]) -> TurnId {
    serde_json::from_value(
        seen.iter().find(|r| r["operation"] == "submit").unwrap()["request"]["turn_id"].clone(),
    )
    .unwrap()
}

#[derive(Clone)]
struct Frame {
    phase: TurnPhase,
    current: Option<TurnId>,
    connected: bool,
    input: String,
    text: String,
    cancel: Option<CancelStatus>,
    finished: bool,
    /// 当前 Turn 已被后台接受或正在运行：助手标题线已出现。
    started: bool,
}
fn frame(app: &App) -> Frame {
    Frame {
        phase: app.phase,
        current: app.current_turn,
        connected: app.connected,
        input: app.input.text(),
        text: crate::view::transcript_lines(app, 200)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
        cancel: app.cancel_status,
        finished: app
            .state
            .as_ref()
            .is_some_and(|s| s.phase == ObservationPhase::Finished),
        started: app.blocks.iter().any(|block| {
            matches!(block, Block::AssistantStart { turn_id, .. } if Some(*turn_id) == app.current_turn)
        }),
    }
}
struct Harness {
    options: TuiOptions,
    app: App,
    events: UnboundedReceiver<std::io::Result<Event>>,
    observer: watch::Sender<Frame>,
    remote: Remote,
    _root: tempfile::TempDir,
}
#[derive(Clone)]
struct Remote {
    keys: UnboundedSender<std::io::Result<Event>>,
    frames: watch::Receiver<Frame>,
}
impl Harness {
    fn new(new_session: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let options = TuiOptions {
            session_id: (!new_session).then(|| SESSION.into()),
            agent: "unused".into(),
            session_dir: root.path().join("history"),
            agents_dir: root.path().join("agents"),
            project_root: root.path().into(),
        };
        let app = App::new(
            SessionContext {
                session_id: (!new_session).then(sid),
                active_agent: "unused".into(),
            },
            false,
            &options.session_dir,
        );
        let (keys, events) = unbounded();
        let (observer, frames) = watch::channel(frame(&app));
        Self {
            options,
            app,
            events,
            observer,
            remote: Remote { keys, frames },
            _root: root,
        }
    }
    async fn run(&mut self, client: &LocalClient, turn: Option<LocalTurn>) -> LoopExit {
        let observer = self.observer.clone();
        run_loop(
            &self.options,
            &mut self.app,
            client,
            turn,
            &mut self.events,
            |app| {
                observer.send_replace(frame(app));
                Ok(())
            },
        )
        .await
    }
}
impl Remote {
    fn press(&self, code: KeyCode, modifiers: KeyModifiers) {
        self.keys
            .unbounded_send(Ok(Event::Key(KeyEvent::new(code, modifiers))))
            .unwrap();
    }
    fn ctrl(&self, c: char) {
        self.press(KeyCode::Char(c), KeyModifiers::CONTROL);
    }
    fn line(&self, text: &str) {
        for c in text.chars() {
            self.press(KeyCode::Char(c), KeyModifiers::NONE);
        }
        self.press(KeyCode::Enter, KeyModifiers::NONE);
    }
    async fn wait(&self, predicate: impl FnMut(&Frame) -> bool) -> Frame {
        tokio::time::timeout(
            Duration::from_secs(5),
            self.frames.clone().wait_for(predicate),
        )
        .await
        .unwrap()
        .unwrap()
        .clone()
    }
}

#[tokio::test]
async fn live_observation_waits_for_user_history_before_drawing_header() {
    let id = lattice_api::local::new_turn_id();
    let live = view(id, false);
    let (history_tx, history_rx) = oneshot::channel();
    let mut history_rx = Some(history_rx);
    let response_history = history(&live);
    let peer = Peer::start(move |r| match r["operation"].as_str().unwrap() {
        "subscribe" => Reply::Now(snapshot(&live)),
        "history" => Reply::Hold(history_rx.take().unwrap()),
        _ => panic!("read only"),
    });
    let mut h = Harness::new(false);
    let remote = h.remote.clone();
    let driver = async {
        let before = remote.wait(|f| f.phase == TurnPhase::Streaming).await;
        assert!(!before.text.contains("── unused ─"));
        peer.wait(|s| count(s, "history") == 1).await;
        history_tx.send(response_history).unwrap();
        let after = remote.wait(|f| f.text.contains("original prompt")).await;
        assert_eq!(
            after.text.matches("── unused ─").count(),
            1,
            "{}",
            after.text
        );
        assert_eq!(after.text.matches(" #1 ──").count(), 2);
        assert!(!after.text.contains("live body"));
        remote.line("/quit");
    };
    let (exit, ()) = tokio::join!(
        h.run(
            &peer.client,
            Some(peer.client.observe(Some(sid()), Some(id)))
        ),
        driver
    );
    exit.result.unwrap();
}

#[test]
fn unaccepted_submissions_never_start_an_assistant_segment() {
    use lattice_api::local::{LocalError, LocalErrorKind, NotSubmittedStage};
    for kind in [
        Some(LocalErrorKind::Busy),
        Some(LocalErrorKind::BadRequest),
        None,
    ] {
        let mut h = Harness::new(true);
        h.app.input.set_text("prompt");
        h.app.apply_action(KeyAction::Submit).unwrap();
        let turn_id = h.app.current_turn.unwrap();
        let update = match kind {
            Some(kind) => LocalUpdate::Submission {
                turn_id,
                result: Err(LocalError {
                    kind,
                    message: "not accepted".into(),
                    recovery: None,
                    store: None,
                }),
            },
            None => LocalUpdate::NotSubmitted {
                turn_id,
                stage: NotSubmittedStage::FreezingConfiguration,
            },
        };
        h.app.apply_update(update);
        assert_eq!(h.app.input.text(), "prompt");
        assert!(!frame(&h.app).text.contains("── unused ─"));
        assert!(
            !h.app
                .blocks
                .iter()
                .any(|block| matches!(block, Block::AssistantStart { .. }))
        );
    }
}

#[test]
fn comment_rewind_during_queue_edit_preserves_remaining_order() {
    let mut h = comment_app();
    let app = &mut h.app;
    let target = app.observed_turn.unwrap();
    app.input.set_text("running");
    app.submit().unwrap();
    save_comment(app, "drop this");
    app.focus = Focus::Dock;
    app.submit();
    for text in ["middle", "last"] {
        app.input.set_text(text);
        app.submit();
    }
    app.apply_update(LocalUpdate::State {
        view: Box::new(stopped_view(app.current_turn.unwrap(), false, 3)),
        phase: ObservationPhase::Finished,
        error: None,
    });
    comment_browse(app, "middle");
    app.apply_action(KeyAction::Submit);
    assert!(app.queue.is_editing());
    let mut snapshot = view(target, true);
    snapshot.session.current = lattice_api::NodeId(1);
    snapshot.history.head = lattice_api::NodeId(1);
    app.apply_update(LocalUpdate::State {
        view: Box::new(snapshot.clone()),
        phase: ObservationPhase::Finished,
        error: None,
    });
    app.apply_update(LocalUpdate::History {
        anchor: snapshot.history.clone(),
        generation: app.generation,
        stale: false,
        result: Ok(Box::new(
            serde_json::from_value(
                json!({"anchor":snapshot.history,"entries":history_entries(&snapshot)}),
            )
            .unwrap(),
        )),
    });
    app.apply_action(KeyAction::Clear);
    assert_eq!(
        app.queue
            .messages()
            .map(|m| m.text.as_str())
            .collect::<Vec<_>>(),
        ["middle", "last"]
    );
}

#[test]
fn off_path_summary_is_independent_and_history_assigns_observed_ordinals() {
    let observed = lattice_api::local::new_turn_id();
    let other = lattice_api::local::new_turn_id();
    let mut h = Harness::new(false);
    let closed = view(observed, true);
    h.app.apply_state(LocalState {
        view: Box::new(closed.clone()),
        phase: ObservationPhase::Finished,
        error: None,
    });
    assert!(!frame(&h.app).text.contains("── unused ─"));
    let other_history = serde_json::from_value(
        json!({"anchor":view(other,false).history,"entries":history_entries(&view(other,false))}),
    )
    .unwrap();
    h.app
        .replace_history(crate::session::history_blocks(other_history));
    let lines = crate::view::transcript_lines(&h.app, 200);
    assert!(
        !frame(&h.app).text.contains("── unused ─"),
        "off-path outcome must not start another Turn's assistant"
    );
    let summary = lines
        .iter()
        .find(|line| line.to_string().contains("completed"))
        .unwrap();
    assert!(
        summary
            .spans
            .iter()
            .skip(1)
            .all(|span| span.style == crate::view::role_style(crate::view::Role::DimMeta))
    );
    let loaded =
        serde_json::from_value(json!({"anchor":closed.history,"entries":history_entries(&closed)}))
            .unwrap();
    h.app
        .replace_history(crate::session::history_blocks(loaded));
    assert_eq!(frame(&h.app).text.matches("── unused ─").count(), 1);
}

#[test]
fn history_and_local_submission_use_user_positions_not_node_numbers() {
    let first = lattice_api::local::new_turn_id();
    let second = lattice_api::local::new_turn_id();
    let mut entries = history_entries(&view(first, true))
        .as_array()
        .unwrap()
        .clone();
    entries.push(json!({"node":7,"agent_name":"second-agent","content":{"kind":"user_message","parent":2,"turn_id":second,"message":{"role":"user","parts":[{"semantic":{"text":{"text":"second prompt"}}}]}}}));
    let history =
        serde_json::from_value(json!({"anchor":{"session_id":sid(),"head":7},"entries":entries}))
            .unwrap();
    let blocks = crate::session::history_blocks(history);
    let ordinals: Vec<_> = blocks
        .iter()
        .filter_map(|block| match block {
            Block::UserTurn(id, ordinal) => Some((*id, ordinal.get())),
            _ => None,
        })
        .collect();
    assert_eq!(ordinals, [(first, 1), (second, 2)]);
    let mut h = Harness::new(false);
    h.app.replace_history(blocks);
    assert_eq!(
        (
            h.app.model.as_ref(),
            h.app.provider.as_ref(),
            h.app.context_tokens
        ),
        (None, None, None)
    );
    h.app.input.set_text("third prompt");
    assert_eq!(
        h.app.apply_action(KeyAction::Submit).as_deref(),
        Some("third prompt")
    );
    let text = frame(&h.app).text;
    let user_headers: Vec<_> = text
        .lines()
        .filter(|line| line.contains("── USER ─"))
        .collect();
    assert_eq!(user_headers.len(), 3, "{text}");
    for (line, suffix) in user_headers.iter().zip([" #1 ──", " #2 ──", " #3 ──"]) {
        assert!(line.ends_with(suffix), "{line}");
    }
    assert!(
        text.lines()
            .any(|line| line.contains("── unused ─") && line.ends_with(" #1 ──"))
    );
    assert!(!text.contains("── second-agent ─"), "no reply yet");
}

#[tokio::test]
async fn accepted_and_existing_start_one_assistant_header_before_body_and_history() {
    for acceptance in ["accepted", "existing"] {
        let (submit_tx, submit_rx) = oneshot::channel();
        let mut submit_rx = Some(submit_rx);
        let (stream_tx, stream_rx) = unbounded();
        let mut stream_rx = Some(stream_rx);
        let (history_tx, history_rx) = oneshot::channel();
        let mut history_rx = Some(history_rx);
        let mut subscriptions = 0;
        let peer = Peer::start(move |r| match r["operation"].as_str().unwrap() {
            "submit" => Reply::Hold(submit_rx.take().unwrap()),
            "subscribe" => {
                subscriptions += 1;
                if subscriptions == 1 {
                    Reply::Never
                } else {
                    Reply::Stream(stream_rx.take().unwrap())
                }
            }
            "history" => history_rx.take().map_or(Reply::Never, Reply::Hold),
            _ => Reply::Never,
        });
        let mut h = Harness::new(true);
        let remote = h.remote.clone();
        let driver = async {
            remote.line("original prompt");
            let id = submitted(&peer.wait(|s| count(s, "submit") == 1).await);
            let before = remote.wait(|f| f.text.contains("original prompt")).await;
            assert!(
                before
                    .text
                    .lines()
                    .any(|l| l.contains("── USER ─") && l.ends_with(" #1 ──"))
            );
            assert!(!before.text.contains("── accepted-agent ─"));
            let mut live = view(id, false);
            live.selected_turn.as_mut().unwrap().agent_name = "accepted-agent".into();
            peer.wait(|s| count(s, "subscribe") == 1).await;
            submit_tx.send(response("acceptance", json!({"acceptance":acceptance,"turn":lattice_api::local::TurnView::from(live.selected_turn.as_ref().unwrap())}))).unwrap();
            let accepted = remote
                .wait(|f| {
                    f.started && (acceptance == "accepted" || f.text.contains("找回既有请求"))
                })
                .await;
            // 接受只由助手标题线表达，不另写接受行；找回照常显示。
            assert!(!accepted.text.contains("已确认接受"), "{}", accepted.text);
            // TurnId 不单独显示。
            assert!(!accepted.text.contains("提交身份"), "{}", accepted.text);
            assert_eq!(
                accepted.text.matches("── accepted-agent ─").count(),
                1,
                "{}",
                accepted.text
            );
            assert!(!accepted.text.contains("live body"));
            stream_tx.unbounded_send(snapshot(&live)).unwrap();
            peer.wait(|s| count(s, "history") == 1).await;
            let mut loaded = history(&live);
            loaded["value"]["fragments"][0]["agent_name"] = json!("accepted-agent");
            history_tx.send(loaded).unwrap();
            // A token after the loaded history also must not create a second header.
            stream_tx
                .unbounded_send(event(
                    1,
                    TurnEvent::TokenDelta {
                        key: key(id, 0, 1),
                        content: "live body".into(),
                    },
                ))
                .unwrap();
            let frame = remote.wait(|f| f.text.contains("live body")).await;
            assert_eq!(
                frame.text.matches("── accepted-agent ─").count(),
                1,
                "{}",
                frame.text
            );
            let mut done = view(id, true);
            done.selected_turn.as_mut().unwrap().agent_name = "accepted-agent".into();
            done.cursor.sequence = 1;
            stream_tx.unbounded_send(snapshot(&done)).unwrap();
            let mut loaded = finished(&done);
            for entry in loaded["value"]["history"]["entries"]
                .as_array_mut()
                .unwrap()
            {
                entry["agent_name"] = json!("accepted-agent");
            }
            stream_tx.unbounded_send(loaded).unwrap();
            let final_frame = remote.wait(|f| f.finished).await;
            assert_eq!(
                final_frame.text.matches("── accepted-agent ─").count(),
                1,
                "{}",
                final_frame.text
            );
            remote.line("/quit");
        };
        let (exit, ()) = tokio::join!(h.run(&peer.client, None), driver);
        exit.result.unwrap();
        assert_eq!(frame(&h.app).text.matches("── accepted-agent ─").count(), 1);
        assert!(
            !h.app
                .blocks
                .iter()
                .any(|block| matches!(block, Block::AssistantStart { .. })),
            "loaded Step owns the assistant segment"
        );
    }
}

#[tokio::test]
async fn existing_session_waits_for_first_history_but_builtin_commands_still_work() {
    let id = lattice_api::local::new_turn_id();
    let closed = view(id, true);
    let (history_tx, history_rx) = oneshot::channel();
    let mut history_rx = Some(history_rx);
    let response_history = history(&closed);
    let peer = Peer::start(move |r| match r["operation"].as_str().unwrap() {
        "subscribe" => Reply::Now(snapshot(&closed)),
        "history" => Reply::Hold(history_rx.take().unwrap()),
        _ => Reply::Never,
    });
    let mut h = Harness::new(false);
    let remote = h.remote.clone();
    let driver = async {
        peer.wait(|s| count(s, "history") == 1).await;
        remote.line("kept prompt");
        // /help is a barrier after Enter: no sleep-based absence assertion.
        remote.press(KeyCode::Esc, KeyModifiers::NONE);
        remote.line("/help");
        remote.wait(|f| f.text.contains("/reconnect")).await;
        assert_eq!(count(&peer.seen.borrow(), "submit"), 0);
        history_tx.send(response_history).unwrap();
        remote.wait(|f| f.text.contains("live body")).await;
        remote.line("after history");
        peer.wait(|s| count(s, "submit") == 1).await;
        remote.line("/quit");
    };
    let (exit, ()) = tokio::join!(
        h.run(
            &peer.client,
            Some(peer.client.observe(Some(sid()), Some(id)))
        ),
        driver
    );
    exit.result.unwrap();
}

#[tokio::test]
async fn first_submission_obtains_session_only_after_acceptance_and_quit_does_not_cancel() {
    let (tx, rx) = oneshot::channel();
    let mut rx = Some(rx);
    let peer = Peer::start(move |r| match r["operation"].as_str().unwrap() {
        "submit" => {
            assert!(r["request"]["session_id"].is_null());
            assert_eq!(r["request"]["agent"], "unused");
            Reply::Hold(rx.take().unwrap())
        }
        _ => Reply::Never,
    });
    let mut h = Harness::new(true);
    let remote = h.remote.clone();
    let driver = async {
        remote.line(" original ");
        let seen = peer.wait(|s| count(s, "submit") == 1).await;
        let id = submitted(&seen);
        assert_eq!(
            seen.iter().find(|r| r["operation"] == "submit").unwrap()["request"]["input"],
            " original "
        );
        tx.send(accepted(id)).unwrap();
        remote.wait(|f| f.started).await;
        remote.ctrl('d');
    };
    let ((), exit) = tokio::join!(driver, h.run(&peer.client, None));
    assert!(exit.result.is_ok());
    assert_eq!(h.app.ctx.session_id, Some(sid()));
    let report = tokio::time::timeout(Duration::from_secs(1), exit.turn.unwrap().detach())
        .await
        .unwrap();
    assert_eq!(report.cancel, None);
    assert!(cancels(&peer.seen.borrow()).is_empty());
}

#[tokio::test]
async fn cancel_receipt_orders_and_late_probe_keep_original_intent() {
    for order in [0, 1, 2] {
        let mut subscriptions = 0;
        let (stx, srx) = oneshot::channel();
        let mut srx = Some(srx);
        let (ctx, crx) = oneshot::channel();
        let mut crx = Some(crx);
        let peer = Peer::start(move |r| match r["operation"].as_str().unwrap() {
            "submit" => Reply::Hold(srx.take().unwrap()),
            "subscribe" => {
                subscriptions += 1;
                let id = serde_json::from_value(r["target"]["turn_id"].clone()).unwrap();
                let mut fixed = view(id, false);
                fixed.owned = order != 0 || subscriptions > 1;
                Reply::Now(snapshot(&fixed))
            }
            "history" => Reply::Never,
            "cancel" => match crx.take() {
                Some(rx) => Reply::Hold(rx),
                None => Reply::Now(response("cancelled", json!({"owned":true}))),
            },
            _ => panic!("no configuration reload"),
        });
        let mut h = Harness::new(false);
        h.app.replace_history(Vec::new()); // 已装入的空路径；本测试从提交窗口开始。
        let remote = h.remote.clone();
        let driver = async {
            remote.line("hi");
            let id = submitted(&peer.wait(|s| count(s, "submit") == 1).await);
            remote.ctrl('c');
            peer.wait(|s| count(s, "cancel") == 1).await;
            if order == 1 {
                stx.send(accepted(id)).unwrap();
                remote.wait(|f| f.started).await;
                ctx.send(response("cancelled", json!({"owned":false})))
                    .unwrap();
            } else {
                ctx.send(response("cancelled", json!({"owned":order==2})))
                    .unwrap();
                remote
                    .wait(|f| {
                        f.cancel
                            == Some(if order == 2 {
                                CancelStatus::Taken
                            } else {
                                CancelStatus::NotTaken
                            })
                    })
                    .await;
                stx.send(if order == 2 {
                    response(
                        "acceptance",
                        json!({"acceptance":"needs_configuration","agent_name":"unused"}),
                    )
                } else {
                    accepted(id)
                })
                .unwrap();
            }
            remote
                .wait(|f| {
                    f.cancel == Some(CancelStatus::Taken)
                        && (order != 2 || f.text.contains("已停止后续配置"))
                })
                .await;
            remote.ctrl('c');
            id
        };
        let (id, exit) = tokio::join!(driver, h.run(&peer.client, None));
        assert!(exit.result.is_ok());
        let report = tokio::time::timeout(Duration::from_secs(1), exit.turn.unwrap().detach())
            .await
            .unwrap();
        assert_eq!(report.cancel, Some(CancelStatus::Taken));
        assert_eq!(report.turn_id, Some(id));
        assert_eq!(count(&peer.seen.borrow(), "submit"), 1);
        assert_eq!(
            cancels(&peer.seen.borrow()),
            vec![json!(id); if order == 2 { 1 } else { 2 }]
        );
    }
}

#[tokio::test]
async fn exit_before_receipt_settles_cancel_and_configuration_abandonment() {
    for receipt in ["accepted", "rejected", "needs_configuration", "never"] {
        let (tx, rx) = oneshot::channel();
        let mut rx = Some(rx);
        let mut count_cancel = 0;
        let peer = Peer::start(move |r| match r["operation"].as_str().unwrap() {
            "submit" => Reply::Hold(rx.take().unwrap()),
            "cancel" => {
                count_cancel += 1;
                Reply::Now(response("cancelled", json!({"owned":count_cancel>1})))
            }
            _ => Reply::Never,
        });
        let mut h = Harness::new(false);
        h.app.replace_history(Vec::new()); // 已装入的空路径；本测试从提交窗口开始。
        let remote = h.remote.clone();
        let driver = async {
            remote.line("hi");
            let id = submitted(&peer.wait(|s| count(s, "submit") == 1).await);
            remote.ctrl('c');
            peer.wait(|s| count(s, "cancel") == 1).await;
            remote.ctrl('c');
            id
        };
        let (id, exit) = tokio::join!(driver, h.run(&peer.client, None));
        assert!(exit.result.is_ok());
        if receipt != "never" {
            tx.send(match receipt {
                "accepted" => accepted(id),
                "rejected" => error("rejected"),
                _ => response(
                    "acceptance",
                    json!({"acceptance":"needs_configuration","agent_name":"unused"}),
                ),
            })
            .unwrap();
        }
        let started = Instant::now();
        let report = tokio::time::timeout(Duration::from_secs(8), exit.turn.unwrap().detach())
            .await
            .unwrap();
        assert_eq!(report.turn_id, Some(id));
        if receipt == "never" {
            assert!(started.elapsed() >= Duration::from_secs(4));
            assert!(matches!(
                report.cancel,
                Some(CancelStatus::NotTaken | CancelStatus::Unknown)
            ));
        }
        if receipt == "accepted" {
            assert_eq!(report.cancel, Some(CancelStatus::Taken));
        }
        if receipt == "needs_configuration" {
            assert!(report.not_submitted.is_some());
        }
        assert_eq!(count(&peer.seen.borrow(), "submit"), 1);
        assert_eq!(
            cancels(&peer.seen.borrow()),
            vec![json!(id); if receipt == "accepted" { 2 } else { 1 }]
        );
    }
}

#[tokio::test]
async fn late_transport_error_keeps_observation_after_submission_grace() {
    let (close, rx) = oneshot::channel();
    let mut rx = Some(rx);
    let (events, incoming) = unbounded();
    let mut incoming = Some(incoming);
    let mut subscriptions = 0;
    let peer = Peer::start(move |r| match r["operation"].as_str().unwrap() {
        "submit" => Reply::Close(rx.take().unwrap()),
        "subscribe" => {
            subscriptions += 1;
            if subscriptions == 1 {
                let id = serde_json::from_value(r["target"]["turn_id"].clone()).unwrap();
                Reply::Now(snapshot(&view(id, false)))
            } else {
                Reply::Stream(incoming.take().unwrap())
            }
        }
        "history" => Reply::Never,
        "cancel" => Reply::Now(response("cancelled", json!({"owned":true}))),
        _ => panic!("unexpected request"),
    });
    let mut h = Harness::new(false);
    h.app.replace_history(Vec::new()); // 已装入的空路径；本测试从提交窗口开始。
    let remote = h.remote.clone();
    let driver = async {
        remote.line("hi");
        let id = submitted(
            &peer
                .wait(|s| count(s, "submit") == 1 && count(s, "subscribe") == 1)
                .await,
        );
        remote.ctrl('c');
        tokio::time::sleep(Duration::from_secs(4)).await;
        remote.wait(|f| f.text.contains("提交结果未知")).await;
        peer.wait(|s| count(s, "subscribe") == 2).await;
        events.unbounded_send(snapshot(&view(id, false))).unwrap();
        events
            .unbounded_send(event(
                1,
                TurnEvent::TokenDelta {
                    key: key(id, 0, 1),
                    content: "before receipt failure".into(),
                },
            ))
            .unwrap();
        remote
            .wait(|f| f.text.contains("before receipt failure"))
            .await;
        close.send(()).unwrap();
        let frame = remote
            .wait(|f| f.text.contains("local connection failed"))
            .await;
        assert_eq!(frame.current, Some(id));
        assert!(frame.input.is_empty());
        assert!(frame.connected);
        events
            .unbounded_send(event(
                2,
                TurnEvent::TokenDelta {
                    key: key(id, 0, 1),
                    content: " after receipt failure".into(),
                },
            ))
            .unwrap();
        remote
            .wait(|f| f.text.contains("after receipt failure"))
            .await;
        events.unbounded_send(snapshot(&view(id, true))).unwrap();
        events.unbounded_send(finished(&view(id, true))).unwrap();
        remote
            .wait(|f| f.phase == TurnPhase::Idle && f.text.contains("completed"))
            .await;
        remote.ctrl('d');
        id
    };
    let (id, exit) = tokio::join!(driver, h.run(&peer.client, None));
    assert!(exit.result.is_ok());
    let report = exit.turn.unwrap().detach().await;
    assert_eq!(report.turn_id, Some(id));
    assert!(report.submission_diagnostic.is_some());
    assert_eq!(report.cancel, Some(CancelStatus::Taken));
}

#[tokio::test]
async fn idle_deadline_survives_repeated_poll_rebuilds() {
    let id = lattice_api::local::new_turn_id();
    let peer = Peer::start(move |r| match r["operation"].as_str().unwrap() {
        "subscribe" => Reply::Now(snapshot(&view(id, false))),
        "history" => Reply::Now(history(&view(id, false))),
        _ => panic!("read only"),
    });
    let mut h = Harness::new(false);
    let remote = h.remote.clone();
    let initial = peer.client.observe(Some(sid()), Some(id));
    let driver = async {
        remote.wait(|f| f.phase == TurnPhase::Streaming).await;
        peer.wait(|s| count(s, "history") == 1).await;
        tokio::time::pause();
        for _ in 0..lattice_api::STREAM_IDLE_TIMEOUT_SECS - 1 {
            tokio::time::advance(Duration::from_secs(1)).await;
            for _ in 0..8 {
                tokio::task::yield_now().await;
            }
            assert!(remote.frames.borrow().connected);
        }
        tokio::time::advance(Duration::from_secs(1)).await;
        let frame = remote.wait(|f| !f.connected).await;
        assert_eq!(frame.current, Some(id));
        assert_eq!(frame.phase, TurnPhase::Streaming);
        assert!(frame.text.contains("无增量"));
        tokio::time::resume();
        remote.ctrl('d');
    };
    let ((), exit) = tokio::join!(driver, h.run(&peer.client, Some(initial)));
    assert!(exit.result.is_ok());
    assert_eq!(exit.turn.unwrap().detach().await.cancel, None);
}

#[tokio::test]
async fn switching_targets_keeps_old_receipts_and_diagnostics_out_of_new_state() {
    // Esc 只清空输入，取消与选择活动目标由首次 Ctrl+C 或 /reconnect 触发。
    for gesture in [1, 2] {
        let old = lattice_api::local::new_turn_id();
        let active = lattice_api::local::new_turn_id();
        let (old_tx, old_rx) = unbounded();
        let mut old_rx = Some(old_rx);
        let (cancel_tx, cancel_rx) = oneshot::channel();
        let mut cancel_rx = Some(cancel_rx);
        let peer = Peer::start(move |r| match r["operation"].as_str().unwrap() {
            "query" => Reply::Now(response("view", json!(view(old, false)))),
            "subscribe" if r["target"]["turn_id"] == json!(old) => {
                Reply::Stream(old_rx.take().unwrap())
            }
            "subscribe" => {
                assert_eq!(r["target"]["turn_id"], json!(active));
                Reply::Now(snapshot(&view(active, false)))
            }
            "cancel" if r["target"]["turn_id"] == json!(old) => {
                Reply::Hold(cancel_rx.take().unwrap())
            }
            "cancel" => {
                assert_eq!(r["target"]["turn_id"], json!(active));
                Reply::Now(response("cancelled", json!({"owned":true})))
            }
            "history" => Reply::Never,
            _ => panic!("read only target selection"),
        });
        let mut h = Harness::new(false);
        let remote = h.remote.clone();
        let driver = async {
            remote.line("/reconnect");
            peer.wait(|s| count(s, "subscribe") == 1).await;
            old_tx.unbounded_send(snapshot(&view(old, false))).unwrap();
            remote.wait(|f| f.current == Some(old)).await;
            remote.ctrl('c');
            peer.wait(|s| count(s, "cancel") == 1).await;
            let mut closed = view(old, true);
            closed.session.active_turn = Some(active);
            old_tx.unbounded_send(snapshot(&closed)).unwrap();
            remote.wait(|f| f.current == Some(active)).await;
            match gesture {
                1 => {
                    // 第二次是对 B 的新取消，不是两秒窗口内的退出。
                    tokio::time::sleep(CTRL_C_WINDOW + Duration::from_millis(50)).await;
                    remote.ctrl('c');
                }
                _ => remote.line("/reconnect"),
            };
            remote
                .wait(|f| {
                    f.current == Some(active)
                        && if gesture == 2 {
                            f.phase == TurnPhase::Streaming
                        } else {
                            f.cancel == Some(CancelStatus::Taken)
                        }
                })
                .await;
            cancel_tx
                .send(if gesture == 2 {
                    response("cancelled", json!({"owned":true}))
                } else {
                    error("A cancellation denied")
                })
                .unwrap();
            let f = remote
                .wait(|f| {
                    f.text.contains(if gesture == 2 {
                        "历史交接：后台已接管取消"
                    } else {
                        "A cancellation denied"
                    })
                })
                .await;
            assert_eq!(f.current, Some(active));
            assert_eq!(
                f.cancel,
                if gesture == 2 {
                    None
                } else {
                    Some(CancelStatus::Taken)
                }
            );
            // 旧 Turn 的结局已实时显示，交接报告不再重述。
            assert!(f.text.contains(&old.to_string()), "{}", f.text);
            assert!(!f.text.contains("历史交接：Turn"), "{}", f.text);
            remote.ctrl('d');
        };
        let ((), exit) = tokio::join!(driver, h.run(&peer.client, None));
        assert!(exit.result.is_ok());
        assert!(exit.handoffs.is_empty());
        let report = exit.turn.unwrap().detach().await;
        assert_eq!(report.turn_id, Some(active));
        assert!(report.cancel_error.is_none());
        assert_eq!(
            cancels(&peer.seen.borrow()),
            if gesture == 2 {
                vec![json!(old)]
            } else {
                vec![json!(old), json!(active)]
            }
        );
        assert_eq!(count(&peer.seen.borrow(), "submit"), 0);
    }
}

#[tokio::test]
async fn detached_cancel_keeps_original_target_diagnostic() {
    let id = lattice_api::local::new_turn_id();
    let (tx, rx) = oneshot::channel();
    let mut rx = Some(rx);
    let peer = Peer::start(move |r| match r["operation"].as_str().unwrap() {
        "subscribe" => Reply::Now(snapshot(&view(id, false))),
        "history" => Reply::Now(history(&view(id, false))),
        "cancel" => match rx.take() {
            Some(rx) => Reply::Hold(rx),
            None => Reply::Now(error("original cancellation denied")),
        },
        _ => panic!("read only"),
    });
    let mut h = Harness::new(false);
    let remote = h.remote.clone();
    let initial = peer.client.observe(Some(sid()), Some(id));
    let driver = async {
        remote.wait(|f| f.current == Some(id)).await;
        remote.ctrl('c');
        peer.wait(|s| count(s, "cancel") == 1).await;
        remote.ctrl('d');
    };
    let ((), exit) = tokio::join!(driver, h.run(&peer.client, Some(initial)));
    assert!(exit.result.is_ok());
    tx.send(error("original cancellation denied")).unwrap();
    let report = exit.turn.unwrap().detach().await;
    assert_eq!(report.turn_id, Some(id));
    assert_eq!(report.cancel, Some(CancelStatus::Unknown));
    assert_eq!(cancels(&peer.seen.borrow()), vec![json!(id); 2]);
    assert!(detach_lines(report).iter().any(
        |line| line.contains("original cancellation denied") && line.contains(&id.to_string())
    ));
}

#[tokio::test]
async fn busy_submission_preserves_draft_and_can_cancel_displayed_active_turn() {
    let active = lattice_api::local::new_turn_id();
    let mut subs = 0;
    let peer = Peer::start(move |r| match r["operation"].as_str().unwrap() {
        "submit" => Reply::Now(response(
            "error",
            json!({"kind":"busy","message":"session is busy","recovery":null,"store":null}),
        )),
        "subscribe" => {
            subs += 1;
            if subs == 1 {
                Reply::Never
            } else {
                let mut v = view(active, false);
                v.owned = r["target"]["turn_id"] == json!(active);
                if !v.owned {
                    v.selected_turn = None;
                }
                Reply::Now(snapshot(&v))
            }
        }
        "history" => Reply::Never,
        "cancel" => {
            assert_eq!(r["target"]["turn_id"], json!(active));
            Reply::Now(response("cancelled", json!({"owned":true})))
        }
        _ => panic!("no config loading"),
    });
    let mut h = Harness::new(false);
    h.app.replace_history(Vec::new()); // 已装入的空路径；本测试从提交窗口开始。
    let remote = h.remote.clone();
    let driver = async {
        remote.line("hi");
        let id = submitted(
            &peer
                .wait(|s| count(s, "submit") == 1 && count(s, "subscribe") == 1)
                .await,
        );
        remote
            .wait(|f| f.input == "hi" && f.text.contains("session is busy"))
            .await;
        remote.press(KeyCode::Esc, KeyModifiers::NONE);
        remote.line("/reconnect");
        remote.wait(|f| f.current == Some(active)).await;
        remote.ctrl('c');
        let f = remote.wait(|f| f.cancel == Some(CancelStatus::Taken)).await;
        // 被拒回执已实时显示，交接报告不再重述。
        assert!(f.text.contains(&id.to_string()), "{}", f.text);
        assert!(!f.text.contains("历史交接：请求"), "{}", f.text);
        remote.ctrl('d');
    };
    let ((), exit) = tokio::join!(driver, h.run(&peer.client, None));
    assert!(exit.result.is_ok());
    assert_eq!(exit.turn.unwrap().detach().await.turn_id, Some(active));
    assert_eq!(count(&peer.seen.borrow(), "submit"), 1);
    assert_eq!(cancels(&peer.seen.borrow()), vec![json!(active)]);
}

#[tokio::test]
async fn existing_submission_keeps_explicit_agent() {
    let peer = Peer::start(|r| match r["operation"].as_str().unwrap() {
        "submit" => Reply::Now(response(
            "error",
            json!({"kind":"busy","message":"busy","recovery":null,"store":null}),
        )),
        _ => Reply::Never,
    });
    let mut h = Harness::new(false);
    h.app.replace_history(Vec::new()); // 已装入的空路径；本测试从提交窗口开始。
    h.options.agent = "override-agent".into();
    let remote = h.remote.clone();
    let driver = async {
        remote.line("hi");
        let seen = peer.wait(|s| count(s, "submit") == 1).await;
        assert_eq!(
            seen.iter().find(|r| r["operation"] == "submit").unwrap()["request"]["agent"],
            "override-agent"
        );
        remote.wait(|f| f.input == "hi").await;
        remote.press(KeyCode::Esc, KeyModifiers::NONE);
        remote.line("/quit");
    };
    let ((), exit) = tokio::join!(driver, h.run(&peer.client, None));
    assert!(exit.result.is_ok());
}

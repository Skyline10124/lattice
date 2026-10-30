// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 真实 LocalTurn、Unix socket 与 run_loop；对端只替代 IPC 系统边界。
use super::fixtures::*;
use super::*;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use lattice_api::local::{LocalErrorKind, UsageCompleteness::*, ViewUpdate};
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{oneshot, watch};

fn release_dock(app: &mut App) {
    assert!(app.dock.message_id().is_some(), "Dock 应有暂驻消息");
    app.apply_action(KeyAction::Insert(' '));
    assert!(app.dock.shows_input());
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

/// 一轮带 `parts` 回复的对话。`closed` 时以 Finish 结束；否则这一步以 Continue 关闭，
/// 下一步仍在请求中，这一轮尚未结束。
fn comment_history(app: &mut App, id: TurnId, parts: Value, closed: bool) {
    let first = step(2, id, 0).parts(parts);
    if closed {
        open_view(
            app,
            head(2, Some(id), false),
            vec![user(1, id, 1, "original prompt"), first.finished().entry()],
        );
    } else {
        open_view(
            app,
            head(3, Some(id), true),
            vec![
                user(1, id, 1, "original prompt"),
                first.tool(1, true).completed_attempt(1).continued().entry(),
                step(3, id, 1).sent().entry(),
            ],
        );
    }
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
    assert!(h.app.queue.is_editing(), "closed text opens comment editor");
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

#[test]
fn comment_esc_delete_paths_saved_count_and_blank_save() {
    let mut h = comment_app();
    let app = &mut h.app;
    comment_browse(app, "comment target");
    app.apply_action(KeyAction::Submit);
    app.apply_action(KeyAction::Paste("new discarded".into()));
    assert_eq!(app.queue.draft_count(), 0, "正在写的新评论不计入待发送");
    app.apply_action(KeyAction::Clear);
    assert!(!transcript(app).contains("new discarded"));
    save_comment(app, "saved note");
    assert_eq!(app.queue.draft_count(), 1);
    comment_browse(app, "saved note");
    app.apply_action(KeyAction::Submit);
    app.apply_action(KeyAction::Paste(" discarded edit".into()));
    app.apply_action(KeyAction::Clear);
    assert!(transcript(app).contains("saved note"));
    assert!(!transcript(app).contains("discarded edit"));
    // 不在编辑时：可打印字符与粘贴被忽略，Esc 只回到输入区，Delete 才删除草稿。
    comment_browse(app, "saved note");
    app.apply_action(KeyAction::Insert('z'));
    app.apply_action(KeyAction::Paste("pasted".into()));
    assert!(matches!(app.focus, Focus::Transcript(_)));
    assert!(app.input.is_empty());
    app.apply_action(KeyAction::Clear);
    assert_eq!(app.focus, Focus::Dock);
    assert_eq!(app.queue.draft_count(), 1);
    comment_browse(app, "saved note");
    app.apply_action(KeyAction::Delete);
    assert_eq!(app.queue.draft_count(), 0);
    save_comment(app, " \n\t");
    assert_eq!(app.queue.draft_count(), 0);
}

/// 提交未被接受的四种方式：Busy、其他拒绝、发出前取消、查询确认后台没有这个请求。
fn reject(app: &mut App, id: TurnId, how: usize) {
    match how {
        0 | 1 => app.apply_submission(receipt(
            id,
            SubmissionResult::Rejected {
                error: local_error(
                    if how == 0 {
                        LocalErrorKind::Busy
                    } else {
                        LocalErrorKind::BadRequest
                    },
                    "rejected",
                ),
            },
        )),
        // 发出前取消的回执不来自查询；查询确认没有这个请求的回执来自查询。
        _ => app.apply_submission(LocalUpdate::Submit {
            update: SubmissionUpdate::Receipt(SubmissionReceipt {
                turn_id: id,
                result: SubmissionResult::NotAccepted,
                recovered: how == 3,
            }),
        }),
    }
}

#[test]
fn comment_manual_rejections_keep_user_markers_and_restore_one_draft_copy() {
    for how in 0..4 {
        let mut h = comment_app();
        let app = &mut h.app;
        save_comment(app, "rejected note");
        app.focus = Focus::Dock;
        app.input.set_text("body");
        let wire = app.apply_action(KeyAction::Submit).unwrap();
        assert_eq!(crate::comment::Message::parse(&wire).unwrap().text, "body");
        assert_eq!(app.history.prev(), Some("body"));
        let id = app.own_turn.unwrap();
        reject(app, id, how);
        assert_eq!(app.input.text(), "body", "{how}");
        let text = transcript(app);
        assert_eq!(text.matches("[COMMENT 1]").count(), 1);
        assert_eq!(text.matches("rejected note").count(), 1, "{how}");
        assert!(text.contains("草稿 · #1"), "{how}: {text}");
        comment_browse(app, "rejected note");
        app.apply_action(KeyAction::Submit);
        assert!(app.queue.is_editing(), "restored draft remains editable");
        app.apply_action(KeyAction::Clear);
        comment_browse(app, "rejected note");
        app.apply_action(KeyAction::Delete);
        save_comment(app, "next note");
        assert_eq!(
            app.queue.drafts().next().unwrap().comment().n,
            2,
            "retained rejected marker reserves its number"
        );
    }
}

#[test]
fn comment_queue_only_edit_rejection_and_delete() {
    for how in 0..4 {
        let mut h = comment_app();
        let app = &mut h.app;
        app.focus = Focus::Dock;
        app.input.set_text("running prompt");
        app.submit().unwrap();
        let running = app.own_turn.unwrap();
        save_comment(app, "queued note");
        app.focus = Focus::Dock;
        app.input.set_text("queued body");
        assert!(app.apply_action(KeyAction::Submit).is_none());
        let message_id = app.queue.messages().next().unwrap().id;
        assert!(transcript(app).contains("排队 · #1"));
        assert_eq!(app.queue.draft_count(), 0);
        // 从队列标题进入编辑，空正文仍保留只带评论的消息。
        let rendered = app.prepare_transcript();
        let row = rendered
            .owners
            .iter()
            .position(|o| o.id == message_id)
            .unwrap();
        app.focus = Focus::Transcript(rendered.cursor(row, 0).unwrap());
        app.apply_action(KeyAction::Submit);
        assert!(app.queue.is_editing());
        app.apply_action(KeyAction::Home);
        for _ in "queued body".chars() {
            app.apply_action(KeyAction::Delete);
        }
        app.apply_action(KeyAction::Insert(' '));
        app.apply_action(KeyAction::Submit);
        assert_eq!(app.queue.messages().count(), 1);
        app.apply_submission(receipt(running, accepted(running)));
        finish(app, running, "running prompt");
        let wire = app.auto_submit().unwrap();
        assert!(!transcript(app).contains("排队 · #1"));
        assert_eq!(transcript(app).matches("queued note").count(), 1);
        assert_eq!(crate::comment::Message::parse(&wire).unwrap().text, "");
        assert_eq!(app.history.prev(), Some("running prompt"));
        let id = app.own_turn.unwrap();
        reject(app, id, how);
        assert!(app.input.is_empty());
        assert!(app.queue.is_paused());
        let text = transcript(app);
        assert_eq!(text.matches("[COMMENT 1]").count(), 2);
        assert_eq!(text.matches("queued note").count(), 1);
        let rendered = app.prepare_transcript();
        let row = rendered
            .owners
            .iter()
            .position(|o| o.id == message_id)
            .unwrap();
        app.focus = Focus::Transcript(rendered.cursor(row, 0).unwrap());
        app.apply_action(KeyAction::Clear);
        assert_eq!(app.queue.messages().count(), 1, "Esc 不删除排队消息");
        app.focus = Focus::Transcript(rendered.cursor(row, 0).unwrap());
        app.apply_action(KeyAction::Delete);
        assert_eq!(app.queue.messages().count(), 0);
        assert!(!transcript(app).contains("queued note"));
    }
}

/// 重新打开会话：`on_path` 时被评论的那一轮仍在路径上。
fn reload_comment(app: &mut App, target: TurnId, wire: &str, on_path: bool) {
    let new_turn = lattice_api::local::new_turn_id();
    let mut entries = Vec::new();
    if on_path {
        entries.push(user(1, target, 1, "original prompt"));
        entries.push(step(2, target, 0).text("comment target").finished().entry());
    }
    let node = entries.len() as u64 + 1;
    entries.push(user(node, new_turn, node / 2 + 1, wire));
    entries.push(
        step(node + 1, new_turn, 0)
            .text("following answer")
            .finished()
            .entry(),
    );
    open_view(app, head(node + 1, Some(new_turn), false), entries);
}

#[test]
fn comment_reload_attaches_or_falls_back() {
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
        let text = transcript(&h.app);
        assert_eq!(text.matches("reload note").count(), 1);
        assert_eq!(text.contains("原文不匹配 · #1"), mismatch != 0);
        if mismatch == 0 {
            assert!(text.find("reload note").unwrap() < text.find("[COMMENT 1]").unwrap());
        } else {
            assert!(text.find("reload note").unwrap() > text.find("[COMMENT 1]").unwrap());
        }
    }
}

#[test]
fn older_turns_keep_the_view_inside_a_reply_with_inline_comments() {
    let mut source = comment_app();
    save_comment(&mut source.app, "inline note");
    source.app.focus = Focus::Dock;
    let wire = source.app.submit().unwrap();
    let target = crate::comment::Message::parse(&wire).unwrap().comments[0]
        .location
        .fragment
        .turn;
    let mut h = Harness::new(false);
    let app = &mut h.app;
    app.term_width = 80;
    app.term_height = 12;
    let last = lattice_api::local::new_turn_id();
    open_view(
        app,
        head(8, Some(last), false),
        vec![
            user(5, target, 3, "prompt-3"),
            step(6, target, 0)
                .text("comment target\n\nlater paragraph")
                .finished()
                .entry(),
            user(7, last, 4, &wire),
            step(8, last, 0).text("answer-4").finished().entry(),
        ],
    );
    let mut request = None;
    for _ in 0..20 {
        app.apply_action(KeyAction::ScrollPageUp);
        request = request.or(app.older_request.take());
    }
    let request = request.expect("翻到顶部时查询更早的轮");
    // 查询在途时往下翻：顶部停在这一轮回复的正文里，同一块下方穿插着内嵌评论。
    let lines: Vec<String> = app
        .prepare_transcript()
        .lines
        .iter()
        .map(ToString::to_string)
        .collect();
    let find = |marker: &str| lines.iter().position(|line| line.contains(marker)).unwrap();
    let (anchor, note, later) = (
        find("comment target"),
        find("inline note"),
        find("later paragraph"),
    );
    assert!(anchor < note && note < later, "{lines:#?}");
    app.viewport.shift(anchor);
    let top = |app: &mut App| {
        let transcript = app.prepare_transcript();
        transcript.lines[app.viewport.scroll()].to_string()
    };
    let before = top(app);
    assert!(before.contains("comment target"), "{before}");
    app.apply_older(
        request,
        Ok(LocalHistory {
            entries: turns(1..=2),
        }),
    );
    assert_eq!(top(app), before, "视图停在原来看到的那一行");
}

/// CLI 退回到被评论那一轮的 UserMessage：这一轮仍在路径上，被评论的 Step 已不在。
fn rewind_to_user(app: &mut App, target: TurnId) {
    let rewound = SessionHead {
        path_replaced: true,
        ..head(1, Some(target), false)
    };
    app.apply_view(LocalUpdate::Head(rewound));
    app.apply_view(entry_update(user(1, target, 1, "original prompt")));
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
    rewind_to_user(app, target);
    assert_eq!(app.queue.draft_count(), 0);
    assert_eq!(
        app.queue
            .messages()
            .map(|m| m.text.as_str())
            .collect::<Vec<_>>(),
        ["kept text"]
    );
    assert!(app.queue.messages().next().unwrap().comments.is_empty());
    assert_eq!(
        app.dock.message(),
        Some(&status(
            NoticeLevel::Warn,
            "锚点 Step 不在当前历史，丢弃评论：#1、#2、#3"
        ))
    );
}

#[test]
fn comment_rewind_during_queue_edit_preserves_remaining_order() {
    let mut h = comment_app();
    let app = &mut h.app;
    let target = app.head.as_ref().unwrap().turn_id.unwrap();
    app.input.set_text("running");
    app.submit().unwrap();
    let running = app.own_turn.unwrap();
    save_comment(app, "drop this");
    app.focus = Focus::Dock;
    app.submit();
    for text in ["middle", "last"] {
        app.input.set_text(text);
        app.submit();
    }
    app.apply_submission(receipt(running, accepted(running)));
    stop(app, running, "running", "failed");
    comment_browse(app, "middle");
    app.apply_action(KeyAction::Submit);
    assert!(app.queue.is_editing());
    rewind_to_user(app, target);
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
fn comment_warn_error_keeps_space_paste_and_hidden_input_untouched() {
    for level in [NoticeLevel::Warn, NoticeLevel::Error] {
        let mut h = comment_app();
        let app = &mut h.app;
        app.input.set_text("hidden input");
        comment_browse(app, "comment target");
        app.apply_action(KeyAction::Submit);
        app.push_status(status(level, "blocked dock"));
        app.apply_action(KeyAction::Insert('a'));
        app.apply_action(KeyAction::Insert(' '));
        app.apply_action(KeyAction::Paste("b".into()));
        app.apply_action(KeyAction::CancelOrClear);
        app.apply_action(KeyAction::Submit);
        assert_eq!(app.queue.drafts().next().unwrap().comment().text, "a b");
        assert_eq!(app.input.text(), "hidden input");
        assert!(app.dock.message_id().is_some());
    }
}

#[test]
fn comment_adjacent_text_fragments_and_generating_text_are_not_commentable() {
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
    let live = h.app.own_turn.unwrap();
    h.app.apply_submission(receipt(live, accepted(live)));
    h.app
        .apply_view(LocalUpdate::Head(head(4, Some(live), true)));
    h.app.apply_view(entry_update(user(3, live, 2, "run")));
    h.app
        .apply_view(entry_update(step(4, live, 0).sent().entry()));
    h.app
        .apply_view(text(4, 0, 0, &[("live comment target", false)]));
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
    let running = app.own_turn.unwrap();
    save_comment(app, "queue eight");
    app.focus = Focus::Dock;
    app.submit();
    assert_eq!(app.queue.messages().next().unwrap().comments[0].n, 8);
    app.apply_view(LocalUpdate::Connection {
        status: ConnectionStatus::Eof,
    });
    reject(app, running, 0);
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
            assert_eq!(app.cancel_requested, Some(CancelTarget::Own));
        }
        app.input.set_text(" \n ");
        assert_eq!(app.apply_action(KeyAction::Submit), None);
        assert_eq!(app.input.text(), " \n ");
        app.input.set_text("/help");
        app.apply_action(KeyAction::Submit);
        // `/help` 打开 Dock 面板并遮住输入区；Esc 关闭后才回到输入区。
        assert!(!app.dock.shows_input());
        app.apply_action(KeyAction::Clear);
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
        let text = transcript(app);
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
    for level in [NoticeLevel::Warn, NoticeLevel::Error] {
        let mut h = Harness::new(true);
        let app = &mut h.app;
        app.input.set_text("running");
        app.submit().unwrap();
        for text in ["first", "ab中\ncd"] {
            app.input.set_text(text);
            app.submit();
        }
        app.apply_action(KeyAction::Up);
        assert!(transcript(app).contains("Enter 编辑 · Delete 删除"));
        app.apply_action(KeyAction::Submit);
        assert!(transcript(app).contains("Enter 完成编辑"));
        app.push_status(status(level, "dock marker"));
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
        assert!(transcript(app).contains("cd e\nf!"));
        assert!(app.dock.message_id().is_some());
        assert!(matches!(app.focus, Focus::Transcript(_)));
        app.apply_action(KeyAction::Submit);
        app.apply_action(KeyAction::Insert('x'));
        app.apply_action(KeyAction::Clear);
        assert!(!transcript(app).contains("f!x"));
        app.apply_action(KeyAction::Insert(' '));
        assert!(app.dock.shows_input());
        app.apply_action(KeyAction::Delete);
        assert!(!transcript(app).contains("ab中"));
        assert!(transcript(app).contains("first"));
        assert!(matches!(app.focus, Focus::Transcript(_)));
    }
}

fn queued_app() -> Harness {
    let mut h = Harness::new(true);
    h.app.input.set_text("running");
    h.app.submit().unwrap();
    let id = h.app.own_turn.unwrap();
    h.app.apply_submission(receipt(id, accepted(id)));
    for n in 0..5 {
        h.app.input.set_text(&format!(" q{n}\nraw "));
        h.app.submit();
    }
    finish(&mut h.app, id, "running");
    h
}

#[test]
fn automatic_rejections_restore_exact_front_and_do_not_touch_input() {
    for how in 0..4 {
        let mut h = queued_app();
        let app = &mut h.app;
        assert_eq!(app.auto_submit(), Some(" q0\nraw ".into()));
        let id = app.own_turn.unwrap();
        assert!(app.queue.is_full(), "awaiting automatic prompt owns a slot");
        app.input.set_text("user is typing");
        app.submit();
        assert_eq!(app.input.text(), "user is typing");
        reject(app, id, how);
        assert_eq!(app.input.text(), "user is typing", "{how}");
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
        assert_eq!(transcript(app).matches("暂停").count(), 5);
        assert_eq!(app.auto_submit(), None);
        assert_eq!(app.phase, TurnPhase::Idle);
    }
}

#[test]
fn accepting_and_unknown_receipts_release_capacity_without_requeue() {
    for how in 0..4 {
        let mut h = queued_app();
        let app = &mut h.app;
        app.auto_submit().unwrap();
        let id = app.own_turn.unwrap();
        let result = match how {
            0 | 1 => accepted(id),
            2 => SubmissionResult::Unknown { error: None },
            _ => SubmissionResult::Unknown {
                error: Some(local_error(LocalErrorKind::Transport, "lost")),
            },
        };
        app.apply_submission(LocalUpdate::Submit {
            update: SubmissionUpdate::Receipt(SubmissionReceipt {
                turn_id: id,
                result,
                recovered: how == 1,
            }),
        });
        assert!(!app.queue.is_full(), "{how} releases the reserved slot");
        assert_eq!(app.queue.messages().count(), 4);
        assert_eq!(app.queue.is_paused(), how >= 2);
        app.input.set_text("new queued input");
        if how < 2 {
            app.submit();
            assert!(app.input.is_empty(), "接受后这一轮在运行，Enter 入队");
            assert!(app.queue.is_full());
            assert_eq!(
                app.queue.messages().last().unwrap().text,
                "new queued input"
            );
        } else {
            // 结果未知时这条已让出名额，迟到的确定拒绝不放回队首，免得队列超过上限；
            // 只给状态消息，原输入从输入历史找回。不碰用户的新草稿。
            app.apply_submission(receipt(
                id,
                SubmissionResult::Rejected {
                    error: local_error(LocalErrorKind::BadRequest, "late"),
                },
            ));
            assert_eq!(app.input.text(), "new queued input");
            assert_eq!(app.queue.messages().count(), 4);
            let error = app.dock.message().unwrap();
            assert!(
                error.text.contains("late") && error.text.contains("Ctrl+↑"),
                "{}",
                error.text
            );
            assert_eq!(
                app.history.prev(),
                Some("q0\nraw"),
                "自动发送时已记进输入历史"
            );
        }
    }
}

#[test]
fn queue_pauses_on_failure_cancel_and_disconnect_and_resumes_after_failed_manual_turn() {
    for cause in 0..5 {
        let mut h = queued_app();
        let app = &mut h.app;
        app.auto_submit().unwrap();
        let id = app.own_turn.unwrap();
        app.apply_submission(receipt(id, accepted(id)));
        match cause {
            0 => stop(app, id, " q0\nraw ", "failed"),
            1 => stop(app, id, " q0\nraw ", "cancelled"),
            _ => app.apply_view(LocalUpdate::Connection {
                status: match cause {
                    2 => ConnectionStatus::Eof,
                    3 => ConnectionStatus::Idle,
                    _ => ConnectionStatus::Error(local_error(LocalErrorKind::Transport, "offline")),
                },
            }),
        }
        assert!(app.queue.is_paused(), "cause {cause}");
        assert_eq!(transcript(app).matches("暂停").count(), 4);
        assert_eq!(app.auto_submit(), None);
        // 观察中断后重新观察得到成功结局也不能自行恢复。
        if cause >= 2 {
            finish(app, id, " q0\nraw ");
            assert_eq!(app.auto_submit(), None);
        }
        // 失败结局与观察中断是 Error 与 Warn，暂驻 Dock；取消结局是 Info，直接进对话记录。
        assert_eq!(app.dock.message_id().is_some(), cause != 1, "cause {cause}");
        if cause != 1 {
            release_dock(app);
        }
        app.input.set_text("manual while paused");
        assert_eq!(
            app.apply_action(KeyAction::Submit),
            Some("manual while paused".into())
        );
        assert!(app.queue.is_paused());
        let manual = app.own_turn.unwrap();
        app.apply_submission(receipt(manual, accepted(manual)));
        stop(app, manual, "manual while paused", "failed");
        assert!(app.input.is_empty());
        assert_eq!(
            app.apply_action(KeyAction::Submit),
            None,
            "失败结局暂驻 Dock 时输入区不可达"
        );
        release_dock(app);
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
    for save in [true, false] {
        let mut h = queued_app();
        let app = &mut h.app;
        app.auto_submit().unwrap();
        let first = app.own_turn.unwrap();
        app.apply_submission(receipt(first, accepted(first)));
        app.apply_action(KeyAction::Up);
        app.apply_action(KeyAction::Submit);
        app.apply_action(KeyAction::Insert('!'));
        finish(app, first, " q0\nraw ");
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
        let id = app.own_turn.unwrap();
        app.apply_submission(receipt(id, accepted(id)));
        finish(app, id, " q1\nraw ");
        assert_eq!(app.auto_submit(), Some(" q2\nraw ".into()));
        assert_eq!(app.auto_submit(), None);
    }
}

#[test]
fn detached_automatic_receipt_settles_its_slot_without_touching_new_turn() {
    for how in 0..4 {
        let mut h = queued_app();
        let app = &mut h.app;
        app.auto_submit().unwrap();
        let id = app.own_turn.unwrap();
        // 会话视图可以先于迟到的提交回执读到结局。
        finish(app, id, " q0\nraw ");
        app.input.set_text("new manual");
        app.submit().unwrap();
        let manual = app.own_turn;
        app.input.set_text("new draft");
        let result = match how {
            0 => Some(accepted(id)),
            1 => Some(SubmissionResult::Unknown { error: None }),
            2 => Some(SubmissionResult::Rejected {
                error: local_error(LocalErrorKind::Busy, "busy"),
            }),
            _ => None,
        };
        app.apply_handoff(DetachReport {
            session_id: sid(),
            turn_id: Some(id),
            submission: result.map(|result| SubmissionReceipt {
                turn_id: id,
                result,
                recovered: false,
            }),
            submission_diagnostic: None,
            cancel: None,
            cancel_error: None,
            head: None,
            summary: None,
        });
        assert_eq!(app.input.text(), "new draft");
        assert_eq!(app.own_turn, manual);
        assert_eq!(app.phase, TurnPhase::Streaming);
        // 视图中的 UserMessage 已经说明接受；迟到的回执不再把它放回排队，也不暂停排队。
        assert!(!app.queue.is_full(), "{how}");
        assert!(!app.queue.is_paused(), "{how}");
    }
}

#[test]
fn handoff_reports_a_definite_result_after_a_live_unknown() {
    let mut h = Harness::new(false);
    let app = &mut h.app;
    app.input.set_text("first");
    app.submit().unwrap();
    let id = app.own_turn.unwrap();
    app.apply_submission(receipt(id, SubmissionResult::Unknown { error: None }));
    release_dock(app);
    app.input.set_text("second");
    app.submit().unwrap();
    let next = app.own_turn;
    app.input.set_text("new draft");
    let report = |result| DetachReport {
        session_id: sid(),
        turn_id: Some(id),
        submission: Some(SubmissionReceipt {
            turn_id: id,
            result,
            recovered: false,
        }),
        submission_diagnostic: None,
        cancel: None,
        cancel_error: None,
        head: None,
        summary: None,
    };
    // 同一事实已实时显示过，不再重述。
    app.apply_handoff(report(SubmissionResult::Unknown { error: None }));
    assert_eq!(transcript(app).matches("提交结果未知").count(), 1);
    // 未知之后后台明确拒绝：没有显示过，以带原身份的 Info 说明。
    app.apply_handoff(report(SubmissionResult::Rejected {
        error: local_error(LocalErrorKind::Busy, "backend refused"),
    }));
    let text = transcript(app);
    let line = text
        .lines()
        .find(|line| line.contains("backend refused"))
        .unwrap_or_else(|| panic!("{text}"));
    assert!(line.contains(&id.to_string()), "{line}");
    assert!(app.dock.message().is_none(), "Info 不暂驻");
    assert_eq!(app.input.text(), "new draft");
    assert_eq!(app.own_turn, next);
    assert_eq!(app.phase, TurnPhase::Streaming);
    assert_eq!(app.cancel_status, None);
}

#[test]
fn manual_rejections_restore_the_input() {
    // 结果未知之后迟到的确定拒绝同样归还；发出前取消不会晚于未知。
    for (how, late) in (0..4)
        .map(|how| (how, false))
        .chain([(0, true), (1, true), (3, true)])
    {
        let mut h = Harness::new(true);
        let app = &mut h.app;
        app.input.set_text(" manual\nraw ");
        app.submit().unwrap();
        let id = app.own_turn.unwrap();
        if late {
            app.apply_submission(receipt(id, SubmissionResult::Unknown { error: None }));
        }
        reject(app, id, how);
        assert_eq!(app.input.text(), " manual\nraw ", "{how} {late}");
        assert!(!transcript(app).contains("Ctrl+↑"), "{how} {late}");
        assert_eq!(app.queue.messages().count(), 0);
        assert_eq!(app.phase, TurnPhase::Idle);
    }
}

#[test]
fn manual_rejections_keep_a_new_draft_and_point_to_the_input_history() {
    for (how, late) in (0..4)
        .map(|how| (how, false))
        .chain([(0, true), (1, true), (3, true)])
    {
        let mut h = Harness::new(true);
        let app = &mut h.app;
        app.input.set_text(" manual\nraw ");
        app.submit().unwrap();
        let id = app.own_turn.unwrap();
        if late {
            app.apply_submission(receipt(id, SubmissionResult::Unknown { error: None }));
        }
        app.input.set_text("new draft");
        reject(app, id, how);
        assert_eq!(
            app.input.text(),
            "new draft",
            "{how} {late}: 不覆盖用户的草稿"
        );
        let hint = app
            .blocks
            .iter()
            .filter_map(|block| match block {
                Block::Status(status) => Some(status.text.as_str()),
                _ => None,
            })
            .next_back()
            .unwrap();
        assert!(
            hint.contains("Ctrl+↑") && !hint.contains("输入已保留"),
            "{how} {late}: {hint}"
        );
        assert_eq!(
            app.history.prev(),
            Some("manual\nraw"),
            "原输入在输入历史中"
        );
        assert_eq!(app.phase, TurnPhase::Idle);
    }
}

#[test]
fn empty_edit_deletes_and_ctrl_c_never_clears_hidden_input_or_editor() {
    for level in [NoticeLevel::Warn, NoticeLevel::Error] {
        let mut h = Harness::new(true);
        let app = &mut h.app;
        app.input.set_text("running");
        app.submit().unwrap();
        let id = app.own_turn.unwrap();
        app.input.set_text("中a");
        app.submit();
        app.apply_action(KeyAction::Up);
        app.apply_action(KeyAction::Submit);
        app.apply_submission(receipt(id, accepted(id)));
        stop(app, id, "running", "failed");
        app.input.set_text("hidden draft");
        app.push_status(status(level, "dock"));
        handle_input_event(
            app,
            Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        );
        assert!(app.last_ctrl_c.is_some());
        assert!(!app.should_quit);
        assert_eq!(app.input.text(), "hidden draft");
        app.apply_action(KeyAction::Insert(' '));
        assert!(app.dock.message_id().is_some());
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

enum Reply {
    Now(Value),
    Hold(oneshot::Receiver<Value>),
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
                assert_eq!(&magic, lattice_api::local::test_support::FRAME_MAGIC);
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
/// 会话视图的推送：每个 watch 连接都收到已推送的全部内容与之后的推送。
#[derive(Clone, Default)]
struct Views(std::sync::Arc<std::sync::Mutex<ViewState>>);
#[derive(Default)]
struct ViewState {
    pushed: Vec<Value>,
    watchers: Vec<UnboundedSender<Value>>,
}
impl Views {
    fn subscribe(&self) -> Reply {
        let (sender, receiver) = unbounded();
        let mut state = self.0.lock().unwrap();
        for value in &state.pushed {
            sender.unbounded_send(value.clone()).unwrap();
        }
        state.watchers.push(sender);
        Reply::Stream(receiver)
    }
    fn push(&self, values: impl IntoIterator<Item = Value>) {
        let mut state = self.0.lock().unwrap();
        for value in values {
            state
                .watchers
                .retain(|sender| sender.unbounded_send(value.clone()).is_ok());
            state.pushed.push(value);
        }
    }
}
async fn send(stream: &mut UnixStream, value: &Value, seq: &mut u64) {
    let bytes = serde_json::to_vec(&json!({"message_id":*seq,"offset":0,"json_fragment":serde_json::to_string(value).unwrap(),"complete":true})).unwrap();
    *seq += 1;
    if stream
        .write_all(lattice_api::local::test_support::FRAME_MAGIC)
        .await
        .is_err()
    {
        return;
    }
    let _ = stream.write_u32(bytes.len() as u32).await;
    let _ = stream.write_all(&bytes).await;
}
fn response(kind: &str, value: Value) -> Value {
    json!({"response":kind,"value":value})
}
fn error_reply(error: LocalError) -> Value {
    response("error", serde_json::to_value(error).unwrap())
}
fn view_reply(update: ViewUpdate) -> Value {
    response("view", serde_json::to_value(update).unwrap())
}
fn head_reply(head: SessionHead) -> Value {
    view_reply(ViewUpdate::Head(head))
}
fn entry_reply(entry: ConversationEntry) -> Value {
    view_reply(ViewUpdate::Entry {
        entry: Box::new(entry),
    })
}
/// 接受回执，会话取请求写明的那一个。
fn accepted_reply(request: &Value) -> Value {
    let mut turn = serde_json::to_value(turn_view(request_turn(request))).unwrap();
    turn["session_id"] = json!(request_session(request).id());
    response("acceptance", json!({"acceptance":"accepted","turn":turn}))
}
fn request_turn(request: &Value) -> TurnId {
    serde_json::from_value(request["request"]["turn_id"].clone()).unwrap()
}
fn request_session(request: &Value) -> SubmitSession {
    serde_json::from_value(request["request"]["session"].clone()).unwrap()
}
fn requests<'a>(seen: &'a [Value], op: &'a str) -> impl Iterator<Item = &'a Value> {
    seen.iter().filter(move |r| r["operation"] == op)
}
fn count(seen: &[Value], op: &str) -> usize {
    requests(seen, op).count()
}

/// 循环测试的总上限：测试端或应用循环挂住时以失败结束，不无限占用验证资源。
const LOOP_LIMIT: Duration = Duration::from_secs(20);
async fn bounded<T>(limit: Duration, run: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(limit, run)
        .await
        .expect("循环测试超过总上限，测试端或应用循环挂住")
}

#[derive(Clone)]
struct Frame {
    phase: TurnPhase,
    session: Option<SessionId>,
    connected: bool,
    input: String,
    text: String,
    /// Dock 中暂驻的 Warn 或 Error 的文字；没有暂驻消息时为空。它不在 `text` 里。
    dock: String,
    cancel: Option<CancelStatus>,
    usage: Option<UsageSummary>,
    context_tokens: Option<u64>,
    statusline: String,
}
fn frame(app: &App) -> Frame {
    Frame {
        phase: app.phase,
        session: app.ctx.session_id,
        connected: app.connected,
        input: app.input.text(),
        text: transcript(app),
        dock: app
            .dock
            .message()
            .map_or_else(String::new, |status| status.text.clone()),
        cancel: app.cancel_status,
        usage: app.turn_usage.clone(),
        context_tokens: app.context_tokens,
        statusline: crate::view::statusline_text(app, 200),
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
    async fn run(&mut self, client: &LocalClient) -> LoopExit {
        let observer = self.observer.clone();
        run_loop(
            &self.options,
            &mut self.app,
            client,
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
        self.wait_within(Duration::from_secs(5), predicate).await
    }
    async fn wait_within(&self, limit: Duration, predicate: impl FnMut(&Frame) -> bool) -> Frame {
        tokio::time::timeout(limit, self.frames.clone().wait_for(predicate))
            .await
            .unwrap()
            .unwrap()
            .clone()
    }
    /// 处理屏障，用于暂停的时钟。暂停的时钟只在运行时既没有就绪任务、也没有待处理的 IO 时才自动前进，
    /// 所以这次睡眠到期一定晚于：观察任务处理完已到期的计时器，它的推送经队列被应用循环取走并应用。
    /// 睡过两帧，应用循环已在其后的帧时刻绘出这些变化；没有变化时不重绘，帧仍是此前的状态。
    async fn processed(&self) -> Frame {
        tokio::time::sleep(FRAME * 2).await;
        self.frames.borrow().clone()
    }
}

#[tokio::test]
async fn metering_waits_for_position_and_counts_retries_and_steps_once() {
    let views = Views::default();
    let view_tx = views.clone();
    let peer = Peer::start(move |r| match r["operation"].as_str().unwrap() {
        "watch" => views.subscribe(),
        "submit" => Reply::Now(accepted_reply(r)),
        _ => Reply::Never,
    });
    let mut h = Harness::new(false);
    let remote = h.remote.clone();
    let driver = async {
        peer.wait(|seen| count(seen, "watch") == 1).await;
        let old = numbered_turn(1);
        view_tx.push([
            head_reply(head(2, Some(old), false)),
            entry_reply(user(1, old, 1, "old")),
            entry_reply(
                step(2, old, 0)
                    .completed_attempt(0)
                    .metering(
                        json!({"input":8000,"output":800}),
                        Complete,
                        Some((0.50, true)),
                    )
                    .finished()
                    .entry(),
            ),
        ]);
        remote
            .wait(|f| {
                f.usage
                    .as_ref()
                    .is_some_and(|u| u.input.known == Some(8000))
            })
            .await;
        remote.line("new");
        let seen = peer.wait(|seen| count(seen, "submit") == 1).await;
        let id = request_turn(requests(&seen, "submit").next().unwrap());
        remote
            .wait(|f| f.phase == TurnPhase::Streaming && f.usage.is_none())
            .await;
        view_tx.push([
            head_reply(head(4, Some(id), true)),
            entry_reply(user(3, id, 2, "new")),
        ]);
        let loading = remote
            .wait(|f| f.text.contains("new") && f.usage.is_none() && f.context_tokens.is_none())
            .await;
        assert_eq!(loading.phase, TurnPhase::Streaming);
        let retry = || {
            step(4, id, 0).retried("info", "retry").metering(
                json!({"input":100,"output":10,"cache_read":0}),
                Partial,
                Some((0.01, false)),
            )
        };
        view_tx.push([entry_reply(retry().sent().entry())]);
        let live = remote
            .wait(|f| f.usage.as_ref().is_some_and(|u| u.attempts == 2))
            .await;
        let live_usage = live.usage.unwrap();
        assert_eq!(live_usage.input.known, Some(100));
        assert!(!live_usage.input.complete);
        assert_eq!(live_usage.partial_attempts, 2);
        let continued = || {
            retry()
                .completed_attempt(0)
                .metering(
                    json!({"input":900,"output":90,"cache_read":900}),
                    Complete,
                    Some((0.02, true)),
                )
                .continued()
                .entry()
        };
        view_tx.push([
            head_reply(head(5, Some(id), true)),
            entry_reply(continued()),
            entry_reply(step(5, id, 1).sent().entry()),
        ]);
        let before_repeat = remote
            .wait(|f| {
                f.usage.as_ref().is_some_and(|u| {
                    u.attempts == 3 && u.partial_attempts == 2 && u.input.known == Some(1000)
                })
            })
            .await;
        // 同一视图流中的后续文字已绘出，才能证明重复条目已经被消费。
        view_tx.push([
            entry_reply(continued()),
            view_reply(ViewUpdate::Text(TextUpdate {
                node: NodeId(5),
                attempt: 0,
                offset: 0,
                segments: vec![TextSegment::Text {
                    text: "after repeated entry".into(),
                }],
            })),
        ]);
        let repeated = remote
            .wait(|f| f.text.contains("after repeated entry"))
            .await;
        assert_eq!(repeated.usage, before_repeat.usage);
        assert_eq!(repeated.context_tokens, before_repeat.context_tokens);
        view_tx.push([
            head_reply(head(5, Some(id), false)),
            entry_reply(
                step(5, id, 1)
                    .text("after repeated entry done")
                    .completed_attempt(0)
                    .metering(
                        json!({"input":1000,"output":100,"cache_read":0}),
                        Complete,
                        Some((0.04, true)),
                    )
                    .finished()
                    .entry(),
            ),
        ]);
        let closed = remote
            .wait(|f| {
                f.phase == TurnPhase::Idle
                    && f.usage.as_ref().is_some_and(|u| {
                        u.partial_attempts == 1 && u.input.known == Some(2000) && u.input.complete
                    })
            })
            .await;
        let usage = closed.usage.as_ref().unwrap();
        assert_eq!(usage.attempts, 3);
        assert_eq!(usage.input.known, Some(2000));
        assert_eq!(usage.output.known, Some(200));
        assert_eq!(usage.cache_read.known, Some(900));
        assert_eq!(usage.partial_attempts, 1);
        assert!(closed.statusline.contains("缓存 45%"));
        let cost = usage.cost.known.unwrap();
        assert!((cost.usd - 0.07).abs() < 1e-12);
        assert!(!cost.exact);
        assert_eq!(closed.context_tokens, Some(1100));
        remote.ctrl('d');
    };
    let ((), exit) = bounded(LOOP_LIMIT, async {
        tokio::join!(driver, h.run(&peer.client))
    })
    .await;
    assert!(exit.result.is_ok());
    assert_eq!(count(&peer.seen.borrow(), "usage"), 0, "显示计量只消费视图");
}

#[tokio::test(start_paused = true)]
async fn ctrl_c_hint_expiry_redraws_without_input() {
    let mut h = Harness::new(true);
    let client = LocalClient::new(lattice_api::StateLayout::in_state_root(h._root.path()));
    let remote = h.remote.clone();
    let driver = async {
        remote.ctrl('c');
        let shown = remote
            .wait(|f| f.statusline.contains("Ctrl+C to quit"))
            .await;
        tokio::time::advance(CTRL_C_WINDOW + FRAME).await;
        let cleared = remote
            .wait(|f| !f.statusline.contains("Ctrl+C to quit"))
            .await;
        assert_eq!(cleared.input, shown.input);
        assert_eq!(cleared.phase, shown.phase);
        assert_eq!(cleared.text, shown.text);
        remote.ctrl('d');
    };
    let ((), exit) = bounded(LOOP_LIMIT, async { tokio::join!(driver, h.run(&client)) }).await;
    assert!(exit.result.is_ok());
}

#[tokio::test]
async fn new_session_submits_new_until_accepted_then_existing() {
    let views = Views::default();
    let view_tx = views.clone();
    let mut submits = 0;
    let peer = Peer::start(move |r| match r["operation"].as_str().unwrap() {
        "submit" => {
            submits += 1;
            match submits {
                1 | 3 => Reply::Now(error_reply(local_error(
                    LocalErrorKind::Busy,
                    "session is busy",
                ))),
                _ => Reply::Now(accepted_reply(r)),
            }
        }
        "watch" => views.subscribe(),
        _ => Reply::Never,
    });
    let mut h = Harness::new(true);
    let remote = h.remote.clone();
    let driver = async {
        remote.line("first");
        let seen = peer.wait(|s| count(s, "submit") == 1).await;
        let SubmitSession::New(created) = request_session(&seen[0]) else {
            panic!("新会话首次提交写明新建：{}", seen[0]);
        };
        let shown = remote
            .wait(|f| f.input == "first" && f.dock.contains("session is busy"))
            .await;
        assert_eq!(shown.session, None, "被拒之前没有已建立的会话");
        remote.press(KeyCode::Char(' '), KeyModifiers::NONE);
        remote.press(KeyCode::Enter, KeyModifiers::NONE);
        let seen = peer.wait(|s| count(s, "submit") == 2).await;
        let second = requests(&seen, "submit").nth(1).unwrap();
        assert_eq!(
            request_session(second),
            SubmitSession::New(created),
            "确认被接受前沿用同一 SessionId 与 New"
        );
        assert_ne!(request_turn(second), request_turn(&seen[0]));
        remote.wait(|f| f.session == Some(created)).await;
        let seen = peer.wait(|s| count(s, "watch") >= 1).await;
        assert!(
            requests(&seen, "watch").all(|r| r["session_id"] == json!(created)),
            "接受后观察建立的会话"
        );
        let turn = request_turn(second);
        let created_head = |position, owner| SessionHead {
            session_id: created,
            ..head(position, Some(turn), owner)
        };
        view_tx.push([
            head_reply(created_head(2, false)),
            entry_reply(user(1, turn, 1, "first")),
            entry_reply(step(2, turn, 0).text("done").finished().entry()),
        ]);
        remote
            .wait(|f| f.phase == TurnPhase::Idle && f.text.contains("done"))
            .await;
        assert_eq!(
            count(&peer.seen.borrow(), "watch"),
            1,
            "提交句柄不另开视图，只有 TUI 自己的 watch"
        );
        remote.line("third");
        let seen = peer.wait(|s| count(s, "submit") == 3).await;
        assert_eq!(
            request_session(requests(&seen, "submit").nth(2).unwrap()),
            SubmitSession::Existing(created),
            "确认被接受后改为 Existing"
        );
        remote.wait(|f| f.dock.contains("session is busy")).await;
        remote.press(KeyCode::Char(' '), KeyModifiers::NONE);
        remote.press(KeyCode::Esc, KeyModifiers::NONE);
        remote.ctrl('d');
    };
    let ((), exit) = bounded(LOOP_LIMIT, async {
        tokio::join!(driver, h.run(&peer.client))
    })
    .await;
    assert!(exit.result.is_ok());
    assert_eq!(count(&peer.seen.borrow(), "cancel"), 0, "普通退出不取消");
}

#[tokio::test]
async fn session_exists_conflict_recovers_the_previous_submission_by_turn_id() {
    let mut submits = 0;
    let mut lookups = 0;
    let mut created = Value::Null;
    let peer = Peer::start(move |r| match r["operation"].as_str().unwrap() {
        "submit" => {
            submits += 1;
            created = json!(request_session(r).id());
            if submits == 1 {
                // 后台报告接受事务结果不明；随后的查询也失败，提交结果仍未知。
                Reply::Now(response(
                    "acceptance",
                    json!({"acceptance":"confirmation_unknown","turn_id":r["request"]["turn_id"],
                        "session_id":created}),
                ))
            } else {
                let mut error = local_error(LocalErrorKind::Conflict, "session exists");
                error.conflict = Some(lattice_api::local::ConflictReason::SessionExists);
                Reply::Now(error_reply(error))
            }
        }
        "turn" => {
            lookups += 1;
            if lookups == 1 {
                Reply::Now(error_reply(local_error(
                    LocalErrorKind::StoreUnavailable,
                    "lookup failed",
                )))
            } else {
                let turn_id = serde_json::from_value(r["turn_id"].clone()).unwrap();
                let mut turn = serde_json::to_value(turn_detail(turn_id)).unwrap();
                turn["session_id"] = created.clone();
                Reply::Now(response("turn", json!({"status":"accepted","turn":turn})))
            }
        }
        _ => Reply::Never,
    });
    let mut h = Harness::new(true);
    let remote = h.remote.clone();
    let driver = async {
        remote.line("first");
        let seen = peer.wait(|s| count(s, "turn") == 1).await;
        let first = request_turn(&seen[0]);
        let created = request_session(&seen[0]).id();
        remote.wait(|f| f.dock.contains("提交结果未知")).await;
        remote.press(KeyCode::Char(' '), KeyModifiers::NONE);
        remote.line("second");
        let seen = peer.wait(|s| count(s, "turn") == 2).await;
        assert_eq!(
            request_session(requests(&seen, "submit").nth(1).unwrap()),
            SubmitSession::New(created)
        );
        assert_eq!(
            requests(&seen, "turn").nth(1).unwrap()["turn_id"],
            json!(first),
            "按上一次提交的 TurnId 找回"
        );
        let shown = remote
            .wait(|f| f.text.contains("按 TurnId 查到已接受的请求") && f.session.is_some())
            .await;
        assert_eq!(shown.input, "second", "被拒的这一次保留输入");
        let seen = peer.wait(|s| count(s, "watch") == 1).await;
        assert_eq!(
            requests(&seen, "watch").next().unwrap()["session_id"],
            json!(created),
            "找回后按建立的会话开始 watch"
        );
        remote.press(KeyCode::Char(' '), KeyModifiers::NONE);
        remote.press(KeyCode::Esc, KeyModifiers::NONE);
        remote.ctrl('d');
    };
    let ((), exit) = bounded(LOOP_LIMIT, async {
        tokio::join!(driver, h.run(&peer.client))
    })
    .await;
    assert!(exit.result.is_ok());
    assert_eq!(count(&peer.seen.borrow(), "submit"), 2);
}

#[tokio::test]
async fn unknown_acceptance_is_recovered_by_turn_query_and_the_view_takes_over() {
    let views = Views::default();
    let view_tx = views.clone();
    let mut lookups = 0;
    let peer = Peer::start(move |r| match r["operation"].as_str().unwrap() {
        "watch" => views.subscribe(),
        "submit" => Reply::Now(response(
            "acceptance",
            json!({"acceptance":"confirmation_unknown","turn_id":r["request"]["turn_id"],"session_id":SESSION}),
        )),
        "turn" => {
            lookups += 1;
            let turn_id = serde_json::from_value(r["turn_id"].clone()).unwrap();
            Reply::Now(if lookups == 1 {
                response("turn", json!({"status":"in_flight"}))
            } else {
                response(
                    "turn",
                    json!({"status":"accepted","turn":turn_detail(turn_id)}),
                )
            })
        }
        _ => Reply::Never,
    });
    let mut h = Harness::new(false);
    let remote = h.remote.clone();
    let driver = async {
        view_tx.push([head_reply(head(0, None, false))]);
        remote.line("prompt");
        let seen = peer.wait(|s| count(s, "turn") == 2).await;
        let turn = request_turn(requests(&seen, "submit").next().unwrap());
        remote
            .wait(|f| f.text.contains("按 TurnId 查到已接受的请求"))
            .await;
        view_tx.push([
            head_reply(head(2, Some(turn), true)),
            entry_reply(user(1, turn, 1, "prompt")),
            entry_reply(step(2, turn, 0).sent().entry()),
        ]);
        let shown = remote.wait(|f| f.text.contains("#1 ──")).await;
        assert_eq!(shown.text.matches("prompt").count(), 1, "{}", shown.text);
        assert_eq!(shown.phase, TurnPhase::Streaming);
        remote.ctrl('c');
        remote.ctrl('c');
    };
    let ((), exit) = bounded(LOOP_LIMIT, async {
        tokio::join!(driver, h.run(&peer.client))
    })
    .await;
    assert!(exit.result.is_ok());
    assert_eq!(count(&peer.seen.borrow(), "submit"), 1, "找回不重新提交");
}

#[tokio::test]
async fn cancel_goes_to_the_running_turn_by_turn_id() {
    // 其他客户端提交的一轮：按会话头所示的 TurnId 直接取消。
    let other = numbered_turn(1);
    let views = Views::default();
    let view_tx = views.clone();
    let peer = Peer::start(move |r| match r["operation"].as_str().unwrap() {
        "watch" => views.subscribe(),
        "cancel" => Reply::Now(response("cancelled", json!({"owned":true}))),
        _ => Reply::Never,
    });
    let mut h = Harness::new(false);
    let remote = h.remote.clone();
    let driver = async {
        view_tx.push([
            head_reply(head(2, Some(other), true)),
            entry_reply(user(1, other, 1, "cli prompt")),
            entry_reply(step(2, other, 0).sent().entry()),
        ]);
        remote.wait(|f| f.phase == TurnPhase::Streaming).await;
        remote.ctrl('c');
        let seen = peer.wait(|s| count(s, "cancel") == 1).await;
        assert_eq!(
            requests(&seen, "cancel").next().unwrap()["turn_id"],
            json!(other)
        );
        remote.wait(|f| f.text.contains("后台已接管取消")).await;
        tokio::time::sleep(CTRL_C_WINDOW + Duration::from_millis(50)).await;
        remote.ctrl('d');
    };
    let ((), exit) = bounded(LOOP_LIMIT, async {
        tokio::join!(driver, h.run(&peer.client))
    })
    .await;
    assert!(exit.result.is_ok());
    assert!(exit.turn.is_none());

    // 本客户端的提交：经它的 LocalTurn 按同一 TurnId 取消。
    let (submit_tx, submit_rx) = oneshot::channel();
    let mut submit_rx = Some(submit_rx);
    let peer = Peer::start(move |r| match r["operation"].as_str().unwrap() {
        "submit" => Reply::Hold(submit_rx.take().unwrap()),
        "cancel" => Reply::Now(response("cancelled", json!({"owned":true}))),
        _ => Reply::Never,
    });
    let mut h = Harness::new(false);
    let remote = h.remote.clone();
    let driver = async {
        remote.line("mine");
        let seen = peer.wait(|s| count(s, "submit") == 1).await;
        let submit = requests(&seen, "submit").next().unwrap().clone();
        let own = request_turn(&submit);
        remote.ctrl('c');
        let seen = peer.wait(|s| count(s, "cancel") == 1).await;
        assert_eq!(
            requests(&seen, "cancel").next().unwrap()["turn_id"],
            json!(own)
        );
        remote.wait(|f| f.cancel == Some(CancelStatus::Taken)).await;
        submit_tx.send(accepted_reply(&submit)).unwrap();
        remote.ctrl('c');
        own
    };
    let (own, exit) = bounded(LOOP_LIMIT, async {
        tokio::join!(driver, h.run(&peer.client))
    })
    .await;
    assert!(exit.result.is_ok());
    let report = exit.turn.unwrap().detach().await;
    assert_eq!(report.turn_id, Some(own));
    assert_eq!(report.cancel, Some(CancelStatus::Taken));
}

#[tokio::test]
async fn configuration_failure_shows_the_error_and_keeps_the_input() {
    let peer = Peer::start(|request| match request["operation"].as_str().unwrap() {
        "submit" => Reply::Now(response(
            "error",
            json!({"kind":"bad_request", "message":"profile for unused is missing", "conflict":null, "recovery":null, "store":null}),
        )),
        _ => Reply::Never,
    });
    let mut h = Harness::new(false);
    let remote = h.remote.clone();
    let driver = async {
        remote.line("needs config");
        let shown = remote
            .wait(|f| f.dock.contains("profile for unused is missing"))
            .await;
        assert_eq!(shown.input, "needs config");
        assert_eq!(shown.phase, TurnPhase::Idle);
        remote.press(KeyCode::Char(' '), KeyModifiers::NONE);
        remote.press(KeyCode::Esc, KeyModifiers::NONE);
        remote.ctrl('d');
    };
    let ((), exit) = bounded(LOOP_LIMIT, async {
        tokio::join!(driver, h.run(&peer.client))
    })
    .await;
    assert!(exit.result.is_ok());
    assert_eq!(count(&peer.seen.borrow(), "submit"), 1);
}

#[tokio::test]
async fn cancel_after_acceptance_reaches_the_new_turn_despite_stale_heads() {
    let previous = numbered_turn(1);
    let views = Views::default();
    let view_tx = views.clone();
    let peer = Peer::start(move |r| match r["operation"].as_str().unwrap() {
        "watch" => views.subscribe(),
        "submit" => Reply::Now(accepted_reply(r)),
        "cancel" => Reply::Now(response("cancelled", json!({"owned":true}))),
        _ => Reply::Never,
    });
    let mut h = Harness::new(false);
    let remote = h.remote.clone();
    let previous_step = |text: &str| step(2, previous, 0).text(text).finished().entry();
    let driver = async {
        view_tx.push([
            head_reply(head(2, Some(previous), false)),
            entry_reply(user(1, previous, 1, "before")),
            entry_reply(previous_step("old answer")),
        ]);
        remote
            .wait(|f| f.text.contains("old answer") && f.phase == TurnPhase::Idle)
            .await;
        remote.line("mine");
        let seen = peer.wait(|s| count(s, "submit") == 1).await;
        let own = request_turn(requests(&seen, "submit").next().unwrap());
        remote
            .wait(|f| f.text.matches("── unused ").count() == 2)
            .await;
        // 接受回执之后，视图里积压的上一轮会话头与当前位置条目才转给新提交的句柄。
        view_tx.push([
            head_reply(head(2, Some(previous), false)),
            entry_reply(previous_step("old answer again")),
        ]);
        remote.wait(|f| f.text.contains("old answer again")).await;
        remote.ctrl('c');
        let seen = peer.wait(|s| count(s, "cancel") == 1).await;
        assert_eq!(
            requests(&seen, "cancel").next().unwrap()["turn_id"],
            json!(own),
            "取消按新提交的 TurnId 发出"
        );
        remote.ctrl('c');
    };
    let ((), exit) = bounded(LOOP_LIMIT, async {
        tokio::join!(driver, h.run(&peer.client))
    })
    .await;
    assert!(exit.result.is_ok());
}

#[tokio::test]
async fn late_receipt_after_unknown_starts_watching_or_shows_the_rejection() {
    for accept in [true, false] {
        let views = Views::default();
        let view_tx = views.clone();
        let (submit_tx, submit_rx) = oneshot::channel();
        let mut submit_rx = Some(submit_rx);
        let peer = Peer::start(move |r| match r["operation"].as_str().unwrap() {
            "submit" => Reply::Hold(submit_rx.take().unwrap()),
            "cancel" => Reply::Now(response("cancelled", json!({"owned":true}))),
            "watch" => views.subscribe(),
            _ => Reply::Never,
        });
        let mut h = Harness::new(true);
        let remote = h.remote.clone();
        let driver = async {
            remote.line("first");
            let seen = peer.wait(|s| count(s, "submit") == 1).await;
            let submit = requests(&seen, "submit").next().unwrap().clone();
            let turn = request_turn(&submit);
            let SubmitSession::New(created) = request_session(&submit) else {
                panic!("新会话首次提交写明新建：{submit}");
            };
            // 首次取消后提交等待到期：结果未知，句柄继续等迟到的回执。
            remote.ctrl('c');
            remote
                .wait_within(Duration::from_secs(10), |f| f.dock.contains("提交结果未知"))
                .await;
            remote.press(KeyCode::Char(' '), KeyModifiers::NONE);
            for c in "new draft".chars() {
                remote.press(KeyCode::Char(c), KeyModifiers::NONE);
            }
            remote.wait(|f| f.input == "new draft").await;
            if accept {
                submit_tx.send(accepted_reply(&submit)).unwrap();
                // 迟到的接受：按这次提交的 SessionId 开始观察，视图照常推进。
                let seen = peer.wait(|s| count(s, "watch") == 1).await;
                assert_eq!(
                    requests(&seen, "watch").next().unwrap()["session_id"],
                    json!(created)
                );
                let created_head = SessionHead {
                    session_id: created,
                    ..head(2, Some(turn), true)
                };
                view_tx.push([
                    head_reply(created_head),
                    entry_reply(user(1, turn, 1, "first")),
                    entry_reply(step(2, turn, 0).text("late answer").sent().entry()),
                ]);
                let shown = remote.wait(|f| f.text.contains("late answer")).await;
                assert_eq!(shown.session, Some(created));
                assert_eq!(shown.input, "new draft");
                // 正在运行：第一次 Ctrl+C 取消这一轮，窗口内第二次退出。
                remote.ctrl('c');
                remote.ctrl('c');
            } else {
                submit_tx
                    .send(error_reply(local_error(LocalErrorKind::Busy, "late busy")))
                    .unwrap();
                // 迟到的拒绝按后台错误回执显示，不开始观察；输入区已有草稿，不覆盖，原输入从输入历史找回。
                let shown = remote.wait(|f| f.dock.contains("late busy")).await;
                assert_eq!(shown.input, "new draft");
                assert!(shown.dock.contains("Ctrl+↑"), "{}", shown.dock);
                assert_eq!(shown.session, None);
                assert_eq!(count(&peer.seen.borrow(), "watch"), 0);
                remote.press(KeyCode::Char(' '), KeyModifiers::NONE);
                remote.press(KeyCode::Esc, KeyModifiers::NONE);
                remote.ctrl('d');
            }
        };
        let ((), exit) = bounded(LOOP_LIMIT, async {
            tokio::join!(driver, h.run(&peer.client))
        })
        .await;
        assert!(exit.result.is_ok());
    }
}

#[tokio::test]
async fn paging_up_queries_turns_before_the_earliest_user_message() {
    let views = Views::default();
    let view_tx = views.clone();
    let peer = Peer::start(move |r| match r["operation"].as_str().unwrap() {
        "watch" => views.subscribe(),
        "history" => Reply::Now(response(
            "history",
            serde_json::to_value(LocalHistory {
                entries: turns(1..=2),
            })
            .unwrap(),
        )),
        _ => Reply::Never,
    });
    let mut h = Harness::new(false);
    let remote = h.remote.clone();
    let driver = async {
        let last = numbered_turn(4);
        view_tx.push([head_reply(head(8, Some(last), false))]);
        view_tx.push(turns(3..=4).into_iter().map(entry_reply));
        remote.wait(|f| f.text.contains("answer-4")).await;
        for _ in 0..10 {
            remote.press(KeyCode::PageUp, KeyModifiers::NONE);
        }
        let seen = peer.wait(|s| count(s, "history") == 1).await;
        let request = requests(&seen, "history").next().unwrap();
        assert_eq!(request["before"], json!(5));
        assert_eq!(request["turns"], json!(lattice_api::local::HISTORY_TURNS));
        assert_eq!(request["session_id"], json!(SESSION));
        remote.wait(|f| f.text.contains("prompt-1")).await;
        remote.press(KeyCode::Char('d'), KeyModifiers::CONTROL);
    };
    let ((), exit) = bounded(LOOP_LIMIT, async {
        tokio::join!(driver, h.run(&peer.client))
    })
    .await;
    assert!(exit.result.is_ok());
    assert_eq!(
        count(&peer.seen.borrow(), "history"),
        1,
        "第 1 轮已装入后不再查询"
    );
}

#[tokio::test]
async fn completed_turn_sends_the_queue_through_the_real_loop_once() {
    let running = numbered_turn(1);
    let views = Views::default();
    let view_tx = views.clone();
    let peer = Peer::start(move |r| match r["operation"].as_str().unwrap() {
        "watch" => views.subscribe(),
        "submit" => Reply::Now(accepted_reply(r)),
        _ => Reply::Never,
    });
    let mut h = Harness::new(false);
    let remote = h.remote.clone();
    let driver = async {
        view_tx.push([
            head_reply(head(2, Some(running), true)),
            entry_reply(user(1, running, 1, "other client")),
            entry_reply(step(2, running, 0).sent().entry()),
        ]);
        remote.wait(|f| f.phase == TurnPhase::Streaming).await;
        remote.line("queued via loop");
        remote.line("second");
        for c in "untouched draft".chars() {
            remote.press(KeyCode::Char(c), KeyModifiers::NONE);
        }
        remote.wait(|f| f.input == "untouched draft").await;
        view_tx.push([
            head_reply(head(2, Some(running), false)),
            entry_reply(step(2, running, 0).text("done").finished().entry()),
        ]);
        let seen = peer.wait(|s| count(s, "submit") == 1).await;
        let request = requests(&seen, "submit").next().unwrap();
        assert_eq!(request["request"]["input"], "queued via loop");
        assert_eq!(request_session(request), SubmitSession::Existing(sid()));
        let shown = remote
            .wait(|f| f.phase == TurnPhase::Streaming && f.text.matches("排队").count() == 1)
            .await;
        assert_eq!(shown.input, "untouched draft");
        remote.press(KeyCode::Esc, KeyModifiers::NONE);
        remote.line("/quit");
    };
    let ((), exit) = bounded(LOOP_LIMIT, async {
        tokio::join!(driver, h.run(&peer.client))
    })
    .await;
    assert!(exit.result.is_ok());
    assert_eq!(count(&peer.seen.borrow(), "submit"), 1);
}

#[tokio::test]
async fn idle_timer_runs_only_while_the_head_shows_an_owner() {
    for owner in [true, false] {
        let id = numbered_turn(1);
        let views = Views::default();
        let view_tx = views.clone();
        let peer = Peer::start(move |r| match r["operation"].as_str().unwrap() {
            "watch" => views.subscribe(),
            _ => Reply::Never,
        });
        let mut h = Harness::new(false);
        let remote = h.remote.clone();
        let driver = async {
            let step = if owner {
                step(2, id, 0).sent()
            } else {
                step(2, id, 0).text("done").finished()
            };
            view_tx.push([
                head_reply(head(2, Some(id), owner)),
                entry_reply(user(1, id, 1, "prompt")),
                entry_reply(step.entry()),
            ]);
            remote.wait(|f| f.text.contains("prompt")).await;
            tokio::time::pause();
            // 处理屏障等帧时暂停的时钟会前进几帧，判定点离空闲超时留出余量。
            let idle = Duration::from_secs(lattice_api::STREAM_IDLE_TIMEOUT_SECS);
            tokio::time::advance(idle - Duration::from_secs(2)).await;
            assert!(remote.processed().await.connected);
            tokio::time::advance(Duration::from_secs(3)).await;
            if owner {
                let shown = remote.wait(|f| !f.connected).await;
                assert_eq!(shown.phase, TurnPhase::Streaming);
                assert!(shown.dock.contains("无增量"), "{}", shown.dock);
                remote.press(KeyCode::Char(' '), KeyModifiers::NONE);
            } else {
                assert!(remote.processed().await.connected, "会话空闲时不计时");
            }
            tokio::time::resume();
            remote.ctrl('d');
        };
        // 暂停的时钟会推进过空闲超时，上限要越过它。
        let limit = LOOP_LIMIT + Duration::from_secs(lattice_api::STREAM_IDLE_TIMEOUT_SECS);
        let ((), exit) = bounded(limit, async { tokio::join!(driver, h.run(&peer.client)) }).await;
        assert!(exit.result.is_ok());
    }
}

#[tokio::test]
async fn existing_submission_keeps_explicit_agent() {
    let peer = Peer::start(|r| match r["operation"].as_str().unwrap() {
        "submit" => Reply::Now(error_reply(local_error(LocalErrorKind::Busy, "busy"))),
        _ => Reply::Never,
    });
    let mut h = Harness::new(false);
    h.options.agent = "override-agent".into();
    let remote = h.remote.clone();
    let driver = async {
        remote.line("hi");
        let seen = peer.wait(|s| count(s, "submit") == 1).await;
        let request = requests(&seen, "submit").next().unwrap();
        assert_eq!(request["request"]["agent"], "override-agent");
        assert_eq!(request_session(request), SubmitSession::Existing(sid()));
        remote.wait(|f| f.input == "hi" && !f.dock.is_empty()).await;
        // 被拒回执是 Error，暂驻 Dock；释放后回到输入区。
        remote.press(KeyCode::Char(' '), KeyModifiers::NONE);
        remote.press(KeyCode::Esc, KeyModifiers::NONE);
        remote.line("/quit");
    };
    let ((), exit) = bounded(LOOP_LIMIT, async {
        tokio::join!(driver, h.run(&peer.client))
    })
    .await;
    assert!(exit.result.is_ok());
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
        let exit = run_loop(&h.options, &mut h.app, &client, events, |_| {
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

// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 真实 LocalTurn、Unix socket 与 run_loop；对端只替代 IPC 系统边界。
use super::fixtures::*;
use super::*;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use lattice_local::{LocalErrorKind, UsageCompleteness::*, ViewUpdate};
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
    let id = lattice_local::new_turn_id();
    let parts = json!([{"semantic":{"reasoning":{"text":"hidden"}}},{"semantic":{"text":{"text":"comment target"}}}]);
    comment_history(&mut h.app, id, parts.clone(), false);
    comment_browse(&mut h.app, "comment target");
    h.app.apply_action(KeyAction::Submit);
    assert!(!h.app.queue.is_editing());
    let mut h = Harness::new(false);
    let id = lattice_local::new_turn_id();
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
    let id = lattice_local::new_turn_id();
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
    let new_turn = lattice_local::new_turn_id();
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
    let last = lattice_local::new_turn_id();
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
    let id = lattice_local::new_turn_id();
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
        let layout = lattice_local::StateLayout::in_state_root(root.path());
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
                assert_eq!(&magic, lattice_local::test_support::FRAME_MAGIC);
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
        .write_all(lattice_local::test_support::FRAME_MAGIC)
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
    turn["session_id"] = json!(request_session(request));
    response("acceptance", json!({"acceptance":"accepted","turn":turn}))
}
fn configure_reply(
    head: SessionHead,
    adjustment: Option<lattice_local::EffortAdjustment>,
) -> Value {
    response(
        "configure",
        serde_json::to_value(ConfigureReply { head, adjustment }).unwrap(),
    )
}
fn request_turn(request: &Value) -> TurnId {
    serde_json::from_value(request["request"]["turn_id"].clone()).unwrap()
}
fn request_session(request: &Value) -> SessionId {
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
    session: SessionId,
    connected: bool,
    input: String,
    text: String,
    /// Dock 中暂驻的 Warn 或 Error 的文字；没有暂驻消息时为空。它不在 `text` 里。
    dock: String,
    cancel: Option<CancelStatus>,
    usage: Option<UsageSummary>,
    context_tokens: Option<u64>,
    statusline: String,
    panel: String,
}
fn frame(app: &App) -> Frame {
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 20)).unwrap();
    terminal
        .draw(|frame| Dock::new(app).render(frame, frame.area()))
        .unwrap();
    let panel = buffer_text(terminal.backend().buffer());
    Frame {
        phase: app.phase,
        session: app.session_id,
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
        panel,
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
    fn new(_new_session: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let unused = lattice_local::AgentName::parse("unused").unwrap();
        let options = TuiOptions {
            session_id: Some(SESSION.into()),
            agent: Some(unused),
            session_dir: root.path().join("history"),
            project_root: root.path().into(),
        };
        Self::with_context(
            options,
            SessionContext {
                session_id: sid(),
                initial_head: SessionHead {
                    agent: "startup".into(),
                    model: None,
                    provider: None,
                    effort: None,
                    settings_version: settings_version(0x01),
                    ..head(0, None, false)
                },
                initial_adjustment: None,
            },
            root,
        )
    }
    fn with_context(options: TuiOptions, ctx: SessionContext, root: tempfile::TempDir) -> Self {
        let app = App::new(ctx, false, &options.session_dir);
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
            .unwrap_or_else(|_| {
                let frame = self.frames.borrow();
                panic!(
                    "等待帧超时：panel={} statusline={} input={} dock={}",
                    frame.panel, frame.statusline, frame.input, frame.dock
                )
            })
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
    let peer = Peer::start(|_| Reply::Never);
    let mut h = Harness::new(true);
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
    let ((), exit) = bounded(LOOP_LIMIT, async {
        tokio::join!(driver, h.run(&peer.client))
    })
    .await;
    assert!(exit.result.is_ok());
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
async fn late_receipt_after_unknown_continues_streaming_or_shows_the_rejection() {
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
            let seen = peer.wait(|s| count(s, "watch") == 1).await;
            assert_eq!(
                requests(&seen, "watch").next().unwrap()["session_id"],
                json!(sid())
            );
            view_tx.push([head_reply(head(0, None, false))]);
            remote.wait(|f| f.connected).await;
            remote.line("first");
            let seen = peer.wait(|s| count(s, "submit") == 1).await;
            let submit = requests(&seen, "submit").next().unwrap().clone();
            let turn = request_turn(&submit);
            assert_eq!(request_session(&submit), sid());
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
                let created_head = SessionHead {
                    model: Some(ModelId::parse("test").unwrap()),
                    provider: Some(ProviderId::parse("openai-compatible").unwrap()),
                    effort: None,
                    session_id: sid(),
                    ..head(2, Some(turn), true)
                };
                view_tx.push([
                    head_reply(created_head),
                    entry_reply(user(1, turn, 1, "first")),
                    entry_reply(step(2, turn, 0).text("late answer").sent().entry()),
                ]);
                let shown = remote.wait(|f| f.text.contains("late answer")).await;
                assert_eq!(shown.session, sid());
                assert_eq!(shown.input, "new draft");
                // 正在运行：第一次 Ctrl+C 取消这一轮，窗口内第二次退出。
                remote.ctrl('c');
                remote.ctrl('c');
            } else {
                submit_tx
                    .send(error_reply(local_error(LocalErrorKind::Busy, "late busy")))
                    .unwrap();
                // 迟到的拒绝按后台错误回执显示；输入区已有草稿，不覆盖，原输入从输入历史找回。
                let shown = remote.wait(|f| f.dock.contains("late busy")).await;
                assert_eq!(shown.input, "new draft");
                assert!(shown.dock.contains("Ctrl+↑"), "{}", shown.dock);
                assert_eq!(shown.session, sid());
                assert_eq!(count(&peer.seen.borrow(), "watch"), 1);
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
        assert_eq!(request["turns"], json!(lattice_local::HISTORY_TURNS));
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
            head_reply(SessionHead {
                effort: Some(lattice_local::Effort::High),
                settings_version: settings_version(0x11),
                ..head(2, Some(running), true)
            }),
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
            head_reply(SessionHead {
                effort: Some(lattice_local::Effort::High),
                settings_version: settings_version(0x11),
                ..head(2, Some(running), false)
            }),
            entry_reply(step(2, running, 0).text("done").finished().entry()),
        ]);
        let seen = peer.wait(|s| count(s, "submit") == 1).await;
        let request = requests(&seen, "submit").next().unwrap();
        assert_eq!(request["request"]["input"], "queued via loop");
        assert_eq!(
            request["request"]["expected_settings"],
            json!(settings_version(0x11))
        );
        assert_eq!(request_session(request), sid());
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
            let idle = Duration::from_secs(lattice_local::STREAM_IDLE_TIMEOUT_SECS);
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
        let limit = LOOP_LIMIT + Duration::from_secs(lattice_local::STREAM_IDLE_TIMEOUT_SECS);
        let ((), exit) = bounded(limit, async { tokio::join!(driver, h.run(&peer.client)) }).await;
        assert!(exit.result.is_ok());
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
        let peer = Peer::start(|_| Reply::Never);
        let exit = run_loop(&h.options, &mut h.app, &peer.client, events, |_| {
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

fn model_reply(query: &Value) -> Value {
    let models = if let Some(target) = query["target"].as_str() {
        let (provider, model) = target.split_once('/').unwrap();
        vec![
            json!({"provider":provider,"model":model,"name":null,"context":null,"output":null,"cost":null,"availability":{"status":"ready"},"supported_efforts":["off","low"]}),
        ]
    } else {
        assert_eq!(query, &json!({"list":{"provider":null,"ready_only":true}}));
        vec![
            json!({"provider":"p","model":"ignored","name":null,"context":null,"output":null,"cost":null,"availability":{"status":"ready"},"supported_efforts":[]}),
            json!({"provider":"p","model":"chosen","name":null,"context":null,"output":null,"cost":null,"availability":{"status":"ready"},"supported_efforts":["off","low"]}),
        ]
    };
    response("models", json!({"models":models,"warnings":[]}))
}

#[tokio::test]
async fn model_and_effort_panels_configure_session_and_submit_uses_configured_head() {
    let views = Views::default();
    let view_tx = views.clone();
    let mut configures = 0;
    let peer = Peer::start(move |r| match r["operation"].as_str().unwrap() {
        "watch" => views.subscribe(),
        "models" => Reply::Now(model_reply(&r["query"])),
        "configure" => {
            configures += 1;
            let configured = match configures {
                1 => SessionHead {
                    provider: Some(ProviderId::parse("p").unwrap()),
                    model: Some(ModelId::parse("chosen").unwrap()),
                    effort: Some(lattice_local::Effort::Low),
                    settings_version: settings_version(0x22),
                    ..head(0, None, false)
                },
                _ => SessionHead {
                    provider: Some(ProviderId::parse("p").unwrap()),
                    model: Some(ModelId::parse("chosen").unwrap()),
                    effort: Some(lattice_local::Effort::Off),
                    settings_version: settings_version(0x33),
                    ..head(0, None, false)
                },
            };
            Reply::Now(configure_reply(configured, None))
        }
        "submit" => Reply::Now(error_reply(local_error(
            LocalErrorKind::Busy,
            "chosen busy",
        ))),
        _ => Reply::Never,
    });
    let mut h = Harness::new(true);
    let remote = h.remote.clone();
    let driver = async {
        view_tx.push([head_reply(SessionHead {
            model: None,
            provider: None,
            effort: None,
            settings_version: settings_version(0x10),
            ..head(0, None, false)
        })]);
        remote.wait(|f| f.statusline.contains("unused")).await;
        remote.line("/effort");
        remote.wait(|f| f.panel.contains("先用 /model")).await;
        remote.press(KeyCode::Esc, KeyModifiers::NONE);
        remote.line("/model");
        let seen = peer.wait(|s| count(s, "models") == 1).await;
        assert_eq!(
            requests(&seen, "models").next().unwrap()["query"],
            json!({"list":{"provider":null,"ready_only":true}})
        );
        remote
            .wait(|f| f.panel.contains("p/chosen") && f.panel.contains("p/ignored"))
            .await;
        remote
            .keys
            .unbounded_send(Ok(Event::Paste("P/CHOSEN".into())))
            .unwrap();
        remote
            .wait(|f| f.panel.contains("P/CHOSEN") && !f.panel.contains("p/ignored"))
            .await;
        remote.press(KeyCode::Enter, KeyModifiers::NONE);
        let seen = peer.wait(|s| count(s, "configure") == 1).await;
        let first_cfg = &requests(&seen, "configure").next().unwrap()["request"];
        assert_eq!(first_cfg["session"], json!({"existing": sid()}));
        assert_eq!(first_cfg["model"], json!("p/chosen"));
        assert_eq!(first_cfg["effort"], Value::Null);
        assert_eq!(first_cfg["agent"], Value::Null);
        remote
            .wait(|f| f.statusline.contains("chosen") && !f.panel.contains("MODEL"))
            .await;
        remote.line("/effort");
        let seen = peer.wait(|s| count(s, "models") == 2).await;
        assert_eq!(
            requests(&seen, "models").nth(1).unwrap()["query"],
            json!({"target":"p/chosen"})
        );
        remote
            .wait(|f| f.panel.contains("off") && f.panel.contains("low"))
            .await;
        remote.press(KeyCode::Up, KeyModifiers::NONE);
        remote.press(KeyCode::Enter, KeyModifiers::NONE);
        let seen = peer.wait(|s| count(s, "configure") == 2).await;
        let second_cfg = &requests(&seen, "configure").nth(1).unwrap()["request"];
        assert_eq!(second_cfg["session"], json!({"existing": sid()}));
        assert_eq!(second_cfg["model"], Value::Null);
        assert_eq!(second_cfg["effort"], json!("off"));
        remote
            .wait(|f| f.statusline.contains("off") && !f.panel.contains("EFFORT"))
            .await;
        remote.line("selected prompt");
        let seen = peer.wait(|s| count(s, "submit") == 1).await;
        let request = &requests(&seen, "submit").next().unwrap()["request"];
        assert_eq!(request["session"], json!(sid()));
        assert_eq!(request["input"], "selected prompt");
        assert_eq!(request["expected_settings"], json!(settings_version(0x33)));
        assert!(request.get("model").is_none());
        assert!(request.get("effort").is_none());
        assert!(request.get("agent").is_none());
        let rejected = remote.wait(|f| f.dock.contains("chosen busy")).await;
        assert_eq!(rejected.input, "selected prompt");
        assert!(rejected.statusline.contains("chosen") && rejected.statusline.contains("off"));
        remote.press(KeyCode::Char(' '), KeyModifiers::NONE);
        remote.press(KeyCode::Esc, KeyModifiers::NONE);
        remote.ctrl('d');
    };
    let ((), exit) = bounded(LOOP_LIMIT, async {
        tokio::join!(driver, h.run(&peer.client))
    })
    .await;
    assert!(exit.result.is_ok());
}

#[tokio::test]
async fn held_model_query_keeps_view_cancel_and_exit_responsive() {
    let views = Views::default();
    let view_tx = views.clone();
    let (_hold, held) = oneshot::channel();
    let mut held = Some(held);
    let peer = Peer::start(move |r| match r["operation"].as_str().unwrap() {
        "watch" => views.subscribe(),
        "models" => Reply::Hold(held.take().unwrap()),
        "cancel" => Reply::Now(response("cancelled", json!({"owned":true}))),
        _ => Reply::Never,
    });
    let running = numbered_turn(1);
    let mut h = Harness::new(false);
    let remote = h.remote.clone();
    let driver = async {
        view_tx.push([
            head_reply(head(2, Some(running), true)),
            entry_reply(user(1, running, 1, "running")),
            entry_reply(step(2, running, 0).sent().entry()),
        ]);
        remote.wait(|f| f.phase == TurnPhase::Streaming).await;
        remote.line("/model");
        peer.wait(|s| count(s, "models") == 1).await;
        remote.wait(|f| f.panel.contains("加载中")).await;
        view_tx.push([entry_reply(
            step(2, running, 0)
                .text("progress while query held")
                .sent()
                .entry(),
        )]);
        remote
            .wait(|f| f.text.contains("progress while query held"))
            .await;
        remote.ctrl('c');
        let seen = peer.wait(|s| count(s, "cancel") == 1).await;
        assert_eq!(
            requests(&seen, "cancel").next().unwrap()["turn_id"],
            json!(running)
        );
        remote.ctrl('c');
    };
    let ((), exit) = bounded(LOOP_LIMIT, async {
        tokio::join!(driver, h.run(&peer.client))
    })
    .await;
    assert!(exit.result.is_ok());
    assert_eq!(count(&peer.seen.borrow(), "submit"), 0);
}

#[tokio::test(start_paused = true)]
async fn head_change_retargets_effort_and_ignores_old_results_while_new_query_is_held() {
    use lattice_local::Effort::{Low, Off};
    for old_success in [true, false] {
        let views = Views::default();
        let view_tx = views.clone();
        let (old_tx, old_rx) = oneshot::channel();
        let (new_tx, new_rx) = oneshot::channel();
        let mut queries = [Some(old_rx), Some(new_rx)].into_iter();
        let peer = Peer::start(move |r| match r["operation"].as_str().unwrap() {
            "watch" => views.subscribe(),
            "models" => Reply::Hold(queries.next().unwrap().unwrap()),
            "submit" => Reply::Now(error_reply(local_error(
                LocalErrorKind::Busy,
                "probe captured",
            ))),
            _ => Reply::Never,
        });
        let mut h = Harness::new(false);
        let remote = h.remote.clone();
        let driver = async {
            let mut initial = head(0, None, false);
            initial.provider = Some(ProviderId::parse("p").unwrap());
            initial.model = Some(ModelId::parse("pending").unwrap());
            initial.effort = Some(Low);
            initial.settings_version = settings_version(0x11);
            view_tx.push([head_reply(initial)]);
            remote
                .wait(|f| f.statusline.contains("pending") && f.statusline.contains("low"))
                .await;
            remote.line("/effort");
            let seen = peer.wait(|s| count(s, "models") == 1).await;
            assert_eq!(
                requests(&seen, "models").next().unwrap()["query"],
                json!({"target":"p/pending"})
            );
            remote.press(KeyCode::Char('L'), KeyModifiers::NONE);
            remote
                .wait(|f| f.panel.contains("过滤：L") && f.panel.contains("加载中"))
                .await;
            let current_turn = numbered_turn(2);
            let mut current = head(2, Some(current_turn), false);
            current.agent = "head-c-arrived".into();
            current.provider = Some(ProviderId::parse("other").unwrap());
            current.model = Some(ModelId::parse("current").unwrap());
            current.effort = Some(Off);
            current.settings_version = settings_version(0x22);
            view_tx.push([
                head_reply(current),
                entry_reply(user(1, current_turn, 1, "other client")),
                entry_reply(
                    step(2, current_turn, 0)
                        .text("current answer")
                        .finished()
                        .entry(),
                ),
            ]);
            let seen = peer.wait(|s| count(s, "models") == 2).await;
            assert_eq!(
                requests(&seen, "models").nth(1).unwrap()["query"],
                json!({"target":"other/current"})
            );
            let retargeted = remote
                .wait(|f| {
                    f.statusline.contains("head-c-arrived")
                        && f.statusline.contains("current")
                        && f.statusline.contains("off")
                        && f.text.contains("current answer")
                })
                .await;
            assert!(retargeted.panel.contains("过滤：L"));
            assert!(retargeted.panel.contains("加载中"));
            let old = if old_success {
                let mut result = model_reply(&json!({"target":"p/pending"}));
                result["value"]["models"][0]["supported_efforts"] = json!(["low"]);
                result
            } else {
                error_reply(local_error(
                    LocalErrorKind::Transport,
                    "obsolete exact failure",
                ))
            };
            old_tx.send(old).unwrap();
            // 暂停时钟的处理屏障保证旧响应已处理；新查询仍被 oneshot 持有。
            let waiting = remote.processed().await;
            assert!(waiting.dock.is_empty(), "旧错误不能盖住新面板");
            assert!(
                waiting.panel.contains("EFFORT") && waiting.panel.contains("加载中"),
                "旧成功不能装入候选：{}",
                waiting.panel
            );
            assert!(
                !waiting.panel.lines().any(|line| line.trim() == "low"),
                "新结果到达前不可出现旧档位"
            );
            remote.press(KeyCode::Enter, KeyModifiers::NONE);
            let after_enter = remote.processed().await;
            assert!(
                after_enter.panel.contains("EFFORT") && after_enter.panel.contains("加载中"),
                "加载时 Enter 不选定旧候选"
            );
            assert!(
                after_enter.statusline.contains("off") && !after_enter.statusline.contains("low")
            );
            let mut result = model_reply(&json!({"target":"other/current"}));
            result["value"]["models"][0]["supported_efforts"] = json!(["max"]);
            new_tx.send(result).unwrap();
            remote
                .wait(|f| f.panel.contains("没有匹配项") && f.panel.contains("过滤：L"))
                .await;
            remote.press(KeyCode::Backspace, KeyModifiers::NONE);
            remote
                .wait(|f| f.panel.lines().any(|line| line.trim() == "max"))
                .await;
            remote.press(KeyCode::Esc, KeyModifiers::NONE);
            remote.line("probe inherited choices");
            let seen = peer.wait(|s| count(s, "submit") == 1).await;
            let request = &requests(&seen, "submit").next().unwrap()["request"];
            assert_eq!(request["expected_settings"], json!(settings_version(0x22)));
            remote.wait(|f| f.dock.contains("probe captured")).await;
            remote.press(KeyCode::Char(' '), KeyModifiers::NONE);
            remote.press(KeyCode::Esc, KeyModifiers::NONE);
            remote.ctrl('d');
        };
        let ((), exit) = bounded(LOOP_LIMIT, async {
            tokio::join!(driver, h.run(&peer.client))
        })
        .await;
        assert!(exit.result.is_ok());
    }
}

#[tokio::test]
async fn readonly_open_rejects_mismatched_project_path_before_watch() {
    let sid = sid();
    let other_raw = b"/other/project-\xff";
    let peer = Peer::start(move |request| match request["operation"].as_str() {
        Some("resolve_session") => Reply::Now(json!({
            "response": "session",
            "value": {
                "session_id": sid,
                "project_path": other_raw.as_slice(),
                "current": 0,
                "created_at": "2026-10-17T00:00:00Z",
                "updated_at": "2026-10-17T00:00:01Z"
            }
        })),
        Some("log") => Reply::Now(json!({
            "response": "log",
            "value": {
                "session": {
                    "session_id": sid,
                    "project_path": other_raw.as_slice(),
                    "current": 0,
                    "created_at": "2026-10-17T00:00:00Z",
                    "updated_at": "2026-10-17T00:00:01Z"
                },
                "current_agent": "coder",
                "unclosed_turn": null,
                "nodes": []
            }
        })),
        _ => Reply::Never,
    });
    let current = tempfile::tempdir().expect("创建当前工作区临时目录");
    let workspace = std::fs::canonicalize(current.path()).expect("解析当前工作区");
    let options = crate::TuiOptions {
        session_id: Some("abcd".into()),
        agent: None,
        session_dir: current.path().join("state"),
        project_root: workspace.clone(),
    };
    let error = match crate::session::open_or_create(&options, &workspace, &peer.client).await {
        Ok(_) => panic!("只读打开属于另一个项目的会话必须在 watch 之前失败"),
        Err(error) => error,
    };
    let seen = peer.wait(|seen| count(seen, "resolve_session") == 1).await;
    assert_eq!(count(&seen, "watch"), 0, "项目路径不一致时不得开始 watch");
    assert_eq!(count(&seen, "head"), 0, "项目路径不一致时不得读取 head");
    assert_eq!(
        count(&seen, "log"),
        0,
        "项目路径不一致时应在读取 log 之前退出"
    );
    let local = error
        .downcast_ref::<lattice_local::LocalError>()
        .expect("启动错误应保持为可被 CLI 分类输出的 LocalError");
    assert_eq!(local.kind, lattice_local::LocalErrorKind::Conflict);
    assert_eq!(
        local.conflict,
        Some(lattice_local::ConflictReason::BindingMismatch)
    );
    assert!(
        local.message.contains("/other/project-\u{FFFD}"),
        "错误消息应使用中文写明会话所属项目的有损路径: {}",
        local.message
    );
}

#[tokio::test]
async fn configure_in_flight_enqueues_enter_and_sends_with_configured_version() {
    for c1_succeed in [true, false] {
        let views = Views::default();
        let view_tx = views.clone();
        let (cfg1_tx, cfg1_rx) = oneshot::channel();
        let (cfg2_tx, cfg2_rx) = oneshot::channel();
        let mut cfgs = [Some(cfg1_rx), Some(cfg2_rx)].into_iter();
        let peer = Peer::start(move |r| match r["operation"].as_str().unwrap() {
            "watch" => views.subscribe(),
            "models" => Reply::Now(model_reply(&r["query"])),
            "configure" => Reply::Hold(cfgs.next().unwrap().unwrap()),
            "submit" => Reply::Now(error_reply(LocalError {
                conflict: Some(lattice_local::ConflictReason::SettingsChanged),
                ..local_error(
                    LocalErrorKind::Conflict,
                    "session settings changed before turn acceptance",
                )
            })),
            _ => Reply::Never,
        });
        let mut h = Harness::new(true);
        let remote = h.remote.clone();
        let driver = async {
            view_tx.push([head_reply(SessionHead {
                settings_version: settings_version(0x10),
                ..head(0, None, false)
            })]);
            remote.wait(|f| f.statusline.contains("unused")).await;

            // 第一次 /model 选择并发出在途 configure C1。
            remote.line("/model");
            remote.wait(|f| f.panel.contains("p/chosen")).await;
            remote
                .keys
                .unbounded_send(Ok(Event::Paste("p/chosen".into())))
                .unwrap();
            remote
                .wait(|f| f.panel.contains("p/chosen") && !f.panel.contains("p/ignored"))
                .await;
            remote.press(KeyCode::Enter, KeyModifiers::NONE);
            peer.wait(|s| count(s, "configure") == 1).await;

            // C1 在途时 Dock 提示 Enter 排队，按 Enter 入队并绑定 C1 的代次。
            remote.wait(|f| f.panel.contains("Enter 排队")).await;
            remote.line("queued during configure");
            let queued_frame = remote.wait(|f| f.text.contains("排队")).await;
            assert_eq!(
                count(&peer.seen.borrow(), "submit"),
                0,
                "configure 在途不能直接提交"
            );
            assert!(queued_frame.input.is_empty());

            // C1 回复前再次 /effort 发出 C2；消息不改绑到 C2，面板与状态行只采用最新代次 C2。
            remote.line("/effort");
            remote.wait(|f| f.panel.contains("off")).await;
            remote.press(KeyCode::Enter, KeyModifiers::NONE);
            peer.wait(|s| count(s, "configure") == 2).await;

            if c1_succeed {
                cfg1_tx
                    .send(configure_reply(
                        SessionHead {
                            provider: Some(ProviderId::parse("p").unwrap()),
                            model: Some(ModelId::parse("chosen").unwrap()),
                            settings_version: settings_version(0x99),
                            ..head(0, None, false)
                        },
                        None,
                    ))
                    .unwrap();
                let after_c1 = remote.processed().await;
                assert_eq!(
                    count(&peer.seen.borrow(), "submit"),
                    0,
                    "C2 仍在途时不能提前出队提交"
                );
                assert!(
                    !after_c1.statusline.contains("chosen"),
                    "面板与状态行不采用非最新代次 C1 的回复"
                );

                cfg2_tx
                    .send(configure_reply(
                        SessionHead {
                            provider: Some(ProviderId::parse("p").unwrap()),
                            model: Some(ModelId::parse("chosen").unwrap()),
                            effort: Some(lattice_local::Effort::Off),
                            settings_version: settings_version(0x44),
                            ..head(0, None, false)
                        },
                        None,
                    ))
                    .unwrap();
                let seen = peer.wait(|s| count(s, "submit") == 1).await;
                let submit = &requests(&seen, "submit").next().unwrap()["request"];
                assert_eq!(submit["input"], "queued during configure");
                assert_eq!(
                    submit["expected_settings"],
                    json!(settings_version(0x99)),
                    "排队消息必须携带入队时绑定的 C1 版本发出"
                );
                let rejected = remote.wait(|f| f.dock.contains("会话设置已变更")).await;
                assert!(
                    rejected.statusline.contains("chosen") && rejected.statusline.contains("off"),
                    "状态行应采用最新代次 C2 的回复：{}",
                    rejected.statusline
                );
                assert!(
                    rejected.text.contains("暂停")
                        && rejected.text.contains("queued during configure"),
                    "后台返回 SettingsChanged 后消息应保留在暂停的队列中：{}",
                    rejected.text
                );
                remote.press(KeyCode::Char(' '), KeyModifiers::NONE);
            } else {
                cfg1_tx
                    .send(error_reply(local_error(
                        LocalErrorKind::BadRequest,
                        "bad model choice",
                    )))
                    .unwrap();
                let failed = remote.wait(|f| f.dock.contains("bad model choice")).await;
                assert_eq!(
                    count(&peer.seen.borrow(), "submit"),
                    0,
                    "绑定的 C1 失败时排队消息不能发出"
                );
                assert!(
                    failed.text.contains("暂停") && failed.text.contains("queued during configure"),
                    "C1 失败时消息应留在暂停的队列中：{}",
                    failed.text
                );
                cfg2_tx
                    .send(configure_reply(
                        SessionHead {
                            provider: Some(ProviderId::parse("p").unwrap()),
                            model: Some(ModelId::parse("chosen").unwrap()),
                            effort: Some(lattice_local::Effort::Off),
                            settings_version: settings_version(0x44),
                            ..head(0, None, false)
                        },
                        None,
                    ))
                    .unwrap();
                let after_c2 = remote
                    .wait(|f| f.statusline.contains("chosen") && f.statusline.contains("off"))
                    .await;
                assert_eq!(
                    count(&peer.seen.borrow(), "submit"),
                    0,
                    "后续 C2 成功也不能自动发出已因 C1 失败而暂停的消息"
                );
                assert!(
                    after_c2.text.contains("暂停")
                        && after_c2.text.contains("queued during configure"),
                    "消息仍留在暂停的队列中：{}",
                    after_c2.text
                );
                remote.press(KeyCode::Char(' '), KeyModifiers::NONE);
            }
            remote.press(KeyCode::Esc, KeyModifiers::NONE);
            remote.ctrl('d');
        };
        let ((), exit) = bounded(LOOP_LIMIT, async {
            tokio::join!(driver, h.run(&peer.client))
        })
        .await;
        assert!(exit.result.is_ok());
        assert!(h.app.queue.is_paused(), "拒绝或失败后队列应保持暂停");
    }
}

#[tokio::test]
async fn submit_before_first_watch_head_carries_startup_head_version() {
    let dir = tempfile::tempdir().expect("创建工作区目录");
    let workspace = std::fs::canonicalize(dir.path()).expect("解析工作区");
    let raw_cwd = std::os::unix::ffi::OsStrExt::as_bytes(workspace.as_os_str()).to_vec();
    let views = Views::default();
    let view_tx = views.clone();
    let peer = Peer::start(move |r| match r["operation"].as_str() {
        Some("resolve_session") => Reply::Now(json!({
            "response": "session",
            "value": {
                "session_id": sid(),
                "project_path": raw_cwd.as_slice(),
                "current": 0,
                "created_at": "2026-10-17T00:00:00Z",
                "updated_at": "2026-10-17T00:00:01Z"
            }
        })),
        Some("head") => Reply::Now(response(
            "head",
            serde_json::to_value(SessionHead {
                agent: "readonly-agent".into(),
                settings_version: settings_version(0x77),
                ..head(0, None, false)
            })
            .unwrap(),
        )),
        Some("watch") => views.subscribe(),
        Some("submit") => Reply::Now(accepted_reply(r)),
        _ => Reply::Never,
    });
    let options = crate::TuiOptions {
        session_id: Some(SESSION.into()),
        agent: None,
        session_dir: dir.path().join("state"),
        project_root: workspace.clone(),
    };
    let ctx = crate::session::open_or_create(&options, &workspace, &peer.client)
        .await
        .expect("只读打开已有会话应读取启动会话头");
    {
        let seen = peer.seen.borrow();
        assert_eq!(count(&seen, "resolve_session"), 1);
        assert_eq!(count(&seen, "head"), 1);
        assert_eq!(count(&seen, "configure"), 0);
        assert_eq!(count(&seen, "watch"), 0);
    }
    assert_eq!(ctx.initial_head.settings_version, settings_version(0x77));

    let mut h = Harness::with_context(options, ctx, dir);
    let remote = h.remote.clone();
    let driver = async {
        peer.wait(|s| count(s, "watch") == 1).await;
        remote
            .wait(|f| f.statusline.contains("readonly-agent"))
            .await;

        // 推迟首个 watch 会话头到达，此时按 Enter 立即提交。
        remote.line("before first watch head");
        let seen = peer.wait(|s| count(s, "submit") == 1).await;
        let submit_req = requests(&seen, "submit").next().unwrap();
        let accepted_turn = request_turn(submit_req);
        assert_eq!(submit_req["request"]["input"], "before first watch head");
        assert_eq!(
            submit_req["request"]["expected_settings"],
            json!(settings_version(0x77)),
            "首个 watch 会话头到达前发出的提交必须携带启动时会话头的版本"
        );

        view_tx.push([
            head_reply(SessionHead {
                agent: "readonly-agent".into(),
                settings_version: settings_version(0x77),
                ..head(2, Some(accepted_turn), false)
            }),
            entry_reply(user(1, accepted_turn, 1, "before first watch head")),
            entry_reply(step(2, accepted_turn, 0).text("done").finished().entry()),
        ]);
        remote
            .wait(|f| f.phase == TurnPhase::Idle && f.text.contains("done"))
            .await;
        remote.ctrl('d');
    };
    let ((), exit) = bounded(LOOP_LIMIT, async {
        tokio::join!(driver, h.run(&peer.client))
    })
    .await;
    assert!(exit.result.is_ok());
}

#[tokio::test]
async fn queued_message_uses_enqueued_settings_version_across_later_head_pushes() {
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
            head_reply(SessionHead {
                settings_version: settings_version(0x21),
                ..head(2, Some(running), true)
            }),
            entry_reply(user(1, running, 1, "running prompt")),
            entry_reply(step(2, running, 0).sent().entry()),
        ]);
        remote.wait(|f| f.phase == TurnPhase::Streaming).await;
        remote.line("enqueued at 0x21");
        remote.wait(|f| f.text.contains("排队")).await;

        // 另一客户端在这一轮完成前后修改了会话设置，推送了新版本 0x22。
        view_tx.push([
            head_reply(SessionHead {
                settings_version: settings_version(0x22),
                ..head(2, Some(running), false)
            }),
            entry_reply(step(2, running, 0).text("done").finished().entry()),
        ]);
        let seen = peer.wait(|s| count(s, "submit") == 1).await;
        let submit = &requests(&seen, "submit").next().unwrap()["request"];
        assert_eq!(submit["input"], "enqueued at 0x21");
        assert_eq!(
            submit["expected_settings"],
            json!(settings_version(0x21)),
            "排队消息必须使用入队时记录的会话头版本，不被之后的推送改写"
        );
        remote.ctrl('c');
        remote.ctrl('c');
    };
    let ((), exit) = bounded(LOOP_LIMIT, async {
        tokio::join!(driver, h.run(&peer.client))
    })
    .await;
    assert!(exit.result.is_ok());
}

#[tokio::test]
async fn startup_configure_and_open_modes_enforce_configure_and_busy_before_watch() {
    let dir = tempfile::tempdir().expect("创建工作区目录");
    let workspace = std::fs::canonicalize(dir.path()).expect("解析工作区");

    // 1. 新建会话 configure(New) 失败时以启动错误退出，不开始 watch。
    let fail_peer = Peer::start(|r| match r["operation"].as_str() {
        Some("configure") => Reply::Now(error_reply(local_error(
            LocalErrorKind::BadRequest,
            "agent profile missing",
        ))),
        _ => Reply::Never,
    });
    let new_opts = crate::TuiOptions {
        session_id: None,
        agent: Some(lattice_local::AgentName::parse("coder").unwrap()),
        session_dir: dir.path().join("state"),
        project_root: workspace.clone(),
    };
    let err = match crate::session::open_or_create(&new_opts, &workspace, &fail_peer.client).await {
        Ok(_) => panic!("新建会话 configure 失败必须以启动错误退出"),
        Err(err) => err,
    };
    let seen = fail_peer.wait(|s| count(s, "configure") == 1).await;
    assert_eq!(count(&seen, "watch"), 0);
    let cfg_req = &requests(&seen, "configure").next().unwrap()["request"];
    assert!(cfg_req["session"].get("new").is_some());
    assert_eq!(cfg_req["agent"], json!("coder"));
    let local = err.downcast_ref::<LocalError>().expect("保持 LocalError");
    assert!(local.message.contains("agent profile missing"));

    // 2. 新建会话 configure(New) 成功时返回初始会话头与 adjustment。
    let ok_peer = Peer::start(|r| match r["operation"].as_str() {
        Some("configure") => {
            let new_id: SessionId =
                serde_json::from_value(r["request"]["session"]["new"].clone()).unwrap();
            let mut h = head(0, None, false);
            h.session_id = new_id;
            h.agent = "coder".into();
            Reply::Now(configure_reply(
                h,
                Some(lattice_local::EffortAdjustment {
                    from: Some(lattice_local::Effort::High),
                    to: Some(lattice_local::Effort::Low),
                }),
            ))
        }
        _ => Reply::Never,
    });
    let created_ctx = crate::session::open_or_create(&new_opts, &workspace, &ok_peer.client)
        .await
        .expect("configure(New) 成功");
    assert_eq!(created_ctx.initial_head.agent.as_str(), "coder");
    assert!(created_ctx.initial_adjustment.is_some());

    // 3. `lattice tui S --agent A` 遇到 Busy 时写明会话正在执行一轮、稍后再试，并退出。
    let raw_cwd = std::os::unix::ffi::OsStrExt::as_bytes(workspace.as_os_str()).to_vec();
    let busy_peer = Peer::start(move |r| match r["operation"].as_str() {
        Some("resolve_session") => Reply::Now(json!({
            "response": "session",
            "value": {
                "session_id": sid(),
                "project_path": raw_cwd.as_slice(),
                "current": 2,
                "created_at": "2026-10-17T00:00:00Z",
                "updated_at": "2026-10-17T00:00:01Z"
            }
        })),
        Some("configure") => Reply::Now(error_reply(local_error(
            LocalErrorKind::Busy,
            "session is busy",
        ))),
        _ => Reply::Never,
    });
    let existing_agent_opts = crate::TuiOptions {
        session_id: Some("11111111".into()),
        agent: Some(lattice_local::AgentName::parse("reviewer").unwrap()),
        session_dir: dir.path().join("state"),
        project_root: workspace.clone(),
    };
    let busy_err =
        match crate::session::open_or_create(&existing_agent_opts, &workspace, &busy_peer.client)
            .await
        {
            Ok(_) => panic!("已有会话 configure Busy 必须以启动错误退出"),
            Err(err) => err,
        };
    let seen = busy_peer.wait(|s| count(s, "configure") == 1).await;
    assert_eq!(count(&seen, "watch"), 0);
    assert_eq!(
        requests(&seen, "configure").next().unwrap()["request"]["session"],
        json!({"existing": sid()})
    );
    let busy_local = busy_err
        .downcast_ref::<LocalError>()
        .expect("保持 LocalError");
    assert_eq!(busy_local.kind, LocalErrorKind::Busy);
    assert!(
        busy_local.message.contains("正在执行一轮") && busy_local.message.contains("稍后再试"),
        "Busy 错误应说明会话正在执行一轮、稍后再试：{}",
        busy_local.message
    );
}

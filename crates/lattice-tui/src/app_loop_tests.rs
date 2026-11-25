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
    assert!(!h.app.drafts.is_editing());
    let mut h = Harness::new(false);
    let id = lattice_local::new_turn_id();
    comment_history(&mut h.app, id, parts, true);
    comment_browse(&mut h.app, "comment target");
    h.app.apply_action(KeyAction::Submit);
    assert!(
        h.app.drafts.is_editing(),
        "已关闭轮次的正文应打开评论编辑器"
    );
    h.app.apply_action(KeyAction::Paste("note".into()));
    h.app.apply_action(KeyAction::Submit);
    h.app.focus = Focus::Dock;
    h.app.input.set_text(" \n\t");
    let req = h.app.apply_action(KeyAction::Submit).unwrap();
    assert_eq!(req.session, sid());
    assert_eq!(req.workspace, h.options.project_root);
    assert_eq!(req.expected_settings, Some(settings_version(0x11)));
    let [UserInputPart::Text { text: wire }] = &req.inputs[0].parts[..] else {
        panic!("提交请求应只含一段文本");
    };
    let message = crate::comment::Message::parse(wire).unwrap();
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
    assert!(app.drafts.is_editing());
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
    assert_eq!(app.drafts.draft_count(), 0, "正在写的新评论不计入待发送");
    app.apply_action(KeyAction::Clear);
    assert!(!transcript(app).contains("new discarded"));
    save_comment(app, "saved note");
    assert_eq!(app.drafts.draft_count(), 1);
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
    assert_eq!(app.drafts.draft_count(), 1);
    comment_browse(app, "saved note");
    app.apply_action(KeyAction::Delete);
    assert_eq!(app.drafts.draft_count(), 0);
    save_comment(app, " \n\t");
    assert_eq!(app.drafts.draft_count(), 0);
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
fn comment_manual_rejections_restore_draft_and_in_flight_comments_reserve_numbers() {
    for how in 0..4 {
        let mut h = comment_app();
        let app = &mut h.app;
        save_comment(app, "rejected note");
        app.focus = Focus::Dock;
        app.input.set_text("body");
        let req = app.apply_action(KeyAction::Submit).unwrap();
        assert_eq!(req.session, sid());
        assert_eq!(req.workspace, h.options.project_root);
        assert_eq!(req.expected_settings, Some(settings_version(0x11)));
        let [UserInputPart::Text { text: wire }] = &req.inputs[0].parts[..] else {
            panic!("提交请求应只含一段文本");
        };
        assert_eq!(crate::comment::Message::parse(wire).unwrap().text, "body");
        assert_eq!(app.history.prev(), Some("body"));
        // 提交在途（无乐观块）期间新建评论草稿，编号必须避开在途的 #1。
        save_comment(app, "in-flight second");
        assert_eq!(
            app.drafts.drafts().next().unwrap().comment().n,
            2,
            "在途提交携带的评论 #1 必须占用编号，避免被拒归还后撞号"
        );
        let id = app.own_turn.unwrap();
        reject(app, id, how);
        assert_eq!(app.input.text(), "body", "{how}");
        let text = transcript(app);
        assert_eq!(
            text.matches("[COMMENT 1]").count(),
            0,
            "A1 无乐观被拒 USER 块"
        );
        assert_eq!(text.matches("rejected note").count(), 1, "{how}");
        assert!(text.contains("草稿 · #1"), "{how}: {text}");
        assert!(text.contains("草稿 · #2"), "{how}: {text}");
        comment_browse(app, "rejected note");
        app.apply_action(KeyAction::Submit);
        assert!(app.drafts.is_editing(), "归还的草稿应仍可编辑");
        app.apply_action(KeyAction::Clear);
        comment_browse(app, "rejected note");
        app.apply_action(KeyAction::Delete);
        save_comment(app, "next note");
        assert_eq!(
            app.drafts
                .drafts()
                .map(|draft| draft.comment().n)
                .max()
                .unwrap(),
            3,
            "在途期间新建的 #2 仍在草稿中，后续评论应递增到 #3"
        );
    }
}

#[test]
fn comment_only_unknown_then_not_accepted_does_not_claim_input_restored() {
    for rewound in [false, true] {
        let mut h = comment_app();
        let app = &mut h.app;
        let target = app.head.as_ref().unwrap().turn_id.unwrap();
        save_comment(app, "comment only");
        app.focus = Focus::Dock;
        app.submit().expect("仅含评论的提交应返回请求");
        let id = app.own_turn.unwrap();
        app.apply_submission(receipt(id, SubmissionResult::Unknown { error: None }));
        release_dock(app);
        app.input.set_text("新草稿");
        if rewound {
            rewind_to_user(app, target);
        }
        reject(app, id, 3);
        assert_eq!(app.input.text(), "新草稿", "已有新草稿不应被覆盖");
        assert_eq!(
            app.drafts.draft_count(),
            usize::from(!rewound),
            "锚点存在时恢复评论草稿，退回后丢弃评论草稿"
        );
        if rewound {
            assert!(
                transcript(app).contains("锚点 Step 不在当前历史，丢弃评论：#1"),
                "退回后丢弃评论的提示应保留在对话记录中"
            );
        }
        let err = app.dock.message().expect("查询确认未被接受应暂驻 Error");
        assert_eq!(err.level, NoticeLevel::Error);
        assert!(
            err.text.contains("确认后台没有这个请求")
                && !err.text.contains("输入已保留")
                && !err.text.contains("Ctrl+↑")
                && !err.text.contains("图片未放回"),
            "仅含评论的提交在查询确认未被接受时不应提示输入已保留：{text}",
            text = err.text
        );
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
            .turn_ordinal(node / 2 + 1)
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
    let req = source.app.submit().unwrap();
    assert_eq!(req.session, sid());
    assert_eq!(req.workspace, source.options.project_root);
    assert_eq!(req.expected_settings, Some(settings_version(0x11)));
    let [UserInputPart::Text { text: wire }] = &req.inputs[0].parts[..] else {
        panic!("提交请求应只含一段文本");
    };
    let original = crate::comment::Message::parse(wire).unwrap();
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
    let req = source.app.submit().unwrap();
    assert_eq!(req.session, sid());
    assert_eq!(req.workspace, source.options.project_root);
    assert_eq!(req.expected_settings, Some(settings_version(0x11)));
    let [UserInputPart::Text { text: wire }] = &req.inputs[0].parts[..] else {
        panic!("提交请求应只含一段文本");
    };
    let target = crate::comment::Message::parse(wire).unwrap().comments[0]
        .location
        .fragment
        .turn;
    let mut h = Harness::new(false);
    let app = &mut h.app;
    app.term_width = 80;
    app.term_height = 12;
    let last = lattice_local::new_turn_id();
    let mut h = head(8, Some(last), false);
    h.has_older = Some(true);
    open_view(
        app,
        h,
        vec![
            user(5, target, 3, "prompt-3"),
            step(6, target, 0)
                .turn_ordinal(3)
                .text("comment target\n\nlater paragraph")
                .finished()
                .entry(),
            user(7, last, 4, wire),
            step(8, last, 0)
                .turn_ordinal(4)
                .text("answer-4")
                .finished()
                .entry(),
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
            has_more: false,
        }),
    );
    assert_eq!(top(app), before, "视图停在原来看到的那一行");
}

/// CLI 退回到被评论那一轮的前置 Input 节点：被评论的 Step 已不在当前路径。
fn rewind_to_user(app: &mut App, target: TurnId) {
    let rewound = SessionHead {
        path_replaced: true,
        ..head(1, None, false)
    };
    app.apply_view(LocalUpdate::Head(rewound));
    app.apply_view(entry_update(user(1, target, 1, "original prompt")));
}

#[test]
fn comment_missing_step_discards_drafts_and_active_editor() {
    let mut h = comment_app();
    let app = &mut h.app;
    save_comment(app, "saved draft");
    comment_browse(app, "comment target");
    app.apply_action(KeyAction::Submit);
    app.apply_action(KeyAction::Paste("editing draft".into()));
    assert!(app.drafts.is_editing());
    let target = app
        .drafts
        .drafts()
        .next()
        .unwrap()
        .comment()
        .location
        .fragment
        .turn;
    rewind_to_user(app, target);
    assert_eq!(app.drafts.draft_count(), 0);
    assert!(!app.drafts.is_editing());
    assert_eq!(
        app.dock.message(),
        Some(&status(
            NoticeLevel::Warn,
            "锚点 Step 不在当前历史，丢弃评论：#1、#2"
        ))
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
        assert_eq!(app.drafts.drafts().next().unwrap().comment().text, "a b");
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
    assert!(!h.app.drafts.is_editing());
    h.app.focus = Focus::Dock;
    h.app.input.set_text("run");
    h.app.submit().unwrap();
    let live = h.app.own_turn.unwrap();
    h.app
        .apply_submission(receipt(live, accepted_at(live, &[3], 4)));
    h.app
        .apply_view(LocalUpdate::Head(head(4, Some(live), true)));
    h.app.apply_view(entry_update(user(3, live, 2, "run")));
    h.app.apply_view(entry_update(
        step(4, live, 0).turn_ordinal(2).sent().entry(),
    ));
    h.app
        .apply_view(text(4, 0, 0, &[("live comment target", false)]));
    comment_browse(&mut h.app, "live comment target");
    h.app.apply_action(KeyAction::Submit);
    assert!(!h.app.drafts.is_editing());
}

#[test]
fn comment_numbering_continues_from_history_and_drafts() {
    let mut source = comment_app();
    save_comment(&mut source.app, "history note");
    source.app.focus = Focus::Dock;
    let source_req = source.app.submit().unwrap();
    let [UserInputPart::Text { text: source_wire }] = &source_req.inputs[0].parts[..] else {
        panic!("提交请求应只含一段文本");
    };
    let mut message = crate::comment::Message::parse(source_wire).unwrap();
    let target = message.comments[0].location.fragment.turn;
    message.comments[0].n = 7;
    let mut h = Harness::new(false);
    let app = &mut h.app;
    reload_comment(app, target, &message.encode(), true);
    save_comment(app, "draft eight");
    save_comment(app, "draft nine");
    app.focus = Focus::Dock;
    let out_req = app.submit().unwrap();
    assert_eq!(out_req.session, sid());
    assert_eq!(out_req.workspace, h.options.project_root);
    assert_eq!(out_req.expected_settings, Some(settings_version(0x11)));
    let [UserInputPart::Text { text: out_wire }] = &out_req.inputs[0].parts[..] else {
        panic!("提交请求应只含一段文本");
    };
    let outgoing = crate::comment::Message::parse(out_wire).unwrap();
    assert_eq!(
        outgoing.comments.iter().map(|c| c.n).collect::<Vec<_>>(),
        [8, 9]
    );
    assert_eq!(app.history.prev(), None, "仅含评论的提交不应写入输入历史");
}

fn submit_comment_wire(app: &mut App, comment_text: &str) -> (TurnId, String) {
    save_comment(app, comment_text);
    app.focus = Focus::Dock;
    let req = app.submit().expect("提交评论草稿应生成 TurnRequest");
    let [UserInputPart::Text { text: wire }] = &req.inputs[0].parts[..] else {
        panic!("评论提交请求应只含一段文本");
    };
    (req.turn_id, wire.clone())
}

fn delete_draft_by_text(app: &mut App, text: &str) {
    comment_browse(app, text);
    app.apply_action(KeyAction::Delete);
}

#[test]
fn comment_accepted_reserves_number_until_input_syncs_and_prunes() {
    let mut h = comment_app();
    let app = &mut h.app;
    let (live, wire) = submit_comment_wire(app, "accepted note");

    // 收到 Accepted 回执，但视图尚未同步 Input / Step 0。
    app.apply_submission(receipt(live, accepted_at(live, &[3], 4)));
    assert!(app.submitted.is_none(), "已接受提交应从 submitted 移出");
    assert!(
        app.accepted_comments.contains_key(&live),
        "Input 条目同步前应在 accepted_comments 中继续保留编号占用"
    );
    assert_eq!(app.drafts.draft_count(), 0, "已接受的评论不应回到草稿");

    // 同步窗口期新建评论 B，编号必须为 #2。
    save_comment(app, "draft during sync window");
    assert_eq!(
        app.drafts
            .draft_comments()
            .iter()
            .map(|c| c.n)
            .collect::<Vec<_>>(),
        vec![2],
        "Accepted 后 Input 未到达时新建评论必须避开已接受的 #1"
    );

    // Input 与 Step 0 到达后，由 Block::CommentedUser 接管，accepted_comments 被裁剪释放。
    app.apply_view(LocalUpdate::Head(head(4, Some(live), true)));
    app.apply_view(entry_update(user(3, live, 2, &wire)));
    app.apply_view(entry_update(
        step(4, live, 0).turn_ordinal(2).finished().entry(),
    ));
    assert!(
        app.accepted_comments.is_empty(),
        "Input 条目同步进视图后应释放 accepted_comments 占位"
    );

    // 保留草稿 #2 时继续新建评论 C 应得到 #3。
    save_comment(app, "draft three");
    assert_eq!(
        app.drafts
            .draft_comments()
            .iter()
            .map(|c| c.n)
            .collect::<Vec<_>>(),
        vec![2, 3],
        "保留草稿 #2 时新建评论应递增到 #3"
    );

    // 删除草稿 #3 与 #2 后再新建评论，由视图中已发送的 #1 接管，仍从 #2 起算。
    delete_draft_by_text(app, "draft three");
    delete_draft_by_text(app, "draft during sync window");
    assert_eq!(app.drafts.draft_count(), 0, "草稿应已全部删除");
    save_comment(app, "draft after takeover");
    assert_eq!(
        app.drafts
            .draft_comments()
            .iter()
            .map(|c| c.n)
            .collect::<Vec<_>>(),
        vec![2],
        "accepted_comments 释放后仍由 Block::CommentedUser 占用 #1，新评论应得 #2"
    );
}

#[test]
fn comment_unknown_resolved_to_accepted_reserves_number_via_receipt_and_handoff() {
    for via_handoff in [false, true] {
        let mut h = comment_app();
        let app = &mut h.app;
        let (live, wire) = submit_comment_wire(app, "unknown then accepted");

        app.apply_submission(receipt(live, SubmissionResult::Unknown { error: None }));
        release_dock(app);
        assert!(
            app.unknown_submission.is_some(),
            "结果未知时应先暂存在 unknown_submission"
        );
        assert!(app.accepted_comments.is_empty());

        let accepted_receipt = SubmissionReceipt {
            turn_id: live,
            result: accepted_at(live, &[3], 4),
            recovered: true,
        };
        if via_handoff {
            app.apply_handoff(DetachReport {
                session_id: sid(),
                turn_id: Some(live),
                submission: Some(accepted_receipt),
                submission_diagnostic: None,
                cancel: None,
                cancel_error: None,
                head: None,
                summary: None,
            });
        } else {
            app.apply_submission(LocalUpdate::Submit {
                update: SubmissionUpdate::Receipt(accepted_receipt),
            });
        }

        assert!(
            app.unknown_submission.is_none(),
            "确定已接受后应移出 unknown_submission（via_handoff={via_handoff}）"
        );
        assert!(
            app.accepted_comments.contains_key(&live),
            "Unknown 转为 Accepted 后 Input 未到达前应保留在 accepted_comments（via_handoff={via_handoff}）"
        );
        save_comment(app, "second after recovery");
        assert_eq!(
            app.drafts
                .draft_comments()
                .iter()
                .map(|c| c.n)
                .collect::<Vec<_>>(),
            vec![2],
            "Unknown 恢复为 Accepted 后视图尚未同步前新建评论应得 #2（via_handoff={via_handoff}）"
        );

        app.apply_view(LocalUpdate::Head(head(4, Some(live), false)));
        app.apply_view(entry_update(user(3, live, 2, &wire)));
        app.apply_view(entry_update(
            step(4, live, 0).turn_ordinal(2).finished().entry(),
        ));
        assert!(
            app.accepted_comments.is_empty(),
            "对应 Input 同步进视图后应裁剪释放（via_handoff={via_handoff}）"
        );
    }
}

#[test]
fn comment_accepted_survives_disconnect_and_reconnect_on_same_path() {
    let mut h = comment_app();
    let app = &mut h.app;
    let t1 = app.head.as_ref().unwrap().turn_id.unwrap();
    let (live, wire) = submit_comment_wire(app, "accepted before eof");

    app.apply_submission(receipt(live, accepted_at(live, &[3], 4)));
    app.apply_view(LocalUpdate::Connection {
        status: ConnectionStatus::Eof,
    });
    assert!(
        app.accepted_comments.contains_key(&live),
        "断线时不得清空 accepted_comments"
    );

    // 断线期间新建评论得到 #2。
    save_comment(app, "during disconnect");
    assert_eq!(
        app.drafts
            .draft_comments()
            .iter()
            .map(|c| c.n)
            .collect::<Vec<_>>(),
        vec![2],
        "断线期间新建评论应避开已接受的 #1"
    );

    // 发起重连：首条会话头到达后处于 replace_pending，此时绝不能因旧条目或状态提前裁剪。
    app.begin_view();
    app.apply_view(LocalUpdate::Head(head(4, Some(live), false)));
    assert!(
        app.replace_pending,
        "新视图首个条目到达前应处于 replace_pending"
    );
    assert!(
        app.accepted_comments.contains_key(&live),
        "replace_pending 期间必须保留 accepted_comments"
    );
    delete_draft_by_text(app, "during disconnect");
    save_comment(app, "during replace pending");
    assert_eq!(
        app.drafts
            .draft_comments()
            .iter()
            .map(|c| c.n)
            .collect::<Vec<_>>(),
        vec![2],
        "replace_pending 期间新建评论仍应得到 #2"
    );

    // 初始条目流式到达：t1 的条目先到，live 的 Input 尚未到，编号仍不丢。
    app.apply_view(entry_update(user(1, t1, 1, "original prompt")));
    app.apply_view(entry_update(
        step(2, t1, 0).text("comment target").finished().entry(),
    ));
    assert!(
        !app.replace_pending,
        "首个初始条目到达后应退出 replace_pending"
    );
    assert!(
        app.accepted_comments.contains_key(&live),
        "重连后对应 Input 尚未到达前必须保留 accepted_comments"
    );
    delete_draft_by_text(app, "during replace pending");
    save_comment(app, "during streaming initial");
    assert_eq!(
        app.drafts
            .draft_comments()
            .iter()
            .map(|c| c.n)
            .collect::<Vec<_>>(),
        vec![2],
        "初始条目流式到达且 Input 未到前，新建评论仍应得到 #2"
    );

    // live 的 Input 与 Step 到达后由 Block::CommentedUser 接管并释放 accepted_comments。
    app.apply_view(entry_update(user(3, live, 2, &wire)));
    app.apply_view(entry_update(
        step(4, live, 0).turn_ordinal(2).finished().entry(),
    ));
    assert!(
        app.accepted_comments.is_empty(),
        "重连后 Input 到达应由 Block::CommentedUser 接管并释放 accepted_comments"
    );
    delete_draft_by_text(app, "during streaming initial");
    save_comment(app, "after reconnect takeover");
    assert_eq!(
        app.drafts
            .draft_comments()
            .iter()
            .map(|c| c.n)
            .collect::<Vec<_>>(),
        vec![2],
        "接管释放后视图中的 #1 继续占位，新评论应得 #2"
    );
}

#[test]
fn comment_accepted_released_without_restore_on_rewound_reconnect() {
    // 子场景 1：!has_older，重连后会话已回退到该轮之前。
    let mut h = Harness::new(false);
    h.app.term_width = 100;
    h.app.term_height = 40;
    let app = &mut h.app;
    let t1 = lattice_local::new_turn_id();
    let t2 = lattice_local::new_turn_id();
    open_view(
        app,
        head(4, Some(t2), false),
        vec![
            user(1, t1, 1, "prompt-1"),
            step(2, t1, 0).text("comment target").finished().entry(),
            user(3, t2, 2, "prompt-2"),
            step(4, t2, 0)
                .turn_ordinal(2)
                .text("reply-2")
                .finished()
                .entry(),
        ],
    );
    let (live, _) = submit_comment_wire(app, "rewound accepted note");
    app.apply_submission(receipt(live, accepted_at(live, &[5], 6)));
    app.apply_view(LocalUpdate::Connection {
        status: ConnectionStatus::Eof,
    });
    save_comment(app, "offline draft");
    assert_eq!(
        app.drafts
            .draft_comments()
            .iter()
            .map(|c| c.n)
            .collect::<Vec<_>>(),
        vec![2],
        "断线期间新建评论应得 #2"
    );

    // 重连到已回退路径（head 停在节点 4，has_older=false）：初始批次收齐前先保留占位。
    app.begin_view();
    app.apply_view(LocalUpdate::Head(SessionHead {
        has_older: Some(false),
        ..head(4, Some(t2), false)
    }));
    app.apply_view(entry_update(user(1, t1, 1, "prompt-1")));
    app.apply_view(entry_update(
        step(2, t1, 0).text("comment target").finished().entry(),
    ));
    assert!(
        app.accepted_comments.contains_key(&live),
        "初始条目尚未收齐确认 head 前应继续保留 accepted_comments"
    );
    delete_draft_by_text(app, "offline draft");
    save_comment(app, "mid-batch draft");
    assert_eq!(
        app.drafts
            .draft_comments()
            .iter()
            .map(|c| c.n)
            .collect::<Vec<_>>(),
        vec![2],
        "初始条目尚未收齐前新建评论仍应避开 #1"
    );

    // 补齐剩余初始条目确认 head(4)，确认 live 不在当前路径后释放占用且绝不恢复为草稿。
    app.apply_view(entry_update(user(3, t2, 2, "prompt-2")));
    app.apply_view(entry_update(
        step(4, t2, 0)
            .turn_ordinal(2)
            .text("reply-2")
            .finished()
            .entry(),
    ));
    assert!(
        app.accepted_comments.is_empty(),
        "!has_older 且新路径不含该轮时，收齐后应释放 accepted_comments"
    );
    assert!(
        app.pending_accepted.is_none(),
        "收齐后不在路径上的轮次应清理 pending_accepted"
    );
    assert_eq!(
        app.drafts
            .draft_comments()
            .iter()
            .map(|c| (c.n, c.text.as_str()))
            .collect::<Vec<_>>(),
        vec![(2, "mid-batch draft")],
        "已接受的评论绝不可被恢复进本地草稿"
    );
    delete_draft_by_text(app, "mid-batch draft");
    save_comment(app, "fresh after rewind");
    assert_eq!(
        app.drafts
            .draft_comments()
            .iter()
            .map(|c| c.n)
            .collect::<Vec<_>>(),
        vec![1],
        "回退释放占用并删除草稿 #2 后，新评论应从当前路径最大编号 + 1（#1）起算"
    );

    // 子场景 2：has_older == true 且 input_node >= window_min（也在 head_pos 之后）。
    let mut h2 = Harness::new(false);
    h2.app.term_width = 100;
    h2.app.term_height = 40;
    let app2 = &mut h2.app;
    let t_win = lattice_local::new_turn_id();
    open_view(
        app2,
        SessionHead {
            has_older: Some(true),
            ..head(4, Some(t_win), false)
        },
        vec![
            user(3, t_win, 2, "prompt-win"),
            step(4, t_win, 0)
                .turn_ordinal(2)
                .text("comment target")
                .finished()
                .entry(),
        ],
    );
    let (live2, _) = submit_comment_wire(app2, "rewound older window accepted");
    app2.apply_submission(receipt(live2, accepted_at(live2, &[5], 6)));
    app2.apply_view(LocalUpdate::Connection {
        status: ConnectionStatus::Eof,
    });
    save_comment(app2, "offline draft older");
    assert_eq!(
        app2.drafts
            .draft_comments()
            .iter()
            .map(|c| c.n)
            .collect::<Vec<_>>(),
        vec![2],
        "断线期间新建评论应得 #2"
    );

    // 重连后窗口为节点 3..=4 且 has_older == true，live2 的 input_node=5 >= window_min(3)。
    app2.begin_view();
    app2.apply_view(LocalUpdate::Head(SessionHead {
        has_older: Some(true),
        ..head(4, Some(t_win), false)
    }));
    app2.apply_view(entry_update(user(3, t_win, 2, "prompt-win")));
    assert!(
        app2.accepted_comments.contains_key(&live2),
        "初始条目尚未收齐前应保留 accepted_comments"
    );
    app2.apply_view(entry_update(
        step(4, t_win, 0)
            .turn_ordinal(2)
            .text("comment target")
            .finished()
            .entry(),
    ));
    assert!(app2.has_older, "当前窗口前仍有更早历史");
    assert!(
        app2.accepted_comments.is_empty(),
        "has_older 为真但 input_node >= window_min 说明必在已放弃分叉上，应立即释放占用"
    );
    assert_eq!(
        app2.drafts
            .draft_comments()
            .iter()
            .map(|c| (c.n, c.text.as_str()))
            .collect::<Vec<_>>(),
        vec![(2, "offline draft older")],
        "has_older 场景下同样绝不将已接受评论恢复为草稿"
    );
    delete_draft_by_text(app2, "offline draft older");
    save_comment(app2, "fresh after older rewind");
    assert_eq!(
        app2.drafts
            .draft_comments()
            .iter()
            .map(|c| c.n)
            .collect::<Vec<_>>(),
        vec![1],
        "删除草稿 #2 后，回退路径上新建评论应重新从 #1 起算"
    );

    // 子场景 3：has_older == true 且 input_node < window_min 时先保留，翻页到达 !has_older 后收口释放。
    let mut h3 = Harness::new(false);
    h3.app.term_width = 100;
    h3.app.term_height = 40;
    let app3 = &mut h3.app;
    let t_a = lattice_local::new_turn_id();
    let t_b = lattice_local::new_turn_id();
    open_view(
        app3,
        SessionHead {
            has_older: Some(false),
            ..head(2, Some(t_a), false)
        },
        vec![
            user(1, t_a, 1, "prompt-a"),
            step(2, t_a, 0).text("comment target").finished().entry(),
        ],
    );
    let (live3, _) = submit_comment_wire(app3, "older fork accepted");
    app3.apply_submission(receipt(live3, accepted_at(live3, &[3], 4)));
    app3.apply_view(LocalUpdate::Connection {
        status: ConnectionStatus::Eof,
    });
    open_view(
        app3,
        SessionHead {
            has_older: Some(true),
            ..head(6, Some(t_b), false)
        },
        vec![
            user(5, t_b, 2, "prompt-b"),
            step(6, t_b, 0)
                .turn_ordinal(2)
                .text("comment target")
                .finished()
                .entry(),
        ],
    );
    assert!(
        app3.accepted_comments.contains_key(&live3),
        "input_node < window_min 且 has_older 为真时应暂留占位等待向上翻页核对"
    );
    let older_req = OlderRequest {
        session_id: sid(),
        before: NodeId(5),
        epoch: app3.epoch,
    };
    app3.apply_older(
        older_req,
        Ok(LocalHistory {
            entries: vec![
                user(1, t_a, 1, "prompt-a"),
                step(2, t_a, 0).text("comment target").finished().entry(),
            ],
            has_more: false,
        }),
    );
    assert!(!app3.has_older, "翻页到底后 has_older 应为 false");
    assert!(
        app3.accepted_comments.is_empty(),
        "翻页到底确认该轮不在当前路径后应释放残留 accepted_comments 占位"
    );
}

#[test]
fn comment_accepted_online_path_replaced_keeps_until_batch_complete_then_reconciles() {
    // 子场景 1：在线收到 path_replaced 后，该轮仍在活跃路径上，收齐后由 Input 接管。
    let mut h_keep = Harness::new(false);
    h_keep.app.term_width = 100;
    h_keep.app.term_height = 40;
    let app = &mut h_keep.app;
    let t1 = lattice_local::new_turn_id();
    let t2 = lattice_local::new_turn_id();
    open_view(
        app,
        head(4, Some(t2), false),
        vec![
            user(1, t1, 1, "prompt-1"),
            step(2, t1, 0).text("comment target").finished().entry(),
            user(3, t2, 2, "prompt-2"),
            step(4, t2, 0)
                .turn_ordinal(2)
                .text("reply-2")
                .finished()
                .entry(),
        ],
    );
    let (live, wire) = submit_comment_wire(app, "kept across path replaced");
    app.apply_submission(receipt(live, accepted_at(live, &[5], 6)));
    app.apply_view(LocalUpdate::Head(SessionHead {
        path_replaced: true,
        has_older: Some(false),
        ..head(6, Some(live), false)
    }));
    app.apply_view(entry_update(user(1, t1, 1, "prompt-1")));
    app.apply_view(entry_update(
        step(2, t1, 0).text("comment target").finished().entry(),
    ));
    assert!(
        app.accepted_comments.contains_key(&live),
        "path_replaced 初始批次未收齐前必须保留 accepted_comments"
    );
    save_comment(app, "draft before path_replaced finishes");
    assert_eq!(
        app.drafts
            .draft_comments()
            .iter()
            .map(|c| c.n)
            .collect::<Vec<_>>(),
        vec![2],
        "path_replaced 初始批次未收齐前新建评论仍应得 #2"
    );
    app.apply_view(entry_update(user(3, t2, 2, "prompt-2")));
    app.apply_view(entry_update(
        step(4, t2, 0)
            .turn_ordinal(2)
            .text("reply-2")
            .finished()
            .entry(),
    ));
    app.apply_view(entry_update(user(5, live, 3, &wire)));
    app.apply_view(entry_update(
        step(6, live, 0).turn_ordinal(3).finished().entry(),
    ));
    assert!(
        app.accepted_comments.is_empty(),
        "path_replaced 后若该轮仍在路径上，Input 到达即完成接管并释放 accepted_comments"
    );
    delete_draft_by_text(app, "draft before path_replaced finishes");
    save_comment(app, "after kept path_replaced");
    assert_eq!(
        app.drafts
            .draft_comments()
            .iter()
            .map(|c| c.n)
            .collect::<Vec<_>>(),
        vec![2],
        "由 Input 接管后新建评论仍从 #2 起算"
    );

    // 子场景 2：watch 已观察到该轮启动会话头（因果水位 >= start_node，Input 尚未到达），随后在线 path_replaced 回退到早先轮次（head 停在节点 4）。
    let mut h_rewind = Harness::new(false);
    h_rewind.app.term_width = 100;
    h_rewind.app.term_height = 40;
    let app = &mut h_rewind.app;
    open_view(
        app,
        head(4, Some(t2), false),
        vec![
            user(1, t1, 1, "prompt-1"),
            step(2, t1, 0).text("comment target").finished().entry(),
            user(3, t2, 2, "prompt-2"),
            step(4, t2, 0)
                .turn_ordinal(2)
                .text("reply-2")
                .finished()
                .entry(),
        ],
    );
    let (live_rw, _) = submit_comment_wire(app, "rewound by path replaced");
    app.apply_submission(receipt(live_rw, accepted_at(live_rw, &[5], 6)));
    // watch 先看到该轮启动时的会话头（节点 6 >= start_node），但 Input(5) 尚未到达。
    app.apply_view(LocalUpdate::Head(head(6, Some(live_rw), true)));
    assert!(
        app.accepted_comments.contains_key(&live_rw),
        "仅收到新轮会话头而 Input 未到时仍保留 accepted_comments"
    );
    app.apply_view(LocalUpdate::Head(SessionHead {
        path_replaced: true,
        has_older: Some(false),
        ..head(4, Some(t2), false)
    }));
    app.apply_view(entry_update(user(1, t1, 1, "prompt-1")));
    app.apply_view(entry_update(
        step(2, t1, 0).text("comment target").finished().entry(),
    ));
    save_comment(app, "draft during rewind batch");
    assert_eq!(
        app.drafts
            .draft_comments()
            .iter()
            .map(|c| c.n)
            .collect::<Vec<_>>(),
        vec![2],
        "在线 path_replaced 收齐前新建评论仍应得 #2"
    );
    app.apply_view(entry_update(user(3, t2, 2, "prompt-2")));
    app.apply_view(entry_update(
        step(4, t2, 0)
            .turn_ordinal(2)
            .text("reply-2")
            .finished()
            .entry(),
    ));
    assert!(
        app.accepted_comments.is_empty(),
        "因果确认晚于接受的在线 path_replaced 回退收齐后应释放 accepted_comments"
    );
    assert!(
        app.pending_accepted.is_none(),
        "因果确认晚于接受的在线 path_replaced 回退收齐后应清理 pending_accepted"
    );
    assert_eq!(
        app.drafts
            .draft_comments()
            .iter()
            .map(|c| (c.n, c.text.as_str()))
            .collect::<Vec<_>>(),
        vec![(2, "draft during rewind batch")],
        "在线 path_replaced 回退后绝不恢复已接受评论到草稿"
    );
    delete_draft_by_text(app, "draft during rewind batch");
    save_comment(app, "fresh after online rewind");
    assert_eq!(
        app.drafts
            .draft_comments()
            .iter()
            .map(|c| c.n)
            .collect::<Vec<_>>(),
        vec![1],
        "删除草稿 #2 后在回退后的路径上新建评论应重新从 #1 起算"
    );

    // 子场景 3：watch 已观察到该轮启动会话头（因果水位 >= start_node），随后在线 path_replaced 直接回退到根节点 NodeId(0)（立即确认会话头）。
    let mut h_root = comment_app();
    let app = &mut h_root.app;
    let (live_root, _) = submit_comment_wire(app, "rewound to root");
    app.apply_submission(receipt(live_root, accepted_at(live_root, &[3], 4)));
    app.apply_view(LocalUpdate::Head(head(4, Some(live_root), true)));
    app.apply_view(LocalUpdate::Head(SessionHead {
        path_replaced: true,
        has_older: Some(false),
        ..head(0, None, false)
    }));
    assert!(
        app.accepted_comments.is_empty(),
        "因果确认晚于接受并回退到根节点 0 后应释放 accepted_comments"
    );
    assert!(
        app.pending_accepted.is_none(),
        "因果确认晚于接受并回退到根节点 0 后应清理 pending_accepted"
    );
    assert_eq!(
        app.drafts.draft_count(),
        0,
        "回退到根节点 0 后绝不恢复已接受评论进草稿"
    );
}

#[test]
fn comment_accepted_late_pre_accept_path_replaced_snapshot_keeps_number_until_input_syncs() {
    // 子场景 1：非空旧回退快照 S 早于接受生成、晚于 Accepted 回执到达，绝不可提前释放 #1。
    let mut h = Harness::new(false);
    h.app.term_width = 100;
    h.app.term_height = 40;
    let app = &mut h.app;
    let t1 = lattice_local::new_turn_id();
    let t2 = lattice_local::new_turn_id();
    open_view(
        app,
        head(4, Some(t2), false),
        vec![
            user(1, t1, 1, "prompt-1"),
            step(2, t1, 0).text("comment target").finished().entry(),
            user(3, t2, 2, "prompt-2"),
            step(4, t2, 0)
                .turn_ordinal(2)
                .text("reply-2")
                .finished()
                .entry(),
        ],
    );
    let (live, wire) = submit_comment_wire(app, "accepted after stale snapshot");
    app.apply_submission(receipt(live, accepted_at(live, &[5], 6)));

    // 晚到的是接受前生成的回退快照 S（节点 1..=2，未过 start_node=6 水位）。
    app.apply_view(LocalUpdate::Head(SessionHead {
        path_replaced: true,
        has_older: Some(false),
        ..head(2, Some(t1), false)
    }));
    app.apply_view(entry_update(user(1, t1, 1, "prompt-1")));
    app.apply_view(entry_update(
        step(2, t1, 0).text("comment target").finished().entry(),
    ));
    assert!(
        app.accepted_comments.contains_key(&live),
        "晚到的接受前 path_replaced 快照收齐后必须继续保留 #1 占位"
    );
    assert_eq!(
        app.drafts.draft_count(),
        0,
        "旧快照收齐后不应将评论恢复为草稿"
    );
    assert_eq!(
        app.pending_accepted,
        Some(live),
        "旧快照未证明晚于接受时应继续保留 pending_accepted"
    );
    assert_eq!(
        app.phase,
        TurnPhase::Streaming,
        "旧快照未证明晚于接受时应保持运行中状态"
    );

    // 此时在 t1 上新建评论草稿 B，绝不可复用 #1，必须得到 #2。
    save_comment(app, "draft after stale snapshot");
    assert_eq!(
        app.drafts
            .draft_comments()
            .iter()
            .map(|c| c.n)
            .collect::<Vec<_>>(),
        vec![2],
        "接受前旧回退快照收齐后、Input 到达前新建评论必须得到 #2"
    );

    // 随后 watch 推来接受后的真实条目，由 Block::CommentedUser 接管并清理等待状态。
    app.apply_view(LocalUpdate::Head(head(6, Some(live), false)));
    app.apply_view(entry_update(user(5, live, 2, &wire)));
    app.apply_view(entry_update(
        step(6, live, 0).turn_ordinal(2).finished().entry(),
    ));
    assert!(
        app.accepted_comments.is_empty(),
        "真实 Input 到达后应由 Block::CommentedUser 接管并清空 accepted_comments"
    );
    assert!(
        app.pending_accepted.is_none(),
        "真实 Step 0 到达后应清理 pending_accepted"
    );
    assert_eq!(app.phase, TurnPhase::Idle, "该轮已结束，界面应回到 Idle");
    delete_draft_by_text(app, "draft after stale snapshot");
    save_comment(app, "draft after real input takeover");
    assert_eq!(
        app.drafts
            .draft_comments()
            .iter()
            .map(|c| c.n)
            .collect::<Vec<_>>(),
        vec![2],
        "删除草稿 #2 后仍由视图中的 #1 占用，新草稿应得 #2"
    );

    // 子场景 2：接受前生成的根节点回退快照 S（NodeId(0)）晚于 Accepted 到达，同样不得提前释放。
    let mut h_root = comment_app();
    let app = &mut h_root.app;
    let (live_root, wire_root) = submit_comment_wire(app, "accepted after stale root snapshot");
    app.apply_submission(receipt(live_root, accepted_at(live_root, &[3], 4)));
    app.apply_view(LocalUpdate::Head(SessionHead {
        path_replaced: true,
        has_older: Some(false),
        ..head(0, None, false)
    }));
    assert!(
        app.accepted_comments.contains_key(&live_root),
        "晚到的接受前根节点回退快照不得提前释放 #1 占位"
    );
    assert_eq!(app.drafts.draft_count(), 0, "不得将已接受评论恢复为草稿");
    assert_eq!(
        app.pending_accepted,
        Some(live_root),
        "接受前根节点快照不得提前清理 pending_accepted"
    );
    assert_eq!(
        app.phase,
        TurnPhase::Streaming,
        "接受前根节点快照不得误判为 Idle"
    );

    // 随后 watch 推来接受后的真实条目，接管 #1 并结束等待。
    app.apply_view(LocalUpdate::Head(head(4, Some(live_root), false)));
    app.apply_view(entry_update(user(3, live_root, 1, &wire_root)));
    app.apply_view(entry_update(
        step(4, live_root, 0)
            .turn_ordinal(1)
            .text("comment target")
            .finished()
            .entry(),
    ));
    assert!(
        app.accepted_comments.is_empty(),
        "根快照后的真实 Input 到达应接管并释放 accepted_comments"
    );
    assert!(
        app.pending_accepted.is_none(),
        "根快照后的真实 Step 0 到达应清理 pending_accepted"
    );
    assert_eq!(app.phase, TurnPhase::Idle);
    save_comment(app, "draft after root recovery takeover");
    assert_eq!(
        app.drafts
            .draft_comments()
            .iter()
            .map(|c| c.n)
            .collect::<Vec<_>>(),
        vec![2],
        "视图中的 #1 接管后新建评论应得 #2"
    );

    // 子场景 3（反向乱序）：watch 先收齐因果晚于 A(3, 4) 的回退分叉快照（节点 5、6），随后才收到迟到的 Accepted（覆盖 receipt 与 handoff 两条路径）。
    for via_handoff in [false, true] {
        let mut h_rev = comment_app();
        let app = &mut h_rev.app;
        let t1 = app.head.as_ref().unwrap().turn_id.unwrap();
        let (live_rev, _) = submit_comment_wire(app, "late accepted after newer fork");
        if via_handoff {
            app.apply_submission(receipt(live_rev, SubmissionResult::Unknown { error: None }));
            release_dock(app);
        }

        let t_other = lattice_local::new_turn_id();
        app.apply_view(LocalUpdate::Head(SessionHead {
            path_replaced: true,
            has_older: Some(false),
            ..head(6, Some(t_other), false)
        }));
        app.apply_view(entry_update(user(1, t1, 1, "original prompt")));
        app.apply_view(entry_update(
            step(2, t1, 0).text("comment target").finished().entry(),
        ));
        app.apply_view(entry_update(user(5, t_other, 2, "other prompt")));
        app.apply_view(entry_update(
            step(6, t_other, 0)
                .turn_ordinal(2)
                .text("other reply")
                .finished()
                .entry(),
        ));

        let late_receipt = SubmissionReceipt {
            turn_id: live_rev,
            result: accepted_at(live_rev, &[3], 4),
            recovered: via_handoff,
        };
        if via_handoff {
            app.apply_handoff(DetachReport {
                session_id: sid(),
                turn_id: Some(live_rev),
                submission: Some(late_receipt),
                submission_diagnostic: None,
                cancel: None,
                cancel_error: None,
                head: None,
                summary: None,
            });
        } else {
            app.apply_submission(LocalUpdate::Submit {
                update: SubmissionUpdate::Receipt(late_receipt),
            });
        }

        assert!(
            app.accepted_comments.is_empty(),
            "迟到 Accepted 遇到已确认的 watch_max_node(6) >= 4 后应立即核销 accepted_comments（via_handoff={via_handoff}）"
        );
        assert!(
            app.pending_accepted.is_none(),
            "迟到 Accepted 核销后不得悬留 pending_accepted（via_handoff={via_handoff}）"
        );
        assert_eq!(
            app.phase,
            TurnPhase::Idle,
            "迟到 Accepted 核销后界面应保持 Idle（via_handoff={via_handoff}）"
        );
        assert_eq!(
            app.drafts.draft_count(),
            0,
            "迟到 Accepted 核销后绝不恢复已接受评论到草稿（via_handoff={via_handoff}）"
        );
        save_comment(app, "fresh after late accepted release");
        assert_eq!(
            app.drafts
                .draft_comments()
                .iter()
                .map(|c| c.n)
                .collect::<Vec<_>>(),
            vec![1],
            "反向乱序核销释放占位后新建评论应从 #1 起算（via_handoff={via_handoff}）"
        );
    }
}

#[test]
fn comment_accepted_after_begin_view_keeps_number_on_stale_initial_snapshot_until_causal_proof() {
    let mut h = comment_app();
    let app = &mut h.app;
    let t1 = app.head.as_ref().unwrap().turn_id.unwrap();
    let (live, _) = submit_comment_wire(app, "accepted after begin_view");

    // 先发起新 watch（begin_view），随后才收到本次提交的 Accepted 回执。
    app.begin_view();
    app.apply_submission(receipt(live, accepted_at(live, &[3], 4)));

    // 该次 begin_view 在接受前捕获的旧初始快照到达（active_view_gen == accepted_view_gen 且 watch_max_node=2 < 4）。
    app.apply_view(LocalUpdate::Head(SessionHead {
        has_older: Some(false),
        ..head(2, Some(t1), false)
    }));
    app.apply_view(entry_update(user(1, t1, 1, "original prompt")));
    app.apply_view(entry_update(
        step(2, t1, 0).text("comment target").finished().entry(),
    ));
    assert!(
        app.accepted_comments.contains_key(&live),
        "begin_view 早于接受发起时，在途旧初始快照收齐后不得释放 #1 占位"
    );
    assert_eq!(
        app.pending_accepted,
        Some(live),
        "未过因果水位的旧初始快照不得清理 pending_accepted"
    );
    assert_eq!(
        app.phase,
        TurnPhase::Streaming,
        "未过因果水位的旧初始快照应保持 Streaming"
    );
    save_comment(app, "draft during stale initial view");
    assert_eq!(
        app.drafts
            .draft_comments()
            .iter()
            .map(|c| c.n)
            .collect::<Vec<_>>(),
        vec![2],
        "旧初始快照收齐后新建评论仍必须避开 #1，得到 #2"
    );

    // 同连接随后收到因果晚于接受的新分叉快照（新轮 t_other 推进到节点 5、6，满足 watch_max_node(6) >= 4）。
    let t_other = lattice_local::new_turn_id();
    app.apply_view(LocalUpdate::Head(SessionHead {
        path_replaced: true,
        has_older: Some(false),
        ..head(6, Some(t_other), false)
    }));
    app.apply_view(entry_update(user(1, t1, 1, "original prompt")));
    app.apply_view(entry_update(
        step(2, t1, 0).text("comment target").finished().entry(),
    ));
    app.apply_view(entry_update(user(5, t_other, 2, "other prompt")));
    app.apply_view(entry_update(
        step(6, t_other, 0)
            .turn_ordinal(2)
            .text("other reply")
            .finished()
            .entry(),
    ));
    assert!(
        app.accepted_comments.is_empty(),
        "同连接观察到节点 6 >= start_node(4) 且当前路径不含 live 后应立即释放 #1 占位"
    );
    assert!(
        app.pending_accepted.is_none(),
        "因果确认不在当前路径后应清理 pending_accepted"
    );
    assert_eq!(app.phase, TurnPhase::Idle, "因果确认后界面应回到 Idle");
    assert_eq!(
        app.drafts
            .draft_comments()
            .iter()
            .map(|c| (c.n, c.text.as_str()))
            .collect::<Vec<_>>(),
        vec![(2, "draft during stale initial view")],
        "因果确认释放占位时绝不将已接受评论恢复为草稿"
    );
    delete_draft_by_text(app, "draft during stale initial view");
    save_comment(app, "fresh after causal fork release");
    assert_eq!(
        app.drafts
            .draft_comments()
            .iter()
            .map(|c| c.n)
            .collect::<Vec<_>>(),
        vec![1],
        "删除草稿 #2 后在新分叉上新建评论应重新从 #1 起算"
    );
}

#[test]
fn running_enter_warns_and_preserves_input_and_drafts_while_commands_still_work() {
    for cancelling in [false, true] {
        let mut h = comment_app();
        let img_path = h.options.project_root.join("busy.png");
        std::fs::write(&img_path, b"\x89PNG\r\n\x1a\nbusy-img").expect("写入测试图片");
        let app = &mut h.app;
        app.focus = Focus::Dock;
        app.input.set_text("running");
        app.apply_action(KeyAction::Submit).unwrap();
        if cancelling {
            app.apply_action(KeyAction::CancelOrClear);
            assert_eq!(app.cancel_requested, Some(CancelTarget::Own));
        }
        app.input.set_text(" \n ");
        assert!(app.apply_action(KeyAction::Submit).is_none());
        assert_eq!(app.input.text(), " \n ");
        assert!(app.dock.message().is_none(), "空白输入不触发忙碌警告");

        app.input.set_text("/help");
        app.apply_action(KeyAction::Submit);
        // `/help` 打开 Dock 面板并遮住输入区；Esc 关闭后才回到输入区。
        assert!(!app.dock.shows_input());
        app.apply_action(KeyAction::Clear);

        save_comment(app, "running draft comment");
        app.focus = Focus::Dock;
        app.apply_action(KeyAction::Paste("draft while running".into()));
        app.apply_action(KeyAction::Paste(img_path.to_str().unwrap().to_owned()));

        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 40)).unwrap();
        terminal
            .draw(|frame| crate::view::draw(frame, app))
            .unwrap();
        let before_screen = buffer_text(terminal.backend().buffer());
        assert!(
            !before_screen.contains("Enter 排队") && !before_screen.contains("排队已满"),
            "输入区标题线不应出现排队提示：{before_screen}"
        );
        assert!(
            before_screen.contains("1 条评论待发送"),
            "输入区标题线应保留评论草稿计数：{before_screen}"
        );

        assert!(
            app.apply_action(KeyAction::Submit).is_none(),
            "运行中按 Enter 不能提交"
        );
        assert_eq!(
            app.dock.message(),
            Some(&status(
                NoticeLevel::Warn,
                "当前有一轮正在执行，请等本轮结束后再提交"
            ))
        );
        assert_eq!(app.input.text(), "draft while running", "输入文字应保留");
        assert_eq!(app.input.image_count(), 1, "输入图片原子应保留");
        assert_eq!(app.drafts.draft_count(), 1, "评论草稿应保留");
        assert_eq!(
            app.history.prev(),
            Some("/help"),
            "被阻止的提交不应写入输入历史"
        );
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
    accepted_reply_at(request, &[1], 2)
}
fn accepted_reply_at(request: &Value, input_nodes: &[u64], start_node: u64) -> Value {
    let mut turn =
        serde_json::to_value(turn_view_at(request_turn(request), input_nodes, start_node)).unwrap();
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
        let app = App::new(
            ctx,
            false,
            &options.session_dir,
            options.project_root.clone(),
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
        "submit" => Reply::Now(accepted_reply_at(r, &[3], 4)),
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
            step(4, id, 0)
                .turn_ordinal(2)
                .retried("info", "retry")
                .metering(
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
        let shown = remote.wait(|f| f.text.contains("@2 ──")).await;
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
        "submit" => Reply::Now(accepted_reply_at(r, &[3], 4)),
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
            .wait(|f| f.text.contains("请求已接受，等待对话同步…"))
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
                has_more: false,
            })
            .unwrap(),
        )),
        _ => Reply::Never,
    });
    let mut h = Harness::new(false);
    let remote = h.remote.clone();
    let driver = async {
        let last = numbered_turn(4);
        let mut initial = head(8, Some(last), false);
        initial.has_older = Some(true);
        view_tx.push([head_reply(initial)]);
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
        assert_eq!(request["trigger_channel"], json!("tui"));
        assert_eq!(
            request["inputs"],
            json!([{"channel":"tui","parts":[{"kind":"text","text":"selected prompt"}]}])
        );
        assert_eq!(request["expected_settings"], json!(settings_version(0x33)));
        assert!(request["expected_position"].is_null());
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
async fn configure_in_flight_blocks_submit_and_only_latest_generation_applies() {
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
            "submit" => Reply::Now(accepted_reply(r)),
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

            // C1 在途时按 Enter 不提交、不入队，显示 Warn 并保留输入。
            remote.line("draft during configure");
            let blocked = remote
                .wait(|f| f.dock.contains("正在修改会话设置，请稍后再提交"))
                .await;
            assert_eq!(
                count(&peer.seen.borrow(), "submit"),
                0,
                "configure 在途不能直接提交"
            );
            assert_eq!(blocked.input, "draft during configure");
            assert!(!blocked.panel.contains("Enter 排队"));
            remote.press(KeyCode::Char(' '), KeyModifiers::NONE);
            remote.wait(|f| f.dock.is_empty()).await;
            remote.press(KeyCode::Esc, KeyModifiers::NONE);
            remote.wait(|f| f.input.is_empty()).await;

            // C1 回复前再次 /effort 发出 C2；面板、状态行与错误提示只采用最新代次 C2。
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
            } else {
                cfg1_tx
                    .send(error_reply(local_error(
                        LocalErrorKind::BadRequest,
                        "stale c1 failure",
                    )))
                    .unwrap();
            }
            let after_c1 = remote.processed().await;
            assert!(
                !after_c1.statusline.contains("chosen"),
                "面板与状态行不采用非最新代次 C1 的回复"
            );
            assert!(
                after_c1.dock.is_empty(),
                "非最新代次 C1 失败不应弹出错误提示：{}",
                after_c1.dock
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
            remote
                .wait(|f| f.statusline.contains("chosen") && f.statusline.contains("off"))
                .await;

            // 全部 configure 完成后手动提交，携带最新代次 C2 的版本。
            remote.line("after configure");
            let seen = peer.wait(|s| count(s, "submit") == 1).await;
            let submit = &requests(&seen, "submit").next().unwrap()["request"];
            assert_eq!(submit["trigger_channel"], json!("tui"));
            assert_eq!(
                submit["inputs"],
                json!([{"channel":"tui","parts":[{"kind":"text","text":"after configure"}]}])
            );
            assert_eq!(submit["expected_settings"], json!(settings_version(0x44)));
            assert!(submit["expected_position"].is_null());
            remote.ctrl('c');
            remote.ctrl('c');
        };
        let ((), exit) = bounded(LOOP_LIMIT, async {
            tokio::join!(driver, h.run(&peer.client))
        })
        .await;
        assert!(exit.result.is_ok());
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
        assert_eq!(submit_req["request"]["trigger_channel"], json!("tui"));
        assert_eq!(
            submit_req["request"]["inputs"],
            json!([{"channel":"tui","parts":[{"kind":"text","text":"before first watch head"}]}])
        );
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

#[tokio::test]
async fn idle_submit_preflight_failure_preserves_input_draft_and_never_sends() {
    let closed = numbered_turn(1);
    let views = Views::default();
    let view_tx = views.clone();
    let peer = Peer::start(move |r| match r["operation"].as_str().unwrap() {
        "watch" => views.subscribe(),
        "submit" => Reply::Now(accepted_reply_at(r, &[3], 4)),
        _ => Reply::Never,
    });
    let mut h = Harness::new(false);
    let remote = h.remote.clone();
    let line = format!("{}\n", "x".repeat(79));
    let body = format!("{}tail", line.repeat(26_213));
    assert!(body.len() <= 2_097_152, "输入文字本身不能超过单轮文本上限");
    let body_for_driver = body.clone();
    let driver = async move {
        view_tx.push([
            head_reply(head(2, Some(closed), false)),
            entry_reply(user(1, closed, 1, "original prompt")),
            entry_reply(step(2, closed, 0).text("comment target").finished().entry()),
        ]);
        let initial = remote
            .wait(|f| f.phase == TurnPhase::Idle && f.text.contains("comment target"))
            .await;
        let total = initial.text.lines().count();
        let target_row = initial
            .text
            .lines()
            .position(|l| l.contains("comment target"))
            .expect("对话记录中应包含评论目标行");
        for _ in 0..(total - target_row) {
            remote.press(KeyCode::Up, KeyModifiers::NONE);
        }
        remote.press(KeyCode::Enter, KeyModifiers::NONE);
        remote
            .keys
            .unbounded_send(Ok(Event::Paste("草稿评论".into())))
            .unwrap();
        remote.press(KeyCode::Enter, KeyModifiers::NONE);
        remote.press(KeyCode::Esc, KeyModifiers::NONE);
        remote
            .wait(|f| f.text.contains("草稿 · #1") && f.panel.contains("1 条评论待发送"))
            .await;

        remote
            .keys
            .unbounded_send(Ok(Event::Paste(body_for_driver.clone())))
            .unwrap();
        remote.press(KeyCode::Enter, KeyModifiers::NONE);
        let shown = remote.wait(|f| !f.dock.is_empty()).await;
        assert!(
            shown.panel.contains("── ERROR ─"),
            "预检未通过应在底部区域暂驻错误状态：{}",
            shown.panel
        );
        assert!(
            shown.dock.contains("2,097,152")
                && shown.dock.contains("输入已保留")
                && !shown.dock.contains("被拒绝"),
            "错误文案应说明原因与输入保留且不写被拒绝：{}",
            shown.dock
        );
        assert_eq!(shown.input, body_for_driver, "输入区文字应保持原样");
        assert!(
            shown.text.contains("草稿 · #1") && shown.text.contains("草稿评论"),
            "评论草稿应留在原位：{}",
            shown.text
        );
        assert_eq!(
            shown.text.matches("── USER ").count(),
            1,
            "对话记录中不应出现新的用户消息块"
        );
        assert_eq!(shown.phase, TurnPhase::Idle);
        remote.ctrl('c');
        remote.ctrl('c');
    };
    let ((), exit) = bounded(LOOP_LIMIT, async {
        tokio::join!(driver, h.run(&peer.client))
    })
    .await;
    assert!(exit.result.is_ok());
    assert!(exit.turn.is_none());
    assert_eq!(
        count(&peer.seen.borrow(), "submit"),
        0,
        "预检失败时假后台不应收到提交请求"
    );
    assert_eq!(h.app.input.text(), body, "输入区文字应保持原样");
    assert_eq!(h.app.drafts.draft_count(), 1, "评论草稿应保持原样");
    assert!(
        compose_parts(&h.app.input, &h.app.drafts.draft_comments())
            .iter()
            .map(|part| match part {
                UserInputPart::Text { text } => text.len(),
                UserInputPart::Image { .. } => 0,
            })
            .sum::<usize>()
            > 2_097_152,
        "输入文字加评论草稿编码后应超过上限"
    );
    assert_eq!(h.app.history.prev(), None, "输入历史不应新增记录");
    assert!(
        !h.app
            .blocks
            .iter()
            .any(|b| matches!(b, Block::CommentedUser(_)))
            && h.app.submitted.is_none(),
        "不应登记新的用户提交块或提交态"
    );
    assert!(
        h.app
            .blocks
            .iter()
            .any(|b| matches!(b, Block::Status(s) if s.level == NoticeLevel::Error)),
        "应记录错误等级的状态消息"
    );
}

#[tokio::test]
async fn paste_interleaved_text_and_image_submits_exact_parts_in_order() {
    let views = Views::default();
    let view_tx = views.clone();
    let peer = Peer::start(move |r| match r["operation"].as_str().unwrap() {
        "watch" => views.subscribe(),
        "submit" => Reply::Now(accepted_reply_at(r, &[3], 4)),
        _ => Reply::Never,
    });
    let mut h = Harness::new(false);
    let remote = h.remote.clone();
    let png_bytes = b"\x89PNG\r\n\x1a\ninterleaved-payload".to_vec();
    let image_path = h.options.project_root.join("shot.png");
    std::fs::write(&image_path, &png_bytes).expect("写入测试图片文件");
    let path_str = image_path.to_str().expect("路径应为有效 UTF-8").to_owned();

    let driver = async {
        view_tx.push([head_reply(head(0, None, false))]);
        remote.wait(|f| f.phase == TurnPhase::Idle).await;

        remote
            .keys
            .unbounded_send(Ok(Event::Paste("前缀文字".into())))
            .expect("发送前段文字粘贴事件");
        remote
            .keys
            .unbounded_send(Ok(Event::Paste(path_str.clone())))
            .expect("发送图片路径粘贴事件");
        remote
            .keys
            .unbounded_send(Ok(Event::Paste("后缀文字".into())))
            .expect("发送后段文字粘贴事件");

        remote
            .wait(|f| f.panel.contains("前缀文字[IMAGE 1 · png · 27 B]后缀文字"))
            .await;

        remote.press(KeyCode::Enter, KeyModifiers::NONE);
        let submitted_frame = remote
            .wait(|f| {
                f.phase == TurnPhase::Streaming
                    && (f.text.contains("正在提交请求…")
                        || f.text.contains("请求已接受，等待对话同步…"))
            })
            .await;
        assert_eq!(submitted_frame.phase, TurnPhase::Streaming);

        peer.wait(|s| count(s, "submit") == 1).await;
        remote.ctrl('c');
        remote.ctrl('c');
    };
    let ((), exit) = bounded(LOOP_LIMIT, async {
        tokio::join!(driver, h.run(&peer.client))
    })
    .await;
    assert!(exit.result.is_ok());

    let seen = peer.seen.borrow().clone();
    let submit_val = &requests(&seen, "submit")
        .next()
        .expect("假后台应收到一次提交")["request"];
    let decoded: TurnRequest =
        serde_json::from_value(submit_val.clone()).expect("反序列化提交请求");
    assert_eq!(decoded.trigger_channel, Channel::parse("tui").unwrap());
    assert_eq!(decoded.expected_position, None);
    assert_eq!(decoded.inputs.len(), 1);
    let parts = &decoded.inputs[0].parts;
    assert_eq!(parts.len(), 3, "提交片段应保持三段交错顺序");
    assert!(
        matches!(&parts[0], UserInputPart::Text { text } if text == "前缀文字"),
        "首段应为前缀文字"
    );
    assert!(
        matches!(&parts[1], UserInputPart::Image { image } if *image == png_bytes),
        "中段应为与原文件完全一致的图片字节"
    );
    assert!(
        matches!(&parts[2], UserInputPart::Text { text } if text == "后缀文字"),
        "末段应为后缀文字"
    );
    assert_eq!(
        h.app.history.prev(),
        Some("前缀文字后缀文字"),
        "输入历史只记录文字部分"
    );
}

#[tokio::test]
async fn modified_image_file_after_paste_blocks_submit_and_preserves_input() {
    let views = Views::default();
    let view_tx = views.clone();
    let peer = Peer::start(move |r| match r["operation"].as_str().unwrap() {
        "watch" => views.subscribe(),
        "submit" => Reply::Now(accepted_reply_at(r, &[3], 4)),
        _ => Reply::Never,
    });
    let mut h = Harness::new(false);
    let remote = h.remote.clone();
    let image_path = h.options.project_root.join("mutated.png");
    std::fs::write(&image_path, b"\x89PNG\r\n\x1a\nversion-1").expect("写入初始 PNG");
    let path_str = image_path.to_str().expect("路径应为有效 UTF-8").to_owned();
    let image_path_for_driver = image_path.clone();

    let driver = async move {
        view_tx.push([head_reply(head(0, None, false))]);
        remote.wait(|f| f.phase == TurnPhase::Idle).await;

        remote
            .keys
            .unbounded_send(Ok(Event::Paste("请看图".into())))
            .expect("粘贴文字");
        remote
            .keys
            .unbounded_send(Ok(Event::Paste(path_str)))
            .expect("粘贴图片路径");
        remote
            .wait(|f| f.panel.contains("请看图[IMAGE 1 · png · 17 B]"))
            .await;

        // 改写为相同字节长度、相同 PNG 魔数但内容不同的图片。
        std::fs::write(&image_path_for_driver, b"\x89PNG\r\n\x1a\nversion-2")
            .expect("改写同长度同魔数的不同 PNG 内容");

        remote.press(KeyCode::Enter, KeyModifiers::NONE);
        let shown = remote.wait(|f| !f.dock.is_empty()).await;
        assert!(
            shown.panel.contains("── ERROR ─"),
            "图片内容改变时应在 Dock 暂驻错误：{}",
            shown.panel
        );
        assert!(
            shown.dock.contains("在预检后已改变") && shown.dock.contains("输入已保留"),
            "错误文案应指出图片在预检后已改变且输入已保留：{}",
            shown.dock
        );

        remote.press(KeyCode::Char(' '), KeyModifiers::NONE);
        remote
            .wait(|f| f.dock.is_empty() && f.panel.contains("请看图[IMAGE 1 · png · 17 B]"))
            .await;

        remote.keys.close_channel();
    };
    let ((), exit) = bounded(LOOP_LIMIT, async {
        tokio::join!(driver, h.run(&peer.client))
    })
    .await;
    assert!(exit.result.is_ok());
    assert_eq!(
        count(&peer.seen.borrow(), "submit"),
        0,
        "图片被改写时不应发出提交请求"
    );
    assert_eq!(h.app.input.text(), "请看图", "输入区文字应保持原样");
    assert_eq!(h.app.input.image_count(), 1, "输入区图片原子应保持原样");
}

#[tokio::test]
async fn deleted_image_file_after_paste_blocks_submit_and_preserves_input() {
    let views = Views::default();
    let view_tx = views.clone();
    let peer = Peer::start(move |r| match r["operation"].as_str().unwrap() {
        "watch" => views.subscribe(),
        "submit" => Reply::Now(accepted_reply_at(r, &[3], 4)),
        _ => Reply::Never,
    });
    let mut h = Harness::new(false);
    let remote = h.remote.clone();
    let image_path = h.options.project_root.join("deleted.png");
    std::fs::write(&image_path, b"\x89PNG\r\n\x1a\npayload").expect("写入初始 PNG");
    let path_str = image_path.to_str().expect("路径应为有效 UTF-8").to_owned();
    let image_path_for_driver = image_path.clone();

    let driver = async move {
        view_tx.push([head_reply(head(0, None, false))]);
        remote.wait(|f| f.phase == TurnPhase::Idle).await;

        remote
            .keys
            .unbounded_send(Ok(Event::Paste("截图说明".into())))
            .expect("粘贴文字");
        remote
            .keys
            .unbounded_send(Ok(Event::Paste(path_str)))
            .expect("粘贴图片路径");
        remote
            .wait(|f| f.panel.contains("截图说明[IMAGE 1 · png · 15 B]"))
            .await;

        std::fs::remove_file(&image_path_for_driver).expect("删除粘贴后的图片文件");

        remote.press(KeyCode::Enter, KeyModifiers::NONE);
        let shown = remote.wait(|f| !f.dock.is_empty()).await;
        assert!(
            shown.panel.contains("── ERROR ─"),
            "图片被删除时应在 Dock 暂驻错误：{}",
            shown.panel
        );
        let (_, after_prefix) = shown
            .dock
            .split_once("无法读取第 1 张图片：")
            .unwrap_or_else(|| panic!("错误应包含读取失败前缀：{}", shown.dock));
        let (reason, _) = after_prefix
            .split_once("；输入已保留")
            .unwrap_or_else(|| panic!("错误应说明输入已保留：{}", shown.dock));
        assert!(!reason.trim().is_empty(), "错误应保留具体读取失败原因");

        remote.press(KeyCode::Char(' '), KeyModifiers::NONE);
        remote
            .wait(|f| f.dock.is_empty() && f.panel.contains("截图说明[IMAGE 1 · png · 15 B]"))
            .await;

        remote.keys.close_channel();
    };
    let ((), exit) = bounded(LOOP_LIMIT, async {
        tokio::join!(driver, h.run(&peer.client))
    })
    .await;
    assert!(exit.result.is_ok());
    assert_eq!(
        count(&peer.seen.borrow(), "submit"),
        0,
        "图片删除后不应发出提交请求"
    );
    assert_eq!(h.app.input.text(), "截图说明", "输入区文字应保持原样");
    assert_eq!(h.app.input.image_count(), 1, "输入区图片原子应保持原样");
}

#[test]
fn image_submission_rejection_restores_when_empty_and_distinguishes_pure_image_vs_text_draft() {
    // 1. 手动提交含图消息被拒且输入区为空：完整图文按原顺序放回输入区。
    for (how, late) in (0..4)
        .map(|how| (how, false))
        .chain([(0, true), (1, true), (3, true)])
    {
        let mut h = Harness::new(true);
        let img_path = h.options.project_root.join("restore.png");
        std::fs::write(&img_path, b"\x89PNG\r\n\x1a\nrestore!").expect("写入图片");
        let app = &mut h.app;
        app.apply_action(KeyAction::Paste("前文".into()));
        app.apply_action(KeyAction::Paste(img_path.to_str().unwrap().to_owned()));
        app.apply_action(KeyAction::Paste("后文".into()));
        app.submit().expect("空闲手动提交含图消息应返回请求");
        let id = app.own_turn.expect("应记录本客户端提交身份");
        if late {
            app.apply_submission(receipt(id, SubmissionResult::Unknown { error: None }));
        }
        reject(app, id, how);
        assert_eq!(app.input.image_count(), 1, "输入区为空时图片原子应完整放回");
        assert_eq!(app.input.text(), "前文后文", "输入区为空时文字应放回");
        assert_eq!(
            crate::view::input_visual(&app.input, 200).0,
            vec!["前文[IMAGE 1 · png · 16 B]后文".to_owned()],
            "放回后的图文应保持原有交错顺序"
        );
        assert_eq!(app.phase, TurnPhase::Idle);
    }

    // 2. 输入区已有新草稿时，纯图片消息被拒：新草稿不动，提示写明图片未放回且不含 Ctrl+↑，历史为空。
    for (how, late) in (0..4)
        .map(|how| (how, false))
        .chain([(0, true), (1, true), (3, true)])
    {
        let mut h = Harness::new(true);
        let img_path = h.options.project_root.join("pure.png");
        std::fs::write(&img_path, b"\x89PNG\r\n\x1a\npure-img").expect("写入纯图片");
        let app = &mut h.app;
        app.apply_action(KeyAction::Paste(img_path.to_str().unwrap().to_owned()));
        app.submit().expect("空闲手动提交纯图片应返回请求");
        let id = app.own_turn.expect("应记录本客户端提交身份");
        if late {
            app.apply_submission(receipt(id, SubmissionResult::Unknown { error: None }));
        }
        app.input.set_text("用户新草稿");
        reject(app, id, how);
        assert_eq!(
            app.input.text(),
            "用户新草稿",
            "输入区已有新草稿时不得覆盖新草稿"
        );
        assert_eq!(app.input.image_count(), 0, "纯图片不应覆盖新草稿");
        let status_msg = app
            .blocks
            .iter()
            .filter_map(|block| match block {
                Block::Status(s) => Some(s.text.as_str()),
                _ => None,
            })
            .next_back()
            .expect("应生成拒绝或未接受状态消息");
        assert!(
            status_msg.contains("图片未放回")
                && !status_msg.contains("Ctrl+↑")
                && !status_msg.contains("输入已保留"),
            "纯图片被拒且有新草稿时不应提示 Ctrl+↑ 或输入已保留：{status_msg}"
        );
        assert_eq!(app.history.prev(), None, "纯图片消息不应写入输入历史");
    }

    // 3. 输入区已有新草稿时，图文混排消息被拒：新草稿不动，提示含 Ctrl+↑，输入历史只含文字。
    for (how, late) in (0..4)
        .map(|how| (how, false))
        .chain([(0, true), (1, true), (3, true)])
    {
        let mut h = Harness::new(true);
        let img_path = h.options.project_root.join("mixed.png");
        std::fs::write(&img_path, b"\x89PNG\r\n\x1a\nmixed-img").expect("写入图文混排图片");
        let app = &mut h.app;
        app.apply_action(KeyAction::Paste("图文里的文字".into()));
        app.apply_action(KeyAction::Paste(img_path.to_str().unwrap().to_owned()));
        app.submit().expect("空闲手动提交图文混排消息应返回请求");
        let id = app.own_turn.expect("应记录本客户端提交身份");
        if late {
            app.apply_submission(receipt(id, SubmissionResult::Unknown { error: None }));
        }
        app.input.set_text("另一份新草稿");
        reject(app, id, how);
        assert_eq!(app.input.text(), "另一份新草稿", "新草稿应保持原样");
        assert_eq!(app.input.image_count(), 0, "原图片不应混入新草稿");
        let status_msg = app
            .blocks
            .iter()
            .filter_map(|block| match block {
                Block::Status(s) => Some(s.text.as_str()),
                _ => None,
            })
            .next_back()
            .expect("应生成拒绝或未接受状态消息");
        assert!(
            status_msg.contains("Ctrl+↑")
                && status_msg.contains("随附图片未放回")
                && !status_msg.contains("输入已保留"),
            "图文消息被拒且有新草稿时应提示按 Ctrl+↑ 从历史找回文字且随附图片未放回：{status_msg}"
        );
        assert_eq!(
            app.history.prev(),
            Some("图文里的文字"),
            "输入历史应只包含图文消息中的文字部分"
        );
    }
}

#[test]
fn initial_replay_manual_submit_preflight_failure_preserves_comment_draft_until_step_arrives() {
    let mut h = comment_app();
    let app = &mut h.app;
    let target = app
        .head
        .as_ref()
        .expect("初始会话头")
        .turn_id
        .expect("被评论轮次");
    save_comment(app, "重连前草稿");
    app.focus = Focus::Dock;
    assert_eq!(app.drafts.draft_count(), 1, "保存后应有 1 条评论草稿");

    // 重连后开始初始回放：用户消息已回放，被评论的 Step 尚未回放。
    app.begin_view();
    app.apply_view(LocalUpdate::Head(head(2, Some(target), false)));
    app.apply_view(entry_update(user(1, target, 1, "original prompt")));
    assert!(app.initial, "末尾 Step 尚未到达时仍处于初始回放期间");
    assert!(app.present_steps().is_empty(), "此时对话记录中尚无 Step");

    let oversized = "x".repeat(2_097_153);
    app.input.set_text(&oversized);
    assert!(app.submit().is_none(), "超过 2 MiB 的消息预检应拒绝提交");

    let err = app.dock.message().expect("预检失败应在 Dock 暂驻错误");
    assert_eq!(err.level, NoticeLevel::Error);
    assert!(
        err.text.contains("2,097,152") && err.text.contains("输入已保留"),
        "错误文案应说明单轮文本超限且输入已保留：{}",
        err.text
    );
    assert_eq!(app.input.text(), oversized, "预检失败后输入区文字应保留");
    assert_eq!(
        app.drafts.draft_count(),
        1,
        "预检失败时不得提前丢弃锚点暂未回放的评论草稿"
    );
    assert!(
        !app.blocks
            .iter()
            .any(|b| matches!(b, Block::Status(s) if s.level == NoticeLevel::Warn)),
        "预检失败时不应产生丢弃评论的警告"
    );

    // 随后被评论的 Step 到达，初始回放结束，草稿仍保留并重新挂回锚点。
    app.apply_view(entry_update(
        step(2, target, 0).text("comment target").finished().entry(),
    ));
    assert!(!app.initial, "末尾 Step 到达后初始回放应结束");
    assert_eq!(
        app.drafts.draft_count(),
        1,
        "Step 到达后评论草稿仍应为 1 条"
    );
    assert_eq!(
        app.drafts
            .drafts()
            .next()
            .expect("保留的草稿")
            .comment()
            .text,
        "重连前草稿"
    );
    assert!(
        transcript(app).contains("草稿 · #1"),
        "草稿卡片应重新显示在锚点下方"
    );
}

#[test]
fn apply_clipboard_stages_image_in_private_tmp_submits_bytes_and_removes_dir_on_drop() {
    let mut h = Harness::new(false);
    let png = b"\x89PNG\r\n\x1a\nclipboard-png".to_vec();
    h.app
        .apply_clipboard(Ok(crate::clipboard::Content::Image(png.clone())));

    let parts = h.app.input.parts();
    let [
        UserInputPart::Image {
            image:
                PendingImage {
                    source: ImageSource::Temp(temp_path),
                    ..
                },
        },
    ] = &parts[..]
    else {
        panic!("输入区应含 1 个 Temp 图片原子");
    };
    let staging_dir = temp_path
        .parent()
        .expect("暂存图片文件应有父目录")
        .to_path_buf();
    assert_eq!(
        staging_dir.parent(),
        Some(Path::new("/tmp")),
        "暂存目录应位于 /tmp 下"
    );
    let dir_name = staging_dir
        .file_name()
        .and_then(|s| s.to_str())
        .expect("暂存目录名应为 UTF-8");
    assert!(
        dir_name.starts_with("lattice-tui-"),
        "暂存目录名应以 lattice-tui- 开头：{dir_name}"
    );
    assert!(staging_dir.exists(), "丢弃 App 前暂存目录应存在");

    let req = h
        .app
        .apply_action(KeyAction::Submit)
        .expect("按 Enter 应返回提交请求");
    assert!(
        req.inputs[0].parts == [UserInputPart::Image { image: png }],
        "提交请求应携带剪贴板图片的原始字节"
    );

    drop(h);
    assert!(!staging_dir.exists(), "丢弃 App 后暂存目录应被删除");
}

#[test]
fn apply_clipboard_invalid_image_pushes_error_and_no_session_pushes_warn() {
    let mut h = Harness::new(false);
    let app = &mut h.app;
    app.input.set_text("原有草稿");
    let before = app.input.clone();

    app.apply_clipboard(Ok(crate::clipboard::Content::Image(
        b"not an image".to_vec(),
    )));
    let err = app
        .dock
        .message()
        .expect("剪贴板返回非图片字节时应在 Dock 暂驻错误");
    assert_eq!(err.level, NoticeLevel::Error, "非图片字节应推 Error");
    assert!(
        err.text.contains("无法粘贴图片："),
        "错误文案应说明无法粘贴图片：{}",
        err.text
    );
    assert!(app.input == before, "推 Error 后输入区应保持不变");

    release_dock(app);
    app.apply_clipboard(Err(crate::clipboard::ClipboardError::NoSession));
    let warn = app
        .dock
        .message()
        .expect("未检测到图形会话时应在 Dock 暂驻警告");
    assert_eq!(warn.level, NoticeLevel::Warn, "无图形会话应推 Warn");
    assert_eq!(
        warn.text,
        "未检测到图形会话（WAYLAND_DISPLAY 或 DISPLAY），无法读取剪贴板；可直接粘贴本地图片路径"
    );
    assert!(app.input == before, "推 Warn 后输入区应保持不变");
}

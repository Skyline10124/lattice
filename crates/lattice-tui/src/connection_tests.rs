// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 视图推送、历史查询与回执的消费：条目按 NodeId 维护，状态消息按是否新到进入 Dock 或对话记录。
use super::fixtures::*;
use super::*;
use lattice_api::local::LocalErrorKind;

fn turn(n: u64) -> TurnId {
    numbered_turn(n)
}

fn block_id(app: &App, marker: &str) -> BlockId {
    app.blocks
        .entries
        .iter()
        .find(|entry| match &entry.block {
            Block::User(text) | Block::Assistant(text) => text.contains(marker),
            Block::HistoryAssistant(text) => text.text.contains(marker),
            _ => false,
        })
        .unwrap_or_else(|| panic!("no block with {marker}"))
        .id
}

fn docked(app: &App) -> String {
    app.dock
        .message()
        .map_or_else(String::new, |status| status.text.clone())
}

fn tool_states(app: &App) -> Vec<ToolState> {
    app.blocks
        .iter()
        .filter_map(|block| match block {
            Block::Tool { state, .. } => Some(state.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn entries_replace_by_node_in_place_and_new_ones_append() {
    let mut app = test_app(true);
    let t = turn(1);
    open_view(
        &mut app,
        head(2, Some(t), true),
        vec![user(1, t, 1, "prompt"), step(2, t, 0).sent().entry()],
    );
    app.apply_view(text(2, 0, 0, &[("draft answer", false)]));
    let draft = block_id(&app, "draft answer");
    app.push_status(status(NoticeLevel::Info, "between"));
    let closed = || {
        step(2, t, 0)
            .text("draft answer final")
            .tool(1, true)
            .completed_attempt(1234)
            .continued()
            .entry()
    };
    app.apply_view(LocalUpdate::Head(head(3, Some(t), true)));
    app.apply_view(entry_update(closed()));
    app.apply_view(entry_update(step(3, t, 1).sent().entry()));
    app.apply_view(text(3, 0, 0, &[("second step", false)]));
    // 同一节点再推一次（例如推送读到了多一次提交）：显示不变。
    app.apply_view(entry_update(closed()));
    assert_eq!(transcript(&app).matches("prompt").count(), 1);
    assert_eq!(transcript(&app).matches("draft answer").count(), 1);
    assert_eq!(
        block_id(&app, "draft answer final"),
        draft,
        "同一 NodeId 原位替换，块身份不变"
    );
    // 状态消息出现后不移动：第 2 步原位更新，第 3 步接在提示之后。
    assert!(row(&app, "draft answer final") < row(&app, "between"));
    assert!(row(&app, "between") < row(&app, "second step"));
    assert_eq!(app.model.as_deref(), Some("model-a"));
    assert_eq!(app.provider.as_deref(), Some("openai"));
    assert_eq!(app.context_tokens, Some(1234));
}

#[test]
fn own_submission_user_block_is_replaced_by_its_user_message() {
    let mut app = test_app(true);
    open_view(&mut app, head(0, None, false), Vec::new());
    app.input.set_text("hello there");
    assert_eq!(app.submit().as_deref(), Some("hello there"));
    let (t, session) = app.submission_identity().unwrap();
    assert_eq!(session, SubmitSession::Existing(sid()));
    let user_line = |app: &App| {
        transcript(app)
            .lines()
            .find(|line| line.contains("── USER "))
            .unwrap()
            .to_owned()
    };
    assert!(!user_line(&app).contains('#'), "条目未到的一轮还没有序号");
    let pending = block_id(&app, "hello there");
    app.push_status(status(NoticeLevel::Info, "after submit"));
    app.apply_submission(receipt(t, accepted(t)));
    assert!(
        transcript(&app).contains("── unused "),
        "接受即显示助手标题线"
    );
    assert_eq!(app.phase, TurnPhase::Streaming, "会话头指向它之前仍在运行");
    app.apply_view(LocalUpdate::Head(head(2, Some(t), true)));
    app.apply_view(entry_update(user(1, t, 7, "hello there")));
    app.apply_view(entry_update(step(2, t, 0).sent().entry()));
    let shown = transcript(&app);
    assert_eq!(shown.matches("hello there").count(), 1, "{shown}");
    assert_eq!(block_id(&app, "hello there"), pending, "原位取代");
    assert!(row(&app, "hello there") < row(&app, "after submit"));
    assert!(user_line(&app).ends_with("#7 ──"), "序号取自条目");
    assert_eq!(shown.matches("── unused ").count(), 1);
}

#[test]
fn own_submission_stays_busy_until_its_head_is_confirmed() {
    let mut app = test_app(true);
    let a = turn(1);
    open_view(
        &mut app,
        head(2, Some(a), false),
        vec![
            user(1, a, 1, "first"),
            step(2, a, 0).text("done").finished().entry(),
        ],
    );
    app.input.set_text("second");
    app.submit().unwrap();
    let (b, _) = app.submission_identity().unwrap();
    app.input.set_text("queued");
    assert!(app.submit().is_none(), "运行中 Enter 入队");
    app.apply_submission(receipt(b, accepted(b)));
    let busy = |app: &mut App, when: &str| {
        assert_eq!(app.phase, TurnPhase::Streaming, "{when}");
        assert_eq!(
            app.auto_submit(),
            None,
            "{when}: 不能凭上一轮的完成发送排队"
        );
    };
    // B 的会话头已到、当前位置的条目未到：已确认的会话头仍是已完成的 A。
    app.apply_view(LocalUpdate::Head(head(4, Some(b), true)));
    busy(&mut app, "会话头未确认");
    // UserMessage 只证明已接受，仍等会话头确认。
    app.apply_view(entry_update(user(3, b, 2, "second")));
    busy(&mut app, "只有 UserMessage");
    app.apply_action(KeyAction::CancelOrClear);
    assert_eq!(app.cancel_requested.take(), Some(CancelTarget::Own));
    app.apply_view(entry_update(step(4, b, 0).sent().entry()));
    assert_eq!(app.auto_submit(), None);
    assert_eq!(app.queue.messages().count(), 1);
}

#[test]
fn a_new_view_rejudges_the_accepted_turn_it_was_waiting_for() {
    let mut app = test_app(true);
    open_view(&mut app, head(2, Some(turn(1)), false), turns(1..=1));
    app.input.set_text("mine");
    app.submit().unwrap();
    let (mine, _) = app.submission_identity().unwrap();
    app.apply_submission(receipt(mine, accepted(mine)));
    assert_eq!(app.phase, TurnPhase::Streaming, "会话头指向它之前仍在运行");
    // 重连时这一轮已不在新窗口里：会话头在别的轮，例如它之后又跑了十轮以上。
    open_view(&mut app, head(24, Some(turn(12)), false), turns(3..=12));
    assert_eq!(app.phase, TurnPhase::Idle);
    // 接受回执晚于新视图开始时，初始内容可能读于接受之前，仍等会话头。
    app.input.set_text("next");
    app.submit().unwrap();
    let (next, _) = app.submission_identity().unwrap();
    app.begin_view();
    app.apply_submission(receipt(next, accepted(next)));
    app.apply_view(LocalUpdate::Head(head(24, Some(turn(12)), false)));
    for entry in turns(3..=12) {
        app.apply_view(entry_update(entry));
    }
    assert_eq!(app.phase, TurnPhase::Streaming);
}

#[test]
fn steps_after_an_unaccepted_user_block_get_their_own_assistant_title() {
    let mut app = test_app(true);
    open_view(&mut app, head(2, Some(turn(1)), false), turns(1..=1));
    // 其他客户端的一轮已开始、会话头尚未确认时，本客户端照常提交，随后被拒。
    let other = turn(2);
    app.apply_view(LocalUpdate::Head(head(4, Some(other), true)));
    app.apply_view(entry_update(user(3, other, 2, "other prompt")));
    app.input.set_text("mine");
    app.submit().unwrap();
    let (mine, _) = app.submission_identity().unwrap();
    app.apply_view(entry_update(step(4, other, 0).text("other answer").entry()));
    app.apply_submission(receipt(
        mine,
        SubmissionResult::Rejected {
            error: local_error(LocalErrorKind::Busy, "session is busy"),
        },
    ));
    let shown = transcript(&app);
    let lines: Vec<&str> = shown.lines().collect();
    let titled = lines[row(&app, "mine")..row(&app, "other answer")]
        .iter()
        .any(|line| line.starts_with("── unused ") && line.ends_with("#2 ──"));
    assert!(
        titled,
        "接在未被接受的 USER 块之后的步骤另起助手标题线：{shown}"
    );
}

#[test]
fn recovery_not_found_is_an_error_and_returns_the_input() {
    for keep_draft in [false, true] {
        let mut app = test_app(false);
        app.input.set_text("first");
        app.submit().unwrap();
        let (first, _) = app.submission_identity().unwrap();
        app.apply_submission(receipt(first, SubmissionResult::Unknown { error: None }));
        app.input.set_text("second");
        app.submit().unwrap();
        let (second, _) = app.submission_identity().unwrap();
        app.apply_submission(receipt(
            second,
            SubmissionResult::Rejected {
                error: LocalError {
                    conflict: Some(lattice_api::local::ConflictReason::SessionExists),
                    ..local_error(LocalErrorKind::Conflict, "session exists")
                },
            },
        ));
        assert_eq!(app.recover_request.take(), Some(first));
        // 第二条被拒后已放回输入区；用户清空时第一条才放回。
        assert_eq!(app.input.text(), "second");
        if !keep_draft {
            app.input.clear();
        }
        app.apply_recovery(first, Err(local_error(LocalErrorKind::NotFound, "no turn")));
        let error = app.dock.message().unwrap();
        assert_eq!(error.level, NoticeLevel::Error);
        assert!(
            error.text.contains("未被接受") && error.text.contains(&first.to_string()),
            "{}",
            error.text
        );
        if keep_draft {
            assert_eq!(app.input.text(), "second", "不覆盖输入区已有的内容");
            assert!(error.text.contains("Ctrl+↑"), "{}", error.text);
        } else {
            assert_eq!(app.input.text(), "first", "提交确定未被接受，输入保留");
            assert!(error.text.contains("输入已保留"), "{}", error.text);
        }
        assert_eq!(app.phase, TurnPhase::Idle);
    }
}

#[test]
fn new_view_and_path_replacement_discard_entries_but_keep_statuses_and_unaccepted_user() {
    let mut app = test_app(true);
    let (a, b) = (turn(1), turn(2));
    open_view(&mut app, head(2, Some(a), false), turns(1..=1));
    app.push_status(status(NoticeLevel::Info, "kept status"));
    app.input.set_text("rejected input");
    app.submit().unwrap();
    let (rejected, _) = app.submission_identity().unwrap();
    app.apply_submission(receipt(
        rejected,
        SubmissionResult::Rejected {
            error: local_error(LocalErrorKind::Busy, "session is busy"),
        },
    ));
    assert_eq!(app.input.text(), "rejected input");
    app.input.clear();
    let replaced = SessionHead {
        path_replaced: true,
        ..head(4, Some(b), true)
    };
    app.apply_view(LocalUpdate::Head(replaced));
    app.apply_view(entry_update(user(3, b, 1, "new prompt")));
    app.apply_view(entry_update(step(4, b, 0).sent().entry()));
    app.apply_view(text(4, 0, 0, &[("partial", false)]));
    let shown = transcript(&app);
    assert!(
        !shown.contains("prompt-1") && !shown.contains("answer-1"),
        "{shown}"
    );
    for kept in ["kept status", "rejected input", "new prompt", "partial"] {
        assert_eq!(shown.matches(kept).count(), 1, "{kept}: {shown}");
    }
    // 重新开始 watch：生成中的文字随旧条目丢弃，等新视图从头给出。
    open_view(
        &mut app,
        head(4, Some(b), true),
        vec![user(3, b, 1, "new prompt"), step(4, b, 0).sent().entry()],
    );
    let shown = transcript(&app);
    assert!(!shown.contains("partial"), "{shown}");
    for kept in ["kept status", "rejected input", "new prompt"] {
        assert_eq!(shown.matches(kept).count(), 1, "{kept}: {shown}");
    }
    assert!(row(&app, "kept status") < row(&app, "new prompt"));
}

/// 打开第 3、4 轮并翻到顶部；返回发出的历史查询。
fn scrolled_to_top(app: &mut App) -> OlderRequest {
    app.term_width = 80;
    app.term_height = 12;
    open_view(app, head(8, Some(turn(4)), false), turns(3..=4));
    for _ in 0..20 {
        app.apply_action(KeyAction::ScrollPageUp);
        if let Some(request) = app.older_request.take() {
            return request;
        }
    }
    panic!("翻到顶部而第 1 轮尚未装入时应查询更早的轮");
}

fn top_line(app: &mut App) -> String {
    app.prepare_transcript();
    crate::view::transcript_lines(app, 80)[app.viewport.scroll()].to_string()
}

#[test]
fn older_turns_insert_before_the_earliest_turn_and_keep_the_view() {
    let mut app = test_app(true);
    let request = scrolled_to_top(&mut app);
    assert_eq!(
        request.before,
        NodeId(5),
        "以已装入的最早一轮的 UserMessage 为 before"
    );
    assert_eq!(request.session_id, sid());
    let top = top_line(&mut app);
    app.apply_older(
        request,
        Ok(LocalHistory {
            entries: turns(1..=2),
        }),
    );
    assert_eq!(top_line(&mut app), top, "视图停在原来看到的内容上");
    assert!(row(&app, "prompt-1") < row(&app, "answer-2"));
    assert!(row(&app, "answer-2") < row(&app, "prompt-3"));
    // 查询带来的结局不是新消息，直接进对话记录。
    assert!(app.dock.message().is_none(), "{}", docked(&app));
    assert_eq!(
        transcript(&app).matches("失败：provider failure").count(),
        4
    );
    for _ in 0..20 {
        app.apply_action(KeyAction::ScrollPageUp);
    }
    assert_eq!(app.older_request, None, "第 1 轮已装入");
}

#[test]
fn older_turns_keep_the_view_on_the_turns_below_the_banner_and_a_leading_status() {
    let mut app = test_app(true);
    app.show_banner();
    app.push_status(status(NoticeLevel::Info, "leading status"));
    let request = scrolled_to_top(&mut app);
    assert_eq!(app.focus, Focus::Dock);
    // 产品中翻到顶部时第 0 行总是横幅；插入点在横幅与前置状态消息之后。
    assert!(top_line(&mut app).contains("LATTICE"));
    let below = row(&app, "leading status") + 1;
    let screen = |app: &mut App| {
        app.prepare_transcript();
        let height = app.transcript_height(crate::view::transcript_lines(app, 80).len());
        crate::view::transcript_lines(app, 80)
            .iter()
            .skip(app.viewport.scroll())
            .take(height)
            .map(ToString::to_string)
            .collect::<Vec<_>>()
    };
    let before = screen(&mut app);
    assert!(below < before.len(), "第 3 轮的开头在屏幕上：{before:#?}");
    app.apply_older(
        request,
        Ok(LocalHistory {
            entries: turns(1..=2),
        }),
    );
    // 原来看到的第 3 轮仍在屏幕上同一位置；横幅仍在对话记录最上方，翻到顶部才看得到。
    assert_eq!(screen(&mut app)[below..], before[below..]);
    assert!(row(&app, "leading status") < row(&app, "prompt-1"));
    assert!(row(&app, "LATTICE") < row(&app, "leading status"));
}

#[test]
fn older_turns_failure_warns_and_requeries_on_the_next_top() {
    let mut app = test_app(true);
    let request = scrolled_to_top(&mut app);
    app.apply_older(
        request,
        Err(local_error(
            LocalErrorKind::StoreUnavailable,
            "history read failed",
        )),
    );
    let warn = app.dock.message().unwrap();
    assert_eq!(warn.level, NoticeLevel::Warn);
    assert!(warn.text.contains("history read failed"), "{}", warn.text);
    assert_eq!(app.older_request, None, "失败后不自动重查");
    app.apply_action(KeyAction::Insert(' '));
    app.apply_action(KeyAction::ScrollPageUp);
    assert_eq!(app.older_request, Some(request), "下次翻到顶部时重新查询");
}

#[test]
fn older_turns_arriving_after_path_replacement_are_dropped() {
    let mut app = test_app(true);
    let request = scrolled_to_top(&mut app);
    app.apply_view(LocalUpdate::Head(SessionHead {
        path_replaced: true,
        ..head(8, Some(turn(4)), false)
    }));
    for entry in turns(3..=4) {
        app.apply_view(entry_update(entry));
    }
    app.apply_older(
        request,
        Ok(LocalHistory {
            entries: turns(1..=2),
        }),
    );
    assert!(!transcript(&app).contains("prompt-1"));
    for _ in 0..20 {
        app.apply_action(KeyAction::ScrollPageUp);
    }
    let again = app.older_request.expect("新路径照常往上翻");
    assert_ne!(again.epoch, request.epoch);
}

#[test]
fn generating_text_is_replaced_by_the_entry_and_restarts_on_retry() {
    let mut app = test_app(true);
    let t = turn(1);
    open_view(
        &mut app,
        head(2, Some(t), true),
        vec![user(1, t, 1, "prompt"), step(2, t, 0).sent().entry()],
    );
    // reasoning 不显示，但计入续发的字节位置。
    app.apply_view(text(2, 0, 0, &[("Hel", false), ("think", true)]));
    app.apply_view(text(2, 0, 8, &[("lo", false)]));
    let shown = transcript(&app);
    assert!(
        shown.contains("Hello") && !shown.contains("think"),
        "{shown}"
    );
    app.push_status(status(NoticeLevel::Info, "after text"));
    let retrying = || step(2, t, 0).retried("info", "HTTP 429");
    app.apply_view(LocalUpdate::Head(head(2, Some(t), true)));
    app.apply_view(entry_update(retrying().sent().entry()));
    assert!(
        !transcript(&app).contains("Hello"),
        "失败的尝试的文字随之移除"
    );
    app.apply_view(text(2, 1, 0, &[("New", false)]));
    app.apply_view(text(2, 1, 3, &[(" answer", false)]));
    assert!(
        row(&app, "New answer") < row(&app, "after text"),
        "显示在所属 Step 的位置"
    );
    app.apply_view(entry_update(
        retrying()
            .completed_attempt(10)
            .text("New answer")
            .finished()
            .entry(),
    ));
    app.apply_view(text(2, 1, 10, &[("!", false)]));
    let shown = transcript(&app);
    assert_eq!(shown.matches("New answer").count(), 1, "{shown}");
    assert!(!shown.contains("New answer!"), "{shown}");
    assert!(row(&app, "New answer") < row(&app, "after text"));
}

#[test]
fn retry_notices_and_closings_reach_the_dock_only_when_new() {
    let mut app = test_app(true);
    let (a, b) = (turn(1), turn(2));
    let running = |retries: &[(&str, &str)]| {
        let mut step = step(4, b, 0);
        for (level, text) in retries {
            step = step.retried(level, text);
        }
        step
    };
    open_view(
        &mut app,
        head(4, Some(b), true),
        vec![
            user(1, a, 1, "first"),
            step(2, a, 0)
                .retried("warn", "old retry")
                .completed_attempt(1)
                .text("first answer")
                .stopped("failed")
                .entry(),
            user(3, b, 2, "second"),
            running(&[("warn", "initial retry")]).sent().entry(),
        ],
    );
    assert!(
        app.dock.message().is_none(),
        "初始条目中的提示与结局不打断输入"
    );
    let shown = transcript(&app);
    for old in ["old retry", "initial retry", "失败：provider failure"] {
        assert_eq!(shown.matches(old).count(), 1, "{old}: {shown}");
    }
    let retries = [
        ("warn", "initial retry"),
        ("info", "info retry"),
        ("error", "fresh retry"),
    ];
    app.apply_view(LocalUpdate::Head(head(4, Some(b), true)));
    app.apply_view(entry_update(running(&retries).sent().entry()));
    // 等级取自条目：Info 直接进对话记录，Error 暂驻 Dock 且不在条目中重复。
    assert!(docked(&app).contains("fresh retry"), "{}", docked(&app));
    assert_eq!(app.dock.message().unwrap().level, NoticeLevel::Error);
    assert!(transcript(&app).contains("info retry"));
    assert!(!transcript(&app).contains("fresh retry"));
    app.apply_view(entry_update(running(&retries).sent().entry()));
    assert!(
        !transcript(&app).contains("fresh retry"),
        "再推同一条目不重复"
    );
    app.apply_action(KeyAction::Insert(' '));
    assert!(
        row(&app, "info retry") < row(&app, "fresh retry"),
        "释放后回到条目中的原位"
    );
    app.apply_view(entry_update(
        running(&retries)
            .completed_attempt(1)
            .text("second answer")
            .stopped("failed")
            .entry(),
    ));
    assert!(
        docked(&app).contains(&format!("turn {b} 失败")),
        "{}",
        docked(&app)
    );
    assert!(!transcript(&app).contains(&format!("turn {b} 失败")));
    app.apply_action(KeyAction::Insert(' '));
    assert!(row(&app, "second answer") < row(&app, &format!("turn {b} 失败")));
    // 暂驻的提示不随条目丢弃而消失；释放后所属条目已不在，不再出现。
    let c = turn(3);
    app.apply_view(LocalUpdate::Head(head(6, Some(c), true)));
    app.apply_view(entry_update(user(5, c, 3, "third")));
    app.apply_view(entry_update(
        step(6, c, 0)
            .retried("warn", "dropped retry")
            .sent()
            .entry(),
    ));
    assert!(docked(&app).contains("dropped retry"));
    open_view(&mut app, head(2, Some(a), false), turns(1..=1));
    assert!(docked(&app).contains("dropped retry"));
    app.apply_action(KeyAction::Insert(' '));
    assert!(!transcript(&app).contains("dropped retry"));
}

#[test]
fn reconnect_keeps_shown_content_until_the_first_initial_entry() {
    let mut app = test_app(true);
    let (a, b) = (turn(1), turn(2));
    let mut entries = turns(1..=1);
    entries.push(user(3, b, 2, "second"));
    entries.push(step(4, b, 0).sent().entry());
    open_view(&mut app, head(4, Some(b), true), entries);
    app.apply_view(text(4, 0, 0, &[("partial", false)]));
    // 重连后只收到会话头就断开：已显示的条目、结局与生成中的文字保留。
    app.begin_view();
    app.apply_view(LocalUpdate::Head(head(4, Some(b), true)));
    app.apply_view(LocalUpdate::Connection {
        status: ConnectionStatus::Eof,
    });
    let shown = transcript(&app);
    let closing = format!("turn {a} 失败");
    for kept in [
        "prompt-1",
        "answer-1",
        closing.as_str(),
        "second",
        "partial",
    ] {
        assert_eq!(shown.matches(kept).count(), 1, "{kept}: {shown}");
    }
    // 再次重连：首个初始条目到达时才按新视图替换。
    app.begin_view();
    app.apply_view(LocalUpdate::Head(head(4, Some(b), true)));
    assert!(transcript(&app).contains("prompt-1"));
    app.apply_view(entry_update(user(3, b, 2, "second")));
    let shown = transcript(&app);
    assert!(
        !shown.contains("prompt-1") && !shown.contains("partial"),
        "{shown}"
    );
    app.apply_view(entry_update(step(4, b, 0).sent().entry()));
    assert_eq!(transcript(&app).matches("second").count(), 1);
}

#[test]
fn parked_message_reloaded_with_a_new_path_is_not_shown_twice() {
    let mut app = test_app(true);
    let t = turn(1);
    let running = |retried: bool| {
        let step = step(2, t, 0);
        let step = if retried {
            step.retried("error", "parked retry")
        } else {
            step
        };
        step.sent().entry()
    };
    open_view(
        &mut app,
        head(2, Some(t), true),
        vec![user(1, t, 1, "prompt"), running(false)],
    );
    app.apply_view(LocalUpdate::Head(head(2, Some(t), true)));
    app.apply_view(entry_update(running(true)));
    assert!(docked(&app).contains("parked retry"), "{}", docked(&app));
    // 新路径仍含这个节点与尝试：条目重建后，暂驻期间仍不在条目中重复显示。
    app.apply_view(LocalUpdate::Head(SessionHead {
        path_replaced: true,
        ..head(2, Some(t), true)
    }));
    app.apply_view(entry_update(user(1, t, 1, "prompt")));
    app.apply_view(entry_update(running(true)));
    assert!(docked(&app).contains("parked retry"));
    assert!(
        !transcript(&app).contains("parked retry"),
        "{}",
        transcript(&app)
    );
    app.apply_action(KeyAction::Insert(' '));
    assert_eq!(transcript(&app).matches("parked retry").count(), 1);
    // 结局按 TurnId：暂驻时重连，新视图再给出同一轮的结局也不重复显示。
    let stopped = || step(2, t, 0).text("answer").stopped("failed").entry();
    app.apply_view(LocalUpdate::Head(head(2, Some(t), false)));
    app.apply_view(entry_update(stopped()));
    let closing = format!("turn {t} 失败");
    assert!(docked(&app).contains(&closing), "{}", docked(&app));
    open_view(
        &mut app,
        head(2, Some(t), false),
        vec![user(1, t, 1, "prompt"), stopped()],
    );
    assert!(docked(&app).contains(&closing));
    assert!(!transcript(&app).contains(&closing), "{}", transcript(&app));
    app.apply_action(KeyAction::Insert(' '));
    assert_eq!(transcript(&app).matches(&closing).count(), 1);
}

#[test]
fn ownerless_head_ends_the_turn_and_warns_once_per_view() {
    let mut app = test_app(true);
    let t = turn(1);
    let entries = || {
        vec![
            user(1, t, 1, "prompt"),
            step(2, t, 0)
                .text("calling")
                .tool(1, false)
                .completed_attempt(1)
                .executing()
                .entry(),
        ]
    };
    open_view(&mut app, head(2, Some(t), true), entries());
    assert_eq!(tool_states(&app), [ToolState::Running]);
    assert_eq!(app.phase, TurnPhase::Streaming);
    // 会话头之后紧接着给出当前位置的条目；收到它之前不拿新会话头配旧条目判定。
    app.apply_view(LocalUpdate::Head(head(2, Some(t), false)));
    assert_eq!(app.phase, TurnPhase::Streaming);
    assert!(app.dock.message().is_none(), "{}", docked(&app));
    app.apply_view(entry_update(entries().pop().unwrap()));
    assert_eq!(app.phase, TurnPhase::Idle);
    assert!(
        matches!(tool_states(&app)[..], [ToolState::Failed { .. }]),
        "结束后按会话库给出的结论显示：{:?}",
        tool_states(&app)
    );
    let warn = app.dock.message().unwrap().clone();
    assert_eq!(warn.level, NoticeLevel::Warn);
    assert!(warn.text.contains("执行者已经消失"), "{}", warn.text);
    // 结局先作为新消息暂驻，随后被 Warn 顶替，落回所属条目。
    assert_eq!(
        transcript(&app).matches(&format!("turn {t} 失败")).count(),
        1
    );
    app.apply_view(LocalUpdate::Head(head(2, Some(t), false)));
    app.apply_view(entry_update(entries().pop().unwrap()));
    app.apply_action(KeyAction::Insert(' '));
    assert_eq!(transcript(&app).matches("执行者已经消失").count(), 1);
    open_view(&mut app, head(2, Some(t), false), entries());
    assert!(
        docked(&app).contains("执行者已经消失"),
        "新视图开始时再提示一次"
    );
    assert_eq!(
        transcript(&app).matches(&format!("turn {t} 失败")).count(),
        1
    );

    // 没有 Owner 的会话头先于关闭这一步的条目逐条到达：等到当前位置的条目再判定，不误报。
    let mut app = test_app(true);
    open_view(&mut app, head(2, Some(t), true), entries());
    app.apply_view(LocalUpdate::Head(head(2, Some(t), false)));
    app.apply_view(entry_update(step(2, t, 0).text("done").finished().entry()));
    assert_eq!(app.phase, TurnPhase::Idle);
    assert!(!docked(&app).contains("执行者已经消失"), "{}", docked(&app));
    assert!(!transcript(&app).contains("执行者已经消失"));
    assert!(!transcript(&app).contains(&format!("turn {t} 失败")));
}

#[test]
fn head_diagnostic_is_one_error() {
    let mut app = test_app(true);
    let diagnosed = SessionHead {
        diagnostic: Some(local_error(
            LocalErrorKind::StoreUnavailable,
            "commit failed",
        )),
        ..head(0, None, false)
    };
    open_view(&mut app, diagnosed.clone(), Vec::new());
    app.apply_view(LocalUpdate::Head(diagnosed));
    let error = app.dock.message().unwrap();
    assert_eq!(error.level, NoticeLevel::Error);
    assert!(error.text.contains("commit failed"));
    app.apply_action(KeyAction::Insert(' '));
    assert_eq!(transcript(&app).matches("commit failed").count(), 1);
}

#[test]
fn lost_observation_marks_running_tools_unknown_and_keeps_the_turn_running() {
    let mut app = test_app(true);
    let t = turn(1);
    open_view(
        &mut app,
        head(2, Some(t), true),
        vec![
            user(1, t, 1, "prompt"),
            step(2, t, 0)
                .tool(1, false)
                .completed_attempt(1)
                .executing()
                .entry(),
        ],
    );
    app.apply_view(LocalUpdate::Connection {
        status: ConnectionStatus::Idle,
    });
    assert_eq!(app.phase, TurnPhase::Streaming);
    assert!(!app.connected);
    assert!(app.queue.is_paused());
    assert_eq!(tool_states(&app), [ToolState::Unknown]);
    let warn = app.dock.message().unwrap();
    assert_eq!(
        (warn.level.clone(), warn.closing.clone()),
        (NoticeLevel::Warn, None)
    );
}

#[test]
fn own_messages_carry_their_level() {
    use CancelStatus::*;
    type Apply = Box<dyn Fn(&mut App, TurnId)>;
    let cases: Vec<(&str, Apply, NoticeLevel, &str)> = vec![
        (
            "rejected",
            Box::new(|app, id| {
                app.apply_submission(receipt(
                    id,
                    SubmissionResult::Rejected {
                        error: local_error(LocalErrorKind::BadRequest, "bad profile"),
                    },
                ))
            }),
            NoticeLevel::Error,
            "bad profile",
        ),
        (
            "not accepted",
            // 按 TurnId 查询确认后台没有这个请求。
            Box::new(|app, id| {
                app.apply_submission(LocalUpdate::Submit {
                    update: SubmissionUpdate::Receipt(SubmissionReceipt {
                        turn_id: id,
                        result: SubmissionResult::NotAccepted,
                        recovered: true,
                    }),
                })
            }),
            NoticeLevel::Error,
            "未被接受",
        ),
        (
            "unknown",
            Box::new(|app, id| {
                app.apply_submission(receipt(
                    id,
                    SubmissionResult::Unknown {
                        error: Some(local_error(LocalErrorKind::Transport, "socket closed")),
                    },
                ))
            }),
            NoticeLevel::Warn,
            "socket closed",
        ),
        (
            "recovered",
            Box::new(|app, id| {
                app.apply_submission(LocalUpdate::Submit {
                    update: SubmissionUpdate::Receipt(SubmissionReceipt {
                        turn_id: id,
                        result: accepted(id),
                        recovered: true,
                    }),
                })
            }),
            NoticeLevel::Info,
            "按 TurnId 查到已接受的请求",
        ),
        (
            "cancelled before sending",
            Box::new(|app, id| {
                app.apply_submission(LocalUpdate::Submit {
                    update: SubmissionUpdate::Receipt(SubmissionReceipt {
                        turn_id: id,
                        result: SubmissionResult::NotAccepted,
                        recovered: false,
                    }),
                })
            }),
            NoticeLevel::Info,
            "提交在发出前取消",
        ),
        (
            "cancel taken",
            Box::new(|app, id| app.apply_cancel_result(id, Ok(true))),
            NoticeLevel::Info,
            "后台已接管取消",
        ),
        (
            "cancel not taken",
            Box::new(|app, id| app.apply_cancel_result(id, Ok(false))),
            NoticeLevel::Warn,
            "后台未接管本次取消",
        ),
        (
            "cancel error",
            Box::new(|app, id| {
                app.apply_cancel_result(
                    id,
                    Err(local_error(LocalErrorKind::Internal, "cancel denied")),
                )
            }),
            NoticeLevel::Error,
            "cancel denied",
        ),
        (
            "own cancel unknown",
            Box::new(|app, id| {
                app.apply_submission(LocalUpdate::Cancel {
                    turn_id: id,
                    status: Unknown,
                    owned: None,
                    error: None,
                })
            }),
            NoticeLevel::Warn,
            "取消确认未知",
        ),
    ];
    for (name, apply, level, marker) in cases {
        let mut app = test_app(false);
        app.input.set_text("prompt");
        app.submit().unwrap();
        let (id, _) = app.submission_identity().unwrap();
        apply(&mut app, id);
        let pushed = app
            .blocks
            .iter()
            .filter_map(|block| match block {
                Block::Status(status) => Some(status.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(pushed.len(), 1, "{name}: 一件事一条消息 {pushed:?}");
        assert_eq!(pushed[0].level, level, "{name}");
        assert!(
            pushed[0].text.contains(marker),
            "{name}: {}",
            pushed[0].text
        );
        assert_eq!(
            app.dock.message().is_some(),
            level != NoticeLevel::Info,
            "{name}"
        );
        if ["rejected", "not accepted", "cancelled before sending"].contains(&name) {
            assert_eq!(app.input.text(), "prompt", "{name}: 未被接受的输入归还");
        }
    }
}

#[test]
fn detach_lines_describe_receipts_cancel_and_outcome_in_words() {
    let id = turn(9);
    let report =
        |submission, summary: Option<lattice_api::local::TurnSummary>, owner| DetachReport {
            session_id: sid(),
            turn_id: Some(id),
            submission,
            submission_diagnostic: Some(local_error(LocalErrorKind::Transport, "late failure")),
            cancel: Some(CancelStatus::Unknown),
            cancel_error: Some(local_error(LocalErrorKind::Internal, "cancel denied")),
            head: Some(head(2, Some(id), owner)),
            summary,
        };
    let failed = match step(2, id, 0).stopped("failed").entry() {
        ConversationEntry::Step { summary, .. } => summary,
        ConversationEntry::UserMessage { .. } => unreachable!(),
    };
    let lines = detach_lines(
        report(
            Some(SubmissionReceipt {
                turn_id: id,
                result: SubmissionResult::Unknown { error: None },
                recovered: false,
            }),
            failed.clone(),
            true,
        ),
        Viewed::Unknown,
    );
    let text = lines.join("\n");
    for marker in [
        "提交结果未知",
        "late failure",
        "取消未确认",
        "cancel denied",
        "失败：provider failure",
    ] {
        assert!(text.contains(marker), "{marker}: {text}");
    }
    assert!(lines.iter().all(|line| !line.contains("Some(")), "{text}");
    // 已显示的结局不重述；未关闭且仍有 Owner 时 summary 不是结局。
    assert!(
        !detach_lines(report(None, failed, true), Viewed::ClosingShown)
            .join("\n")
            .contains("失败")
    );
    // 退出报告中的未被接受同样按来源区分：不来自查询的是发出前取消。
    let not_accepted = |recovered| {
        detach_lines(
            DetachReport {
                submission: Some(SubmissionReceipt {
                    turn_id: id,
                    result: SubmissionResult::NotAccepted,
                    recovered,
                }),
                submission_diagnostic: None,
                cancel: None,
                cancel_error: None,
                ..report(None, None, true)
            },
            Viewed::Unknown,
        )
        .join("\n")
    };
    let cancelled = not_accepted(false);
    assert!(cancelled.contains("提交在发出前取消"), "{cancelled}");
    assert!(!cancelled.contains("查询"), "{cancelled}");
    let queried = not_accepted(true);
    assert!(
        queried.contains("按 TurnId 查询确认后台没有这个请求"),
        "{queried}"
    );
    let running = match step(2, id, 0).entry() {
        ConversationEntry::Step { summary, .. } => summary,
        ConversationEntry::UserMessage { .. } => unreachable!(),
    };
    assert!(
        !detach_lines(report(None, running.clone(), true), Viewed::Unknown)
            .join("\n")
            .contains(&format!("Turn {id}"))
    );
    // 会话头在别的轮：app 按已确认的会话头与条目判定这一轮已不在执行时照常给出结局；
    // 只凭报告中的会话头不算，它可能早于这一轮被接受。
    let elsewhere = || DetachReport {
        head: Some(head(4, Some(turn(10)), true)),
        ..report(None, running.clone(), true)
    };
    let outcome = format!("Turn {id} 失败");
    assert!(
        detach_lines(elsewhere(), Viewed::Ended)
            .join("\n")
            .contains(&outcome)
    );
    assert!(
        !detach_lines(elsewhere(), Viewed::Unknown)
            .join("\n")
            .contains(&outcome)
    );
    assert!(
        detach_lines(report(None, running, false), Viewed::Unknown)
            .join("\n")
            .contains(&format!("Turn {id} 失败"))
    );
}

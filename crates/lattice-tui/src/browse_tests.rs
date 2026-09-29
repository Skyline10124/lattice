// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn app() -> App {
    App::new(
        SessionContext {
            session_id: None,
            active_agent: "agent".into(),
        },
        false,
        Path::new("/nonexistent-tui-test-history"),
    )
}

fn cursor(app: &App) -> crate::view::browse::BrowseCursor {
    let Focus::Transcript(cursor) = app.focus else {
        panic!("expected browse focus")
    };
    cursor
}

#[test]
fn browse_moves_in_cells_keeps_desired_column_and_scrolls_at_edges() {
    let mut app = app();
    app.term_height = 10;
    app.blocks.push(Block::Assistant(format!(
        "{}abcdef\nx\n你好吗abc\n\nend",
        "prefix\n".repeat(20)
    )));
    app.apply_action(KeyAction::Up);
    app.apply_action(KeyAction::Home);
    app.apply_action(KeyAction::Up);
    app.apply_action(KeyAction::Up);
    let current = |app: &App| {
        let c = cursor(app);
        (
            c.row,
            c.column(&transcript_lines(app, 78), 78),
            c.desired_col,
        )
    };
    app.apply_action(KeyAction::Right);
    assert_eq!((current(&app).1, current(&app).2), (2, 2));
    app.apply_action(KeyAction::End);
    assert_eq!(current(&app).1, 8);
    app.apply_action(KeyAction::Up);
    assert_eq!((current(&app).1, current(&app).2), (0, 8));
    app.apply_action(KeyAction::Up);
    assert_eq!((current(&app).1, current(&app).2), (5, 8));
    app.apply_action(KeyAction::Down);
    app.apply_action(KeyAction::Down);
    assert_eq!(current(&app).1, 8);
    app.apply_action(KeyAction::Left);
    assert_eq!(current(&app).1, 7);
    while current(&app).0 > app.viewport.scroll() {
        app.apply_action(KeyAction::Up);
    }
    let scroll = app.viewport.scroll();
    assert!(scroll > 0);
    app.apply_action(KeyAction::Up);
    assert_eq!(app.viewport.scroll(), scroll - 1);
    while current(&app).0 < app.viewport.scroll() + app.transcript_height() - 1 {
        app.apply_action(KeyAction::Down);
    }
    let scroll = app.viewport.scroll();
    app.apply_action(KeyAction::Down);
    assert_eq!(app.viewport.scroll(), scroll + 1);
    app.apply_action(KeyAction::Home);
    assert_eq!((current(&app).1, current(&app).2), (0, 0));
}

#[test]
fn browse_entry_exit_conditions_and_reserved_keys() {
    let mut empty = app();
    empty.apply_action(KeyAction::Up);
    assert_eq!(empty.focus, Focus::Dock);
    for exit in [
        KeyAction::Down,
        KeyAction::Clear,
        KeyAction::Insert('x'),
        KeyAction::ModifiedSpace,
        KeyAction::Paste("x\ny".into()),
    ] {
        let mut app = app();
        app.term_width = 7; // content_w = 5; two soft rows
        app.blocks.push(Block::Assistant("body".into()));
        app.input.set_text("abcdef");
        app.apply_action(KeyAction::Up);
        assert_eq!(app.focus, Focus::Dock, "not on the first visual input row");
        app.apply_action(KeyAction::Up);
        assert!(matches!(app.focus, Focus::Transcript(_)));
        assert!(!app.viewport.following());
        let input = app.input.text();
        let position = app.input.cursor;
        for action in [
            KeyAction::Submit,
            KeyAction::Newline,
            KeyAction::Delete,
            KeyAction::Backspace,
            KeyAction::HistoryPrev,
        ] {
            assert!(app.apply_action(action).is_none());
            assert_eq!(app.input.text(), input);
            assert_eq!(app.input.cursor, position);
        }
        let suffix = match &exit {
            KeyAction::Insert(_) => "x",
            KeyAction::ModifiedSpace => " ",
            KeyAction::Paste(_) => "x\ny",
            _ => "",
        };
        app.apply_action(exit);
        assert_eq!(app.focus, Focus::Dock);
        assert!(app.viewport.following());
        assert_eq!(app.input.text(), format!("a{suffix}bcdef"));
    }
    let mut app = app();
    app.blocks.push(Block::Assistant("body".into()));
    app.apply_action(KeyAction::Up);
    handle_input_event(
        &mut app,
        Event::Key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL)),
    );
    assert!(
        !app.should_quit,
        "Ctrl+D only exits from empty input, not browsing"
    );
    app.input.set_text("draft");
    handle_input_event(
        &mut app,
        Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
    );
    assert!(app.input.is_empty());
    assert!(matches!(app.focus, Focus::Transcript(_)));
    handle_input_event(
        &mut app,
        Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
    );
    assert!(app.should_quit);
}

#[test]
fn browse_paging_clamps_cursor_and_only_control_end_resumes_following() {
    let mut app = app();
    app.term_height = 10;
    app.blocks
        .push(Block::Assistant(vec!["abcdef"; 90].join("\n")));
    app.input.set_text("abc");
    let input_cursor = app.input.cursor;
    app.apply_action(KeyAction::ScrollPageUp);
    assert_eq!(app.focus, Focus::Dock);
    assert_eq!(app.input.cursor, input_cursor);
    app.apply_action(KeyAction::Up);
    for action in [
        KeyAction::ScrollPageUp,
        KeyAction::ScrollPageUp,
        KeyAction::ScrollPageDown,
        KeyAction::ScrollPageDown,
    ] {
        app.apply_action(action);
        let c = cursor(&app);
        assert!(
            c.row >= app.viewport.scroll()
                && c.row < app.viewport.scroll() + app.transcript_height(),
            "cursor must stay on screen"
        );
        assert_eq!(c.desired_col, 3);
        assert!(!app.viewport.following());
    }
    app.apply_action(KeyAction::ScrollToBottom);
    assert!(app.viewport.following());
    assert_eq!(cursor(&app).desired_col, 3);
    assert!(cursor(&app).row >= app.viewport.scroll());
    app.apply_builtin("/clear");
    assert_eq!(app.focus, Focus::Dock);
}

#[test]
fn zero_visible_area_returns_to_input_and_prevents_browse_entry() {
    for (width, height) in [(2, 24), (80, 1), (0, 0)] {
        let mut app = app();
        app.blocks.push(Block::Assistant("body".into()));
        app.apply_action(KeyAction::Up);
        assert!(matches!(app.focus, Focus::Transcript(_)));
        handle_input_event(&mut app, Event::Resize(width, height));
        app.prepare_transcript();
        assert_eq!(app.focus, Focus::Dock);
        assert!(app.viewport.following());
        app.apply_action(KeyAction::ScrollPageUp);
        app.apply_action(KeyAction::Up);
        assert_eq!(app.focus, Focus::Dock);
        assert!(app.viewport.following());
    }
}

#[test]
fn history_relocates_browse_anchor_and_missing_block_to_nearest_start() {
    let mut app = app();
    app.term_width = 24;
    app.term_height = 10;
    let turn_id = lattice_api::local::new_turn_id();
    let key = ModelAttemptKey {
        turn_id,
        model_step_ordinal: 0,
        attempt_ordinal: 1,
    };
    app.apply_turn_event(TurnEvent::TokenDelta {
        key,
        content: "first\nsecond\nthird\nfourth\nfifth\nsixth".into(),
    });
    app.input.set_text("abc");
    app.apply_action(KeyAction::Up);
    for _ in 0..3 {
        app.apply_action(KeyAction::Up);
    }
    let before = cursor(&app);
    let screen_row = before.row.saturating_sub(app.viewport.scroll());
    let history = vec![
        Block::AgentName("agent".into()),
        Block::UserTurn(turn_id, std::num::NonZeroUsize::MIN),
        Block::User("long prompt\n".repeat(15)),
        Block::Step(StepKey {
            turn_id,
            ordinal: 0,
        }),
        Block::Assistant("first\nsecond\nthird\nfourth\nfifth\nsixth".into()),
    ];
    app.replace_history(history.clone());
    app.prepare_transcript();
    let after = cursor(&app);
    assert_eq!(after.block, before.block);
    assert_eq!(after.row.saturating_sub(app.viewport.scroll()), screen_row);
    assert!(
        after.row > before.row,
        "newly loaded prefix relocates global row"
    );
    assert_eq!(after.desired_col, 3);
    app.apply_action(KeyAction::ScrollToBottom);
    // Rewind to UserMessage removes the Step; no mutation of a saved node.
    app.replace_history(history.into_iter().take(3).collect());
    app.prepare_transcript();
    let removed = cursor(&app);
    assert_ne!(removed.block.id, before.block.id);
    assert_eq!(removed.block.row, 0);
    assert_eq!(removed.desired_col, 0);
}

#[test]
fn streaming_preserves_browse_position_and_control_end_follows_new_content() {
    let mut app = app();
    app.term_height = 10;
    let key = ModelAttemptKey {
        turn_id: lattice_api::local::new_turn_id(),
        model_step_ordinal: 0,
        attempt_ordinal: 1,
    };
    app.apply_turn_event(TurnEvent::TokenDelta {
        key,
        content: "line\n".repeat(30),
    });
    app.apply_action(KeyAction::Up);
    app.apply_action(KeyAction::Up);
    let before = app.focus;
    let scroll = app.viewport.scroll();
    app.apply_turn_event(TurnEvent::TokenDelta {
        key,
        content: "more\n".repeat(20),
    });
    app.prepare_transcript();
    assert_eq!(app.focus, before);
    assert_eq!(app.viewport.scroll(), scroll);
    assert!(app.viewport.hidden_below() >= 20);
    app.apply_action(KeyAction::ScrollToBottom);
    app.apply_turn_event(TurnEvent::TokenDelta {
        key,
        content: "last\n".repeat(20),
    });
    let transcript = app.prepare_transcript();
    assert!(app.viewport.following());
    assert_eq!(
        app.viewport.scroll(),
        transcript.lines.len() - app.transcript_height()
    );
    assert!(cursor(&app).row >= app.viewport.scroll());
}

fn show_status(app: &mut App, severity: DockSeverity, text: &str) {
    let kind = match severity {
        DockSeverity::Warn => StatusKind::LlmRetry {
            attempt: 1,
            delay_ms: 100,
            message: text.into(),
        },
        DockSeverity::Error => StatusKind::LlmFailed {
            attempt: 1,
            message: text.into(),
        },
    };
    app.push_dock_status(kind, severity);
}

#[test]
fn following_view_stays_at_bottom_when_the_dock_grows_or_shrinks() {
    let mut app = app();
    app.term_height = 24;
    app.blocks.push(Block::Assistant("body\n".repeat(60)));
    let bottom = |app: &mut App| {
        let rows = app.prepare_transcript().lines.len();
        rows.saturating_sub(app.transcript_height())
    };
    let before = bottom(&mut app);
    assert_eq!(app.viewport.scroll(), before);
    show_status(&mut app, DockSeverity::Error, &"line\n".repeat(5));
    let grown = bottom(&mut app);
    assert!(grown > before);
    assert_eq!(app.viewport.scroll(), grown, "跟随时最新内容不被 Dock 遮住");
    app.apply_action(KeyAction::Insert(' '));
    let released = bottom(&mut app);
    assert_eq!(app.viewport.scroll(), released);
}

#[test]
fn warn_error_preserve_browse_focus_view_and_space_releases_without_editing() {
    for severity in [DockSeverity::Warn, DockSeverity::Error] {
        for draft in ["draft", "one\ntwo\nthree\nfour\nfive"] {
            let mut app = app();
            app.term_height = 24;
            app.blocks.push(Block::Assistant("body\n".repeat(60)));
            app.input.set_text(draft);
            app.input.cursor = (0, 3);
            app.apply_action(KeyAction::Up);
            let before = app.focus;
            let scroll = app.viewport.scroll();
            show_status(&mut app, severity, "problem\nline\nline");
            app.prepare_transcript();
            assert_eq!(app.focus, before);
            assert_eq!(app.viewport.scroll(), scroll);
            assert!(!app.viewport.following());
            for event in [
                Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)),
                Event::Key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::SHIFT)),
                Event::Paste("blocked".into()),
                Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
                Event::Key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)),
                Event::Key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL)),
            ] {
                handle_input_event(&mut app, event);
                assert_eq!(app.focus, before);
                assert_eq!(app.viewport.scroll(), scroll);
                assert_eq!(app.input.text(), draft);
            }
            handle_input_event(
                &mut app,
                Event::Key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE)),
            );
            app.prepare_transcript();
            assert!(app.dock_status().is_none());
            assert_eq!(app.focus, before);
            assert_eq!(app.viewport.scroll(), scroll);
            assert!(!app.viewport.following());
            assert_eq!(app.input.text(), draft);
        }
    }
}

#[test]
fn occupied_dock_blocks_entry_but_not_browse_navigation_or_exit() {
    for exit in [KeyAction::Down, KeyAction::Clear] {
        let mut app = app();
        app.blocks.push(Block::Assistant("first\nsecond".into()));
        app.input.set_text("draft");
        app.apply_action(KeyAction::Up);
        show_status(&mut app, DockSeverity::Error, "failure");
        app.apply_action(KeyAction::Up);
        app.apply_action(KeyAction::Home);
        app.apply_action(KeyAction::Right);
        assert_eq!(cursor(&app).desired_col, 1);
        app.apply_action(KeyAction::End);
        app.apply_action(KeyAction::Left);
        assert_eq!(cursor(&app).desired_col, 3);
        app.apply_action(KeyAction::Down);
        app.apply_action(exit);
        assert_eq!(app.focus, Focus::Dock);
        assert!(app.viewport.following());
        let before = app.input.cursor;
        for event in [
            Event::Key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)),
            Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)),
            Event::Paste("ignored".into()),
        ] {
            handle_input_event(&mut app, event);
        }
        assert_eq!(app.focus, Focus::Dock);
        assert_eq!(app.input.text(), "draft");
        assert_eq!(app.input.cursor, before);
        app.input.clear();
        handle_input_event(
            &mut app,
            Event::Key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL)),
        );
        assert!(!app.should_quit);
        app.apply_action(KeyAction::ScrollPageUp);
        let scroll = app.viewport.scroll();
        show_status(&mut app, DockSeverity::Warn, "replacement");
        app.apply_action(KeyAction::Insert(' '));
        assert_eq!(app.focus, Focus::Dock);
        assert_eq!(app.viewport.scroll(), scroll);
        assert!(!app.viewport.following());
        assert!(app.input.is_empty());
    }
}

#[test]
fn browse_graphemes_and_resize_use_display_cells_and_block_rows() -> anyhow::Result<()> {
    use ratatui::{Terminal, backend::TestBackend};
    let mut app = app();
    app.blocks.push(Block::Assistant("👨‍👩‍👧‍👦e\u{301}中x".into()));
    app.apply_action(KeyAction::Up);
    app.apply_action(KeyAction::End);
    assert_eq!(cursor(&app).desired_col, 5);
    app.apply_action(KeyAction::Home);
    app.apply_action(KeyAction::Right);
    app.apply_action(KeyAction::Right);
    assert_eq!(cursor(&app).desired_col, 3);
    app.apply_action(KeyAction::Right);
    assert_eq!(cursor(&app).desired_col, 5);
    app.apply_action(KeyAction::Left);
    assert_eq!(cursor(&app).desired_col, 3);
    app.apply_action(KeyAction::Clear);
    app.blocks
        .push(Block::Assistant("long words to wrap ".repeat(20)));
    app.apply_action(KeyAction::Up);
    let before = cursor(&app);
    let mut terminal = Terminal::new(TestBackend::new(22, 10))?;
    terminal.draw(|frame| crate::view::draw(frame, &mut app))?;
    assert_eq!(cursor(&app).block, before.block);
    terminal.backend_mut().resize(200, 50);
    terminal.draw(|frame| crate::view::draw(frame, &mut app))?;
    let projection = app.prepare_transcript();
    let last = projection
        .owners
        .iter()
        .rfind(|p| p.id == before.block.id)
        .unwrap();
    assert_eq!(cursor(&app).block.row, before.block.row.min(last.row));
    assert_eq!(cursor(&app).desired_col, before.desired_col);
    Ok(())
}

#[test]
fn warning_after_control_end_does_not_move_following_cursor_on_repeated_frames() {
    let mut app = app();
    app.term_height = 12;
    app.blocks.push(Block::Assistant("body\n".repeat(60)));
    app.apply_action(KeyAction::Up);
    app.apply_action(KeyAction::ScrollToBottom);
    let before = app.focus;
    let scroll = app.viewport.scroll();
    show_status(&mut app, DockSeverity::Warn, "one\ntwo\nthree\nfour\nfive");
    for _ in 0..2 {
        app.prepare_transcript();
        assert_eq!(app.focus, before);
        assert_eq!(app.viewport.scroll(), scroll);
        assert!(app.viewport.following());
    }
}

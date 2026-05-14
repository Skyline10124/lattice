use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, MouseEvent, MouseEventKind};

use super::app::App;
use super::keymap::KeyBindingsMatch;
use super::pager::PagerOutcome;
use super::state::{ClickAction, MenuKind, TextSelection};

pub(super) async fn handle_key(app: &mut App, key: KeyEvent) -> Result<()> {
    if let Some(pager) = app.pager.as_mut() {
        if pager.handle_key(key, &app.keymap.pager) == PagerOutcome::Closed {
            app.pager = None;
        }
        return Ok(());
    }

    if app.menu.is_some() {
        if app.keymap.list.up.matches_key(key) {
            move_menu_up(app);
            return Ok(());
        }
        if app.keymap.list.down.matches_key(key) {
            move_menu_down(app);
            return Ok(());
        }
        if app.keymap.list.accept.matches_key(key) {
            accept_menu_selection(app);
            return Ok(());
        }
        if app.keymap.list.close.matches_key(key) {
            app.menu = None;
            return Ok(());
        }
        match key.code {
            KeyCode::Up => {
                move_menu_up(app);
                return Ok(());
            }
            KeyCode::Down => {
                move_menu_down(app);
                return Ok(());
            }
            KeyCode::Enter => {
                accept_menu_selection(app);
                return Ok(());
            }
            KeyCode::Esc | KeyCode::Char(' ') => {
                app.menu = None;
                return Ok(());
            }
            _ => {}
        }
    }

    let suggestions = app.slash_suggestions();
    if !suggestions.is_empty() {
        match key.code {
            KeyCode::Tab if app.keymap.composer.accept_suggestion.matches_key(key) => {
                app.accept_selected_suggestion();
                return Ok(());
            }
            KeyCode::Up => {
                app.composer.suggestion_up();
                return Ok(());
            }
            KeyCode::Down => {
                app.composer.suggestion_down();
                return Ok(());
            }
            _ => {}
        }
    }

    match key.code {
        KeyCode::Char('c') if app.keymap.app.quit.matches_key(key) => {
            app.should_quit = true;
        }
        KeyCode::Char('l') if app.keymap.app.clear.matches_key(key) => {
            app.cancel_active_turn();
            app.messages.clear();
            app.turn.queued_inputs.clear();
            app.clear_search();
            app.help_open = false;
        }
        KeyCode::Char('n') if app.keymap.app.new_session.matches_key(key) => {
            app.reset_conversation();
        }
        KeyCode::Char('o') if app.keymap.app.toggle_trace.matches_key(key) => {
            app.toggle_recent_reasoning();
        }
        KeyCode::Char('f') if app.keymap.app.find.matches_key(key) => {
            app.composer.set_input("/find ");
            app.help_open = false;
        }
        KeyCode::Char('g') if app.keymap.app.next_match.matches_key(key) => {
            app.jump_search_next();
        }
        KeyCode::Char('e') if app.keymap.app.toggle_tool.matches_key(key) => {
            app.toggle_tool_expand();
        }
        KeyCode::Char('y') if app.keymap.app.copy.matches_key(key) => {
            app.copy_last_assistant();
        }
        KeyCode::Char('t') if app.keymap.app.open_transcript.matches_key(key) => {
            app.open_transcript_pager();
        }
        KeyCode::Char('d') if app.keymap.app.open_tool_detail.matches_key(key) => {
            app.open_recent_tool_pager();
        }
        KeyCode::Char('?')
            if app.composer.is_empty() && app.keymap.app.toggle_help.matches_key(key) =>
        {
            app.help_open = !app.help_open;
        }
        KeyCode::Char(c) => {
            app.help_open = false;
            app.composer.insert_char(c);
        }
        KeyCode::Backspace => {
            app.composer.backspace();
        }
        KeyCode::Delete => {
            app.composer.delete();
        }
        KeyCode::Left => {
            app.composer.move_left();
        }
        KeyCode::Right => {
            app.composer.move_right();
        }
        KeyCode::Home => app.composer.move_home(),
        KeyCode::End => {
            if app.composer.is_empty() && app.scroll_offset > 0 {
                app.scroll_offset = 0;
            } else {
                app.composer.move_end();
            }
        }
        KeyCode::Enter => {
            if app.keymap.composer.newline.matches_key(key) {
                app.composer.insert_newline();
            } else if app.keymap.composer.submit.matches_key(key) {
                if app.queue_prompt_active() {
                    app.submit_next_queued().await?;
                } else {
                    app.submit().await?;
                }
            }
        }
        KeyCode::Esc => {
            if app.composer.has_history_mode() {
                app.composer.exit_history_mode();
                app.composer.clear();
            } else if app.help_open {
                app.help_open = false;
            } else if app.activity.search.is_active() && app.composer.is_empty() {
                app.clear_search();
            } else if !app.composer.is_empty() {
                app.composer.clear();
            }
        }
        KeyCode::Up => {
            if app.composer.is_empty() && app.composer.has_history() {
                app.composer.navigate_history_prev();
            } else {
                app.scroll_offset = app.scroll_offset.saturating_add(1);
            }
        }
        KeyCode::Down => {
            if app.composer.has_history_mode() {
                app.composer.navigate_history_next();
            } else if app.scroll_offset > 0 {
                app.scroll_offset -= 1;
            }
        }
        KeyCode::PageUp => {
            app.scroll_offset = app.scroll_offset.saturating_add(8);
        }
        KeyCode::PageDown => {
            app.scroll_offset = app.scroll_offset.saturating_sub(8);
        }
        _ => {}
    }
    Ok(())
}

pub(super) async fn handle_mouse(app: &mut App, mouse: MouseEvent) -> Result<()> {
    if app.menu.is_some() {
        return Ok(());
    }

    match mouse.kind {
        MouseEventKind::ScrollUp => {
            app.scroll_offset = app.scroll_offset.saturating_add(3);
        }
        MouseEventKind::ScrollDown if app.scroll_offset > 0 => {
            app.scroll_offset = app.scroll_offset.saturating_sub(3);
        }
        MouseEventKind::Down(_) => {
            let (col, row) = (mouse.column, mouse.row);
            let action = app.activity.click_zones.borrow().iter().find_map(|zone| {
                let (zx, zy, zw, zh) = zone.rect;
                if col >= zx && col < zx + zw && row >= zy && row < zy + zh {
                    Some(zone.action.clone())
                } else {
                    None
                }
            });
            if let Some(action) = action {
                match action {
                    ClickAction::JumpToBottom => app.scroll_offset = 0,
                }
            } else {
                app.activity.selection = Some(TextSelection {
                    start_row: row,
                    start_col: col,
                    end_row: row,
                    end_col: col,
                    active: true,
                });
            }
        }
        MouseEventKind::Drag(_) => {
            if let Some(ref mut sel) = app.activity.selection {
                if sel.active {
                    sel.end_row = mouse.row;
                    sel.end_col = mouse.column;
                }
            }
        }
        MouseEventKind::Up(_) => {
            if let Some(sel) = app.activity.selection.take() {
                if sel.active && (sel.start_row != sel.end_row || sel.start_col != sel.end_col) {
                    copy_selection(app, &sel);
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn move_menu_up(app: &mut App) {
    if let Some(ref mut menu) = app.menu {
        menu.move_up();
    }
}

fn move_menu_down(app: &mut App) {
    if let Some(ref mut menu) = app.menu {
        menu.move_down();
    }
}

fn accept_menu_selection(app: &mut App) {
    let action = app.menu.as_ref().and_then(|menu| {
        menu.selected()
            .map(str::to_string)
            .map(|selected| (menu.kind.clone(), selected))
    });
    if let Some((kind, selected)) = action {
        app.menu = None;
        match kind {
            MenuKind::Model => {
                app.cancel_active_turn();
                app.current_model = selected.clone();
                app.previous_model.clear();
                app.current_provider.clear();
                app.clear_runtime_handles();
                app.push_system_message(&format!("Model: {selected}"));
            }
            MenuKind::Provider => {
                app.cancel_active_turn();
                app.provider_override = Some(selected.clone());
                app.current_provider = selected.clone();
                app.clear_runtime_handles();
                app.push_system_message(&format!("Provider: {selected}"));
            }
            MenuKind::Permissions => {
                app.switch_permissions(&selected);
            }
            MenuKind::Plugin => {
                app.composer.set_input(format!("/plugin {selected} "));
            }
        }
    }
}

pub(super) fn copy_selection(app: &App, sel: &TextSelection) {
    let (top, bot) = if sel.start_row <= sel.end_row {
        (sel.start_row, sel.end_row)
    } else {
        (sel.end_row, sel.start_row)
    };
    let rows = app.activity.visible_rows.borrow();
    let origin = app.activity.visible_rows_origin.get();
    let Some(start_row) = top.checked_sub(origin) else {
        return;
    };
    let Some(end_row) = bot.checked_sub(origin) else {
        return;
    };
    let start = (start_row as usize).min(rows.len());
    let end = (end_row as usize + 1).min(rows.len());
    if start < end {
        let text: String = rows[start..end]
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let trimmed = text.trim().to_string();
        if !trimmed.is_empty() {
            app.copy_to_clipboard(&trimmed);
        }
    }
}

#[cfg(test)]
mod queued_prompt_enter {
    use super::*;
    use crate::frontend::tui::action::TuiAction;
    use crate::frontend::tui::state::AppStatus;
    use crossterm::event::{KeyEvent, KeyModifiers};
    use lattice_core::types::Role;

    #[tokio::test]
    async fn finished_turn_does_not_auto_submit_queued_prompt() {
        let mut app = App::new();
        app.status = AppStatus::Streaming;
        app.turn.active_turn_id = Some(1);
        app.turn.active_assistant_index = Some(0);
        app.turn.pending_user = Some("current prompt".into());
        app.messages
            .push(crate::frontend::tui::state::ChatMessage::new(
                Role::Assistant,
                "done".into(),
            ));
        app.turn
            .queued_inputs
            .push_back("queued prompt should wait".into());

        crate::frontend::tui::update::apply_action(
            &mut app,
            TuiAction::StreamToken {
                turn_id: 1,
                content: String::new(),
                reasoning: None,
                done: true,
                error: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(app.status, AppStatus::Ready);
        assert_eq!(app.turn.queued_inputs.len(), 1);
        assert!(app
            .messages
            .iter()
            .all(|message| message.content != "queued prompt should wait"));
    }

    #[tokio::test]
    async fn enter_on_queued_prompt_submits_one_prompt() {
        let mut app = App::new();
        app.turn
            .queued_inputs
            .push_back("first queued prompt".into());
        app.turn
            .queued_inputs
            .push_back("second queued prompt".into());

        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .unwrap();

        assert_eq!(app.turn.queued_inputs.len(), 1);
        assert_eq!(
            app.turn.queued_inputs.front().map(String::as_str),
            Some("second queued prompt")
        );
        let submitted = app
            .messages
            .iter()
            .filter(|message| {
                message.role == Role::User && message.content == "first queued prompt"
            })
            .count();
        assert_eq!(submitted, 1);
    }

    #[test]
    fn queue_ack_copy_mentions_send_queued_prompt() {
        let mut app = App::new();

        app.queue_input("follow-up".into());

        assert_eq!(
            app.messages.last().unwrap().content,
            "Queued prompt #1. Press Enter on \"Send queued prompt\" when ready."
        );
    }
}

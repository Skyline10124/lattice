use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use lattice_agent::{default_tool_definitions, Agent, DefaultToolExecutor, LoopEvent};
use lattice_core::router::ModelRouter;
use lattice_core::types::Role;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::Mutex;

use super::event::Event;

/// Helper: find byte index of the character just before `cursor`.
fn prev_char_boundary(s: &str, cursor: usize) -> usize {
    s[..cursor]
        .char_indices()
        .last()
        .map(|(i, _)| i)
        .unwrap_or(0)
}

/// Helper: byte length of the character starting at `cursor`.
fn char_byte_len(s: &str, cursor: usize) -> usize {
    s[cursor..]
        .chars()
        .next()
        .map(|c| c.len_utf8())
        .unwrap_or(1)
}

/// Pack an error string into a terminal StreamToken event.
fn pack_error(turn_id: u64, msg: String) -> Event {
    Event::StreamToken {
        turn_id,
        content: msg.clone(),
        reasoning: None,
        done: true,
        error: Some(msg),
    }
}

/// A single message in the chat.
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
    pub reasoning: Option<String>,
    pub collapsed: bool,
    cached_lines:
        std::cell::RefCell<Option<(usize, std::sync::Arc<Vec<ratatui::text::Line<'static>>>)>>,
    cached_width: std::cell::Cell<Option<u16>>,
}

impl Clone for ChatMessage {
    fn clone(&self) -> Self {
        Self {
            role: self.role.clone(),
            content: self.content.clone(),
            reasoning: self.reasoning.clone(),
            collapsed: self.collapsed,
            cached_lines: std::cell::RefCell::new(None),
            cached_width: std::cell::Cell::new(None),
        }
    }
}

impl std::fmt::Debug for ChatMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChatMessage")
            .field("role", &self.role)
            .field("content", &self.content)
            .field("reasoning", &self.reasoning)
            .field("collapsed", &self.collapsed)
            .finish_non_exhaustive()
    }
}

impl ChatMessage {
    pub fn new(role: Role, content: String) -> Self {
        Self {
            role,
            content,
            reasoning: None,
            collapsed: false,
            cached_lines: std::cell::RefCell::new(None),
            cached_width: std::cell::Cell::new(None),
        }
    }

    pub fn set_cache(
        &self,
        content_len: usize,
        lines: Vec<ratatui::text::Line<'static>>,
        width: u16,
    ) {
        self.cached_lines
            .replace(Some((content_len, std::sync::Arc::new(lines))));
        self.cached_width.set(Some(width));
    }

    pub fn get_cache(
        &self,
        content_len: usize,
        width: u16,
    ) -> Option<std::sync::Arc<Vec<ratatui::text::Line<'static>>>> {
        if self.cached_width.get() == Some(width) {
            self.cached_lines
                .borrow()
                .as_ref()
                .filter(|(cl, _)| *cl == content_len)
                .map(|(_, lines)| lines.clone())
        } else {
            None
        }
    }

    #[allow(dead_code)]
    pub fn clear_cache(&self) {
        self.cached_lines.replace(None);
        self.cached_width.set(None);
    }
}

/// Send text to the terminal clipboard via OSC 52.
/// Works in most modern terminals (kitty, wezterm, foot, iterm2, windows terminal).
fn write_osc52_clipboard(text: &str) {
    use std::io::Write;
    let encoded = base64_encode(text);
    let osc = format!("\x1b]52;c;{}\x07", encoded);
    let _ = std::io::stdout().write_all(osc.as_bytes());
    let _ = std::io::stdout().flush();
}

type ClipboardSink = Arc<dyn Fn(&str) + Send + Sync>;

fn base64_encode(input: &str) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = input.as_bytes();
    let mut result = String::new();
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        result.push(CHARS[((triple >> 18) & 0x3f) as usize] as char);
        result.push(CHARS[((triple >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            result.push(CHARS[((triple >> 6) & 0x3f) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(CHARS[(triple & 0x3f) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}

#[derive(Debug, Clone, Copy)]
pub struct SlashSuggestion {
    pub command: &'static str,
    pub description: &'static str,
}

const SLASH_COMMANDS: &[SlashSuggestion] = &[
    SlashSuggestion {
        command: "/help",
        description: "show keyboard shortcuts",
    },
    SlashSuggestion {
        command: "/clear",
        description: "clear visible transcript",
    },
    SlashSuggestion {
        command: "/new",
        description: "start a fresh agent context",
    },
    SlashSuggestion {
        command: "/model <name>",
        description: "switch model or show current",
    },
    SlashSuggestion {
        command: "/provider <name>",
        description: "set provider override or show",
    },
    SlashSuggestion {
        command: "/status",
        description: "show current session state",
    },
    SlashSuggestion {
        command: "/tokens",
        description: "show token breakdown",
    },
    SlashSuggestion {
        command: "/effort <off|low..max>",
        description: "set thinking effort level",
    },
    SlashSuggestion {
        command: "/trace",
        description: "toggle thinking trace on/off",
    },
    SlashSuggestion {
        command: "/expand",
        description: "expand last collapsed tool output",
    },
    SlashSuggestion {
        command: "/copy",
        description: "copy last assistant response",
    },
    SlashSuggestion {
        command: "/save",
        description: "force-save current session",
    },
    SlashSuggestion {
        command: "/sessions restore <id>",
        description: "list or restore a session",
    },
    SlashSuggestion {
        command: "/quit",
        description: "exit the TUI",
    },
];

/// Application state.
pub struct App {
    pub should_quit: bool,
    pub messages: Vec<ChatMessage>,
    pub input: String,
    pub input_cursor: usize,
    pub status: AppStatus,
    pub current_model: String,
    pub previous_model: String,
    pub current_provider: String,
    pub token_count: usize,
    pub scroll_offset: usize,
    pub help_open: bool,
    pub transcript_mode: bool,
    pub spinner_index: usize,
    pub provider_override: Option<String>,
    pub credentials: HashMap<String, String>,
    pub save_sessions: bool,
    pub session: Option<crate::session::Session>,
    pub pending_user: Option<String>,
    pub event_tx: Option<UnboundedSender<Event>>,
    pub agent: Option<Arc<Mutex<Agent>>>,
    pub active_turn_id: Option<u64>,
    pub next_turn_id: u64,
    pub active_assistant_index: Option<usize>,
    input_history: Vec<String>,
    history_index: Option<usize>,
    draft_before_history: String,
    pub click_zones: std::cell::RefCell<Vec<ClickZone>>,
    pub stream_started: Option<std::time::Instant>,
    pub reasoning_started: Option<std::time::Instant>,
    pub reasoning_duration: Option<std::time::Duration>,
    pub thinking_effort: Option<String>,
    pub context_limit: u32,
    pub selection: Option<TextSelection>,
    /// Text of each visible transcript row (populated during render, used for copy-on-select)
    pub visible_rows: std::cell::RefCell<Vec<String>>,
    pub visible_rows_origin: std::cell::Cell<u16>,
    pub menu: Option<MenuState>,
    clipboard: ClipboardSink,
}

#[derive(Debug, Clone)]
pub struct MenuState {
    pub kind: MenuKind,
    pub options: Vec<String>,
    pub index: usize,
}

#[derive(Debug, Clone)]
pub enum MenuKind {
    Model,
    Provider,
    #[allow(dead_code)]
    Session,
}

#[derive(Debug, Clone)]
pub struct TextSelection {
    pub start_row: u16,
    pub start_col: u16,
    pub end_row: u16,
    pub end_col: u16,
    pub active: bool, // true while mouse button held
}

#[derive(Debug, Clone)]
pub struct ClickZone {
    pub rect: (u16, u16, u16, u16), // x, y, w, h
    pub action: ClickAction,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum ClickAction {
    JumpToBottom,
    ToggleTrace,
    ToggleToolExpand,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AppStatus {
    Ready,
    Streaming,
    Error(String),
}

impl App {
    pub fn new() -> Self {
        Self {
            should_quit: false,
            messages: vec![],
            input: String::new(),
            input_cursor: 0,
            status: AppStatus::Ready,
            current_model: "deepseek-v4-flash".into(),
            previous_model: String::new(),
            current_provider: "".into(),
            token_count: 0,
            scroll_offset: 0,
            help_open: false,
            transcript_mode: false,
            spinner_index: 0,
            provider_override: None,
            credentials: HashMap::new(),
            save_sessions: false,
            session: None,
            pending_user: None,
            event_tx: None,
            agent: None,
            active_turn_id: None,
            next_turn_id: 1,
            active_assistant_index: None,
            input_history: Vec::new(),
            history_index: None,
            draft_before_history: String::new(),
            click_zones: std::cell::RefCell::new(Vec::new()),
            menu: None,
            stream_started: None,
            reasoning_started: None,
            reasoning_duration: None,
            thinking_effort: None,
            context_limit: 128000,
            selection: None,
            visible_rows: std::cell::RefCell::new(Vec::new()),
            visible_rows_origin: std::cell::Cell::new(0),
            clipboard: Arc::new(write_osc52_clipboard),
        }
    }

    pub fn load_session(&mut self, session: crate::session::Session) {
        self.cancel_active_turn();
        self.messages = session
            .messages
            .iter()
            .map(|msg| ChatMessage::new(role_from_session(&msg.role), msg.content.clone()))
            .collect();
        self.current_provider = session.provider.clone();
        self.session = Some(session);
        self.scroll_offset = 0;
    }

    fn begin_turn(&mut self, user: String) -> u64 {
        let turn_id = self.next_turn_id;
        self.next_turn_id = self.next_turn_id.wrapping_add(1).max(1);
        self.active_turn_id = Some(turn_id);
        self.active_assistant_index = Some(self.messages.len());
        self.pending_user = Some(user);
        turn_id
    }

    fn cancel_active_turn(&mut self) {
        self.active_turn_id = None;
        self.active_assistant_index = None;
        self.pending_user = None;
        if self.status == AppStatus::Streaming {
            self.status = AppStatus::Ready;
        }
        self.stream_started = None;
        self.reasoning_started = None;
        self.reasoning_duration = None;
    }

    pub fn accepts_turn(&self, turn_id: u64) -> bool {
        self.active_turn_id == Some(turn_id)
    }

    #[cfg(test)]
    fn set_clipboard_sink(&mut self, sink: ClipboardSink) {
        self.clipboard = sink;
    }

    fn copy_to_clipboard(&self, text: &str) {
        (self.clipboard)(text);
    }

    pub fn tick(&mut self) {
        self.spinner_index = self.spinner_index.wrapping_add(1);
    }

    pub async fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        // Menu navigation: intercept when menu is open
        if self.menu.is_some() {
            match key.code {
                KeyCode::Up => {
                    if let Some(ref mut menu) = self.menu {
                        if menu.index > 0 {
                            menu.index -= 1;
                        }
                    }
                    return Ok(());
                }
                KeyCode::Down => {
                    if let Some(ref mut menu) = self.menu {
                        if menu.index + 1 < menu.options.len() {
                            menu.index += 1;
                        }
                    }
                    return Ok(());
                }
                KeyCode::Enter => {
                    let action = if let Some(ref menu) = self.menu {
                        menu.options
                            .get(menu.index)
                            .cloned()
                            .map(|s| (menu.kind.clone(), s))
                    } else {
                        None
                    };
                    if let Some((kind, selected)) = action {
                        self.menu = None;
                        match kind {
                            MenuKind::Model => {
                                self.cancel_active_turn();
                                self.current_model = selected.clone();
                                self.previous_model.clear();
                                self.current_provider.clear();
                                self.agent = None;
                                self.push_system_message(&format!("Model: {selected}"));
                            }
                            MenuKind::Provider => {
                                self.cancel_active_turn();
                                self.provider_override = Some(selected.clone());
                                self.current_provider = selected.clone();
                                self.agent = None;
                                self.push_system_message(&format!("Provider: {selected}"));
                            }
                            MenuKind::Session => {
                                let manager = crate::session::SessionManager::new();
                                match manager.load(&selected) {
                                    Ok(Some(session)) => {
                                        self.load_session(session);
                                        self.push_system_message("Session restored.");
                                    }
                                    _ => self.push_system_message(&format!(
                                        "Failed to load: {selected}"
                                    )),
                                }
                            }
                        }
                    }
                    return Ok(());
                }
                KeyCode::Esc | KeyCode::Char(' ') => {
                    self.menu = None;
                    return Ok(());
                }
                _ => {}
            }
        }

        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true;
            }
            KeyCode::Char('l') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.cancel_active_turn();
                self.messages.clear();
                self.help_open = false;
            }
            KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.reset_conversation();
            }
            KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.transcript_mode = !self.transcript_mode;
            }
            KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.toggle_tool_expand();
            }
            KeyCode::Char('y') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.copy_last_assistant();
            }
            KeyCode::Char('?') if self.input.is_empty() => {
                self.help_open = !self.help_open;
            }
            KeyCode::Char(c) => {
                self.help_open = false;
                self.exit_history_mode();
                self.input.insert(self.input_cursor, c);
                self.input_cursor += c.len_utf8();
            }
            KeyCode::Backspace if self.input_cursor > 0 => {
                let prev = prev_char_boundary(&self.input, self.input_cursor);
                self.input.remove(prev);
                self.input_cursor = prev;
            }
            KeyCode::Delete
                if self.input_cursor < self.input.len()
                    && self.input.is_char_boundary(self.input_cursor) =>
            {
                self.input.remove(self.input_cursor);
            }
            KeyCode::Left if self.input_cursor > 0 => {
                self.input_cursor = prev_char_boundary(&self.input, self.input_cursor);
            }
            KeyCode::Right if self.input_cursor < self.input.len() => {
                let len = char_byte_len(&self.input, self.input_cursor);
                self.input_cursor = (self.input_cursor + len).min(self.input.len());
            }
            KeyCode::Home => self.input_cursor = 0,
            KeyCode::End => {
                if self.input.is_empty() && self.scroll_offset > 0 {
                    self.scroll_offset = 0;
                } else {
                    self.input_cursor = self.input.len();
                }
            }
            KeyCode::Enter => {
                if key.modifiers.contains(KeyModifiers::SHIFT) {
                    self.input.insert(self.input_cursor, '\n');
                    self.input_cursor += '\n'.len_utf8();
                } else {
                    self.submit().await?;
                }
            }
            KeyCode::Esc => {
                if self.history_index.is_some() {
                    self.exit_history_mode();
                    self.input.clear();
                    self.input_cursor = 0;
                } else if self.help_open {
                    self.help_open = false;
                } else if !self.input.is_empty() {
                    self.input.clear();
                    self.input_cursor = 0;
                }
            }
            KeyCode::Up => {
                if self.input.is_empty() && !self.input_history.is_empty() {
                    self.navigate_history_prev();
                } else {
                    self.scroll_offset = self.scroll_offset.saturating_add(1);
                }
            }
            KeyCode::Down => {
                if self.history_index.is_some() {
                    self.navigate_history_next();
                } else if self.scroll_offset > 0 {
                    self.scroll_offset -= 1;
                }
            }
            KeyCode::PageUp => {
                self.scroll_offset = self.scroll_offset.saturating_add(8);
            }
            KeyCode::PageDown => {
                self.scroll_offset = self.scroll_offset.saturating_sub(8);
            }
            _ => {}
        }
        Ok(())
    }

    /// Insert text at the cursor (e.g. from paste or IME commit).
    pub fn insert_text(&mut self, text: &str) {
        self.exit_history_mode();
        for c in text.chars() {
            self.input.insert(self.input_cursor, c);
            self.input_cursor += c.len_utf8();
        }
    }

    fn add_to_history(&mut self, text: String) {
        // Don't duplicate consecutive identical entries
        if self.input_history.last().map(|s| s.as_str()) != Some(text.as_str()) {
            self.input_history.push(text);
        }
        self.history_index = None;
    }

    fn navigate_history_prev(&mut self) {
        if self.input_history.is_empty() {
            return;
        }
        let idx = match self.history_index {
            None => {
                self.draft_before_history = std::mem::take(&mut self.input);
                self.input_history.len().saturating_sub(1)
            }
            Some(i) if i > 0 => i - 1,
            _ => return,
        };
        self.history_index = Some(idx);
        self.input = self.input_history[idx].clone();
        self.input_cursor = self.input.len();
    }

    fn navigate_history_next(&mut self) {
        match self.history_index {
            Some(i) if i + 1 < self.input_history.len() => {
                self.history_index = Some(i + 1);
                self.input = self.input_history[i + 1].clone();
                self.input_cursor = self.input.len();
            }
            Some(_) => {
                self.history_index = None;
                self.input = std::mem::take(&mut self.draft_before_history);
                self.input_cursor = self.input.len();
            }
            None => {}
        }
    }

    fn exit_history_mode(&mut self) {
        if self.history_index.is_some() {
            self.history_index = None;
            self.draft_before_history.clear();
        }
    }

    pub async fn handle_mouse(&mut self, mouse: MouseEvent) -> Result<()> {
        if self.menu.is_some() {
            return Ok(());
        }

        match mouse.kind {
            MouseEventKind::ScrollUp => {
                self.scroll_offset = self.scroll_offset.saturating_add(3);
            }
            MouseEventKind::ScrollDown if self.scroll_offset > 0 => {
                self.scroll_offset = self.scroll_offset.saturating_sub(3);
            }
            MouseEventKind::Down(_) => {
                let (col, row) = (mouse.column, mouse.row);
                // Check click zones first
                let action = self.click_zones.borrow().iter().find_map(|zone| {
                    let (zx, zy, zw, zh) = zone.rect;
                    if col >= zx && col < zx + zw && row >= zy && row < zy + zh {
                        Some(zone.action.clone())
                    } else {
                        None
                    }
                });
                if let Some(action) = action {
                    match action {
                        ClickAction::JumpToBottom => self.scroll_offset = 0,
                        ClickAction::ToggleTrace => self.transcript_mode = !self.transcript_mode,
                        ClickAction::ToggleToolExpand => self.toggle_tool_expand(),
                    }
                } else {
                    // Start text selection
                    self.selection = Some(TextSelection {
                        start_row: row,
                        start_col: col,
                        end_row: row,
                        end_col: col,
                        active: true,
                    });
                }
            }
            MouseEventKind::Drag(_) => {
                if let Some(ref mut sel) = self.selection {
                    if sel.active {
                        sel.end_row = mouse.row;
                        sel.end_col = mouse.column;
                    }
                }
            }
            MouseEventKind::Up(_) => {
                if let Some(sel) = self.selection.take() {
                    if sel.active && (sel.start_row != sel.end_row || sel.start_col != sel.end_col)
                    {
                        self.copy_selection(&sel);
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn copy_selection(&self, sel: &TextSelection) {
        let (top, bot) = if sel.start_row <= sel.end_row {
            (sel.start_row, sel.end_row)
        } else {
            (sel.end_row, sel.start_row)
        };
        let rows = self.visible_rows.borrow();
        let origin = self.visible_rows_origin.get();
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
                self.copy_to_clipboard(&trimmed);
            }
        }
    }

    async fn submit(&mut self) -> Result<()> {
        let text = self.input.trim().to_string();
        if text.is_empty() {
            return Ok(());
        }
        self.add_to_history(text.clone());
        if self.handle_slash_command(&text) {
            self.input.clear();
            self.input_cursor = 0;
            self.scroll_offset = 0;
            return Ok(());
        }
        if self.active_turn_id.is_some() || self.status == AppStatus::Streaming {
            self.push_system_message(
                "A response is still streaming. Wait for it to finish or start a new session.",
            );
            self.input.clear();
            self.input_cursor = 0;
            return Ok(());
        }

        // Bash mode: !<command>
        if let Some(cmd) = text.strip_prefix('!') {
            self.messages
                .push(ChatMessage::new(Role::User, text.clone()));
            self.input.clear();
            self.input_cursor = 0;
            self.scroll_offset = 0;
            self.status = AppStatus::Streaming;
            self.stream_started = Some(std::time::Instant::now());

            let output = match tokio::process::Command::new("bash")
                .arg("-c")
                .arg(cmd)
                .output()
                .await
            {
                Ok(out) => {
                    let stdout = String::from_utf8_lossy(&out.stdout);
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    let mut result = String::new();
                    if !stdout.is_empty() {
                        result.push_str(stdout.trim());
                    }
                    if !stderr.is_empty() {
                        if !result.is_empty() {
                            result.push('\n');
                        }
                        result.push_str(&format!("[stderr]\n{}", stderr.trim()));
                    }
                    if result.is_empty() {
                        result = format!("(exit: {})", out.status.code().unwrap_or(-1));
                    }
                    result
                }
                Err(e) => format!("bash: {e}"),
            };

            let collapsed = output.lines().count() > 6;
            let mut msg = ChatMessage::new(Role::Tool, output);
            msg.collapsed = collapsed;
            self.messages.push(msg);
            self.status = AppStatus::Ready;
            self.stream_started = None;
            return Ok(());
        }

        // Add user message
        self.messages
            .push(ChatMessage::new(Role::User, text.clone()));
        self.token_count += lattice_core::tokens::TokenEstimator::estimate_text(&text) as usize;
        self.input.clear();
        self.input_cursor = 0;
        self.scroll_offset = 0;
        self.status = AppStatus::Streaming;
        self.stream_started = Some(std::time::Instant::now());
        self.reasoning_started = None;
        self.reasoning_duration = None;
        let turn_id = self.begin_turn(text.clone());

        // Thinking indicator — replaced by real content once streaming starts
        self.messages
            .push(ChatMessage::new(Role::Assistant, String::new()));

        let tx = match self.event_tx.clone() {
            Some(tx) => tx,
            None => {
                self.status = AppStatus::Error("Event channel not initialized".into());
                self.cancel_active_turn();
                return Ok(());
            }
        };

        // --- Rebuild agent if none exists or model changed ---
        let needs_rebuild = self.agent.is_none() || self.current_model != self.previous_model;

        if needs_rebuild {
            // Preserve conversation history from the old agent on rebuild
            // (e.g. model switch) so the new model sees prior turns.
            let prior_messages = if let Some(arc) = self.agent.as_ref() {
                let agent = arc.lock().await;
                agent.messages().to_vec()
            } else {
                vec![]
            };

            let model = self.current_model.clone();
            let provider_override = self.provider_override.clone();
            let credentials = self.credentials.clone();
            let resolved = match tokio::task::spawn_blocking(move || {
                let router = ModelRouter::with_credentials(credentials);
                router.resolve(&model, provider_override.as_deref())
            })
            .await
            {
                Ok(Ok(r)) => r,
                Ok(Err(e)) => {
                    let _ = tx.send(pack_error(turn_id, format!("resolve failed: {e}")));
                    self.status = AppStatus::Ready;
                    return Ok(());
                }
                Err(_) => {
                    let _ = tx.send(pack_error(turn_id, "resolve task panicked".into()));
                    self.status = AppStatus::Ready;
                    return Ok(());
                }
            };

            // Report resolved model info
            self.context_limit = if resolved.context_length > 0 {
                resolved.context_length
            } else {
                128000
            };
            let _ = tx.send(Event::ModelInfo {
                turn_id,
                model: resolved.canonical_id.clone(),
                provider: resolved.provider.clone(),
            });

            let executor = match DefaultToolExecutor::new(".").map_err(|e| anyhow!(e)) {
                Ok(executor) => executor,
                Err(e) => {
                    let _ = tx.send(pack_error(turn_id, format!("tool executor failed: {e}")));
                    self.status = AppStatus::Ready;
                    return Ok(());
                }
            };
            let mut agent = Agent::new(resolved)
                .with_tools(default_tool_definitions())
                .with_tool_executor(Box::new(executor))
                .with_thinking_effort(self.thinking_effort.clone());
            agent.set_system_prompt("You are a helpful assistant.");
            // Prefer full agent history over lossy session messages.
            // prior_messages includes tool calls/results that session omits.
            if !prior_messages.is_empty() {
                agent.seed_messages(prior_messages);
            } else if let Some(session) = self.session.as_ref() {
                agent.seed_messages(crate::session::messages_for_agent(session));
            }
            self.agent = Some(Arc::new(Mutex::new(agent)));
            self.previous_model = self.current_model.clone();
        }

        // Clone Arc for spawned task — cheap, just increments ref count
        let agent_arc = self.agent.clone().unwrap();
        let text_for_spawn = text;

        tokio::spawn(async move {
            let mut agent = agent_arc.lock().await;
            // Use run_streaming so each token is emitted in real-time
            // as the LLM produces it, producing a typewriter effect.
            let errored = std::sync::atomic::AtomicBool::new(false);
            let _events = agent
                .run_streaming(&text_for_spawn, 10, |event| match event {
                    LoopEvent::Token { text } => {
                        let _ = tx.send(Event::StreamToken {
                            turn_id,
                            content: text,
                            reasoning: None,
                            done: false,
                            error: None,
                        });
                    }
                    LoopEvent::Reasoning { text } => {
                        let _ = tx.send(Event::StreamToken {
                            turn_id,
                            content: String::new(),
                            reasoning: Some(text),
                            done: false,
                            error: None,
                        });
                    }
                    LoopEvent::ToolCallRequired { calls } => {
                        for call in calls {
                            let _ = tx.send(Event::ToolOutput {
                                turn_id,
                                name: call.function.name,
                                arguments: call.function.arguments,
                                result: None,
                            });
                        }
                    }
                    LoopEvent::ToolResult { call, result } => {
                        let _ = tx.send(Event::ToolOutput {
                            turn_id,
                            name: call.function.name,
                            arguments: call.function.arguments,
                            result: Some(result),
                        });
                    }
                    LoopEvent::Done { .. } => {}
                    LoopEvent::Error { ref message, .. } => {
                        errored.store(true, std::sync::atomic::Ordering::Relaxed);
                        let _ = tx.send(Event::StreamToken {
                            turn_id,
                            content: message.clone(),
                            reasoning: None,
                            done: true,
                            error: Some(message.clone()),
                        });
                    }
                })
                .await;

            // Send final done only if no error already signaled completion
            if !errored.load(std::sync::atomic::Ordering::Relaxed) {
                let _ = tx.send(Event::StreamToken {
                    turn_id,
                    content: String::new(),
                    reasoning: None,
                    done: true,
                    error: None,
                });
            }
        });

        Ok(())
    }

    fn handle_slash_command(&mut self, text: &str) -> bool {
        if !text.starts_with('/') {
            return false;
        }

        let mut parts = text.split_whitespace();
        let command = parts.next().unwrap_or_default();
        match command {
            "/help" => {
                self.help_open = true;
                true
            }
            "/clear" => {
                self.cancel_active_turn();
                self.messages.clear();
                self.help_open = false;
                true
            }
            "/new" => {
                self.reset_conversation();
                self.push_system_message("Started a fresh agent context.");
                true
            }
            "/model" => {
                let model = parts.collect::<Vec<_>>().join(" ");
                if model.is_empty() {
                    let router = lattice_core::router::ModelRouter::with_credentials(
                        self.credentials.clone(),
                    );
                    let models = router.list_authenticated_models();
                    let idx = models
                        .iter()
                        .position(|m| *m == self.current_model)
                        .unwrap_or(0);
                    self.menu = Some(MenuState {
                        kind: MenuKind::Model,
                        options: models,
                        index: idx,
                    });
                } else {
                    self.cancel_active_turn();
                    self.current_model = model.clone();
                    self.previous_model.clear();
                    self.current_provider.clear();
                    self.status = AppStatus::Ready;
                    self.agent = None;
                    self.push_system_message(&format!("Model switched to {model}."));
                }
                true
            }
            "/status" => {
                let provider = if self.current_provider.is_empty() {
                    "unresolved"
                } else {
                    &self.current_provider
                };
                self.push_system_message(&format!(
                    "model={} provider={} messages={} tokens={} transcript={}",
                    self.current_model,
                    provider,
                    self.messages.len(),
                    self.token_count,
                    if self.transcript_mode { "on" } else { "off" }
                ));
                true
            }
            "/provider" => {
                let arg = parts.collect::<Vec<_>>().join(" ");
                if arg.is_empty() {
                    let router = lattice_core::router::ModelRouter::with_credentials(
                        self.credentials.clone(),
                    );
                    // Collect unique providers from authenticated models
                    let mut providers: Vec<String> = Vec::new();
                    let mut seen = std::collections::BTreeSet::new();
                    for model in router.list_authenticated_models() {
                        // Parse model string to extract provider
                        if let Some((provider, _)) = model.split_once('/') {
                            if seen.insert(provider.to_string()) {
                                providers.push(provider.to_string());
                            }
                        }
                    }
                    if providers.is_empty() {
                        let p = if self.current_provider.is_empty() {
                            "auto"
                        } else {
                            &self.current_provider
                        };
                        self.push_system_message(&format!("Current provider: {p}"));
                    } else {
                        let current = if self.current_provider.is_empty() {
                            "auto"
                        } else {
                            &self.current_provider
                        };
                        let idx = providers.iter().position(|p| p == current).unwrap_or(0);
                        self.menu = Some(MenuState {
                            kind: MenuKind::Provider,
                            options: providers,
                            index: idx,
                        });
                    }
                } else {
                    self.cancel_active_turn();
                    self.provider_override = Some(arg.clone());
                    self.current_provider = arg;
                    self.agent = None;
                    self.push_system_message(
                        "Provider override set. Agent will rebuild next turn.",
                    );
                }
                true
            }
            "/tokens" => {
                use lattice_core::tokens::TokenEstimator;
                let content_tokens: usize = self
                    .messages
                    .iter()
                    .map(|m| TokenEstimator::estimate_text(&m.content) as usize)
                    .sum();
                let reasoning_tokens: usize = self
                    .messages
                    .iter()
                    .filter_map(|m| {
                        m.reasoning
                            .as_ref()
                            .map(|r| TokenEstimator::estimate_text(r) as usize)
                    })
                    .sum();
                self.push_system_message(&format!(
                    "tokens: {} content + {} reasoning = {} total ({} messages)",
                    content_tokens,
                    reasoning_tokens,
                    content_tokens + reasoning_tokens,
                    self.messages.len(),
                ));
                true
            }
            "/trace" => {
                self.transcript_mode = !self.transcript_mode;
                self.push_system_message(&format!(
                    "Trace {}. {}",
                    if self.transcript_mode { "on" } else { "off" },
                    if self.transcript_mode {
                        "Reasoning will be shown inline."
                    } else {
                        "Reasoning hidden · Ctrl+O or /trace to toggle."
                    },
                ));
                true
            }
            "/effort" => {
                let level = parts.next().unwrap_or("");
                match level {
                    "off" | "none" => {
                        self.cancel_active_turn();
                        self.thinking_effort = None;
                        self.agent = None;
                        self.push_system_message(
                            "Thinking effort: auto (model default). Agent will rebuild.",
                        );
                    }
                    "low" | "medium" | "high" | "xhigh" | "max" => {
                        self.cancel_active_turn();
                        self.thinking_effort = Some(level.to_string());
                        self.agent = None;
                        self.push_system_message(&format!(
                            "Thinking effort: {level}. Agent will rebuild next turn."
                        ));
                    }
                    "" => {
                        let current = self.thinking_effort.as_deref().unwrap_or("auto");
                        self.push_system_message(&format!(
                            "Thinking effort: {current}. Use /effort <off|low|medium|high|xhigh|max>."
                        ));
                    }
                    _ => {
                        self.push_system_message("Usage: /effort <off|low|medium|high|xhigh|max>");
                    }
                }
                true
            }
            "/expand" => {
                self.toggle_tool_expand();
                true
            }
            "/copy" => {
                self.copy_last_assistant();
                true
            }
            "/save" => {
                if let Some(ref session) = self.session {
                    let manager = crate::session::SessionManager::new();
                    match manager.save(session) {
                        Ok(_) => self.push_system_message(&format!(
                            "Session saved ({} messages, {} tok).",
                            session.messages.len(),
                            self.token_count,
                        )),
                        Err(e) => self.push_system_message(&format!("Save failed: {e}")),
                    }
                } else {
                    self.push_system_message("No session to save yet. Send a message first.");
                }
                true
            }
            "/sessions" => {
                let sub = parts.next().unwrap_or("list");
                let manager = crate::session::SessionManager::new();
                match sub {
                    "restore" | "load" => {
                        let id = parts.next().unwrap_or("");
                        if id.is_empty() {
                            self.push_system_message("Usage: /sessions restore <id>");
                        } else {
                            // Try exact match first, then prefix match
                            let full_id = match manager.load(id) {
                                Ok(Some(_)) => Some(id.to_string()),
                                _ => {
                                    // Prefix search
                                    manager.list().ok().and_then(|sessions| {
                                        sessions
                                            .iter()
                                            .find(|s| s.id.starts_with(id))
                                            .map(|s| s.id.clone())
                                    })
                                }
                            };
                            match full_id {
                                Some(fid) => match manager.load(&fid) {
                                    Ok(Some(session)) => {
                                        let model = session.model.clone();
                                        let msgs = session.messages.len();
                                        self.load_session(session);
                                        self.push_system_message(&format!(
                                            "Restored session {} ({model}, {msgs} messages).",
                                            &fid[..fid.len().min(8)],
                                        ));
                                    }
                                    _ => self.push_system_message(&format!("Failed to load: {id}")),
                                },
                                None => {
                                    self.push_system_message(&format!("Session not found: {id}"));
                                }
                            }
                        }
                    }
                    _ => {
                        match manager.list() {
                            Ok(sessions) => {
                                if sessions.is_empty() {
                                    self.push_system_message("No saved sessions. Use /sessions restore <id> to load one.");
                                } else {
                                    let lines: Vec<String> = sessions
                                        .iter()
                                        .map(|s| {
                                            format!(
                                                "  {}  {} msgs  {}  {}",
                                                &s.id[..s.id.len().min(8)],
                                                s.message_count,
                                                s.model,
                                                &s.created_at[..s.created_at.len().min(16)],
                                            )
                                        })
                                        .collect();
                                    self.push_system_message(&format!(
                                        "{} session(s) · /sessions restore <id> to load:\n{}",
                                        sessions.len(),
                                        lines.join("\n"),
                                    ));
                                }
                            }
                            Err(e) => self.push_system_message(&format!("Failed: {e}")),
                        }
                    }
                }
                true
            }
            "/quit" | "/exit" => {
                self.should_quit = true;
                true
            }
            _ => {
                self.push_system_message(&format!(
                    "Unknown command: {command}. Type /help for shortcuts."
                ));
                true
            }
        }
    }

    fn reset_conversation(&mut self) {
        self.messages.clear();
        self.input.clear();
        self.input_cursor = 0;
        self.scroll_offset = 0;
        self.help_open = false;
        self.token_count = 0;
        self.agent = None;
        self.previous_model.clear();
        self.session = None;
        self.pending_user = None;
        self.status = AppStatus::Ready;
    }

    fn push_system_message(&mut self, content: &str) {
        self.messages
            .push(ChatMessage::new(Role::System, content.to_string()));
    }

    pub fn slash_suggestions(&self) -> Vec<SlashSuggestion> {
        let Some(query) = self.input.strip_prefix('/') else {
            return Vec::new();
        };
        if self.input.chars().any(char::is_whitespace) {
            return Vec::new();
        }

        let query = query.to_ascii_lowercase();
        SLASH_COMMANDS
            .iter()
            .copied()
            .filter(|s| s.command.trim_start_matches('/').starts_with(&query))
            .collect()
    }

    pub fn mode_label(&self) -> &'static str {
        if self.input.starts_with('!') {
            "bash"
        } else {
            "prompt"
        }
    }

    pub fn spinner(&self) -> &'static str {
        const FRAMES: &[&str] = &["-", "\\", "|", "/"];
        FRAMES[self.spinner_index % FRAMES.len()]
    }

    /// Apply a streaming token to the last assistant message.
    pub fn apply_stream_token(
        &mut self,
        turn_id: u64,
        content: String,
        reasoning: Option<String>,
        done: bool,
        error: Option<String>,
    ) {
        if !self.accepts_turn(turn_id) {
            return;
        }

        let has_payload = !content.is_empty() || reasoning.is_some() || error.is_some();
        let assistant_index = match self.active_assistant_index {
            Some(idx) if idx < self.messages.len() => idx,
            _ => {
                if has_payload {
                    self.messages
                        .push(ChatMessage::new(Role::Assistant, String::new()));
                    let idx = self.messages.len().saturating_sub(1);
                    self.active_assistant_index = Some(idx);
                    idx
                } else {
                    return;
                }
            }
        };

        if self
            .messages
            .get(assistant_index)
            .is_none_or(|msg| msg.role != Role::Assistant)
        {
            return;
        }

        if !content.is_empty() {
            if let Some(last) = self.messages.get_mut(assistant_index) {
                last.content.push_str(&content);
            }
            self.token_count +=
                lattice_core::tokens::TokenEstimator::estimate_text(&content) as usize;
        }
        if let Some(r) = reasoning {
            self.token_count += lattice_core::tokens::TokenEstimator::estimate_text(&r) as usize;
            if self.reasoning_started.is_none() {
                self.reasoning_started = Some(std::time::Instant::now());
            }
            if let Some(last) = self.messages.get_mut(assistant_index) {
                match last.reasoning {
                    Some(ref mut existing) => existing.push_str(&r),
                    None => last.reasoning = Some(r),
                }
            }
        } else if self.reasoning_started.is_some() && self.reasoning_duration.is_none() {
            // Reasoning phase ended — record duration
            if let Some(start) = self.reasoning_started {
                self.reasoning_duration = Some(start.elapsed());
            }
        }
        if let Some(ref msg) = error {
            let error_text = format!("Error: {}", msg);
            if let Some(last) = self.messages.get_mut(assistant_index) {
                if last.content.is_empty() {
                    last.content = error_text.clone();
                } else if !last.content.contains(&error_text) {
                    if !last.content.ends_with('\n') && !last.content.is_empty() {
                        last.content.push('\n');
                    }
                    last.content.push_str(&error_text);
                }
            }
        }

        if let Some(msg) = error {
            self.status = AppStatus::Error(msg);
            self.active_turn_id = None;
            self.active_assistant_index = None;
            self.pending_user = None;
            self.stream_started = None;
            self.reasoning_started = None;
            self.reasoning_duration = None;
        } else if done {
            self.status = AppStatus::Ready;
            // Recalculate total token count from all messages for accuracy
            self.recount_tokens();
            // Cache is populated lazily on next render at message_lines()
            self.save_pending_turn();
            self.active_assistant_index = None;
            self.active_turn_id = None;
            self.pending_user = None;
            self.stream_started = None;
            self.reasoning_started = None;
            self.reasoning_duration = None;
        }
    }

    pub fn apply_tool_output(
        &mut self,
        turn_id: u64,
        name: String,
        arguments: String,
        result: Option<String>,
    ) {
        if !self.accepts_turn(turn_id) {
            return;
        }

        if let Some(idx) = self.active_assistant_index {
            if matches!(
                self.messages.get(idx),
                Some(last)
                    if last.role == Role::Assistant
                        && last.content.is_empty()
                        && last.reasoning.as_deref().unwrap_or("").is_empty()
            ) {
                self.messages.remove(idx);
                self.active_assistant_index = None;
            }
        }

        let content = match result {
            Some(result) => format!(
                "{} {}\n{}",
                name,
                compact_json(&arguments),
                trim_tool_result(&result)
            ),
            None => format!("{} {}...", name, compact_json(&arguments)),
        };

        let mut msg = ChatMessage::new(Role::Tool, content);
        msg.collapsed = true;
        self.messages.push(msg);
        self.scroll_offset = 0;
    }

    pub fn toggle_tool_expand(&mut self) {
        if let Some(last) = self.messages.last_mut() {
            if last.role == Role::Tool {
                last.collapsed = !last.collapsed;
                last.clear_cache();
            }
        }
    }

    fn recount_tokens(&mut self) {
        use lattice_core::tokens::TokenEstimator;
        let mut total = 0usize;
        for msg in &self.messages {
            total += TokenEstimator::estimate_text(&msg.content) as usize;
            if let Some(ref r) = msg.reasoning {
                total += TokenEstimator::estimate_text(r) as usize;
            }
        }
        self.token_count = total;
    }

    pub fn copy_last_assistant(&mut self) {
        let text = self
            .messages
            .iter()
            .rev()
            .find(|m| m.role == Role::Assistant && !m.content.is_empty())
            .map(|m| m.content.clone())
            .unwrap_or_default();
        if !text.is_empty() {
            self.copy_to_clipboard(&text);
            self.push_system_message("Copied to clipboard");
        }
    }

    fn save_pending_turn(&mut self) {
        if !self.save_sessions {
            self.pending_user = None;
            return;
        }

        let Some(user) = self.pending_user.take() else {
            return;
        };
        let assistant = self
            .messages
            .iter()
            .rev()
            .find(|msg| msg.role == Role::Assistant && !msg.content.trim().is_empty())
            .map(|msg| msg.content.clone())
            .unwrap_or_default();
        if assistant.is_empty() {
            return;
        }

        let session = crate::session::finalize_session_turn(
            self.session.take(),
            self.current_model.clone(),
            self.current_provider.clone(),
            user,
            assistant,
        );
        let manager = crate::session::SessionManager::new();
        if let Err(err) = manager.save(&session) {
            self.messages.push(ChatMessage::new(
                Role::System,
                format!("Failed to save session: {err}"),
            ));
        }
        self.session = Some(session);
    }
}

fn role_from_session(role: &str) -> Role {
    match role.to_ascii_lowercase().as_str() {
        "assistant" => Role::Assistant,
        "system" => Role::System,
        "tool" => Role::Tool,
        _ => Role::User,
    }
}

fn compact_json(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(value) => value.to_string(),
        Err(_) => trimmed.to_string(),
    }
}

fn trim_tool_result(result: &str) -> String {
    const LIMIT: usize = 4000;
    if result.len() <= LIMIT {
        return result.to_string();
    }

    let mut end = LIMIT;
    while !result.is_char_boundary(end) {
        end -= 1;
    }
    let mut trimmed = result[..end].to_string();
    trimmed.push_str("\n...[tool output truncated]");
    trimmed
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_session() -> crate::session::Session {
        crate::session::Session {
            id: "session-1".into(),
            model: "old-model".into(),
            provider: "old-provider".into(),
            title: None,
            messages: vec![
                crate::session::SessionMessage {
                    role: "user".into(),
                    content: "old question".into(),
                },
                crate::session::SessionMessage {
                    role: "assistant".into(),
                    content: "old answer".into(),
                },
            ],
            created_at: "2026-05-05T00:00:00+08:00".into(),
            updated_at: "2026-05-05T00:00:00+08:00".into(),
        }
    }

    #[test]
    fn stream_error_sets_status_and_appends_visible_error() {
        let mut app = App::new();
        app.status = AppStatus::Streaming;
        app.active_turn_id = Some(7);
        app.active_assistant_index = Some(0);
        app.messages
            .push(ChatMessage::new(Role::Assistant, "partial response".into()));

        app.apply_stream_token(
            7,
            String::new(),
            None,
            false,
            Some("provider failed".into()),
        );

        assert_eq!(app.status, AppStatus::Error("provider failed".into()));
        assert_eq!(
            app.messages.last().unwrap().content,
            "partial response\nError: provider failed"
        );
    }

    #[test]
    fn stale_turn_tokens_are_ignored() {
        let mut app = App::new();
        app.active_turn_id = Some(9);
        app.active_assistant_index = Some(0);
        app.messages
            .push(ChatMessage::new(Role::Assistant, String::new()));

        app.apply_stream_token(8, "ignored".into(), None, false, None);

        assert!(app.messages[0].content.is_empty());
    }

    #[test]
    fn slash_model_switch_updates_state() {
        let mut app = App::new();
        app.current_provider = "mock".into();
        app.previous_model = "old".into();

        let handled = app.handle_slash_command("/model llama-3.1").then_some(());

        assert!(handled.is_some());
        assert_eq!(app.current_model, "llama-3.1");
        assert!(app.current_provider.is_empty());
        assert_eq!(app.status, AppStatus::Ready);
        assert!(app.agent.is_none());
        assert_eq!(
            app.messages.last().unwrap().content,
            "Model switched to llama-3.1."
        );
    }

    #[test]
    fn slash_suggestions_filter_prefixes() {
        let mut app = App::new();
        app.input = "/st".into();

        let suggestions = app.slash_suggestions();

        assert!(suggestions.iter().any(|s| s.command == "/status"));
        assert!(!suggestions.iter().any(|s| s.command == "/clear"));
    }

    #[test]
    fn tool_output_adds_visible_tool_message() {
        let mut app = App::new();
        app.active_turn_id = Some(1);

        app.apply_tool_output(
            1,
            "list_directory".into(),
            r#"{"path":"."}"#.into(),
            Some("FILE Cargo.toml".into()),
        );

        let msg = app.messages.last().unwrap();
        assert_eq!(msg.role, Role::Tool);
        assert!(msg.content.contains("list_directory"));
        assert!(msg.content.contains("FILE Cargo.toml"));
    }

    #[test]
    fn stream_after_tool_output_creates_visible_assistant_message() {
        let mut app = App::new();
        app.messages
            .push(ChatMessage::new(Role::Assistant, String::new()));
        app.active_turn_id = Some(1);
        app.active_assistant_index = Some(0);
        app.apply_tool_output(
            1,
            "read_file".into(),
            r#"{"path":"Cargo.toml"}"#.into(),
            None,
        );

        app.apply_stream_token(1, "final answer".into(), None, false, None);

        let msg = app.messages.last().unwrap();
        assert_eq!(msg.role, Role::Assistant);
        assert_eq!(msg.content, "final answer");
    }

    #[test]
    fn selection_copy_uses_viewport_origin() {
        let mut app = App::new();
        app.set_clipboard_sink(Arc::new(|_| {}));
        app.visible_rows
            .replace(vec!["line two".into(), "line three".into()]);
        app.visible_rows_origin.set(10);
        let sel = TextSelection {
            start_row: 10,
            start_col: 0,
            end_row: 11,
            end_col: 0,
            active: false,
        };

        app.copy_selection(&sel);
    }

    #[test]
    fn loading_session_does_not_override_explicit_model() {
        let mut app = App::new();
        app.current_model = "explicit-model".into();

        app.load_session(test_session());

        assert_eq!(app.current_model, "explicit-model");
        assert_eq!(app.current_provider, "old-provider");
        assert_eq!(app.messages.len(), 2);
    }
}

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
fn pack_error(msg: String) -> Event {
    Event::StreamToken {
        content: msg.clone(),
        reasoning: None,
        done: true,
        error: Some(msg),
    }
}

/// A single message in the chat.
#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
    pub reasoning: Option<String>,
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
        description: "switch model for the next turn",
    },
    SlashSuggestion {
        command: "/status",
        description: "show current session state",
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
        }
    }

    pub fn load_session(&mut self, session: crate::session::Session) {
        self.messages = session
            .messages
            .iter()
            .map(|msg| ChatMessage {
                role: role_from_session(&msg.role),
                content: msg.content.clone(),
                reasoning: None,
            })
            .collect();
        self.current_provider = session.provider.clone();
        self.session = Some(session);
        self.scroll_offset = 0;
    }

    pub fn tick(&mut self) {
        self.spinner_index = self.spinner_index.wrapping_add(1);
    }

    pub async fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true;
            }
            KeyCode::Char('l') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.messages.clear();
                self.help_open = false;
            }
            KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.reset_conversation();
            }
            KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.transcript_mode = !self.transcript_mode;
            }
            KeyCode::Char('?') if self.input.is_empty() => {
                self.help_open = !self.help_open;
            }
            KeyCode::Char(c) => {
                self.help_open = false;
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
            KeyCode::End => self.input_cursor = self.input.len(),
            KeyCode::Enter => {
                if key.modifiers.contains(KeyModifiers::SHIFT) {
                    self.input.insert(self.input_cursor, '\n');
                    self.input_cursor += '\n'.len_utf8();
                } else {
                    self.submit().await?;
                }
            }
            KeyCode::Esc => {
                if self.help_open {
                    self.help_open = false;
                } else if !self.input.is_empty() {
                    self.input.clear();
                    self.input_cursor = 0;
                }
            }
            KeyCode::Up => {
                self.scroll_offset = self.scroll_offset.saturating_add(1);
            }
            KeyCode::Down if self.scroll_offset > 0 => {
                self.scroll_offset -= 1;
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
        for c in text.chars() {
            self.input.insert(self.input_cursor, c);
            self.input_cursor += c.len_utf8();
        }
    }

    pub async fn handle_mouse(&mut self, mouse: MouseEvent) -> Result<()> {
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                self.scroll_offset = self.scroll_offset.saturating_add(3);
            }
            MouseEventKind::ScrollDown if self.scroll_offset > 0 => {
                self.scroll_offset = self.scroll_offset.saturating_sub(3);
            }
            _ => {}
        }
        Ok(())
    }

    async fn submit(&mut self) -> Result<()> {
        let text = self.input.trim().to_string();
        if text.is_empty() {
            return Ok(());
        }
        if self.handle_slash_command(&text) {
            self.input.clear();
            self.input_cursor = 0;
            self.scroll_offset = 0;
            return Ok(());
        }

        // Add user message
        self.messages.push(ChatMessage {
            role: Role::User,
            content: text.clone(),
            reasoning: None,
        });
        self.input.clear();
        self.input_cursor = 0;
        self.scroll_offset = 0;
        self.status = AppStatus::Streaming;
        self.pending_user = Some(text.clone());

        // Thinking indicator — replaced by real content once streaming starts
        self.messages.push(ChatMessage {
            role: Role::Assistant,
            content: String::new(),
            reasoning: None,
        });

        let tx = match self.event_tx.clone() {
            Some(tx) => tx,
            None => {
                self.status = AppStatus::Error("Event channel not initialized".into());
                return Ok(());
            }
        };

        // --- Rebuild agent if none exists or model changed ---
        let needs_rebuild = self.agent.is_none() || self.current_model != self.previous_model;

        if needs_rebuild {
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
                    let _ = tx.send(pack_error(format!("resolve failed: {e}")));
                    self.status = AppStatus::Ready;
                    return Ok(());
                }
                Err(_) => {
                    let _ = tx.send(pack_error("resolve task panicked".into()));
                    self.status = AppStatus::Ready;
                    return Ok(());
                }
            };

            // Report resolved model info
            let _ = tx.send(Event::ModelInfo {
                model: resolved.canonical_id.clone(),
                provider: resolved.provider.clone(),
            });

            let executor = match DefaultToolExecutor::new(".").map_err(|e| anyhow!(e)) {
                Ok(executor) => executor,
                Err(e) => {
                    let _ = tx.send(pack_error(format!("tool executor failed: {e}")));
                    self.status = AppStatus::Ready;
                    return Ok(());
                }
            };
            let mut agent = Agent::new(resolved)
                .with_tools(default_tool_definitions())
                .with_tool_executor(Box::new(executor));
            agent.set_system_prompt("You are a helpful assistant.");
            if let Some(session) = self.session.as_ref() {
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
            let events = agent.run(&text_for_spawn, 10).await;

            for event in events {
                match event {
                    LoopEvent::Token { text } => {
                        let _ = tx.send(Event::StreamToken {
                            content: text,
                            reasoning: None,
                            done: false,
                            error: None,
                        });
                    }
                    LoopEvent::Reasoning { text } => {
                        let _ = tx.send(Event::StreamToken {
                            content: String::new(),
                            reasoning: Some(text),
                            done: false,
                            error: None,
                        });
                    }
                    LoopEvent::ToolCallRequired { calls } => {
                        for call in calls {
                            let _ = tx.send(Event::ToolOutput {
                                name: call.function.name,
                                arguments: call.function.arguments,
                                result: None,
                            });
                        }
                    }
                    LoopEvent::ToolResult { call, result } => {
                        let _ = tx.send(Event::ToolOutput {
                            name: call.function.name,
                            arguments: call.function.arguments,
                            result: Some(result),
                        });
                    }
                    LoopEvent::Done { .. } => {}
                    LoopEvent::Error { message, .. } => {
                        let _ = tx.send(Event::StreamToken {
                            content: message.clone(),
                            reasoning: None,
                            done: true,
                            error: Some(message),
                        });
                        return;
                    }
                }
            }

            let _ = tx.send(Event::StreamToken {
                content: String::new(),
                reasoning: None,
                done: true,
                error: None,
            });
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
                    self.push_system_message(&format!("Current model: {}", self.current_model));
                } else {
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
        self.messages.push(ChatMessage {
            role: Role::System,
            content: content.to_string(),
            reasoning: None,
        });
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
        content: String,
        reasoning: Option<String>,
        done: bool,
        error: Option<String>,
    ) {
        let has_payload = !content.is_empty() || reasoning.is_some() || error.is_some();
        if has_payload
            && !matches!(self.messages.last(), Some(last) if last.role == Role::Assistant)
        {
            self.messages.push(ChatMessage {
                role: Role::Assistant,
                content: String::new(),
                reasoning: None,
            });
        }

        if let Some(last) = self
            .messages
            .last_mut()
            .filter(|msg| msg.role == Role::Assistant)
        {
            if !content.is_empty() {
                last.content.push_str(&content);
                self.token_count += content.len() / 4;
            }
            if let Some(r) = reasoning {
                match last.reasoning {
                    Some(ref mut existing) => existing.push_str(&r),
                    None => last.reasoning = Some(r),
                }
            }
            if let Some(ref msg) = error {
                let error_text = format!("Error: {}", msg);
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
            self.pending_user = None;
        } else if done {
            self.status = AppStatus::Ready;
            self.save_pending_turn();
        }
    }

    pub fn apply_tool_output(&mut self, name: String, arguments: String, result: Option<String>) {
        if matches!(
            self.messages.last(),
            Some(last)
                if last.role == Role::Assistant
                    && last.content.is_empty()
                    && last.reasoning.as_deref().unwrap_or("").is_empty()
        ) {
            self.messages.pop();
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

        self.messages.push(ChatMessage {
            role: Role::Tool,
            content,
            reasoning: None,
        });
        self.scroll_offset = 0;
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
            self.messages.push(ChatMessage {
                role: Role::System,
                content: format!("Failed to save session: {err}"),
                reasoning: None,
            });
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
        app.messages.push(ChatMessage {
            role: Role::Assistant,
            content: "partial response".into(),
            reasoning: None,
        });

        app.apply_stream_token(String::new(), None, false, Some("provider failed".into()));

        assert_eq!(app.status, AppStatus::Error("provider failed".into()));
        assert_eq!(
            app.messages.last().unwrap().content,
            "partial response\nError: provider failed"
        );
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

        app.apply_tool_output(
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
        app.messages.push(ChatMessage {
            role: Role::Assistant,
            content: String::new(),
            reasoning: None,
        });
        app.apply_tool_output("read_file".into(), r#"{"path":"Cargo.toml"}"#.into(), None);

        app.apply_stream_token("final answer".into(), None, false, None);

        let msg = app.messages.last().unwrap();
        assert_eq!(msg.role, Role::Assistant);
        assert_eq!(msg.content, "final answer");
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

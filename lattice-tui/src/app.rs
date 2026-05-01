use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use lattice_agent::{
    default_tool_definitions, Agent, DefaultToolExecutor, LoopEvent, ToolExecutor,
};
use lattice_core::types::Role;
use tokio::sync::mpsc::UnboundedSender;

use crate::event::Event;

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

/// Application state.
pub struct App {
    pub should_quit: bool,
    pub messages: Vec<ChatMessage>,
    pub input: String,
    pub input_cursor: usize,
    pub status: AppStatus,
    pub current_model: String,
    pub current_provider: String,
    pub token_count: usize,
    pub scroll_offset: usize,
    pub event_tx: Option<UnboundedSender<Event>>,
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
            current_provider: "".into(),
            token_count: 0,
            scroll_offset: 0,
            event_tx: None,
        }
    }

    pub fn tick(&mut self) {
        // Animation tick (e.g. spinner rotation) can go here
    }

    pub async fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true;
            }
            KeyCode::Char('l') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.messages.clear();
            }
            KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.messages.clear();
                self.input.clear();
                self.input_cursor = 0;
            }
            KeyCode::Char(c) => {
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
            KeyCode::Esc if !self.input.is_empty() => {
                self.input.clear();
                self.input_cursor = 0;
            }
            KeyCode::Up if self.scroll_offset > 0 => {
                self.scroll_offset -= 1;
            }
            KeyCode::Down => {
                self.scroll_offset += 1;
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
            MouseEventKind::ScrollUp if self.scroll_offset > 0 => {
                self.scroll_offset -= 1;
            }
            MouseEventKind::ScrollDown => {
                self.scroll_offset += 1;
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
        let model = self.current_model.clone();

        tokio::spawn(async move {
            // --- Resolve model (in-place to avoid blocking) ---
            let resolved =
                match tokio::task::spawn_blocking(move || lattice_core::resolve(&model)).await {
                    Ok(Ok(r)) => r,
                    Ok(Err(e)) => {
                        let _ = tx.send(pack_error(format!("resolve failed: {e}")));
                        return;
                    }
                    Err(_) => {
                        let _ = tx.send(pack_error("resolve task panicked".into()));
                        return;
                    }
                };

            // Report resolved model info
            let _ = tx.send(Event::ModelInfo {
                model: resolved.canonical_id.clone(),
                provider: resolved.provider.clone(),
            });

            // --- Create Agent ---
            let mut agent = Agent::new(resolved).with_tools(default_tool_definitions());
            let executor = DefaultToolExecutor::new(".");

            // --- Send message (async) ---
            let mut events = agent.send_message_async(&text).await;
            let mut cumulative_tokens = 0u64;

            // --- Conversation loop (handles tool call rounds) ---
            loop {
                let mut tool_calls = Vec::new();

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
                            tool_calls = calls;
                        }
                        LoopEvent::Done { usage } => {
                            if let Some(ref u) = usage {
                                cumulative_tokens += u.total_tokens as u64;
                                let _ = tx.send(Event::StreamToken {
                                    content: format!("\n\n[{} tok]", cumulative_tokens),
                                    reasoning: None,
                                    done: true,
                                    error: None,
                                });
                            } else {
                                let _ = tx.send(Event::StreamToken {
                                    content: String::new(),
                                    reasoning: None,
                                    done: true,
                                    error: None,
                                });
                            }
                        }
                        LoopEvent::Error { message } => {
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

                // No tool calls — conversation round is complete
                if tool_calls.is_empty() {
                    return;
                }

                // Notify UI that tools are being executed
                let _ = tx.send(Event::StreamToken {
                    content: format!("\n[{} tool call(s)]", tool_calls.len()),
                    reasoning: None,
                    done: false,
                    error: None,
                });

                // Execute tools
                let results: Vec<(String, String)> = tool_calls
                    .iter()
                    .map(|call| {
                        let result = executor.execute(call);
                        (call.id.clone(), result)
                    })
                    .collect();

                // Submit results and get next round of events
                events = agent.submit_tools(results, None);
            }
        });

        Ok(())
    }

    /// Apply a streaming token to the last assistant message.
    pub fn apply_stream_token(
        &mut self,
        content: String,
        reasoning: Option<String>,
        done: bool,
        error: Option<String>,
    ) {
        if let Some(last) = self.messages.last_mut() {
            if last.role == Role::Assistant {
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
                // If there's an error but no content accumulated, show the error as visible text
                if let Some(ref msg) = error {
                    if last.content.is_empty() {
                        last.content = format!("Error: {}", msg);
                    }
                }
            }
        }

        if done {
            self.status = match error {
                Some(msg) => AppStatus::Error(msg),
                None => AppStatus::Ready,
            };
        }
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

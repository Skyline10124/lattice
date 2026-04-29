use anyhow::Result;
use artemis_core::types::{Message as CoreMessage, Role};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use futures::StreamExt;
use tokio::sync::mpsc::UnboundedSender;

use crate::event::Event;

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
    pub show_reasoning: bool,
    pub scroll_offset: usize,
    pub event_tx: Option<UnboundedSender<Event>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AppStatus {
    Ready,
    Streaming,
    Waiting,
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
            current_model: "sonnet".into(),
            current_provider: "nous".into(),
            token_count: 0,
            show_reasoning: true,
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
                self.input_cursor += 1;
            }
            KeyCode::Backspace => {
                if self.input_cursor > 0 {
                    self.input_cursor -= 1;
                    self.input.remove(self.input_cursor);
                }
            }
            KeyCode::Delete => {
                if self.input_cursor < self.input.len() {
                    self.input.remove(self.input_cursor);
                }
            }
            KeyCode::Left => {
                if self.input_cursor > 0 {
                    self.input_cursor -= 1;
                }
            }
            KeyCode::Right => {
                if self.input_cursor < self.input.len() {
                    self.input_cursor += 1;
                }
            }
            KeyCode::Home => self.input_cursor = 0,
            KeyCode::End => self.input_cursor = self.input.len(),
            KeyCode::Enter => {
                if key.modifiers.contains(KeyModifiers::SHIFT) {
                    self.input.insert(self.input_cursor, '\n');
                    self.input_cursor += 1;
                } else {
                    self.submit().await?;
                }
            }
            KeyCode::Esc => {
                if !self.input.is_empty() {
                    self.input.clear();
                    self.input_cursor = 0;
                }
            }
            KeyCode::Up => {
                if self.scroll_offset > 0 {
                    self.scroll_offset -= 1;
                }
            }
            KeyCode::Down => {
                self.scroll_offset += 1;
            }
            _ => {}
        }
        Ok(())
    }

    pub async fn handle_mouse(&mut self, mouse: MouseEvent) -> Result<()> {
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                if self.scroll_offset > 0 {
                    self.scroll_offset -= 1;
                }
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

        // Placeholder assistant message that will be filled by the stream
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
            let resolved = match artemis_core::resolve(&model) {
                Ok(r) => r,
                Err(e) => {
                    let _ = tx.send(Event::StreamToken {
                        content: String::new(),
                        reasoning: None,
                        done: true,
                        error: Some(e.to_string()),
                    });
                    return;
                }
            };

            let messages = vec![CoreMessage::new(Role::User, text.into(), None, None, None)];

            let mut stream = match artemis_core::chat(&resolved, &messages, &[]).await {
                Ok(s) => s,
                Err(e) => {
                    let _ = tx.send(Event::StreamToken {
                        content: String::new(),
                        reasoning: None,
                        done: true,
                        error: Some(e.to_string()),
                    });
                    return;
                }
            };

            while let Some(event) = stream.next().await {
                match event {
                    artemis_core::StreamEvent::Token { content } => {
                        let _ = tx.send(Event::StreamToken {
                            content,
                            reasoning: None,
                            done: false,
                            error: None,
                        });
                    }
                    artemis_core::StreamEvent::Reasoning { content } => {
                        let _ = tx.send(Event::StreamToken {
                            content: String::new(),
                            reasoning: Some(content),
                            done: false,
                            error: None,
                        });
                    }
                    artemis_core::StreamEvent::Done { usage, .. } => {
                        if let Some(u) = usage {
                            let _ = tx.send(Event::StreamToken {
                                content: format!("\n\n[{} tok]", u.total_tokens),
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
                        break;
                    }
                    artemis_core::StreamEvent::Error { message } => {
                        let _ = tx.send(Event::StreamToken {
                            content: String::new(),
                            reasoning: None,
                            done: true,
                            error: Some(message),
                        });
                        break;
                    }
                    _ => {}
                }
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

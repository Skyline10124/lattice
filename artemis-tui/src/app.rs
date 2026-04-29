use anyhow::Result;
use artemis_core::types::{Message, Role};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};

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
                // Cancel streaming or clear input
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

        // In MVP, we do a placeholder assistant response.
        // Full streaming integration will call artemis_core::chat() here.
        self.status = AppStatus::Streaming;
        
        // Placeholder: add a fake assistant response for demo
        // In real implementation, this would spawn an async task
        // that consumes the SSE stream and pushes tokens into the message.
        tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;
        
        self.messages.push(ChatMessage {
            role: Role::Assistant,
            content: format!("(MVP placeholder response to: {})", text),
            reasoning: Some("Thinking process placeholder".into()),
        });
        
        self.token_count += text.len() / 4; // rough estimate
        self.status = AppStatus::Ready;

        Ok(())
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

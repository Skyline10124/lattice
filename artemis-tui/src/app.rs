use anyhow::{anyhow, Result};
use artemis_agent::{Agent, LoopEvent};
use artemis_core::types::{Role, ToolCall, ToolDefinition};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use serde::Deserialize;
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
            current_model: "deepseek-v4-flash".into(),
            current_provider: "".into(),
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
            // --- Resolve model ---
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

            // Report resolved model info to the statusline
            let _ = tx.send(Event::ModelInfo {
                model: resolved.canonical_id.clone(),
                provider: resolved.provider.clone(),
            });

            // --- Create Agent with tools ---
            let mut agent = Agent::new(resolved).with_tools(tool_definitions());
            let mut events = agent.send_message(&text);
            let mut cumulative_tokens = 0u64;

            // --- Conversation loop (handles tool call rounds) ---
            loop {
                let mut tool_calls: Vec<ToolCall> = Vec::new();
                let mut usage_text = String::new();

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
                                usage_text =
                                    format!("\n\n[{} tok]", cumulative_tokens);
                            }
                        }
                        LoopEvent::Error { message } => {
                            let _ = tx.send(Event::StreamToken {
                                content: String::new(),
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
                    let _ = tx.send(Event::StreamToken {
                        content: usage_text,
                        reasoning: None,
                        done: true,
                        error: None,
                    });
                    break;
                }

                // Notify UI that tools are being executed
                let _ = tx.send(Event::StreamToken {
                    content: format!("\n[{} tool call(s)]", tool_calls.len()),
                    reasoning: None,
                    done: false,
                    error: None,
                });

                // Execute tools and collect results
                let results: Vec<(String, String)> = tool_calls
                    .iter()
                    .map(|call| {
                        let result = execute_tool(&call.function.name, &call.function.arguments)
                            .unwrap_or_else(|e| format!("Error: {}", e));
                        (call.id.clone(), result)
                    })
                    .collect();

                // Submit results back to the agent and get next round of events
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

// ---------------------------------------------------------------------------
// Tool definitions and execution (mirrors artemis-cli/src/commands/run.rs)
// ---------------------------------------------------------------------------

fn tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition::new(
            "read_file".into(),
            "Read the contents of a file at the given path".into(),
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Absolute path to the file to read"
                    }
                },
                "required": ["path"]
            }),
        ),
        ToolDefinition::new(
            "grep".into(),
            "Search for a pattern in files within a directory".into(),
            serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Search pattern (regex)"
                    },
                    "path": {
                        "type": "string",
                        "description": "Directory path to search in (default: current directory)"
                    }
                },
                "required": ["pattern"]
            }),
        ),
    ]
}

fn execute_tool(name: &str, args_json: &str) -> Result<String> {
    match name {
        "read_file" => {
            #[derive(Deserialize)]
            struct Args {
                path: String,
            }
            let args: Args =
                serde_json::from_str(args_json).map_err(|e| anyhow!("Invalid args: {}", e))?;
            std::fs::read_to_string(&args.path)
                .map_err(|e| anyhow!("Failed to read {}: {}", args.path, e))
        }
        "grep" => {
            #[derive(Deserialize)]
            struct Args {
                pattern: String,
                path: Option<String>,
            }
            let args: Args =
                serde_json::from_str(args_json).map_err(|e| anyhow!("Invalid args: {}", e))?;
            let dir = args.path.unwrap_or_else(|| ".".to_string());
            let output = std::process::Command::new("grep")
                .args(["-rn", "--color=never", &args.pattern, &dir])
                .output()
                .map_err(|e| anyhow!("Failed to run grep: {}", e))?;
            if output.status.success() {
                Ok(String::from_utf8_lossy(&output.stdout).to_string())
            } else if output.status.code() == Some(1) {
                Ok(String::new())
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                Err(anyhow!("grep failed: {}", stderr))
            }
        }
        _ => Err(anyhow!("Unknown tool: {}", name)),
    }
}

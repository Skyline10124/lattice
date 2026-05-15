use crate::session::SessionStore;

use super::app::{permission_modes, summarize_prompt, App};
use super::state::{AppStatus, MenuKind, MenuState};

/// Parsed slash command with its remaining whitespace-separated arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ParsedCommand<'a> {
    pub(super) name: &'a str,
    tail: &'a str,
}

impl<'a> ParsedCommand<'a> {
    pub(super) fn parse(input: &'a str) -> Option<Self> {
        if !input.starts_with('/') {
            return None;
        }
        let trimmed = input.trim();
        let (name, tail) = match trimmed.find(char::is_whitespace) {
            Some(idx) => (&trimmed[..idx], trimmed[idx..].trim()),
            None => (trimmed, ""),
        };
        Some(Self { name, tail })
    }

    pub(super) fn args(&self) -> impl Iterator<Item = &'a str> {
        self.tail.split_whitespace()
    }
}

pub(super) struct CommandRegistry;

impl CommandRegistry {
    pub(super) fn parse(input: &str) -> Option<ParsedCommand<'_>> {
        ParsedCommand::parse(input)
    }

    pub(super) fn execute(app: &mut App, input: &str) -> bool {
        let Some(parsed) = Self::parse(input) else {
            return false;
        };
        let command = parsed.name;
        let mut parts = parsed.args();
        match command {
            "/help" => {
                app.help_open = true;
                true
            }
            "/clear" => {
                app.cancel_active_turn();
                app.messages.clear();
                app.help_open = false;
                true
            }
            "/new" => {
                app.reset_conversation();
                app.push_system_message("Started a fresh agent context.");
                true
            }
            "/model" => {
                let model = parts.collect::<Vec<_>>().join(" ");
                if model.is_empty() {
                    let models = crate::model_inventory::authenticated_models(&app.credentials);
                    let idx = models
                        .iter()
                        .position(|m| *m == app.current_model)
                        .unwrap_or(0);
                    app.menu = Some(MenuState::new(MenuKind::Model, models, idx));
                } else {
                    app.cancel_active_turn();
                    app.current_model = model.clone();
                    app.previous_model.clear();
                    app.current_provider.clear();
                    app.status = AppStatus::Ready;
                    app.clear_runtime_sessions();
                    app.push_system_message(&format!("Model switched to {model}."));
                }
                true
            }
            "/status" => {
                let provider = if app.current_provider.is_empty() {
                    "unresolved"
                } else {
                    &app.current_provider
                };
                let snapshot = app.snapshot_label.as_deref().unwrap_or("snapshot:none");
                app.push_system_message(&format!(
                    "model={} provider={} messages={} tokens={} transcript={} sandbox={} {} {}",
                    app.current_model,
                    provider,
                    app.messages.len(),
                    app.token_count,
                    if app.transcript_mode { "on" } else { "off" },
                    app.security.mode_label,
                    app.agent_label,
                    snapshot,
                ));
                true
            }
            "/permissions" | "/permission" => {
                let mode = parts.next().unwrap_or("");
                if mode.is_empty() {
                    let modes = permission_modes();
                    let idx = modes
                        .iter()
                        .position(|m| m == &app.security.mode_label)
                        .unwrap_or(0);
                    app.menu = Some(MenuState::new(MenuKind::Permissions, modes, idx));
                } else {
                    app.switch_permissions(mode);
                }
                true
            }
            "/approve" => {
                let selector = parts.next().unwrap_or("");
                if selector.is_empty() || parts.next().is_some() {
                    app.push_system_message("Usage: /approve <id|last>");
                } else {
                    app.approve_pending_proposal(selector);
                }
                true
            }
            "/provider" => {
                let arg = parts.collect::<Vec<_>>().join(" ");
                if arg.is_empty() {
                    let providers =
                        crate::model_inventory::authenticated_providers(&app.credentials);
                    if providers.is_empty() {
                        let p = if app.current_provider.is_empty() {
                            "auto"
                        } else {
                            &app.current_provider
                        };
                        app.push_system_message(&format!("Current provider: {p}"));
                    } else {
                        let current = if app.current_provider.is_empty() {
                            "auto"
                        } else {
                            &app.current_provider
                        };
                        let idx = providers.iter().position(|p| p == current).unwrap_or(0);
                        app.menu = Some(MenuState::new(MenuKind::Provider, providers, idx));
                    }
                } else {
                    app.cancel_active_turn();
                    app.provider_override = Some(arg.clone());
                    app.current_provider = arg;
                    app.clear_runtime_sessions();
                    app.push_system_message("Provider override set. Agent will rebuild next turn.");
                }
                true
            }
            "/tokens" => {
                use lattice_core::tokens::TokenEstimator;
                let content_tokens: usize = app
                    .messages
                    .iter()
                    .map(|m| TokenEstimator::estimate_text(&m.content) as usize)
                    .sum();
                let reasoning_tokens: usize = app
                    .messages
                    .iter()
                    .filter_map(|m| {
                        m.reasoning
                            .as_ref()
                            .map(|r| TokenEstimator::estimate_text(r) as usize)
                    })
                    .sum();
                app.push_system_message(&format!(
                    "tokens: {} content + {} reasoning = {} total ({} messages)",
                    content_tokens,
                    reasoning_tokens,
                    content_tokens + reasoning_tokens,
                    app.messages.len(),
                ));
                true
            }
            "/find" | "/search" => {
                let query = parts.collect::<Vec<_>>().join(" ");
                if query.is_empty() {
                    if app.activity.search.is_active() {
                        let status = match app.activity.search.position() {
                            Some((current, total)) => format!(
                                "Search: '{}' ({current}/{total}). Use /next, /prev, Esc to clear.",
                                app.activity.search.query
                            ),
                            None => {
                                format!("Search: '{}' (no matches).", app.activity.search.query)
                            }
                        };
                        app.push_system_message(&status);
                    } else {
                        app.push_system_message("Usage: /find <text>");
                    }
                } else {
                    app.start_search(query);
                }
                true
            }
            "/next" => {
                app.jump_search_next();
                true
            }
            "/prev" | "/previous" => {
                app.jump_search_prev();
                true
            }
            "/trace" => {
                app.transcript_mode = !app.transcript_mode;
                app.push_system_message(&format!(
                    "Trace {}. {}",
                    if app.transcript_mode { "on" } else { "off" },
                    if app.transcript_mode {
                        "Reasoning will be shown above the composer."
                    } else {
                        "Reasoning hidden by default."
                    },
                ));
                true
            }
            "/transcript" => {
                app.open_transcript_pager();
                true
            }
            "/detail" | "/diff" => {
                app.open_recent_tool_pager();
                true
            }
            "/effort" => {
                let level = parts.next().unwrap_or("");
                match level {
                    "off" | "none" => {
                        app.cancel_active_turn();
                        app.thinking_effort = None;
                        app.clear_runtime_sessions();
                        app.push_system_message(
                            "Thinking effort: auto (model default). Agent will rebuild.",
                        );
                    }
                    "low" | "medium" | "high" | "xhigh" | "max" => {
                        app.cancel_active_turn();
                        app.thinking_effort = Some(level.to_string());
                        app.clear_runtime_sessions();
                        app.push_system_message(&format!(
                            "Thinking effort: {level}. Agent will rebuild next turn."
                        ));
                    }
                    "" => {
                        let current = app.thinking_effort.as_deref().unwrap_or("auto");
                        app.push_system_message(&format!(
                            "Thinking effort: {current}. Use /effort <off|low|medium|high|xhigh|max>."
                        ));
                    }
                    _ => {
                        app.push_system_message("Usage: /effort <off|low|medium|high|xhigh|max>");
                    }
                }
                true
            }
            "/expand" => {
                app.toggle_tool_expand();
                true
            }
            "/copy" => {
                app.copy_last_assistant();
                true
            }
            "/save" => {
                if let Some(session) = app.session.clone() {
                    let manager = crate::session::session_manager();
                    match manager.save(&session) {
                        Ok(_) => app.push_system_message(&format!(
                            "Session saved ({} messages, {} tok).",
                            session.messages.len(),
                            app.token_count,
                        )),
                        Err(e) => app.push_system_message(&format!("Save failed: {e}")),
                    }
                } else {
                    app.push_system_message("No session to save yet. Send a message first.");
                }
                true
            }
            "/queue" => {
                let sub = parts.next().unwrap_or("list");
                match sub {
                    "clear" => {
                        let count = app.turn.queued_inputs.len();
                        app.turn.queued_inputs.clear();
                        app.push_system_message(&format!("Cleared {count} queued prompt(s)."));
                    }
                    _ => {
                        if app.turn.queued_inputs.is_empty() {
                            app.push_system_message("Queue is empty.");
                        } else {
                            let lines = app
                                .turn
                                .queued_inputs
                                .iter()
                                .enumerate()
                                .map(|(idx, queued)| {
                                    format!("  {}. {}", idx + 1, summarize_prompt(queued, 80))
                                })
                                .collect::<Vec<_>>()
                                .join("\n");
                            app.push_system_message(&format!(
                                "{} queued prompt(s) · /queue clear to drop:\n{}",
                                app.turn.queued_inputs.len(),
                                lines
                            ));
                        }
                    }
                }
                true
            }
            "/sessions" => {
                let sub = parts.next().unwrap_or("list");
                let manager = crate::session::session_manager();
                match sub {
                    "restore" | "load" | "checkout" => {
                        let id = parts.next().unwrap_or("");
                        if id.is_empty() {
                            app.push_system_message("Usage: /sessions checkout <id-or-branch>");
                        } else {
                            match manager.checkout(id) {
                                Ok(session) => {
                                    let fid = session.id.clone();
                                    let model = session.model.clone();
                                    let msgs = session.messages.len();
                                    app.load_session(session);
                                    app.push_system_message(&format!(
                                        "Checked out session {} ({model}, {msgs} messages).",
                                        &fid[..fid.len().min(8)],
                                    ));
                                }
                                Err(err) => {
                                    app.push_system_message(&format!("Checkout failed: {err}"))
                                }
                            }
                        }
                    }
                    "current" => match manager.current() {
                        Ok(Some(session)) => app.push_system_message(&format!(
                            "Current session {} ({} messages).",
                            &session.id[..session.id.len().min(8)],
                            session.messages.len()
                        )),
                        Ok(None) => app.push_system_message("No current session."),
                        Err(err) => app.push_system_message(&format!("Failed: {err}")),
                    },
                    "branch" => {
                        let name = parts.next().unwrap_or("");
                        if name.is_empty() {
                            app.push_system_message("Usage: /sessions branch <name>");
                        } else {
                            match manager.create_branch(name, None) {
                                Ok(session) => app.push_system_message(&format!(
                                    "Created branch {name} at session {}.",
                                    &session.id[..session.id.len().min(8)]
                                )),
                                Err(err) => {
                                    app.push_system_message(&format!("Branch failed: {err}"))
                                }
                            }
                        }
                    }
                    "branches" => match manager.branches() {
                        Ok(branches) if branches.is_empty() => {
                            app.push_system_message("No saved session branches.")
                        }
                        Ok(branches) => {
                            let lines = branches
                                .iter()
                                .map(|b| {
                                    format!(
                                        "{} {}  {}",
                                        if b.is_current { "*" } else { " " },
                                        b.name,
                                        &b.session_id[..b.session_id.len().min(8)],
                                    )
                                })
                                .collect::<Vec<_>>()
                                .join("\n");
                            app.push_system_message(&format!(
                                "{} branch(es):\n{}",
                                branches.len(),
                                lines
                            ));
                        }
                        Err(err) => app.push_system_message(&format!("Failed: {err}")),
                    },
                    _ => match manager.list() {
                        Ok(sessions) => {
                            if sessions.is_empty() {
                                app.push_system_message("No saved sessions. Use /sessions checkout <id-or-branch> to load one.");
                            } else {
                                let lines: Vec<String> = sessions
                                    .iter()
                                    .map(|s| {
                                        format!(
                                            "{} {}  {} msgs  {}  {}",
                                            if s.is_current { "*" } else { " " },
                                            &s.id[..s.id.len().min(8)],
                                            s.message_count,
                                            s.model,
                                            &s.created_at[..s.created_at.len().min(16)],
                                        )
                                    })
                                    .collect();
                                app.push_system_message(&format!(
                                        "{} session(s) · /sessions checkout <id-or-branch> to load:\n{}",
                                        sessions.len(),
                                        lines.join("\n"),
                                    ));
                            }
                        }
                        Err(e) => app.push_system_message(&format!("Failed: {e}")),
                    },
                }
                true
            }
            "/quit" | "/exit" => {
                app.should_quit = true;
                true
            }
            _ => {
                app.push_system_message(&format!(
                    "Unknown command: {command}. Type /help for shortcuts."
                ));
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_command_name_and_tail() {
        let parsed = CommandRegistry::parse("/model gpt-5.4").unwrap();

        assert_eq!(parsed.name, "/model");
        assert_eq!(parsed.tail, "gpt-5.4");
        assert_eq!(parsed.args().collect::<Vec<_>>(), vec!["gpt-5.4"]);
    }

    #[test]
    fn ignores_non_slash_input() {
        assert!(CommandRegistry::parse("hello").is_none());
    }
}

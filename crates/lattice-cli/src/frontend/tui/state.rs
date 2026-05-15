use std::cell::{Cell, RefCell};
use std::sync::Arc;

use lattice_core::types::Role;
use ratatui::text::Line;

/// A single message in the chat.
pub(in crate::frontend::tui) struct ChatMessage {
    pub(in crate::frontend::tui) role: Role,
    pub(in crate::frontend::tui) content: String,
    pub(in crate::frontend::tui) reasoning: Option<String>,
    pub(in crate::frontend::tui) collapsed: bool,
    pub(in crate::frontend::tui) reasoning_collapsed: bool,
    pub(in crate::frontend::tui) tool: Option<ToolDisplay>,
    cached_lines: RefCell<Option<(usize, Arc<Vec<Line<'static>>>)>>,
    cached_width: Cell<Option<u16>>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(in crate::frontend::tui) enum ToolStatus {
    Running,
    Done,
    Error,
}

#[derive(Debug, Clone)]
pub(in crate::frontend::tui) struct ToolDisplay {
    pub(in crate::frontend::tui) call_id: String,
    pub(in crate::frontend::tui) name: String,
    pub(in crate::frontend::tui) arguments: String,
    pub(in crate::frontend::tui) result: Option<String>,
    pub(in crate::frontend::tui) file_diff: Option<FileDiffDisplay>,
    pub(in crate::frontend::tui) file_before: Option<FileSnapshot>,
    pub(in crate::frontend::tui) status: ToolStatus,
    pub(in crate::frontend::tui) started_at: std::time::Instant,
    pub(in crate::frontend::tui) finished_at: Option<std::time::Instant>,
    pub(in crate::frontend::tui) grouped_calls: Vec<ToolCallDisplay>,
}

#[derive(Debug, Clone)]
pub(in crate::frontend::tui) struct ToolCallDisplay {
    pub(in crate::frontend::tui) call_id: String,
    pub(in crate::frontend::tui) arguments: String,
    pub(in crate::frontend::tui) result: Option<String>,
    pub(in crate::frontend::tui) file_diff: Option<FileDiffDisplay>,
    pub(in crate::frontend::tui) file_before: Option<FileSnapshot>,
    pub(in crate::frontend::tui) status: ToolStatus,
    pub(in crate::frontend::tui) started_at: std::time::Instant,
    pub(in crate::frontend::tui) finished_at: Option<std::time::Instant>,
}

#[derive(Debug, Clone)]
pub(in crate::frontend::tui) struct FileSnapshot {
    pub(in crate::frontend::tui) path: String,
    pub(in crate::frontend::tui) content: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::frontend::tui) struct FileDiffDisplay {
    pub(in crate::frontend::tui) path: String,
    pub(in crate::frontend::tui) text: String,
}

impl Clone for ChatMessage {
    fn clone(&self) -> Self {
        Self {
            role: self.role.clone(),
            content: self.content.clone(),
            reasoning: self.reasoning.clone(),
            collapsed: self.collapsed,
            reasoning_collapsed: self.reasoning_collapsed,
            tool: self.tool.clone(),
            cached_lines: RefCell::new(None),
            cached_width: Cell::new(None),
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
            .field("reasoning_collapsed", &self.reasoning_collapsed)
            .field("tool", &self.tool)
            .finish_non_exhaustive()
    }
}

impl ChatMessage {
    pub(in crate::frontend::tui) fn new(role: Role, content: String) -> Self {
        Self {
            role,
            content,
            reasoning: None,
            collapsed: false,
            reasoning_collapsed: true,
            tool: None,
            cached_lines: RefCell::new(None),
            cached_width: Cell::new(None),
        }
    }

    pub(in crate::frontend::tui) fn tool_call(
        call_id: String,
        name: String,
        arguments: String,
    ) -> Self {
        let mut message = Self::new(Role::Tool, String::new());
        message.collapsed = true;
        message.tool = Some(ToolDisplay {
            call_id,
            name,
            arguments,
            result: None,
            file_diff: None,
            file_before: None,
            status: ToolStatus::Running,
            started_at: std::time::Instant::now(),
            finished_at: None,
            grouped_calls: Vec::new(),
        });
        message
    }

    pub(in crate::frontend::tui) fn set_cache(
        &self,
        content_len: usize,
        lines: Vec<Line<'static>>,
        width: u16,
    ) {
        self.cached_lines
            .replace(Some((content_len, Arc::new(lines))));
        self.cached_width.set(Some(width));
    }

    pub(in crate::frontend::tui) fn get_cache(
        &self,
        content_len: usize,
        width: u16,
    ) -> Option<Arc<Vec<Line<'static>>>> {
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

    pub(in crate::frontend::tui) fn invalidate_cache(&self) {
        self.cached_lines.replace(None);
        self.cached_width.set(None);
    }
}

#[derive(Debug, Clone, Copy)]
pub(in crate::frontend::tui) struct SlashSuggestion {
    pub(in crate::frontend::tui) command: &'static str,
    pub(in crate::frontend::tui) description: &'static str,
}

pub(in crate::frontend::tui) const SELECTION_LIST_PAGE_SIZE: usize = 7;

pub(in crate::frontend::tui) const SLASH_COMMANDS: &[SlashSuggestion] = &[
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
        command: "/permissions <mode>",
        description: "switch sandbox mode for later turns",
    },
    SlashSuggestion {
        command: "/approve <id|last>",
        description: "approve the latest or selected permission proposal",
    },
    SlashSuggestion {
        command: "/tokens",
        description: "show token breakdown",
    },
    SlashSuggestion {
        command: "/find <text>",
        description: "search the visible transcript",
    },
    SlashSuggestion {
        command: "/next",
        description: "jump to next search match",
    },
    SlashSuggestion {
        command: "/prev",
        description: "jump to previous search match",
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
        command: "/transcript",
        description: "open full transcript pager",
    },
    SlashSuggestion {
        command: "/detail",
        description: "open latest tool detail pager",
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
        command: "/queue",
        description: "show or clear queued prompts",
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

impl SlashSuggestion {
    pub(in crate::frontend::tui) fn name(&self) -> &'static str {
        self.command
            .split_whitespace()
            .next()
            .unwrap_or(self.command)
    }

    pub(in crate::frontend::tui) fn completion(&self) -> String {
        match self.command.find('<') {
            Some(idx) => self.command[..idx].trim_end().to_string() + " ",
            None => self.name().to_string(),
        }
    }
}

#[derive(Debug, Clone)]
pub(in crate::frontend::tui) struct MenuState {
    pub(in crate::frontend::tui) kind: MenuKind,
    pub(in crate::frontend::tui) options: Vec<String>,
    pub(in crate::frontend::tui) index: usize,
    pub(in crate::frontend::tui) window_start: usize,
}

#[derive(Debug, Clone)]
pub(in crate::frontend::tui) enum MenuKind {
    Model,
    Provider,
    Permissions,
}

impl MenuState {
    pub(in crate::frontend::tui) fn new(
        kind: MenuKind,
        options: Vec<String>,
        index: usize,
    ) -> Self {
        let mut menu = Self {
            kind,
            options,
            index,
            window_start: 0,
        };
        menu.clamp();
        menu
    }

    pub(in crate::frontend::tui) fn selected(&self) -> Option<&str> {
        self.options.get(self.index).map(String::as_str)
    }

    pub(in crate::frontend::tui) fn move_up(&mut self) {
        if self.index > 0 {
            self.index -= 1;
        }
        if self.index < self.window_start {
            self.window_start = self.index;
        }
    }

    pub(in crate::frontend::tui) fn move_down(&mut self) {
        if self.index + 1 < self.options.len() {
            self.index += 1;
        }
        if self.index >= self.window_start + SELECTION_LIST_PAGE_SIZE {
            self.window_start = self.index + 1 - SELECTION_LIST_PAGE_SIZE;
        }
    }

    fn clamp(&mut self) {
        if self.options.is_empty() {
            self.index = 0;
            self.window_start = 0;
            return;
        }
        self.index = self.index.min(self.options.len() - 1);
        let max_start = self.options.len().saturating_sub(SELECTION_LIST_PAGE_SIZE);
        self.window_start = self.window_start.min(max_start);
        if self.index < self.window_start {
            self.window_start = self.index;
        } else if self.index >= self.window_start + SELECTION_LIST_PAGE_SIZE {
            self.window_start = self.index + 1 - SELECTION_LIST_PAGE_SIZE;
        }
    }
}

#[cfg(test)]
mod menu_tests {
    use super::*;

    #[test]
    fn menu_window_scrolls_only_at_visible_edges() {
        let options = (0..12).map(|idx| format!("option-{idx}")).collect();
        let mut menu = MenuState::new(MenuKind::Model, options, 0);

        for _ in 0..SELECTION_LIST_PAGE_SIZE - 1 {
            menu.move_down();
        }
        assert_eq!(menu.index, SELECTION_LIST_PAGE_SIZE - 1);
        assert_eq!(menu.window_start, 0);

        menu.move_down();
        assert_eq!(menu.index, SELECTION_LIST_PAGE_SIZE);
        assert_eq!(menu.window_start, 1);

        menu.move_up();
        assert_eq!(menu.index, SELECTION_LIST_PAGE_SIZE - 1);
        assert_eq!(menu.window_start, 1);

        for _ in 0..SELECTION_LIST_PAGE_SIZE - 2 {
            menu.move_up();
        }
        assert_eq!(menu.index, 1);
        assert_eq!(menu.window_start, 1);

        menu.move_up();
        assert_eq!(menu.index, 0);
        assert_eq!(menu.window_start, 0);
    }
}

#[derive(Debug, Clone, Default)]
pub(in crate::frontend::tui) struct SearchState {
    pub(in crate::frontend::tui) query: String,
    pub(in crate::frontend::tui) matches: Vec<usize>,
    pub(in crate::frontend::tui) index: Option<usize>,
    pub(in crate::frontend::tui) target_message: Option<usize>,
}

impl SearchState {
    pub(in crate::frontend::tui) fn is_active(&self) -> bool {
        !self.query.is_empty()
    }

    pub(in crate::frontend::tui) fn position(&self) -> Option<(usize, usize)> {
        self.index
            .filter(|_| !self.matches.is_empty())
            .map(|idx| (idx + 1, self.matches.len()))
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

pub(in crate::frontend::tui) fn tool_result_status(result: Option<&str>) -> ToolStatus {
    let result = result.unwrap_or_default().trim_start().to_ascii_lowercase();
    if result.starts_with("error")
        || result.starts_with("sandbox violation")
        || result.contains("permission denied")
        || result.contains("timed out")
    {
        ToolStatus::Error
    } else {
        ToolStatus::Done
    }
}

pub(in crate::frontend::tui) fn tool_display_content(tool: &ToolDisplay) -> String {
    let mut content = tool_call_display_content(
        &tool.name,
        &tool.arguments,
        tool.status,
        tool.started_at,
        tool.finished_at,
        tool.result.as_deref(),
    );
    for call in &tool.grouped_calls {
        content.push('\n');
        content.push('\n');
        content.push_str(&tool_call_display_content(
            &tool.name,
            &call.arguments,
            call.status,
            call.started_at,
            call.finished_at,
            call.result.as_deref(),
        ));
    }
    content
}

fn tool_call_display_content(
    name: &str,
    arguments: &str,
    status: ToolStatus,
    started_at: std::time::Instant,
    finished_at: Option<std::time::Instant>,
    result: Option<&str>,
) -> String {
    let mut content = format!("{name} {}", compact_json(arguments));
    let elapsed = finished_at
        .unwrap_or_else(std::time::Instant::now)
        .saturating_duration_since(started_at);
    let status = match status {
        ToolStatus::Running => "running".to_string(),
        ToolStatus::Done => format!("done in {}", format_short_duration(elapsed)),
        ToolStatus::Error => format!("error after {}", format_short_duration(elapsed)),
    };
    content.push_str(&format!("\nstatus: {status}"));
    if let Some(result) = result {
        content.push('\n');
        content.push_str(&trim_tool_result(result));
    }
    content
}

fn format_short_duration(duration: std::time::Duration) -> String {
    let millis = duration.as_millis();
    if millis < 1000 {
        format!("{millis}ms")
    } else {
        let secs = duration.as_secs_f32();
        format!("{secs:.1}s")
    }
}

pub(in crate::frontend::tui) fn message_search_content(
    msg: &ChatMessage,
    transcript_mode: bool,
) -> String {
    if transcript_mode {
        match msg.reasoning.as_deref() {
            Some(reasoning) if !reasoning.is_empty() => {
                format!("{}\n{}", msg.content, reasoning)
            }
            _ => msg.content.clone(),
        }
    } else {
        msg.content.clone()
    }
}

pub(in crate::frontend::tui) fn estimated_message_rows(msg: &ChatMessage) -> usize {
    let content_rows = msg.content.lines().count().max(1);
    content_rows.saturating_add(1)
}

#[derive(Debug, Clone)]
pub(in crate::frontend::tui) struct TextSelection {
    pub(in crate::frontend::tui) start_row: u16,
    pub(in crate::frontend::tui) start_col: u16,
    pub(in crate::frontend::tui) end_row: u16,
    pub(in crate::frontend::tui) end_col: u16,
    pub(in crate::frontend::tui) active: bool,
}

#[derive(Debug, Clone)]
pub(in crate::frontend::tui) struct ClickZone {
    pub(in crate::frontend::tui) rect: (u16, u16, u16, u16),
    pub(in crate::frontend::tui) action: ClickAction,
}

#[derive(Debug, Clone)]
pub(in crate::frontend::tui) enum ClickAction {
    JumpToBottom,
}

#[derive(Debug, Clone, PartialEq)]
pub(in crate::frontend::tui) enum AppStatus {
    Ready,
    Streaming,
    Error(String),
}

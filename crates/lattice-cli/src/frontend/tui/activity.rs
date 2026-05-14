use std::cell::{Cell, RefCell};

use std::path::{Path, PathBuf};

use lattice_core::types::Role;

use super::state::{
    estimated_message_rows, message_search_content, tool_display_content, tool_result_status,
    ChatMessage, ClickZone, FileDiffDisplay, FileSnapshot, SearchState, TextSelection,
    ToolCallDisplay, ToolDisplay, ToolStatus,
};
use super::turn::TurnState;

/// Transcript-side interaction state: search, selection, and render hit targets.
#[derive(Debug)]
pub(super) struct ActivityState {
    pub(super) search: SearchState,
    pub(super) selection: Option<TextSelection>,
    pub(super) click_zones: RefCell<Vec<ClickZone>>,
    pub(super) visible_rows: RefCell<Vec<String>>,
    pub(super) visible_rows_origin: Cell<u16>,
}

pub(super) struct ActivityContext<'a> {
    pub(super) turn: &'a mut TurnState,
    pub(super) messages: &'a mut Vec<ChatMessage>,
    pub(super) workdir: &'a Path,
    pub(super) security: &'a crate::security::RuntimeSecurity,
}

pub(super) struct ToolOutputUpdate {
    pub(super) turn_id: u64,
    pub(super) call_id: String,
    pub(super) name: String,
    pub(super) arguments: String,
    pub(super) result: Option<String>,
    pub(super) file_before: Option<FileSnapshot>,
}

impl Default for ActivityState {
    fn default() -> Self {
        Self {
            search: SearchState::default(),
            selection: None,
            click_zones: RefCell::new(Vec::new()),
            visible_rows: RefCell::new(Vec::new()),
            visible_rows_origin: Cell::new(0),
        }
    }
}

impl ActivityState {
    pub(super) fn start_search(
        &mut self,
        messages: &[ChatMessage],
        transcript_mode: bool,
        query: String,
    ) {
        let query = query.trim().to_string();
        if query.is_empty() {
            self.clear_search();
            return;
        }

        let lowered = query.to_ascii_lowercase();
        let matches = messages
            .iter()
            .enumerate()
            .filter_map(|(idx, msg)| {
                message_search_content(msg, transcript_mode)
                    .to_ascii_lowercase()
                    .contains(&lowered)
                    .then_some(idx)
            })
            .collect::<Vec<_>>();

        self.search = SearchState {
            query,
            target_message: matches.first().copied(),
            index: (!matches.is_empty()).then_some(0),
            matches,
        };
    }

    pub(super) fn jump_search_next(&mut self) {
        if self.search.matches.is_empty() {
            return;
        }
        let next = self
            .search
            .index
            .map(|idx| (idx + 1) % self.search.matches.len())
            .unwrap_or(0);
        self.search.index = Some(next);
        self.search.target_message = self.search.matches.get(next).copied();
    }

    pub(super) fn jump_search_prev(&mut self) {
        if self.search.matches.is_empty() {
            return;
        }
        let prev = self
            .search
            .index
            .map(|idx| {
                if idx == 0 {
                    self.search.matches.len() - 1
                } else {
                    idx - 1
                }
            })
            .unwrap_or(0);
        self.search.index = Some(prev);
        self.search.target_message = self.search.matches.get(prev).copied();
    }

    pub(super) fn refresh_search_matches(
        &mut self,
        messages: &[ChatMessage],
        transcript_mode: bool,
    ) {
        if !self.search.is_active() {
            return;
        }
        let query = self.search.query.clone();
        let previous_target = self.search.target_message;
        self.start_search(messages, transcript_mode, query);
        let index = previous_target
            .and_then(|target| self.search.matches.iter().position(|m| *m == target))
            .or_else(|| (!self.search.matches.is_empty()).then_some(0));
        self.search.index = index;
        self.search.target_message = index.and_then(|idx| self.search.matches.get(idx).copied());
    }

    pub(super) fn target_scroll_offset(&self, messages: &[ChatMessage]) -> Option<usize> {
        let target = self.search.target_message?;
        let rows_after_target = messages
            .iter()
            .skip(target.saturating_add(1))
            .map(estimated_message_rows)
            .sum::<usize>();
        Some(rows_after_target.saturating_add(2))
    }

    pub(super) fn clear_search(&mut self) {
        self.search = SearchState::default();
    }

    pub(super) fn clear_render_state(&self) {
        self.click_zones.borrow_mut().clear();
    }

    pub(super) fn apply_tool_output(
        &mut self,
        context: &mut ActivityContext<'_>,
        update: ToolOutputUpdate,
    ) -> bool {
        if !context.turn.accepts(update.turn_id) {
            return false;
        }

        let existing_tool_idx = find_tool_message_index(context.messages, &update.call_id);
        if existing_tool_idx.is_none() {
            close_active_assistant_for_tool(context);
        }

        if let Some(result) = update.result {
            let file_diff = if tool_result_status(Some(&result)) == ToolStatus::Done {
                context
                    .messages
                    .iter()
                    .filter_map(|msg| msg.tool.as_ref())
                    .find(|tool| tool_contains_call(tool, &update.call_id))
                    .and_then(|tool| tool_file_before_for_call(tool, &update.call_id))
                    .and_then(|snapshot| {
                        completed_file_diff(snapshot, context.workdir, context.security)
                    })
            } else {
                None
            };

            if let Some(idx) = existing_tool_idx {
                update_tool_result(
                    &mut context.messages[idx],
                    &update.call_id,
                    result,
                    file_diff,
                );
            } else {
                append_new_tool_call(
                    context.messages,
                    update.call_id,
                    update.name,
                    update.arguments,
                    Some(result),
                    update.file_before,
                );
            }
        } else if existing_tool_idx.is_none() {
            append_new_tool_call(
                context.messages,
                update.call_id,
                update.name,
                update.arguments,
                None,
                update.file_before,
            );
        }
        true
    }
}

fn close_active_assistant_for_tool(context: &mut ActivityContext<'_>) {
    let Some(idx) = context.turn.active_assistant_index else {
        return;
    };
    match context.messages.get(idx) {
        Some(last)
            if last.role == Role::Assistant
                && last.content.is_empty()
                && last.reasoning.as_deref().unwrap_or("").is_empty() =>
        {
            context.messages.remove(idx);
        }
        Some(last) if last.role == Role::Assistant => {}
        _ => {}
    }
    context.turn.active_assistant_index = None;
}

fn find_tool_message_index(messages: &[ChatMessage], call_id: &str) -> Option<usize> {
    messages.iter().position(|msg| {
        msg.tool
            .as_ref()
            .is_some_and(|tool| tool_contains_call(tool, call_id))
    })
}

fn tool_contains_call(tool: &ToolDisplay, call_id: &str) -> bool {
    tool.call_id == call_id
        || tool
            .grouped_calls
            .iter()
            .any(|call| call.call_id == call_id)
}

fn tool_file_before_for_call<'a>(tool: &'a ToolDisplay, call_id: &str) -> Option<&'a FileSnapshot> {
    if tool.call_id == call_id {
        return tool.file_before.as_ref();
    }
    tool.grouped_calls
        .iter()
        .find(|call| call.call_id == call_id)
        .and_then(|call| call.file_before.as_ref())
}

fn append_new_tool_call(
    messages: &mut Vec<ChatMessage>,
    call_id: String,
    name: String,
    arguments: String,
    result: Option<String>,
    file_before: Option<FileSnapshot>,
) {
    if let Some(msg) = messages.last_mut() {
        if let Some(tool) = msg
            .tool
            .as_mut()
            .filter(|tool| tool.name == name && !tool_contains_call(tool, &call_id))
        {
            let status = result
                .as_deref()
                .map(|result| tool_result_status(Some(result)))
                .unwrap_or(ToolStatus::Running);
            tool.grouped_calls.push(ToolCallDisplay {
                call_id,
                arguments,
                result,
                file_diff: None,
                file_before,
                status,
                started_at: std::time::Instant::now(),
                finished_at: None,
            });
            if let Some(call) = tool.grouped_calls.last_mut() {
                if call.result.is_some() {
                    call.finished_at = Some(std::time::Instant::now());
                }
            }
            msg.content = tool_display_content(tool);
            msg.invalidate_cache();
            return;
        }
    }

    let mut msg = ChatMessage::tool_call(call_id, name, arguments);
    if let Some(tool) = msg.tool.as_mut() {
        tool.file_before = file_before;
        if let Some(result) = result {
            tool.result = Some(result);
            tool.status = tool_result_status(tool.result.as_deref());
            tool.finished_at = Some(std::time::Instant::now());
        }
        msg.content = tool_display_content(tool);
    }
    messages.push(msg);
}

fn update_tool_result(
    msg: &mut ChatMessage,
    call_id: &str,
    result: String,
    file_diff: Option<FileDiffDisplay>,
) {
    if let Some(tool) = msg.tool.as_mut() {
        if tool.call_id == call_id {
            tool.result = Some(result);
            tool.status = tool_result_status(tool.result.as_deref());
            tool.file_diff = file_diff;
            tool.finished_at = Some(std::time::Instant::now());
        } else if let Some(call) = tool
            .grouped_calls
            .iter_mut()
            .find(|call| call.call_id == call_id)
        {
            call.result = Some(result);
            call.status = tool_result_status(call.result.as_deref());
            call.file_diff = file_diff;
            call.finished_at = Some(std::time::Instant::now());
        }
        msg.content = tool_display_content(tool);
        msg.invalidate_cache();
    }
}

pub(super) fn file_snapshot_for_tool(
    name: &str,
    arguments: &str,
    workdir: &Path,
    security: &crate::security::RuntimeSecurity,
) -> Option<FileSnapshot> {
    let path = editable_file_path(name, arguments)?;
    let resolved = resolve_tool_file_path(workdir, &path);
    let resolved_str = resolved.to_string_lossy();
    if security.sandbox.check_write(&resolved_str).is_err() {
        return None;
    }

    let content = match read_file_for_diff(&resolved, &security.sandbox) {
        DiffRead::Content(content) => Some(content),
        DiffRead::Missing => None,
        DiffRead::Unavailable => return None,
    };

    Some(FileSnapshot { path, content })
}

pub(super) fn editable_file_path(name: &str, arguments: &str) -> Option<String> {
    match name {
        "write_file" => {
            let args = serde_json::from_str::<serde_json::Value>(arguments.trim()).ok()?;
            let path = args.get("path")?.as_str()?.trim();
            (!path.is_empty()).then(|| path.to_string())
        }
        "patch" => structured_patch_first_path(arguments),
        _ => None,
    }
}

fn structured_patch_first_path(arguments: &str) -> Option<String> {
    let args = serde_json::from_str::<serde_json::Value>(arguments.trim()).ok()?;
    let patch = args.get("patch")?.as_str()?;
    for line in patch.lines() {
        for prefix in ["*** Add File:", "*** Update File:", "*** Delete File:"] {
            if let Some(path) = line
                .strip_prefix(prefix)
                .map(str::trim)
                .filter(|path| !path.is_empty())
            {
                return Some(path.to_string());
            }
        }
    }
    None
}

fn resolve_tool_file_path(workdir: &Path, path: &str) -> PathBuf {
    workdir.join(path.trim_start_matches('/'))
}

enum DiffRead {
    Content(String),
    Missing,
    Unavailable,
}

fn read_file_for_diff(path: &Path, sandbox: &lattice_sandbox::SandboxConfig) -> DiffRead {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return DiffRead::Missing,
        Err(_) => return DiffRead::Unavailable,
    };

    if !metadata.is_file() || metadata.len() > sandbox.max_read_size as u64 {
        return DiffRead::Unavailable;
    }

    let path_str = path.to_string_lossy();
    if sandbox.check_read(&path_str).is_err() {
        return DiffRead::Unavailable;
    }

    match std::fs::read_to_string(path) {
        Ok(content) => DiffRead::Content(content),
        Err(_) => DiffRead::Unavailable,
    }
}

pub(super) fn completed_file_diff(
    snapshot: &FileSnapshot,
    workdir: &Path,
    security: &crate::security::RuntimeSecurity,
) -> Option<FileDiffDisplay> {
    let resolved = resolve_tool_file_path(workdir, &snapshot.path);
    let new_content = match read_file_for_diff(&resolved, &security.sandbox) {
        DiffRead::Content(content) => content,
        DiffRead::Missing | DiffRead::Unavailable => return None,
    };
    let old_content = snapshot.content.as_deref().unwrap_or_default();
    let old_label = snapshot
        .content
        .as_ref()
        .map(|_| format!("a/{}", snapshot.path))
        .unwrap_or_else(|| "/dev/null".to_string());
    let new_label = format!("b/{}", snapshot.path);
    let diff = unified_diff(&old_label, &new_label, old_content, &new_content)?;

    Some(FileDiffDisplay {
        path: snapshot.path.clone(),
        text: diff,
    })
}

fn unified_diff(
    old_label: &str,
    new_label: &str,
    old_content: &str,
    new_content: &str,
) -> Option<String> {
    if old_content == new_content {
        return None;
    }
    let diff = similar::TextDiff::from_lines(old_content, new_content);
    let mut text = diff
        .unified_diff()
        .header(old_label, new_label)
        .context_radius(3)
        .to_string();
    while text.ends_with('\n') {
        text.pop();
    }
    (!text.trim().is_empty()).then_some(text)
}

use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::super::markdown::render_markdown;
use super::super::state::{ChatMessage, FileDiffDisplay, ToolCallDisplay, ToolDisplay, ToolStatus};
use super::super::theme::Theme;

pub(in crate::frontend::tui) fn message_lines(
    msg: &ChatMessage,
    theme: &Theme,
    _transcript_mode: bool,
    width: u16,
) -> Vec<Line<'static>> {
    if msg.role == lattice_core::types::Role::Assistant && msg.content.is_empty() {
        return Vec::new();
    }

    let (label, label_style, _body_style) = role_styles(msg, theme);
    let mut rows: Vec<Line<'static>> = Vec::new();
    let content_width = stripe_content_width(width) as u16;

    let is_assistant = msg.role == lattice_core::types::Role::Assistant && !msg.content.is_empty();
    let is_tool = msg.role == lattice_core::types::Role::Tool;
    let use_md = is_assistant || (is_tool && !msg.content.is_empty());

    if msg.tool.is_some() {
        rows.extend(tool_lines(msg, theme, content_width));
    } else if is_tool && msg.collapsed {
        let usable_width = content_width.max(1) as usize;
        let prefix_width = label_prefix_width(label);
        let content = display_content(msg);
        let total = content.lines().count();
        let preview: String = content.lines().take(3).collect::<Vec<_>>().join("\n");
        rows.extend(labelled_lines(
            label,
            label_style,
            _body_style,
            &preview,
            usable_width,
        ));
        rows.push(Line::from(vec![
            Span::raw(" ".repeat(prefix_width)),
            Span::styled(
                format!(
                    "... {} more lines · Ctrl+E to expand",
                    total.saturating_sub(3)
                ),
                Style::default()
                    .fg(theme.subtext)
                    .add_modifier(Modifier::ITALIC),
            ),
        ]));
    } else if use_md {
        let prefix_width = label_prefix_width(label);
        let md_width = content_width.saturating_sub(prefix_width as u16).max(20);

        // Use cached render if content+width unchanged; otherwise compute + cache
        let md_lines = match msg.get_cache(msg.content.len(), md_width) {
            Some(cached) => (*cached).clone(),
            None => {
                let lines = render_markdown(&msg.content, theme, md_width);
                msg.set_cache(msg.content.len(), lines.clone(), md_width);
                lines
            }
        };

        let indent = " ".repeat(prefix_width);
        for (i, md_line) in md_lines.into_iter().enumerate() {
            let md_spans = md_line.spans;
            if i == 0 {
                let mut spans = role_prefix_spans(label, label_style);
                spans.extend(md_spans);
                rows.push(Line::from(spans));
            } else {
                let mut spans = vec![Span::raw(indent.clone())];
                spans.extend(md_spans);
                rows.push(Line::from(spans));
            }
        }
    } else {
        let usable_width = content_width.max(1) as usize;
        rows.extend(labelled_lines(
            label,
            label_style,
            _body_style,
            display_content(msg),
            usable_width,
        ));
    }

    trim_trailing_blank_rows(&mut rows);
    if rows.is_empty() {
        return Vec::new();
    }

    stripe_lines(msg, rows, theme, width)
}

fn stripe_content_width(width: u16) -> usize {
    (width as usize)
        .saturating_sub(UnicodeWidthStr::width(STRIPE_PREFIX))
        .max(1)
}

fn stripe_lines(
    msg: &ChatMessage,
    rows: Vec<Line<'static>>,
    theme: &Theme,
    width: u16,
) -> Vec<Line<'static>> {
    let content_width = stripe_content_width(width);
    let stripe_style = stripe_style(msg, theme);
    let mut out = Vec::with_capacity(rows.len().saturating_add(1));

    for row in rows {
        out.push(stripe_body_line(row, stripe_style, content_width));
    }
    out.push(Line::raw(""));
    out
}

const STRIPE_PREFIX: &str = "▌ ";

fn stripe_style(msg: &ChatMessage, theme: &Theme) -> Style {
    match msg.role {
        lattice_core::types::Role::User => Style::default().fg(theme.user_accent),
        lattice_core::types::Role::Assistant => Style::default().fg(theme.assistant_accent),
        lattice_core::types::Role::System => Style::default().fg(theme.subtext),
        lattice_core::types::Role::Tool => Style::default().fg(theme.tool_accent),
    }
}

fn stripe_body_line(mut row: Line<'static>, style: Style, content_width: usize) -> Line<'static> {
    let current_width = line_width(&row);
    let pad = content_width.saturating_sub(current_width);
    let mut spans = vec![Span::styled(STRIPE_PREFIX, style)];
    spans.append(&mut row.spans);
    if pad > 0 {
        spans.push(Span::raw(" ".repeat(pad)));
    }
    Line::from(spans)
}

fn line_width(line: &Line<'_>) -> usize {
    line.spans
        .iter()
        .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
        .sum()
}

fn trim_trailing_blank_rows(rows: &mut Vec<Line<'static>>) {
    while rows.last().is_some_and(line_is_blank) {
        rows.pop();
    }
}

fn line_is_blank(line: &Line<'_>) -> bool {
    line.spans.iter().all(|span| span.content.trim().is_empty())
}

fn role_styles(msg: &ChatMessage, theme: &Theme) -> (&'static str, Style, Style) {
    match msg.role {
        lattice_core::types::Role::User => (
            "",
            theme.user_style().add_modifier(Modifier::BOLD),
            Style::default().fg(theme.text),
        ),
        lattice_core::types::Role::Assistant => (
            "",
            theme.assistant_style().add_modifier(Modifier::BOLD),
            theme.assistant_style(),
        ),
        lattice_core::types::Role::System => (
            "",
            Style::default()
                .fg(theme.subtext)
                .add_modifier(Modifier::BOLD),
            Style::default().fg(theme.subtext),
        ),
        lattice_core::types::Role::Tool => (
            "",
            theme.tool_style().add_modifier(Modifier::BOLD),
            theme.tool_style(),
        ),
    }
}

fn label_prefix_width(label: &str) -> usize {
    if label.is_empty() {
        0
    } else {
        UnicodeWidthStr::width(label) + 1
    }
}

fn display_content(msg: &ChatMessage) -> &str {
    if msg.role == lattice_core::types::Role::Assistant && msg.content.is_empty() {
        "Thinking..."
    } else {
        &msg.content
    }
}

fn tool_lines(msg: &ChatMessage, theme: &Theme, width: u16) -> Vec<Line<'static>> {
    let Some(tool) = msg.tool.as_ref() else {
        return Vec::new();
    };
    let usable_width = width.max(1) as usize;
    if msg.collapsed {
        return vec![tool_summary_line(tool, theme, usable_width)];
    }

    let calls = tool_call_views(tool);
    let mut rows = Vec::new();
    for (idx, call) in calls.iter().enumerate() {
        if idx > 0 {
            rows.push(Line::raw(""));
        }
        rows.push(tool_call_header_line(tool, call, theme, usable_width));
        if let Some(result) = call.result.filter(|result| !result.trim().is_empty()) {
            rows.extend(tool_body_lines(result, theme, usable_width));
        }
        if let Some(diff) = call.file_diff {
            rows.extend(tool_diff_lines(&diff.path, &diff.text, theme, usable_width));
        }
    }
    rows
}

#[derive(Clone, Copy)]
struct ToolCallView<'a> {
    arguments: &'a str,
    result: Option<&'a str>,
    file_diff: Option<&'a FileDiffDisplay>,
    status: ToolStatus,
    started_at: std::time::Instant,
    finished_at: Option<std::time::Instant>,
}

fn tool_call_views(tool: &ToolDisplay) -> Vec<ToolCallView<'_>> {
    let mut calls = Vec::with_capacity(tool.grouped_calls.len().saturating_add(1));
    calls.push(ToolCallView {
        arguments: &tool.arguments,
        result: tool.result.as_deref(),
        file_diff: tool.file_diff.as_ref(),
        status: tool.status,
        started_at: tool.started_at,
        finished_at: tool.finished_at,
    });
    calls.extend(tool.grouped_calls.iter().map(tool_grouped_call_view));
    calls
}

fn tool_grouped_call_view(call: &ToolCallDisplay) -> ToolCallView<'_> {
    ToolCallView {
        arguments: &call.arguments,
        result: call.result.as_deref(),
        file_diff: call.file_diff.as_ref(),
        status: call.status,
        started_at: call.started_at,
        finished_at: call.finished_at,
    }
}

fn tool_summary_line(tool: &ToolDisplay, theme: &Theme, usable_width: usize) -> Line<'static> {
    let calls = tool_call_views(tool);
    let first = calls
        .first()
        .copied()
        .expect("tool card should always include the primary call");
    let mut summary = tool_call_label(&tool.name, first.arguments, usable_width);
    if calls.len() > 1 {
        summary.push_str(&format!(" +{} calls", calls.len().saturating_sub(1)));
    }
    summary.push_str(" · ");
    summary.push_str(&tool_group_status(&calls));
    Line::from(vec![Span::styled(
        truncate_to_width(&summary, usable_width),
        tool_dim_style(theme),
    )])
}

fn tool_call_header_line(
    tool: &ToolDisplay,
    call: &ToolCallView<'_>,
    theme: &Theme,
    usable_width: usize,
) -> Line<'static> {
    let mut summary = tool_call_label(&tool.name, call.arguments, usable_width);
    summary.push_str(" · ");
    summary.push_str(&tool_call_status(*call));
    Line::from(vec![Span::styled(
        truncate_to_width(&summary, usable_width),
        tool_dim_style(theme),
    )])
}

fn tool_call_label(name: &str, raw_args: &str, usable_width: usize) -> String {
    let display_name = tool_display_name(name, raw_args);
    let summary_width = usable_width
        .saturating_sub(UnicodeWidthStr::width(display_name.as_str()))
        .saturating_sub(2);
    let summary = tool_argument_summary(name, raw_args, summary_width);
    if summary.is_empty() {
        display_name
    } else {
        format!("{display_name}({summary})")
    }
}

fn tool_group_status(calls: &[ToolCallView<'_>]) -> String {
    if calls.iter().any(|call| call.status == ToolStatus::Running) {
        return "running".into();
    }
    if calls.iter().any(|call| call.status == ToolStatus::Error) {
        return "error".into();
    }
    if calls.len() == 1 {
        return tool_call_status(calls[0]);
    }
    "done".into()
}

fn tool_call_status(call: ToolCallView<'_>) -> String {
    let elapsed = call
        .finished_at
        .unwrap_or_else(std::time::Instant::now)
        .saturating_duration_since(call.started_at);
    match call.status {
        ToolStatus::Running => "running".into(),
        ToolStatus::Done => format!("done in {}", short_duration(elapsed)),
        ToolStatus::Error => format!("error after {}", short_duration(elapsed)),
    }
}

fn tool_body_lines(content: &str, theme: &Theme, usable_width: usize) -> Vec<Line<'static>> {
    let prefix = "  ";
    let prefix_width = UnicodeWidthStr::width(prefix);
    let body_width = usable_width.saturating_sub(prefix_width).max(1);
    let mut lines = Vec::new();

    for line in normalized_lines(content) {
        for chunk in wrap_line(line, body_width) {
            lines.push(Line::from(vec![
                Span::raw(prefix),
                Span::styled(chunk, tool_dim_style(theme)),
            ]));
        }
    }

    lines
}

fn tool_diff_lines(
    path: &str,
    diff: &str,
    theme: &Theme,
    usable_width: usize,
) -> Vec<Line<'static>> {
    let diff_lines: Vec<&str> = diff.lines().collect();
    if diff_lines.is_empty() {
        return Vec::new();
    }

    let mut rows = vec![Line::from(vec![
        Span::raw("  "),
        Span::styled(
            format!("Diff {path}"),
            tool_dim_style(theme).add_modifier(Modifier::BOLD),
        ),
    ])];
    rows.extend(diff_block_lines(
        &diff_lines.join("\n"),
        theme,
        usable_width,
    ));
    rows
}

fn diff_block_lines(content: &str, theme: &Theme, usable_width: usize) -> Vec<Line<'static>> {
    let border_prefix = "  ";
    let body_prefix = "  ";
    let prefix_width = UnicodeWidthStr::width(body_prefix);
    let body_width = usable_width.saturating_sub(prefix_width).max(1);
    let separator = truncate_to_width(
        &"╌".repeat(usable_width.saturating_sub(UnicodeWidthStr::width(border_prefix))),
        usable_width.saturating_sub(UnicodeWidthStr::width(border_prefix)),
    );
    let mut rows = vec![Line::from(vec![
        Span::raw(border_prefix),
        Span::styled(separator.clone(), tool_dim_style(theme)),
    ])];

    for line in normalized_lines(content) {
        let chunks = wrap_line(line, body_width);
        for (idx, chunk) in chunks.into_iter().enumerate() {
            let prefix = if idx == 0 {
                body_prefix.to_string()
            } else {
                " ".repeat(prefix_width)
            };
            rows.push(Line::from(vec![
                Span::raw(prefix),
                Span::styled(chunk, tool_dim_style(theme)),
            ]));
        }
    }

    rows.push(Line::from(vec![
        Span::raw(border_prefix),
        Span::styled(separator, tool_dim_style(theme)),
    ]));
    rows
}

fn tool_dim_style(theme: &Theme) -> Style {
    Style::default().fg(theme.subtext)
}

fn tool_display_name(name: &str, raw_args: &str) -> String {
    let args = parse_tool_args(raw_args);
    match name {
        "read_file" => "Read".into(),
        "grep" => "Search".into(),
        "write_file" => {
            if string_arg(args.as_ref(), "content").is_some_and(|content| content.is_empty()) {
                "Create".into()
            } else {
                "Write".into()
            }
        }
        "list_directory" => "List".into(),
        "bash" => "Bash".into(),
        "patch" => "Edit".into(),
        "web_search" => "Fetch".into(),
        "bus:fetch" => "Fetch".into(),
        _ => title_case_tool_name(name),
    }
}

fn tool_argument_summary(name: &str, raw_args: &str, max_width: usize) -> String {
    let Some(args) = parse_tool_args(raw_args) else {
        return truncate_to_width(
            &raw_args.split_whitespace().collect::<Vec<_>>().join(" "),
            max_width,
        );
    };

    let summary = match name {
        "bash" => string_arg(Some(&args), "command"),
        "read_file" | "write_file" | "list_directory" => string_arg(Some(&args), "path"),
        "patch" => structured_patch_summary(raw_args),
        "web_search" => string_arg(Some(&args), "url"),
        "bus:fetch" => string_arg(Some(&args), "key"),
        "grep" => grep_summary(&args),
        _ => generic_tool_summary(&args),
    };

    truncate_to_width(&summary.unwrap_or_else(|| args.to_string()), max_width)
}

fn parse_tool_args(raw: &str) -> Option<serde_json::Value> {
    serde_json::from_str::<serde_json::Value>(raw.trim()).ok()
}

fn string_arg(args: Option<&serde_json::Value>, key: &str) -> Option<String> {
    let value = args?.get(key)?;
    match value {
        serde_json::Value::String(value) => Some(value.clone()),
        serde_json::Value::Null => None,
        value => Some(value.to_string()),
    }
}

fn grep_summary(args: &serde_json::Value) -> Option<String> {
    let pattern = string_arg(Some(args), "pattern")?;
    match string_arg(Some(args), "path") {
        Some(path) if !path.is_empty() => Some(format!("{pattern} in {path}")),
        _ => Some(pattern),
    }
}

fn structured_patch_summary(raw_args: &str) -> Option<String> {
    let args = parse_tool_args(raw_args)?;
    let patch = args.get("patch")?.as_str()?;
    let mut paths = Vec::new();
    for line in patch.lines() {
        for prefix in ["*** Add File:", "*** Update File:", "*** Delete File:"] {
            if let Some(path) = line
                .strip_prefix(prefix)
                .map(str::trim)
                .filter(|path| !path.is_empty())
            {
                paths.push(path.to_string());
            }
        }
    }

    Some(match paths.as_slice() {
        [] => "structured patch".into(),
        [path] => path.clone(),
        [first, rest @ ..] => format!("{first} +{} more", rest.len()),
    })
}

fn generic_tool_summary(args: &serde_json::Value) -> Option<String> {
    for key in [
        "path",
        "file_path",
        "command",
        "query",
        "pattern",
        "url",
        "name",
    ] {
        if let Some(value) = string_arg(Some(args), key).filter(|value| !value.is_empty()) {
            return Some(value);
        }
    }
    None
}

fn title_case_tool_name(name: &str) -> String {
    name.split(['_', '-', ':'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => {
                    let mut word = String::new();
                    word.extend(first.to_uppercase());
                    word.push_str(chars.as_str());
                    word
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn truncate_to_width(text: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut width = 0usize;
    for ch in text.chars() {
        let char_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + char_width > max_width {
            if max_width > 1 {
                out.push('…');
            }
            break;
        }
        out.push(ch);
        width += char_width;
    }
    out
}

fn short_duration(duration: std::time::Duration) -> String {
    let millis = duration.as_millis();
    if millis < 1000 {
        format!("{millis}ms")
    } else {
        format!("{:.1}s", duration.as_secs_f32())
    }
}

fn labelled_lines(
    label: &'static str,
    label_style: Style,
    body_style: Style,
    content: &str,
    usable_width: usize,
) -> Vec<Line<'static>> {
    let prefix_width = UnicodeWidthStr::width(label) + 1;
    let body_width = usable_width.saturating_sub(prefix_width).max(1);
    let indent = " ".repeat(prefix_width);
    let mut lines = Vec::new();

    for (logical_idx, line) in normalized_lines(content).enumerate() {
        for (chunk_idx, chunk) in wrap_line(line, body_width).into_iter().enumerate() {
            if logical_idx == 0 && chunk_idx == 0 {
                let mut spans = role_prefix_spans(label, label_style);
                spans.push(Span::styled(chunk, body_style));
                lines.push(Line::from(spans));
            } else {
                lines.push(Line::from(vec![
                    Span::raw(indent.clone()),
                    Span::styled(chunk, body_style),
                ]));
            }
        }
    }

    if lines.is_empty() {
        lines.push(Line::from(role_prefix_spans(label, label_style)));
    }

    lines
}

fn role_prefix_spans(label: &'static str, label_style: Style) -> Vec<Span<'static>> {
    if label.is_empty() {
        Vec::new()
    } else {
        vec![Span::styled(label, label_style), Span::raw(" ")]
    }
}

fn wrap_line(line: &str, max_width: usize) -> Vec<String> {
    if line.is_empty() {
        return vec![String::new()];
    }

    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;

    for ch in line.chars() {
        let char_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if current_width > 0 && current_width.saturating_add(char_width) > max_width {
            chunks.push(std::mem::take(&mut current));
            current_width = 0;
        }
        current.push(ch);
        current_width = current_width.saturating_add(char_width);
    }

    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

fn normalized_lines(content: &str) -> impl Iterator<Item = &str> {
    let content = if content.is_empty() { " " } else { content };
    content.split('\n')
}

#[cfg(test)]
mod tests {
    use super::super::super::state::{FileDiffDisplay, ToolCallDisplay};
    use super::*;

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn message_lines_marks_conversation_with_left_stripe() {
        let msg = ChatMessage::new(lattice_core::types::Role::User, "hello".into());

        let lines = message_lines(&msg, &Theme::default(), false, 40);
        let text: Vec<String> = lines.iter().map(line_text).collect();

        assert!(text[0].starts_with("▌ hello"));
        assert!(!text.iter().any(|line| line.contains('●')));
        assert!(!text.iter().any(|line| line.starts_with('╭')));
        assert!(!text.iter().any(|line| line.starts_with('╰')));
    }

    #[test]
    fn message_lines_trim_trailing_card_blank_rows() {
        let msg = ChatMessage::new(lattice_core::types::Role::Assistant, "answer\n\n".into());

        let lines = message_lines(&msg, &Theme::default(), false, 40);
        let text: Vec<String> = lines.iter().map(line_text).collect();

        assert!(text[0].starts_with("▌ answer"));
        assert_eq!(text.last().map(String::as_str), Some(""));
        assert!(!text[..text.len().saturating_sub(1)]
            .iter()
            .any(|line| line.trim_end() == "▌"));
    }

    #[test]
    fn message_lines_marks_tool_execution_with_left_stripe() {
        let msg = ChatMessage::tool_call(
            "call_1".into(),
            "read_file".into(),
            serde_json::json!({ "path": "src/lib.rs" }).to_string(),
        );

        let lines = message_lines(&msg, &Theme::default(), false, 48);
        let text: Vec<String> = lines.iter().map(line_text).collect();

        assert!(text[0].starts_with("▌ Read(src/lib.rs)"));
        assert!(text
            .iter()
            .any(|line| line.contains("▌ Read(src/lib.rs) · running")));
        assert!(!text.iter().any(|line| line.contains('●')));
        assert!(!text.iter().any(|line| line.contains('⎿')));
    }

    #[test]
    fn message_lines_hide_collapsed_reasoning_hint() {
        let mut msg = ChatMessage::new(lattice_core::types::Role::Assistant, "answer".into());
        msg.reasoning = Some("private thinking".into());
        msg.reasoning_collapsed = true;

        let lines = message_lines(&msg, &Theme::default(), false, 80);
        let text = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");

        assert!(!text.contains("thought for"));
        assert!(!text.contains("Ctrl+O or /trace"));
        assert!(!text.contains("private thinking"));
    }

    #[test]
    fn message_lines_hide_expanded_reasoning_inline() {
        let mut msg = ChatMessage::new(lattice_core::types::Role::Assistant, "answer".into());
        msg.reasoning = Some("private thinking".into());
        msg.reasoning_collapsed = false;

        let lines = message_lines(&msg, &Theme::default(), true, 80);
        let text = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");

        assert!(text.contains("answer"));
        assert!(!text.contains("private thinking"));
        assert!(!text.contains("thinking expanded"));
    }

    #[test]
    fn message_lines_hide_empty_assistant_thinking_placeholder() {
        let mut msg = ChatMessage::new(lattice_core::types::Role::Assistant, String::new());
        msg.reasoning = Some("private thinking".into());

        let lines = message_lines(&msg, &Theme::default(), true, 80);

        assert!(lines.is_empty());
    }

    #[test]
    fn tool_header_uses_cc_shape_for_bash() {
        let msg = ChatMessage::tool_call(
            "call_1".into(),
            "bash".into(),
            serde_json::json!({ "command": "cargo test --all" }).to_string(),
        );

        let lines = tool_lines(&msg, &Theme::default(), 80);

        assert_eq!(line_text(&lines[0]), "Bash(cargo test --all) · running");
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn tool_header_uses_friendly_name_and_key_arg() {
        let msg = ChatMessage::tool_call(
            "call_1".into(),
            "read_file".into(),
            serde_json::json!({ "path": "lattice-cli/src/main.rs" }).to_string(),
        );

        let lines = tool_lines(&msg, &Theme::default(), 80);

        assert_eq!(
            line_text(&lines[0]),
            "Read(lattice-cli/src/main.rs) · running"
        );
    }

    #[test]
    fn patch_header_summarizes_structured_patch_paths() {
        let patch = "*** Begin Patch\n\
*** Add File: src/new.rs\n\
+hello\n\
*** Update File: src/lib.rs\n\
*** Old Hash: blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n\
@@\n\
 old\n\
-value\n\
+new value\n\
*** Delete File: src/old.rs\n\
*** Old Hash: blake3:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n\
*** End Patch";
        let msg = ChatMessage::tool_call(
            "call_1".into(),
            "patch".into(),
            serde_json::json!({ "patch": patch }).to_string(),
        );

        let lines = tool_lines(&msg, &Theme::default(), 80);

        assert_eq!(line_text(&lines[0]), "Edit(src/new.rs +2 more) · running");
    }

    #[test]
    fn patch_header_falls_back_for_headerless_structured_patch() {
        let msg = ChatMessage::tool_call(
            "call_1".into(),
            "patch".into(),
            serde_json::json!({ "patch": "*** Begin Patch\n*** End Patch" }).to_string(),
        );

        let lines = tool_lines(&msg, &Theme::default(), 80);

        assert_eq!(line_text(&lines[0]), "Edit(structured patch) · running");
    }

    #[test]
    fn collapsed_tool_result_keeps_one_line_without_expand_hint() {
        let mut msg = ChatMessage::tool_call(
            "call_1".into(),
            "grep".into(),
            serde_json::json!({ "pattern": "Runtime", "path": "lattice-cli/src" }).to_string(),
        );
        msg.collapsed = true;
        let tool = msg.tool.as_mut().unwrap();
        tool.status = ToolStatus::Done;
        tool.finished_at = Some(tool.started_at + std::time::Duration::from_millis(25));
        tool.result = Some(["one", "two", "three", "four", "five"].join("\n"));

        let lines = tool_lines(&msg, &Theme::default(), 80);

        let text = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert_eq!(
            line_text(&lines[0]),
            "Search(Runtime in lattice-cli/src) · done in 25ms"
        );
        assert_eq!(lines.len(), 1);
        assert!(!text.contains('⎿'));
        assert!(!text.contains("Ctrl+E"));
        assert!(!text.contains("three"));
    }

    #[test]
    fn tool_result_renders_file_diff_block() {
        let mut msg = ChatMessage::tool_call(
            "call_1".into(),
            "write_file".into(),
            serde_json::json!({ "path": "notes.txt", "content": "new\nsame\n" }).to_string(),
        );
        msg.collapsed = false;
        let tool = msg.tool.as_mut().unwrap();
        tool.status = ToolStatus::Done;
        tool.finished_at = Some(tool.started_at + std::time::Duration::from_millis(25));
        tool.result = Some("Wrote 9 bytes to notes.txt".into());
        tool.file_diff = Some(FileDiffDisplay {
            path: "notes.txt".into(),
            text: "--- a/notes.txt\n+++ b/notes.txt\n@@ -1,2 +1,2 @@\n-old\n+new\n same".into(),
        });

        let lines = tool_lines(&msg, &Theme::default(), 80);
        let text: Vec<String> = lines.iter().map(line_text).collect();

        assert!(text.contains(&"Write(notes.txt) · done in 25ms".to_string()));
        assert!(text.contains(&"  Wrote 9 bytes to notes.txt".to_string()));
        assert!(text.contains(&"  Diff notes.txt".to_string()));
        assert!(text.iter().any(|line| line.starts_with("  ╌")));
        assert!(text.contains(&"  -old".to_string()));
        assert!(text.contains(&"  +new".to_string()));
        assert!(!text.iter().any(|line| line.contains('⎿')));
    }

    #[test]
    fn grouped_tool_card_expands_all_calls() {
        let mut msg = ChatMessage::tool_call(
            "call_1".into(),
            "read_file".into(),
            serde_json::json!({ "path": "Cargo.toml" }).to_string(),
        );
        let tool = msg.tool.as_mut().unwrap();
        tool.status = ToolStatus::Done;
        tool.finished_at = Some(tool.started_at + std::time::Duration::from_millis(10));
        tool.result = Some("workspace".into());
        tool.grouped_calls.push(ToolCallDisplay {
            call_id: "call_2".into(),
            arguments: serde_json::json!({ "path": "Cargo.lock" }).to_string(),
            result: Some("lockfile".into()),
            file_diff: None,
            file_before: None,
            status: ToolStatus::Done,
            started_at: tool.started_at,
            finished_at: Some(tool.started_at + std::time::Duration::from_millis(12)),
        });

        let collapsed = tool_lines(&msg, &Theme::default(), 80)
            .iter()
            .map(line_text)
            .collect::<Vec<_>>();
        assert_eq!(collapsed, vec!["Read(Cargo.toml) +1 calls · done"]);

        msg.collapsed = false;
        let expanded = tool_lines(&msg, &Theme::default(), 80)
            .iter()
            .map(line_text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(expanded.contains("Read(Cargo.toml) · done in 10ms"));
        assert!(expanded.contains("Read(Cargo.lock) · done in 12ms"));
        assert!(expanded.contains("workspace"));
        assert!(expanded.contains("lockfile"));
    }
}

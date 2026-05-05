use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::super::app::ChatMessage;
use super::super::markdown::render_markdown;
use super::super::theme::Theme;

pub fn message_lines(
    msg: &ChatMessage,
    theme: &Theme,
    transcript_mode: bool,
    width: u16,
) -> Vec<Line<'static>> {
    let (label, label_style, _body_style) = role_styles(msg, theme);
    let mut rows: Vec<Line<'static>> = Vec::new();

    let is_assistant = msg.role == lattice_core::types::Role::Assistant && !msg.content.is_empty();
    let is_tool = msg.role == lattice_core::types::Role::Tool;
    let use_md = is_assistant || (is_tool && !msg.content.is_empty());

    if is_tool && msg.collapsed {
        let usable_width = width.max(1) as usize;
        let prefix_width = UnicodeWidthStr::width(label) + 1;
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
        let prefix_width = UnicodeWidthStr::width(label) + 1; // label + space
        let md_width = width.saturating_sub(prefix_width as u16).max(20);

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
                let mut spans = vec![Span::styled(label, label_style), Span::raw(" ")];
                spans.extend(md_spans);
                rows.push(Line::from(spans));
            } else {
                let mut spans = vec![Span::raw(indent.clone())];
                spans.extend(md_spans);
                rows.push(Line::from(spans));
            }
        }
    } else {
        let usable_width = width.max(1) as usize;
        rows.extend(labelled_lines(
            label,
            label_style,
            _body_style,
            display_content(msg),
            usable_width,
        ));
    }

    if transcript_mode {
        if let Some(reasoning) = msg.reasoning.as_deref() {
            let usable_width = width.max(1) as usize;
            rows.extend(labelled_lines(
                "Trace",
                theme.thinking_style().add_modifier(Modifier::BOLD),
                theme.thinking_style(),
                reasoning,
                usable_width,
            ));
        }
    } else if msg.reasoning.as_deref().is_some_and(|r| !r.is_empty()) {
        // Show a collapsed indicator when reasoning is available but hidden
        let hint = format!(
            "{} lines of thinking hidden · Ctrl+O to toggle",
            msg.reasoning.as_deref().unwrap().lines().count()
        );
        rows.push(Line::from(vec![
            Span::styled("  ", Style::default()),
            Span::styled(hint, theme.thinking_style()),
        ]));
    }

    rows.push(Line::raw(""));

    // User messages get full-width background highlight
    if msg.role == lattice_core::types::Role::User {
        let user_bg = Style::default().bg(theme.surface);
        rows = rows
            .into_iter()
            .map(|line| {
                let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
                let current_w = UnicodeWidthStr::width(text.as_str());
                let pad = (width as usize).saturating_sub(current_w);
                let mut spans = line.spans;
                if pad > 0 {
                    spans.push(Span::styled(" ".repeat(pad), user_bg));
                }
                Line::from(spans)
            })
            .collect();
    }

    rows
}

fn role_styles(msg: &ChatMessage, theme: &Theme) -> (&'static str, Style, Style) {
    match msg.role {
        lattice_core::types::Role::User => (
            "●",
            theme.user_style().add_modifier(Modifier::BOLD),
            Style::default().fg(theme.text),
        ),
        lattice_core::types::Role::Assistant => (
            "●",
            theme.assistant_style().add_modifier(Modifier::BOLD),
            theme.assistant_style(),
        ),
        lattice_core::types::Role::System => (
            "●",
            Style::default()
                .fg(theme.subtext)
                .add_modifier(Modifier::BOLD),
            Style::default().fg(theme.subtext),
        ),
        lattice_core::types::Role::Tool => (
            "●",
            theme.tool_style().add_modifier(Modifier::BOLD),
            theme.tool_style(),
        ),
    }
}

fn display_content(msg: &ChatMessage) -> &str {
    if msg.role == lattice_core::types::Role::Assistant && msg.content.is_empty() {
        "Thinking..."
    } else {
        &msg.content
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
                lines.push(Line::from(vec![
                    Span::styled(label, label_style),
                    Span::raw(" "),
                    Span::styled(chunk, body_style),
                ]));
            } else {
                lines.push(Line::from(vec![
                    Span::raw(indent.clone()),
                    Span::styled(chunk, body_style),
                ]));
            }
        }
    }

    if lines.is_empty() {
        lines.push(Line::from(vec![
            Span::styled(label, label_style),
            Span::raw(" "),
        ]));
    }

    lines
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

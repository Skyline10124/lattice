use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::super::app::ChatMessage;
use super::super::theme::Theme;

pub fn message_lines(
    msg: &ChatMessage,
    theme: &Theme,
    transcript_mode: bool,
    width: u16,
) -> Vec<Line<'static>> {
    let usable_width = width.max(1) as usize;
    let (label, label_style, body_style) = role_styles(msg, theme);
    let mut rows = labelled_lines(
        label,
        label_style,
        body_style,
        display_content(msg),
        usable_width,
    );

    if transcript_mode {
        if let Some(reasoning) = msg.reasoning.as_deref() {
            rows.extend(labelled_lines(
                "Trace",
                theme.thinking_style().add_modifier(Modifier::BOLD),
                theme.thinking_style(),
                reasoning,
                usable_width,
            ));
        }
    }

    rows.push(Line::raw(""));
    rows
}

fn role_styles(msg: &ChatMessage, theme: &Theme) -> (&'static str, Style, Style) {
    match msg.role {
        lattice_core::types::Role::User => (
            "You",
            theme.user_style().add_modifier(Modifier::BOLD),
            Style::default().fg(theme.text),
        ),
        lattice_core::types::Role::Assistant => (
            "LATTICE",
            theme.assistant_style().add_modifier(Modifier::BOLD),
            theme.assistant_style(),
        ),
        lattice_core::types::Role::System => (
            "Note",
            Style::default()
                .fg(theme.highlight)
                .add_modifier(Modifier::BOLD),
            Style::default().fg(theme.subtext),
        ),
        lattice_core::types::Role::Tool => (
            "Tool",
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

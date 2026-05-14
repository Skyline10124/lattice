use ratatui::{
    style::{Modifier, Style},
    text::Line,
};

use super::super::theme::Theme;
use super::layout::truncate_str;

pub(super) fn render_math_block(text: &str, theme: &Theme, max_width: usize) -> Vec<Line<'static>> {
    let style = Style::default()
        .fg(theme.thinking)
        .add_modifier(Modifier::ITALIC);
    let border = theme.code_block_border_style();

    let mut lines = Vec::new();
    lines.push(Line::styled("┌─ math ─".to_string(), border));

    for line in text.lines() {
        let visible = truncate_str(line, max_width.saturating_sub(4));
        let padded = format!("│ {} ", visible);
        lines.push(Line::styled(padded, style));
    }

    lines.push(Line::styled("└────────".to_string(), border));
    lines
}

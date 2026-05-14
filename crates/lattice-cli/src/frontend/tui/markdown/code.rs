use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};

use super::super::theme::Theme;
use super::layout::{truncate_line_to_width, truncate_str};

pub(super) fn render_code_block(
    lang: &Option<String>,
    text: &str,
    theme: &Theme,
    max_width: usize,
) -> Vec<Line<'static>> {
    let is_diff = lang.as_deref() == Some("diff");
    let bg = theme.code_block_style();
    let border = theme.code_block_border_style();
    let code_bg = bg.bg.unwrap_or(theme.bg);

    // Detect pre-formatted Unicode art tables — skip our borders to avoid double-frame
    let is_art_table = text.contains('╭') || text.contains('╔') || text.contains('┏');

    let mut lines = Vec::new();

    if !is_art_table {
        let top = if let Some(l) = lang {
            format!("┌─ {} ", l)
        } else {
            "┌────".to_string()
        };
        lines.push(Line::styled(top, border));
    }

    // Try syntax highlighting for non-diff blocks with a known language
    let highlighted = if !is_diff && !is_art_table {
        highlight_syntax(lang.as_deref().unwrap_or(""), text, theme)
    } else {
        None
    };

    let prefix = if is_art_table { "" } else { "│ " };
    let border_prefix_width = if is_art_table { 0 } else { 2 };

    if let Some(hl_lines) = highlighted {
        for hl_line in hl_lines {
            let visible =
                truncate_line_to_width(&hl_line, max_width.saturating_sub(border_prefix_width));
            let mut parts = if is_art_table {
                Vec::new()
            } else {
                vec![Span::styled(prefix.to_string(), border)]
            };
            for (text_seg, seg_style) in visible {
                parts.push(Span::styled(text_seg, seg_style));
            }
            lines.push(Line::from(parts));
        }
    } else {
        let add_style = Style::default().fg(theme.success).bg(code_bg);
        let del_style = Style::default().fg(theme.error).bg(code_bg);
        let hunk_style = Style::default().fg(theme.highlight).bg(code_bg);
        let normal_style = Style::default().fg(theme.text).bg(code_bg);

        for (li, code_line) in text.lines().enumerate() {
            let display = if code_line.is_empty() { " " } else { code_line };
            let visible = truncate_str(display, max_width.saturating_sub(border_prefix_width));
            let style = if is_diff {
                if display.starts_with('+') && !display.starts_with("+++") {
                    add_style
                } else if display.starts_with('-') && !display.starts_with("---") {
                    del_style
                } else if display.starts_with("@@") {
                    hunk_style
                } else {
                    normal_style
                }
            } else {
                Style::default().fg(theme.text).bg(code_bg)
            };
            let content = if is_diff {
                let num = format!("{:>3} ", li + 1);
                format!("│{}{}", num, visible)
            } else if is_art_table {
                visible
            } else {
                format!("│ {}", visible)
            };
            lines.push(Line::styled(content, style));
        }
    }

    if !is_art_table {
        lines.push(Line::styled("└────".to_string(), border));
    }
    lines
}

/// Syntax-highlight code using syntect. Returns per-line styled segments.
fn highlight_syntax(lang: &str, text: &str, theme: &Theme) -> Option<Vec<Vec<(String, Style)>>> {
    use std::sync::OnceLock;
    use syntect::highlighting::ThemeSet;
    use syntect::parsing::SyntaxSet;

    static SYNTAX_SET: OnceLock<SyntaxSet> = OnceLock::new();
    static THEME_SET: OnceLock<ThemeSet> = OnceLock::new();

    let ss = SYNTAX_SET.get_or_init(SyntaxSet::load_defaults_newlines);
    let ts = THEME_SET.get_or_init(ThemeSet::load_defaults);

    let syntax = ss.find_syntax_by_token(lang)?;
    let syn_theme = &ts.themes["base16-ocean.dark"];

    let mut highlighter = syntect::easy::HighlightLines::new(syntax, syn_theme);
    let ranges = highlighter.highlight_line(text, ss).ok()?;

    // syntect returns flat ranges; split by newline
    let mut result: Vec<Vec<(String, Style)>> = Vec::new();
    let mut current_line: Vec<(String, Style)> = Vec::new();

    for (seg_style, seg_text) in &ranges {
        let style = syntect_style_to_ratatui(*seg_style, theme);
        for (idx, part) in seg_text.split('\n').enumerate() {
            if idx > 0 {
                result.push(std::mem::take(&mut current_line));
            }
            if !part.is_empty() {
                current_line.push((part.to_string(), style));
            }
        }
    }
    if !current_line.is_empty() || result.is_empty() {
        result.push(current_line);
    }

    Some(result)
}

fn syntect_style_to_ratatui(syn_style: syntect::highlighting::Style, theme: &Theme) -> Style {
    use ratatui::style::Color;
    use syntect::highlighting::FontStyle;

    let fg = Color::Rgb(
        syn_style.foreground.r,
        syn_style.foreground.g,
        syn_style.foreground.b,
    );
    let mut style = Style::default().fg(fg);

    if syn_style.font_style.contains(FontStyle::BOLD) {
        style = style.add_modifier(Modifier::BOLD);
    }
    if syn_style.font_style.contains(FontStyle::ITALIC) {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if syn_style.font_style.contains(FontStyle::UNDERLINE) {
        style = style.add_modifier(Modifier::UNDERLINED);
    }

    // Don't set bg on individual spans — the code block Line carries the bg

    // If the syntect fg is very dark, use theme text color instead
    let lum = (syn_style.foreground.r as u32 * 299
        + syn_style.foreground.g as u32 * 587
        + syn_style.foreground.b as u32 * 114)
        / 1000;
    if lum < 30 {
        style = style.fg(theme.text);
    }

    style
}

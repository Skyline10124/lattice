use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};
use unicode_width::UnicodeWidthStr;

use super::super::theme::Theme;
use super::ast::MdSpan;

pub(super) const MIN_BLOCK_WIDTH: usize = 20;
const MIN_BODY_WIDTH: usize = 10;

pub(super) fn render_plain_text(content: &str, theme: &Theme, width: u16) -> Vec<Line<'static>> {
    let usable = (width as usize).max(MIN_BLOCK_WIDTH);
    let mut lines = Vec::new();
    for line in content.lines() {
        if line.is_empty() {
            lines.push(Line::raw(""));
            continue;
        }
        for chunk in wrap_line_text(line, usable) {
            lines.push(Line::styled(chunk, Style::default().fg(theme.text)));
        }
    }
    if lines.is_empty() {
        lines.push(Line::raw(""));
    }
    lines
}

fn wrap_line_text(line: &str, max_width: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;
    for word in line.split_whitespace() {
        let word_w = UnicodeWidthStr::width(word);
        if current_width > 0 && current_width + 1 + word_w > max_width {
            chunks.push(std::mem::take(&mut current));
            current_width = 0;
        }
        if !current.is_empty() {
            current.push(' ');
            current_width += 1;
        }
        current.push_str(word);
        current_width += word_w;
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    if chunks.is_empty() {
        chunks.push(String::new());
    }
    chunks
}

/// Word-wrap spans into lines, splitting on newlines and wrapping at width.
pub(super) fn wrap_spans(
    spans: &[MdSpan],
    base_style: Style,
    theme: &Theme,
    max_width: usize,
) -> Vec<Line<'static>> {
    wrap_spans_with_prefix(spans, base_style, "", theme, max_width)
}

pub(super) fn wrap_spans_with_prefix(
    spans: &[MdSpan],
    base_style: Style,
    prefix: &str,
    theme: &Theme,
    max_width: usize,
) -> Vec<Line<'static>> {
    let prefix_width = UnicodeWidthStr::width(prefix);
    wrap_spans_with_custom_prefix(spans, prefix, prefix_width, base_style, theme, max_width)
}

pub(super) fn wrap_spans_with_custom_prefix(
    spans: &[MdSpan],
    prefix: &str,
    prefix_width: usize,
    prefix_style: Style,
    theme: &Theme,
    max_width: usize,
) -> Vec<Line<'static>> {
    let chunks = span_chunks(spans, Style::default(), theme);
    let body_width = max_width.saturating_sub(prefix_width).max(MIN_BODY_WIDTH);

    // Split chunks on hard newlines first
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut current_line_spans: Vec<Span<'static>> = Vec::new();
    let mut current_width = 0usize;
    let mut first_line = true;

    for (text, style, _chunk_width) in &chunks {
        // Handle hard breaks embedded in text
        if text.contains('\n') {
            for (idx, part) in text.split('\n').enumerate() {
                if idx > 0 {
                    // flush line
                    lines.push(build_line(
                        prefix,
                        prefix_width,
                        prefix_style,
                        &current_line_spans,
                        first_line,
                    ));
                    current_line_spans.clear();
                    current_width = prefix_width;
                    first_line = false;
                }
                if !part.is_empty() {
                    let pw = UnicodeWidthStr::width(part);
                    pack_word(
                        part,
                        *style,
                        pw,
                        &mut current_line_spans,
                        &mut current_width,
                        body_width,
                        &mut lines,
                        prefix,
                        prefix_width,
                        prefix_style,
                        &mut first_line,
                    );
                }
            }
        } else {
            let pw = UnicodeWidthStr::width(text as &str);
            pack_word(
                text,
                *style,
                pw,
                &mut current_line_spans,
                &mut current_width,
                body_width,
                &mut lines,
                prefix,
                prefix_width,
                prefix_style,
                &mut first_line,
            );
        }
    }

    // flush final line
    if !current_line_spans.is_empty() {
        lines.push(build_line(
            prefix,
            prefix_width,
            prefix_style,
            &current_line_spans,
            first_line,
        ));
    }

    if lines.is_empty() {
        lines.push(Line::raw(""));
    }

    lines
}

/// Like truncate_str but preserves styled segments.
pub(super) fn truncate_line_to_width(
    segments: &[(String, Style)],
    max_width: usize,
) -> Vec<(String, Style)> {
    let mut result = Vec::new();
    let mut w = 0usize;
    for (text, style) in segments {
        if w >= max_width {
            break;
        }
        let mut visible = String::new();
        for ch in text.chars() {
            let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            if w + cw > max_width {
                break;
            }
            visible.push(ch);
            w += cw;
        }
        if !visible.is_empty() {
            result.push((visible, *style));
        }
    }
    result
}

pub(super) fn truncate_str(s: &str, max_width: usize) -> String {
    let mut result = String::new();
    let mut w = 0usize;
    for ch in s.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if w + cw > max_width {
            break;
        }
        result.push(ch);
        w += cw;
    }
    if w < UnicodeWidthStr::width(s) {
        result.push('…');
    }
    result
}

/// Build a flat list of (text, style, width) chunks from MdSpans.
fn span_chunks(spans: &[MdSpan], base_style: Style, theme: &Theme) -> Vec<(String, Style, usize)> {
    let mut chunks = Vec::new();
    for span in spans {
        let mut style = base_style;
        if span.bold {
            style = style.patch(theme.bold_style());
        }
        if span.italic {
            style = style.patch(theme.italic_style());
        }
        if span.strikethrough {
            style = style.add_modifier(Modifier::CROSSED_OUT);
        }
        if span.code {
            style = theme.inline_code_style();
        }
        if span.link_url.is_some() {
            style = style.patch(theme.link_style());
        }

        let text = &span.text;
        let w = UnicodeWidthStr::width(text as &str);
        chunks.push((text.clone(), style, w));
    }
    chunks
}

#[allow(clippy::too_many_arguments)]
fn pack_word(
    text: &str,
    style: Style,
    word_width: usize,
    current_spans: &mut Vec<Span<'static>>,
    current_width: &mut usize,
    max_width: usize,
    lines: &mut Vec<Line<'static>>,
    prefix: &str,
    prefix_width: usize,
    prefix_style: Style,
    first_line: &mut bool,
) {
    let is_space = text.trim().is_empty() && text != "\n";

    if *current_width == 0 && is_space {
        return; // skip leading space
    }

    if *current_width > 0 && *current_width + word_width > max_width && !is_space {
        // wrap
        lines.push(build_line(
            prefix,
            prefix_width,
            prefix_style,
            current_spans,
            *first_line,
        ));
        current_spans.clear();
        *current_width = 0;
        *first_line = false;
    }

    // now add the word as a span
    current_spans.push(Span::styled(text.to_string(), style));
    // Only add to width if we actually added the span
    if *current_width > 0 || !is_space {
        *current_width += word_width;
    } else {
        // first word on line
        *current_width = word_width;
    }
}

fn build_line(
    prefix: &str,
    prefix_width: usize,
    prefix_style: Style,
    spans: &[Span<'static>],
    show_prefix: bool,
) -> Line<'static> {
    if show_prefix && !prefix.is_empty() {
        let mut all = vec![Span::styled(prefix.to_string(), prefix_style)];
        all.extend_from_slice(spans);
        Line::from(all)
    } else if !show_prefix && !prefix.is_empty() {
        let indent = " ".repeat(prefix_width);
        let mut all = vec![Span::raw(indent)];
        all.extend_from_slice(spans);
        Line::from(all)
    } else {
        Line::from(spans.to_vec())
    }
}

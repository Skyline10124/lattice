use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};
use unicode_width::UnicodeWidthStr;

use super::super::theme::Theme;
use super::ast::MdSpan;

const TABLE_SAFETY_MARGIN: usize = 4;
const TABLE_MIN_COL_WIDTH: usize = 3;
const TABLE_MAX_ROW_LINES: usize = 4;

pub(super) fn render_table(
    headers: &[Vec<MdSpan>],
    rows: &[Vec<Vec<MdSpan>>],
    theme: &Theme,
    max_width: usize,
) -> Vec<Line<'static>> {
    let col_count = headers
        .len()
        .max(rows.iter().map(|r| r.len()).max().unwrap_or(0));
    if col_count == 0 {
        return vec![Line::raw("")];
    }

    let htexts: Vec<String> = pad_cell_texts(headers, col_count);
    let rtexts: Vec<Vec<String>> = rows
        .iter()
        .map(|row| pad_cell_texts(row, col_count))
        .collect();

    // Three-tier column width calculation
    let (col_widths, needs_hard_wrap) =
        compute_col_widths_3tier(&htexts, &rtexts, col_count, max_width);
    if col_widths.is_empty() {
        return vec![Line::raw("")];
    }

    let max_row_lines = calc_max_row_lines(&htexts, &rtexts, &col_widths, needs_hard_wrap);
    let use_vertical = max_row_lines > TABLE_MAX_ROW_LINES;

    if use_vertical {
        return render_vertical_table(&htexts, &rtexts, theme, max_width);
    }

    let header_style = Style::default().add_modifier(Modifier::BOLD).fg(theme.text);
    let cell_style = Style::default().fg(theme.text);
    let tbl_border = Style::default().fg(theme.subtext);
    let mut lines = Vec::new();

    lines.push(render_hborder(&col_widths, "┌", "┬", "┐", tbl_border));
    lines.extend(render_ml_row(
        &htexts,
        &col_widths,
        header_style,
        tbl_border,
        needs_hard_wrap,
    ));
    lines.push(render_hborder(&col_widths, "├", "┼", "┤", tbl_border));
    for row in &rtexts {
        lines.extend(render_ml_row(
            row,
            &col_widths,
            cell_style,
            tbl_border,
            needs_hard_wrap,
        ));
    }
    lines.push(render_hborder(&col_widths, "└", "┴", "┘", tbl_border));

    // Safety check: if any line exceeds max_width, fallback to vertical
    let max_line_w = lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
                .sum::<usize>()
        })
        .max()
        .unwrap_or(0);
    if max_line_w > max_width.saturating_sub(TABLE_SAFETY_MARGIN) {
        return render_vertical_table(&htexts, &rtexts, theme, max_width);
    }

    lines
}

fn pad_cell_texts(row: &[Vec<MdSpan>], col_count: usize) -> Vec<String> {
    (0..col_count)
        .map(|i| {
            row.get(i)
                .map(|spans| spans.iter().map(|s| s.text.as_str()).collect::<String>())
                .unwrap_or_default()
        })
        .collect()
}

/// Three-tier column width: min (longest word), ideal (full content), proportional shrink.
fn compute_col_widths_3tier(
    headers: &[String],
    rows: &[Vec<String>],
    col_count: usize,
    max_width: usize,
) -> (Vec<usize>, bool) {
    // border overhead: │ text │ ... │ = 1 + (width + 2 padding + 1 border) per col
    let overhead = 1 + col_count * 3;
    let available = max_width
        .saturating_sub(overhead)
        .saturating_sub(TABLE_SAFETY_MARGIN);

    // Longest word per column (minimum to avoid breaking words)
    let min_widths: Vec<usize> = (0..col_count)
        .map(|ci| {
            let mut mw = TABLE_MIN_COL_WIDTH;
            for col_texts in std::iter::once(headers).chain(rows.iter().map(|r| r.as_slice())) {
                if let Some(t) = col_texts.get(ci) {
                    mw = mw.max(
                        t.split_whitespace()
                            .map(UnicodeWidthStr::width)
                            .max()
                            .unwrap_or(0)
                            .max(TABLE_MIN_COL_WIDTH),
                    );
                }
            }
            mw
        })
        .collect();

    // Ideal width per column (full content, no wrapping)
    let ideal_widths: Vec<usize> = (0..col_count)
        .map(|ci| {
            let mut iw = TABLE_MIN_COL_WIDTH;
            for col_texts in std::iter::once(headers).chain(rows.iter().map(|r| r.as_slice())) {
                if let Some(t) = col_texts.get(ci) {
                    iw = iw.max(UnicodeWidthStr::width(t.as_str()));
                }
            }
            iw
        })
        .collect();

    let total_min: usize = min_widths.iter().sum();
    let total_ideal: usize = ideal_widths.iter().sum();

    if total_ideal <= available {
        // Everything fits — use ideal widths
        return (ideal_widths, false);
    }

    if total_min <= available {
        // Shrink proportionally by overflow
        let extra = available - total_min;
        let overflows: Vec<usize> = (0..col_count)
            .map(|i| ideal_widths[i].saturating_sub(min_widths[i]))
            .collect();
        let total_overflow: usize = overflows.iter().sum();
        let widths: Vec<usize> = (0..col_count)
            .map(|i| {
                min_widths[i]
                    + (overflows[i] * extra)
                        .checked_div(total_overflow)
                        .unwrap_or(0)
            })
            .collect();
        return (widths, false);
    }

    // Table wider than terminal even at minimum — hard wrap (break words)
    let scale = available as f64 / total_min as f64;
    let widths: Vec<usize> = min_widths
        .iter()
        .map(|w| ((*w as f64 * scale) as usize).max(TABLE_MIN_COL_WIDTH))
        .collect();
    (widths, true)
}

fn calc_max_row_lines(
    headers: &[String],
    rows: &[Vec<String>],
    col_widths: &[usize],
    hard_wrap: bool,
) -> usize {
    let mut max_lines = 1usize;
    for col_texts in std::iter::once(headers).chain(rows.iter().map(|r| r.as_slice())) {
        for (ci, text) in col_texts.iter().enumerate() {
            let w = col_widths.get(ci).copied().unwrap_or(TABLE_MIN_COL_WIDTH);
            let wrapped = wrap_cell_text(text, w, hard_wrap);
            max_lines = max_lines.max(wrapped.len());
        }
    }
    max_lines
}

fn wrap_cell_text(text: &str, width: usize, hard: bool) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    let mut result = Vec::new();
    for line in text.lines() {
        if line.is_empty() {
            if result.is_empty() {
                result.push(String::new());
            }
            continue;
        }
        if hard {
            // Break at character level
            let mut pos = 0;
            let mut line_w = 0usize;
            for (i, ch) in line.char_indices() {
                let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
                if line_w + cw > width && line_w > 0 {
                    result.push(line[pos..i].to_string());
                    pos = i;
                    line_w = 0;
                }
                line_w += cw;
            }
            if pos < line.len() {
                result.push(line[pos..].to_string());
            }
        } else {
            // Word-wrap
            let mut current = String::new();
            let mut current_w = 0usize;
            for word in line.split_whitespace() {
                let word_w = UnicodeWidthStr::width(word);
                if current_w > 0 && current_w + 1 + word_w > width {
                    result.push(std::mem::take(&mut current));
                    current_w = 0;
                }
                if !current.is_empty() {
                    current.push(' ');
                    current_w += 1;
                }
                current.push_str(word);
                current_w += word_w;
            }
            if !current.is_empty() {
                result.push(current);
            }
            if result.is_empty() {
                result.push(String::new());
            }
        }
    }
    if result.is_empty() {
        result.push(String::new());
    }
    result
}

/// Render a single table row with multi-line cells, vertically centered.
fn render_ml_row(
    cells: &[String],
    widths: &[usize],
    cell_style: Style,
    border_style: Style,
    hard_wrap: bool,
) -> Vec<Line<'static>> {
    let cell_lines: Vec<Vec<String>> = cells
        .iter()
        .enumerate()
        .map(|(i, text)| wrap_cell_text(text, widths.get(i).copied().unwrap_or(3), hard_wrap))
        .collect();

    let max_lines = cell_lines.iter().map(|cl| cl.len()).max().unwrap_or(1);
    let offsets: Vec<usize> = cell_lines
        .iter()
        .map(|cl| (max_lines - cl.len()) / 2)
        .collect();

    let mut lines = Vec::new();
    for line_idx in 0..max_lines {
        let mut parts = vec![Span::styled("│", border_style), Span::raw(" ")];
        for (ci, cl) in cell_lines.iter().enumerate() {
            if ci > 0 {
                parts.push(Span::raw(" "));
                parts.push(Span::styled("│", border_style));
                parts.push(Span::raw(" "));
            }
            let offset = offsets[ci];
            let content_idx = if line_idx >= offset {
                line_idx - offset
            } else {
                usize::MAX // will result in empty string
            };
            let text = cl.get(content_idx).map(|s| s.as_str()).unwrap_or("");
            let col_w = widths.get(ci).copied().unwrap_or(3);
            let pad = " ".repeat(col_w.saturating_sub(UnicodeWidthStr::width(text)));
            parts.push(Span::styled(text.to_string(), cell_style));
            parts.push(Span::raw(pad));
        }
        parts.push(Span::raw(" "));
        parts.push(Span::styled("│", border_style));
        lines.push(Line::from(parts));
    }
    lines
}

fn render_hborder(
    widths: &[usize],
    left: &str,
    mid: &str,
    right: &str,
    style: Style,
) -> Line<'static> {
    let mut parts = vec![Span::styled(left.to_string(), style)];
    for (i, w) in widths.iter().enumerate() {
        if i > 0 {
            parts.push(Span::styled(mid.to_string(), style));
        }
        parts.push(Span::styled("─".repeat(w + 2), style));
    }
    parts.push(Span::styled(right.to_string(), style));
    Line::from(parts)
}

/// Vertical key-value format for narrow terminals or overflow tables.
fn render_vertical_table(
    headers: &[String],
    rows: &[Vec<String>],
    theme: &Theme,
    max_width: usize,
) -> Vec<Line<'static>> {
    let header_style = Style::default().add_modifier(Modifier::BOLD).fg(theme.text);
    let cell_style = Style::default().fg(theme.text);
    let separator = "─".repeat(max_width.min(40));
    let indent = "  ";
    let mut lines: Vec<Line<'static>> = Vec::new();

    for (ri, row) in rows.iter().enumerate() {
        if ri > 0 {
            lines.push(Line::styled(
                separator.clone(),
                Style::default().fg(theme.border),
            ));
        }
        for (ci, cell_text) in row.iter().enumerate() {
            let label = headers.get(ci).map(|s| s.as_str()).unwrap_or("?");
            let value = cell_text.trim();
            let first_w = max_width.saturating_sub(UnicodeWidthStr::width(label) + 2);
            let rest_w = max_width.saturating_sub(indent.len());
            let wrapped = wrap_cell_text(value, first_w.max(10), false);

            // First line: bold label + value
            let mut spans = vec![Span::styled(format!("{}: ", label), header_style)];
            if let Some(first) = wrapped.first() {
                spans.push(Span::styled(first.to_string(), cell_style));
            }
            lines.push(Line::from(spans));

            // Continuation lines
            for cont in wrapped.iter().skip(1) {
                let rewrapped = wrap_cell_text(cont, rest_w, false);
                for rline in rewrapped {
                    if !rline.trim().is_empty() {
                        lines.push(Line::from(vec![
                            Span::raw(indent.to_string()),
                            Span::styled(rline, cell_style),
                        ]));
                    }
                }
            }
        }
    }
    lines
}

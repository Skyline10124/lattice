// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Display-only word wrapping. Input editing deliberately uses its own character wrapping.

use std::borrow::Cow;

use ratatui::style::Style;
use ratatui::text::{Line, Span, StyledGrapheme};
use unicode_width::UnicodeWidthStr;

/// Styles do not form word boundaries. Newlines do; wide characters may break individually.
/// Use the renderer's graphemes so combining marks and emoji sequences stay intact,
/// without adding a second segmentation dependency. Callers expand tabs first.
pub(crate) fn wrap(spans: Vec<Span<'static>>, width: usize) -> Vec<Line<'static>> {
    let mut paragraphs = vec![Vec::new()];
    for span in &spans {
        for (index, text) in span.content.split('\n').enumerate() {
            if index > 0 {
                paragraphs.push(Vec::new());
            }
            paragraphs
                .last_mut()
                .unwrap()
                .push(Span::styled(text.to_owned(), span.style));
        }
    }
    let mut lines = Vec::new();
    for paragraph in &paragraphs {
        let glyphs: Vec<_> = paragraph
            .iter()
            .flat_map(|span| span.styled_graphemes(Style::default()))
            .collect();
        wrap_line(&glyphs, width.max(1), &mut lines);
    }
    lines
}

/// ratatui silently drops control characters, so display text entering the renderer
/// replaces tabs once, before any width is measured. Stops are every 4 columns from the
/// start of each source line.
pub(crate) fn expand_tabs(text: &str) -> Cow<'_, str> {
    expand_tabs_tracking(text, |_, _| {})
}

/// 回调记录展开区间与源 Tab 偏移，供 Markdown 还原字节范围。
pub(crate) fn expand_tabs_tracking(
    text: &str,
    mut tab: impl FnMut(std::ops::Range<usize>, usize),
) -> Cow<'_, str> {
    if !text.contains('\t') {
        return Cow::Borrowed(text);
    }
    let mut expanded = String::with_capacity(text.len());
    let mut original = 0usize;
    for (index, line) in text.split('\n').enumerate() {
        if index > 0 {
            expanded.push('\n');
            original = original.saturating_add(1);
        }
        let mut column = 0usize;
        for (index, part) in line.split('\t').enumerate() {
            if index > 0 {
                let spaces = 4usize.saturating_sub(column % 4);
                let start = expanded.len();
                expanded.push_str(&" ".repeat(spaces));
                tab(start..expanded.len(), original);
                original = original.saturating_add(1);
                column = column.saturating_add(spaces);
            }
            expanded.push_str(part);
            original = original.saturating_add(part.len());
            column = column.saturating_add(part.width());
        }
    }
    Cow::Owned(expanded)
}

fn push(line: &mut Line<'static>, glyph: &StyledGrapheme<'_>) {
    if let Some(last) = line.spans.last_mut()
        && last.style == glyph.style
    {
        last.content.to_mut().push_str(glyph.symbol);
    } else {
        line.spans
            .push(Span::styled(glyph.symbol.to_owned(), glyph.style));
    }
}

fn whitespace(glyph: &StyledGrapheme<'_>) -> bool {
    glyph.symbol.chars().all(char::is_whitespace)
}

fn wrap_line(source: &[StyledGrapheme<'_>], width: usize, lines: &mut Vec<Line<'static>>) {
    let mut line = Line::default();
    let mut used = 0;
    let mut start = 0;
    let mut spaces = 0..0;
    while start < source.len() {
        let glyph = &source[start];
        let mut end = start + 1;
        if whitespace(glyph) {
            while end < source.len() && whitespace(&source[end]) {
                end += 1;
            }
            spaces = start..end;
            start = end;
            continue;
        }
        if glyph.symbol.width() < 2 {
            while end < source.len() && !whitespace(&source[end]) && source[end].symbol.width() < 2
            {
                end += 1;
            }
        }
        let word_width: usize = source[start..end]
            .iter()
            .map(|glyph| glyph.symbol.width())
            .sum();
        let space_width: usize = source[spaces.clone()]
            .iter()
            .map(|glyph| glyph.symbol.width())
            .sum();
        if used > 0 && word_width <= width && used + space_width + word_width > width {
            lines.push(std::mem::take(&mut line));
            used = 0;
            spaces = 0..0;
        }
        for glyph in &source[spaces.clone()] {
            if used + glyph.symbol.width() > width {
                lines.push(std::mem::take(&mut line));
                used = 0;
            }
            push(&mut line, glyph);
            used += glyph.symbol.width();
        }
        spaces = 0..0;
        for glyph in &source[start..end] {
            let size = glyph.symbol.width();
            if used > 0 && size > 0 && used + size > width {
                lines.push(std::mem::take(&mut line));
                used = 0;
            }
            push(&mut line, glyph);
            used += size;
        }
        start = end;
    }
    // Trailing spaces have no visible content, except on whitespace-only source lines.
    if line.spans.is_empty() && !spaces.is_empty() {
        for glyph in &source[spaces] {
            if used + glyph.symbol.width() > width {
                lines.push(std::mem::take(&mut line));
                used = 0;
            }
            push(&mut line, glyph);
            used += glyph.symbol.width();
        }
    }
    lines.push(line);
}

// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Markdown is a presentation projection: neither parsed events nor layout enter Block storage.

use std::sync::LazyLock;

use pulldown_cmark::{CodeBlockKind, Event, OffsetIter, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use syntect::easy::HighlightLines;
use syntect::highlighting::{Color as SyntaxColor, StyleModifier, Theme, ThemeItem, ThemeSettings};
use syntect::parsing::SyntaxSet;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::view::{Role, role_style};
use crate::wrapping::{expand_tabs_tracking, wrap};

const OPTIONS: Options = Options::ENABLE_TABLES
    .union(Options::ENABLE_TASKLISTS)
    .union(Options::ENABLE_STRIKETHROUGH);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SourceElement {
    pub(crate) range: std::ops::Range<usize>,
    pub(crate) indent: usize,
}

/// 正文和源映射一起缓存；折行共享元素范围。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RenderedLine {
    pub(crate) line: Line<'static>,
    pub(crate) element: Option<SourceElement>,
    pub(crate) ending_items: Vec<std::ops::Range<usize>>,
}

impl std::ops::Deref for RenderedLine {
    type Target = Line<'static>;
    fn deref(&self) -> &Self::Target {
        &self.line
    }
}

impl std::fmt::Display for RenderedLine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.line, f)
    }
}

fn mapped(
    lines: Vec<Line<'static>>,
    element: Option<SourceElement>,
) -> impl Iterator<Item = RenderedLine> {
    lines.into_iter().map(move |line| RenderedLine {
        line,
        element: element.clone(),
        ending_items: Vec::new(),
    })
}

fn source_element(source: &str, range: std::ops::Range<usize>) -> Option<SourceElement> {
    let text = source.get(range.clone())?;
    Some(SourceElement {
        range: range.start
            ..range
                .start
                .saturating_add(text.trim_end_matches(['\r', '\n']).len()),
        indent: 0,
    })
}

/// 正文只由原文与宽度决定；一次完整投影结束即回收不再使用的版本。
#[derive(Default)]
pub(crate) struct RenderCache {
    entries: std::collections::HashMap<u16, std::collections::HashMap<String, CachedBody>>,
}

struct CachedBody {
    lines: std::rc::Rc<Vec<RenderedLine>>,
    used: bool,
}

impl RenderCache {
    pub(crate) fn render(&mut self, source: &str, width: u16) -> std::rc::Rc<Vec<RenderedLine>> {
        let entries = self.entries.entry(width).or_default();
        if let Some(entry) = entries.get_mut(source) {
            entry.used = true;
            return entry.lines.clone();
        }
        let lines = std::rc::Rc::new(render(source, width));
        entries.insert(
            source.to_owned(),
            CachedBody {
                lines: lines.clone(),
                used: true,
            },
        );
        lines
    }

    pub(crate) fn finish_render(&mut self) {
        self.entries.retain(|_, entries| {
            entries.retain(|_, entry| std::mem::take(&mut entry.used));
            !entries.is_empty()
        });
    }
}

#[cfg(test)]
mod cache_tests {
    use super::*;
    use std::rc::Rc;

    #[test]
    fn cached_render_hits_and_reclaims_unused_widths_and_stream_versions() {
        let mut cache = RenderCache::default();
        let source = "# 标题\n\n```rust\nlet value = 42;\n```";
        let first = cache.render(source, 24);
        assert_eq!(*first, render(source, 24));
        assert!(
            Rc::ptr_eq(&first, &cache.render(source, 24)),
            "unchanged body must hit cache"
        );
        cache.finish_render();
        let old = Rc::downgrade(&first);
        drop(first);
        let narrow = cache.render(source, 12);
        let growing = cache.render(&format!("{source}\n追加"), 24);
        assert_eq!(*narrow, render(source, 12));
        assert_eq!(*growing, render(&format!("{source}\n追加"), 24));
        cache.finish_render();
        assert!(old.upgrade().is_none(), "unused width/source is reclaimed");
        assert!(Rc::ptr_eq(&narrow, &cache.render(source, 12)));
        cache.finish_render();
        let unused = Rc::downgrade(&growing);
        drop(growing);
        assert!(
            unused.upgrade().is_none(),
            "only the growing body was invalidated"
        );
        let remaining = Rc::downgrade(&narrow);
        drop(narrow);
        cache.finish_render();
        assert!(remaining.upgrade().is_none());
    }
}

#[cfg(test)]
mod source_tests {
    use super::*;

    #[test]
    fn source_ranges_follow_wrapped_paragraphs_and_headings() {
        let source = "# 标题\n\nfirst **word**\nnext";
        let rendered = render(source, 8);
        let mapped: Vec<_> = rendered
            .iter()
            .map(|row| row.element.as_ref().map(|e| (e.range.clone(), e.indent)))
            .collect();
        assert_eq!(
            mapped,
            [
                Some((0..8, 0)),
                None,
                Some((10..29, 0)),
                Some((10..29, 0)),
                Some((10..29, 0))
            ]
        );
    }

    #[test]
    fn source_ranges_preserve_tabs_and_nested_list_quote_code_table_elements() {
        for (source, width, expected) in [
            (
                "- outer\n  - inner wide\n- last\n\n> quoted\n> words",
                12,
                vec![
                    Some(("- outer\n  - inner wide", 2)),
                    Some(("- inner wide", 4)),
                    Some(("- inner wide", 4)),
                    Some(("- last", 2)),
                    None,
                    Some(("quoted\n> words", 2)),
                    Some(("quoted\n> words", 2)),
                ],
            ),
            (
                "# 中\t题\n\n段\t落 宽\n\n> 👨‍👩‍👧‍👦\t尾",
                10,
                vec![
                    Some(("# 中\t题", 0)),
                    None,
                    Some(("段\t落 宽", 0)),
                    None,
                    Some(("👨‍👩‍👧‍👦\t尾", 2)),
                ],
            ),
            (
                "> ```\n> \t中 aa bb\n> \n> tail\n> ```",
                12,
                vec![
                    None,
                    Some(("\t中 aa bb", 6)),
                    Some(("\t中 aa bb", 6)),
                    Some(("", 4)),
                    Some(("tail", 4)),
                    None,
                ],
            ),
            (
                "| 中 | b |\n| -- | -- |\n| 宽字 | tail |",
                12,
                vec![
                    None,
                    Some(("| 中 | b |", 2)),
                    None,
                    Some(("| 宽字 | tail |", 2)),
                    Some(("| 宽字 | tail |", 2)),
                    None,
                ],
            ),
        ] {
            let rows = render(source, width);
            let actual: Vec<_> = rows
                .iter()
                .map(|r| {
                    r.element
                        .as_ref()
                        .map(|e| (source.get(e.range.clone()).unwrap(), e.indent))
                })
                .collect();
            assert_eq!(actual, expected, "{source}");
        }
    }
}

pub(crate) fn render(source: &str, width: u16) -> Vec<RenderedLine> {
    // Expanding before parsing matches CommonMark, which also places tab stops from the
    // start of the source line when it determines block structure.
    let mut tabs = Vec::new();
    let expanded = expand_tabs_tracking(source, |range, original| tabs.push((range, original)));
    let mut lines = Renderer {
        source: &expanded,
        // 代码行保留独立偏移；普通行内文字在 inline_event 合并。
        events: Parser::new_ext(&expanded, OPTIONS)
            .into_offset_iter()
            .peekable(),
        task_item: false,
    }
    .blocks(None, usize::from(width), None);
    let original_offset = |offset: usize| {
        let preceding = tabs.partition_point(|(range, _)| range.start < offset);
        match preceding.checked_sub(1).and_then(|index| tabs.get(index)) {
            Some((range, original)) if offset < range.end => *original,
            Some((range, original)) => original
                .saturating_add(1)
                .saturating_add(offset.saturating_sub(range.end)),
            None => offset,
        }
    };
    for row in &mut lines {
        for range in &mut row.ending_items {
            *range = original_offset(range.start)..original_offset(range.end);
        }
        if let Some(element) = &mut row.element {
            element.range =
                original_offset(element.range.start)..original_offset(element.range.end);
            if source.get(element.range.clone()).is_none() {
                row.element = None;
            }
        }
    }
    lines
}

struct Renderer<'a> {
    source: &'a str,
    events: std::iter::Peekable<OffsetIter<'a>>,
    /// Set when the list item being rendered starts with a task checkbox.
    task_item: bool,
}

impl Renderer<'_> {
    fn blocks(
        &mut self,
        end: Option<TagEnd>,
        width: usize,
        item: Option<SourceElement>,
    ) -> Vec<RenderedLine> {
        let mut lines = Vec::new();
        let mut pending = Vec::new();
        while let Some((event, range)) = self.events.next() {
            if matches!(event, Event::End(tag) if Some(tag) == end) {
                break;
            }
            match event {
                Event::Start(Tag::Paragraph) => {
                    begin_block(&mut lines, &mut pending, width, item.clone());
                    let spans = self.inline(TagEnd::Paragraph, role_style(Role::Assistant));
                    lines.extend(mapped(
                        wrap(spans, width),
                        source_element(self.source, range),
                    ));
                }
                Event::Start(Tag::Heading { level, .. }) => {
                    begin_block(&mut lines, &mut pending, width, item.clone());
                    let spans = self.inline(
                        TagEnd::Heading(level),
                        role_style(Role::Assistant).add_modifier(Modifier::BOLD),
                    );
                    lines.extend(mapped(
                        wrap(spans, width),
                        source_element(self.source, range),
                    ));
                }
                Event::Start(Tag::BlockQuote(kind)) => {
                    begin_block(&mut lines, &mut pending, width, item.clone());
                    let quoted = self.blocks(
                        Some(TagEnd::BlockQuote(kind)),
                        width.saturating_sub(2),
                        None,
                    );
                    for mut line in quoted {
                        if let Some(element) = &mut line.element {
                            element.indent = element.indent.saturating_add(2);
                        }
                        for span in &mut line.line.spans {
                            span.style = span.style.patch(role_style(Role::DimMeta));
                        }
                        line.line
                            .spans
                            .insert(0, Span::styled("▎ ", role_style(Role::DimMeta)));
                        lines.push(line);
                    }
                }
                Event::Start(Tag::List(start)) => {
                    begin_block(&mut lines, &mut pending, width, item.clone());
                    lines.extend(self.list(start.is_some(), width));
                }
                Event::Start(Tag::CodeBlock(kind)) => {
                    begin_block(&mut lines, &mut pending, width, item.clone());
                    let fenced = matches!(kind, CodeBlockKind::Fenced(_));
                    let language = match kind {
                        CodeBlockKind::Fenced(info) => {
                            info.split_whitespace().next().unwrap_or("").to_owned()
                        }
                        CodeBlockKind::Indented => String::new(),
                    };
                    // End(CodeBlock) also occurs at EOF or a container boundary. Only an
                    // explicit closing fence extends the block past its opening/body text.
                    let mut text_end = self
                        .source
                        .get(range.clone())
                        .unwrap_or("")
                        .find('\n')
                        .map_or(range.end, |offset| {
                            range.start.saturating_add(offset).saturating_add(1)
                        });
                    let mut text = String::new();
                    let mut elements = Vec::new();
                    for (event, range) in self.events.by_ref() {
                        match event {
                            Event::Text(part) => {
                                let mut start = range.start;
                                for line in self
                                    .source
                                    .get(range.clone())
                                    .unwrap_or("")
                                    .split_inclusive('\n')
                                {
                                    let end = start.saturating_add(line.len());
                                    elements.push(source_element(self.source, start..end));
                                    start = end;
                                }
                                text.push_str(&part);
                                text_end = range.end;
                            }
                            Event::End(TagEnd::CodeBlock) => break,
                            _ => {}
                        }
                    }
                    lines.extend(code_block(
                        &language,
                        &text,
                        width,
                        !fenced || range.end > text_end,
                        elements,
                    ));
                }
                Event::Start(Tag::Table(_)) => {
                    begin_block(&mut lines, &mut pending, width, item.clone());
                    lines.extend(self.table(width));
                }
                Event::Start(Tag::HtmlBlock) => {
                    begin_block(&mut lines, &mut pending, width, item.clone());
                    let text = plain(&self.inline(TagEnd::HtmlBlock, role_style(Role::Assistant)));
                    lines.extend(mapped(
                        wrap(
                            vec![Span::styled(
                                text.trim_end_matches('\n').to_owned(),
                                role_style(Role::Assistant),
                            )],
                            width,
                        ),
                        None,
                    ));
                }
                Event::Rule => {
                    begin_block(&mut lines, &mut pending, width, item.clone());
                    lines.extend(mapped(
                        vec![Line::from(Span::styled(
                            "─".repeat(width.min(8)),
                            role_style(Role::DimMeta),
                        ))],
                        None,
                    ));
                }
                event => pending.extend(self.inline_event(event, role_style(Role::Assistant))),
            }
        }
        flush(&mut lines, &mut pending, width, item);
        lines
    }

    fn inline(&mut self, end: TagEnd, style: Style) -> Vec<Span<'static>> {
        let mut spans = Vec::new();
        while let Some((event, _)) = self.events.next() {
            if event == Event::End(end) {
                break;
            }
            spans.extend(self.inline_event(event, style));
        }
        spans
    }

    fn inline_event(&mut self, event: Event<'_>, style: Style) -> Vec<Span<'static>> {
        match event {
            Event::Text(text) => {
                let mut text = text.into_string();
                while matches!(self.events.peek(), Some((Event::Text(_), _))) {
                    if let Some((Event::Text(next), _)) = self.events.next() {
                        text.push_str(&next);
                    }
                }
                autolinks(&text, style)
            }
            Event::Html(text) | Event::InlineHtml(text) => {
                vec![Span::styled(text.into_string(), style)]
            }
            Event::Code(text) => vec![Span::styled(
                format!("`{text}`"),
                Style {
                    fg: role_style(Role::Assistant).fg,
                    bg: None,
                    ..style
                },
            )],
            Event::SoftBreak | Event::HardBreak => vec![Span::raw("\n")],
            Event::TaskListMarker(checked) => {
                self.task_item = true;
                vec![Span::styled(if checked { "☑ " } else { "☐ " }, style)]
            }
            Event::Start(tag @ (Tag::Emphasis | Tag::Strong | Tag::Strikethrough)) => {
                let modifier = match tag {
                    Tag::Emphasis => Modifier::ITALIC,
                    Tag::Strong => Modifier::BOLD,
                    _ => Modifier::CROSSED_OUT,
                };
                self.inline(tag.to_end(), style.add_modifier(modifier))
            }
            Event::Start(tag @ (Tag::Link { .. } | Tag::Image { .. })) => {
                let end = tag.to_end();
                let destination = match tag {
                    Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. } => dest_url,
                    _ => unreachable!(),
                };
                let mut label = self.inline(end, style);
                if plain(&label) == destination.as_ref() {
                    for span in &mut label {
                        span.style = span.style.patch(role_style(Role::DimMeta));
                    }
                } else {
                    label.push(Span::styled(
                        format!(" ({destination})"),
                        role_style(Role::DimMeta),
                    ));
                }
                label
            }
            // These extensions are not enabled. Container boundaries are handled by blocks/table/list.
            Event::Start(_)
            | Event::End(_)
            | Event::Rule
            | Event::FootnoteReference(_)
            | Event::InlineMath(_)
            | Event::DisplayMath(_) => Vec::new(),
        }
    }

    fn list(&mut self, ordered: bool, width: usize) -> Vec<RenderedLine> {
        // A nested list must not clear the flag of the item that contains it.
        let outer_task = std::mem::take(&mut self.task_item);
        let mut lines = Vec::new();
        while let Some((event, range)) = self.events.next() {
            match event {
                Event::End(TagEnd::List(_)) => break,
                Event::Start(Tag::Item) => {
                    // The parser exposes only the list's initial number. Offsets retain each
                    // item's actual marker, including non-sequential source numbers.
                    let marker = if ordered {
                        let number: String = self
                            .source
                            .get(range.clone())
                            .unwrap_or("")
                            .chars()
                            .take_while(char::is_ascii_digit)
                            .collect();
                        format!("{number}. ")
                    } else {
                        "• ".to_owned()
                    };
                    let indent = marker.width();
                    self.task_item = false;
                    let mut item = self.blocks(
                        Some(TagEnd::Item),
                        width.saturating_sub(indent),
                        source_element(self.source, range.clone()),
                    );
                    // The checkbox replaces the bullet; both are two columns wide.
                    let marker = if self.task_item && !ordered {
                        String::new()
                    } else {
                        marker
                    };
                    if item.is_empty() {
                        item.extend(mapped(vec![Line::default()], None));
                    }
                    if let Some(last) = item.last_mut()
                        && let Some(element) = source_element(self.source, range)
                    {
                        last.ending_items.push(element.range);
                    }
                    for (index, mut line) in item.into_iter().enumerate() {
                        if let Some(element) = &mut line.element {
                            element.indent = element.indent.saturating_add(indent);
                        }
                        line.line.spans.insert(
                            0,
                            Span::styled(
                                if index == 0 {
                                    marker.clone()
                                } else {
                                    " ".repeat(indent)
                                },
                                role_style(Role::Assistant),
                            ),
                        );
                        lines.push(line);
                    }
                }
                _ => {}
            }
        }
        self.task_item = outer_task;
        lines
    }

    fn table(&mut self, width: usize) -> Vec<RenderedLine> {
        let mut rows = Vec::new();
        let mut elements = Vec::new();
        let mut row = Vec::new();
        while let Some((event, range)) = self.events.next() {
            match event {
                Event::End(TagEnd::Table) => break,
                Event::Start(Tag::TableHead | Tag::TableRow) => {
                    elements.push(source_element(self.source, range).map(|mut element| {
                        element.indent = 2;
                        element
                    }));
                }
                Event::Start(Tag::TableCell) => {
                    row.push(self.inline(TagEnd::TableCell, role_style(Role::Assistant)))
                }
                Event::End(TagEnd::TableHead | TagEnd::TableRow) => {
                    rows.push(std::mem::take(&mut row))
                }
                _ => {}
            }
        }
        table_lines(rows, width, elements)
    }
}

/// GFM extended autolinks: bare `http://`, `https://` and `www.` URLs in text display as links.
fn autolinks(text: &str, style: Style) -> Vec<Span<'static>> {
    let link = style.patch(role_style(Role::DimMeta));
    let mut spans = Vec::new();
    let mut rest = text;
    while let Some(url) = find_url(rest) {
        if url.start > 0 {
            spans.push(Span::styled(rest[..url.start].to_owned(), style));
        }
        spans.push(Span::styled(rest[url.clone()].to_owned(), link));
        rest = &rest[url.end..];
    }
    if !rest.is_empty() {
        spans.push(Span::styled(rest.to_owned(), style));
    }
    spans
}

fn find_url(text: &str) -> Option<std::ops::Range<usize>> {
    let mut from = 0;
    loop {
        let (start, prefix) = ["https://", "http://", "www."]
            .into_iter()
            .filter_map(|prefix| Some((from + text[from..].find(prefix)?, prefix)))
            .min()?;
        let boundary = text[..start]
            .chars()
            .next_back()
            .is_none_or(|ch| ch.is_whitespace() || "*_~(".contains(ch));
        let end = text[start..]
            .find(|ch: char| ch.is_whitespace() || ch == '<')
            .map_or(text.len(), |offset| start + offset);
        // Trailing punctuation ends the sentence, and an unmatched `)` closes surrounding text.
        let mut url = &text[start..end];
        loop {
            let trimmed = url.trim_end_matches(['?', '!', '.', ',', ':', '*', '_', '~']);
            let trimmed = match trimmed.strip_suffix(')') {
                Some(inner) if trimmed.matches(')').count() > trimmed.matches('(').count() => inner,
                _ => trimmed,
            };
            if trimmed.len() == url.len() {
                break;
            }
            url = trimmed;
        }
        if boundary && url.len() > prefix.len() {
            return Some(start..start + url.len());
        }
        // Prefixes are ASCII, so the next byte is a character boundary.
        from = start + 1;
    }
}

fn plain(spans: &[Span<'_>]) -> String {
    spans.iter().map(|span| span.content.as_ref()).collect()
}

fn flush(
    lines: &mut Vec<RenderedLine>,
    spans: &mut Vec<Span<'static>>,
    width: usize,
    element: Option<SourceElement>,
) {
    if !spans.is_empty() {
        lines.extend(mapped(wrap(std::mem::take(spans), width), element));
    }
}

/// Separates block-level elements by one blank line. Text still pending here belongs to a
/// tight list item, which stays attached to the nested block that follows it.
fn begin_block(
    lines: &mut Vec<RenderedLine>,
    pending: &mut Vec<Span<'static>>,
    width: usize,
    item: Option<SourceElement>,
) {
    let tight = !pending.is_empty();
    flush(lines, pending, width, item);
    if !tight && !lines.is_empty() {
        lines.extend(mapped(vec![Line::default()], None));
    }
}

// The transcript is redrawn every frame; only per-block parsing state is rebuilt.
static SYNTAXES: LazyLock<SyntaxSet> = LazyLock::new(two_face::syntax::extra_newlines);
static CODE_THEME: LazyLock<Theme> = LazyLock::new(|| Theme {
    settings: ThemeSettings {
        foreground: Some(SyntaxColor {
            r: 0xf5,
            g: 0xf3,
            b: 0xef,
            a: 255,
        }), // cream-2
        ..ThemeSettings::default()
    },
    scopes: [
        (
            "string, string punctuation, string constant",
            (0x68, 0xc6, 0xc6),
        ), // cyan-6
        (
            "constant.numeric, constant.language, support.constant",
            (0xc1, 0xa6, 0xe3),
        ), // purple-6
        // TypeScript also uses entity.name for calls and unspecialized type references;
        // declarations have distinct suffixes (e.g. entity.name.type.alias.ts). Rust marks the
        // implemented type in `impl Item` as entity.name.impl, though it is a reference.
        (
            "entity.name - meta.function-call - entity.name.type.ts - entity.name.impl",
            (0xa2, 0xb0, 0xf2),
        ), // iris-6
        (
            "comment, comment punctuation, comment constant",
            (0xe4, 0xa6, 0x5f),
        ), // amber-6
        ("punctuation", (0xa0, 0x9a, 0x94)), // warm-gray-7
    ]
    .into_iter()
    .map(|(scope, (r, g, b))| ThemeItem {
        scope: scope.parse().expect("static code scope selector"),
        style: StyleModifier {
            foreground: Some(SyntaxColor { r, g, b, a: 255 }),
            ..StyleModifier::default()
        },
    })
    .collect(),
    ..Theme::default()
});

fn code_block(
    language: &str,
    text: &str,
    width: usize,
    closed: bool,
    elements: Vec<Option<SourceElement>>,
) -> Vec<RenderedLine> {
    let dim = role_style(Role::DimMeta);
    let label = if language.is_empty() {
        "┌".to_owned()
    } else {
        format!("┌ {language}")
    };
    let mut title = String::new();
    let mut used = 0usize;
    for ch in label.chars() {
        let size = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used.saturating_add(size) > width {
            break;
        }
        title.push(ch);
        used = used.saturating_add(size);
    }
    let mut lines: Vec<_> = mapped(vec![Line::from(Span::styled(title, dim))], None).collect();
    let language = language.to_ascii_lowercase();
    let mut highlighter = if language.is_empty() || language == "diff" {
        None
    } else {
        SYNTAXES
            .find_syntax_by_token(&language)
            .map(|syntax| HighlightLines::new(syntax, &CODE_THEME))
    };
    for (source_index, source) in text.split_terminator('\n').enumerate() {
        let indent = source
            .len()
            .saturating_sub(source.trim_start_matches(' ').len());
        let body = source.get(indent..).unwrap_or("");
        let element = elements
            .get(source_index)
            .cloned()
            .flatten()
            .map(|mut element| {
                element.indent = indent.saturating_add(2);
                element
            });
        let available = width.saturating_sub(2).saturating_sub(indent);
        let spans = if let Some(parser) = highlighter.as_mut() {
            match code_spans(parser, source, indent) {
                Ok(spans) => spans,
                Err(_) => {
                    // A failed parse may leave partial state. Keep the remaining text readable.
                    highlighter = None;
                    vec![Span::styled(body.to_owned(), role_style(Role::Assistant))]
                }
            }
        } else {
            plain_code_spans(&language, body, source)
        };
        for (index, mut line) in wrap(spans, available).into_iter().enumerate() {
            line.spans.insert(
                0,
                Span::styled(" ".repeat(indent), role_style(Role::Assistant)),
            );
            line.spans
                .insert(0, Span::styled(if index == 0 { "│ " } else { "┆ " }, dim));
            lines.push(RenderedLine {
                line,
                element: element.clone(),
                ending_items: Vec::new(),
            });
        }
    }
    if closed {
        lines.extend(mapped(
            vec![Line::from(Span::styled(
                if width > 0 { "└" } else { "" },
                dim,
            ))],
            None,
        ));
    }
    lines
}

fn code_spans(
    highlighter: &mut HighlightLines<'_>,
    source: &str,
    mut indent: usize,
) -> Result<Vec<Span<'static>>, syntect::Error> {
    // Parse the complete source line, including indentation and the required newline,
    // before stripping layout-only bytes. The caller reuses this state for the whole block.
    let source = format!("{source}\n");
    let ranges = highlighter.highlight_line(&source, &SYNTAXES)?;
    Ok(ranges
        .into_iter()
        .filter_map(|(style, text)| {
            let text = text.trim_end_matches('\n');
            let skip = indent.min(text.len());
            indent -= skip;
            let text = &text[skip..];
            let color = style.foreground;
            (!text.is_empty()).then(|| {
                Span::styled(
                    text.to_owned(),
                    Style::default().fg(Color::Rgb(color.r, color.g, color.b)),
                )
            })
        })
        .collect())
}

fn plain_code_spans(language: &str, text: &str, source: &str) -> Vec<Span<'static>> {
    let role = if language != "diff" {
        Role::Assistant
    } else if source.starts_with("---") || source.starts_with("+++") || source.starts_with("@@") {
        Role::DimMeta
    } else if source.starts_with('+') {
        Role::ToolOk
    } else if source.starts_with('-') {
        Role::ToolErr
    } else {
        Role::Assistant
    };
    vec![Span::styled(text.to_owned(), role_style(role))]
}

fn column_widths(natural: &[usize], width: usize) -> Vec<usize> {
    if natural.is_empty() {
        return Vec::new();
    }
    let natural: Vec<_> = natural.iter().map(|&value| value.max(2)).collect();
    let budget = width.saturating_sub(3 * natural.len() + 1);
    if natural.iter().sum::<usize>() <= budget {
        return natural;
    }
    if budget < 2 * natural.len() {
        return vec![2; natural.len()];
    }

    let average = budget / natural.len();
    let mut result = vec![0; natural.len()];
    let mut remaining = budget;
    let mut flexible = Vec::new();
    for (index, &value) in natural.iter().enumerate() {
        if value <= average {
            result[index] = value;
            remaining -= value;
        } else {
            flexible.push(index);
        }
    }
    // Enforce the lower bound before proportional allocation; never subtract a
    // rounded share from another column or let unsigned remaining width underflow.
    loop {
        let total: usize = flexible.iter().map(|&index| natural[index]).sum();
        let small: Vec<_> = flexible
            .iter()
            .copied()
            .filter(|&index| remaining * natural[index] < 2 * total)
            .collect();
        if small.is_empty() {
            break;
        }
        for index in small {
            result[index] = 2;
            remaining -= 2;
            flexible.retain(|&other| other != index);
        }
    }
    let total: usize = flexible.iter().map(|&index| natural[index]).sum();
    let mut remainders = Vec::new();
    for &index in &flexible {
        result[index] = remaining * natural[index] / total;
        remainders.push((index, remaining * natural[index] % total));
    }
    remainders.sort_by_key(|&(index, remainder)| (std::cmp::Reverse(remainder), index));
    let spare = budget - result.iter().sum::<usize>();
    for &(index, _) in remainders.iter().take(spare) {
        result[index] += 1;
    }
    result
}

fn table_lines(
    rows: Vec<Vec<Vec<Span<'static>>>>,
    width: usize,
    elements: Vec<Option<SourceElement>>,
) -> Vec<RenderedLine> {
    let columns = rows.first().map_or(0, Vec::len);
    let natural: Vec<_> = (0..columns)
        .map(|index| {
            rows.iter()
                .filter_map(|row| row.get(index))
                .map(|cell| {
                    plain(cell)
                        .split('\n')
                        .map(UnicodeWidthStr::width)
                        .max()
                        .unwrap_or(0)
                })
                .max()
                .unwrap_or(0)
        })
        .collect();
    let widths = column_widths(&natural, width);
    let dim = role_style(Role::DimMeta);
    let border = |left: char, middle: char, right: char| {
        let mut border = String::from(left);
        for (index, &width) in widths.iter().enumerate() {
            border.push_str(&"─".repeat(width + 2));
            border.push(if index + 1 == columns { right } else { middle });
        }
        RenderedLine {
            line: Line::from(Span::styled(border, dim)),
            element: None,
            ending_items: Vec::new(),
        }
    };
    let mut lines = vec![border('┌', '┬', '┐')];
    for (index, row) in rows.into_iter().enumerate() {
        let cells: Vec<_> = row
            .into_iter()
            .zip(&widths)
            .map(|(cell, &width)| wrap(cell, width))
            .collect();
        let height = cells.iter().map(Vec::len).max().unwrap_or(1);
        for y in 0..height {
            let mut spans = vec![Span::styled("│", dim)];
            for (cell, &width) in cells.iter().zip(&widths) {
                spans.push(Span::raw(" "));
                let line = cell.get(y).cloned().unwrap_or_default();
                let padding = width.saturating_sub(line.width());
                spans.extend(line.spans);
                spans.push(Span::raw(" ".repeat(padding + 1)));
                spans.push(Span::styled("│", dim));
            }
            lines.push(RenderedLine {
                line: Line::from(spans),
                element: elements.get(index).cloned().flatten(),
                ending_items: Vec::new(),
            });
        }
        if index == 0 {
            lines.push(border('├', '┼', '┤'));
        }
    }
    lines.push(border('└', '┴', '┘'));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(source: &str, width: u16) -> Vec<String> {
        render(source, width)
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    #[test]
    fn inline_elements_keep_text_and_terminal_attributes() {
        let lines = render("# Heading\n\n*italic* **bold** ~~gone~~ `code`", 80);
        assert_eq!(lines[0].to_string(), "Heading");
        assert!(
            lines[0].spans[0]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
        let spans = &lines.last().unwrap().spans;
        for (word, modifier) in [
            ("italic", Modifier::ITALIC),
            ("bold", Modifier::BOLD),
            ("gone", Modifier::CROSSED_OUT),
        ] {
            assert!(
                spans.iter().any(|span| span.content.contains(word)
                    && span.style.add_modifier.contains(modifier))
            );
        }
        let code = spans
            .iter()
            .find(|span| span.content.contains("`code`"))
            .unwrap();
        assert_eq!(code.style, role_style(Role::Assistant));
        assert!(
            lines
                .iter()
                .flat_map(|line| &line.spans)
                .all(|span| span.style.bg.is_none())
        );
    }

    #[test]
    fn inline_code_preserves_outer_modifiers() {
        for (source, modifier) in [
            ("# `name`", Modifier::BOLD),
            ("**`name`**", Modifier::BOLD),
            ("*`name`*", Modifier::ITALIC),
            ("~~`name`~~", Modifier::CROSSED_OUT),
        ] {
            let lines = render(source, 80);
            assert_eq!(lines[0].to_string(), "`name`");
            for span in &lines[0].spans {
                assert!(span.style.add_modifier.contains(modifier), "{source}");
                assert_eq!(span.style.fg, role_style(Role::Assistant).fg);
                assert!(span.style.bg.is_none());
            }
        }
    }

    #[test]
    fn soft_hard_breaks_and_raw_html_remain_visible() {
        assert_eq!(
            text("first\nsecond  \nthird", 80),
            ["first", "second", "third"]
        );
        assert_eq!(text("a <b>raw</b> &amp; text", 80), ["a <b>raw</b> & text"]);
        assert_eq!(
            text("<div>\n*not italic*\n</div>", 80),
            ["<div>", "*not italic*", "</div>"]
        );
    }

    #[test]
    fn lists_preserve_nesting_source_numbers_and_tasks() {
        assert_eq!(
            text("3. first\n8. second\n   - nested\n     - deeper", 40),
            ["3. first", "8. second", "   • nested", "     • deeper"]
        );
        assert_eq!(
            text("- [ ] todo\n- [x] done\n  - [ ] nested\n1. [x] step", 40),
            ["☐ todo", "☑ done", "  ☐ nested", "", "1. ☑ step"]
        );
        assert_eq!(text("- one three two", 9), ["• one", "  three", "  two"]);
    }

    #[test]
    fn blocks_are_separated_but_tight_list_items_stay_together() {
        assert_eq!(
            text("# T\npara\n> q\n\n- a\n  - b\n- c\n```\nx\n```\n---", 6),
            [
                "T",
                "",
                "para",
                "",
                "▎ q",
                "",
                "• a",
                "  • b",
                "• c",
                "",
                "┌",
                "│ x",
                "└",
                "",
                "──────",
            ]
        );
    }

    #[test]
    fn quotes_prefix_every_visual_line_and_rules_stay_short() {
        let lines = render("> one three two\n>\n> next", 9);
        assert_eq!(
            lines.iter().map(ToString::to_string).collect::<Vec<_>>(),
            ["▎ one", "▎ three", "▎ two", "▎ ", "▎ next"]
        );
        assert!(
            lines
                .iter()
                .flat_map(|line| &line.spans)
                .all(|span| span.style.fg == role_style(Role::DimMeta).fg)
        );
        for (width, expected) in [(80, "────────"), (8, "────────"), (3, "───"), (0, "")]
        {
            let rule = render("---", width);
            assert_eq!(rule[0].to_string(), expected);
            assert_eq!(rule[0].spans[0].style, role_style(Role::DimMeta));
        }
    }

    #[test]
    fn links_show_destinations_once_and_bare_urls_are_links() {
        let lines = render(
            "[label](https://a.test) <https://b.test> [https://c.test](https://c.test)",
            100,
        );
        assert_eq!(
            lines[0].to_string(),
            "label (https://a.test) https://b.test https://c.test"
        );
        for span in &lines[0].spans {
            if span.content.contains("https://") {
                assert_eq!(span.style.fg, role_style(Role::DimMeta).fg);
            }
        }
        let bare = "see https://example.org/a_b, (www.example.org) and x.www.no";
        let lines = render(bare, 100);
        assert_eq!(lines[0].to_string(), bare);
        let links: Vec<_> = lines[0]
            .spans
            .iter()
            .filter(|span| span.style.fg == role_style(Role::DimMeta).fg)
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(links, ["https://example.org/a_b", "www.example.org"]);
    }

    #[test]
    fn code_borders_wrapping_indentation_and_tabs() {
        let lines = render("```rust extra\n\tone three two\n```", 13);
        assert_eq!(
            lines.iter().map(ToString::to_string).collect::<Vec<_>>(),
            ["┌ rust", "│     one", "┆     three", "┆     two", "└"]
        );
        assert_eq!(lines[0].spans[0].style, role_style(Role::DimMeta));
        for line in &lines[1..] {
            assert_eq!(line.spans[0].style, role_style(Role::DimMeta));
            assert!(
                line.spans[1..]
                    .iter()
                    .all(|span| span.style == role_style(Role::Assistant))
            );
        }
        assert_eq!(text("```\nx\n```", 5), ["┌", "│ x", "└"]);
        assert_eq!(text("```rust\nx\n\n", 12), ["┌ rust", "│ x", "│ "]);
        assert_eq!(text("    plain code", 20), ["┌", "│ plain code", "└"]);
        assert_eq!(text("```rust\nx\n```\nafter", 12).last().unwrap(), "after");
        for language in ["", "unknown-language"] {
            let lines = render(
                &format!("```{language}\nfn load() {{ \"text\"; 42; }}\n```"),
                80,
            );
            assert!(
                lines[1].spans[1..]
                    .iter()
                    .all(|span| span.style == role_style(Role::Assistant))
            );
        }
    }

    #[test]
    fn code_bottom_waits_for_an_explicit_closing_fence() {
        for source in [
            "```",
            "```rust",
            "```rust\n",
            "```rust\nx",
            "```rust\nx\n``",
            "```rust\nx\n~~~",
            "```rust\nx\n``` not a fence",
            "```rust\nx\n    ```",
            "````rust\nx\n```",
            "> ```rust\n> x\n\nafter",
            "- ```rust\n  x\n\nafter",
        ] {
            assert!(
                !text(source, 40)
                    .iter()
                    .any(|line| line.trim_end().ends_with('└')),
                "premature bottom: {source:?}"
            );
        }
        for source in [
            "```\n```",
            "```rust\nx\n```",
            "```rust\nx\n````  \n",
            "~~~rust\nx\n~~~",
            "```rust\r\nx\r\n```\r\n",
            "> ```rust\n> x\n> ```",
            "- ```rust\n  x\n  ```",
        ] {
            assert!(
                text(source, 40).last().unwrap().ends_with('└'),
                "missing bottom: {source:?}"
            );
        }
        assert_eq!(text("```\n```", 20), ["┌", "└"]);
        for width in 0..=3 {
            let lines = render("```语言\nx\n```", width);
            assert!(lines.first().unwrap().width() <= usize::from(width));
            assert!(lines.last().unwrap().width() <= usize::from(width));
        }
    }

    fn assert_color(line: &Line<'_>, text: &str, color: Color) {
        let start = line.to_string().find(text).expect("expected code text");
        for style in line
            .spans
            .iter()
            .flat_map(|span| std::iter::repeat_n(span.style, span.content.len()))
            .skip(start)
            .take(text.len())
        {
            assert_eq!(style, Style::default().fg(color), "color of {text:?}");
        }
    }

    #[test]
    fn rust_code_colors_definitions_not_calls_and_keeps_literal_delimiters() {
        let lines = render(
            "```rust\nfn load(n: u32) {\n    let s = \"hello\\n\";\n    let n = 42; let ok = true;\n    load(n); // comment\n}\nstruct Item;\nfn inspect(item: Item) {}\nimpl Item {}\n```",
            80,
        );
        let body = Color::Rgb(0xf5, 0xf3, 0xef);
        assert_color(&lines[1], "fn", body);
        assert_color(&lines[1], "load", Color::Rgb(0xa2, 0xb0, 0xf2));
        assert_color(&lines[2], "let s", body);
        assert_color(&lines[1], "u32", body);
        assert_color(&lines[1], "(", Color::Rgb(0xa0, 0x9a, 0x94));
        assert_color(&lines[2], "\"hello\\n\"", Color::Rgb(0x68, 0xc6, 0xc6));
        assert_color(&lines[2], ";", Color::Rgb(0xa0, 0x9a, 0x94));
        assert_color(&lines[3], "42", Color::Rgb(0xc1, 0xa6, 0xe3));
        assert_color(&lines[3], "true", Color::Rgb(0xc1, 0xa6, 0xe3));
        assert_color(&lines[4], "load", body);
        assert_color(&lines[4], "// comment", Color::Rgb(0xe4, 0xa6, 0x5f));
        assert_color(&lines[6], "Item", Color::Rgb(0xa2, 0xb0, 0xf2));
        assert_color(&lines[7], "Item", body);
        assert_color(&lines[8], "Item", body);
    }

    #[test]
    fn code_language_alias_is_case_insensitive() {
        let lines = render(
            "```TS extra\nfunction load(item: Item): Item { return item; }\nload(value);\nconst value = \"hello\";\ntype Alias = Item;\n```",
            80,
        );
        let body = Color::Rgb(0xf5, 0xf3, 0xef);
        assert_color(&lines[1], "load", Color::Rgb(0xa2, 0xb0, 0xf2));
        assert_color(&lines[1], "Item", body);
        assert_color(&lines[2], "load", body);
        assert_color(&lines[3], "\"hello\"", Color::Rgb(0x68, 0xc6, 0xc6));
        assert_color(&lines[4], "Alias", Color::Rgb(0xa2, 0xb0, 0xf2));
        assert_color(&lines[4], "Item", body);
    }

    #[test]
    fn code_parser_retains_multiline_comment_and_string_state() {
        let lines = render(
            "```rs\n/* first\n   second */\nlet s = \"first\nsecond\";\n```",
            80,
        );
        assert_color(&lines[1], "/* first", Color::Rgb(0xe4, 0xa6, 0x5f));
        assert_color(&lines[2], "second */", Color::Rgb(0xe4, 0xa6, 0x5f));
        assert_color(&lines[3], "\"first", Color::Rgb(0x68, 0xc6, 0xc6));
        assert_color(&lines[4], "second\"", Color::Rgb(0x68, 0xc6, 0xc6));
        assert_color(&lines[4], ";", Color::Rgb(0xa0, 0x9a, 0x94));
    }

    #[test]
    fn diff_headers_take_precedence_over_additions_and_deletions() {
        let lines = render(
            "```DiFf\n--- old\n+++ new\n@@ range\n+added\n-deleted\n context\n```",
            40,
        );
        for (line, role) in lines[1..].iter().zip([
            Role::DimMeta,
            Role::DimMeta,
            Role::DimMeta,
            Role::ToolOk,
            Role::ToolErr,
            Role::Assistant,
        ]) {
            let content = line
                .spans
                .iter()
                .rev()
                .find(|span| !span.content.trim().is_empty())
                .unwrap();
            assert_eq!(content.style.fg, role_style(role).fg);
            assert!(line.spans.iter().all(|span| span.style.bg.is_none()));
        }
        assert_eq!(lines[4].to_string(), "│ +added");
        assert_eq!(lines[5].to_string(), "│ -deleted");
    }

    #[test]
    fn table_widths_natural_compressed_minimum_and_remainders() {
        assert_eq!(column_widths(&[2, 10, 20], 42), [2, 10, 20]);
        assert_eq!(column_widths(&[2, 10, 20], 30), [2, 6, 12]);
        assert_eq!(column_widths(&[2, 10, 20], 27), [2, 5, 10]);
        assert_eq!(column_widths(&[10, 10, 10], 24), [5, 5, 4]);
        assert_eq!(column_widths(&[0, 1, 20], 0), [2, 2, 2]);
        assert_eq!(column_widths(&[4, 5, 1000], 20), [2, 2, 6]);
        for width in 0..80 {
            let widths = column_widths(&[2, 10, 20], width);
            assert!(widths.iter().all(|&value| value >= 2));
            if width >= 16 {
                assert!(widths.iter().sum::<usize>() + 10 <= width);
            }
        }
    }

    #[test]
    fn table_cells_wrap_and_pad_to_the_tallest_cell() {
        let source = "| id | description |\n| --- | --- |\n| 中 | **one three** |";
        let natural = render(source, 40);
        assert_eq!(natural[0].to_string(), "┌────┬─────────────┐");
        assert_eq!(natural[1].to_string(), "│ id │ description │");
        assert_eq!(natural[2].to_string(), "├────┼─────────────┤");
        assert_eq!(natural[4].to_string(), "└────┴─────────────┘");
        for border in [0, 2, 4] {
            assert_eq!(natural[border].spans[0].style, role_style(Role::DimMeta));
        }
        let compressed = render(source, 14);
        assert_eq!(
            compressed
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            [
                "┌────┬───────┐",
                "│ id │ descr │",
                "│    │ iptio │",
                "│    │ n     │",
                "├────┼───────┤",
                "│ 中 │ one   │",
                "│    │ three │",
                "└────┴───────┘",
            ]
        );
        assert!(compressed[6].spans.iter().any(
            |span| span.content == "three" && span.style.add_modifier.contains(Modifier::BOLD)
        ));
        assert!(render(source, 2).iter().all(|line| line.width() == 11));
    }

    #[test]
    fn nested_code_tables_and_tiny_widths_are_safe() {
        let source = "> - ```rust\n>   \tindented code\n>   ```\n>\n> |a|b|\n> |-|-|\n> |中|word|";
        for width in 0..20 {
            let lines = render(source, width);
            assert!(!lines.is_empty());
            assert!(lines.iter().all(|line| line.to_string().starts_with("▎ ")));
        }
    }
}

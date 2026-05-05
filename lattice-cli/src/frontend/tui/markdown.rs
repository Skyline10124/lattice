//! Markdown-to-ratatui renderer.
//!
//! Two-phase architecture:
//! 1. `parse_blocks` — pulldown-cmark events → `Vec<MdBlock>` (structural blocks)
//! 2. `blocks_to_lines` — blocks → styled `Vec<Line>` with word-wrapping at terminal width
//!
//! Supports: headings, bold/italic/strikethrough, inline code, fenced code blocks,
//! blockquotes, ordered/unordered/task lists, links, horizontal rules.

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};
use unicode_width::UnicodeWidthStr;

use super::theme::Theme;

const MIN_BLOCK_WIDTH: usize = 20;
const MIN_BODY_WIDTH: usize = 10;

/// Scan first 500 chars for markdown syntax markers. If none found, content is plain
/// text and we can skip pulldown-cmark parsing entirely.
fn has_markdown_syntax(s: &str) -> bool {
    let sample = if s.len() > 500 {
        let mut end = 500;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        &s[..end]
    } else {
        s
    };
    // Check for consecutive newlines (paragraph break), code fences, or common markers
    let bytes = sample.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'#' | b'*' | b'`' | b'>' | b'\\' | b'_' | b'~' => return true,
            b'-' | b'|' => return true,
            b'\n' if i + 1 < bytes.len() && bytes[i + 1] == b'\n' => return true,
            _ => {}
        }
    }
    // Check for ordered-list start (N. at line start or after newline)
    let text = sample;
    for (i, _) in text.char_indices() {
        let rest = &text[i..];
        if let Some(after) = rest.strip_prefix('\n') {
            if after.chars().next().is_some_and(|c| c.is_ascii_digit()) && after.contains(". ") {
                return true;
            }
        }
    }
    // Check if starts with a digit followed by ". " (ordered list at start)
    if let Some(first) = text.chars().next() {
        if first.is_ascii_digit() && text.contains(". ") {
            return true;
        }
    }
    false
}

pub fn render_markdown(content: &str, theme: &Theme, width: u16) -> Vec<Line<'static>> {
    if !has_markdown_syntax(content) {
        return render_plain_text(content, theme, width);
    }
    let blocks = parse_blocks(content);
    blocks_to_lines(blocks, theme, width)
}

fn render_plain_text(content: &str, theme: &Theme, width: u16) -> Vec<Line<'static>> {
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

// -- parse phase --

enum MdBlock {
    Heading {
        level: u8,
        spans: Vec<MdSpan>,
    },
    Paragraph {
        spans: Vec<MdSpan>,
    },
    CodeBlock {
        lang: Option<String>,
        text: String,
    },
    Blockquote {
        spans: Vec<MdSpan>,
    },
    ListItem {
        depth: u32,
        marker: ListMarker,
        spans: Vec<MdSpan>,
    },
    Table {
        headers: Vec<Vec<MdSpan>>,
        rows: Vec<Vec<Vec<MdSpan>>>,
    },
    Math {
        _display: bool,
        text: String,
    },
    Rule,
}

enum ListMarker {
    Bullet,
    Numbered(u64),
    Task { checked: bool },
}

struct MdSpan {
    text: String,
    bold: bool,
    italic: bool,
    strikethrough: bool,
    code: bool,
    link_url: Option<String>,
}

fn parse_blocks(content: &str) -> Vec<MdBlock> {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_FOOTNOTES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_MATH);

    let parser = Parser::new_ext(content, options);

    let mut blocks: Vec<MdBlock> = Vec::new();
    let mut current_spans: Vec<MdSpan> = Vec::new();
    let mut code_block_text = String::new();

    // inline state
    let mut bold = false;
    let mut italic = false;
    let mut strikethrough = false;
    // pulldown-cmark emits Event::Code(String) for inline code, never
    // Start/End toggle events, so this stays false for MdSpan text fields.
    let code = false;
    let mut link_url: Option<String> = None;

    // block state
    let mut heading_level: Option<u8> = None;
    let mut in_blockquote = false;
    let mut in_code_block = false;
    let mut code_block_lang: Option<String> = None;
    let mut list_marker: Option<ListMarker> = None;
    let mut list_depth: u32 = 0;
    let mut list_is_ordered: Vec<bool> = Vec::new();
    let mut list_item_count: Vec<u32> = Vec::new();

    // table state
    let mut table_headers: Vec<Vec<MdSpan>> = Vec::new();
    let mut table_rows: Vec<Vec<Vec<MdSpan>>> = Vec::new();
    let mut table_cells: Vec<Vec<MdSpan>> = Vec::new();
    let mut table_cell_spans: Vec<MdSpan> = Vec::new();
    let mut in_table_cell = false;

    macro_rules! flush_inline {
        () => {
            if !current_spans.is_empty() {
                let spans = std::mem::take(&mut current_spans);
                if let Some(level) = heading_level.take() {
                    blocks.push(MdBlock::Heading { level, spans });
                } else if in_blockquote {
                    blocks.push(MdBlock::Blockquote { spans });
                } else if let Some(marker) = list_marker.take() {
                    blocks.push(MdBlock::ListItem {
                        depth: list_depth,
                        marker,
                        spans,
                    });
                } else {
                    blocks.push(MdBlock::Paragraph { spans });
                }
            }
        };
    }

    for event in parser {
        match event {
            Event::Start(tag) => match tag {
                Tag::Heading { level, .. } => {
                    flush_inline!();
                    heading_level = Some(level as u8);
                }
                Tag::Paragraph => {}
                Tag::CodeBlock(kind) => {
                    flush_inline!();
                    in_code_block = true;
                    code_block_lang = match kind {
                        CodeBlockKind::Fenced(lang) if !lang.is_empty() => Some(lang.to_string()),
                        _ => None,
                    };
                }
                Tag::BlockQuote(_) => {
                    flush_inline!();
                    in_blockquote = true;
                }
                Tag::List(order) => {
                    list_depth += 1;
                    list_is_ordered.push(order.is_some());
                    list_item_count.push(0);
                }
                Tag::Item => {
                    let idx = list_depth.saturating_sub(1) as usize;
                    if idx < list_item_count.len() {
                        list_item_count[idx] += 1;
                        if idx < list_is_ordered.len() && list_is_ordered[idx] {
                            list_marker = Some(ListMarker::Numbered(list_item_count[idx] as u64));
                        } else {
                            list_marker = Some(ListMarker::Bullet);
                        }
                    } else {
                        list_marker = Some(ListMarker::Bullet);
                    }
                }
                Tag::Table(_) => {
                    flush_inline!();
                    table_headers.clear();
                    table_rows.clear();
                }
                Tag::TableHead => {
                    table_cells.clear();
                }
                Tag::TableRow => {
                    table_cells.clear();
                }
                Tag::TableCell => {
                    in_table_cell = true;
                    table_cell_spans.clear();
                }
                Tag::Emphasis => italic = true,
                Tag::Strong => bold = true,
                Tag::Strikethrough => strikethrough = true,
                Tag::Link { dest_url, .. } => {
                    link_url = Some(dest_url.to_string());
                }
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Heading(_) => {
                    flush_inline!();
                    heading_level = None;
                }
                TagEnd::Paragraph => {
                    flush_inline!();
                }
                TagEnd::CodeBlock => {
                    in_code_block = false;
                    if !code_block_text.is_empty() {
                        // trim trailing newline
                        if code_block_text.ends_with('\n') {
                            code_block_text.pop();
                        }
                        blocks.push(MdBlock::CodeBlock {
                            lang: code_block_lang.take(),
                            text: std::mem::take(&mut code_block_text),
                        });
                    }
                    code_block_lang = None;
                }
                TagEnd::BlockQuote(_) => {
                    flush_inline!();
                    in_blockquote = false;
                }
                TagEnd::List(_) if list_depth > 0 => {
                    list_depth -= 1;
                    list_is_ordered.pop();
                    list_item_count.pop();
                }
                TagEnd::Item => {
                    flush_inline!();
                    list_marker = None;
                }
                TagEnd::TableCell => {
                    in_table_cell = false;
                    let cell = std::mem::take(&mut table_cell_spans);
                    table_cells.push(cell);
                }
                TagEnd::TableRow => {
                    let row = std::mem::take(&mut table_cells);
                    table_rows.push(row);
                }
                TagEnd::TableHead => {
                    // Header cells are directly inside TableHead (no TableRow wrapper)
                    table_headers = std::mem::take(&mut table_cells);
                }
                TagEnd::Table => {
                    let headers = std::mem::take(&mut table_headers);
                    let rows = std::mem::take(&mut table_rows);
                    if !headers.is_empty() || !rows.is_empty() {
                        blocks.push(MdBlock::Table { headers, rows });
                    }
                }
                TagEnd::Emphasis => italic = false,
                TagEnd::Strong => bold = false,
                TagEnd::Strikethrough => strikethrough = false,
                TagEnd::Link => link_url = None,
                _ => {}
            },
            Event::Text(text) => {
                let s = text.to_string();
                if in_code_block {
                    code_block_text.push_str(&s);
                } else if in_table_cell {
                    table_cell_spans.push(MdSpan {
                        text: s,
                        bold,
                        italic,
                        strikethrough,
                        code,
                        link_url: link_url.clone(),
                    });
                } else {
                    current_spans.push(MdSpan {
                        text: s,
                        bold,
                        italic,
                        strikethrough,
                        code,
                        link_url: link_url.clone(),
                    });
                }
            }
            Event::Code(text) => {
                if in_code_block {
                    code_block_text.push_str(&text);
                    code_block_text.push('\n');
                } else if in_table_cell {
                    table_cell_spans.push(MdSpan {
                        text: text.to_string(),
                        bold: false,
                        italic: false,
                        strikethrough: false,
                        code: true,
                        link_url: None,
                    });
                } else {
                    current_spans.push(MdSpan {
                        text: text.to_string(),
                        bold: false,
                        italic: false,
                        strikethrough: false,
                        code: true,
                        link_url: None,
                    });
                }
            }
            Event::SoftBreak => {
                let span = MdSpan {
                    text: " ".to_string(),
                    bold,
                    italic,
                    strikethrough,
                    code,
                    link_url: link_url.clone(),
                };
                if in_table_cell {
                    table_cell_spans.push(span);
                } else {
                    current_spans.push(span);
                }
            }
            Event::HardBreak => {
                let span = MdSpan {
                    text: "\n".to_string(),
                    bold,
                    italic,
                    strikethrough,
                    code,
                    link_url: link_url.clone(),
                };
                if in_table_cell {
                    table_cell_spans.push(span);
                } else {
                    current_spans.push(span);
                }
            }
            Event::InlineMath(text) => current_spans.push(MdSpan {
                text: format!("${}$", text),
                bold: false,
                italic: false,
                strikethrough: false,
                code: true,
                link_url: None,
            }),
            Event::DisplayMath(text) => {
                flush_inline!();
                blocks.push(MdBlock::Math {
                    _display: true,
                    text: text.to_string(),
                });
            }
            Event::Rule => {
                flush_inline!();
                blocks.push(MdBlock::Rule);
            }
            Event::TaskListMarker(checked) => {
                list_marker = Some(ListMarker::Task { checked });
            }
            _ => {}
        }
    }

    flush_inline!();
    blocks
}

// -- render phase --

fn blocks_to_lines(blocks: Vec<MdBlock>, theme: &Theme, width: u16) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let usable = (width as usize).max(MIN_BLOCK_WIDTH);

    for block in blocks {
        match block {
            MdBlock::Heading { level, spans } => {
                let style = theme.heading_style(level);
                lines.extend(wrap_spans(&spans, style, theme, usable));
                lines.push(Line::raw(""));
            }
            MdBlock::Paragraph { spans } => {
                lines.extend(wrap_spans(&spans, Style::default(), theme, usable));
                lines.push(Line::raw(""));
            }
            MdBlock::CodeBlock { lang, text } => {
                lines.extend(render_code_block(&lang, &text, theme, usable));
                lines.push(Line::raw(""));
            }
            MdBlock::Blockquote { spans } => {
                let style = theme.blockquote_style();
                lines.extend(wrap_spans_with_prefix(&spans, style, "│ ", theme, usable));
                lines.push(Line::raw(""));
            }
            MdBlock::ListItem {
                depth,
                marker,
                spans,
            } => {
                let prefix = list_prefix(&marker, depth);
                let prefix_width = UnicodeWidthStr::width(prefix.as_str());
                let marker_style = theme.list_marker_style();
                lines.extend(wrap_spans_with_custom_prefix(
                    &spans,
                    &prefix,
                    prefix_width,
                    marker_style,
                    theme,
                    usable,
                ));
            }
            MdBlock::Table { headers, rows } => {
                lines.extend(render_table(&headers, &rows, theme, usable));
                lines.push(Line::raw(""));
            }
            MdBlock::Math { text, .. } => {
                lines.extend(render_math_block(&text, theme, usable));
                lines.push(Line::raw(""));
            }
            MdBlock::Rule => {
                let rule = "─".repeat(usable.min(40));
                lines.push(Line::styled(rule, Style::default().fg(theme.border)));
                lines.push(Line::raw(""));
            }
        }
    }

    lines
}

fn list_prefix(marker: &ListMarker, depth: u32) -> String {
    let indent = "  ".repeat(depth.saturating_sub(1) as usize);
    match marker {
        ListMarker::Bullet => format!("{}• ", indent),
        ListMarker::Numbered(n) => format!("{}{}. ", indent, n),
        ListMarker::Task { checked } => {
            let box_ch = if *checked { "☑" } else { "☐" };
            format!("{}{} ", indent, box_ch)
        }
    }
}

// -- word wrapping helpers --

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

/// Word-wrap spans into lines, splitting on newlines and wrapping at width.
fn wrap_spans(
    spans: &[MdSpan],
    base_style: Style,
    theme: &Theme,
    max_width: usize,
) -> Vec<Line<'static>> {
    wrap_spans_with_prefix(spans, base_style, "", theme, max_width)
}

fn wrap_spans_with_prefix(
    spans: &[MdSpan],
    base_style: Style,
    prefix: &str,
    theme: &Theme,
    max_width: usize,
) -> Vec<Line<'static>> {
    let prefix_width = UnicodeWidthStr::width(prefix);
    wrap_spans_with_custom_prefix(spans, prefix, prefix_width, base_style, theme, max_width)
}

fn wrap_spans_with_custom_prefix(
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

// -- code block rendering --

fn render_code_block(
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

/// Like truncate_str but preserves styled segments.
fn truncate_line_to_width(segments: &[(String, Style)], max_width: usize) -> Vec<(String, Style)> {
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

const TABLE_SAFETY_MARGIN: usize = 4;
const TABLE_MIN_COL_WIDTH: usize = 3;
const TABLE_MAX_ROW_LINES: usize = 4;

fn render_table(
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

fn render_math_block(text: &str, theme: &Theme, max_width: usize) -> Vec<Line<'static>> {
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

fn truncate_str(s: &str, max_width: usize) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn t() -> Theme {
        Theme::catppuccin_mocha()
    }

    fn line_texts(lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    // -- paragraphs --

    #[test]
    fn plain_paragraph() {
        let lines = render_markdown("hello world", &t(), 80);
        let texts = line_texts(&lines);
        assert!(
            texts.iter().any(|t| t.contains("hello world")),
            "texts: {:?}",
            texts
        );
    }

    #[test]
    fn empty_input() {
        let lines = render_markdown("", &t(), 80);
        // empty markdown produces no paragraph blocks, but a trailing blank line
        assert!(
            lines.is_empty()
                || lines
                    .iter()
                    .all(|l| l.spans.is_empty()
                        || l.spans.iter().all(|s| s.content.trim().is_empty())),
            "expected empty or blank-only lines, got {:?}",
            line_texts(&lines)
        );
    }

    #[test]
    fn long_unicode_plain_text_does_not_panic() {
        let content = "你".repeat(260);
        let lines = render_markdown(&content, &t(), 80);
        assert!(!lines.is_empty());
    }

    // -- headings --

    #[test]
    fn heading_h1_renders_bold() {
        let lines = render_markdown("# Title", &t(), 80);
        let texts = line_texts(&lines);
        assert!(
            texts.iter().any(|t| t.contains("Title")),
            "heading not found: {:?}",
            texts
        );
    }

    #[test]
    fn heading_h2_renders() {
        let lines = render_markdown("## Section", &t(), 80);
        let texts = line_texts(&lines);
        assert!(
            texts.iter().any(|t| t.contains("Section")),
            "h2 not found: {:?}",
            texts
        );
    }

    #[test]
    fn heading_h3_renders() {
        let lines = render_markdown("### Subsection", &t(), 80);
        let texts = line_texts(&lines);
        assert!(
            texts.iter().any(|t| t.contains("Subsection")),
            "h3 not found: {:?}",
            texts
        );
    }

    // -- inline styles --

    #[test]
    fn bold_text() {
        let lines = render_markdown("hello **world** foo", &t(), 80);
        let has_bold = lines.iter().any(|l| {
            l.spans.iter().any(|s| {
                s.content.contains("world") && s.style.add_modifier(Modifier::BOLD) == s.style
            })
        });
        assert!(has_bold, "bold span not found: {:?}", lines);
    }

    #[test]
    fn italic_text() {
        let lines = render_markdown("hello *world* foo", &t(), 80);
        assert!(
            line_texts(&lines).iter().any(|t| t.contains("world")),
            "texts: {:?}",
            line_texts(&lines)
        );
    }

    #[test]
    fn strikethrough_text() {
        let lines = render_markdown("hello ~~world~~ foo", &t(), 80);
        assert!(
            line_texts(&lines).iter().any(|t| t.contains("world")),
            "texts: {:?}",
            line_texts(&lines)
        );
    }

    #[test]
    fn inline_code() {
        let lines = render_markdown("use `std::io` here", &t(), 80);
        let has_code_bg = lines.iter().any(|l| {
            l.spans
                .iter()
                .any(|s| s.content.contains("std::io") && s.style.bg == Some(t().surface))
        });
        assert!(
            has_code_bg,
            "inline code missing surface bg: {:?}",
            line_texts(&lines)
        );
    }

    // -- code blocks --

    #[test]
    fn code_block_no_lang() {
        let lines = render_markdown("```\nlet x = 1;\n```", &t(), 80);
        let texts = line_texts(&lines);
        assert!(
            texts.iter().any(|t| t.contains("let x = 1")),
            "code block missing: {:?}",
            texts
        );
        assert!(
            texts.iter().any(|t| t.contains("┌────")),
            "code block border missing: {:?}",
            texts
        );
    }

    #[test]
    fn code_block_with_lang() {
        let lines = render_markdown("```rust\nfn main() {}\n```", &t(), 80);
        let texts = line_texts(&lines);
        assert!(
            texts.iter().any(|t| t.contains("rust")),
            "lang tag missing: {:?}",
            texts
        );
        assert!(
            texts.iter().any(|t| t.contains("fn main()")),
            "code missing: {:?}",
            texts
        );
    }

    // -- blockquotes --

    #[test]
    fn blockquote_with_prefix() {
        let lines = render_markdown("> quoted text", &t(), 80);
        let texts = line_texts(&lines);
        assert!(
            texts
                .iter()
                .any(|t| t.contains("│") && t.contains("quoted text")),
            "blockquote prefix missing: {:?}",
            texts
        );
    }

    // -- lists --

    #[test]
    fn unordered_list() {
        let lines = render_markdown("- item one\n- item two", &t(), 80);
        let texts = line_texts(&lines);
        assert!(
            texts.iter().any(|t| t.contains("item one")),
            "list item 1: {:?}",
            texts
        );
        assert!(
            texts.iter().any(|t| t.contains("item two")),
            "list item 2: {:?}",
            texts
        );
    }

    #[test]
    fn ordered_list() {
        let lines = render_markdown("1. first\n2. second", &t(), 80);
        let texts = line_texts(&lines);
        assert!(
            texts.iter().any(|t| t.contains("first")),
            "ordered item 1: {:?}",
            texts
        );
        assert!(
            texts.iter().any(|t| t.contains("second")),
            "ordered item 2: {:?}",
            texts
        );
    }

    #[test]
    fn task_list() {
        let lines = render_markdown("- [x] done\n- [ ] todo", &t(), 80);
        let texts = line_texts(&lines);
        assert!(
            texts.iter().any(|t| t.contains("done")),
            "checked item: {:?}",
            texts
        );
        assert!(
            texts.iter().any(|t| t.contains("todo")),
            "unchecked item: {:?}",
            texts
        );
        assert!(
            texts.iter().any(|t| t.contains('\u{2611}')),
            "checked box missing: {:?}",
            texts
        );
        assert!(
            texts.iter().any(|t| t.contains('\u{2610}')),
            "unchecked box missing: {:?}",
            texts
        );
    }

    // -- links --

    #[test]
    fn link_rendered() {
        let lines = render_markdown("see [docs](https://example.com)", &t(), 80);
        let texts = line_texts(&lines);
        assert!(
            texts.iter().any(|t| t.contains("docs")),
            "link text: {:?}",
            texts
        );
    }

    // -- horizontal rule --

    #[test]
    fn horizontal_rule() {
        let lines = render_markdown("before\n\n---\n\nafter", &t(), 80);
        let texts = line_texts(&lines);
        assert!(
            texts.iter().any(|t| t.contains('─')),
            "rule missing: {:?}",
            texts
        );
    }

    // -- multi-block --

    #[test]
    fn heading_then_paragraph() {
        let lines = render_markdown("# Intro\n\nThis is text.", &t(), 80);
        let texts = line_texts(&lines);
        assert!(
            texts.iter().any(|t| t.contains("Intro")),
            "heading: {:?}",
            texts
        );
        assert!(
            texts.iter().any(|t| t.contains("This is text")),
            "paragraph: {:?}",
            texts
        );
    }

    // -- narrow width wrapping --

    #[test]
    fn narrow_width_wraps() {
        let lines = render_markdown("this is a long line that should wrap", &t(), 20);
        let texts = line_texts(&lines);
        // Should produce more lines than a single unwrapped line
        assert!(
            texts.len() > 1,
            "narrow width did not wrap: {} lines, {:?}",
            texts.len(),
            texts
        );
    }

    // -- tables --

    #[test]
    fn table_with_headers() {
        let md = "| Name | Value |\n|------|-------|\n| foo  | 42    |";
        let lines = render_markdown(md, &t(), 80);
        let texts = line_texts(&lines);
        assert!(
            texts.iter().any(|t| t.contains("Name")),
            "header missing: {:?}",
            texts
        );
        assert!(
            texts.iter().any(|t| t.contains("foo")),
            "cell missing: {:?}",
            texts
        );
        assert!(
            texts.iter().any(|t| t.contains("42")),
            "value missing: {:?}",
            texts
        );
        assert!(
            texts.iter().any(|t| t.contains('│')),
            "border missing: {:?}",
            texts
        );
    }

    #[test]
    fn table_no_headers() {
        // pulldown-cmark requires a separator row (---) to detect a table
        let md = "| a | b |\n|---|---|\n| c | d |";
        let lines = render_markdown(md, &t(), 80);
        let texts = line_texts(&lines);
        assert!(
            texts.iter().any(|t| t.contains('│')),
            "border missing: {:?}",
            texts
        );
    }

    #[test]
    fn table_narrow() {
        let md = "| Col | Value |\n|-----|-------|\n| x   | 100   |";
        let lines = render_markdown(md, &t(), 30);
        let texts = line_texts(&lines);
        assert!(
            texts.iter().any(|t| t.contains('│')),
            "border at narrow: {:?}",
            texts
        );
    }

    // -- math --

    #[test]
    fn inline_math_rendered() {
        let lines = render_markdown("the formula $E=mc^2$ is famous", &t(), 80);
        let texts = line_texts(&lines);
        assert!(
            texts.iter().any(|t| t.contains("E=mc^2")),
            "inline math missing: {:?}",
            texts
        );
    }

    #[test]
    fn display_math_rendered() {
        let lines = render_markdown("$$\n\\sum_{i=1}^n x_i\n$$", &t(), 80);
        let texts = line_texts(&lines);
        assert!(
            texts
                .iter()
                .any(|t| t.contains("math") || t.contains("sum")),
            "display math missing: {:?}",
            texts
        );
    }

    // -- block-level return type --

    #[test]
    fn returns_lines() {
        let lines = render_markdown("**test**", &t(), 80);
        // Every span must be Static
        for line in &lines {
            for span in &line.spans {
                let _: &Span<'static> = span; // type check: must compile
            }
        }
    }
}

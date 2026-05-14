use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

use super::ast::{ListMarker, MdBlock, MdSpan};

/// Scan first 500 chars for markdown syntax markers. If none found, content is plain
/// text and we can skip pulldown-cmark parsing entirely.
pub(super) fn has_markdown_syntax(s: &str) -> bool {
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

pub(super) fn parse_blocks(content: &str) -> Vec<MdBlock> {
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

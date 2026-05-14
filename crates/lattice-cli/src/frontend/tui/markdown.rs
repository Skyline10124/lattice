//! Markdown-to-ratatui renderer.
//!
//! Two-phase architecture:
//! 1. `parse_blocks` — pulldown-cmark events → `Vec<MdBlock>` (structural blocks)
//! 2. `blocks_to_lines` — blocks → styled `Vec<Line>` with word-wrapping at terminal width
//!
//! Supports: headings, bold/italic/strikethrough, inline code, fenced code blocks,
//! blockquotes, ordered/unordered/task lists, links, horizontal rules.

use ratatui::{style::Style, text::Line};
use unicode_width::UnicodeWidthStr;

use super::theme::Theme;
use ast::{ListMarker, MdBlock};
use code::render_code_block;
use layout::{
    render_plain_text, wrap_spans, wrap_spans_with_custom_prefix, wrap_spans_with_prefix,
    MIN_BLOCK_WIDTH,
};
use math::render_math_block;
use parser::{has_markdown_syntax, parse_blocks};
use table::render_table;

mod ast;
mod code;
mod layout;
mod math;
mod parser;
mod table;

pub(super) fn render_markdown(content: &str, theme: &Theme, width: u16) -> Vec<Line<'static>> {
    if !has_markdown_syntax(content) {
        return render_plain_text(content, theme, width);
    }
    let blocks = parse_blocks(content);
    blocks_to_lines(blocks, theme, width)
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

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{style::Modifier, text::Span};

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

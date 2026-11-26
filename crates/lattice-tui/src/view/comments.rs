// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::blocks::BlockId;
use crate::comment::{Anchor, Location, SentComment};

pub(crate) enum CommentNavigation {
    Compose(Anchor),
}

impl Transcript {
    pub(crate) fn comment_navigation(
        &self,
        cursor: browse::BrowseCursor,
    ) -> Option<CommentNavigation> {
        self.anchors
            .iter()
            .find(|(row, _, _)| *row == cursor.row)
            .map(|(_, a, _)| CommentNavigation::Compose(a.clone()))
    }

    pub(crate) fn anchor_cursor(&self, location: &Location) -> Option<browse::BrowseCursor> {
        let row = self
            .anchors
            .iter()
            .find(|(_, a, _)| a.location() == location)?
            .0;
        self.cursor(row, 0)
    }

    /// 定位到指定块身份的首个非空视觉行（若全为空则落在该块首行）。
    pub(crate) fn block_cursor(&self, id: BlockId) -> Option<browse::BrowseCursor> {
        let row = self
            .owners
            .iter()
            .enumerate()
            .filter(|(_, owner)| owner.id == id)
            .find(|(row, _)| {
                self.lines.get(*row).is_some_and(|line| {
                    line.spans
                        .iter()
                        .any(|span| !span.content.trim().is_empty())
                })
            })
            .or_else(|| {
                self.owners
                    .iter()
                    .enumerate()
                    .find(|(_, owner)| owner.id == id)
            })?
            .0;
        self.cursor(row, 0)
    }

    /// 正在编辑的输入按哪个宽度折行；没有编辑器时是正文宽度。
    pub(crate) fn editor_width(&self, width: u16) -> u16 {
        self.editor_width.unwrap_or(width)
    }
}

/// 在 `tui/comment` 通道的 `Input` 块内部原地按序渲染已发送评论卡片（块内局部编号 `#1`、`#2`……）。
pub(super) fn render_sent_cards(
    lines: &mut Vec<Line<'static>>,
    comments: &[SentComment],
    width: u16,
) {
    for (index, comment) in comments.iter().enumerate() {
        let number = index.saturating_add(1);
        render_card(
            lines,
            CardSpec {
                indent: 0,
                number,
                state: "",
                quote: Some(&comment.quote),
                text: &comment.text,
                editor: None,
            },
            width,
            None,
        );
    }
}

struct PlacedDraft<'a> {
    after: usize,
    indent: usize,
    id: BlockId,
    number: usize,
    quote: Option<&'a str>,
    text: &'a str,
    state: &'static str,
    editor: Option<&'a InputBuffer>,
}

/// 挂载本地未发送评论草稿：
/// - 锚点在屏且引文一致时高亮锚点行并在元素末尾原地挂载；
/// - 锚点当前不在屏时挂载在对话记录末尾、`SubmissionWait` 之前（含 `SubmissionWait` 前为 0 行的边界）。
pub(super) fn attach(app: &App, transcript: &mut Transcript, wait_start: usize, width: u16) {
    let anchors: std::collections::HashMap<&Location, (usize, &Anchor, usize)> = transcript
        .anchors
        .iter()
        .map(|(r, a, i)| (a.location(), (*r, a, *i)))
        .collect();

    let mut anchored = Vec::new();
    let mut fallback = Vec::new();
    let mut highlighted = std::collections::HashSet::new();

    for (number, draft) in app.drafts.numbered_drafts() {
        let comment = draft.comment();
        let editor = app.drafts.editor(draft.id);
        if let Some((row, _, indent)) = anchors
            .get(&comment.location)
            .filter(|(_, a, _)| a.matches(comment))
        {
            highlighted.insert(&comment.location);
            let after = transcript
                .ending_items
                .get(&comment.location)
                .copied()
                .unwrap_or(*row);
            anchored.push(PlacedDraft {
                after,
                indent: *indent,
                id: draft.id,
                number,
                quote: None,
                text: &comment.text,
                state: "草稿",
                editor,
            });
        } else {
            fallback.push(PlacedDraft {
                after: wait_start,
                indent: 0,
                id: draft.id,
                number,
                quote: Some(&comment.quote),
                text: &comment.text,
                state: "草稿 · 原位置未显示",
                editor,
            });
        }
    }

    anchored.sort_by_key(|p| (p.after, p.number));
    let mut anchored = anchored.into_iter().peekable();
    let mut old_lines = std::mem::take(&mut transcript.lines);
    for (row, anchor, indent) in &transcript.anchors {
        if highlighted.contains(anchor.location())
            && let Some(line) = old_lines.get_mut(*row)
        {
            highlight(line, *indent);
        }
    }
    let old_owners = std::mem::take(&mut transcript.owners);
    let old_len = old_lines.len();
    let mut relocation = Vec::with_capacity(old_len);

    let mut fallback = Some(fallback);
    for (row, (line, owner)) in old_lines.into_iter().zip(old_owners).enumerate() {
        if row == wait_start
            && let Some(items) = fallback.take()
            && !items.is_empty()
        {
            for draft in items {
                render_draft(transcript, draft, width);
            }
            let wait_has_text = line
                .spans
                .iter()
                .any(|span| !span.content.trim().is_empty());
            if wait_has_text {
                transcript.lines.push(Line::default());
                transcript.owners.push(BlockPosition {
                    id: owner.id,
                    row: 0,
                });
            }
        }
        relocation.push(transcript.lines.len());
        transcript.lines.push(line);
        transcript.owners.push(owner);
        while anchored.peek().is_some_and(|p| p.after == row) {
            let Some(draft) = anchored.next() else {
                break;
            };
            render_draft(transcript, draft, width);
        }
    }

    if let Some(items) = fallback.take() {
        for draft in items {
            render_draft(transcript, draft, width);
        }
    }

    for (row, _, _) in &mut transcript.anchors {
        if let Some(mapped) = relocation.get(*row) {
            *row = *mapped;
        }
    }
}

/// 锚点元素的视觉行从元素正文起点铺底色到文字结束处；列表符号等前缀与行尾空白不铺。
fn highlight(line: &mut Line<'static>, indent: usize) {
    let end = line.width().saturating_sub(
        line.spans
            .iter()
            .rev()
            .map(|span| (span.width(), span.content.trim_end().width()))
            .take_while(|(_, text)| *text == 0)
            .map(|(width, _)| width)
            .sum::<usize>(),
    );
    let mut col = 0usize;
    for span in &mut line.spans {
        let start = col;
        col = col.saturating_add(span.width());
        if start >= indent && col <= end {
            span.style = span.style.bg(COMMENT_ANCHOR_BG);
        }
    }
}

/// 评论卡片的标签，两端各留一格。
const COMMENT_TAG: &str = " COMMENT ";

/// 左右内边距列数。终端字符单元高约为宽的两倍，两列与上下各一行的留白视觉上接近。
const CARD_PAD: usize = 2;

struct CardSpec<'a> {
    indent: usize,
    number: usize,
    state: &'static str,
    quote: Option<&'a str>,
    text: &'a str,
    editor: Option<&'a InputBuffer>,
}

#[derive(Default)]
struct EditorMeta {
    cursor: Option<(usize, usize)>,
    width: Option<u16>,
}

fn render_draft(transcript: &mut Transcript, placed: PlacedDraft<'_>, width: u16) {
    let start = transcript.lines.len();
    let mut editor_meta = EditorMeta::default();
    render_card(
        &mut transcript.lines,
        CardSpec {
            indent: placed.indent,
            number: placed.number,
            state: placed.state,
            quote: placed.quote,
            text: placed.text,
            editor: placed.editor,
        },
        width,
        Some(&mut editor_meta),
    );
    if let Some(cursor) = editor_meta.cursor {
        transcript.editor_cursor = Some(cursor);
    }
    if let Some(ew) = editor_meta.width {
        transcript.editor_width = Some(ew);
    }
    transcript
        .owners
        .extend((start..transcript.lines.len()).map(|row| BlockPosition {
            id: placed.id,
            row: row.saturating_sub(start),
        }));
}

/// 渲染单张评论卡片（供已发送自包含评论、屏内锚点草稿与底部兜底草稿共用）。
fn render_card(
    lines: &mut Vec<Line<'static>>,
    spec: CardSpec<'_>,
    width: u16,
    editor_meta: Option<&mut EditorMeta>,
) {
    let indent = spec.indent.min(usize::from(width));
    let card_w = usize::from(width).saturating_sub(indent);
    let body_width = u16::try_from(card_w.saturating_sub(2 * CARD_PAD)).unwrap_or(u16::MAX);
    let mut right = Vec::new();
    if !spec.state.is_empty() {
        right.push(spec.state.to_owned());
    }
    if spec.editor.is_some() {
        right.extend(["Enter 保存".to_owned(), "Esc 放弃".to_owned()]);
    }
    right.push(format!("#{}", spec.number));

    if !lines.is_empty() {
        lines.push(Line::default());
    }
    lines.push(card_row(indent, card_w, Vec::new()));
    lines.push(card_row(indent, card_w, tag_row(card_w, &right)));
    lines.push(card_row(indent, card_w, Vec::new()));

    if let Some(quote) = spec.quote {
        let quote_width = usize::from(body_width).saturating_sub(2).max(1);
        for raw_line in quote.split('\n') {
            let clean = raw_line.strip_suffix('\r').unwrap_or(raw_line);
            let wrapped = wrapping::wrap(
                vec![Span::styled(
                    wrapping::expand_tabs(clean).into_owned(),
                    role_style(Role::DimMeta),
                )],
                quote_width,
            );
            for line in wrapped {
                let mut spans = vec![
                    Span::raw(" ".repeat(CARD_PAD)),
                    Span::styled("▎ ", role_style(Role::DimMeta)),
                ];
                spans.extend(line.spans);
                lines.push(card_row(indent, card_w, spans));
            }
        }
        lines.push(card_row(indent, card_w, Vec::new()));
    }

    let visual = if let Some(input) = spec.editor {
        let (visual, row, col) = input_visual(input, body_width);
        if let Some(meta) = editor_meta {
            meta.cursor = Some((
                lines.len().saturating_add(row),
                indent.saturating_add(CARD_PAD).saturating_add(col),
            ));
            meta.width = Some(body_width);
        }
        visual
            .into_iter()
            .map(|text| Line::from(Span::styled(text, role_style(Role::Assistant))))
            .collect::<Vec<_>>()
    } else {
        wrapping::wrap(
            vec![Span::styled(
                wrapping::expand_tabs(spec.text).into_owned(),
                role_style(Role::Assistant),
            )],
            usize::from(body_width),
        )
    };
    for line in visual {
        let mut spans = vec![Span::raw(" ".repeat(CARD_PAD))];
        spans.extend(line.spans);
        lines.push(card_row(indent, card_w, spans));
    }
    lines.push(card_row(indent, card_w, Vec::new()));
}

/// 标签行：左侧 COMMENT 标签，右端状态放不下时从最左一项起省略。
fn tag_row(card_w: usize, right: &[String]) -> Vec<Span<'static>> {
    let tag = truncate_display(COMMENT_TAG, card_w.saturating_sub(2 * CARD_PAD));
    let used = display_width(&tag).saturating_add(2 * CARD_PAD);
    let mut right = right;
    // 标签与状态之间至少留一格，状态右侧是内边距。
    while !right.is_empty()
        && used
            .saturating_add(1)
            .saturating_add(joined_width(right.iter().map(String::as_str)))
            > card_w
    {
        right = &right[1..];
    }
    let mut spans = vec![
        Span::raw(" ".repeat(CARD_PAD)),
        Span::styled(
            tag,
            Style::default()
                .fg(role_color(Role::Assistant))
                .bg(COMMENT_TAG_BG)
                .add_modifier(ratatui::style::Modifier::BOLD),
        ),
    ];
    if !right.is_empty() {
        let right = right.join(" · ");
        let gap = card_w.saturating_sub(used.saturating_add(display_width(&right)));
        spans.push(Span::raw(" ".repeat(gap)));
        spans.push(Span::styled(right, role_style(Role::DimMeta)));
    }
    spans
}

/// 在卡片内容上铺卡片底色并补足到卡片宽度；已有底色的片段（标签）保持原样。
fn card_row(indent: usize, card_w: usize, spans: Vec<Span<'static>>) -> Line<'static> {
    let mut line = vec![Span::raw(" ".repeat(indent))];
    let mut used = 0usize;
    for mut span in spans {
        used = used.saturating_add(span.width());
        if span.style.bg.is_none() {
            span.style = span.style.bg(COMMENT_CARD_BG);
        }
        line.push(span);
    }
    // 补足到卡片宽度，其中包含右侧内边距。
    line.push(Span::styled(
        " ".repeat(card_w.saturating_sub(used)),
        Style::default().bg(COMMENT_CARD_BG),
    ));
    Line::from(line)
}

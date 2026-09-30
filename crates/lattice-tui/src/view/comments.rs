// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::blocks::BlockId;
use crate::comment::{Anchor, Comment, Location};

pub(super) struct Marker {
    row: usize,
    columns: std::ops::Range<usize>,
    comment: BlockId,
}
pub(crate) enum CommentNavigation {
    Jump(browse::BrowseCursor),
    Compose(Anchor),
}

impl Transcript {
    pub(crate) fn comment_navigation(
        &self,
        cursor: browse::BrowseCursor,
        width: u16,
    ) -> Option<CommentNavigation> {
        let col = cursor.column(&self.lines, width);
        if let Some(marker) = self
            .markers
            .iter()
            .find(|m| m.row == cursor.row && m.columns.contains(&col))
        {
            let row = self.owners.iter().position(|o| o.id == marker.comment)?;
            return self.cursor(row, 0).map(CommentNavigation::Jump);
        }
        // 草稿使用普通 BlockId，Enter 仍是编辑，即使被拒 USER 的标记指向它。
        if matches!(cursor.block.id, BlockId::Comment { .. })
            && let Some(marker) = self
                .markers
                .iter()
                .rev()
                .find(|m| m.comment == cursor.block.id)
        {
            return self
                .cursor(marker.row, marker.columns.start)
                .map(CommentNavigation::Jump);
        }
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
    pub(crate) fn editor_width(&self, width: u16) -> u16 {
        let Some((row, _)) = self.editor_cursor else {
            return width;
        };
        let indent = self
            .lines
            .get(row)
            .and_then(|l| l.spans.first())
            .map_or(0, |s| s.width());
        width.saturating_sub(u16::try_from(indent).unwrap_or(u16::MAX))
    }
}

pub(super) fn identified(
    comments: &[Comment],
    owner: BlockId,
) -> impl Iterator<Item = (BlockId, &Comment)> {
    let mut occurrences = std::collections::HashMap::<usize, usize>::new();
    comments.iter().map(move |comment| {
        let occurrence = occurrences.entry(comment.n).or_default();
        let id = owner.comment(comment.n, *occurrence);
        *occurrence = occurrence.saturating_add(1);
        (id, comment)
    })
}

pub(super) fn markers<'c>(
    lines: &mut Vec<Line<'static>>,
    markers: &mut Vec<Marker>,
    comments: impl IntoIterator<Item = (BlockId, &'c Comment)>,
    width: u16,
) {
    let mut spans = Vec::new();
    let mut col = 0usize;
    for (id, comment) in comments {
        let text = format!("[COMMENT {}]", comment.n);
        let len = text.width();
        if col > 0 && col.saturating_add(1).saturating_add(len) > usize::from(width) {
            lines.push(Line::from(std::mem::take(&mut spans)));
            col = 0;
        }
        if col > 0 {
            spans.push(Span::raw(" "));
            col = col.saturating_add(1);
        }
        markers.push(Marker {
            row: lines.len(),
            columns: col..col.saturating_add(len).min(usize::from(width)),
            comment: id,
        });
        spans.push(Span::styled(text, role_style(Role::User)));
        col = col.saturating_add(len);
    }
    if !spans.is_empty() {
        lines.push(Line::from(spans));
    }
}

struct PlacedComment<'a> {
    after: usize,
    indent: usize,
    id: BlockId,
    comment: &'a Comment,
    state: &'static str,
    editor: Option<&'a InputBuffer>,
}

pub(super) fn attach<'a>(app: &'a App, transcript: &mut Transcript, width: u16) {
    // 一次建立元素终点和 USER 终点索引，不按每条评论遍历全部显示行。
    let anchors: std::collections::HashMap<&Location, (usize, &Anchor, usize)> = transcript
        .anchors
        .iter()
        .map(|(r, a, i)| (a.location(), (*r, a, *i)))
        .collect();
    let ends: std::collections::HashMap<_, _> = transcript
        .owners
        .iter()
        .enumerate()
        .map(|(r, o)| (o.id, r))
        .collect();
    let mut placed = Vec::new();
    let mut highlighted = std::collections::HashSet::new();
    let mut place = |id, comment: &'a Comment, origin, fallback, editor| {
        let anchor = anchors
            .get(&comment.location)
            .filter(|(_, a, _)| a.matches(comment));
        let (after, indent, state) = if let Some((row, _, indent)) = anchor {
            highlighted.insert(&comment.location);
            (
                transcript
                    .ending_items
                    .get(&comment.location)
                    .copied()
                    .unwrap_or(*row),
                *indent,
                origin,
            )
        } else if let Some(owner) = fallback {
            let Some(row) = ends.get(&owner) else {
                return;
            };
            (*row, 0, "原文不匹配")
        } else {
            return;
        };
        placed.push(PlacedComment {
            after,
            indent,
            id,
            comment,
            state,
            editor,
        });
    };
    for entry in &app.blocks.entries {
        if let Block::CommentedUser(message) = &entry.block {
            for (id, comment) in identified(&message.comments, entry.id) {
                place(id, comment, "", Some(entry.id), None);
            }
        }
    }
    for message in app.queue.messages() {
        for (id, comment) in identified(&message.comments, message.id) {
            place(id, comment, "排队", Some(message.id), None);
        }
    }
    for draft in app.queue.drafts() {
        place(
            draft.id,
            draft.comment(),
            "草稿",
            None,
            app.queue.editor(draft.id),
        );
    }
    placed.sort_by_key(|p| (p.after, p.comment.n));
    let mut placed = placed.into_iter().peekable();
    let mut old_lines = std::mem::take(&mut transcript.lines);
    for (row, anchor, indent) in &transcript.anchors {
        if highlighted.contains(anchor.location())
            && let Some(line) = old_lines.get_mut(*row)
        {
            highlight(line, *indent);
        }
    }
    let old_owners = std::mem::take(&mut transcript.owners);
    let mut relocation = Vec::with_capacity(old_lines.len());
    let old_editor = transcript.editor_cursor.take();
    for (row, (line, owner)) in old_lines.into_iter().zip(old_owners).enumerate() {
        relocation.push(transcript.lines.len());
        transcript.lines.push(line);
        transcript.owners.push(owner);
        while placed.peek().is_some_and(|p| p.after == row) {
            let Some(comment) = placed.next() else {
                break;
            };
            render(transcript, comment, width);
        }
    }
    for (row, _, _) in &mut transcript.anchors {
        if let Some(mapped) = relocation.get(*row) {
            *row = *mapped;
        }
    }
    for marker in &mut transcript.markers {
        if let Some(mapped) = relocation.get(marker.row) {
            marker.row = *mapped;
        }
    }
    if let Some((row, col)) = old_editor {
        transcript.editor_cursor = relocation.get(row).map(|r| (*r, col));
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

fn render(transcript: &mut Transcript, placed: PlacedComment<'_>, width: u16) {
    let start = transcript.lines.len();
    let indent = placed.indent.min(usize::from(width));
    let body_indent = indent.saturating_add(2).min(usize::from(width));
    let body_width = width.saturating_sub(u16::try_from(body_indent).unwrap_or(u16::MAX));
    let mut right = Vec::new();
    if !placed.state.is_empty() {
        right.push(placed.state.into());
    }
    if placed.editor.is_some() {
        right.extend(["Enter 保存".into(), "Esc 放弃".into()]);
    }
    right.push(format!("#{}", placed.comment.n));
    transcript.lines.push(Line::default());
    let mut title = title_line(
        "COMMENT",
        Role::User,
        &right,
        usize::from(width).saturating_sub(indent),
    );
    title.spans.insert(0, Span::raw(" ".repeat(indent)));
    transcript.lines.push(title);
    let visual = if let Some(input) = placed.editor {
        let (visual, row, col) = input_visual(input, body_width);
        transcript.editor_cursor = Some((
            transcript.lines.len().saturating_add(row),
            body_indent.saturating_add(col),
        ));
        visual
            .into_iter()
            .map(|text| Line::from(Span::styled(text, role_style(Role::Assistant))))
            .collect::<Vec<_>>()
    } else {
        wrapping::wrap(
            vec![Span::styled(
                wrapping::expand_tabs(&placed.comment.text).into_owned(),
                role_style(Role::Assistant),
            )],
            usize::from(body_width),
        )
    };
    for mut line in visual {
        line.spans.insert(0, Span::raw(" ".repeat(body_indent)));
        transcript.lines.push(line);
    }
    transcript
        .owners
        .extend((start..transcript.lines.len()).map(|row| BlockPosition {
            id: placed.id,
            row: row.saturating_sub(start),
        }));
}

// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{App, Block, DockSeverity, StatusKind, ToolState, TurnPhase};
use crate::input::{CTRL_C_WINDOW, InputBuffer};
use crate::wrapping;

pub(crate) mod browse;
pub(crate) use browse::Focus;
mod comments;
pub(crate) use comments::CommentNavigation;
mod dock;
mod statusline;
pub(crate) use dock::{Dock, DockIntent};

pub(crate) const SIDEBAR_W: u16 = 40;
pub(crate) const SIDEBAR_ASPECT: u16 = 3;
pub(crate) const MIN_CONTENT_W_WITH_SIDEBAR: u16 = 72;
pub(crate) const GAP: u16 = 1;
const CTRL_C_HINT: &str = "Ctrl+C to quit";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Sidebar {
    Visible,
    Hidden,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FrameLayout {
    pub(crate) left_w: u16,
    pub(crate) content_w: u16,
    pub(crate) sidebar: Sidebar,
}

pub(crate) fn frame_layout(frame_w: u16, frame_h: u16) -> FrameLayout {
    let sidebar_content_w = frame_w.saturating_sub(SIDEBAR_W).saturating_sub(GAP);
    let sufficiently_landscape = frame_w >= frame_h.saturating_mul(SIDEBAR_ASPECT);
    let enough_conversation_width = sidebar_content_w >= MIN_CONTENT_W_WITH_SIDEBAR;
    if sufficiently_landscape && enough_conversation_width {
        FrameLayout {
            left_w: sidebar_content_w,
            content_w: sidebar_content_w,
            sidebar: Sidebar::Visible,
        }
    } else {
        FrameLayout {
            left_w: frame_w,
            content_w: frame_w,
            sidebar: Sidebar::Hidden,
        }
    }
}

pub(crate) struct Viewport {
    /// First visible transcript row.
    scroll: usize,
    /// While true, `scroll` is pinned to the bottom on every content change.
    follow: bool,
    /// 最近一次已知的对话记录行数与视口高度；statusline 据此算下方行数，
    /// 不再为一个提示重渲染整段对话记录。
    rows: usize,
    height: usize,
}

impl Viewport {
    pub(crate) fn new() -> Self {
        Self {
            scroll: 0,
            follow: true,
            rows: 0,
            height: 0,
        }
    }

    /// Content changed to `content_rows`; viewport height is `height`.
    pub(crate) fn on_content_changed(&mut self, content_rows: usize, height: usize) {
        if (self.rows, self.height) == (content_rows, height) {
            return;
        }
        let shrank = content_rows < self.rows || height != self.height;
        (self.rows, self.height) = (content_rows, height);
        let max = content_rows.saturating_sub(height);
        if self.follow {
            self.scroll = max;
        } else if shrank {
            self.scroll = self.scroll.min(max);
        }
    }

    /// 浏览时 Dock 占位/释放不等于用户滚动：视图不动，允许底部暂留空行。
    pub(crate) fn on_dock_changed(&mut self, content_rows: usize, height: usize) {
        (self.rows, self.height) = (content_rows, height);
    }

    pub(crate) fn page_up(&mut self, height: usize) {
        self.height = height;
        self.follow = false;
        self.scroll = self.scroll.saturating_sub(height.saturating_sub(1));
    }

    #[rustfmt::skip]
    pub(crate) fn page_down(&mut self, content_rows: usize, height: usize) {
        (self.rows, self.height) = (content_rows, height);
        if self.follow { return; }
        let max = content_rows.saturating_sub(height);
        self.scroll = self.scroll.saturating_add(height.saturating_sub(1)).min(max);
        if self.scroll >= max { self.follow = true; }
    }

    #[allow(clippy::wrong_self_convention)]
    pub(crate) fn to_bottom(&mut self, content_rows: usize, height: usize) {
        (self.rows, self.height) = (content_rows, height);
        self.follow = true;
        self.scroll = content_rows.saturating_sub(height);
    }

    pub(crate) fn scroll(&self) -> usize {
        self.scroll
    }

    pub(crate) fn following(&self) -> bool {
        self.follow
    }

    /// 视图下方尚未显示的对话记录行数。
    pub(crate) fn hidden_below(&self) -> usize {
        self.rows
            .saturating_sub(self.scroll.saturating_add(self.height))
    }
}

/// 合同中的六种界面前景角色；语法高亮另有配色。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Role {
    User,
    Assistant,
    ToolOk,
    ToolErr,
    Warn,
    DimMeta,
}

#[rustfmt::skip]
fn role_color(role: Role) -> Color {
    match role {
        Role::User => Color::Rgb(0x86, 0x95, 0xe3),
        Role::Assistant => Color::Rgb(0xf5, 0xf3, 0xef),
        Role::ToolOk => Color::Rgb(0x2a, 0xb0, 0xb0),
        Role::ToolErr => Color::Rgb(0xcb, 0x2b, 0x31),
        Role::Warn => Color::Rgb(0xd3, 0x89, 0x26),
        Role::DimMeta => Color::Rgb(0x85, 0x7f, 0x78),
    }
}

pub(crate) fn role_style(role: Role) -> Style {
    Style::default().fg(role_color(role))
}

fn status_surface_style() -> Style {
    role_style(Role::DimMeta).bg(Color::Rgb(0x2c, 0x42, 0x67))
}

/// 挂着草稿、排队或已发送评论的锚点元素底色（iris-12）。
pub(crate) const COMMENT_ANCHOR_BG: Color = Color::Rgb(0x1f, 0x25, 0x4b);
/// 评论卡片底色（warm-gray-12）。
pub(crate) const COMMENT_CARD_BG: Color = Color::Rgb(0x2b, 0x28, 0x25);
/// COMMENT 标签底色（iris-9）。
const COMMENT_TAG_BG: Color = Color::Rgb(0x59, 0x65, 0xca);

/// Status symbols.
#[rustfmt::skip]
pub(crate) fn symbol(state: SymbolKind) -> &'static str {
    match state {
        SymbolKind::Running => "·",
        SymbolKind::Ok => "✓",
        SymbolKind::Failed | SymbolKind::Cancelled => "✗",
    }
}

#[derive(Clone, Copy)]
pub(crate) enum SymbolKind {
    Running,
    Ok,
    Failed,
    Cancelled,
}

fn fold_symbol(expanded: bool) -> &'static str {
    match expanded {
        false => "▸",
        true => "▾",
    }
}

pub(crate) fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    app.term_width = area.width;
    app.term_height = area.height;
    let transcript = app.prepare_transcript();
    let geometry = frame_layout(area.width, area.height);
    match geometry.sidebar {
        Sidebar::Visible => {
            let columns = Layout::horizontal([
                Constraint::Length(geometry.left_w),
                Constraint::Length(GAP),
                Constraint::Length(SIDEBAR_W),
            ])
            .split(area);
            draw_left(frame, app, columns[0], geometry.content_w, &transcript);
            frame.render_widget(Paragraph::new("").style(status_surface_style()), columns[2]);
        }
        Sidebar::Hidden => {
            draw_left(frame, app, area, geometry.content_w, &transcript);
        }
    }
}

fn draw_left(frame: &mut Frame, app: &App, area: Rect, content_w: u16, transcript: &Transcript) {
    let dock = Dock::new(app);
    let bottom_h = 1.min(area.height);
    let above = area.height.saturating_sub(bottom_h);
    let dock_h = dock.height(content_w, transcript.lines.len()).min(above);
    let transcript_h = above.saturating_sub(dock_h);
    // 对话记录与 Dock 共享 statusline 之上的高度：内容不足时 Dock 紧跟最后一行，
    // 其下留空；填满后停在 statusline 之上。viewport 仍按可用高度 `transcript_h` 计算。
    let remaining = transcript.lines.len().saturating_sub(app.viewport.scroll());
    let shown = transcript_h.min(u16::try_from(remaining).unwrap_or(u16::MAX));

    let dock_y = area.y.saturating_add(shown);
    let bottom_y = area.y.saturating_add(above);
    draw_transcript(
        frame,
        app,
        Rect::new(area.x, area.y, area.width, shown),
        content_w,
        transcript,
    );
    dock.render(frame, Rect::new(area.x, dock_y, area.width, dock_h));
    if bottom_h > 0 {
        statusline::render(
            frame,
            app,
            Rect::new(area.x, bottom_y, area.width, bottom_h),
        );
    }
}

/// One source of truth for transcript rendering and viewport row counts.
pub(crate) struct Transcript {
    pub(crate) lines: Vec<Line<'static>>,
    pub(crate) owners: Vec<BlockPosition>,
    pub(crate) editor_cursor: Option<(usize, usize)>,
    /// 编辑器折行宽度；与 `editor_cursor` 同时设置。
    editor_width: Option<u16>,
    anchors: Vec<(usize, crate::comment::Anchor, usize)>,
    ending_items: std::collections::HashMap<crate::comment::Location, usize>,
    markers: Vec<comments::Marker>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BlockPosition {
    pub(crate) id: crate::blocks::BlockId,
    pub(crate) row: usize,
}

#[cfg(test)]
pub(crate) fn transcript_lines(app: &App, content_w: u16) -> Vec<Line<'static>> {
    render_transcript(app, content_w).lines
}

pub(crate) fn render_transcript(app: &App, content_w: u16) -> Transcript {
    let mut lines = Vec::new();
    let mut owners = Vec::new();
    let mut anchors = Vec::new();
    let mut ending_items = std::collections::HashMap::new();
    let mut markers = Vec::new();
    let mut cache = app.markdown_cache.borrow_mut();
    let mut assistant_header = false;
    let mut agent: Option<&str> = None;
    let mut turn = None;
    for entry in &app.blocks.entries {
        let block = &entry.block;
        let start = lines.len();
        match block {
            Block::UserTurn(id, ordinal) => turn = Some((*id, *ordinal)),
            Block::Step(step) => {
                if turn.is_some_and(|(id, _)| id != step.turn_id) {
                    turn = None;
                }
            }
            Block::AgentName(name) => agent = Some(name),
            Block::User(text) => {
                transcript_title(
                    &mut lines,
                    "USER",
                    Role::User,
                    turn.map(|(_, n)| n),
                    content_w,
                );
                lines.extend(
                    cache
                        .render(text, content_w)
                        .iter()
                        .map(|row| row.line.clone()),
                );
                assistant_header = false;
            }
            Block::CommentedUser(message) | Block::RejectedUser(message) => {
                transcript_title(
                    &mut lines,
                    "USER",
                    Role::User,
                    turn.map(|(_, n)| n),
                    content_w,
                );
                if matches!(block, Block::RejectedUser(_)) {
                    let targets = message.comments.iter().map(|comment| {
                        let id = app
                            .queue
                            .pending_comment_id(comment.n)
                            .unwrap_or_else(|| entry.id.comment(comment.n, 0));
                        (id, comment)
                    });
                    comments::markers(&mut lines, &mut markers, targets, content_w);
                } else {
                    comments::markers(
                        &mut lines,
                        &mut markers,
                        comments::identified(&message.comments, entry.id),
                        content_w,
                    );
                }
                if !message.text.is_empty() {
                    lines.extend(
                        cache
                            .render(&message.text, content_w)
                            .iter()
                            .map(|row| row.line.clone()),
                    );
                }
                assistant_header = false;
            }
            Block::HistoryAssistant(text) => {
                if !text.text.is_empty() {
                    ensure_assistant_header(
                        &mut lines,
                        &mut assistant_header,
                        agent,
                        turn.map(|(_, n)| n),
                        content_w,
                    );
                    let rendered = cache.render(&text.text, content_w);
                    if app.turn_closed(text.fragment.turn) {
                        for (index, row) in rendered.iter().enumerate() {
                            let at = lines.len().saturating_add(index);
                            if let Some(element) = &row.element
                                && let Some(anchor) = text.anchor(element.range.clone())
                            {
                                anchors.push((at, anchor, element.indent));
                            }
                            for range in &row.ending_items {
                                ending_items.insert(
                                    crate::comment::Location {
                                        fragment: text.fragment.clone(),
                                        range: range.clone(),
                                    },
                                    at,
                                );
                            }
                        }
                    }
                    lines.extend(rendered.iter().map(|row| row.line.clone()));
                }
            }
            Block::AssistantStart {
                turn_id,
                agent: name,
            } => {
                if turn.is_some_and(|(id, _)| id == *turn_id) {
                    agent = Some(name);
                    ensure_assistant_header(
                        &mut lines,
                        &mut assistant_header,
                        agent,
                        turn.map(|(_, n)| n),
                        content_w,
                    );
                }
            }
            Block::Assistant(text) if text.is_empty() => {}
            Block::Assistant(text) => {
                ensure_assistant_header(
                    &mut lines,
                    &mut assistant_header,
                    agent,
                    turn.map(|(_, n)| n),
                    content_w,
                );
                lines.extend(
                    cache
                        .render(text, content_w)
                        .iter()
                        .map(|row| row.line.clone()),
                );
            }
            Block::Tool { name, state, .. } => {
                ensure_assistant_header(
                    &mut lines,
                    &mut assistant_header,
                    agent,
                    turn.map(|(_, n)| n),
                    content_w,
                );
                lines.extend(tool_lines(name, state, content_w));
            }
            Block::Status { kind, placement } => {
                // 暂驻 Dock 的 Warn / Error 不重复出现在 transcript；释放后
                // 会在原本的时间位置出现。行数统计与 viewport 同用过滤结果。
                if matches!(placement, crate::app::StatusPlacement::Dock(_)) {
                    continue;
                }
                match kind {
                    StatusKind::Notice(notice) => {
                        for text in notice {
                            lines.extend(body(text, content_w, role_style(Role::DimMeta)));
                        }
                    }
                    StatusKind::Summary { turn_id, .. }
                        if turn.is_none_or(|(id, _)| id != *turn_id) =>
                    {
                        lines.extend(body(
                            &status_text(kind),
                            content_w,
                            role_style(Role::DimMeta),
                        ));
                    }
                    kind => {
                        ensure_assistant_header(
                            &mut lines,
                            &mut assistant_header,
                            agent,
                            turn.map(|(_, n)| n),
                            content_w,
                        );
                        lines.extend(status_lines(kind, content_w));
                    }
                }
            }
        }
        owners.extend((start..lines.len()).map(|row| BlockPosition {
            id: entry.id,
            row: row.saturating_sub(start),
        }));
    }
    let mut editor_cursor = None;
    for message in app.queue.messages() {
        let start = lines.len();
        separate(&mut lines);
        let mut right = vec![
            if app.queue.is_paused() {
                "暂停"
            } else {
                "排队"
            }
            .into(),
        ];
        if app.queue.editor(message.id).is_some() {
            right.push("Enter 完成编辑".into());
        } else if matches!(app.focus, Focus::Transcript(cursor) if cursor.block.id == message.id) {
            right.extend(["Enter 编辑".into(), "Delete 删除".into()]);
        }
        lines.push(title_line(
            "USER",
            Role::User,
            &right,
            usize::from(content_w),
        ));
        comments::markers(
            &mut lines,
            &mut markers,
            comments::identified(&message.comments, message.id),
            content_w,
        );
        if let Some(editor) = app.queue.editor(message.id) {
            let (visual, row, col) = input_visual(editor, content_w);
            editor_cursor = Some((lines.len().saturating_add(row), col));
            lines.extend(
                visual
                    .into_iter()
                    .map(|text| Line::from(Span::styled(text, role_style(Role::Assistant)))),
            );
        } else {
            lines.extend(body(&message.text, content_w, role_style(Role::DimMeta)));
        }
        owners.extend((start..lines.len()).map(|row| BlockPosition {
            id: message.id,
            row: row.saturating_sub(start),
        }));
    }
    cache.finish_render();
    let mut transcript = Transcript {
        lines,
        owners,
        editor_width: editor_cursor.map(|_| content_w),
        editor_cursor,
        anchors,
        ending_items,
        markers,
    };
    comments::attach(app, &mut transcript, content_w);
    transcript
}

fn separate(lines: &mut Vec<Line<'static>>) {
    let last_has_text = lines.last().is_some_and(|line| {
        line.spans
            .iter()
            .any(|span| !span.content.trim().is_empty())
    });
    if last_has_text {
        lines.push(Line::default());
    }
}

fn transcript_title(
    lines: &mut Vec<Line<'static>>,
    title: &str,
    role: Role,
    ordinal: Option<std::num::NonZeroUsize>,
    content_w: u16,
) {
    separate(lines);
    let right = ordinal
        .map(|n| format!("#{n}"))
        .into_iter()
        .collect::<Vec<_>>();
    lines.push(title_line(title, role, &right, usize::from(content_w)));
}

fn ensure_assistant_header(
    lines: &mut Vec<Line<'static>>,
    shown: &mut bool,
    agent: Option<&str>,
    ordinal: Option<std::num::NonZeroUsize>,
    content_w: u16,
) {
    if !*shown && let (Some(agent), Some(ordinal)) = (agent, ordinal) {
        transcript_title(lines, agent, Role::Assistant, Some(ordinal), content_w);
        *shown = true;
    }
}

fn body(text: &str, content_w: u16, style: Style) -> Vec<Line<'static>> {
    wrapping::wrap(
        vec![Span::styled(
            wrapping::expand_tabs(text).into_owned(),
            style,
        )],
        usize::from(content_w),
    )
}

/// 折叠指示占工具行前两列；工具行的续行与错误详情对齐到指示之后。
fn tool_heading(spans: Vec<Span<'static>>, expanded: bool, content_w: u16) -> Vec<Line<'static>> {
    let fold = Span::styled(
        format!("{} ", fold_symbol(expanded)),
        role_style(Role::DimMeta),
    );
    hanging(
        wrapping::wrap(spans, usize::from(content_w.saturating_sub(2))),
        Some(fold),
    )
}

fn hanging(lines: Vec<Line<'static>>, first: Option<Span<'static>>) -> Vec<Line<'static>> {
    let mut first = first;
    lines
        .into_iter()
        .map(|mut line| {
            line.spans
                .insert(0, first.take().unwrap_or_else(|| Span::raw("  ")));
            line
        })
        .collect()
}

fn tool_lines(name: &str, state: &ToolState, content_w: u16) -> Vec<Line<'static>> {
    let dim = role_style(Role::DimMeta);
    let err = role_style(Role::ToolErr);
    match state {
        ToolState::Running => body(
            &format!("{name} · running {}", symbol(SymbolKind::Running)),
            content_w,
            dim,
        ),
        ToolState::Unknown => body(&format!("{name} · 观察结果未知"), content_w, dim),
        ToolState::Unavailable { outcome } => body(
            &format!("{name} · {outcome}；结果材料不可用"),
            content_w,
            dim,
        ),
        ToolState::Ok { output_len } => tool_heading(
            vec![
                Span::styled(symbol(SymbolKind::Ok), role_style(Role::ToolOk)),
                Span::styled(format!(" {name} · {output_len} B"), dim),
            ],
            false,
            content_w,
        ),
        ToolState::Failed { error, .. } => {
            let mut lines = tool_heading(
                vec![
                    Span::styled(symbol(SymbolKind::Failed), err),
                    Span::styled(format!(" {name}"), dim),
                ],
                true,
                content_w,
            );
            if let Some(error) = error {
                lines.extend(hanging(body(error, content_w.saturating_sub(2), err), None));
            }
            lines
        }
    }
}

/// 状态文本，transcript 与 Dock 共用同一措辞；样式由各自渲染层决定。
#[rustfmt::skip]
fn status_text(kind: &StatusKind) -> String {
    match kind {
        StatusKind::Summary { turn_id, summary } => format!("{} turn {turn_id} {}{}",
            symbol(match summary.outcome { lattice_api::local::PublicTurnOutcome::Completed => SymbolKind::Ok, lattice_api::local::PublicTurnOutcome::Failed => SymbolKind::Failed, lattice_api::local::PublicTurnOutcome::Cancelled => SymbolKind::Cancelled }),
            match summary.outcome { lattice_api::local::PublicTurnOutcome::Completed => "completed", lattice_api::local::PublicTurnOutcome::Failed => "failed", lattice_api::local::PublicTurnOutcome::Cancelled => "cancelled" },
            summary.notice.as_ref().map_or_else(String::new, |notice| format!("：{}", notice.text))),
        StatusKind::TurnFailed { reason } => format!(
            "{} {reason}", symbol(SymbolKind::Failed)
        ),
        StatusKind::LlmRetry { attempt, delay_ms, message } => format!(
            "{} llm retry {attempt} after {delay_ms}ms: {message}",
            symbol(SymbolKind::Running)
        ),
        StatusKind::Notice(_) => String::new(),
    }
}

fn status_lines(kind: &StatusKind, content_w: u16) -> Vec<Line<'static>> {
    let style = match kind {
        StatusKind::Summary { summary, .. } => {
            role_style(match summary.notice.as_ref().map(|n| &n.level) {
                Some(lattice_api::local::NoticeLevel::Error) => Role::ToolErr,
                Some(lattice_api::local::NoticeLevel::Info) => Role::DimMeta,
                Some(lattice_api::local::NoticeLevel::Warn) => Role::Assistant,
                None => Role::ToolOk,
            })
        }
        StatusKind::TurnFailed { .. } => role_style(Role::ToolErr),
        StatusKind::LlmRetry { .. } => role_style(Role::DimMeta),
        StatusKind::Notice(_) => return Vec::new(),
    };
    body(&status_text(kind), content_w, style)
}

/// 同一投影同时提供折行文字、光标与视觉行移动的源字符位置。
struct InputVisual {
    lines: Vec<String>,
    positions: Vec<((usize, usize), (usize, usize))>,
    cursor: (usize, usize),
}

impl InputVisual {
    fn new(input: &InputBuffer, content_w: u16) -> Self {
        let width = usize::from(content_w.max(1));
        let mut visual = Self {
            lines: Vec::new(),
            positions: Vec::new(),
            cursor: (0, 0),
        };
        for (source_row, line) in input.lines.iter().enumerate() {
            visual.lines.push(String::new());
            let mut col = 0usize;
            let chars: Vec<_> = line.chars().collect();
            for (source_col, ch) in chars.iter().copied().enumerate() {
                let cells = UnicodeWidthChar::width(ch).unwrap_or(0);
                if col > 0 && col.saturating_add(cells) > width {
                    visual.lines.push(String::new());
                    col = 0;
                }
                visual.record((source_row, source_col), col, input.cursor);
                visual
                    .lines
                    .last_mut()
                    .expect("current visual row")
                    .push(ch);
                col = col.saturating_add(cells);
            }
            // 行尾占满时为插入光标保留下一行，与绘制和移动使用同一映射。
            if col >= width {
                visual.lines.push(String::new());
                col = 0;
            }
            visual.record((source_row, chars.len()), col, input.cursor);
        }
        visual
    }

    fn record(&mut self, source: (usize, usize), col: usize, cursor: (usize, usize)) {
        let position = (self.lines.len().saturating_sub(1), col);
        self.positions.push((source, position));
        if source == cursor {
            self.cursor = position;
        }
    }
}

fn input_visual(input: &InputBuffer, content_w: u16) -> (Vec<String>, usize, usize) {
    let visual = InputVisual::new(input, content_w);
    (visual.lines, visual.cursor.0, visual.cursor.1)
}

pub(crate) fn input_cursor(input: &InputBuffer, content_w: u16) -> (usize, usize) {
    InputVisual::new(input, content_w).cursor
}

pub(crate) fn move_input_vertical(input: &mut InputBuffer, content_w: u16, down: bool) {
    let visual = InputVisual::new(input, content_w);
    let (row, col) = visual.cursor;
    let target = if down {
        row.checked_add(1)
    } else {
        row.checked_sub(1)
    };
    let Some(target) = target.filter(|target| *target < visual.lines.len()) else {
        return;
    };
    if let Some((source, _)) = visual
        .positions
        .iter()
        .rev()
        .find(|(_, (r, c))| *r == target && *c <= col)
    {
        input.cursor = *source;
    }
}

fn draw_transcript(
    frame: &mut Frame,
    app: &App,
    area: Rect,
    content_w: u16,
    transcript: &Transcript,
) {
    let lines = &transcript.lines;
    let start = app.viewport.scroll().min(lines.len());
    let end = start
        .saturating_add(usize::from(area.height))
        .min(lines.len());
    frame.render_widget(Paragraph::new(lines[start..end].to_vec()), area);
    let cursor = transcript.editor_cursor.or_else(|| match &app.focus {
        Focus::Transcript(cursor) => Some((cursor.row, cursor.column(lines, content_w))),
        Focus::Dock => None,
    });
    if let Some((row, col)) = cursor
        && (start..end).contains(&row)
        && content_w > 0
    {
        let x = u16::try_from(col).unwrap_or(u16::MAX);
        let y = row.saturating_sub(start) as u16;
        if x < area.width {
            frame.set_cursor_position((area.x.saturating_add(x), area.y.saturating_add(y)));
        }
    }
}

fn joined_width<'a>(parts: impl Iterator<Item = &'a str>) -> usize {
    let parts = parts.collect::<Vec<_>>();
    parts
        .iter()
        .fold(0usize, |width, part| {
            width.saturating_add(display_width(part))
        })
        .saturating_add(parts.len().saturating_sub(1).saturating_mul(3))
}

fn phase_and_connection(app: &App) -> String {
    let phase = phase_text(app.phase);
    if app.connected {
        phase.into()
    } else {
        format!("{phase} · offline")
    }
}

fn phase_text(phase: TurnPhase) -> &'static str {
    match phase {
        TurnPhase::Idle => "idle",
        TurnPhase::Streaming => "stream",
        TurnPhase::Cancelling => "cancel",
    }
}

fn phase_role(phase: TurnPhase) -> Role {
    match phase {
        TurnPhase::Streaming => Role::User,
        TurnPhase::Cancelling => Role::ToolErr,
        TurnPhase::Idle => Role::DimMeta,
    }
}

fn ctrl_c_visible(app: &App) -> bool {
    app.last_ctrl_c
        .is_some_and(|then| then.elapsed() <= CTRL_C_WINDOW)
}

fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

/// 标题线从左列起点画到 `width`。
pub(crate) fn title_line(title: &str, role: Role, right: &[String], width: usize) -> Line<'static> {
    let dim = role_style(Role::DimMeta);
    if width < 5 {
        return Line::from(Span::styled("─".repeat(width), dim));
    }
    let mut right = right;
    while !right.is_empty()
        && 9usize
            .saturating_add(display_width(title))
            .saturating_add(joined_width(right.iter().map(String::as_str)))
            > width
    {
        right = &right[1..];
    }
    let title = truncate_display(title, width.saturating_sub(5));
    let mut spans = vec![
        Span::styled("── ", dim),
        Span::styled(
            title.clone(),
            role_style(role).add_modifier(ratatui::style::Modifier::BOLD),
        ),
        Span::styled(" ", dim),
    ];
    let remaining = width.saturating_sub(4usize.saturating_add(display_width(&title)));
    if right.is_empty() {
        spans.push(Span::styled("─".repeat(remaining), dim));
    } else {
        let right = right.join(" · ");
        spans.push(Span::styled(
            "─".repeat(remaining.saturating_sub(display_width(&right).saturating_add(4))),
            dim,
        ));
        spans.push(Span::styled(format!(" {right} ──"), dim));
    }
    Line::from(spans)
}

fn truncate_display(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let mut used = 0usize;
    text.chars()
        .take_while(|ch| {
            let next = used.saturating_add(UnicodeWidthChar::width(*ch).unwrap_or(0));
            if next > width {
                false
            } else {
                used = next;
                true
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{App, Block, StatusKind, StatusPlacement};
    use crate::session::SessionContext;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    pub(super) fn test_app() -> App {
        App::new(
            SessionContext {
                session_id: None,
                active_agent: "agent".into(),
            },
            false,
            std::path::Path::new("/nonexistent-tui-test-history"),
        )
    }

    #[test]
    fn layout_boundaries_are_underflow_safe() {
        for (w, h) in [
            (1, 1),
            (7, 3),
            (43, 10),
            (44, 10),
            (112, 37),
            (113, 37),
            (113, 38),
        ] {
            let _ = frame_layout(w, h);
        }
        assert_eq!(
            frame_layout(1, 1),
            FrameLayout {
                left_w: 1,
                content_w: 1,
                sidebar: Sidebar::Hidden,
            }
        );
        // 正文宽 = w - 41：112x37 正文 71 列，刚好不足；113x37 比例与正文宽度同时达标；
        // 113x38 正文够宽但比例不足 3:1。
        assert_eq!(frame_layout(112, 37).sidebar, Sidebar::Hidden);
        assert_eq!(frame_layout(113, 37).sidebar, Sidebar::Visible);
        assert_eq!(frame_layout(113, 38).sidebar, Sidebar::Hidden);
    }

    #[test]
    fn all_frame_shapes_draw_without_panic() -> anyhow::Result<()> {
        for (width, height) in [
            (0, 0),
            (0, 1),
            (1, 0),
            (1, 1),
            (7, 3),
            (56, 47),
            (80, 24),
            (115, 47),
            (176, 50),
        ] {
            let mut app = test_app();
            app.term_width = width;
            app.term_height = height;
            app.blocks.push(Block::Status {
                kind: StatusKind::Notice(vec!["hello".into()]),
                placement: StatusPlacement::Transcript,
            });
            app.input.set_text("typed");
            let mut terminal = Terminal::new(TestBackend::new(width, height))?;
            terminal.draw(|frame| draw(frame, &mut app))?;
        }
        Ok(())
    }

    #[test]
    fn dock_status_frames_draw_without_panic() -> anyhow::Result<()> {
        use crate::app::DockSeverity;
        for (width, height) in [(1, 1), (7, 3), (80, 24), (176, 50)] {
            for severity in [DockSeverity::Warn, DockSeverity::Error] {
                let mut app = test_app();
                app.blocks.push(Block::Status {
                    kind: match severity {
                        DockSeverity::Warn => StatusKind::LlmRetry {
                            attempt: 2,
                            delay_ms: 250,
                            message: "retrying model request".into(),
                        },
                        DockSeverity::Error => StatusKind::TurnFailed {
                            reason: "permission denied".into(),
                        },
                    },
                    placement: StatusPlacement::Dock(severity),
                });
                app.input.set_text("draft");
                let mut terminal = Terminal::new(TestBackend::new(width, height))?;
                terminal.draw(|frame| draw(frame, &mut app))?;
            }
        }
        Ok(())
    }
}

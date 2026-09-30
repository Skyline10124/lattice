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
            right.extend(["Enter 编辑".into(), "Esc 删除".into()]);
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
        StatusKind::LlmFailed { attempt, message } => format!(
            "{} llm failed {attempt}: {message}",
            symbol(SymbolKind::Failed)
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
                None => Role::ToolOk,
            })
        }
        StatusKind::TurnFailed { .. } | StatusKind::LlmFailed { .. } => role_style(Role::ToolErr),
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
    use crate::app::{App, Block, StatusKind, StatusPlacement, ToolState};
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

    fn buffer_text(terminal: &Terminal<TestBackend>, width: u16, height: u16) -> String {
        (0..height)
            .map(|y| {
                let mut row = String::new();
                let mut x = 0;
                while x < width {
                    let symbol = terminal.backend().buffer()[(x, y)].symbol();
                    row.push_str(symbol);
                    x += display_width(symbol).max(1) as u16;
                }
                row
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn projection_owns_rows_and_reuses_only_unchanged_markdown_bodies() {
        use std::rc::Rc;
        let mut app = test_app();
        let user = "# question\n\n```rust\nlet x = 1;\n```";
        app.blocks.push(Block::User(user.into()));
        app.blocks.push(Block::Assistant("answer".into()));
        app.blocks.push(Block::Status {
            kind: StatusKind::Notice(vec!["notice".into()]),
            placement: StatusPlacement::Transcript,
        });
        app.blocks.push(Block::Status {
            kind: StatusKind::LlmFailed {
                attempt: 1,
                message: "dock".into(),
            },
            placement: StatusPlacement::Dock(DockSeverity::Error),
        });
        let transcript = app.prepare_transcript();
        assert_eq!(transcript.lines.len(), transcript.owners.len());
        assert_eq!(
            transcript.owners.first().unwrap().id,
            app.blocks.entries[0].id
        );
        assert_eq!(
            transcript.owners.last().unwrap().id,
            app.blocks.entries[2].id
        );
        assert!(
            !transcript
                .owners
                .iter()
                .any(|p| p.id == app.blocks.entries[3].id)
        );
        for (index, owner) in transcript.owners.iter().enumerate() {
            if index == 0 || transcript.owners[index - 1].id != owner.id {
                assert_eq!(owner.row, 0);
            } else {
                assert_eq!(owner.row, transcript.owners[index - 1].row + 1);
            }
        }
        let user_lines = app.markdown_cache.borrow_mut().render(user, 80);
        let answer = app.markdown_cache.borrow_mut().render("answer", 80);
        let old_answer = Rc::downgrade(&answer);
        drop(answer);
        app.prepare_transcript(); // complete frame resets marks set by inspection
        app.blocks[1] = Block::Assistant("answer\nstreamed **suffix**".into());
        let cached = app.prepare_transcript();
        assert!(Rc::ptr_eq(
            &user_lines,
            &app.markdown_cache.borrow_mut().render(user, 80)
        ));
        assert!(old_answer.upgrade().is_none());
        *app.markdown_cache.borrow_mut() = Default::default();
        let fresh = app.prepare_transcript();
        assert_eq!(cached.lines, fresh.lines);
        assert_eq!(cached.owners, fresh.owners);
    }

    #[test]
    fn browse_cursor_uses_terminal_cursor_and_hides_input_hint() -> anyhow::Result<()> {
        use ratatui::backend::Backend;
        let mut app = test_app();
        app.blocks.push(Block::Assistant("abc你".into()));
        app.input.set_text("abc");
        app.apply_action(crate::input::KeyAction::Up);
        let mut terminal = Terminal::new(TestBackend::new(80, 24))?;
        terminal.draw(|frame| draw(frame, &mut app))?;
        assert_eq!(terminal.backend_mut().get_cursor_position()?, (3, 0).into());
        let text = buffer_text(&terminal, 80, 24);
        assert!(text.contains("↓ 0 行"));
        assert!(!text.contains("Alt+Enter"));
        Ok(())
    }

    #[test]
    fn left_column_has_statusline_with_or_without_empty_full_height_sidebar() -> anyhow::Result<()>
    {
        for (w, h) in [(80, 24), (176, 50), (1, 1), (7, 3)] {
            let mut app = test_app();
            app.term_width = w;
            app.term_height = h;
            let geometry = frame_layout(w, h);
            let mut terminal = Terminal::new(TestBackend::new(w, h))?;
            terminal.draw(|f| draw(f, &mut app))?;
            // 宽字符尾格不发送到终端；背景由字符起始格覆盖整个 glyph。
            let mut x = 0;
            while x < geometry.left_w {
                let cell = &terminal.backend().buffer()[(x, h - 1)];
                assert_eq!(cell.bg, Color::Rgb(0x2c, 0x42, 0x67));
                x += display_width(cell.symbol()).max(1) as u16;
            }
            if geometry.sidebar == Sidebar::Visible {
                for y in 0..h {
                    assert_eq!(
                        terminal.backend().buffer()[(geometry.left_w, y)].symbol(),
                        " "
                    );
                    for x in geometry.left_w + GAP..w {
                        let cell = &terminal.backend().buffer()[(x, y)];
                        assert_eq!(cell.symbol(), " ");
                        assert_eq!(cell.bg, Color::Rgb(0x2c, 0x42, 0x67));
                    }
                }
            }
        }
        Ok(())
    }

    #[test]
    fn title_line_omits_leftmost_status_before_truncating_title() {
        let line = title_line("USER", Role::User, &["↑ 2 行".into(), "↓ 3 行".into()], 30);
        assert_eq!(line.to_string(), "── USER ─── ↑ 2 行 · ↓ 3 行 ──");
        assert_eq!(
            title_line("USER", Role::User, &["↑ 2 行".into(), "↓ 3 行".into()], 19).to_string(),
            "── USER ─ ↓ 3 行 ──"
        );
        assert_eq!(
            title_line("USER", Role::User, &["too long".into()], 9).to_string(),
            "── USER ─"
        );
        assert_eq!(
            title_line("中文标题", Role::User, &[], 7).to_string(),
            "── 中 ─"
        );
        assert_eq!(line.width(), 30);
        assert_eq!(
            line.spans[1].style,
            role_style(Role::User).add_modifier(ratatui::style::Modifier::BOLD)
        );
        for width in 0..8 {
            assert!(title_line("中文标题", Role::User, &["right".into()], width).width() <= width);
        }
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
    fn notices_and_unrelated_outcomes_do_not_start_or_reset_assistant_headers() {
        let mut app = test_app();
        let id = lattice_api::local::new_turn_id();
        let other = lattice_api::local::new_turn_id();
        app.blocks = vec![
            Block::AgentName("agent".into()),
            Block::UserTurn(id, std::num::NonZeroUsize::MIN),
            Block::User("question".into()),
            Block::Assistant(String::new()),
            Block::Status {
                kind: StatusKind::Notice(vec!["before".into()]),
                placement: StatusPlacement::Transcript,
            },
            Block::Status {
                kind: StatusKind::Summary {
                    turn_id: other,
                    summary: lattice_api::TurnSummary {
                        outcome: lattice_api::local::PublicTurnOutcome::Completed,
                        notice: None,
                    },
                },
                placement: StatusPlacement::Transcript,
            },
            Block::AssistantStart {
                turn_id: id,
                agent: "agent".into(),
            },
            Block::Status {
                kind: StatusKind::Notice(vec!["after".into()]),
                placement: StatusPlacement::Transcript,
            },
            Block::Assistant("answer".into()),
        ]
        .into();
        let lines = transcript_lines(&app, 80);
        let text: Vec<_> = lines.iter().map(ToString::to_string).collect();
        let title = text
            .iter()
            .position(|line| line.contains("── agent ─"))
            .unwrap();
        let summary = text
            .iter()
            .position(|line| line.contains("completed"))
            .unwrap();
        assert!(summary < title);
        assert_eq!(
            text.iter()
                .filter(|line| line.contains("── agent ─"))
                .count(),
            1
        );
        assert_eq!(text[title + 1].trim(), "after");
        assert_eq!(text[title + 2].trim(), "answer");
        assert_eq!(
            lines[title].spans[1].style,
            role_style(Role::Assistant).add_modifier(ratatui::style::Modifier::BOLD)
        );
        for (width, height) in [(0, 0), (1, 1), (2, 2), (6, 3)] {
            let lines = transcript_lines(&app, width);
            assert_eq!(lines.len(), app.content_rows(width));
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        }
    }

    #[test]
    fn transcript_groups_one_agent_header_and_shares_row_count() {
        let mut app = test_app();
        let id = lattice_api::local::new_turn_id();
        app.blocks.extend([
            Block::AgentName("agent".into()),
            Block::UserTurn(id, std::num::NonZeroUsize::MIN),
            Block::User("question".into()),
            Block::Assistant("before tool".into()),
            Block::Tool {
                id: "opaque-id".into(),
                name: "observe.workspace".into(),
                state: ToolState::Ok { output_len: 1204 },
                source: crate::app::ToolSource::HistoryResult,
            },
            Block::Assistant("after tool".into()),
            Block::Status {
                kind: StatusKind::Summary {
                    turn_id: id,
                    summary: lattice_api::TurnSummary {
                        outcome: lattice_api::local::PublicTurnOutcome::Completed,
                        notice: None,
                    },
                },
                placement: StatusPlacement::Transcript,
            },
            Block::UserTurn(
                lattice_api::local::new_turn_id(),
                std::num::NonZeroUsize::new(2).unwrap(),
            ),
            Block::User("next".into()),
        ]);

        let lines = transcript_lines(&app, 61);
        let text = lines.iter().map(ToString::to_string).collect::<Vec<_>>();
        assert_eq!(
            text.iter()
                .filter(|line| line.contains("── agent ─") && line.ends_with(" #1 ──"))
                .count(),
            1
        );
        assert!(text.iter().any(|line| line.contains("1204 B")));
        assert!(!text.iter().any(|line| line.contains("opaque-id")));
        assert_eq!(lines.len(), app.content_rows(61));
        let Some(tool) = text
            .iter()
            .position(|line| line.contains("observe.workspace"))
        else {
            panic!("tool line missing");
        };
        assert_eq!(text[tool - 1].trim(), "before tool");
        assert_eq!(text[tool + 1].trim(), "after tool");
    }

    #[test]
    fn dock_status_is_hidden_from_transcript_until_released() {
        let mut app = test_app();
        app.blocks.push(Block::Status {
            kind: StatusKind::TurnFailed {
                reason: "boom".into(),
            },
            placement: StatusPlacement::Dock(crate::app::DockSeverity::Error),
        });
        let text = transcript_lines(&app, 61)
            .iter()
            .map(ToString::to_string)
            .collect::<String>();
        assert!(
            !text.contains("boom"),
            "暂驻 Dock 的状态不出现在 transcript"
        );

        for block in app.blocks.iter_mut() {
            if let Block::Status { placement, .. } = block {
                *placement = StatusPlacement::Transcript;
            }
        }
        let text = transcript_lines(&app, 61)
            .iter()
            .map(ToString::to_string)
            .collect::<String>();
        assert!(text.contains("boom"), "释放后状态回到 transcript");
    }

    #[test]
    fn failed_tool_expands_error_and_success_uses_bytes() {
        let mut app = test_app();
        app.blocks.extend([
            Block::Tool {
                id: "ok".into(),
                name: "observe.workspace".into(),
                state: ToolState::Ok { output_len: 1204 },
                source: crate::app::ToolSource::HistoryResult,
            },
            Block::Tool {
                id: "failed".into(),
                name: "mutate.edit".into(),
                source: crate::app::ToolSource::HistoryResult,
                state: ToolState::Failed {
                    output_len: 0,
                    error: Some("expected_hash mismatch".into()),
                },
            },
        ]);
        let text = transcript_lines(&app, 61)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("1204 B"));
        assert!(!text.contains("chars"));
        assert!(text.contains("expected_hash mismatch"));
    }

    #[test]
    fn dock_compression_preserves_content_before_title_and_spacer() -> anyhow::Result<()> {
        for (height, expected) in [
            (1, vec!["draft"]),
            (2, vec!["── USER", "draft"]),
            (3, vec!["", "── USER", "draft"]),
        ] {
            let mut app = test_app();
            app.input.set_text("draft");
            let mut terminal = Terminal::new(TestBackend::new(40, height))?;
            terminal.draw(|f| Dock::new(&app).render(f, f.area()))?;
            let screen = buffer_text(&terminal, 40, height);
            for (row, prefix) in screen.lines().zip(expected) {
                if prefix.is_empty() {
                    assert!(row.trim().is_empty());
                } else {
                    assert!(row.starts_with(prefix), "{screen}");
                }
            }
            assert_eq!(
                terminal.get_cursor_position()?,
                ratatui::layout::Position::new(5, height - 1)
            );
        }
        Ok(())
    }

    #[test]
    fn dock_height_grows_into_space_left_by_transcript() {
        let mut app = test_app();
        assert_eq!(Dock::new(&app).height(10, 0), 3, "空输入：空行、标题、内容");
        app.input.set_text("a\nb\nc");
        assert_eq!(Dock::new(&app).height(10, 0), 5);
        app.input.set_text(&["x"; 30].join("\n"));
        // 24 行终端 statusline 之上 23 行：上限取对话记录留下的空间与 11 中的较大者。
        for (rows, expected) in [(0, 23), (4, 19), (12, 11), (100, 11)] {
            assert_eq!(Dock::new(&app).height(10, rows), expected, "rows={rows}");
        }
        for (height, half, all) in [
            (0, 0, 0),
            (1, 0, 0),
            (2, 0, 1),
            (3, 1, 2),
            (4, 1, 3),
            (5, 2, 4),
            (6, 2, 5),
            (7, 3, 6),
        ] {
            app.term_height = height;
            assert_eq!(Dock::new(&app).height(10, 100), half, "frame_h={height}");
            assert_eq!(Dock::new(&app).height(10, 0), all, "frame_h={height}");
        }
    }

    #[test]
    fn dock_status_height_follows_wrapped_rows() {
        let retry = |message: String| StatusKind::LlmRetry {
            attempt: 1,
            delay_ms: 0,
            message,
        };
        // 超长单词按字符拆分；状态的计算高度与绘制共用正文折行结果。
        let mut app = test_app();
        app.blocks.push(Block::Status {
            kind: retry("x".repeat(40)),
            placement: StatusPlacement::Dock(crate::app::DockSeverity::Warn),
        });
        assert_eq!(Dock::new(&app).height(80, 0), 3, "1 视觉行");
        app.blocks[0] = Block::Status {
            kind: retry("x".repeat(200)),
            placement: StatusPlacement::Dock(crate::app::DockSeverity::Warn),
        };
        assert_eq!(Dock::new(&app).height(80, 0), 5, "3 视觉行");
        app.blocks[0] = Block::Status {
            kind: retry("x".repeat(450)),
            placement: StatusPlacement::Dock(crate::app::DockSeverity::Warn),
        };
        assert_eq!(Dock::new(&app).height(80, 0), 8, "6 视觉行不再夹到 5");
        assert_eq!(Dock::new(&app).height(80, 100), 8, "未超过一半上限");

        app.blocks[0] = Block::Status {
            kind: StatusKind::TurnFailed {
                reason: "x".repeat(450),
            },
            placement: StatusPlacement::Dock(crate::app::DockSeverity::Error),
        };
        assert_eq!(
            Dock::new(&app).height(80, 0),
            8,
            "Error 与 Warn 共用同一套高度"
        );
    }

    #[test]
    fn input_has_no_prompt_symbol_and_continuation_aligns() -> anyhow::Result<()> {
        let mut app = test_app();
        app.input.set_text("abcdefghijklmn");
        let dock = Dock::new(&app);
        let mut terminal = Terminal::new(TestBackend::new(12, 4))?;
        terminal.draw(|frame| dock.render(frame, Rect::new(0, 0, 12, 4)))?;
        let screen = buffer_text(&terminal, 12, 4);
        assert!(!screen.contains('>'), "{screen}");
        assert!(screen.contains("── USER"));
        let rows: Vec<&str> = screen.lines().collect();
        assert_eq!(rows[2].trim_end(), "abcdefghijkl");
        assert_eq!(rows[3].trim_end(), "mn", "续行与首行正文同列");
        assert_eq!(
            terminal.get_cursor_position()?,
            ratatui::layout::Position::new(2, 3)
        );
        app.input.clear();
        terminal.draw(|frame| Dock::new(&app).render(frame, frame.area()))?;
        let screen = buffer_text(&terminal, 12, 4);
        assert!(screen.lines().skip(2).all(|line| line.trim().is_empty()));
        assert_eq!(
            terminal.get_cursor_position()?,
            ratatui::layout::Position::new(0, 2)
        );
        Ok(())
    }

    #[test]
    fn dock_status_title_carries_release_hint_and_caps_content() -> anyhow::Result<()> {
        for severity in [DockSeverity::Warn, DockSeverity::Error] {
            let mut app = test_app();
            app.blocks.push(Block::Status {
                kind: StatusKind::TurnFailed {
                    reason: "first\nsecond\nthird\nfourth\nfifth\nsixth".into(),
                },
                placement: StatusPlacement::Dock(severity),
            });
            let dock = Dock::new(&app);
            let mut terminal = Terminal::new(TestBackend::new(80, 7))?;
            terminal.draw(|frame| dock.render(frame, frame.area()))?;
            let screen = buffer_text(&terminal, 80, 7);
            let label = if severity == DockSeverity::Warn {
                "WARN"
            } else {
                "ERROR"
            };
            assert!(
                screen
                    .lines()
                    .nth(1)
                    .unwrap()
                    .starts_with(&format!("── {label} "))
            );
            assert!(screen.contains("Space 关闭"));
            assert!(screen.contains("fifth") && !screen.contains("sixth"));
            let (title, text) = if severity == DockSeverity::Warn {
                (Role::Warn, Role::Assistant)
            } else {
                (Role::ToolErr, Role::ToolErr)
            };
            assert_eq!(terminal.backend().buffer()[(3, 1)].fg, role_color(title));
            assert_eq!(terminal.backend().buffer()[(0, 2)].fg, role_color(text));
        }
        Ok(())
    }

    #[test]
    fn statusline_orders_fields_and_focus_hints() {
        let mut app = test_app();
        let line = statusline::line(&app, 40);
        assert_eq!(line.to_string(), " 未接受 · agent   Alt+Enter 换行 · idle ");
        app.kitty_keyboard = true;
        app.last_ctrl_c = Some(std::time::Instant::now());
        app.phase = TurnPhase::Streaming;
        app.connected = false;
        let line = statusline::line(&app, 80);
        assert!(
            line.to_string()
                .ends_with("Ctrl+C to quit · S-Enter 换行 · stream · offline ")
        );
        app.blocks.push(Block::Status {
            kind: StatusKind::TurnFailed {
                reason: "oops".into(),
            },
            placement: StatusPlacement::Dock(DockSeverity::Error),
        });
        let line = statusline::line(&app, 80);
        assert!(!line.to_string().contains("换行"));
        assert!(
            line.to_string()
                .ends_with("Ctrl+C to quit · stream · offline ")
        );
    }

    #[test]
    fn colors_use_contract_rgb_values() {
        for (role, color) in [
            (Role::User, Color::Rgb(0x86, 0x95, 0xe3)),
            (Role::Assistant, Color::Rgb(0xf5, 0xf3, 0xef)),
            (Role::ToolOk, Color::Rgb(0x2a, 0xb0, 0xb0)),
            (Role::ToolErr, Color::Rgb(0xcb, 0x2b, 0x31)),
            (Role::Warn, Color::Rgb(0xd3, 0x89, 0x26)),
            (Role::DimMeta, Color::Rgb(0x85, 0x7f, 0x78)),
        ] {
            assert_eq!(role_style(role).fg, Some(color));
        }
        assert_eq!(
            status_surface_style().bg,
            Some(Color::Rgb(0x2c, 0x42, 0x67))
        );
    }

    #[test]
    fn symbols_use_unicode() {
        assert_eq!(symbol(SymbolKind::Running), "·");
        assert_eq!(symbol(SymbolKind::Ok), "✓");
        assert_eq!(symbol(SymbolKind::Failed), "✗");
        assert_eq!(symbol(SymbolKind::Cancelled), "✗");
        assert_eq!(fold_symbol(false), "▸");
        assert_eq!(fold_symbol(true), "▾");
    }

    #[test]
    /// 对话记录与 Dock 共享 statusline 之上的高度：内容不足时 Dock 紧跟最后一行，
    /// 填满后 Dock 停在 statusline 之上；statusline 始终在最后一行。
    fn dock_follows_short_transcript_and_statusline_stays_last() -> anyhow::Result<()> {
        let (width, height) = (40u16, 12u16);
        for notices in [1usize, 3, 30] {
            let mut app = test_app();
            app.term_width = width;
            app.term_height = height;
            for n in 0..notices {
                app.blocks.push(Block::Status {
                    kind: StatusKind::Notice(vec![format!("notice {n}")]),
                    placement: StatusPlacement::Transcript,
                });
            }
            let content = transcript_lines(&app, width).len();
            let mut terminal = Terminal::new(TestBackend::new(width, height))?;
            terminal.draw(|frame| draw(frame, &mut app))?;
            let rows = terminal
                .backend()
                .buffer()
                .content
                .chunks(usize::from(width))
                .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
                .collect::<Vec<_>>();
            let title = rows
                .iter()
                .position(|row| row.contains("USER"))
                .expect("dock title");
            let dock_h = usize::from(Dock::new(&app).height(width, content));
            let available = usize::from(height) - 1 - dock_h;
            // Dock 的第一行是上方空行，标题线紧随其后。
            assert_eq!(
                title,
                content.min(available) + 1,
                "notices={notices}\n{}",
                rows.join("\n")
            );
            assert!(
                rows[usize::from(height) - 1].contains("idle"),
                "{}",
                rows.join("\n")
            );
        }
        // 对话记录短时，多行输入可以长过一半高度，占满对话记录之下的空间。
        let mut app = test_app();
        app.term_width = width;
        app.term_height = height;
        app.blocks.push(Block::Status {
            kind: StatusKind::Notice(vec!["notice".into()]),
            placement: StatusPlacement::Transcript,
        });
        let input = (0..8).map(|n| format!("line{n}")).collect::<Vec<_>>();
        app.input.set_text(&input.join("\n"));
        let mut terminal = Terminal::new(TestBackend::new(width, height))?;
        terminal.draw(|frame| draw(frame, &mut app))?;
        let screen = buffer_text(&terminal, width, height);
        assert!(input.iter().all(|line| screen.contains(line)), "{screen}");
        Ok(())
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

    #[test]
    fn markdown_projection_and_plain_status_share_rendered_row_counts() -> anyhow::Result<()> {
        let mut app = test_app();
        let source = "# Title\n\n- one three two";
        app.blocks = vec![
            Block::User(source.into()),
            Block::Assistant("**answer**".into()),
            Block::Status {
                kind: StatusKind::Notice(vec!["**notice**".into()]),
                placement: StatusPlacement::Transcript,
            },
        ]
        .into();
        let lines = transcript_lines(&app, 10);
        assert_eq!(lines.len(), app.content_rows(10));
        assert!(matches!(&app.blocks[0], Block::User(raw) if raw == source));
        let mut terminal = Terminal::new(TestBackend::new(12, lines.len() as u16))?;
        let transcript = render_transcript(&app, 10);
        terminal.draw(|frame| draw_transcript(frame, &app, frame.area(), 10, &transcript))?;
        let screen = buffer_text(&terminal, 12, lines.len() as u16);
        for (actual, expected) in screen.lines().zip(&lines) {
            assert_eq!(actual.trim_end(), expected.to_string().trim_end());
        }
        assert!(screen.contains("Title"));
        assert!(screen.contains("answer"));
        assert!(!screen.contains("**answer**"));
        assert!(screen.contains("**notice**"));
        Ok(())
    }

    #[test]
    fn tool_headings_errors_and_dock_status_wrap_words_not_markdown() -> anyhow::Result<()> {
        let success = tool_lines("one three", &ToolState::Ok { output_len: 12 }, 9);
        assert_eq!(success[0].to_string(), "▸ ✓ one");
        assert_eq!(success[1].to_string(), "  three ·");
        let failed = tool_lines(
            "tool",
            &ToolState::Failed {
                output_len: 0,
                error: Some("one three two".into()),
            },
            8,
        );
        assert_eq!(
            failed.iter().map(ToString::to_string).collect::<Vec<_>>(),
            ["▾ ✗ tool", "  one", "  three", "  two"]
        );
        let mut app = test_app();
        app.blocks.push(Block::Status {
            kind: StatusKind::TurnFailed {
                reason: "one **three** two".into(),
            },
            placement: StatusPlacement::Dock(DockSeverity::Error),
        });
        let dock = Dock::new(&app);
        let expected = body("✗ one **three** two", 9, role_style(Role::ToolErr));
        assert_eq!(dock.height(9, 0), 2 + expected.len() as u16);
        let mut terminal = Terminal::new(TestBackend::new(9, dock.height(9, 0)))?;
        terminal.draw(|frame| dock.render(frame, frame.area()))?;
        let screen = buffer_text(&terminal, 9, dock.height(9, 0));
        assert_eq!(
            screen
                .lines()
                .skip(2)
                .map(str::trim_end)
                .collect::<Vec<_>>(),
            vec!["✗ one", "**three**", "two"]
        );
        Ok(())
    }

    #[test]
    fn input_cursor_tracks_wide_character_wrap_and_full_line_end() {
        let mut input = InputBuffer::new();
        input.set_text("abcd中x");
        input.cursor = (0, 4);
        let (lines, row, col) = input_visual(&input, 5);
        assert_eq!(lines, ["abcd", "中x"]);
        assert_eq!((row, col), (1, 0));
        move_input_vertical(&mut input, 5, false);
        assert_eq!(input.cursor, (0, 0));
        input.set_text("12345");
        let (lines, row, col) = input_visual(&input, 5);
        assert_eq!(lines, ["12345", ""]);
        assert_eq!((row, col), (1, 0));
    }

    #[test]
    fn input_keeps_character_wrapping_and_cursor_positions() {
        let mut app = test_app();
        app.input.set_text("one three");
        let (lines, row, col) = input_visual(&app.input, 7);
        assert_eq!(lines, ["one thr", "ee"]);
        assert_eq!((row, col), (1, 2));
        app.input.cursor = (0, 7);
        let (_, row, col) = input_visual(&app.input, 7);
        assert_eq!((row, col), (1, 0));
        assert_eq!(
            body("one three", 7, Style::default())
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            vec!["one", "three"]
        );
    }

    #[test]
    fn table_minimum_width_is_clipped_by_paragraph_without_extra_rows() -> anyhow::Result<()> {
        let mut app = test_app();
        app.blocks = vec![Block::User(
            "| aa | bb |\n| --- | --- |\n| cc | dd |".into(),
        )]
        .into();
        let lines = transcript_lines(&app, 8);
        assert_eq!(lines[1].width(), 11, "two 2-cell columns plus frames");
        assert_eq!(app.content_rows(8), 6);
        let mut terminal = Terminal::new(TestBackend::new(8, 6))?;
        let transcript = render_transcript(&app, 8);
        terminal.draw(|frame| draw_transcript(frame, &app, frame.area(), 8, &transcript))?;
        assert_eq!(
            buffer_text(&terminal, 8, 6).lines().collect::<Vec<_>>(),
            [
                "── USE ─",
                "┌────┬──",
                "│ aa │ b",
                "├────┼──",
                "│ cc │ d",
                "└────┴──",
            ]
        );
        Ok(())
    }
}

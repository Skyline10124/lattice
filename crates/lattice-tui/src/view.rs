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

pub(crate) const SIDEBAR_W: u16 = 40;
pub(crate) const SIDEBAR_ASPECT: u16 = 3;
pub(crate) const MIN_CONTENT_W_WITH_SIDEBAR: u16 = 72;
pub(crate) const GAP: u16 = 1;
pub(crate) const GUTTER: u16 = 2;
/// rail 下方的内容行数上下限；Dock 总高 = 1 行 rail + 1..=5 行内容。
pub(crate) const DOCK_CONTENT_MIN_ROWS: u16 = 1;
pub(crate) const DOCK_CONTENT_MAX_ROWS: u16 = 5;
/// Dock tag 固定占 6 cells：`PROMPT` / `WARN··` / `ERROR·`（`·` 为 tag 内 padding）。
const DOCK_TAG_CELLS: usize = 6;
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
    let sidebar_content_w = frame_w
        .saturating_sub(SIDEBAR_W)
        .saturating_sub(GAP)
        .saturating_sub(GUTTER);
    let sufficiently_landscape = frame_w >= frame_h.saturating_mul(SIDEBAR_ASPECT);
    let enough_conversation_width = sidebar_content_w >= MIN_CONTENT_W_WITH_SIDEBAR;
    if sufficiently_landscape && enough_conversation_width {
        FrameLayout {
            left_w: frame_w.saturating_sub(SIDEBAR_W).saturating_sub(GAP),
            content_w: sidebar_content_w,
            sidebar: Sidebar::Visible,
        }
    } else {
        FrameLayout {
            left_w: frame_w,
            content_w: frame_w.saturating_sub(GUTTER),
            sidebar: Sidebar::Hidden,
        }
    }
}

pub(crate) struct Viewport {
    /// First visible transcript row.
    scroll: usize,
    /// While true, `scroll` is pinned to the bottom on every content change.
    follow: bool,
}

impl Viewport {
    pub(crate) fn new() -> Self {
        Self {
            scroll: 0,
            follow: true,
        }
    }

    /// Content changed to `content_rows`; viewport height is `height`.
    pub(crate) fn on_content_changed(&mut self, content_rows: usize, height: usize) {
        let max = content_rows.saturating_sub(height);
        if self.follow {
            self.scroll = max;
        } else {
            self.scroll = self.scroll.min(max);
        }
    }

    pub(crate) fn page_up(&mut self, height: usize) {
        self.follow = false;
        self.scroll = self.scroll.saturating_sub(height.saturating_sub(1));
    }

    #[rustfmt::skip]
    pub(crate) fn page_down(&mut self, content_rows: usize, height: usize) {
        if self.follow { return; }
        let max = content_rows.saturating_sub(height);
        self.scroll = self.scroll.saturating_add(height.saturating_sub(1)).min(max);
        if self.scroll >= max { self.follow = true; }
    }

    #[allow(clippy::wrong_self_convention)]
    pub(crate) fn to_bottom(&mut self, content_rows: usize, height: usize) {
        self.follow = true;
        self.scroll = content_rows.saturating_sub(height);
    }

    pub(crate) fn scroll(&self) -> usize {
        self.scroll
    }

    pub(crate) fn following(&self) -> bool {
        self.follow
    }
}

/// Exactly five foreground roles. Truecolor changes values, not vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Role {
    User,
    Assistant,
    ToolOk,
    ToolErr,
    DimMeta,
}

fn truecolor_terminal() -> bool {
    std::env::var("COLORTERM")
        .ok()
        .is_some_and(|value| matches!(value.to_ascii_lowercase().as_str(), "truecolor" | "24bit"))
}

#[rustfmt::skip]
fn role_color(role: Role, truecolor: bool) -> Color {
    match (role, truecolor) {
        (Role::User, true) => Color::Rgb(0x86, 0x95, 0xe3),
        (Role::Assistant, true) => Color::Rgb(0xf5, 0xf3, 0xef),
        (Role::ToolOk, true) => Color::Rgb(0x2a, 0xb0, 0xb0),
        (Role::ToolErr, true) => Color::Rgb(0xcb, 0x2b, 0x31),
        (Role::DimMeta, true) => Color::Rgb(0x85, 0x7f, 0x78),
        (Role::User, false) => Color::LightBlue,
        (Role::Assistant, false) => Color::White,
        (Role::ToolOk, false) => Color::Cyan,
        (Role::ToolErr, false) => Color::Red,
        (Role::DimMeta, false) => Color::DarkGray,
    }
}

pub(crate) fn role_style(role: Role) -> Style {
    Style::default().fg(role_color(role, truecolor_terminal()))
}

fn status_surface_style() -> Style {
    let bg = if truecolor_terminal() {
        Color::Rgb(0x2c, 0x42, 0x67)
    } else {
        Color::Blue
    };
    role_style(Role::DimMeta).bg(bg)
}

fn prompt_style() -> Style {
    let bg = if truecolor_terminal() {
        Color::Rgb(0x59, 0x65, 0xca)
    } else {
        Color::LightBlue
    };
    role_style(Role::Assistant).bg(bg)
}

fn warn_style() -> Style {
    let bg = if truecolor_terminal() {
        Color::Rgb(0x9f, 0x62, 0x00)
    } else {
        Color::Yellow
    };
    role_style(Role::Assistant).bg(bg)
}

fn error_style() -> Style {
    let bg = if truecolor_terminal() {
        Color::Rgb(0xcb, 0x2b, 0x31)
    } else {
        Color::Red
    };
    role_style(Role::Assistant).bg(bg)
}

/// Status symbols, with an ASCII fallback when `unicode == false`.
#[rustfmt::skip]
pub(crate) fn symbol(state: SymbolKind, unicode: bool) -> &'static str {
    match (state, unicode) {
        (SymbolKind::Running, true) => "·",
        (SymbolKind::Running, false) => ".",
        (SymbolKind::Ok, true) => "✓",
        (SymbolKind::Ok, false) => "+",
        (SymbolKind::Failed | SymbolKind::Cancelled, true) => "✗",
        (SymbolKind::Failed | SymbolKind::Cancelled, false) => "x",
    }
}

#[derive(Clone, Copy)]
pub(crate) enum SymbolKind {
    Running,
    Ok,
    Failed,
    Cancelled,
}

fn fold_symbol(expanded: bool, unicode: bool) -> &'static str {
    match (expanded, unicode) {
        (false, true) => "▸",
        (true, true) => "▾",
        (false, false) => ">",
        (true, false) => "v",
    }
}

/// 统一 Dock：`PROMPT` / `WARN` / `ERROR` 是同一组件的三个互斥状态；
/// `INFO` 不进 Dock，直接进 transcript。
pub(crate) enum DockView<'a> {
    Prompt,
    Warn(&'a StatusKind),
    Error(&'a StatusKind),
}

/// 左侧底部的完整组件。外部布局只分配一个 Dock Rect；rail、状态标签、
/// `SPACE → PROMPT` 提示、输入 / 状态正文与光标定位全部由它内部拥有。
pub(crate) struct Dock<'a> {
    view: DockView<'a>,
    input: &'a InputBuffer,
}

impl<'a> Dock<'a> {
    pub(crate) fn new(app: &'a App) -> Self {
        let view = match app.dock_status() {
            Some((kind, DockSeverity::Warn)) => DockView::Warn(kind),
            Some((kind, DockSeverity::Error)) => DockView::Error(kind),
            None => DockView::Prompt,
        };
        Self {
            view,
            input: &app.input,
        }
    }

    /// rail + 内容的总行数，2..=6。视口与布局共用这一个来源。
    pub(crate) fn height(&self, width: u16) -> u16 {
        1 + self.content_rows(width).clamp(
            DOCK_CONTENT_MIN_ROWS as usize,
            DOCK_CONTENT_MAX_ROWS as usize,
        ) as u16
    }

    /// 按内容宽度折行后的视觉行数（未夹紧）。Prompt 沿用光标可见口径。
    fn content_rows(&self, width: u16) -> usize {
        match &self.view {
            DockView::Prompt => {
                let (visual, cursor_row, _) = input_visual(self.input, width);
                visual.len().max(cursor_row + 1)
            }
            DockView::Warn(kind) | DockView::Error(kind) => {
                wrap_text(&status_text(kind, locale_utf8()), width).len()
            }
        }
    }

    pub(crate) fn render(&self, frame: &mut Frame, area: Rect) {
        if area.height == 0 || area.width == 0 {
            return;
        }
        let content_w = area.width.saturating_sub(GUTTER);
        self.render_rail(frame, Rect::new(area.x, area.y, area.width, 1));
        let body_area = Rect::new(
            area.x,
            area.y.saturating_add(1),
            area.width,
            area.height.saturating_sub(1),
        );
        match &self.view {
            DockView::Prompt => self.render_prompt(frame, body_area, content_w),
            DockView::Warn(kind) => self.render_status(
                frame,
                body_area,
                content_w,
                &status_text(kind, locale_utf8()),
                role_style(Role::Assistant),
            ),
            DockView::Error(kind) => self.render_status(
                frame,
                body_area,
                content_w,
                &status_text(kind, locale_utf8()),
                role_style(Role::ToolErr),
            ),
        }
    }

    fn tag(&self) -> (&'static str, Style) {
        match &self.view {
            DockView::Prompt => ("PROMPT", prompt_style()),
            DockView::Warn(_) => ("WARN  ", warn_style()),
            DockView::Error(_) => ("ERROR ", error_style()),
        }
    }

    fn render_rail(&self, frame: &mut Frame, area: Rect) {
        let unicode = locale_utf8();
        let unit = if unicode { "─" } else { "-" };
        let dim = role_style(Role::DimMeta);
        let width = area.width as usize;
        let mut spans = Vec::new();
        let mut used = (GUTTER as usize).min(width);
        spans.push(Span::styled(unit.repeat(used), dim));
        let (label, style) = self.tag();
        let tag_room = width.saturating_sub(used).min(DOCK_TAG_CELLS);
        let tag = truncate_display(label, tag_room);
        used += display_width(&tag);
        if !tag.is_empty() {
            spans.push(Span::styled(tag, style));
        }

        let rest = width.saturating_sub(used);
        if rest == 0 {
            frame.render_widget(Paragraph::new(Line::from(spans)), area);
            return;
        }
        match &self.view {
            DockView::Warn(_) | DockView::Error(_) => {
                // SPACE → PROMPT 只在 WARN / ERROR 状态显示；右锚定，
                // 两侧各留 2 列 rail，放不下时整段 best effort 填满。
                let hint = if unicode {
                    "SPACE → PROMPT"
                } else {
                    "SPACE -> PROMPT"
                };
                let hint_w = display_width(hint);
                if rest >= hint_w + 4 {
                    spans.push(Span::styled(unit.repeat(rest - hint_w - 2), dim));
                    spans.push(Span::styled(hint.to_string(), dim));
                    spans.push(Span::styled(unit.repeat(2), dim));
                } else {
                    spans.push(Span::styled(unit.repeat(rest), dim));
                }
            }
            DockView::Prompt => spans.push(Span::styled(unit.repeat(rest), dim)),
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    fn render_prompt(&self, frame: &mut Frame, area: Rect, content_w: u16) {
        let (visual, cursor_row, cursor_col) = input_visual(self.input, content_w);
        let height = area.height as usize;
        let scroll = cursor_row.saturating_sub(height.saturating_sub(1));
        let shown = visual
            .iter()
            .enumerate()
            .skip(scroll)
            .take(height)
            .map(|(index, text)| {
                let prompt = if index == 0 { "> " } else { "  " };
                Line::from(vec![
                    Span::styled(prompt, role_style(Role::User)),
                    Span::styled(text.clone(), role_style(Role::Assistant)),
                ])
            })
            .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(shown), area);

        let screen_y = cursor_row.saturating_sub(scroll) as u16;
        let cursor_x = area
            .x
            .saturating_add(GUTTER)
            .saturating_add(cursor_col as u16);
        if screen_y < area.height && cursor_x < area.x.saturating_add(area.width) {
            frame.set_cursor_position((cursor_x, area.y.saturating_add(screen_y)));
        }
    }

    /// Warn / Error 正文：显示前 5 个视觉行，不给只读 Dock 增加滚动。
    fn render_status(
        &self,
        frame: &mut Frame,
        area: Rect,
        content_w: u16,
        text: &str,
        style: Style,
    ) {
        let shown = body(text, content_w, style)
            .into_iter()
            .take(DOCK_CONTENT_MAX_ROWS as usize)
            .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(shown), area);
    }
}

pub(crate) fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let geometry = frame_layout(area.width, area.height);
    match geometry.sidebar {
        Sidebar::Visible => {
            let columns = Layout::horizontal([
                Constraint::Length(geometry.left_w),
                Constraint::Length(GAP),
                Constraint::Length(SIDEBAR_W),
            ])
            .split(area);
            draw_left(frame, app, columns[0], geometry.content_w, false);
            draw_right_status(frame, app, columns[2]);
        }
        Sidebar::Hidden => {
            draw_left(frame, app, area, geometry.content_w, true);
        }
    }
}

fn draw_left(frame: &mut Frame, app: &App, area: Rect, content_w: u16, bottomline: bool) {
    let dock = Dock::new(app);
    let bottom_h = u16::from(bottomline).min(area.height);
    let above = area.height.saturating_sub(bottom_h);
    let dock_h = dock.height(content_w).min(above);
    let transcript_h = above.saturating_sub(dock_h);

    let dock_y = area.y.saturating_add(transcript_h);
    let bottom_y = dock_y.saturating_add(dock_h);
    draw_transcript(
        frame,
        app,
        Rect::new(area.x, area.y, area.width, transcript_h),
        content_w,
    );
    dock.render(frame, Rect::new(area.x, dock_y, area.width, dock_h));
    if bottom_h > 0 {
        draw_bottomline(
            frame,
            app,
            Rect::new(area.x, bottom_y, area.width, bottom_h),
        );
    }
}

pub(crate) fn locale_utf8() -> bool {
    ["LC_ALL", "LANG"].iter().any(|key| {
        std::env::var(key)
            .ok()
            .is_some_and(|value| value.to_ascii_uppercase().contains("UTF-8"))
    })
}

/// One source of truth for transcript rendering and viewport row counts.
pub(crate) fn transcript_lines(app: &App, content_w: u16) -> Vec<Line<'static>> {
    let unicode = locale_utf8();
    let mut lines = Vec::new();
    let mut assistant_header = false;
    let mut agent = app.ctx.active_agent.as_str();
    for block in &app.blocks {
        match block {
            Block::AgentName(name) => {
                agent = name;
                assistant_header = false;
            }
            Block::User(text) => {
                separate(&mut lines);
                lines.extend(body("user", content_w, role_style(Role::User)));
                lines.extend(body(text, content_w, role_style(Role::Assistant)));
                assistant_header = false;
            }
            Block::Assistant(text) => {
                ensure_assistant_header(&mut lines, &mut assistant_header, agent, content_w);
                lines.extend(body(text, content_w, role_style(Role::Assistant)));
            }
            Block::Tool { name, state, .. } => {
                ensure_assistant_header(&mut lines, &mut assistant_header, agent, content_w);
                lines.extend(tool_lines(name, state, content_w, unicode));
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
                    kind => {
                        ensure_assistant_header(
                            &mut lines,
                            &mut assistant_header,
                            agent,
                            content_w,
                        );
                        lines.extend(status_lines(kind, content_w, unicode));
                    }
                }
            }
        }
    }
    lines
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

fn ensure_assistant_header(
    lines: &mut Vec<Line<'static>>,
    shown: &mut bool,
    agent: &str,
    content_w: u16,
) {
    if !*shown {
        separate(lines);
        lines.extend(body(agent, content_w, role_style(Role::Assistant)));
        *shown = true;
    }
}

fn body(text: &str, content_w: u16, style: Style) -> Vec<Line<'static>> {
    wrap_text(text, content_w)
        .into_iter()
        .map(|text| {
            if text.is_empty() {
                Line::default()
            } else {
                Line::from(vec![Span::raw("  "), Span::styled(text, style)])
            }
        })
        .collect()
}

fn tool_lines(name: &str, state: &ToolState, content_w: u16, unicode: bool) -> Vec<Line<'static>> {
    let dim = role_style(Role::DimMeta);
    let err = role_style(Role::ToolErr);
    match state {
        ToolState::Running => body(
            &format!("{name} · running {}", symbol(SymbolKind::Running, unicode)),
            content_w,
            dim,
        ),
        ToolState::Unknown => body(&format!("{name} · 观察结果未知"), content_w, dim),
        ToolState::Unavailable { outcome } => body(
            &format!("{name} · {outcome}；结果材料不可用"),
            content_w,
            dim,
        ),
        ToolState::Ok { output_len } => vec![Line::from(vec![
            Span::styled(format!("{} ", fold_symbol(false, unicode)), dim),
            Span::styled(symbol(SymbolKind::Ok, unicode), role_style(Role::ToolOk)),
            Span::styled(format!(" {name} · {output_len} B"), dim),
        ])],
        ToolState::Failed { error, .. } => {
            let mut lines = vec![Line::from(vec![
                Span::styled(format!("{} ", fold_symbol(true, unicode)), dim),
                Span::styled(symbol(SymbolKind::Failed, unicode), err),
                Span::styled(format!(" {name}"), dim),
            ])];
            if let Some(error) = error {
                lines.extend(body(error, content_w, err));
            }
            lines
        }
    }
}

/// 状态文本，transcript 与 Dock 共用同一措辞；样式由各自渲染层决定。
#[rustfmt::skip]
fn status_text(kind: &StatusKind, unicode: bool) -> String {
    match kind {
        StatusKind::Committed { commit_id } => format!(
            "{} committed {commit_id}", symbol(SymbolKind::Ok, unicode)
        ),
        StatusKind::Visible { commit_id } => format!("结果可见 {commit_id}；完成确认未知"),
        StatusKind::CommitFailed { reason } => format!(
            "{} commit failed: {reason}", symbol(SymbolKind::Failed, unicode)
        ),
        StatusKind::TurnFailed { reason } => format!(
            "{} {reason}", symbol(SymbolKind::Failed, unicode)
        ),
        StatusKind::Cancelled { reason } => {
            let mark = symbol(SymbolKind::Cancelled, unicode);
            reason.as_ref().map_or_else(
                || format!("{mark} cancelled"),
                |reason| format!("{mark} cancelled: {reason}"),
            )
        }
        StatusKind::LlmRetry { attempt, max_attempts, delay_ms, message } => format!(
            "{} llm retry {attempt}/{max_attempts} after {delay_ms}ms: {message}",
            symbol(SymbolKind::Running, unicode)
        ),
        StatusKind::LlmFailed { attempt, max_attempts, message } => format!(
            "{} llm failed {attempt}/{max_attempts}: {message}",
            symbol(SymbolKind::Failed, unicode)
        ),
        StatusKind::Notice(_) => String::new(),
    }
}

fn status_lines(kind: &StatusKind, content_w: u16, unicode: bool) -> Vec<Line<'static>> {
    let style = match kind {
        StatusKind::Committed { .. } => role_style(Role::ToolOk),
        StatusKind::CommitFailed { .. }
        | StatusKind::TurnFailed { .. }
        | StatusKind::Cancelled { .. }
        | StatusKind::LlmFailed { .. } => role_style(Role::ToolErr),
        StatusKind::LlmRetry { .. } | StatusKind::Visible { .. } => role_style(Role::DimMeta),
        StatusKind::Notice(_) => return Vec::new(),
    };
    body(&status_text(kind, unicode), content_w, style)
}

fn wrap_text(text: &str, width: u16) -> Vec<String> {
    let width = width.max(1) as usize;
    let mut out = Vec::new();
    for paragraph in text.split('\n') {
        if paragraph.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut current = String::new();
        for ch in paragraph.chars() {
            let ch_w = UnicodeWidthChar::width(ch).unwrap_or(0);
            if !current.is_empty()
                && UnicodeWidthStr::width(current.as_str()).saturating_add(ch_w) > width
            {
                out.push(std::mem::take(&mut current));
            }
            current.push(ch);
        }
        out.push(current);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

fn visual_cursor(line: &str, char_index: usize, width: u16) -> (usize, usize) {
    let width = width.max(1) as usize;
    let prefix: String = line.chars().take(char_index).collect();
    let lines = wrap_text(&prefix, width as u16);
    let last_w = lines
        .last()
        .map(|line| UnicodeWidthStr::width(line.as_str()))
        .unwrap_or(0);
    let row = lines.len().saturating_sub(1);
    if last_w >= width && !prefix.is_empty() {
        (row + 1, 0)
    } else {
        (row, last_w)
    }
}

fn input_visual(input: &InputBuffer, content_w: u16) -> (Vec<String>, usize, usize) {
    let mut visual = Vec::new();
    let mut cursor_row = 0usize;
    let mut cursor_col = 0usize;
    for (index, line) in input.lines.iter().enumerate() {
        let wrapped = wrap_text(line, content_w);
        if index < input.cursor.0 {
            cursor_row += wrapped.len().max(1);
        } else if index == input.cursor.0 {
            let (row, col) = visual_cursor(line, input.cursor.1, content_w);
            cursor_row += row;
            cursor_col = col;
        }
        visual.extend(wrapped);
    }
    while visual.len() <= cursor_row {
        visual.push(String::new());
    }
    (visual, cursor_row, cursor_col)
}

fn draw_transcript(frame: &mut Frame, app: &App, area: Rect, content_w: u16) {
    let lines = transcript_lines(app, content_w);
    let start = app.viewport.scroll().min(lines.len());
    let end = (start + area.height as usize).min(lines.len());
    frame.render_widget(Paragraph::new(lines[start..end].to_vec()), area);
}

fn draw_right_status(frame: &mut Frame, app: &App, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let available = area.width.saturating_sub(1) as usize;
    let mut rows = vec![Line::default(); area.height as usize];
    set_status_row(&mut rows, 0, &app.ctx.session_id, Role::DimMeta, available);
    set_status_row(
        &mut rows,
        1,
        &app.ctx.active_agent,
        Role::DimMeta,
        available,
    );
    set_status_row(
        &mut rows,
        3,
        &phase_and_connection(app),
        phase_role(app.phase),
        available,
    );
    if !app.viewport.following() {
        set_status_row(&mut rows, 5, "[scroll]", Role::DimMeta, available);
    }

    let height = rows.len();
    if ctrl_c_visible(app) {
        set_status_row(
            &mut rows,
            height.saturating_sub(3),
            CTRL_C_HINT,
            Role::ToolErr,
            available,
        );
    }
    let key = if app.kitty_keyboard {
        "S-Enter"
    } else {
        "Alt+Enter"
    };
    set_status_row(
        &mut rows,
        height.saturating_sub(2),
        key,
        Role::DimMeta,
        available,
    );
    set_status_row(
        &mut rows,
        height.saturating_sub(1),
        "newline",
        Role::DimMeta,
        available,
    );

    frame.render_widget(Paragraph::new(rows).style(status_surface_style()), area);
}

fn set_status_row(
    rows: &mut [Line<'static>],
    row: usize,
    text: &str,
    role: Role,
    available: usize,
) {
    if row >= rows.len() {
        return;
    }
    rows[row] = Line::from(vec![
        Span::raw(" "),
        Span::styled(truncate_display(text, available), role_style(role)),
    ]);
}

/// 侧栏隐藏时的独立底行：整宽给状态文本，不再携带 PROMPT tag。
fn draw_bottomline(frame: &mut Frame, app: &App, area: Rect) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let line = bottom_status_line(app, area.width as usize);
    frame.render_widget(Paragraph::new(line).style(status_surface_style()), area);
}

fn bottom_status_line(app: &App, width: usize) -> Line<'static> {
    let state = phase_and_connection(app);
    let phase = state.as_str();
    let scroll = (!app.viewport.following()).then_some("[scroll]");
    let key = if ctrl_c_visible(app) {
        CTRL_C_HINT
    } else if app.kitty_keyboard {
        "S-Enter newline"
    } else {
        "Alt+Enter newline"
    };
    let full = [
        app.ctx.session_id.as_str(),
        app.ctx.active_agent.as_str(),
        phase,
        scroll.unwrap_or(""),
        key,
    ];
    let key =
        (joined_width(full.into_iter().filter(|part| !part.is_empty())) <= width).then_some(key);

    let tail = [Some(phase), scroll, key];
    let tail_width = joined_width(tail.into_iter().flatten());
    let agent_width = display_width(&app.ctx.active_agent);
    let session_room = width
        .saturating_sub(tail_width)
        .saturating_sub(agent_width)
        .saturating_sub(6);
    let session = truncate_display(&app.ctx.session_id, session_room);
    let session_cost = display_width(&session) + usize::from(!session.is_empty()) * 3;
    let agent_room = width
        .saturating_sub(tail_width)
        .saturating_sub(session_cost)
        .saturating_sub(3);
    let agent = truncate_display(&app.ctx.active_agent, agent_room);

    let mut spans = Vec::new();
    for field in [&session, &agent]
        .into_iter()
        .filter(|field| !field.is_empty())
    {
        if !spans.is_empty() {
            spans.push(Span::styled(" · ", role_style(Role::DimMeta)));
        }
        spans.push(Span::styled(field.clone(), role_style(Role::DimMeta)));
    }
    if !spans.is_empty() {
        spans.push(Span::styled(" · ", role_style(Role::DimMeta)));
    }
    spans.push(Span::styled(
        truncate_display(phase, width),
        role_style(phase_role(app.phase)),
    ));
    for field in [scroll, key].into_iter().flatten() {
        spans.push(Span::styled(
            format!(" · {field}"),
            role_style(Role::DimMeta),
        ));
    }
    Line::from(spans)
}

fn joined_width<'a>(parts: impl Iterator<Item = &'a str>) -> usize {
    let parts = parts.collect::<Vec<_>>();
    parts.iter().map(|part| display_width(part)).sum::<usize>() + parts.len().saturating_sub(1) * 3
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
        TurnPhase::Committing => "commit",
        TurnPhase::Cancelling => "cancel",
    }
}

fn phase_role(phase: TurnPhase) -> Role {
    match phase {
        TurnPhase::Streaming => Role::User,
        TurnPhase::Cancelling => Role::ToolErr,
        TurnPhase::Idle | TurnPhase::Committing => Role::DimMeta,
    }
}

fn ctrl_c_visible(app: &App) -> bool {
    app.last_ctrl_c
        .is_some_and(|then| then.elapsed() <= CTRL_C_WINDOW)
}

fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
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

    fn test_app() -> App {
        App::new(
            SessionContext {
                session_id: "sess".into(),
                active_agent: "agent".into(),
                project_branch: "main".into(),
            },
            false,
            std::path::Path::new("/nonexistent-tui-test-history"),
        )
    }

    fn buffer_text(terminal: &Terminal<TestBackend>, width: u16, height: u16) -> String {
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn sidebar_visibility_follows_aspect_and_content_width() {
        let visible = frame_layout(176, 50);
        assert_eq!(
            visible,
            FrameLayout {
                left_w: 135,
                content_w: 133,
                sidebar: Sidebar::Visible,
            }
        );
        // 比例不足 3:1，或分栏后正文不足 72 列，都隐藏侧栏。
        for (w, h) in [(115, 47), (85, 47), (56, 47), (80, 24)] {
            assert_eq!(
                frame_layout(w, h),
                FrameLayout {
                    left_w: w,
                    content_w: w.saturating_sub(GUTTER),
                    sidebar: Sidebar::Hidden,
                },
                "{w}x{h}"
            );
        }
    }

    #[test]
    fn layout_boundaries_are_underflow_safe() {
        for (w, h) in [(1, 1), (7, 3), (43, 10), (44, 10), (114, 38), (115, 38)] {
            let _ = frame_layout(w, h);
        }
        assert_eq!(
            frame_layout(1, 1),
            FrameLayout {
                left_w: 1,
                content_w: 0,
                sidebar: Sidebar::Hidden,
            }
        );
        // 114x38 比例刚好不足；115x38 比例与正文宽度同时达标。
        assert_eq!(frame_layout(114, 38).sidebar, Sidebar::Hidden);
        assert_eq!(frame_layout(115, 38).sidebar, Sidebar::Visible);
    }

    #[test]
    fn follow_pins_and_resize_clamps_scroll() {
        let mut viewport = Viewport::new();
        viewport.on_content_changed(20, 10);
        assert_eq!(viewport.scroll(), 10);
        viewport.page_up(10);
        assert!(!viewport.following());
        viewport.on_content_changed(5, 10);
        assert_eq!(viewport.scroll(), 0);
    }

    #[test]
    fn page_down_to_bottom_restores_follow() {
        let mut viewport = Viewport::new();
        viewport.on_content_changed(30, 10);
        viewport.page_up(10);
        viewport.page_down(30, 10);
        assert!(viewport.following());
        assert_eq!(viewport.scroll(), 20);
    }

    #[test]
    fn historical_agent_names_do_not_use_the_current_profile_label() {
        let mut app = test_app();
        app.blocks = vec![
            Block::AgentName("previous-agent".into()),
            Block::User("old prompt".into()),
            Block::Assistant("old answer".into()),
            Block::AgentName("next-agent".into()),
            Block::User("next prompt".into()),
            Block::Assistant("next answer".into()),
        ];
        let text = transcript_lines(&app, 80)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        assert_eq!(
            text.iter()
                .filter(|line| line.trim() == "previous-agent")
                .count(),
            1
        );
        assert_eq!(
            text.iter()
                .filter(|line| line.trim() == "next-agent")
                .count(),
            1
        );
        assert!(!text.iter().any(|line| line.trim() == "agent"));
    }

    #[test]
    fn transcript_groups_one_agent_header_and_shares_row_count() {
        let mut app = test_app();
        app.blocks.extend([
            Block::User("question".into()),
            Block::Assistant("before tool".into()),
            Block::Tool {
                id: "opaque-id".into(),
                name: "observe.workspace".into(),
                state: ToolState::Ok { output_len: 1204 },
            },
            Block::Assistant("after tool".into()),
            Block::Status {
                kind: StatusKind::Committed {
                    commit_id: "abc".into(),
                },
                placement: StatusPlacement::Transcript,
            },
            Block::User("next".into()),
        ]);

        let lines = transcript_lines(&app, 61);
        let text = lines.iter().map(ToString::to_string).collect::<Vec<_>>();
        assert_eq!(text.iter().filter(|line| line.trim() == "agent").count(), 1);
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
            kind: StatusKind::CommitFailed {
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

        for block in &mut app.blocks {
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
    fn user_speaker_and_body_use_distinct_english_styles() {
        let mut app = test_app();
        app.blocks.push(Block::User("question".into()));
        let lines = transcript_lines(&app, 61);
        assert_eq!(lines[0].to_string().trim(), "user");
        assert_eq!(lines[0].spans[1].style, role_style(Role::User));
        assert_eq!(lines[1].spans[1].style, role_style(Role::Assistant));
    }

    #[test]
    fn failed_tool_expands_error_and_success_uses_bytes() {
        let mut app = test_app();
        app.blocks.extend([
            Block::Tool {
                id: "ok".into(),
                name: "observe.workspace".into(),
                state: ToolState::Ok { output_len: 1204 },
            },
            Block::Tool {
                id: "failed".into(),
                name: "mutate.edit".into(),
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
    fn dock_height_clamps_prompt_rows_one_to_five() {
        let mut app = test_app();
        assert_eq!(Dock::new(&app).height(10), 2, "空 Prompt：1 行内容");
        app.input.set_text("a\nb\nc");
        assert_eq!(Dock::new(&app).height(10), 4);
        app.input.set_text("a\nb\nc\nd\ne\nf");
        assert_eq!(Dock::new(&app).height(10), 6, "6 视觉行夹到 5");
        assert_eq!(visual_cursor("中文", 1, 2), (1, 0));
    }

    #[test]
    fn dock_height_clamps_status_rows_one_to_five() {
        let retry = |message: String| StatusKind::LlmRetry {
            attempt: 1,
            max_attempts: 3,
            delay_ms: 0,
            message,
        };
        // "· llm retry 1/3 after 0ms: " 前缀长 27，宽 80 时按 ceil(len/80) 折行。
        let mut app = test_app();
        app.blocks.push(Block::Status {
            kind: retry("x".repeat(40)),
            placement: StatusPlacement::Dock(crate::app::DockSeverity::Warn),
        });
        assert_eq!(Dock::new(&app).height(80), 2, "1 视觉行");
        app.blocks[0] = Block::Status {
            kind: retry("x".repeat(200)),
            placement: StatusPlacement::Dock(crate::app::DockSeverity::Warn),
        };
        assert_eq!(Dock::new(&app).height(80), 4, "3 视觉行");
        app.blocks[0] = Block::Status {
            kind: retry("x".repeat(450)),
            placement: StatusPlacement::Dock(crate::app::DockSeverity::Warn),
        };
        assert_eq!(Dock::new(&app).height(80), 6, "6 视觉行夹到 5");

        app.blocks[0] = Block::Status {
            kind: StatusKind::TurnFailed {
                reason: "x".repeat(450),
            },
            placement: StatusPlacement::Dock(crate::app::DockSeverity::Error),
        };
        assert_eq!(
            Dock::new(&app).height(80),
            6,
            "Error 与 Warn 共用同一套高度"
        );
    }

    #[test]
    fn dock_prompt_scrolls_internally_and_keeps_cursor_visible() -> anyhow::Result<()> {
        let mut app = test_app();
        app.input.set_text("1\n2\n3\n4\n5\n6\n7\n8");
        let dock = Dock::new(&app);
        assert_eq!(dock.height(10), 6);
        let mut terminal = Terminal::new(TestBackend::new(12, 6))?;
        terminal.draw(|frame| dock.render(frame, Rect::new(0, 0, 12, 6)))?;
        // rail 1 行 + 内容 5 行；光标行 8/8 滚动进最后 5 个视觉行内。
        let cursor = terminal.get_cursor_position()?;
        assert_eq!(cursor, ratatui::layout::Position::new(3, 5));
        let screen = buffer_text(&terminal, 12, 6);
        let rows: Vec<&str> = screen.lines().collect();
        assert_eq!(rows[1].trim_end(), "  4", "滚动后首个可见行");
        assert_eq!(rows[5].trim_end(), "  8", "光标行保持可见");
        Ok(())
    }

    #[test]
    fn dock_prompt_appears_only_on_first_visual_line() -> anyhow::Result<()> {
        let mut app = test_app();
        app.input.set_text("abcd");
        let dock = Dock::new(&app);
        let mut terminal = Terminal::new(TestBackend::new(8, 4))?;
        terminal.draw(|frame| dock.render(frame, Rect::new(0, 0, 8, 4)))?;
        let screen = buffer_text(&terminal, 8, 4);
        assert_eq!(screen.matches('>').count(), 1);
        assert!(screen.contains("PROMPT"), "rail 承载 tag");
        assert_eq!(
            terminal.get_cursor_position()?,
            ratatui::layout::Position::new(6, 1)
        );
        Ok(())
    }

    #[test]
    fn dock_status_rail_carries_release_hint() -> anyhow::Result<()> {
        let mut app = test_app();
        app.blocks.push(Block::Status {
            kind: StatusKind::TurnFailed {
                reason: "permission denied".into(),
            },
            placement: StatusPlacement::Dock(crate::app::DockSeverity::Error),
        });
        let dock = Dock::new(&app);
        let mut terminal = Terminal::new(TestBackend::new(80, 6))?;
        terminal.draw(|frame| dock.render(frame, Rect::new(0, 0, 80, 6)))?;
        let screen = buffer_text(&terminal, 80, 6);
        assert!(screen.contains("ERROR"), "rail 上的 tag");
        assert!(screen.contains("SPACE"), "WARN / ERROR rail 上的释放提示");
        assert!(screen.contains("permission denied"), "状态正文");
        // Prompt 态的 rail 不带释放提示。
        let app = test_app();
        let dock = Dock::new(&app);
        let mut terminal = Terminal::new(TestBackend::new(80, 6))?;
        terminal.draw(|frame| dock.render(frame, Rect::new(0, 0, 80, 6)))?;
        let screen = buffer_text(&terminal, 80, 6);
        assert!(screen.contains("PROMPT"));
        assert!(!screen.contains("SPACE"));
        Ok(())
    }

    #[test]
    fn colors_have_truecolor_and_named_fallbacks() {
        assert_eq!(role_color(Role::User, true), Color::Rgb(0x86, 0x95, 0xe3));
        assert_eq!(role_color(Role::User, false), Color::LightBlue);
        assert_eq!(role_color(Role::ToolOk, false), Color::Cyan);
        assert_eq!(role_color(Role::ToolErr, false), Color::Red);
    }

    #[test]
    fn symbols_fall_back_to_ascii() {
        assert_eq!(symbol(SymbolKind::Ok, true), "✓");
        assert_eq!(symbol(SymbolKind::Ok, false), "+");
        assert_eq!(fold_symbol(false, true), "▸");
        assert_eq!(fold_symbol(false, false), ">");
        assert_eq!(fold_symbol(true, false), "v");
    }

    #[test]
    fn all_frame_shapes_draw_without_panic() -> anyhow::Result<()> {
        for (width, height) in [(1, 1), (7, 3), (56, 47), (80, 24), (115, 47), (176, 50)] {
            let mut app = test_app();
            app.blocks.push(Block::Status {
                kind: StatusKind::Notice(vec!["hello".into()]),
                placement: StatusPlacement::Transcript,
            });
            app.input.set_text("typed");
            let mut terminal = Terminal::new(TestBackend::new(width, height))?;
            terminal.draw(|frame| draw(frame, &app))?;
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
                            max_attempts: 3,
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
                terminal.draw(|frame| draw(frame, &app))?;
            }
        }
        Ok(())
    }
}

// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{App, Block, INPUT_MAX_ROWS, INPUT_MIN_ROWS, StatusKind, ToolState, TurnPhase};
use crate::input::CTRL_C_WINDOW;

pub(crate) const SIDEBAR_W: u16 = 40;
pub(crate) const SIDEBAR_ASPECT: u16 = 3;
pub(crate) const MIN_CONTENT_W_WITH_SIDEBAR: u16 = 72;
pub(crate) const GAP: u16 = 1;
pub(crate) const GUTTER: u16 = 2;
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
    let input_h = input_visual_height(app, content_w);
    let [transcript, separator, input, bottom] = left_regions(area, input_h);
    draw_transcript(frame, app, transcript, content_w);
    draw_separator(frame, separator);
    draw_input(frame, app, input, content_w);
    if bottomline {
        draw_bottom_status(frame, app, bottom);
    } else {
        draw_prompt_tag(frame, bottom);
    }
}

fn left_regions(area: Rect, input_h: u16) -> [Rect; 4] {
    let bottom_h = area.height.min(1);
    let above_bottom = area.height.saturating_sub(bottom_h);
    let input_h = input_h.min(above_bottom);
    let above_input = above_bottom.saturating_sub(input_h);
    let separator_h = above_input.min(1);
    let transcript_h = above_input.saturating_sub(separator_h);

    let separator_y = area.y.saturating_add(transcript_h);
    let input_y = separator_y.saturating_add(separator_h);
    let bottom_y = input_y.saturating_add(input_h);
    [
        Rect::new(area.x, area.y, area.width, transcript_h),
        Rect::new(area.x, separator_y, area.width, separator_h),
        Rect::new(area.x, input_y, area.width, input_h),
        Rect::new(area.x, bottom_y, area.width, bottom_h),
    ]
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
    for block in &app.blocks {
        match block {
            Block::User(text) => {
                separate(&mut lines);
                lines.extend(body("user", content_w, role_style(Role::User)));
                lines.extend(body(text, content_w, role_style(Role::Assistant)));
                assistant_header = false;
            }
            Block::Assistant(text) => {
                ensure_assistant_header(
                    &mut lines,
                    &mut assistant_header,
                    &app.ctx.active_agent,
                    content_w,
                );
                lines.extend(body(text, content_w, role_style(Role::Assistant)));
            }
            Block::Tool { name, state, .. } => {
                ensure_assistant_header(
                    &mut lines,
                    &mut assistant_header,
                    &app.ctx.active_agent,
                    content_w,
                );
                lines.extend(tool_lines(name, state, content_w, unicode));
            }
            Block::Status(StatusKind::Notice(notice)) => {
                for text in notice {
                    lines.extend(body(text, content_w, role_style(Role::DimMeta)));
                }
            }
            Block::Status(kind) => {
                ensure_assistant_header(
                    &mut lines,
                    &mut assistant_header,
                    &app.ctx.active_agent,
                    content_w,
                );
                lines.extend(status_lines(kind, content_w, unicode));
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

fn status_lines(kind: &StatusKind, content_w: u16, unicode: bool) -> Vec<Line<'static>> {
    let dim = role_style(Role::DimMeta);
    let err = role_style(Role::ToolErr);
    let (text, style) = match kind {
        StatusKind::Committed { commit_id } => (
            format!("{} committed {commit_id}", symbol(SymbolKind::Ok, unicode)),
            role_style(Role::ToolOk),
        ),
        StatusKind::CommitFailed { reason } => (
            format!(
                "{} commit failed: {reason}",
                symbol(SymbolKind::Failed, unicode)
            ),
            err,
        ),
        StatusKind::TurnFailed { reason } => (
            format!("{} {reason}", symbol(SymbolKind::Failed, unicode)),
            err,
        ),
        StatusKind::Cancelled { reason } => {
            let mark = symbol(SymbolKind::Cancelled, unicode);
            (
                reason.as_ref().map_or_else(
                    || format!("{mark} cancelled"),
                    |reason| format!("{mark} cancelled: {reason}"),
                ),
                err,
            )
        }
        StatusKind::LlmRetry {
            attempt,
            max_attempts,
            delay_ms,
            message,
        } => (
            format!(
                "{} llm retry {attempt}/{max_attempts} after {delay_ms}ms: {message}",
                symbol(SymbolKind::Running, unicode)
            ),
            dim,
        ),
        StatusKind::LlmFailed {
            attempt,
            max_attempts,
            message,
        } => (
            format!(
                "{} llm failed {attempt}/{max_attempts}: {message}",
                symbol(SymbolKind::Failed, unicode)
            ),
            err,
        ),
        StatusKind::Notice(_) => return Vec::new(),
    };
    body(&text, content_w, style)
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

fn input_visual(app: &App, content_w: u16) -> (Vec<String>, usize, usize) {
    let mut visual = Vec::new();
    let mut cursor_row = 0usize;
    let mut cursor_col = 0usize;
    for (index, line) in app.input.lines.iter().enumerate() {
        let wrapped = wrap_text(line, content_w);
        if index < app.input.cursor.0 {
            cursor_row += wrapped.len().max(1);
        } else if index == app.input.cursor.0 {
            let (row, col) = visual_cursor(line, app.input.cursor.1, content_w);
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

pub(crate) fn input_visual_height(app: &App, content_w: u16) -> u16 {
    let (visual, cursor_row, _) = input_visual(app, content_w);
    visual
        .len()
        .max(cursor_row + 1)
        .clamp(INPUT_MIN_ROWS as usize, INPUT_MAX_ROWS as usize) as u16
}

fn draw_transcript(frame: &mut Frame, app: &App, area: Rect, content_w: u16) {
    let lines = transcript_lines(app, content_w);
    let start = app.viewport.scroll().min(lines.len());
    let end = (start + area.height as usize).min(lines.len());
    frame.render_widget(Paragraph::new(lines[start..end].to_vec()), area);
}

fn draw_separator(frame: &mut Frame, area: Rect) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let unit = if locale_utf8() { "─" } else { "-" };
    let line = unit.repeat(area.width as usize);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(line, role_style(Role::DimMeta)))),
        area,
    );
}

fn draw_input(frame: &mut Frame, app: &App, area: Rect, content_w: u16) {
    let (visual, cursor_row, cursor_col) = input_visual(app, content_w);
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

fn draw_prompt_tag(frame: &mut Frame, area: Rect) {
    if area.height == 0 {
        return;
    }
    let x = area.x.saturating_add(GUTTER.min(area.width));
    let width = area.width.saturating_sub(GUTTER).min(6);
    if width == 0 {
        return;
    }
    let tag = truncate_display("PROMPT", width as usize);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(tag, prompt_style()))).style(prompt_style()),
        Rect::new(x, area.y, width, 1),
    );
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
        phase_text(app.phase),
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

fn draw_bottom_status(frame: &mut Frame, app: &App, area: Rect) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let gutter = area.width.min(GUTTER);
    let tag_width = area.width.saturating_sub(gutter).min(6);
    if tag_width > 0 {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                truncate_display("PROMPT", tag_width as usize),
                prompt_style(),
            )))
            .style(prompt_style()),
            Rect::new(area.x.saturating_add(gutter), area.y, tag_width, 1),
        );
    }

    let status_x = area.x.saturating_add(gutter).saturating_add(tag_width);
    let status_width = area.width.saturating_sub(gutter).saturating_sub(tag_width);
    if status_width == 0 {
        return;
    }
    let line = bottom_status_line(app, status_width as usize);
    frame.render_widget(
        Paragraph::new(line).style(status_surface_style()),
        Rect::new(status_x, area.y, status_width, 1),
    );
}

fn bottom_status_line(app: &App, width: usize) -> Line<'static> {
    let phase = phase_text(app.phase);
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
    use crate::app::{App, Block, StatusKind};
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
            Block::Status(StatusKind::Committed {
                commit_id: "abc".into(),
            }),
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
    fn input_height_clamps_and_cursor_wraps_cjk() {
        let mut app = test_app();
        assert_eq!(input_visual_height(&app, 10), 2);
        app.input.set_text("a\nb\nc");
        assert_eq!(input_visual_height(&app, 10), 3);
        app.input.set_text("a\nb\nc\nd\ne\nf");
        assert_eq!(input_visual_height(&app, 10), 5);
        assert_eq!(visual_cursor("中文", 1, 2), (1, 0));
    }

    #[test]
    fn input_prompt_appears_only_on_first_visual_line() -> anyhow::Result<()> {
        let mut app = test_app();
        app.input.set_text("abcd");
        let mut terminal = Terminal::new(TestBackend::new(8, 4))?;
        terminal.draw(|frame| draw_input(frame, &app, Rect::new(0, 0, 8, 2), 6))?;
        let screen = buffer_text(&terminal, 8, 2);
        assert_eq!(screen.matches('>').count(), 1);
        assert_eq!(
            terminal.get_cursor_position()?,
            ratatui::layout::Position::new(6, 0)
        );
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
    fn wide_and_narrow_frames_draw_without_panic() -> anyhow::Result<()> {
        for (width, height) in [(1, 1), (7, 3), (56, 47), (80, 24), (115, 47), (176, 50)] {
            let mut app = test_app();
            app.blocks
                .push(Block::Status(StatusKind::Notice(vec!["hello".into()])));
            app.input.set_text("typed");
            let mut terminal = Terminal::new(TestBackend::new(width, height))?;
            terminal.draw(|frame| draw(frame, &app))?;
        }
        Ok(())
    }
}

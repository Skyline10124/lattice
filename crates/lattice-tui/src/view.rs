// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::{App, Block, StatusKind, ToolState, TurnPhase};
use crate::input::CTRL_C_WINDOW;

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

    /// Content grew to `content_rows`; viewport height is `height`.
    pub(crate) fn on_content_changed(&mut self, content_rows: usize, height: usize) {
        if self.follow {
            self.scroll = content_rows.saturating_sub(height);
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

/// Style slots. Exactly five roles, terminal semantic colors only — no RGB.
pub(crate) enum Role {
    User,
    Assistant,
    ToolOk,
    ToolErr,
    DimMeta,
}

#[rustfmt::skip]
pub(crate) fn role_style(role: Role) -> Style {
    Style::default().fg(match role {
        Role::User => Color::Cyan,
        Role::Assistant => Color::White,
        Role::ToolOk => Color::Green,
        Role::ToolErr => Color::Red,
        Role::DimMeta => Color::DarkGray,
    })
}

/// Wrap one block into styled lines at width `w`.
#[rustfmt::skip]
pub(crate) fn block_lines(block: &Block, w: u16) -> Vec<Line<'static>> {
    let u = locale_utf8();
    match block {
        Block::User(text) => wrap_styled(text, w, role_style(Role::User)),
        Block::Assistant(text) => wrap_styled(text, w, role_style(Role::Assistant)),
        Block::Tool { name, state, .. } => {
            let (kind, extra, role) = match state {
                ToolState::Running => (SymbolKind::Running, " (running)".into(), Role::DimMeta),
                ToolState::Ok { output_len } => (SymbolKind::Ok, format!("  {output_len} chars"), Role::ToolOk),
                ToolState::Failed { output_len, error } => {
                    let mut extra = format!("  {output_len} chars");
                    if let Some(err) = error { extra.push(' '); extra.push_str(err); }
                    (SymbolKind::Failed, extra, Role::ToolErr)
                }
            };
            wrap_styled(&format!("  {} {name}{extra}", symbol(kind, u)), w, role_style(role))
        }
        Block::Status(kind) => status_block_lines(kind, w, u),
    }
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

pub(crate) enum SymbolKind {
    Running,
    Ok,
    Failed,
    Cancelled,
}

pub(crate) fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let input_h = input_visual_height(app, area.width);
    let chunks = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(input_h),
        Constraint::Length(1),
    ])
    .split(area);
    draw_transcript(frame, app, chunks[0]);
    draw_input(frame, app, chunks[1]);
    draw_status(frame, app, chunks[2]);
}

#[rustfmt::skip]
pub(crate) fn locale_utf8() -> bool {
    ["LC_ALL", "LANG"].iter().any(|key| {
        std::env::var(key).ok().is_some_and(|v| v.to_ascii_uppercase().contains("UTF-8"))
    })
}

#[rustfmt::skip]
fn status_block_lines(kind: &StatusKind, w: u16, u: bool) -> Vec<Line<'static>> {
    let dim = role_style(Role::DimMeta);
    let err = role_style(Role::ToolErr);
    let ok = role_style(Role::ToolOk);
    match kind {
        StatusKind::Committed { commit_id } => {
            wrap_styled(&format!("  {} committed {commit_id}", symbol(SymbolKind::Ok, u)), w, ok)
        }
        StatusKind::CommitFailed { reason } => {
            wrap_styled(&format!("  {} commit failed: {reason}", symbol(SymbolKind::Failed, u)), w, err)
        }
        StatusKind::TurnFailed { reason } => {
            wrap_styled(&format!("  {} {reason}", symbol(SymbolKind::Failed, u)), w, err)
        }
        StatusKind::Cancelled { reason } => {
            let mark = symbol(SymbolKind::Cancelled, u);
            let text = match reason {
                Some(r) => format!("  {mark} cancelled: {r}"),
                None => format!("  {mark} cancelled"),
            };
            wrap_styled(&text, w, err)
        }
        StatusKind::LlmRetry { attempt, max_attempts, delay_ms, message } => wrap_styled(
            &format!("  {} llm retry {attempt}/{max_attempts} after {delay_ms}ms: {message}", symbol(SymbolKind::Running, u)),
            w, dim,
        ),
        StatusKind::LlmFailed { attempt, max_attempts, message } => wrap_styled(
            &format!("  {} llm failed {attempt}/{max_attempts}: {message}", symbol(SymbolKind::Failed, u)),
            w, err,
        ),
        StatusKind::Notice(lines) => lines.iter().flat_map(|line| wrap_styled(line, w, dim)).collect(),
    }
}

#[rustfmt::skip]
fn wrap_styled(text: &str, w: u16, style: Style) -> Vec<Line<'static>> {
    wrap_text(text, w).into_iter().map(|s| Line::from(Span::styled(s, style))).collect()
}

fn wrap_text(text: &str, w: u16) -> Vec<String> {
    let w = w.max(1) as usize;
    let mut out = Vec::new();
    for paragraph in text.split('\n') {
        if paragraph.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut cur = String::new();
        for ch in paragraph.chars() {
            if cur.chars().count() >= w {
                out.push(std::mem::take(&mut cur));
            }
            cur.push(ch);
        }
        out.push(cur);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

#[rustfmt::skip]
fn input_visual_height(app: &App, width: u16) -> u16 {
    let w = width.max(1) as usize;
    let rows: usize = app.input.lines.iter().map(|l| l.chars().count().div_ceil(w).max(1)).sum();
    rows.clamp(1, 6) as u16
}

#[rustfmt::skip]
fn draw_transcript(frame: &mut Frame, app: &App, area: Rect) {
    let lines: Vec<Line<'static>> = app.blocks.iter().flat_map(|b| block_lines(b, area.width)).collect();
    let start = app.viewport.scroll().min(lines.len());
    let end = (start + area.height as usize).min(lines.len());
    frame.render_widget(Paragraph::new(lines[start..end].to_vec()), area);
}

#[rustfmt::skip]
fn draw_input(frame: &mut Frame, app: &App, area: Rect) {
    let w = area.width.max(1) as usize;
    let mut visual = Vec::new();
    let mut cursor_row = 0usize;
    let mut cursor_col = 0usize;
    for (i, line) in app.input.lines.iter().enumerate() {
        let chars: Vec<char> = line.chars().collect();
        let wrapped = chars.len().max(1).div_ceil(w).max(1);
        if i < app.input.cursor.0 {
            cursor_row += wrapped;
        } else if i == app.input.cursor.0 {
            cursor_row += app.input.cursor.1 / w;
            cursor_col = app.input.cursor.1 % w;
        }
        if chars.is_empty() {
            visual.push(String::new());
        } else {
            visual.extend(chars.chunks(w).map(|c| c.iter().collect::<String>()));
        }
    }
    let h = area.height as usize;
    let scroll = cursor_row.saturating_sub(h.saturating_sub(1));
    let shown: Vec<Line> = visual.iter().skip(scroll).take(h).map(|s| Line::from(s.clone())).collect();
    frame.render_widget(Paragraph::new(shown), area);
    let screen_y = (cursor_row - scroll) as u16;
    if screen_y < area.height {
        frame.set_cursor_position((area.x + cursor_col as u16, area.y + screen_y));
    }
}

#[rustfmt::skip]
fn draw_status(frame: &mut Frame, app: &App, area: Rect) {
    let nl = if app.kitty_keyboard { "Shift+Enter 换行" } else { "Alt+Enter 换行" };
    let phase = match app.phase {
        TurnPhase::Idle => "idle",
        TurnPhase::Streaming => "streaming",
        TurnPhase::Committing => "committing",
        TurnPhase::Cancelling => "cancelling",
    };
    let ctrl = if app.last_ctrl_c.is_some_and(|t| t.elapsed() <= CTRL_C_WINDOW) {
        "  再按一次 Ctrl+C 退出"
    } else {
        ""
    };
    let follow = if app.viewport.following() { "" } else { "  [scroll]" };
    let text = format!("{}  {}  {phase}  {nl}{ctrl}{follow}", app.ctx.session_id, app.ctx.active_agent);
    frame.render_widget(Paragraph::new(Line::from(Span::styled(text, role_style(Role::DimMeta)))), area);
}

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
use crate::{markdown, wrapping};

mod dock;
mod statusline;
pub(crate) use dock::{Dock, DockIntent};

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
        (self.rows, self.height) = (content_rows, height);
        let max = content_rows.saturating_sub(height);
        if self.follow {
            self.scroll = max;
        } else {
            self.scroll = self.scroll.min(max);
        }
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
            draw_left(frame, app, columns[0], geometry.content_w);
            frame.render_widget(Paragraph::new("").style(status_surface_style()), columns[2]);
        }
        Sidebar::Hidden => {
            draw_left(frame, app, area, geometry.content_w);
        }
    }
}

fn draw_left(frame: &mut Frame, app: &App, area: Rect, content_w: u16) {
    let dock = Dock::new(app);
    let bottom_h = 1.min(area.height);
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
        statusline::render(
            frame,
            app,
            Rect::new(area.x, bottom_y, area.width, bottom_h),
        );
    }
}

/// One source of truth for transcript rendering and viewport row counts.
pub(crate) fn transcript_lines(app: &App, content_w: u16) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut assistant_header = false;
    let mut agent = app.ctx.active_agent.as_str();
    for block in &app.blocks {
        match block {
            Block::Step(_) | Block::UserTurn(_) => {}
            Block::AgentName(name) => {
                agent = name;
                assistant_header = false;
            }
            Block::User(text) => {
                separate(&mut lines);
                lines.extend(body("user", content_w, role_style(Role::User)));
                lines.extend(with_gutter(markdown::render(text, content_w)));
                assistant_header = false;
            }
            Block::Assistant(text) => {
                ensure_assistant_header(&mut lines, &mut assistant_header, agent, content_w);
                lines.extend(with_gutter(markdown::render(text, content_w)));
            }
            Block::Tool { name, state, .. } => {
                ensure_assistant_header(&mut lines, &mut assistant_header, agent, content_w);
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
                    kind => {
                        ensure_assistant_header(
                            &mut lines,
                            &mut assistant_header,
                            agent,
                            content_w,
                        );
                        lines.extend(status_lines(kind, content_w));
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
    with_gutter(wrapping::wrap(
        vec![Span::styled(
            wrapping::expand_tabs(text).into_owned(),
            style,
        )],
        usize::from(content_w),
    ))
}

fn with_gutter(lines: Vec<Line<'static>>) -> Vec<Line<'static>> {
    lines
        .into_iter()
        .map(|mut line| {
            if !line.spans.is_empty() {
                line.spans.insert(0, Span::raw("  "));
            }
            line
        })
        .collect()
}

fn tool_heading(spans: Vec<Span<'static>>, expanded: bool, content_w: u16) -> Vec<Line<'static>> {
    let mut lines = with_gutter(wrapping::wrap(spans, usize::from(content_w)));
    if let Some(first) = lines.first_mut() {
        first.spans[0] = Span::styled(
            format!("{} ", fold_symbol(expanded)),
            role_style(Role::DimMeta),
        );
    }
    lines
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
                lines.extend(body(error, content_w, err));
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

fn draw_transcript(frame: &mut Frame, app: &App, area: Rect, content_w: u16) {
    let lines = transcript_lines(app, content_w);
    let start = app.viewport.scroll().min(lines.len());
    let end = (start + area.height as usize).min(lines.len());
    frame.render_widget(Paragraph::new(lines[start..end].to_vec()), area);
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

/// 不包含 gutter；调用方把标题线放在正文区域。
pub(crate) fn title_line(title: &str, role: Role, right: &[String], width: usize) -> Line<'static> {
    let dim = role_style(Role::DimMeta);
    if width < 5 {
        return Line::from(Span::styled("─".repeat(width), dim));
    }
    let mut right = right;
    while !right.is_empty()
        && 9 + display_width(title) + joined_width(right.iter().map(String::as_str)) > width
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
    let remaining = width.saturating_sub(4 + display_width(&title));
    if right.is_empty() {
        spans.push(Span::styled("─".repeat(remaining), dim));
    } else {
        let right = right.join(" · ");
        spans.push(Span::styled(
            "─".repeat(remaining.saturating_sub(display_width(&right) + 4)),
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
    fn left_column_has_statusline_with_or_without_empty_full_height_sidebar() -> anyhow::Result<()>
    {
        for (w, h) in [(80, 24), (176, 50), (1, 1), (7, 3)] {
            let mut app = test_app();
            app.term_width = w;
            app.term_height = h;
            let geometry = frame_layout(w, h);
            let mut terminal = Terminal::new(TestBackend::new(w, h))?;
            terminal.draw(|f| draw(f, &app))?;
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
    fn transcript_groups_one_agent_header_and_shares_row_count() {
        let mut app = test_app();
        app.blocks.extend([
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
                    turn_id: lattice_api::local::new_turn_id(),
                    summary: lattice_api::TurnSummary {
                        outcome: lattice_api::local::PublicTurnOutcome::Completed,
                        notice: None,
                    },
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
            (1, vec!["> draft"]),
            (2, vec!["  ── USER", "> draft"]),
            (3, vec!["", "  ── USER", "> draft"]),
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
                ratatui::layout::Position::new(7, height - 1)
            );
        }
        Ok(())
    }

    #[test]
    fn dock_height_clamps_input_rows_one_to_ten() {
        let mut app = test_app();
        assert_eq!(Dock::new(&app).height(10), 3, "空输入：空行、标题、内容");
        app.input.set_text("a\nb\nc");
        assert_eq!(Dock::new(&app).height(10), 5);
        app.input.set_text(&["x"; 12].join("\n"));
        assert_eq!(Dock::new(&app).height(10), 11, "24 行终端 Dock 上限为 11");
        app.term_height = 50;
        assert_eq!(Dock::new(&app).height(10), 12, "最多 10 行内容");
        for (height, expected) in [
            (0, 0),
            (1, 0),
            (2, 0),
            (3, 1),
            (4, 1),
            (5, 2),
            (6, 2),
            (7, 3),
        ] {
            app.term_height = height;
            assert_eq!(Dock::new(&app).height(10), expected, "frame_h={height}");
        }
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
                    .starts_with(&format!("  ── {label} "))
            );
            assert!(screen.contains("Space 返回输入"));
            assert!(screen.contains("fifth") && !screen.contains("sixth"));
            let (title, text) = if severity == DockSeverity::Warn {
                (Role::Warn, Role::Assistant)
            } else {
                (Role::ToolErr, Role::ToolErr)
            };
            assert_eq!(terminal.backend().buffer()[(5, 1)].fg, role_color(title));
            assert_eq!(terminal.backend().buffer()[(2, 2)].fg, role_color(text));
        }
        Ok(())
    }

    #[test]
    fn statusline_orders_fields_and_focus_hints_without_session_id() {
        let mut app = test_app();
        let line = statusline::line(&app, 40);
        assert_eq!(line.to_string(), " agent            Alt+Enter 换行 · idle ");
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
        ];
        let lines = transcript_lines(&app, 10);
        assert_eq!(lines.len(), app.content_rows(10));
        assert!(matches!(&app.blocks[0], Block::User(raw) if raw == source));
        let mut terminal = Terminal::new(TestBackend::new(12, lines.len() as u16))?;
        terminal.draw(|frame| draw_transcript(frame, &app, frame.area(), 10))?;
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
            7,
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
        assert_eq!(dock.height(9), 2 + expected.len() as u16);
        let mut terminal = Terminal::new(TestBackend::new(11, dock.height(9)))?;
        terminal.draw(|frame| dock.render(frame, frame.area()))?;
        let screen = buffer_text(&terminal, 11, dock.height(9));
        assert_eq!(
            screen
                .lines()
                .skip(2)
                .map(str::trim_end)
                .collect::<Vec<_>>(),
            ["  ✗ one", "  **three**", "  two"]
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
            ["  one", "  three"]
        );
    }

    #[test]
    fn table_minimum_width_is_clipped_by_paragraph_without_extra_rows() -> anyhow::Result<()> {
        let mut app = test_app();
        app.blocks = vec![Block::User(
            "| aa | bb |\n| --- | --- |\n| cc | dd |".into(),
        )];
        let lines = transcript_lines(&app, 8);
        assert_eq!(
            lines[1].width(),
            13,
            "two 2-cell columns plus frames and gutter"
        );
        assert_eq!(app.content_rows(8), 6);
        let mut terminal = Terminal::new(TestBackend::new(10, 6))?;
        terminal.draw(|frame| draw_transcript(frame, &app, frame.area(), 8))?;
        assert_eq!(
            buffer_text(&terminal, 10, 6).lines().collect::<Vec<_>>(),
            [
                "  user    ",
                "  ┌────┬──",
                "  │ aa │ b",
                "  ├────┼──",
                "  │ cc │ d",
                "  └────┴──",
            ]
        );
        Ok(())
    }
}

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame,
};
use unicode_width::UnicodeWidthStr;

use super::app::{App, SlashSuggestion};
use super::theme::Theme;
use super::widgets::{message::message_lines, statusline::Statusline};

const PROMPT_PREFIX: &str = "❯ ";

pub fn draw(f: &mut Frame, app: &App) {
    let theme = Theme::catppuccin_mocha();
    let size = f.area();
    f.render_widget(Clear, size);
    app.click_zones.borrow_mut().clear();

    if let Some(ref menu) = app.menu {
        render_menu_overlay(f, size, menu, &theme);
        return;
    }

    let suggestions = app.slash_suggestions();
    let bottom_height = bottom_height(app, suggestions.len(), size.height);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(bottom_height)])
        .split(size);

    render_transcript(f, chunks[0], app, &theme);
    let input_area = render_bottom(f, chunks[1], app, &theme, &suggestions);
    set_cursor(f, input_area, app);
}

fn register_zone(app: &App, rect: (u16, u16, u16, u16), action: super::app::ClickAction) {
    app.click_zones
        .borrow_mut()
        .push(super::app::ClickZone { rect, action });
}

fn bottom_height(app: &App, suggestion_count: usize, terminal_height: u16) -> u16 {
    let help: u16 = if app.help_open { 9 } else { 0 };
    let suggestions = if suggestion_count > 0 {
        suggestion_count.min(6) as u16 + 2
    } else {
        0
    };
    let composer = input_height(app).saturating_add(1);
    help.saturating_add(suggestions)
        .saturating_add(composer)
        .min(terminal_height.saturating_sub(1).max(4))
}

fn input_height(app: &App) -> u16 {
    let line_count = app.input.lines().count().max(1) as u16;
    line_count.saturating_add(2).clamp(3, 7)
}

fn render_transcript(f: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    if app.messages.is_empty() {
        render_welcome(f, area, app, theme);
        return;
    }

    let rows = transcript_lines(app, theme, area.width);
    let total_rows = rows.len();
    let viewport_height = area.height as usize;
    let max_scroll = total_rows.saturating_sub(viewport_height);
    let effective_scroll = app.scroll_offset.min(max_scroll);

    // Find last user message for sticky pin when scrolled up
    let last_user_msg = if effective_scroll > 0 {
        app.messages
            .iter()
            .rev()
            .find(|m| m.role == lattice_core::types::Role::User)
    } else {
        None
    };

    let pin_height: u16 = if last_user_msg.is_some() { 1 } else { 0 };

    // Render sticky user message pin at top
    if let Some(user_msg) = last_user_msg {
        let pin_area = Rect::new(area.x, area.y, area.width, 1);
        let pin_text: String = user_msg.content.lines().next().unwrap_or("").to_string();
        let pin_display = format!(
            "● {} ›",
            truncate_to_width(&pin_text, area.width.saturating_sub(6) as usize)
        );
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    pin_display,
                    Style::default()
                        .fg(theme.user_accent)
                        .bg(theme.surface)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    " ".repeat(area.width as usize),
                    Style::default().bg(theme.surface),
                ),
            ])),
            pin_area,
        );
        register_zone(
            app,
            (pin_area.x, pin_area.y, pin_area.width, pin_area.height),
            super::app::ClickAction::JumpToBottom,
        );
    }

    // Transcript viewport below pin
    let transcript_top = area.y.saturating_add(pin_height);
    let transcript_h = area.height.saturating_sub(pin_height);
    let viewport = Rect::new(area.x, transcript_top, area.width, transcript_h);
    if viewport.width == 0 || viewport.height == 0 {
        return;
    }

    let view_h = viewport.height as usize;
    let max_scroll = total_rows.saturating_sub(view_h);
    let effective_scroll = effective_scroll.min(max_scroll);
    let end = total_rows.saturating_sub(effective_scroll);
    let start = end.saturating_sub(view_h);

    // Populate plain-text snapshot for mouse selection copy
    app.visible_rows.borrow_mut().clear();
    app.visible_rows_origin.set(viewport.y);
    for line in &rows[start..end] {
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        app.visible_rows.borrow_mut().push(text);
    }

    // Build visible rows with optional selection highlight
    let sel_range: Option<(u16, u16)> = app
        .selection
        .as_ref()
        .filter(|s| s.active)
        .map(|s| (s.start_row.min(s.end_row), s.start_row.max(s.end_row)));
    let highlight_style = Style::default().bg(theme.highlight).fg(theme.bg);
    let mut styled_rows: Vec<Line<'static>> = Vec::new();
    for (i, line) in rows[start..end].iter().enumerate() {
        let row_y = area.y.saturating_add(pin_height).saturating_add(i as u16);
        let highlighted = sel_range.is_some_and(|(top, bot)| row_y >= top && row_y <= bot);
        if highlighted {
            let spans: Vec<Span> = line
                .spans
                .iter()
                .map(|s| Span::styled(s.content.to_string(), s.style.patch(highlight_style)))
                .collect();
            styled_rows.push(Line::from(spans));
        } else {
            let spans: Vec<Span> = line
                .spans
                .iter()
                .map(|s| Span::styled(s.content.to_string(), s.style))
                .collect();
            styled_rows.push(Line::from(spans));
        }
    }

    f.render_widget(Paragraph::new(Text::from(styled_rows)), viewport);

    if effective_scroll > 0 && pin_height == 0 {
        let marker = format!("↑ {} lines above latest  End/click=jump", effective_scroll);
        let marker_area = Rect::new(area.x, area.y, area.width, 1);
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("↑ ", Style::default().fg(theme.highlight)),
                Span::styled(marker, Style::default().fg(theme.subtext)),
            ]))
            .alignment(Alignment::Right),
            marker_area,
        );
        register_zone(
            app,
            (
                marker_area.x,
                marker_area.y,
                marker_area.width,
                marker_area.height,
            ),
            super::app::ClickAction::JumpToBottom,
        );
    }
}

fn render_menu_overlay(f: &mut Frame, area: Rect, menu: &super::app::MenuState, theme: &Theme) {
    let title = match menu.kind {
        super::app::MenuKind::Model => " Select Model ",
        super::app::MenuKind::Provider => " Select Provider ",
        super::app::MenuKind::Session => " Select Session ",
    };
    let max_h = (menu.options.len() as u16 + 3).min(area.height.saturating_sub(4));
    let max_w = 50u16.min(area.width.saturating_sub(4));
    let menu_area = Rect {
        x: area
            .x
            .saturating_add((area.width.saturating_sub(max_w)) / 2),
        y: area
            .y
            .saturating_add((area.height.saturating_sub(max_h)) / 2),
        width: max_w,
        height: max_h,
    };

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(theme.border_style())
        .style(Style::default().bg(theme.surface));
    let inner = block.inner(menu_area);
    f.render_widget(Clear, menu_area);
    f.render_widget(block, menu_area);

    let visible_h = inner.height as usize;
    let start = menu
        .index
        .saturating_sub(visible_h.saturating_sub(1))
        .min(menu.options.len().saturating_sub(visible_h));
    let visible: Vec<&str> = menu
        .options
        .iter()
        .skip(start)
        .take(visible_h)
        .map(|s| s.as_str())
        .collect();

    let lines: Vec<Line> = visible
        .iter()
        .enumerate()
        .map(|(i, opt)| {
            let real_idx = start + i;
            if real_idx == menu.index {
                Line::from(vec![
                    Span::styled(
                        " ▶ ",
                        Style::default()
                            .fg(theme.highlight)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        *opt,
                        Style::default()
                            .fg(theme.text)
                            .bg(theme.highlight)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        " ".repeat(
                            inner
                                .width
                                .saturating_sub(3 + UnicodeWidthStr::width(*opt) as u16)
                                as usize,
                        ),
                        Style::default().bg(theme.highlight),
                    ),
                ])
            } else {
                Line::from(vec![
                    Span::raw("   "),
                    Span::styled(*opt, Style::default().fg(theme.text)),
                ])
            }
        })
        .collect();

    f.render_widget(Paragraph::new(Text::from(lines)), inner);

    // Footer hint
    let hint = "↑↓ navigate  Enter select  Esc close";
    let hint_line = Line::from(vec![Span::styled(hint, Style::default().fg(theme.subtext))]);
    let hint_area = Rect {
        x: menu_area.x,
        y: menu_area
            .y
            .saturating_add(menu_area.height)
            .saturating_sub(1),
        width: menu_area.width,
        height: 1,
    };
    f.render_widget(
        Paragraph::new(hint_line).alignment(Alignment::Center),
        hint_area,
    );
}

fn truncate_to_width(s: &str, max_width: usize) -> String {
    let mut result = String::new();
    let mut w = 0usize;
    for ch in s.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if w + cw > max_width {
            break;
        }
        result.push(ch);
        w += cw;
    }
    result
}

fn transcript_lines(app: &App, theme: &Theme, width: u16) -> Vec<Line<'static>> {
    app.messages
        .iter()
        .flat_map(|msg| message_lines(msg, theme, app.transcript_mode, width))
        .collect()
}

fn render_welcome(f: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let inner = inset(area, 2, 1);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let provider = if app.current_provider.is_empty() {
        "unresolved"
    } else {
        app.current_provider.as_str()
    };

    let ban_style = Style::default().fg(theme.user_accent);
    let banner = if inner.width >= 40 {
        vec![
            Line::styled("  ██╗      █████╗ ████████╗████████╗██╗ ██████╗███████╗  ", ban_style),
            Line::styled("  ██║     ██╔══██╗╚══██╔══╝╚══██╔══╝██║██╔════╝██╔════╝  ", ban_style),
            Line::styled("  ██║     ███████║   ██║      ██║   ██║██║     █████╗    ", ban_style),
            Line::styled("  ██║     ██╔══██║   ██║      ██║   ██║██║     ██╔══╝    ", ban_style),
            Line::styled("  ███████╗██║  ██║   ██║      ██║   ██║╚██████╗███████╗  ", ban_style),
            Line::styled("  ╚══════╝╚═╝  ╚═╝   ╚═╝      ╚═╝   ╚═╝ ╚═════╝╚══════╝  ", ban_style),
        ]
    } else {
        vec![Line::from(vec![
            Span::styled(
                "●",
                Style::default()
                    .fg(theme.assistant_accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(
                "LATTICE",
                Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" · agent CLI", Style::default().fg(theme.subtext)),
        ])]
    };

    let tip = welcome_tip();

    let mut lines: Vec<Line> = Vec::new();
    lines.extend(banner);
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![Span::styled(
        format!("  model  {} @ {}", app.current_model.as_str(), provider),
        Style::default().fg(theme.subtext),
    )]));
    if let Some(ref e) = app.thinking_effort {
        lines.push(Line::from(vec![Span::styled(
            format!("  effort  {}", e),
            Style::default().fg(theme.subtext),
        )]));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled(
            "  Tip: ",
            Style::default()
                .fg(theme.highlight)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(tip, Style::default().fg(theme.subtext)),
    ]));
    lines.push(Line::raw(""));
    lines.extend(render_welcome_shortcuts(theme));

    f.render_widget(
        Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false }),
        inner,
    );
}

fn welcome_tip() -> &'static str {
    use std::sync::LazyLock;
    static TIP: LazyLock<&str> = LazyLock::new(|| {
        let tips = [
            "Type / to see all available commands.",
            "Use ! before a command to run it in bash (e.g. !ls).",
            "Ctrl+O toggles the thinking trace view.",
            "Ctrl+E expands collapsed tool outputs.",
            "Ctrl+Y copies the last assistant response.",
            "Click and drag to select text — auto-copied.",
            "Use /effort <level> to control thinking depth.",
            "Up/Down arrows navigate input history.",
            "/model <name> switches the LLM for the next turn.",
        ];
        let ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos();
        tips[(ns as usize) % tips.len()]
    });
    *TIP
}

fn render_welcome_shortcuts(theme: &Theme) -> Vec<Line<'static>> {
    let rows: Vec<(&str, &str)> = vec![
        ("Enter", "send message"),
        ("? /help", "show shortcuts"),
        ("/commands", "slash command menu"),
        ("Ctrl+N", "new session"),
        ("Ctrl+L", "clear transcript"),
        ("Ctrl+O", "toggle trace"),
        ("Ctrl+E", "expand tool output"),
        ("Ctrl+Y", "copy last response"),
        ("Ctrl+C", "quit"),
    ];
    rows.into_iter()
        .map(|(key, desc)| {
            Line::from(vec![
                Span::raw("  "),
                Span::styled(format!("{:<14}", key), Style::default().fg(theme.highlight)),
                Span::styled(desc, Style::default().fg(theme.subtext)),
            ])
        })
        .collect()
}

fn render_bottom(
    f: &mut Frame,
    area: Rect,
    app: &App,
    theme: &Theme,
    suggestions: &[SlashSuggestion],
) -> Rect {
    let help_height = if app.help_open { 9 } else { 0 };
    let suggestion_height = if suggestions.is_empty() {
        0
    } else {
        suggestions.len().min(6) as u16 + 2
    };
    let composer_height = input_height(app);

    let constraints = [
        Constraint::Length(help_height),
        Constraint::Length(suggestion_height),
        Constraint::Length(composer_height),
        Constraint::Length(1),
    ];
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    if help_height > 0 {
        render_help(f, chunks[0], theme);
    }
    if suggestion_height > 0 {
        render_suggestions(f, chunks[1], suggestions, theme);
    }
    render_composer(f, chunks[2], app, theme);
    Statusline::new(*theme).render(chunks[3], f.buffer_mut(), app);

    chunks[2]
}

fn render_help(f: &mut Frame, area: Rect, theme: &Theme) {
    if area.height == 0 {
        return;
    }

    let block = Block::default()
        .title(" shortcuts ")
        .borders(Borders::TOP)
        .border_style(theme.border_style())
        .style(Style::default().bg(theme.bg));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let rows = vec![
        help_row("Enter", "send", "Shift+Enter", "newline", theme),
        help_row("?", "toggle help", "/", "commands", theme),
        help_row("Ctrl+N", "new session", "Ctrl+L", "clear transcript", theme),
        help_row("Ctrl+O", "toggle trace", "Ctrl+E", "expand tool", theme),
        help_row("Esc", "clear/close", "End", "jump bottom", theme),
        help_row("↑/↓", "scroll", "Ctrl+C", "quit", theme),
    ];

    f.render_widget(Paragraph::new(Text::from(rows)), inner);
}

fn help_row<'a>(
    left_key: &'static str,
    left_desc: &'static str,
    right_key: &'static str,
    right_desc: &'static str,
    theme: &Theme,
) -> Line<'a> {
    Line::from(vec![
        Span::styled(
            format!("{left_key:<12}"),
            Style::default().fg(theme.highlight),
        ),
        Span::styled(
            format!("{left_desc:<18}"),
            Style::default().fg(theme.subtext),
        ),
        Span::styled(
            format!("{right_key:<12}"),
            Style::default().fg(theme.highlight),
        ),
        Span::styled(right_desc, Style::default().fg(theme.subtext)),
    ])
}

fn render_suggestions(f: &mut Frame, area: Rect, suggestions: &[SlashSuggestion], theme: &Theme) {
    if area.height == 0 {
        return;
    }

    let block = Block::default()
        .title(" commands ")
        .borders(Borders::TOP)
        .border_style(theme.border_style())
        .style(Style::default().bg(theme.bg));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let lines = suggestions
        .iter()
        .take(inner.height as usize)
        .map(|suggestion| {
            Line::from(vec![
                Span::styled(
                    format!("{:<15}", suggestion.command),
                    Style::default()
                        .fg(theme.highlight)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(suggestion.description, Style::default().fg(theme.subtext)),
            ])
        })
        .collect::<Vec<_>>();

    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}

fn render_composer(f: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    if area.height == 0 {
        return;
    }

    // CC-style: separator lines above and below the prompt area
    let block = Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_style(theme.border_style())
        .style(Style::default().bg(theme.bg));
    let inner = block.inner(area);
    f.render_widget(block, area);

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let input = if app.input.is_empty() {
        Text::from(Line::from(vec![
            Span::styled(PROMPT_PREFIX, Style::default().fg(theme.subtext)),
            Span::styled(
                "Type a message or / for commands",
                Style::default().fg(theme.subtext),
            ),
        ]))
    } else {
        prefixed_input(&app.input, theme)
    };

    f.render_widget(
        Paragraph::new(input)
            .style(theme.input_style())
            .wrap(Wrap { trim: false }),
        inner,
    );
}

fn prefixed_input<'a>(input: &'a str, theme: &Theme) -> Text<'a> {
    let mut lines = Vec::new();
    for (idx, line) in input.lines().enumerate() {
        if idx == 0 {
            lines.push(Line::from(vec![
                Span::styled(PROMPT_PREFIX, Style::default().fg(theme.assistant_accent)),
                Span::styled(line, Style::default().fg(theme.text)),
            ]));
        } else {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(line, Style::default().fg(theme.text)),
            ]));
        }
    }
    Text::from(lines)
}

fn set_cursor(f: &mut Frame, input_area: Rect, app: &App) {
    if input_area.width < 2 || input_area.height < 2 {
        return;
    }

    let text_before = &app.input[..app.input_cursor];
    let line_index = text_before.bytes().filter(|b| *b == b'\n').count() as u16;
    let current_line = text_before
        .rsplit_once('\n')
        .map_or(text_before, |(_, tail)| tail);
    let prefix_width = if line_index == 0 {
        UnicodeWidthStr::width(PROMPT_PREFIX) as u16
    } else {
        2
    };
    let visual_x = UnicodeWidthStr::width(current_line) as u16;
    let cursor_x = input_area
        .x
        .saturating_add(prefix_width)
        .saturating_add(visual_x.min(input_area.width.saturating_sub(prefix_width + 1)));
    let cursor_y = input_area
        .y
        .saturating_add(1) // top border
        .saturating_add(line_index.min(input_area.height.saturating_sub(2)));
    f.set_cursor_position((cursor_x, cursor_y));
}

fn inset(area: Rect, horizontal: u16, vertical: u16) -> Rect {
    Rect {
        x: area.x.saturating_add(horizontal),
        y: area.y.saturating_add(vertical),
        width: area.width.saturating_sub(horizontal.saturating_mul(2)),
        height: area.height.saturating_sub(vertical.saturating_mul(2)),
    }
}

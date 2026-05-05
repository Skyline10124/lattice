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

    let viewport = inset(area, 1, 1);
    if viewport.width == 0 || viewport.height == 0 {
        return;
    }

    let rows = transcript_lines(app, theme, viewport.width);
    let total_rows = rows.len();
    let viewport_height = viewport.height as usize;
    let max_scroll = total_rows.saturating_sub(viewport_height);
    let effective_scroll = app.scroll_offset.min(max_scroll);
    let end = total_rows.saturating_sub(effective_scroll);
    let start = end.saturating_sub(viewport_height);

    let visible_rows = rows[start..end].to_vec();
    f.render_widget(Paragraph::new(Text::from(visible_rows)), viewport);

    if effective_scroll > 0 {
        let marker = format!("{} lines above latest", effective_scroll);
        let marker_area = Rect::new(area.x, area.y, area.width, 1);
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("↑ ", Style::default().fg(theme.highlight)),
                Span::styled(marker, Style::default().fg(theme.subtext)),
            ]))
            .alignment(Alignment::Right),
            marker_area,
        );
    }
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

    let lines = vec![
        Line::from(vec![Span::styled(
            "LATTICE",
            Style::default()
                .fg(theme.assistant_accent)
                .add_modifier(Modifier::BOLD),
        )]),
        Line::from(vec![Span::styled(
            "agent CLI",
            Style::default().fg(theme.subtext),
        )]),
        Line::raw(""),
        Line::from(vec![
            Span::styled("model ", Style::default().fg(theme.subtext)),
            Span::styled(app.current_model.as_str(), Style::default().fg(theme.text)),
            Span::styled(" @ ", Style::default().fg(theme.subtext)),
            Span::styled(provider, Style::default().fg(theme.text)),
        ]),
        Line::raw(""),
        Line::from(vec![
            Span::styled("/help", Style::default().fg(theme.highlight)),
            Span::styled(" shortcuts   ", Style::default().fg(theme.subtext)),
            Span::styled("/model", Style::default().fg(theme.highlight)),
            Span::styled(" switch model   ", Style::default().fg(theme.subtext)),
            Span::styled("Ctrl+N", Style::default().fg(theme.highlight)),
            Span::styled(" new session", Style::default().fg(theme.subtext)),
        ]),
    ];

    f.render_widget(
        Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false }),
        inner,
    );
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
        .borders(Borders::ALL)
        .border_style(theme.border_style())
        .style(Style::default().bg(theme.bg));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let rows = vec![
        help_row("Enter", "send", "Shift+Enter", "newline", theme),
        help_row("?", "toggle help", "/", "commands", theme),
        help_row("Ctrl+N", "new session", "Ctrl+L", "clear transcript", theme),
        help_row("Ctrl+O", "toggle trace", "Esc", "clear or close", theme),
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
        .borders(Borders::ALL)
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

    let title = if app.help_open {
        " prompt · help open "
    } else if app.input.starts_with('/') {
        " command "
    } else {
        " prompt "
    };
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(theme.border_style())
        .style(theme.input_style());
    let inner = block.inner(area);
    f.render_widget(block, area);

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let input = if app.input.is_empty() {
        Text::from(Line::from(vec![
            Span::styled(PROMPT_PREFIX, Style::default().fg(theme.assistant_accent)),
            Span::styled(
                "Message LATTICE or type / for commands",
                Style::default()
                    .fg(theme.subtext)
                    .add_modifier(Modifier::ITALIC),
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
    let inner_width = input_area.width.saturating_sub(2);
    let cursor_x = input_area
        .x
        .saturating_add(1)
        .saturating_add(prefix_width)
        .saturating_add(visual_x.min(inner_width.saturating_sub(prefix_width + 1)));
    let cursor_y = input_area
        .y
        .saturating_add(1)
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

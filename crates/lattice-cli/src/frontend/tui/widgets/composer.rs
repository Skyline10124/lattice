use ratatui::{
    layout::Rect,
    style::Style,
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};
use unicode_width::UnicodeWidthStr;

use crate::frontend::tui::state::AppStatus;
use crate::frontend::tui::theme::Theme;
use crate::frontend::tui::view_model::TuiViewModel;

const PROMPT_PREFIX: &str = "❯ ";

pub(in crate::frontend::tui) fn input_height(vm: &TuiViewModel<'_>) -> u16 {
    let line_count = vm.composer.input.lines().count().max(1) as u16;
    line_count.saturating_add(2).clamp(3, 7)
}

pub(in crate::frontend::tui) fn set_cursor(f: &mut Frame, input_area: Rect, vm: &TuiViewModel<'_>) {
    if input_area.width < 2 || input_area.height < 2 {
        return;
    }

    let text_before = &vm.composer.input[..vm.composer.input_cursor];
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
        .saturating_add(1)
        .saturating_add(line_index.min(input_area.height.saturating_sub(2)));
    f.set_cursor_position((cursor_x, cursor_y));
}

pub(in crate::frontend::tui) fn render_help(f: &mut Frame, area: Rect, theme: &Theme) {
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
        help_row("?", "toggle help", "Tab", "complete cmd", theme),
        help_row("Ctrl+N", "new session", "Ctrl+L", "clear transcript", theme),
        help_row("Ctrl+F", "find", "Ctrl+G", "next match", theme),
        help_row("Ctrl+O", "thinking", "Ctrl+E", "expand tool", theme),
        help_row("/permissions", "sandbox", "/plugins", "plugins", theme),
        help_row("Esc", "clear/close", "End", "jump bottom", theme),
        help_row("↑/↓", "scroll/history", "Ctrl+C", "quit", theme),
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

pub(in crate::frontend::tui) fn render_composer(
    f: &mut Frame,
    area: Rect,
    vm: &TuiViewModel<'_>,
    theme: &Theme,
) {
    if area.height == 0 {
        return;
    }

    let block = Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_style(theme.border_style())
        .style(Style::default().bg(theme.bg));
    let inner = block.inner(area);
    f.render_widget(block, area);

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let placeholder = if *vm.statusline.status == AppStatus::Streaming {
        "Type now to queue the next prompt"
    } else if vm.search.is_active() {
        "Esc clears search · /next and /prev navigate matches"
    } else {
        "Type a message or / for commands"
    };

    let input = if vm.composer.input.is_empty() {
        Text::from(Line::from(vec![
            Span::styled(PROMPT_PREFIX, Style::default().fg(theme.subtext)),
            Span::styled(placeholder, Style::default().fg(theme.subtext)),
        ]))
    } else {
        prefixed_input(vm.composer.input, theme)
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

use ratatui::{
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};
use unicode_width::UnicodeWidthStr;

use super::super::app::{App, AppStatus};
use super::super::theme::Theme;

pub struct Statusline {
    theme: Theme,
}

impl Statusline {
    pub fn new(theme: Theme) -> Self {
        Self { theme }
    }

    pub fn render(&self, area: Rect, buf: &mut ratatui::buffer::Buffer, app: &App) {
        if area.height == 0 {
            return;
        }

        let provider = if app.current_provider.is_empty() {
            "unresolved"
        } else {
            app.current_provider.as_str()
        };
        let status_text = match &app.status {
            AppStatus::Ready => "ready".to_string(),
            AppStatus::Streaming => format!("{} streaming", app.spinner()),
            AppStatus::Error(err) => format!("error: {err}"),
        };
        let status_color = match &app.status {
            AppStatus::Ready => self.theme.success,
            AppStatus::Streaming => self.theme.assistant_accent,
            AppStatus::Error(_) => self.theme.error,
        };
        let trace = if app.transcript_mode {
            "trace on"
        } else {
            "trace off"
        };

        let left = Line::from(vec![
            Span::styled(
                format!(" {} ", app.mode_label()),
                Style::default()
                    .fg(self.theme.bg)
                    .bg(self.theme.highlight)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(app.current_model.as_str(), self.theme.statusline_style()),
            Span::styled(
                "@",
                Style::default()
                    .fg(self.theme.subtext)
                    .bg(self.theme.surface),
            ),
            Span::styled(provider, self.theme.statusline_style()),
            Span::raw("  "),
            Span::styled(
                status_text,
                Style::default().fg(status_color).bg(self.theme.surface),
            ),
            Span::raw("  "),
            Span::styled(
                format!("{} tok", app.token_count),
                Style::default()
                    .fg(self.theme.subtext)
                    .bg(self.theme.surface),
            ),
            Span::raw("  "),
            Span::styled(
                trace,
                Style::default()
                    .fg(self.theme.subtext)
                    .bg(self.theme.surface),
            ),
        ]);

        let right = if area.width >= 78 {
            Line::from(vec![
                Span::styled(
                    "? help",
                    Style::default()
                        .fg(self.theme.subtext)
                        .bg(self.theme.surface),
                ),
                Span::raw("  "),
                Span::styled(
                    "/ commands",
                    Style::default()
                        .fg(self.theme.subtext)
                        .bg(self.theme.surface),
                ),
                Span::raw("  "),
                Span::styled(
                    "Ctrl+N new",
                    Style::default()
                        .fg(self.theme.subtext)
                        .bg(self.theme.surface),
                ),
                Span::raw("  "),
                Span::styled(
                    "Ctrl+C quit ",
                    Style::default()
                        .fg(self.theme.subtext)
                        .bg(self.theme.surface),
                ),
            ])
        } else {
            Line::from(vec![
                Span::styled(
                    "? help",
                    Style::default()
                        .fg(self.theme.subtext)
                        .bg(self.theme.surface),
                ),
                Span::raw("  "),
                Span::styled(
                    "Ctrl+C quit ",
                    Style::default()
                        .fg(self.theme.subtext)
                        .bg(self.theme.surface),
                ),
            ])
        };

        let left_width = line_width(&left);
        let right_width = line_width(&right);
        let show_right =
            left_width.saturating_add(right_width).saturating_add(2) < area.width as usize;

        Paragraph::new(left)
            .style(self.theme.statusline_style())
            .render(area, buf);
        if show_right {
            Paragraph::new(right)
                .style(self.theme.statusline_style())
                .alignment(Alignment::Right)
                .render(area, buf);
        }
    }
}

fn line_width(line: &Line<'_>) -> usize {
    line.spans
        .iter()
        .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
        .sum()
}

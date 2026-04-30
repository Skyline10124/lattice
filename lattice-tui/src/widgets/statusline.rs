use ratatui::{
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Widget},
};

use crate::app::{App, AppStatus};
use crate::theme::Theme;

pub struct Statusline {
    theme: Theme,
}

impl Statusline {
    pub fn new(theme: Theme) -> Self {
        Self { theme }
    }

    pub fn render(&self, area: Rect, buf: &mut ratatui::buffer::Buffer, app: &App) {
        let status_icon = match app.status {
            AppStatus::Ready => "\u{2713}",
            AppStatus::Streaming => "\u{21BB}",
            AppStatus::Waiting => "\u{25EF}",
            AppStatus::Error(_) => "\u{2717}",
        };

        let status_color = match app.status {
            AppStatus::Ready => self.theme.success,
            AppStatus::Streaming => self.theme.assistant_accent,
            AppStatus::Waiting => self.theme.thinking,
            AppStatus::Error(_) => self.theme.error,
        };

        let model_provider = format!("{}@{}", app.current_model, app.current_provider);
        let tokens = format!("{} tok", app.token_count);

        let left = vec![
            Span::styled(
                model_provider.clone(),
                self.theme.statusline_style().add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(status_icon.to_string(), Style::default().fg(status_color)),
            Span::raw("  "),
            Span::styled(tokens.clone(), Style::default().fg(self.theme.subtext)),
        ];

        let right = vec![
            Span::styled("/help".to_string(), Style::default().fg(self.theme.subtext)),
            Span::raw("  "),
            Span::styled(
                "\u{F06A9}".to_string(),
                Style::default().fg(self.theme.highlight),
            ),
        ];

        let block = Block::default()
            .borders(Borders::TOP)
            .border_style(self.theme.border_style());

        let inner = block.inner(area);
        block.render(area, buf);

        // Render left part
        let left_para = Paragraph::new(Line::from(left));
        left_para.render(inner, buf);

        // Render right part (overlaid via alignment trick)
        let right_para = Paragraph::new(Line::from(right))
            .style(self.theme.statusline_style())
            .alignment(Alignment::Right);
        right_para.render(inner, buf);
    }
}

use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Paragraph, Widget},
};

use crate::app::ChatMessage;
use crate::theme::Theme;

pub struct MessageWidget<'a> {
    msg: &'a ChatMessage,
    theme: &'a Theme,
}

impl<'a> MessageWidget<'a> {
    pub fn new(msg: &'a ChatMessage, theme: &'a Theme) -> Self {
        Self { msg, theme }
    }

    pub fn render(&self, area: ratatui::layout::Rect, buf: &mut ratatui::buffer::Buffer) {
        let prefix = match self.msg.role {
            artemis_core::types::Role::User => "\u{F2BD} ",
            artemis_core::types::Role::Assistant => "\u{F120} ",
            artemis_core::types::Role::System => "\u{F013} ",
            artemis_core::types::Role::Tool => "\u{F0AD} ",
        };

        let style = match self.msg.role {
            artemis_core::types::Role::User => self.theme.user_style(),
            artemis_core::types::Role::Assistant => self.theme.assistant_style(),
            artemis_core::types::Role::System => Style::default().fg(self.theme.subtext),
            artemis_core::types::Role::Tool => self.theme.tool_style(),
        };

        let mut lines = vec![
            Line::from(vec![
                Span::styled(prefix.to_string(), style.add_modifier(Modifier::BOLD)),
                Span::styled(self.msg.content.clone(), style),
            ]),
        ];

        // Thinking block (collapsible, shown inline for MVP)
        if let Some(ref reasoning) = self.msg.reasoning {
            lines.push(Line::from(vec![
                Span::styled("\u{F0EB} ".to_string(), self.theme.thinking_style()),
                Span::styled(reasoning.clone(), self.theme.thinking_style()),
            ]));
        }

        let text = Text::from(lines);
        let para = Paragraph::new(text);
        para.render(area, buf);
    }
}

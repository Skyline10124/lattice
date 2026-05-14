use ratatui::style::{Color, Modifier, Style};

/// Catppuccin Mocha-inspired dark theme.
#[derive(Debug, Clone, Copy)]
pub(super) struct Theme {
    pub(super) bg: Color,
    pub(super) surface: Color,
    pub(super) text: Color,
    pub(super) subtext: Color,
    pub(super) user_accent: Color,
    pub(super) assistant_accent: Color,
    pub(super) tool_accent: Color,
    pub(super) thinking: Color,
    pub(super) error: Color,
    pub(super) success: Color,
    pub(super) border: Color,
    pub(super) highlight: Color,
}

impl Theme {
    pub(super) fn catppuccin_mocha() -> Self {
        Self {
            bg: Color::Rgb(30, 30, 46),                  // base
            surface: Color::Rgb(49, 50, 68),             // surface0
            text: Color::Rgb(205, 214, 244),             // text
            subtext: Color::Rgb(166, 173, 200),          // subtext1
            user_accent: Color::Rgb(180, 190, 254),      // lavender
            assistant_accent: Color::Rgb(137, 180, 250), // blue
            tool_accent: Color::Rgb(249, 226, 175),      // yellow
            thinking: Color::Rgb(147, 153, 178),         // overlay2
            error: Color::Rgb(243, 139, 168),            // red
            success: Color::Rgb(166, 227, 161),          // green
            border: Color::Rgb(88, 91, 112),             // surface2
            highlight: Color::Rgb(203, 166, 247),        // mauve
        }
    }

    pub(super) fn user_style(&self) -> Style {
        Style::default().fg(self.user_accent)
    }

    pub(super) fn assistant_style(&self) -> Style {
        Style::default().fg(self.text)
    }

    pub(super) fn thinking_style(&self) -> Style {
        Style::default()
            .fg(self.thinking)
            .add_modifier(Modifier::ITALIC)
    }

    pub(super) fn tool_style(&self) -> Style {
        Style::default().fg(self.tool_accent)
    }

    pub(super) fn statusline_style(&self) -> Style {
        Style::default().fg(self.text).bg(self.surface)
    }

    pub(super) fn input_style(&self) -> Style {
        Style::default().fg(self.text).bg(self.bg)
    }

    pub(super) fn border_style(&self) -> Style {
        Style::default().fg(self.border)
    }

    // -- markdown styles --

    pub(super) fn heading_style(&self, level: u8) -> Style {
        let base = Style::default().add_modifier(Modifier::BOLD);
        match level {
            1 => base.fg(self.highlight),
            2 => base.fg(self.user_accent),
            _ => base.fg(self.assistant_accent),
        }
    }

    pub(super) fn bold_style(&self) -> Style {
        Style::default().add_modifier(Modifier::BOLD)
    }

    pub(super) fn italic_style(&self) -> Style {
        Style::default().add_modifier(Modifier::ITALIC)
    }

    pub(super) fn inline_code_style(&self) -> Style {
        Style::default().fg(self.tool_accent).bg(self.surface)
    }

    pub(super) fn code_block_style(&self) -> Style {
        Style::default().bg(self.surface)
    }

    pub(super) fn blockquote_style(&self) -> Style {
        Style::default().fg(self.subtext)
    }

    pub(super) fn link_style(&self) -> Style {
        Style::default()
            .fg(self.assistant_accent)
            .add_modifier(Modifier::UNDERLINED)
    }

    pub(super) fn list_marker_style(&self) -> Style {
        Style::default().fg(self.tool_accent)
    }

    pub(super) fn code_block_border_style(&self) -> Style {
        Style::default().fg(self.border)
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::catppuccin_mocha()
    }
}

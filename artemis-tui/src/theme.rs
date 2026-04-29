use ratatui::style::{Color, Modifier, Style};

/// Catppuccin Mocha-inspired dark theme.
pub struct Theme {
    pub bg: Color,
    pub surface: Color,
    pub overlay: Color,
    pub text: Color,
    pub subtext: Color,
    pub user_accent: Color,
    pub assistant_accent: Color,
    pub tool_accent: Color,
    pub thinking: Color,
    pub error: Color,
    pub success: Color,
    pub border: Color,
    pub highlight: Color,
}

impl Theme {
    pub fn catppuccin_mocha() -> Self {
        Self {
            bg: Color::Rgb(30, 30, 46),         // base
            surface: Color::Rgb(49, 50, 68),    // surface0
            overlay: Color::Rgb(69, 71, 90),    // surface1
            text: Color::Rgb(205, 214, 244),    // text
            subtext: Color::Rgb(166, 173, 200), // subtext1
            user_accent: Color::Rgb(180, 190, 254), // lavender
            assistant_accent: Color::Rgb(137, 180, 250), // blue
            tool_accent: Color::Rgb(249, 226, 175), // yellow
            thinking: Color::Rgb(147, 153, 178),    // overlay2
            error: Color::Rgb(243, 139, 168),   // red
            success: Color::Rgb(166, 227, 161), // green
            border: Color::Rgb(88, 91, 112),    // surface2
            highlight: Color::Rgb(203, 166, 247), // mauve
        }
    }

    pub fn user_style(&self) -> Style {
        Style::default().fg(self.user_accent)
    }

    pub fn assistant_style(&self) -> Style {
        Style::default().fg(self.text)
    }

    pub fn thinking_style(&self) -> Style {
        Style::default().fg(self.thinking).add_modifier(Modifier::ITALIC)
    }

    pub fn tool_style(&self) -> Style {
        Style::default().fg(self.tool_accent)
    }

    pub fn error_style(&self) -> Style {
        Style::default().fg(self.error)
    }

    pub fn statusline_style(&self) -> Style {
        Style::default().fg(self.text).bg(self.surface)
    }

    pub fn input_style(&self) -> Style {
        Style::default().fg(self.text).bg(self.bg)
    }

    pub fn border_style(&self) -> Style {
        Style::default().fg(self.border)
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::catppuccin_mocha()
    }
}

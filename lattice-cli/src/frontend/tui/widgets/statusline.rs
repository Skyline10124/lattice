use ratatui::{
    layout::Rect,
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
            AppStatus::Streaming => {
                let elapsed = app.stream_started.map_or(0, |s| s.elapsed().as_secs());
                let elapsed_str = format_duration(elapsed);
                if let (Some(rs), None) = (app.reasoning_started, app.reasoning_duration) {
                    let thinking_elapsed = rs.elapsed().as_secs();
                    format!(
                        "Thinking… {} · thinking {}",
                        elapsed_str,
                        format_duration(thinking_elapsed)
                    )
                } else if let Some(dur) = app.reasoning_duration {
                    format!(
                        "Streaming {} · thought for {}",
                        elapsed_str,
                        format_duration(dur.as_secs())
                    )
                } else {
                    format!("{} streaming {}", app.spinner(), elapsed_str)
                }
            }
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
        let effort = app.thinking_effort.as_deref().unwrap_or("auto");

        let pill_style = Style::default()
            .fg(self.theme.bg)
            .bg(self.theme.highlight)
            .add_modifier(Modifier::BOLD);
        let dim_style = Style::default().fg(self.theme.subtext);
        let body_style = Style::default().fg(self.theme.text);

        // Build a single line with consistent bg=surface on all non-pill spans
        let mut spans = vec![
            Span::styled(format!(" {} ", app.mode_label()), pill_style),
            Span::raw(" "),
            Span::styled(app.current_model.as_str(), body_style),
            Span::styled("@", dim_style),
            Span::styled(provider, body_style),
            Span::raw("  "),
            Span::styled(status_text, Style::default().fg(status_color)),
            Span::raw("  "),
            Span::styled(format!("{} tok", app.token_count), dim_style),
            Span::raw("  "),
            Span::styled(
                ctx_bar(app.token_count as u32, app.context_limit),
                dim_style,
            ),
            Span::raw("  "),
            Span::styled(trace, dim_style),
            Span::raw("  "),
            Span::styled(format!("effort:{}", effort), dim_style),
        ];

        // Right-side hints: only append if they fit in remaining space
        let current_w: usize = spans
            .iter()
            .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
            .sum();
        let hints = if area.width as usize >= 78 {
            "? help  / commands  Ctrl+N new  Ctrl+C quit "
        } else {
            "? help  Ctrl+C quit "
        };
        let hint_w = UnicodeWidthStr::width(hints);
        let avail = area.width as usize;
        if current_w + hint_w + 2 < avail {
            let pad = avail.saturating_sub(current_w + hint_w);
            spans.push(Span::raw(" ".repeat(pad)));
            spans.push(Span::styled(hints, dim_style));
        }

        Paragraph::new(Line::from(spans))
            .style(self.theme.statusline_style())
            .render(area, buf);
    }
}

fn ctx_bar(tokens: u32, limit: u32) -> String {
    let pct = if limit > 0 {
        (tokens as f64 / limit as f64 * 100.0).min(100.0)
    } else {
        0.0
    };
    let bar_w = 8usize;
    let filled = ((pct / 100.0) * bar_w as f64) as usize;
    let bar: String = (0..bar_w)
        .map(|i| if i < filled { '█' } else { '░' })
        .collect();
    format!("ctx {} {:.0}%", bar, pct)
}

fn format_duration(secs: u64) -> String {
    if secs < 60 {
        format!("{}s", secs)
    } else {
        format!("{}m {}s", secs / 60, secs % 60)
    }
}

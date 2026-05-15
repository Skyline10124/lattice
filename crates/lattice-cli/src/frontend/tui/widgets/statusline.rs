use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};
use unicode_width::UnicodeWidthStr;

use super::super::state::AppStatus;
use super::super::theme::Theme;
use super::super::view_model::{StatuslineView, TuiViewModel};

pub(in crate::frontend::tui) struct Statusline {
    theme: Theme,
}

impl Statusline {
    pub(in crate::frontend::tui) fn new(theme: Theme) -> Self {
        Self { theme }
    }

    pub(in crate::frontend::tui) fn render(
        &self,
        area: Rect,
        buf: &mut ratatui::buffer::Buffer,
        vm: &TuiViewModel<'_>,
    ) {
        if area.height == 0 {
            return;
        }

        let spans = self.spans(vm, area.width);

        Paragraph::new(Line::from(spans))
            .style(self.theme.statusline_style())
            .render(area, buf);
    }

    fn spans(&self, vm: &TuiViewModel<'_>, width: u16) -> Vec<Span<'static>> {
        let status = &vm.statusline;
        let styles = self.styles();
        let effort = status.thinking_effort.unwrap_or("auto");
        let mut spans = vec![
            Span::styled(format!(" {} ", status.mode_label), styles.pill),
            Span::raw(" "),
            Span::styled(status.current_model.to_string(), styles.body),
            Span::styled("@", styles.dim),
            Span::styled(provider_label(status), styles.body),
            Span::raw("  "),
            Span::styled(
                status_text(status),
                Style::default().fg(status_color(status, &self.theme)),
            ),
            Span::raw("  "),
            Span::styled(format!("{} tok", status.token_count), styles.dim),
            Span::raw("  "),
            Span::styled(
                ctx_bar(status.token_count as u32, status.context_limit),
                styles.dim,
            ),
            Span::raw("  "),
            Span::styled(trace_label(status), styles.dim),
            Span::raw("  "),
            Span::styled(format!("effort:{}", effort), styles.dim),
            Span::raw("  "),
            Span::styled(format!("sandbox:{}", status.sandbox), styles.dim),
            Span::raw("  "),
            Span::styled(status.agent_label.to_string(), styles.dim),
        ];
        if let Some(snapshot) = status.snapshot_label {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(snapshot.to_string(), styles.dim));
        }

        append_optional_segments(&mut spans, vm, styles.dim);
        append_fitting_hints(&mut spans, width, styles.dim);
        spans
    }

    fn styles(&self) -> StatuslineStyles {
        StatuslineStyles {
            pill: Style::default()
                .fg(self.theme.bg)
                .bg(self.theme.highlight)
                .add_modifier(Modifier::BOLD),
            dim: Style::default().fg(self.theme.subtext),
            body: Style::default().fg(self.theme.text),
        }
    }
}

#[derive(Clone, Copy)]
struct StatuslineStyles {
    pill: Style,
    dim: Style,
    body: Style,
}

fn provider_label(status: &StatuslineView<'_>) -> String {
    if status.current_provider.is_empty() {
        "unresolved".to_string()
    } else {
        status.current_provider.to_string()
    }
}

fn status_text(status: &StatuslineView<'_>) -> String {
    match status.status {
        AppStatus::Ready => "ready".to_string(),
        AppStatus::Streaming => streaming_status_text(status),
        AppStatus::Error(err) => format!("error: {err}"),
    }
}

fn streaming_status_text(status: &StatuslineView<'_>) -> String {
    let elapsed = status.stream_started.map_or(0, |s| s.elapsed().as_secs());
    let elapsed_str = format_duration(elapsed);
    if let (Some(rs), None) = (status.reasoning_started, status.reasoning_duration) {
        let thinking_elapsed = rs.elapsed().as_secs();
        format!(
            "Thinking… {} · thinking {}",
            elapsed_str,
            format_duration(thinking_elapsed)
        )
    } else if let Some(dur) = status.reasoning_duration {
        format!(
            "Streaming {} · thought for {}",
            elapsed_str,
            format_duration(dur.as_secs())
        )
    } else {
        format!("{} streaming {}", status.spinner, elapsed_str)
    }
}

fn status_color(status: &StatuslineView<'_>, theme: &Theme) -> Color {
    match status.status {
        AppStatus::Ready => theme.success,
        AppStatus::Streaming => theme.assistant_accent,
        AppStatus::Error(_) => theme.error,
    }
}

fn trace_label(status: &StatuslineView<'_>) -> &'static str {
    if status.transcript_mode {
        "trace on"
    } else {
        "trace off"
    }
}

fn append_optional_segments(
    spans: &mut Vec<Span<'static>>,
    vm: &TuiViewModel<'_>,
    dim_style: Style,
) {
    let queued = vm.statusline.queued_count;
    if queued > 0 {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(format!("queued:{}", queued), dim_style));
    }

    if let Some(search) = search_segment(vm) {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(search, dim_style));
    }
}

fn search_segment(vm: &TuiViewModel<'_>) -> Option<String> {
    if !vm.search.is_active() {
        return None;
    }
    Some(match vm.search.position() {
        Some((current, total)) => format!("find:{current}/{total} '{}'", vm.search.query),
        None => format!("find:0 '{}'", vm.search.query),
    })
}

fn append_fitting_hints(spans: &mut Vec<Span<'static>>, width: u16, dim_style: Style) {
    let hints = if width as usize >= 78 {
        "? help  / commands  Ctrl+N new  Ctrl+C quit "
    } else {
        "? help  Ctrl+C quit "
    };
    let current_w = spans_width(spans);
    let hint_w = UnicodeWidthStr::width(hints);
    let avail = width as usize;
    if current_w + hint_w + 2 < avail {
        let pad = avail.saturating_sub(current_w + hint_w);
        spans.push(Span::raw(" ".repeat(pad)));
        spans.push(Span::styled(hints, dim_style));
    }
}

fn spans_width(spans: &[Span<'_>]) -> usize {
    spans
        .iter()
        .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
        .sum()
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

#[cfg(test)]
mod tests {
    use lattice_core::types::Role;
    use ratatui::{buffer::Buffer, layout::Rect};

    use super::*;
    use crate::frontend::tui::app::App;
    use crate::frontend::tui::render_test::buffer_to_trimmed_string;
    use crate::frontend::tui::state::ChatMessage;

    fn render_statusline(app: &App, width: u16) -> String {
        let theme = Theme::catppuccin_mocha();
        let vm = TuiViewModel::from_app(app);
        let mut buffer = Buffer::empty(Rect::new(0, 0, width, 1));
        Statusline::new(theme).render(buffer.area, &mut buffer, &vm);
        buffer_to_trimmed_string(&buffer)
    }

    #[test]
    fn provider_falls_back_to_unresolved() {
        let app = App::new();

        let line = render_statusline(&app, 120);

        assert!(line.contains("deepseek-v4-flash@unresolved"));
        assert!(line.contains("ready"));
    }

    #[test]
    fn optional_queue_and_search_segments_are_state_driven() {
        let mut app = App::new();

        let empty_line = render_statusline(&app, 160);
        assert!(!empty_line.contains("queued:"));
        assert!(!empty_line.contains("find:"));

        app.turn.queued_inputs.push_back("next prompt".into());
        app.messages
            .push(ChatMessage::new(Role::Assistant, "alpha beta".into()));
        app.start_search("alpha".into());

        let active_line = render_statusline(&app, 160);
        assert!(active_line.contains("queued:1"));
        assert!(active_line.contains("find:1/1 'alpha'"));
    }

    #[test]
    fn hints_adapt_to_available_width() {
        let app = App::new();

        let wide = render_statusline(&app, 220);
        assert!(wide.contains("? help  / commands  Ctrl+N new  Ctrl+C quit"));

        let narrow = render_statusline(&app, 70);
        assert!(!narrow.contains("/ commands"));
    }
}

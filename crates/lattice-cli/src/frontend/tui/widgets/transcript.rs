use lattice_core::types::Role;
use ratatui::{
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::Paragraph,
    Frame,
};

use crate::frontend::tui::app::App;
use crate::frontend::tui::state::{ChatMessage, ClickAction, ClickZone};
use crate::frontend::tui::theme::Theme;
use crate::frontend::tui::widgets::message::message_lines;
use crate::frontend::tui::widgets::welcome::render_welcome;

pub(in crate::frontend::tui) fn render_transcript(
    f: &mut Frame,
    area: Rect,
    app: &App,
    theme: &Theme,
) {
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

    let last_user_msg = if effective_scroll > 0 {
        app.messages
            .iter()
            .rev()
            .find(|m| m.role == lattice_core::types::Role::User)
    } else {
        None
    };

    let pin_height: u16 = if last_user_msg.is_some() { 1 } else { 0 };

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
            ClickAction::JumpToBottom,
        );
    }

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

    app.activity.visible_rows.borrow_mut().clear();
    app.activity.visible_rows_origin.set(viewport.y);
    for line in &rows[start..end] {
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        app.activity.visible_rows.borrow_mut().push(text);
    }

    let sel_range: Option<(u16, u16)> = app
        .activity
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
        let marker = if app.activity.search.is_active() {
            match app.activity.search.position() {
                Some((current, total)) => format!(
                    "find {current}/{total} · ↑ {} lines above latest  End/click=jump",
                    effective_scroll
                ),
                None => format!(
                    "find 0 · ↑ {} lines above latest  End/click=jump",
                    effective_scroll
                ),
            }
        } else {
            format!("↑ {} lines above latest  End/click=jump", effective_scroll)
        };
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
            ClickAction::JumpToBottom,
        );
    }
}

fn register_zone(app: &App, rect: (u16, u16, u16, u16), action: ClickAction) {
    app.activity
        .click_zones
        .borrow_mut()
        .push(ClickZone { rect, action });
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

pub(in crate::frontend::tui) fn transcript_lines(
    app: &App,
    theme: &Theme,
    width: u16,
) -> Vec<Line<'static>> {
    let mut rendered = app
        .messages
        .iter()
        .enumerate()
        .filter_map(|(idx, msg)| {
            let mut lines = message_lines(msg, theme, app.transcript_mode, width);
            if lines.is_empty() {
                return None;
            }
            if app.activity.search.is_active() && app.activity.search.matches.contains(&idx) {
                let is_target = app.activity.search.target_message == Some(idx);
                lines = highlight_search_message(lines, theme, is_target);
            }
            Some((msg, lines))
        })
        .collect::<Vec<_>>();

    for idx in 0..rendered.len().saturating_sub(1) {
        let (left, right) = rendered.split_at_mut(idx + 1);
        let (msg, lines) = &mut left[idx];
        let (next_msg, _) = &right[0];
        if connects_with_next_in_turn(msg, next_msg) {
            replace_trailing_separator_with_connector(lines);
        }
    }

    rendered.into_iter().flat_map(|(_, lines)| lines).collect()
}

fn connects_with_next_in_turn(current: &ChatMessage, next: &ChatMessage) -> bool {
    current.role != Role::System && next.role != Role::System && next.role != Role::User
}

fn replace_trailing_separator_with_connector(lines: &mut [Line<'static>]) {
    let stripe_style = previous_stripe_style(lines).unwrap_or_default();
    let Some(last) = lines.last_mut() else {
        return;
    };
    if last
        .spans
        .iter()
        .any(|span| !span.content.trim().is_empty())
    {
        return;
    }
    *last = Line::from(vec![Span::styled("▌ ", stripe_style)]);
}

fn previous_stripe_style(lines: &[Line<'_>]) -> Option<Style> {
    lines.iter().find_map(|line| {
        line.spans
            .first()
            .filter(|span| span.content.as_ref().starts_with('▌'))
            .map(|span| span.style)
    })
}

fn highlight_search_message(
    lines: Vec<Line<'static>>,
    theme: &Theme,
    is_target: bool,
) -> Vec<Line<'static>> {
    let style = if is_target {
        Style::default().fg(theme.bg).bg(theme.highlight)
    } else {
        Style::default().bg(theme.surface)
    };
    lines
        .into_iter()
        .map(|line| {
            if line.spans.is_empty() {
                line
            } else {
                let spans = line
                    .spans
                    .into_iter()
                    .map(|span| Span::styled(span.content.to_string(), span.style.patch(style)))
                    .collect::<Vec<_>>();
                Line::from(spans)
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn transcript_connects_stripes_inside_one_turn() {
        let mut app = App::new();
        app.messages.push(ChatMessage::new(Role::User, "hi".into()));
        app.messages
            .push(ChatMessage::new(Role::Assistant, "answer".into()));

        let lines = transcript_lines(&app, &Theme::default(), 40);
        let text = lines.iter().map(line_text).collect::<Vec<_>>();

        assert!(text[0].starts_with("▌ hi"));
        assert_eq!(text[1].trim_end(), "▌");
        assert!(text[2].starts_with("▌ answer"));
        assert_eq!(lines[1].spans[0].style, lines[0].spans[0].style);
    }

    #[test]
    fn transcript_breaks_stripe_before_next_user_turn() {
        let mut app = App::new();
        app.messages
            .push(ChatMessage::new(Role::Assistant, "answer".into()));
        app.messages
            .push(ChatMessage::new(Role::User, "next".into()));

        let lines = transcript_lines(&app, &Theme::default(), 40);
        let text = lines.iter().map(line_text).collect::<Vec<_>>();

        assert!(text[0].starts_with("▌ answer"));
        assert_eq!(text[1], "");
        assert!(text[2].starts_with("▌ next"));
    }
}

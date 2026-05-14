use crossterm::event::KeyEvent;
use ratatui::{
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};
use unicode_width::UnicodeWidthStr;

use super::keymap::{KeyBindingsMatch, PagerKeymap};
use super::theme::Theme;

#[derive(Debug, Clone)]
pub(super) struct PagerOverlay {
    title: String,
    lines: Vec<Line<'static>>,
    scroll_offset: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PagerOutcome {
    Open,
    Closed,
}

impl PagerOverlay {
    pub(super) fn new(title: impl Into<String>, lines: Vec<Line<'static>>) -> Self {
        Self {
            title: title.into(),
            lines,
            scroll_offset: 0,
        }
    }

    pub(super) fn handle_key(&mut self, key: KeyEvent, keymap: &PagerKeymap) -> PagerOutcome {
        if keymap.close.matches_key(key) {
            return PagerOutcome::Closed;
        }
        if keymap.scroll_up.matches_key(key) {
            self.scroll_offset = self.scroll_offset.saturating_add(1);
        } else if keymap.scroll_down.matches_key(key) {
            self.scroll_offset = self.scroll_offset.saturating_sub(1);
        } else if keymap.page_up.matches_key(key) {
            self.scroll_offset = self.scroll_offset.saturating_add(12);
        } else if keymap.page_down.matches_key(key) {
            self.scroll_offset = self.scroll_offset.saturating_sub(12);
        } else if keymap.half_page_up.matches_key(key) {
            self.scroll_offset = self.scroll_offset.saturating_add(6);
        } else if keymap.half_page_down.matches_key(key) {
            self.scroll_offset = self.scroll_offset.saturating_sub(6);
        } else if keymap.jump_top.matches_key(key) {
            self.scroll_offset = usize::MAX;
        } else if keymap.jump_bottom.matches_key(key) {
            self.scroll_offset = 0;
        }
        PagerOutcome::Open
    }

    pub(super) fn render(&self, f: &mut Frame, area: Rect, theme: &Theme) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        f.render_widget(Clear, area);
        let block = Block::default()
            .title(format!(" {} ", self.title))
            .borders(Borders::ALL)
            .border_style(theme.border_style())
            .style(Style::default().bg(theme.bg));
        let inner = block.inner(area);
        f.render_widget(block, area);

        if inner.height == 0 {
            return;
        }

        let footer_height = 1;
        let content_height = inner.height.saturating_sub(footer_height);
        let content_area = Rect::new(inner.x, inner.y, inner.width, content_height);
        let footer_area = Rect::new(
            inner.x,
            inner.y.saturating_add(content_height),
            inner.width,
            footer_height,
        );

        let total_rows = self.lines.len();
        let view_h = content_area.height as usize;
        let max_scroll = total_rows.saturating_sub(view_h);
        let effective_scroll = self.scroll_offset.min(max_scroll);
        let end = total_rows.saturating_sub(effective_scroll);
        let start = end.saturating_sub(view_h);
        let visible = self.lines[start..end]
            .iter()
            .map(clone_line)
            .collect::<Vec<_>>();

        f.render_widget(
            Paragraph::new(Text::from(visible)).style(Style::default().fg(theme.text)),
            content_area,
        );
        self.render_footer(
            f,
            footer_area,
            theme,
            total_rows,
            max_scroll,
            effective_scroll,
        );
    }

    fn render_footer(
        &self,
        f: &mut Frame,
        area: Rect,
        theme: &Theme,
        total_rows: usize,
        max_scroll: usize,
        effective_scroll: usize,
    ) {
        if area.height == 0 {
            return;
        }
        let percent = if max_scroll == 0 {
            100
        } else {
            let from_top = max_scroll.saturating_sub(effective_scroll);
            ((from_top as f64 / max_scroll as f64) * 100.0).round() as usize
        };
        let left = format!("{total_rows} lines · {percent}%");
        let right = "↑↓ scroll  PgUp/PgDn page  g/G top/bottom  Esc/q close";
        let left_w = UnicodeWidthStr::width(left.as_str());
        let right_w = UnicodeWidthStr::width(right);
        let width = area.width as usize;
        let pad = width.saturating_sub(left_w + right_w).max(1);
        let line = if left_w + right_w < width {
            Line::from(vec![
                Span::styled(left, Style::default().fg(theme.subtext)),
                Span::raw(" ".repeat(pad)),
                Span::styled(right, Style::default().fg(theme.subtext)),
            ])
        } else {
            Line::from(vec![Span::styled(
                format!("{percent}% · Esc/q close"),
                Style::default()
                    .fg(theme.subtext)
                    .add_modifier(Modifier::ITALIC),
            )])
        };
        f.render_widget(Paragraph::new(line).alignment(Alignment::Left), area);
    }
}

fn clone_line(line: &Line<'static>) -> Line<'static> {
    Line::from(
        line.spans
            .iter()
            .map(|span| Span::styled(span.content.to_string(), span.style))
            .collect::<Vec<_>>(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};

    #[test]
    fn pager_closes_on_close_binding() {
        let keymap = super::super::keymap::RuntimeKeymap::new();
        let mut pager = PagerOverlay::new("Test", vec![]);

        let outcome = pager.handle_key(
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            &keymap.pager,
        );

        assert_eq!(outcome, PagerOutcome::Closed);
    }
}

use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};
use unicode_width::UnicodeWidthStr;

use crate::app::App;
use crate::theme::Theme;
use crate::widgets::{message::MessageWidget, statusline::Statusline};

pub fn draw(f: &mut Frame, app: &App) {
    let theme = Theme::catppuccin_mocha();
    let size = f.area();

    // Layout: chat area (top) + input (middle) + statusline (bottom)
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),    // chat messages
            Constraint::Length(3), // input box
            Constraint::Length(1), // statusline
        ])
        .split(size);

    let chat_area = chunks[0];
    let input_area = chunks[1];
    let status_area = chunks[2];

    // --- Chat messages ---
    let chat_block = Block::default()
        .title(" LATTICE ")
        .borders(Borders::ALL)
        .border_style(theme.border_style());
    let inner_chat = chat_block.inner(chat_area);
    f.render_widget(chat_block, chat_area);

    // Render messages (bottom-up, with scroll offset)
    let message_height = 2usize; // rough height per message for MVP
    let visible_count = inner_chat.height as usize / message_height;
    let start = app
        .messages
        .len()
        .saturating_sub(visible_count + app.scroll_offset);
    let end = app.messages.len().saturating_sub(app.scroll_offset);

    let message_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(vec![Constraint::Length(2); visible_count])
        .split(inner_chat);

    for (i, msg) in app.messages[start..end].iter().enumerate() {
        if let Some(area) = message_chunks.get(i) {
            MessageWidget::new(msg, &theme).render(*area, f.buffer_mut());
        }
    }

    // --- Input box ---
    let input_text = if app.input.is_empty() {
        Line::from(Span::styled(
            " Type a message... (Enter to send, Shift+Enter for newline, Ctrl+C to quit)",
            Style::default()
                .fg(theme.subtext)
                .add_modifier(Modifier::ITALIC),
        ))
    } else {
        Line::from(Span::styled(app.input.clone(), theme.input_style()))
    };

    let input_block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border_style());
    let input_para = Paragraph::new(input_text)
        .block(input_block)
        .wrap(Wrap { trim: true });
    f.render_widget(input_para, input_area);

    // Set cursor position (using visual width for CJK support)
    let text_before = &app.input[..app.input_cursor];
    let visual_x = UnicodeWidthStr::width(text_before) as u16;
    let cursor_x = input_area.x + 1 + visual_x.min(input_area.width.saturating_sub(2));
    let cursor_y = input_area.y + 1;
    f.set_cursor_position((cursor_x, cursor_y));

    // --- Statusline HUD ---
    Statusline::new(theme).render(status_area, f.buffer_mut(), app);
}

use ratatui::{backend::TestBackend, buffer::Buffer, Terminal};
use unicode_width::UnicodeWidthStr;

use super::app::App;
use super::ui;

pub(super) fn render_app(app: &App, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("test backend should initialize");
    terminal
        .draw(|frame| ui::draw(frame, app))
        .expect("test frame should render");
    buffer_to_trimmed_string(terminal.backend().buffer())
}

pub(super) fn buffer_to_trimmed_string(buffer: &Buffer) -> String {
    buffer
        .content
        .chunks(buffer.area.width as usize)
        .map(buffer_line)
        .collect::<Vec<_>>()
        .join("\n")
}

fn buffer_line(cells: &[ratatui::buffer::Cell]) -> String {
    let mut line = String::new();
    let mut skip = 0usize;

    for cell in cells {
        if skip == 0 {
            line.push_str(cell.symbol());
        }
        skip = skip
            .max(UnicodeWidthStr::width(cell.symbol()))
            .saturating_sub(1);
    }

    line.trim_end().to_string()
}

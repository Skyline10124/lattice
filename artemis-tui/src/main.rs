use anyhow::Result;
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io;

mod app;
mod event;
mod theme;
mod ui;
mod widgets;

use app::App;
use event::EventHandler;

#[tokio::main]
async fn main() -> Result<()> {
    // Setup terminal
    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create app and event handler
    let mut app = App::new();
    let mut events = EventHandler::new(250);
    app.event_tx = Some(events.sender());

    let res = run_app(&mut terminal, &mut app, &mut events).await;

    // Restore terminal
    terminal::disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    if let Err(err) = res {
        eprintln!("Error: {:?}", err);
    }

    Ok(())
}

async fn run_app<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    events: &mut EventHandler,
) -> Result<()> {
    while !app.should_quit {
        terminal.draw(|f| ui::draw(f, app))?;

        if let Some(event) = events.next().await {
            match event {
                event::Event::Tick => app.tick(),
                event::Event::Key(key) => app.handle_key(key).await?,
                event::Event::Mouse(mouse) => app.handle_mouse(mouse).await?,
                event::Event::Resize(_, _) => {}
                event::Event::StreamToken {
                    content,
                    reasoning,
                    done,
                    error,
                } => {
                    app.apply_stream_token(content, reasoning, done, error);
                }
            }
        }
    }
    Ok(())
}

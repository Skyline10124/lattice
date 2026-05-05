use anyhow::Result;
use crossterm::{
    event::{DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture},
    execute,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io::{self, Write};

use crate::config::Config;
use crate::credentials::CredentialStore;

mod app;
mod event;
mod markdown;
mod theme;
mod ui;
mod widgets;

use app::App;
use event::EventHandler;

struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = std::io::stdout().flush();
        let _ = terminal::disable_raw_mode();
        let _ = execute!(
            std::io::stdout(),
            LeaveAlternateScreen,
            crossterm::event::DisableMouseCapture,
            crossterm::event::DisableBracketedPaste
        );
    }
}

pub struct TuiOptions {
    pub prompt: Option<String>,
    pub model: String,
    pub provider_override: Option<String>,
    pub credentials: std::collections::HashMap<String, String>,
    pub save_sessions: bool,
    pub previous_session: Option<crate::session::Session>,
}

pub async fn run(options: TuiOptions) -> Result<()> {
    terminal::enable_raw_mode()?;
    let _guard = TerminalGuard;
    let mut stdout = io::stdout();
    crossterm::execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new();
    app.current_model = options.model;
    app.provider_override = options.provider_override;
    app.credentials = options.credentials;
    app.save_sessions = options.save_sessions;
    if let Some(session) = options.previous_session {
        app.load_session(session);
    }
    if let Some(prompt) = options.prompt {
        app.input = prompt;
        app.input_cursor = app.input.len();
    }
    let mut events = EventHandler::new(250);
    app.event_tx = Some(events.sender());

    let res = run_app(&mut terminal, &mut app, &mut events).await;

    terminal::disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        crossterm::event::DisableBracketedPaste
    )?;
    terminal.show_cursor()?;

    if let Err(err) = res {
        eprintln!("Error: {:?}", err);
    }

    Ok(())
}

pub fn options_from_config(
    prompt: Option<String>,
    model: Option<String>,
    provider_override: Option<String>,
    config: &Config,
    creds: &CredentialStore,
    save_sessions: bool,
    previous_session: Option<crate::session::Session>,
) -> TuiOptions {
    let model = model
        .or_else(|| {
            previous_session
                .as_ref()
                .map(|session| session.model.clone())
        })
        .unwrap_or_else(|| config.default_model());

    TuiOptions {
        prompt,
        model,
        provider_override,
        credentials: creds.to_hashmap(),
        save_sessions,
        previous_session,
    }
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
                event::Event::StreamToken {
                    turn_id,
                    content,
                    reasoning,
                    done,
                    error,
                } => {
                    app.apply_stream_token(turn_id, content, reasoning, done, error);
                }
                event::Event::ToolOutput {
                    turn_id,
                    name,
                    arguments,
                    result,
                } => {
                    app.apply_tool_output(turn_id, name, arguments, result);
                }
                event::Event::ModelInfo {
                    turn_id,
                    model,
                    provider,
                } => {
                    if app.accepts_turn(turn_id) {
                        app.current_model = model;
                        app.current_provider = provider;
                    }
                }
                event::Event::Paste(text) => {
                    app.insert_text(&text);
                }
            }
        }
    }
    Ok(())
}

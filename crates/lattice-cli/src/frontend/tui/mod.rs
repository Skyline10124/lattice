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

mod action;
mod activity;
mod agent_runner;
mod app;
mod bash_command;
mod bottom;
mod commands;
mod composer;
mod effects;
mod event;
mod interaction;
mod keymap;
mod markdown;
mod pager;
#[cfg(test)]
mod render_test;
mod search;
mod session_state;
mod state;
mod theme;
mod turn;
mod ui;
mod update;
mod view_model;
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

pub(crate) struct TuiOptions {
    pub(crate) prompt: Option<String>,
    pub(crate) agent: Option<String>,
    pub(crate) agent_label: String,
    pub(crate) model: String,
    pub(crate) provider_override: Option<String>,
    pub(crate) workdir: std::path::PathBuf,
    pub(crate) credentials: std::collections::HashMap<String, String>,
    pub(crate) save_sessions: bool,
    pub(crate) previous_session: Option<crate::session::Session>,
    pub(crate) security_config: crate::config::SecurityConfig,
}

pub(crate) async fn run(options: TuiOptions) -> Result<()> {
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
    app.agent_source =
        crate::profile_runtime::profile_source_from_agent_arg(options.agent.as_deref());
    app.agent_label = options.agent_label;
    app.provider_override = options.provider_override;
    app.workdir = options.workdir;
    app.credentials = options.credentials;
    app.save_sessions = options.save_sessions;
    app.runtime = crate::runtime::model_runtime(app.credentials.clone());
    let security = crate::security::build_runtime_security(&options.security_config, &app.workdir)?;
    app.security = security;
    app.security_config = options.security_config;
    if let Some(session) = options.previous_session {
        app.load_session(session);
    }
    if let Some(prompt) = options.prompt {
        app.composer.set_input(prompt);
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

#[allow(clippy::too_many_arguments)]
pub(crate) fn options_from_config(
    prompt: Option<String>,
    agent: Option<String>,
    model: Option<String>,
    provider_override: Option<String>,
    config: &Config,
    creds: &CredentialStore,
    save_sessions: bool,
    previous_session: Option<crate::session::Session>,
    security_config: crate::config::SecurityConfig,
) -> TuiOptions {
    let model = model
        .or_else(|| {
            previous_session
                .as_ref()
                .map(|session| session.model.clone())
        })
        .unwrap_or_else(|| config.default_model());

    let agent_label = agent_label(agent.as_deref());
    TuiOptions {
        prompt,
        agent,
        agent_label,
        model,
        provider_override,
        workdir: std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
        credentials: creds.to_hashmap(),
        save_sessions,
        previous_session,
        security_config,
    }
}

pub(crate) fn agent_label(agent: Option<&str>) -> String {
    match crate::profile_runtime::profile_source_from_agent_arg(agent) {
        lattice_profile::ProfileSource::Default => "agent:default".to_string(),
        lattice_profile::ProfileSource::Name(name) => format!("agent:{name}"),
        lattice_profile::ProfileSource::Path(path) => format!("agent:{}", path.display()),
    }
}

async fn run_app<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    events: &mut EventHandler,
) -> Result<()>
where
    B::Error: Send + Sync + std::error::Error + 'static,
{
    while !app.should_quit {
        terminal.draw(|f| ui::draw(f, app))?;

        if let Some(event) = events.next().await {
            update::apply_action(app, action::TuiAction::from(event)).await?;
        }
    }
    Ok(())
}

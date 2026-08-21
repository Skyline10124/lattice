// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, Event, EventStream, KeyCode, KeyEvent,
    KeyEventKind, KeyModifiers, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use futures::StreamExt;
use lattice_api::{RunTurnRequest, TurnEvent};

use crate::TuiOptions;
use crate::input::{CTRL_C_WINDOW, History, InputBuffer, KeyAction, resolve_key};
use crate::session::SessionContext;
use crate::view::{Viewport, block_lines};

const FRAME: Duration = Duration::from_millis(33);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Block {
    User(String),
    /// Accumulates `TokenDelta`. A new delta appends to the trailing
    /// `Assistant` block when one exists, otherwise pushes a new block.
    Assistant(String),
    Tool {
        id: String,
        name: String,
        state: ToolState,
    },
    Status(StatusKind),
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[rustfmt::skip]
pub(crate) enum ToolState {
    Running,
    Ok { output_len: usize },
    Failed { output_len: usize, error: Option<String> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[rustfmt::skip]
pub(crate) enum StatusKind {
    Committed { commit_id: String },
    CommitFailed { reason: String },
    TurnFailed { reason: String },
    Cancelled { reason: Option<String> },
    LlmRetry { attempt: u32, max_attempts: u32, delay_ms: u64, message: String },
    LlmFailed { attempt: u32, max_attempts: u32, message: String },
    /// `/help` output and startup banner.
    Notice(Vec<String>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TurnPhase {
    Idle,
    Streaming,
    Committing,
    /// Esc / Ctrl+C pressed; `cancel_flag` set, stream still draining.
    Cancelling,
}

pub(crate) struct App {
    pub(crate) ctx: SessionContext,
    pub(crate) blocks: Vec<Block>,
    pub(crate) input: InputBuffer,
    pub(crate) history: History,
    pub(crate) viewport: Viewport,
    pub(crate) phase: TurnPhase,
    pub(crate) cancel_flag: Option<Arc<AtomicBool>>,
    /// Set when `supports_keyboard_enhancement()` reported true at startup.
    pub(crate) kitty_keyboard: bool,
    /// Instant of the last Ctrl+C, for the double-press exit rule.
    pub(crate) last_ctrl_c: Option<Instant>,
    pub(crate) should_quit: bool,
    pub(crate) term_width: u16,
    pub(crate) term_height: u16,
}

impl App {
    pub(crate) fn new(ctx: SessionContext, kitty_keyboard: bool) -> Self {
        Self {
            ctx,
            blocks: Vec::new(),
            input: InputBuffer::new(),
            history: History::load(std::path::Path::new("")),
            viewport: Viewport::new(),
            phase: TurnPhase::Idle,
            cancel_flag: None,
            kitty_keyboard,
            last_ctrl_c: None,
            should_quit: false,
            term_width: 80,
            term_height: 24,
        }
    }

    /// Pure: fold one `TurnEvent` into `blocks` / `phase`. Exhaustive `match`
    /// with **no `_ =>` arm** (§9).
    #[rustfmt::skip]
    pub(crate) fn apply_turn_event(&mut self, event: TurnEvent) {
        match event {
            TurnEvent::TokenDelta { content } => match self.blocks.last_mut() {
                Some(Block::Assistant(buf)) => buf.push_str(&content),
                _ => self.blocks.push(Block::Assistant(content)),
            },
            TurnEvent::ToolCallStarted { id, name } => {
                self.blocks.push(Block::Tool {
                    id,
                    name,
                    state: ToolState::Running,
                });
            }
            TurnEvent::ToolCallFinished {
                id,
                name: _,
                ok,
                output_len,
                error_summary,
            } => {
                if let Some(Block::Tool { state, .. }) = self
                    .blocks
                    .iter_mut()
                    .rev()
                    .find(|b| matches!(b, Block::Tool { id: tid, .. } if *tid == id))
                {
                    *state = if ok {
                        ToolState::Ok { output_len }
                    } else {
                        ToolState::Failed {
                            output_len,
                            error: error_summary,
                        }
                    };
                }
            }
            TurnEvent::TurnCommitting => {
                if self.phase == TurnPhase::Streaming {
                    self.phase = TurnPhase::Committing;
                }
            }
            TurnEvent::TurnCommitted { commit_id, .. } => {
                self.push_status(StatusKind::Committed { commit_id });
            }
            TurnEvent::CommitFailed { reason } => {
                self.push_status(StatusKind::CommitFailed { reason });
            }
            TurnEvent::TurnFailed { reason } => {
                self.push_status(StatusKind::TurnFailed { reason });
            }
            TurnEvent::TurnCancelled { reason } => {
                self.push_status(StatusKind::Cancelled { reason });
            }
            TurnEvent::LlmRequestError {
                attempt,
                max_attempts,
                will_retry,
                delay_ms,
                message,
                ..
            } => {
                if will_retry {
                    self.push_status(StatusKind::LlmRetry {
                        attempt,
                        max_attempts,
                        delay_ms: delay_ms.unwrap_or(0),
                        message,
                    });
                } else {
                    self.push_status(StatusKind::LlmFailed {
                        attempt,
                        max_attempts,
                        message,
                    });
                }
            }
            TurnEvent::TurnStarted { .. }
            | TurnEvent::ModelStarted { .. }
            | TurnEvent::ModelFinished { .. }
            | TurnEvent::ReasoningDelta { .. }
            | TurnEvent::CepCallStarted { .. }
            | TurnEvent::CepCallFinished { .. } => {}
        }
    }

    /// Pure: fold one resolved key action. Returns `true` when a turn should be
    /// submitted with the returned prompt.
    #[rustfmt::skip]
    pub(crate) fn apply_action(&mut self, action: KeyAction) -> Option<String> {
        match action {
            KeyAction::Insert(c) => self.input.insert_char(c),
            KeyAction::Paste(text) => self.input.insert_paste(&text),
            KeyAction::Newline => self.input.newline(),
            KeyAction::Submit => return self.submit(),
            KeyAction::Backspace => self.input.backspace(),
            KeyAction::Delete => self.input.delete(),
            KeyAction::Left => self.input.move_left(),
            KeyAction::Right => self.input.move_right(),
            KeyAction::Home => self.input.home(),
            KeyAction::End => self.input.end(),
            KeyAction::Up => {
                if !self.input.move_up()
                    && let Some(entry) = self.history.prev()
                {
                    self.input.set_text(entry);
                }
            }
            KeyAction::Down => {
                if !self.input.move_down()
                    && let Some(entry) = self.history.next()
                {
                    self.input.set_text(entry);
                }
            }
            KeyAction::ScrollPageUp => self.viewport.page_up(self.transcript_height()),
            KeyAction::ScrollPageDown => {
                let rows = self.content_rows(self.term_width);
                self.viewport.page_down(rows, self.transcript_height());
            }
            KeyAction::ScrollToBottom => {
                let rows = self.content_rows(self.term_width);
                self.viewport.to_bottom(rows, self.transcript_height());
            }
            KeyAction::CancelOrClear => self.cancel_or_clear(),
            KeyAction::Quit => self.should_quit = true,
            KeyAction::Ignored => {}
        }
        None
    }

    /// Pure: `/help`, `/clear`, `/quit`, `/exit`, `/q`, `/?`. Returns
    /// `false` when the input is not a builtin and must be sent as a prompt.
    #[rustfmt::skip]
    pub(crate) fn apply_builtin(&mut self, input: &str) -> bool {
        match input {
            "/quit" | "/exit" | "/q" => { self.should_quit = true; true }
            "/help" | "/?" => { self.push_status(StatusKind::Notice(help_lines(self.kitty_keyboard))); true }
            "/clear" => { self.blocks.clear(); self.viewport = Viewport::new(); true }
            _ => false,
        }
    }

    /// Number of transcript rows at width `w`, used by `Viewport`.
    pub(crate) fn content_rows(&self, w: u16) -> usize {
        self.blocks.iter().map(|b| block_lines(b, w).len()).sum()
    }

    #[rustfmt::skip]
    pub(crate) fn apply_stream_idle_timeout(&mut self) {
        self.push_status(StatusKind::TurnFailed {
            reason: format!("stream timed out after {}s", lattice_api::STREAM_IDLE_TIMEOUT_SECS),
        });
        self.phase = TurnPhase::Idle;
        self.cancel_flag = None;
    }

    fn push_status(&mut self, kind: StatusKind) {
        self.blocks.push(Block::Status(kind));
    }

    fn submit(&mut self) -> Option<String> {
        let raw = self.input.text();
        if raw.trim().is_empty() {
            return None;
        }
        if self.apply_builtin(raw.trim()) {
            self.history.push(raw.trim());
            self.input.clear();
            return None;
        }
        if self.phase != TurnPhase::Idle {
            return None;
        }
        self.history.push(raw.trim());
        self.input.clear();
        self.blocks.push(Block::User(raw.clone()));
        self.phase = TurnPhase::Streaming;
        Some(raw)
    }

    #[rustfmt::skip]
    fn cancel_or_clear(&mut self) {
        match self.phase {
            TurnPhase::Idle => self.input.clear(),
            TurnPhase::Streaming | TurnPhase::Committing => {
                if let Some(flag) = &self.cancel_flag { flag.store(true, Ordering::Relaxed); }
                self.phase = TurnPhase::Cancelling;
            }
            TurnPhase::Cancelling => {}
        }
    }

    fn transcript_height(&self) -> usize {
        let input_h = self.input.rows().clamp(1, 6) as u16;
        self.term_height.saturating_sub(input_h + 1).max(1) as usize
    }
}

/// Terminal lifecycle + the three-arm loop. The only impure function here.
pub(crate) async fn run_app(options: TuiOptions, ctx: SessionContext) -> anyhow::Result<()> {
    let mut terminal = ratatui::try_init()?;
    let kitty = crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);
    crossterm::execute!(std::io::stdout(), EnableBracketedPaste)?;
    if kitty {
        crossterm::execute!(
            std::io::stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES),
        )?;
    }
    install_cleanup_panic_hook(kitty);
    let _cleanup = TerminalCleanup { kitty };

    let mut app = App::new(ctx, kitty);
    app.history = History::load(&options.session_dir);
    app.push_status(StatusKind::Notice(startup_notice(&app.ctx)));

    let mut events = EventStream::new();
    let mut turn_stream: Option<lattice_api::TurnEventStream> = None;
    let mut ticker = tokio::time::interval(FRAME);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut dirty = true;

    loop {
        if app.should_quit {
            break;
        }
        tokio::select! {
            event = events.next() => {
                if let Some(Ok(ev)) = event {
                    if let Some(prompt) = handle_input_event(&mut app, ev) {
                        begin_turn(&options, &mut app, &mut turn_stream, prompt).await;
                    }
                    dirty = true;
                }
            }
            wait = poll_turn(&mut turn_stream) => {
                match wait {
                    TurnWait::Event(ev) => app.apply_turn_event(ev),
                    TurnWait::Ended => {
                        turn_stream = None;
                        app.phase = TurnPhase::Idle;
                        app.cancel_flag = None;
                    }
                    TurnWait::TimedOut => {
                        turn_stream = None;
                        app.apply_stream_idle_timeout();
                    }
                }
                dirty = true;
            }
            _ = ticker.tick() => {
                if !dirty {
                    continue;
                }
                if let Ok(size) = terminal.size() {
                    app.term_width = size.width;
                    app.term_height = size.height;
                    let th = app.transcript_height();
                    app.viewport.on_content_changed(app.content_rows(size.width), th);
                }
                terminal.draw(|f| crate::view::draw(f, &app))?;
                dirty = false;
            }
        }
    }
    Ok(())
}

enum TurnWait {
    Event(TurnEvent),
    Ended,
    TimedOut,
}

async fn poll_turn(stream: &mut Option<lattice_api::TurnEventStream>) -> TurnWait {
    match stream.as_mut() {
        Some(s) => match tokio::time::timeout(
            Duration::from_secs(lattice_api::STREAM_IDLE_TIMEOUT_SECS),
            s.next(),
        )
        .await
        {
            Ok(Some(ev)) => TurnWait::Event(ev),
            Ok(None) => TurnWait::Ended,
            Err(_) => TurnWait::TimedOut,
        },
        None => std::future::pending().await,
    }
}

#[rustfmt::skip]
fn handle_input_event(app: &mut App, event: Event) -> Option<String> {
    match event {
        Event::Paste(text) => {
            app.apply_action(KeyAction::Paste(text));
            None
        }
        Event::Key(key) if key.kind == KeyEventKind::Press => {
            let action = if is_ctrl_c(&key) {
                ctrl_c_action(app)
            } else {
                resolve_key(key, app.kitty_keyboard, app.input.is_empty())
            };
            app.apply_action(action)
        }
        Event::Resize(w, h) => {
            app.term_width = w;
            app.term_height = h;
            None
        }
        _ => None,
    }
}

fn is_ctrl_c(key: &KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('c' | 'C' | '\u{3}'))
        && (key.modifiers.contains(KeyModifiers::CONTROL) || key.code == KeyCode::Char('\u{3}'))
}

#[rustfmt::skip]
fn ctrl_c_action(app: &mut App) -> KeyAction {
    let now = Instant::now();
    let double = app
        .last_ctrl_c
        .is_some_and(|t| now.duration_since(t) <= CTRL_C_WINDOW);
    app.last_ctrl_c = Some(now);
    if double {
        KeyAction::Quit
    } else {
        KeyAction::CancelOrClear
    }
}

#[rustfmt::skip]
async fn begin_turn(
    options: &TuiOptions,
    app: &mut App,
    turn_stream: &mut Option<lattice_api::TurnEventStream>,
    prompt: String,
) {
    let flag = Arc::new(AtomicBool::new(false));
    app.cancel_flag = Some(flag.clone());
    let loaded = match crate::session::load_profile(options, &app.ctx.active_agent) {
        Ok(p) => p,
        Err(e) => return fail_turn(app, format!("{e:#}")),
    };
    let session_api = match crate::session::open_session_api() {
        Ok(s) => s,
        Err(e) => return fail_turn(app, format!("{e:#}")),
    };
    let request = RunTurnRequest {
        input: prompt,
        session_id: app.ctx.session_id.clone(),
        loaded_profile: Some(loaded),
        cancel_flag: Some(flag),
        test_llm: None,
        test_project_root: None,
        test_persist_audit: None,
    };
    match lattice_api::stream_turn(session_api, request).await {
        Ok(s) => *turn_stream = Some(s),
        Err(e) => fail_turn(app, format!("{e:#}")),
    }
}

fn fail_turn(app: &mut App, reason: String) {
    app.apply_turn_event(TurnEvent::TurnFailed { reason });
    app.phase = TurnPhase::Idle;
    app.cancel_flag = None;
}

#[rustfmt::skip]
fn help_lines(kitty: bool) -> Vec<String> {
    let nl = if kitty { "Shift+Enter  newline" } else { "Alt+Enter  newline" };
    vec![
        "/help  this message".into(),
        "/clear  clear transcript".into(),
        "/quit  exit (also Ctrl+D on empty input)".into(),
        "Ctrl+C  cancel turn; twice within 2s to quit".into(),
        "Enter  submit".into(),
        nl.into(),
    ]
}

#[rustfmt::skip]
fn startup_notice(ctx: &SessionContext) -> Vec<String> {
    vec![
        "LATTICE".into(),
        format!("session {}", ctx.session_id),
        format!("agent {}", ctx.active_agent),
        format!("project {}", ctx.project_branch),
        "/help  show commands".into(),
        "/clear  clear transcript".into(),
        "/quit  exit".into(),
    ]
}

#[rustfmt::skip]
fn install_cleanup_panic_hook(kitty: bool) {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = crossterm::execute!(std::io::stdout(), DisableBracketedPaste);
        if kitty {
            let _ = crossterm::execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
        }
        prev(info);
    }));
}

struct TerminalCleanup {
    kitty: bool,
}

#[rustfmt::skip]
impl Drop for TerminalCleanup {
    fn drop(&mut self) {
        if self.kitty {
            let _ = crossterm::execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
        }
        let _ = crossterm::execute!(std::io::stdout(), DisableBracketedPaste);
        ratatui::restore();
    }
}

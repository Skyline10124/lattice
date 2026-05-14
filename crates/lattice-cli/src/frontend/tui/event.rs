use crossterm::event::{self, Event as CEvent, KeyEvent, MouseEvent};
use lattice_runtime::{AgentHandleId, Runtime, TurnId, TurnStatus};
use std::time::Duration;
use tokio::sync::mpsc;

use super::state::FileSnapshot;

pub(super) enum Event {
    Tick,
    Key(KeyEvent),
    Mouse(MouseEvent),
    /// Text pasted or committed via IME.
    Paste(String),
    /// LLM streaming token delivered from background task.
    StreamToken {
        turn_id: u64,
        content: String,
        reasoning: Option<String>,
        done: bool,
        error: Option<String>,
    },
    /// Tool execution progress and output delivered from the background task.
    ToolOutput {
        turn_id: u64,
        call_id: String,
        name: String,
        arguments: String,
        result: Option<String>,
        file_before: Option<FileSnapshot>,
    },
    /// Resolved model + provider info from the background task.
    ModelInfo {
        turn_id: u64,
        model: String,
        provider: String,
    },
    /// Runtime has associated the local widget turn with a runtime turn id.
    RuntimeTurnStarted {
        turn_id: u64,
        handle_id: AgentHandleId,
        runtime_turn_id: TurnId,
    },
    /// Runtime terminal lifecycle reached stream end; completion can be queried.
    RuntimeTurnFinished {
        turn_id: u64,
        runtime: Runtime,
        handle_id: AgentHandleId,
        runtime_turn_id: TurnId,
        status: TurnStatus,
        error: Option<String>,
    },
    /// Runtime rejected a submit because the handle is already active.
    RuntimeBusy {
        turn_id: u64,
        prompt_text: String,
    },
}

pub(super) struct EventHandler {
    rx: mpsc::UnboundedReceiver<Event>,
    _tx: mpsc::UnboundedSender<Event>,
}

impl EventHandler {
    pub(super) fn new(tick_rate: u64) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let tx_clone = tx.clone();
        let tick = Duration::from_millis(tick_rate);

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tick);
            loop {
                interval.tick().await;
                if tx_clone.send(Event::Tick).is_err() {
                    break;
                }
            }
        });

        let tx_clone = tx.clone();
        tokio::spawn(async move {
            loop {
                if let Ok(ready) = event::poll(Duration::from_millis(100)) {
                    if ready {
                        if let Ok(ev) = event::read() {
                            let mapped = match ev {
                                CEvent::Key(k) => Event::Key(k),
                                CEvent::Mouse(m) => Event::Mouse(m),
                                CEvent::Resize(_, _) => Event::Tick,
                                CEvent::Paste(text) => Event::Paste(text),
                                CEvent::FocusGained | CEvent::FocusLost => continue,
                            };
                            if tx_clone.send(mapped).is_err() {
                                break;
                            }
                        }
                    }
                }
            }
        });

        EventHandler { rx, _tx: tx }
    }

    pub(super) async fn next(&mut self) -> Option<Event> {
        self.rx.recv().await
    }

    pub(super) fn sender(&self) -> mpsc::UnboundedSender<Event> {
        self._tx.clone()
    }
}

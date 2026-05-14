use crossterm::event::{KeyEvent, MouseEvent};
use lattice_runtime::{AgentHandleId, Runtime, TurnId, TurnStatus};

use super::event::Event;
use super::state::FileSnapshot;

/// Normalized UI actions consumed by the TUI update layer.
pub(super) enum TuiAction {
    Tick,
    Key(KeyEvent),
    Mouse(MouseEvent),
    Paste(String),
    StreamToken {
        turn_id: u64,
        content: String,
        reasoning: Option<String>,
        done: bool,
        error: Option<String>,
    },
    ToolOutput {
        turn_id: u64,
        call_id: String,
        name: String,
        arguments: String,
        result: Option<String>,
        file_before: Option<FileSnapshot>,
    },
    ModelInfo {
        turn_id: u64,
        model: String,
        provider: String,
    },
    RuntimeTurnStarted {
        turn_id: u64,
        handle_id: AgentHandleId,
        runtime_turn_id: TurnId,
    },
    RuntimeTurnFinished {
        turn_id: u64,
        runtime: Runtime,
        handle_id: AgentHandleId,
        runtime_turn_id: TurnId,
        status: TurnStatus,
        error: Option<String>,
    },
    RuntimeBusy {
        turn_id: u64,
        prompt_text: String,
    },
}

impl From<Event> for TuiAction {
    fn from(event: Event) -> Self {
        match event {
            Event::Tick => Self::Tick,
            Event::Key(key) => Self::Key(key),
            Event::Mouse(mouse) => Self::Mouse(mouse),
            Event::Paste(text) => Self::Paste(text),
            Event::StreamToken {
                turn_id,
                content,
                reasoning,
                done,
                error,
            } => Self::StreamToken {
                turn_id,
                content,
                reasoning,
                done,
                error,
            },
            Event::ToolOutput {
                turn_id,
                call_id,
                name,
                arguments,
                result,
                file_before,
            } => Self::ToolOutput {
                turn_id,
                call_id,
                name,
                arguments,
                result,
                file_before,
            },
            Event::ModelInfo {
                turn_id,
                model,
                provider,
            } => Self::ModelInfo {
                turn_id,
                model,
                provider,
            },
            Event::RuntimeTurnStarted {
                turn_id,
                handle_id,
                runtime_turn_id,
            } => Self::RuntimeTurnStarted {
                turn_id,
                handle_id,
                runtime_turn_id,
            },
            Event::RuntimeTurnFinished {
                turn_id,
                runtime,
                handle_id,
                runtime_turn_id,
                status,
                error,
            } => Self::RuntimeTurnFinished {
                turn_id,
                runtime,
                handle_id,
                runtime_turn_id,
                status,
                error,
            },
            Event::RuntimeBusy {
                turn_id,
                prompt_text,
            } => Self::RuntimeBusy {
                turn_id,
                prompt_text,
            },
        }
    }
}

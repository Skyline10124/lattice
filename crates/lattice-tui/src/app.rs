// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

/// Session config carried through the REPL session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppConfig {
    pub session_id: String,
    pub active_agent: String,
}

/// Lightweight events for tracking turn state in the REPL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppEvent {
    Submitted(String),
    TokenDelta(String),
    ToolStarted { name: String },
    ToolFinished { name: String, ok: bool },
    Committed { commit_id: String },
    Failed { reason: String },
    Cancelled { reason: Option<String> },
    Finished,
    Status(String),
}

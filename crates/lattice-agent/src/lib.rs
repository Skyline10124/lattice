// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! AgentLoop-only agent crate.

pub mod agent_loop;

pub use agent_loop::{
    AgentLoop, AgentLoopConfig, AgentToolResultSummary, AgentTurnEvent, AgentTurnRequest,
    AgentTurnResult, AgentTurnStatus, CepErrorPayload, LlmClient, LlmRetryConfig, ToolCallExecutor,
    ToolExecutionError,
};

#[cfg(feature = "test-utils")]
pub mod testing;

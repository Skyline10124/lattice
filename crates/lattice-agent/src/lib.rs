//! AgentLoop-only agent crate.

pub mod agent_loop;

pub use agent_loop::{
    AgentLoop, AgentLoopConfig, AgentToolResultSummary, AgentTurnEvent, AgentTurnRequest,
    AgentTurnResult, AgentTurnStatus, CepErrorPayload, LlmClient, ToolCallExecutor,
    ToolExecutionError,
};

#[cfg(feature = "test-utils")]
pub mod testing;

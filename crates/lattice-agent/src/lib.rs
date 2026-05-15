pub mod audit;
pub mod behavior;
pub mod bus_event_collector;
mod chat;
mod config;
pub mod context_manager;
mod event;
pub mod final_report;
pub mod hook;
mod manifest;
pub mod memory;
pub mod message_source;
pub mod prompt;
mod run;
pub mod sandbox;
pub mod state;
mod tool_result;
mod turn_engine;

#[cfg(feature = "blob-store")]
pub mod blob;

#[cfg(feature = "blob-store")]
pub mod events_provider;

#[cfg(feature = "blob-store")]
pub use events_provider::EventsFactory;

use lattice_core::retry::RetryPolicy;
use lattice_core::types::ToolDefinition;
use lattice_hooks::PromptHookChain;

// Import tool execution types from lattice-tools for internal use (no longer re-exported).
use lattice_tools::ToolExecutor;

/// Max retries for mid-stream errors in Agent::run().
const MAX_STREAM_RETRIES: u32 = 2;

// Re-export shared types for convenience.
pub use bus_event_collector::ContextEvent;
pub use context_manager::{CompactReport, ContextManager, ContextStats};
pub use event::LoopEvent;
pub use final_report::{
    git_status_snapshot, FinalChangedFile, FinalReport, FinalVerification, GitStatusSnapshot,
};
pub use manifest::{
    FragmentRecord, HookDecisionRecord, PromptSnapshot, SessionContext, ToolDecisionRecord,
    TurnManifestObject,
};
pub use sandbox::SandboxConfig;
pub use state::{
    ApprovalProposalRecord, ChangedFileRecord, TaskState, TaskStateError, TodoItem, TodoStatus,
    ToolFailureRecord, VerificationRecord, VerificationStatus,
};

/// An async LLM agent with tool execution and conversation state.
///
/// # Runtime model
///
/// `Agent::run()` is `async` and **must** be called from within a Tokio runtime
/// (e.g. from `#[tokio::main]` or a `tokio::spawn`-ed task).  Internally it calls
/// [`lattice_core::chat()`] which uses `reqwest` — a Tokio-aware HTTP client.
///
/// For callers that are **not** inside a Tokio runtime (e.g. synchronous library
/// code or FFI), wrap the call at the outermost boundary:
///
/// ```ignore
/// let rt = tokio::runtime::Runtime::new()?;
/// let result = rt.block_on(agent.run("prompt", 10));
/// ```
///
/// Do **not** call `block_on` from within a Tokio context — this will panic
/// or deadlock. Foreign-language bindings should handle this via a shared
/// outer runtime bridge.
pub struct Agent {
    state: state::AgentState,
    tools: Vec<ToolDefinition>,
    retry: RetryPolicy,
    /// Runtime behavior mode: Strict (default) or Yolo with sandbox policy.
    behavior: behavior::BehaviorMode,
    /// Audit log for security-relevant events (best-effort, may be None).
    audit: Option<std::sync::Arc<crate::audit::AuditLog>>,
    memory: Option<std::sync::Arc<dyn memory::Memory>>,
    tool_executor: Option<Box<dyn ToolExecutor>>,
    registry: prompt::PromptRegistry,
    prompt_hooks: PromptHookChain,
    #[cfg(feature = "blob-store")]
    blob_store: Option<std::sync::Arc<crate::blob::BlobStore>>,
    bus_event_collector: crate::bus_event_collector::BusEventCollector,
    thinking_effort: Option<String>,
    session_context: Option<SessionContext>,
}

#[cfg(test)]
mod agent_tests;

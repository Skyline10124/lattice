//! LATTICE v1.1 LLM vocabulary and functional call facade.
//!
//! This crate owns the direct v1.1 LLM contract: message/tool types (`Message`, `Role`,
//! `ToolCall`, `ToolDefinition`), request/response types (`ChatRequest`, `ChatResponse`),
//! streaming events (`LlmEvent`), model routing primitives (`ModelTarget`, `ModelRouter`),
//! and functional `chat`/`stream` calls backed by a deterministic [`StubLlm`] in Phase 6.
//!
//! It owns no runtime, session, agent, profile, CLI, or TUI state.
//! It does not import `lattice-core`, `lattice-runtime`, `lattice-prompt`,
//! `lattice-sandbox`, or `lattice-policy`.

mod client;
mod endpoints;
mod logging;
mod providers;
mod router;
pub mod security;
pub mod sse;
mod stub;
mod types;

pub use logging::init_logging;
pub use router::{CredentialStatus, ModelRouter, ProviderKind, ResolvedModel};
pub use stub::{chat, stream, LlmError, LlmStream, StubLlm};
pub use types::{
    ChatRequest, ChatResponse, FunctionCall, LlmEvent, Message, ModelTarget, Role, TokenUsage,
    ToolCall, ToolDefinition,
};

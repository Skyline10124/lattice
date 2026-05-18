//! LATTICE LLM vocabulary and functional call facade.

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

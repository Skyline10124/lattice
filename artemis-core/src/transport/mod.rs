//! Provider transport layer — format conversion between internal types and API formats.
//!
//! Each transport handles normalization/denormalization for one API format
//! (e.g. OpenAI Chat Completions, Anthropic Messages, etc.).

pub mod chat_completions;
pub mod openai_compat;

pub use chat_completions::{ChatCompletionsTransport, Transport, TransportError};
pub use openai_compat::OpenAICompatTransport;

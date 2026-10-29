// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! LATTICE LLM vocabulary and functional call facade.

pub mod auth;
mod client;
mod tls;
pub use tls::load_tls_roots_der;
mod gateway;
pub use gateway::{GatewayTurn, RequestGateway, ResolvedRequest};
pub use protocol::Protocol;
mod logging;
mod models;
pub use models::{ModelCandidate, ModelCandidates, ModelPrice, ModelWarning, model_candidates};
pub(crate) mod metadata;
pub use metadata::{
    MODELS_URL, MetadataError, MetadataRefreshControl, fetch_snapshot,
    metadata_auto_refresh_enabled,
};
pub mod protocol;
pub(crate) mod provider;
pub use provider::{MAX_MODEL_ID_BYTES, ModelRef, ModelRefError};
mod reply;
mod reply_stream;
pub mod sse;
mod stub;
#[cfg(test)]
mod test_reply;
#[cfg(test)]
mod test_tls;
mod transport;
mod types;

pub use lattice_conversation::TokenUsage;
pub use logging::init_logging;
pub use reply::{
    AccumulatedReply, LiveDelta, ReplyOutcome, ReplyProtocolError, ReplyStop, StreamStop,
};
pub use reply_stream::{CONTROL_POLL_INTERVAL, StreamControl};
pub use stub::{LlmError, LlmStream, StubLlm};
pub use transport::{MAX_DIAGNOSTIC_CHARS, bound_diagnostic};
pub use types::{
    AttemptReport, ChatRequest, LlmEvent, ModelTarget, ProviderFailure, ToolDefinition,
};

#[cfg(feature = "test-utils")]
pub mod testing;

// Expose the real parser only for cross-crate streaming boundary tests.
#[cfg(feature = "test-utils")]
pub use protocol::chat_completions::OpenAiSseParser;

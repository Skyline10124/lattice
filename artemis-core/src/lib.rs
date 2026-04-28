pub mod catalog;
pub mod errors;
pub mod provider;
pub mod providers;
pub mod retry;
pub mod router;
pub mod streaming;
pub mod tokens;
pub mod transport;
pub mod types;

mod mock;

// Re-export key types for convenience
pub use catalog::ResolvedModel;
pub use errors::ArtemisError;
pub use streaming::StreamEvent;
pub use types::{FunctionCall, Message, Role, ToolCall, ToolDefinition};

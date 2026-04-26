use futures::Stream;

/// Token usage statistics for a request.
#[derive(Debug, Clone)]
pub struct TokenUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

/// Events emitted by a streaming chat response.
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// A chunk of generated text.
    Text(String),
    /// A tool call was requested.
    ToolCall {
        id: String,
        name: String,
        arguments: String,
    },
    /// The stream has finished.
    Done {
        finish_reason: String,
        usage: Option<TokenUsage>,
    },
    /// An error occurred during streaming.
    Error(String),
}

/// Boxed, sendable, unpinnable stream of [`StreamEvent`]s.
pub type EventStream = Box<dyn Stream<Item = StreamEvent> + Send + Unpin>;

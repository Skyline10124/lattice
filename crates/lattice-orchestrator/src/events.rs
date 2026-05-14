use serde::Serialize;
use tokio::sync::broadcast;

use lattice_core::handoff::HandoffTarget;

// ---------------------------------------------------------------------------
// PipelineEvent — real-time event stream for pipeline execution
// ---------------------------------------------------------------------------

/// Events emitted by the pipeline during execution.  Clients can subscribe
/// via the WebSocket endpoint to watch pipeline progress in real time.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum PipelineEvent {
    /// A new agent started executing.
    AgentStarted { agent: String, input_size: usize },

    /// The current agent completed and produced output.
    AgentCompleted {
        agent: String,
        output_preview: String, // first 500 chars
        next: Option<HandoffTarget>,
        duration_ms: u64,
    },

    /// A handoff rule matched, routing to the next agent.
    Handoff { from: String, to: HandoffTarget },

    /// A fork: multiple agents launched in parallel.
    Fork { from: String, branches: Vec<String> },

    /// The pipeline completed (all agents finished).
    PipelineCompleted {
        total_agents: usize,
        duration_ms: u64,
    },

    /// The pipeline encountered an error.
    PipelineError {
        agent: String,
        message: String,
        skippable: bool,
    },
}

/// A thread-safe event bus backed by a `tokio::sync::broadcast` channel.
///
/// Multiple receivers can subscribe independently — each gets a full feed
/// of events from the point of subscription onwards.
pub struct EventBus {
    tx: broadcast::Sender<PipelineEvent>,
}

impl EventBus {
    /// Create a new event bus with room for `capacity` events.
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self { tx }
    }

    /// Send an event to all subscribers.
    ///
    /// Returns the number of receivers that received the event.
    /// A zero count means no subscribers are listening, which is normal
    /// (e.g., before any WebSocket client connects).
    pub fn send(&self, event: PipelineEvent) -> usize {
        self.tx.send(event).unwrap_or(0)
    }

    /// Subscribe to the event stream.
    pub fn subscribe(&self) -> broadcast::Receiver<PipelineEvent> {
        self.tx.subscribe()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(256)
    }
}

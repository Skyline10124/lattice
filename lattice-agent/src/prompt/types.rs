use crate::memory::Memory;
use lattice_bus::BusEvent;
use lattice_core::types::{Message, Role};
use lattice_core::ResolvedModel;

#[derive(Debug, Clone)]
pub struct PromptSection {
    pub content: String,
    pub layer: Layer,
    pub priority: u8,
    pub tokens: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Layer {
    System = 0,
    Rules = 1,
    Tools = 2,
    Memory = 3,
    Events = 4,
    Input = 5,
}

impl Layer {
    pub fn all() -> &'static [Layer] {
        &[
            Layer::System,
            Layer::Rules,
            Layer::Tools,
            Layer::Memory,
            Layer::Events,
            Layer::Input,
        ]
    }
}

/// Token budget declaration for a context provider.
///
/// Controls how the compiler allocates the effective budget:
/// - `Fixed(n)`: Reserve exactly n tokens. Unused quota is NOT returned to the pool.
/// - `Ratio(f)`: Claim f proportion of the remaining budget after Fixed deductions.
///   Values outside [0.0, 1.0] are clamped; NaN is treated as 0.0.
/// - `Dynamic`: Consume whatever remains after Fixed and Ratio allocations.
///
/// Dynamic providers are served in priority order; if earlier providers
/// consume all remaining budget, later ones get zero allocation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TokenBudget {
    Fixed(u32),
    Ratio(f64),
    Dynamic,
}

/// Context passed to each provider during produce().
/// Borrows agent state — constructed fresh per compile() call.
pub struct AssemblyContext<'a> {
    pub request_id: &'a str,
    pub memory: Option<&'a dyn Memory>,
    pub model: &'a ResolvedModel,
    #[cfg(feature = "blob-store")]
    pub blob_store: Option<&'a crate::blob::BlobStore>,
    pub bus_events: &'a [BusEvent],
}

/// Final compiled prompt ready to send to LLM.
///
/// System-layer content becomes a `Role::System` message (no markers — the role
/// itself conveys system authority). All other layers become a single `Role::User`
/// message with `=== Layer ===` markers for structural clarity.
#[derive(Debug, Clone)]
pub struct RenderedPrompt {
    /// Compiled messages with proper role assignments.
    pub messages: Vec<Message>,
    /// Sections that survived budget trimming, in render order.
    pub sections: Vec<PromptSection>,
    pub total_tokens: u32,
}

impl RenderedPrompt {
    /// Convenience: return the first User message content, or empty string.
    pub fn user_content(&self) -> &str {
        self.messages
            .iter()
            .find(|m| m.role == Role::User)
            .map(|m| m.content.as_str())
            .unwrap_or("")
    }
}

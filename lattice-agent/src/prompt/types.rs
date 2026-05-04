use crate::memory::Memory;
use lattice_core::ResolvedModel;

#[derive(Debug, Clone)]
pub struct PromptSection {
    pub content: String,
    pub layer: Layer,
    pub priority: u8,
    pub tokens: u32,
    pub budget: TokenBudget,
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
    /// All layer variants in ordinal order.
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

#[derive(Debug, Clone)]
pub struct AgentMetadata {
    pub name: String,
    pub enabled_plugins: Vec<String>,
    pub handoff_targets: Vec<String>,
}

/// Context passed to each provider during produce().
/// Borrows agent state — constructed fresh per compile() call.
pub struct AssemblyContext<'a> {
    pub request_id: &'a str,
    pub memory: Option<&'a dyn Memory>,
    pub metadata: Option<&'a AgentMetadata>,
    pub model: &'a ResolvedModel,
}

/// Final compiled prompt ready to send to LLM.
#[derive(Debug, Clone)]
pub struct RenderedPrompt {
    pub text: String,
    /// Sections that survived budget trimming, in render order.
    pub sections: Vec<PromptSection>,
    pub total_tokens: u32,
}

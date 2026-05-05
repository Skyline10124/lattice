use crate::prompt::types::{AssemblyContext, Layer, PromptSection, TokenBudget};
use async_trait::async_trait;

/// A source of prompt content.
///
/// Each provider is responsible for:
/// - Declaring its layer, priority, and budget (registration-time, immutable)
/// - Producing content with an accurate token estimate
/// - Returning None if it has nothing to contribute this round
///
/// Engine layer does NOT call this concurrently in alpha — providers
/// should not depend on concurrent execution.
#[async_trait]
pub trait ContextProvider: Send + Sync {
    /// Which layer this provider contributes to.
    fn layer(&self) -> Layer;

    /// Within-layer sort priority (0 = highest, 255 = lowest, default 5).
    fn priority(&self) -> u8 {
        5
    }

    /// Token budget declaration for this provider.
    ///
    /// Controls how the compiler allocates effective budget:
    /// - `Fixed(n)`: Reserve exactly n tokens. Unused quota is NOT returned.
    /// - `Ratio(f)`: Claim f proportion of remaining budget after Fixed deductions.
    /// - `Dynamic`: Consume whatever remains after Fixed and Ratio allocations.
    fn budget(&self) -> TokenBudget {
        TokenBudget::Dynamic
    }

    /// Produce a prompt section, or None if nothing to contribute.
    async fn produce(&self, ctx: &AssemblyContext<'_>) -> Option<PromptSection>;
}

pub trait SystemPromptProvider: Send + Sync {
    fn build_delta(&self) -> Option<crate::prompt::system_stack::SystemPromptDelta>;
}

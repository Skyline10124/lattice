use std::any::Any;

use crate::spec::PromptProviderSpec;
use crate::types::{AssemblyContext, Layer, PromptContribution, PromptSection, TokenBudget};
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
    /// Allow a runtime adapter to attach optional host-specific handles.
    ///
    /// Prompt assembly stays host-agnostic: providers that understand a key can
    /// downcast the value, while generic providers simply ignore configuration.
    fn configure(&mut self, _key: &'static str, _value: &(dyn Any + Send + Sync)) {}

    /// Apply configuration from a PromptProviderSpec.
    /// Called by ProviderFactory after creating the provider.
    /// Default: no-op. Providers override to apply spec.layer, spec.priority, spec.budget, spec.limit.
    fn configure_from_spec(&mut self, _spec: &PromptProviderSpec) {}

    /// Provider name for provenance tracking in manifests.
    fn name(&self) -> &str {
        "provider"
    }

    /// Provider name used in ContributionProvenance.
    /// Defaults to `name()`. Override when the spec supplies a custom id
    /// (e.g. ToolSchemaProvider uses spec.id as provenance_id).
    fn provenance_provider(&self) -> &str {
        self.name()
    }

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

    /// Produce typed prompt contributions.
    ///
    /// Returns a Vec to support providers that emit multiple contributions.
    /// The default adapter wraps `produce()` in a single-element Vec.
    /// Providers returning multiple contributions override this.
    async fn contribute(&self, ctx: &AssemblyContext<'_>) -> Option<Vec<PromptContribution>> {
        self.produce(ctx).await.map(|section| {
            vec![PromptContribution::from_section(
                section,
                self.budget(),
                self.provenance_provider(),
            )]
        })
    }
}

pub trait SystemPromptProvider: Send + Sync {
    fn build_delta(&self) -> Option<crate::system_stack::SystemPromptDelta>;
}

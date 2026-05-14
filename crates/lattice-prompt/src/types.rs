use lattice_core::types::{Message, Role};
use lattice_core::ResolvedModel;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PromptCompileError {
    #[error("prompt sections and budgets length mismatch: {sections} sections, {budgets} budgets")]
    LengthMismatch { sections: usize, budgets: usize },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptSection {
    pub content: String,
    pub layer: Layer,
    pub priority: u8,
    pub tokens: u32,
}

/// Channel where a contribution should be rendered for model input.
///
/// The current compiler still maps `Layer::System` to a system message and all
/// other layers to a marked user-context message. This channel field is the
/// forward-compatible contract for provider adapters and declarative prompt
/// specs to state the intended routing before model-specific renderers exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptChannel {
    System,
    Developer,
    UserContext,
    ToolSchema,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictPolicy {
    Append,
    Replace,
    Deny,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContributionProvenance {
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_ref: Option<String>,
}

/// Typed prompt contribution.
///
/// `PromptSection` remains the compatibility input for existing providers.
/// New prompt declarations and hooks should prefer this richer shape because it
/// carries routing, cache, provenance, and conflict policy next to the content.
#[derive(Debug, Clone)]
pub struct PromptContribution {
    pub id: String,
    pub content: String,
    pub layer: Layer,
    pub role: Role,
    pub authority: PromptAuthority,
    pub source: ContextSource,
    pub channel: PromptChannel,
    pub cache_scope: CacheScope,
    pub priority: u8,
    pub tokens: u32,
    pub budget: TokenBudget,
    pub provenance: ContributionProvenance,
    pub conflict_policy: ConflictPolicy,
}

/// Normalized prompt fragment metadata.
///
/// `PromptSection` stays as the provider-facing content unit. During compile,
/// every section is normalized into a `PromptFragment`/`ContextFragment` so the
/// renderer and context manager can reason about identity, authority, cache
/// behavior, and marker boundaries without pushing that burden into providers.
#[derive(Debug, Clone, PartialEq)]
pub struct ContextFragment {
    pub id: String,
    pub role: Role,
    pub authority: PromptAuthority,
    pub source: ContextSource,
    pub cache_scope: CacheScope,
    pub markers: FragmentMarkers,
    pub layer: Layer,
    pub priority: u8,
    pub tokens: u32,
    pub content: String,
}

pub type PromptFragment = ContextFragment;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptAuthority {
    System,
    Rules,
    Tools,
    Memory,
    Events,
    Input,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextSource {
    Kernel,
    ProjectInstructions,
    ToolSchema,
    Memory,
    Event,
    User,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheScope {
    Static,
    Session,
    Turn,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FragmentMarkers {
    pub start: String,
    pub end: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ContextManifestEntry {
    pub id: String,
    pub layer: Layer,
    pub role: Role,
    pub authority: PromptAuthority,
    pub source: ContextSource,
    pub cache_scope: CacheScope,
    pub tokens: u32,
    pub summary: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[repr(u8)]
#[serde(rename_all = "snake_case")]
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

impl Layer {
    pub fn default_role(self) -> Role {
        match self {
            Layer::System => Role::System,
            Layer::Rules | Layer::Tools | Layer::Memory | Layer::Events | Layer::Input => {
                Role::User
            }
        }
    }

    pub fn default_authority(self) -> PromptAuthority {
        match self {
            Layer::System => PromptAuthority::System,
            Layer::Rules => PromptAuthority::Rules,
            Layer::Tools => PromptAuthority::Tools,
            Layer::Memory => PromptAuthority::Memory,
            Layer::Events => PromptAuthority::Events,
            Layer::Input => PromptAuthority::Input,
        }
    }

    pub fn default_source(self) -> ContextSource {
        match self {
            Layer::System => ContextSource::Kernel,
            Layer::Rules => ContextSource::ProjectInstructions,
            Layer::Tools => ContextSource::ToolSchema,
            Layer::Memory => ContextSource::Memory,
            Layer::Events => ContextSource::Event,
            Layer::Input => ContextSource::User,
        }
    }

    pub fn default_cache_scope(self) -> CacheScope {
        match self {
            Layer::System | Layer::Rules | Layer::Tools => CacheScope::Static,
            Layer::Memory => CacheScope::Session,
            Layer::Events | Layer::Input => CacheScope::Turn,
        }
    }

    pub fn default_channel(self) -> PromptChannel {
        match self {
            Layer::System => PromptChannel::System,
            Layer::Rules | Layer::Tools | Layer::Memory | Layer::Events | Layer::Input => {
                PromptChannel::UserContext
            }
        }
    }

    pub fn marker_name(self) -> &'static str {
        match self {
            Layer::System => "system",
            Layer::Rules => "rules",
            Layer::Tools => "tools",
            Layer::Memory => "memory",
            Layer::Events => "events",
            Layer::Input => "input",
        }
    }

    pub fn is_manifest_disclosable(self) -> bool {
        matches!(self, Layer::Memory | Layer::Events)
    }
}

impl PromptSection {
    pub fn new(content: impl Into<String>, layer: Layer, priority: u8, tokens: u32) -> Self {
        Self {
            content: content.into(),
            layer,
            priority,
            tokens,
        }
    }

    pub fn to_fragment(&self) -> PromptFragment {
        self.to_contribution(TokenBudget::Dynamic, "legacy-section")
            .to_fragment()
    }

    pub fn to_contribution(
        &self,
        budget: TokenBudget,
        provider: impl Into<String>,
    ) -> PromptContribution {
        let id = fragment_id(self.layer, self.priority, &self.content);
        PromptContribution {
            id,
            content: self.content.clone(),
            layer: self.layer,
            role: self.layer.default_role(),
            authority: self.layer.default_authority(),
            source: self.layer.default_source(),
            channel: self.layer.default_channel(),
            cache_scope: self.layer.default_cache_scope(),
            priority: self.priority,
            tokens: self.tokens,
            budget,
            provenance: ContributionProvenance {
                provider: provider.into(),
                source_ref: None,
            },
            conflict_policy: ConflictPolicy::Append,
        }
    }
}

impl PromptContribution {
    pub fn new(
        id: impl Into<String>,
        content: impl Into<String>,
        layer: Layer,
        priority: u8,
        tokens: u32,
        budget: TokenBudget,
        provider: impl Into<String>,
    ) -> Self {
        let content = content.into();
        let id = id.into();
        Self {
            id,
            content,
            layer,
            role: layer.default_role(),
            authority: layer.default_authority(),
            source: layer.default_source(),
            channel: layer.default_channel(),
            cache_scope: layer.default_cache_scope(),
            priority,
            tokens,
            budget,
            provenance: ContributionProvenance {
                provider: provider.into(),
                source_ref: None,
            },
            conflict_policy: ConflictPolicy::Append,
        }
    }

    pub fn from_section(
        section: PromptSection,
        budget: TokenBudget,
        provider: impl Into<String>,
    ) -> Self {
        section.to_contribution(budget, provider)
    }

    pub fn section(&self) -> PromptSection {
        PromptSection {
            content: self.content.clone(),
            layer: self.layer,
            priority: self.priority,
            tokens: self.tokens,
        }
    }

    pub fn to_fragment(&self) -> PromptFragment {
        let marker_name = self.layer.marker_name();
        PromptFragment {
            markers: FragmentMarkers {
                start: format!(
                    "<lattice-context id=\"{}\" layer=\"{}\" authority=\"{:?}\" source=\"{:?}\" cache_scope=\"{:?}\" channel=\"{:?}\" provider=\"{}\">",
                    self.id,
                    marker_name,
                    self.authority,
                    self.source,
                    self.cache_scope,
                    self.channel,
                    self.provenance.provider,
                ),
                end: format!(
                    "</lattice-context id=\"{}\" layer=\"{}\">",
                    self.id, marker_name
                ),
            },
            id: self.id.clone(),
            role: self.role.clone(),
            authority: self.authority,
            source: self.source,
            cache_scope: self.cache_scope,
            layer: self.layer,
            priority: self.priority,
            tokens: self.tokens,
            content: self.content.clone(),
        }
    }
}

impl ContextFragment {
    pub fn render_marked(&self) -> String {
        format!(
            "{}\n{}\n{}",
            self.markers.start, self.content, self.markers.end
        )
    }

    pub fn manifest_entry(&self) -> ContextManifestEntry {
        ContextManifestEntry {
            id: self.id.clone(),
            layer: self.layer,
            role: self.role.clone(),
            authority: self.authority,
            source: self.source,
            cache_scope: self.cache_scope,
            tokens: self.tokens,
            summary: summarize_fragment(&self.content),
        }
    }
}

impl ContextManifestEntry {
    pub fn render_line(&self) -> String {
        format!(
            "- id={} layer={:?} role={:?} authority={:?} source={:?} cache_scope={:?} tokens={} summary=\"{}\"",
            self.id,
            self.layer,
            self.role,
            self.authority,
            self.source,
            self.cache_scope,
            self.tokens,
            self.summary.replace('"', "'"),
        )
    }
}

fn fragment_id(layer: Layer, priority: u8, content: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    let hash = hex::encode(hasher.finalize());
    format!("{}-{}-{}", layer.marker_name(), priority, &hash[..16])
}

fn summarize_fragment(content: &str) -> String {
    let compact = content.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut summary = compact.chars().take(160).collect::<String>();
    if compact.chars().count() > 160 {
        summary.push_str("...");
    }
    summary
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
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenBudget {
    Fixed(u32),
    Ratio(f64),
    Dynamic,
}

/// Context passed to each provider during produce().
/// Borrows agent state — constructed fresh per compile() call.
pub struct AssemblyContext<'a> {
    pub request_id: &'a str,
    pub model: &'a ResolvedModel,
    pub user_input: &'a str,
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
    /// Contributions that survived budget trimming, in render order.
    pub contributions: Vec<PromptContribution>,
    /// Normalized fragments for sections that were rendered inline.
    pub fragments: Vec<PromptFragment>,
    /// Manifest entries for fragments withheld from the active prompt because
    /// their provider budget was insufficient.
    pub context_manifest: Vec<ContextManifestEntry>,
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

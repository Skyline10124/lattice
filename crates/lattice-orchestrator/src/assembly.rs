//! Agent assembly from profile -- the canonical construction path.
//!
//! Every entry point (Pipeline, fork, MicroAgent, CLI) should use
//! `assemble_agent()` to ensure consistent wiring of system prompt,
//! tools, executor, and memory.

use std::collections::{BTreeSet, HashMap};
use std::str::FromStr;
use std::sync::Arc;

use crate::profile::AgentProfile;
use crate::runner::AgentRunner;
use lattice_agent::{memory::Memory, Agent};
use lattice_core::router::ModelRouter;
use lattice_core::types::ToolDefinition;
use lattice_prompt::{ContextProvider, ProviderFactory};
use lattice_tools::{
    default_tool_definitions, resolve_capabilities, tool_registry::ToolRegistry,
    CapabilitySelection, DefaultToolExecutor, ToolBundle, ToolPreset,
};
use tracing::warn;

const RETIRED_TOOL_NAMES: &[&str] = &["multi_patch"];

/// Result of assemble_agent -- contains the runner and optional spec metadata.
pub struct AssembledAgent {
    pub runner: AgentRunner,
    pub prompt_spec: Option<lattice_prompt::PromptSpec>,
}

#[derive(Clone)]
pub struct AgentAssemblyOptions {
    pub project_root: String,
    pub sandbox: Option<lattice_tools::SandboxConfig>,
    pub allow_empty_capabilities: bool,
}

impl Default for AgentAssemblyOptions {
    fn default() -> Self {
        Self {
            project_root: ".".into(),
            sandbox: None,
            allow_empty_capabilities: false,
        }
    }
}

#[derive(Clone, Default)]
pub struct AgentFactory {
    memory: Option<Arc<dyn Memory>>,
    tool_registry: Option<Arc<ToolRegistry>>,
    model_router: Option<Arc<ModelRouter>>,
    credentials: Option<HashMap<String, String>>,
}

#[derive(Debug, thiserror::Error)]
pub enum AgentFactoryError {
    #[error("Resolve failed: {0}")]
    Resolve(#[from] lattice_core::LatticeError),
    #[error("Assembly failed: {0}")]
    Assembly(String),
}

impl AgentFactory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_memory(mut self, memory: Option<Arc<dyn Memory>>) -> Self {
        self.memory = memory;
        self
    }

    pub fn with_tool_registry(mut self, registry: Option<Arc<ToolRegistry>>) -> Self {
        self.tool_registry = registry;
        self
    }

    pub fn with_model_router(mut self, router: Option<Arc<ModelRouter>>) -> Self {
        self.model_router = router;
        self
    }

    pub fn with_credentials(mut self, credentials: Option<HashMap<String, String>>) -> Self {
        self.credentials = credentials;
        self
    }

    pub fn resolve_model(
        &self,
        model: &str,
    ) -> Result<lattice_core::ResolvedModel, lattice_core::LatticeError> {
        if let Some(router) = &self.model_router {
            return router.resolve(model, None);
        }
        match &self.credentials {
            Some(creds) => ModelRouter::with_credentials(creds.clone()).resolve(model, None),
            None => ModelRouter::new().resolve(model, None),
        }
    }

    pub fn assemble(&self, profile: &AgentProfile) -> Result<AssembledAgent, AgentFactoryError> {
        let resolved = self.resolve_model(&profile.agent.model)?;

        // Build built-in factories (all 4 ProviderKind variants, per D-11)
        let factories: Vec<Box<dyn lattice_prompt::ProviderFactory>> = {
            let factories: Vec<Box<dyn lattice_prompt::ProviderFactory>> = vec![
                // ProjectInstructions factory (from lattice-prompt)
                Box::new(lattice_prompt::provider_factory::ProjectInstructionsFactory),
                // Memory factory (from lattice-agent) -- passes runtime memory so the
                // spec-declared provider can actually recall entries.
                Box::new(
                    lattice_agent::prompt::memory_provider::MemoryFactory::new()
                        .with_memory(self.memory.clone()),
                ),
            ];

            // Events factory (from lattice-agent, feature-gated on blob-store)
            #[cfg(feature = "blob-store")]
            {
                let mut factories = factories;
                factories.push(Box::new(lattice_agent::events_provider::EventsFactory));
                factories
            }

            #[cfg(not(feature = "blob-store"))]
            {
                factories
            }
        };

        assemble_agent(
            profile,
            resolved,
            self.memory.clone(),
            self.tool_registry.clone(),
            &factories,
        )
        .map_err(|e| AgentFactoryError::Assembly(e.to_string()))
    }
}

/// Assemble an AssembledAgent from a profile and resolved model.
///
/// This is the single canonical path for constructing a fully-wired agent.
/// Applies system prompt, tool definitions, tool executor, and memory
/// from the profile configuration.
///
/// When `tool_registry` is provided and non-empty, custom tool definitions
/// from the registry are merged with the default tools, and the executor
/// checks the registry first for custom tool handlers.
///
/// When `profile.prompt_spec` is present, two-pass assembly is performed:
/// 1. Static sections and content are registered as contributions
/// 2. Dynamic providers are created via factory dispatch on ProviderKind
pub fn assemble_agent(
    profile: &AgentProfile,
    resolved: lattice_core::ResolvedModel,
    memory: Option<Arc<dyn Memory>>,
    tool_registry: Option<Arc<ToolRegistry>>,
    factories: &[Box<dyn lattice_prompt::ProviderFactory>],
) -> Result<AssembledAgent, Box<dyn std::error::Error>> {
    assemble_agent_with_options(
        profile,
        resolved,
        memory,
        tool_registry,
        factories,
        AgentAssemblyOptions::default(),
    )
}

pub fn assemble_agent_with_options(
    profile: &AgentProfile,
    resolved: lattice_core::ResolvedModel,
    memory: Option<Arc<dyn Memory>>,
    tool_registry: Option<Arc<ToolRegistry>>,
    factories: &[Box<dyn lattice_prompt::ProviderFactory>],
    options: AgentAssemblyOptions,
) -> Result<AssembledAgent, Box<dyn std::error::Error>> {
    let mut agent = Agent::new(resolved);

    // Apply system prompt from profile.
    // When PromptSpec.content is present, the old prompt is injected as
    // {{system_prompt}} runtime variable instead of being set directly,
    // avoiding duplication (the content contribution already embeds it).
    let system_prompt = profile.system_prompt()?;
    let has_spec_content = profile
        .prompt_spec
        .as_ref()
        .and_then(|s| s.content.as_ref())
        .is_some();
    if !has_spec_content {
        agent.set_system_prompt(&system_prompt);
    }

    let (final_tools, final_tool_names) = resolve_profile_tools(
        profile,
        tool_registry.as_deref(),
        options.allow_empty_capabilities,
    )?;
    let mut executor = if let Some(reg) = &tool_registry {
        DefaultToolExecutor::new_with_registry(
            &options.project_root,
            Some(reg.clone() as Arc<dyn lattice_tools::RegistryToolAccess>),
        )
        .map_err(|e| format!("failed to build DefaultToolExecutor: {e}"))?
        .with_authorized_tools(final_tool_names.clone())
    } else {
        DefaultToolExecutor::new(&options.project_root)
            .map_err(|e| format!("failed to build DefaultToolExecutor: {e}"))?
            .with_authorized_tools(final_tool_names.clone())
    };
    if let Some(sandbox) = options.sandbox {
        executor = executor.with_sandbox(sandbox);
    }

    let local_tool_schema_factory = ToolSchemaFactory::from_definitions(final_tools.clone());

    let authorized_tool_names: Vec<String> = final_tools.iter().map(|td| td.name.clone()).collect();

    if !final_tools.is_empty() {
        agent = agent.with_tools(final_tools);
        agent = agent.with_tool_executor(Box::new(
            executor.with_authorized_tools(authorized_tool_names),
        ));
    } else {
        warn!("Agent '{}' has no tool definitions", profile.agent.name);
    }

    // --- PromptSpec processing (after tools registration, per D-16) ---
    let mut spec_metadata: Option<lattice_prompt::PromptSpec> = None;
    if let Some(ref spec) = profile.prompt_spec {
        // Pass 3: hooks (Phase 8, HOOK-07)
        if !spec.hooks.is_empty() {
            let hooks = lattice_hooks::build_prompt_hooks(&spec.hooks)
                .map_err(|e| format!("hook build failed: {e}"))?;
            agent = agent.with_prompt_hooks(hooks);
        }

        // Render content field as System-layer contribution (D-03, SPEC-06)
        // Bridge: inject old system.prompt as {{system_prompt}} runtime variable
        #[allow(deprecated)]
        let system_prompt_text = profile.system.prompt.clone();
        let runtime_vars =
            std::collections::HashMap::from([("system_prompt".to_string(), system_prompt_text)]);
        if let Some(content_contribution) = spec.content_contribution(&runtime_vars)? {
            agent = agent.add_contribution(content_contribution);
        }

        // Pass 1: static sections -> contributions (D-17, SPEC-04)
        let contributions = spec.static_contributions()?;
        for contribution in contributions {
            agent = agent.add_contribution(contribution);
        }

        // Pass 2: dynamic providers via factory (D-17, SPEC-05)
        for provider_spec in &spec.providers {
            let provider = if provider_spec.kind == lattice_prompt::spec::ProviderKind::ToolSchema {
                local_tool_schema_factory.create(provider_spec)
            } else {
                let factory = factories
                    .iter()
                    .find(|f| f.kind() == provider_spec.kind)
                    .ok_or_else(|| {
                        format!("no ProviderFactory registered for {:?}", provider_spec.kind)
                    })?;
                factory.create(provider_spec)
            };
            agent = agent.with_provider_dyn(provider);
        }

        spec_metadata = Some(spec.clone());
    }

    // Only register memory via legacy path when the spec does NOT declare a
    // Memory provider. When it does, MemoryFactory already created a provider
    // with the runtime memory instance attached -- registering again via
    // agent.with_memory() would duplicate the provider.
    let spec_has_memory_provider = spec_metadata
        .as_ref()
        .map(|s| s.has_provider_kind(lattice_prompt::spec::ProviderKind::Memory))
        .unwrap_or(false);
    if let Some(ref mem) = memory {
        if !spec_has_memory_provider {
            agent = agent.with_memory(Arc::clone(mem));
        }
    }

    let mut runner = AgentRunner::from_profile(profile.clone(), agent);
    if let Some(ref mem) = memory {
        runner = runner.with_memory(Arc::clone(mem));
    }
    Ok(AssembledAgent {
        runner,
        prompt_spec: spec_metadata,
    })
}

fn resolve_profile_tools(
    profile: &AgentProfile,
    tool_registry: Option<&ToolRegistry>,
    allow_empty_capabilities: bool,
) -> Result<(Vec<ToolDefinition>, BTreeSet<String>), Box<dyn std::error::Error>> {
    if profile.capabilities.presets.is_empty() && profile.capabilities.bundles.is_empty() {
        if allow_empty_capabilities && profile.tools.enabled.is_empty() {
            return Ok((Vec::new(), BTreeSet::new()));
        }
        return Err(format!(
            "agent profile '{}' must declare [capabilities] presets or bundles",
            profile.agent.name
        )
        .into());
    }

    let selection = CapabilitySelection {
        presets: profile
            .capabilities
            .presets
            .iter()
            .map(|preset| {
                ToolPreset::from_str(preset)
                    .map_err(|_| format!("unknown capability preset '{}'", preset))
            })
            .collect::<Result<Vec<_>, _>>()?,
        bundles: profile
            .capabilities
            .bundles
            .iter()
            .map(|bundle| {
                ToolBundle::from_str(bundle)
                    .map_err(|_| format!("unknown capability bundle '{}'", bundle))
            })
            .collect::<Result<Vec<_>, _>>()?,
    };
    let resolved = resolve_capabilities(&selection);
    let resolved_builtin_names = resolved.tool_names.clone();
    let built_in_names: BTreeSet<String> = default_tool_definitions()
        .into_iter()
        .map(|definition| definition.name)
        .collect();
    let requested: BTreeSet<String> = profile.tools.enabled.iter().cloned().collect();

    let mut final_tools = Vec::new();
    let mut final_tool_names = BTreeSet::new();

    for definition in resolved.definitions {
        if requested.is_empty() || requested.contains(&definition.name) {
            final_tool_names.insert(definition.name.clone());
            final_tools.push(definition);
        }
    }

    let registry_defs = match tool_registry {
        Some(registry) if !registry.is_empty() => registry.definitions(),
        _ => Vec::new(),
    };
    let registry_names: BTreeSet<String> = registry_defs.iter().map(|d| d.name.clone()).collect();

    for tool_name in &requested {
        if RETIRED_TOOL_NAMES.contains(&tool_name.as_str()) {
            return Err(format!(
                "tool '{}' has been retired and is not available for agent profile '{}'",
                tool_name, profile.agent.name
            )
            .into());
        }
        if resolved_builtin_names.contains(tool_name) {
            continue;
        }
        if built_in_names.contains(tool_name) {
            return Err(format!(
                "tool '{}' is not enabled by capabilities for agent profile '{}'",
                tool_name, profile.agent.name
            )
            .into());
        }
        if registry_names.contains(tool_name) {
            continue;
        }
        return Err(format!(
            "tool '{}' is not enabled by capabilities for agent profile '{}'",
            tool_name, profile.agent.name
        )
        .into());
    }

    if !requested.is_empty() {
        for definition in registry_defs {
            if requested.contains(&definition.name)
                && !built_in_names.contains(&definition.name)
                && final_tool_names.insert(definition.name.clone())
            {
                final_tools.push(definition);
            }
        }
    }

    Ok((final_tools, final_tool_names))
}

/// Wrapper that implements ToolSchemaSource for ToolRegistry.
struct RegistryToolSchemaSource {
    registry: Arc<ToolRegistry>,
}

impl lattice_prompt::ToolSchemaSource for RegistryToolSchemaSource {
    fn definitions(&self) -> Vec<lattice_core::types::ToolDefinition> {
        self.registry.definitions()
    }
}

#[derive(Clone)]
struct StaticToolSchemaSource {
    definitions: Vec<ToolDefinition>,
}

impl lattice_prompt::ToolSchemaSource for StaticToolSchemaSource {
    fn definitions(&self) -> Vec<ToolDefinition> {
        self.definitions.clone()
    }
}

/// Factory that creates ToolSchemaProvider from a PromptProviderSpec.
/// Lives in lattice-orchestrator because it bridges lattice-prompt and lattice-tools.
pub struct ToolSchemaFactory {
    source: Arc<dyn lattice_prompt::ToolSchemaSource>,
}

impl ToolSchemaFactory {
    /// Create a ToolSchemaFactory backed by a ToolRegistry.
    pub fn new(registry: Arc<ToolRegistry>) -> Self {
        Self {
            source: Arc::new(RegistryToolSchemaSource { registry }),
        }
    }

    pub fn from_definitions(definitions: Vec<ToolDefinition>) -> Self {
        Self {
            source: Arc::new(StaticToolSchemaSource { definitions }),
        }
    }
}

impl lattice_prompt::ProviderFactory for ToolSchemaFactory {
    fn kind(&self) -> lattice_prompt::spec::ProviderKind {
        lattice_prompt::spec::ProviderKind::ToolSchema
    }

    fn create(
        &self,
        spec: &lattice_prompt::spec::PromptProviderSpec,
    ) -> Box<dyn lattice_prompt::ContextProvider> {
        let mut provider = lattice_prompt::ToolSchemaProvider::new(Arc::clone(&self.source));
        provider.configure_from_spec(spec);
        Box::new(provider)
    }
}

/// Compatibility helper that lowers an `AgentProfile` into an `AgentTurnSpec`.
///
/// This delegates to [`crate::agent_spec::lower_profile_to_turn_spec`] and
/// exists so existing assembly callers have an obvious migration path.
/// It returns `AgentTurnSpec`, not `AssembledAgent`.
pub fn lower_agent_profile(
    profile: &AgentProfile,
) -> Result<crate::agent_spec::AgentTurnSpec, crate::agent_spec::ProfileLoweringError> {
    crate::agent_spec::lower_profile_to_turn_spec(profile)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_core::catalog::ApiProtocol;
    use lattice_core::types::ToolDefinition;
    use lattice_core::{CredentialStatus, ResolvedModel};
    use std::collections::HashMap;

    fn make_resolved() -> ResolvedModel {
        ResolvedModel {
            canonical_id: "test".into(),
            provider: "test".into(),
            api_key: None,
            base_url: "".into(),
            api_protocol: ApiProtocol::OpenAiChat,
            api_model_id: "test".into(),
            context_length: 8192,
            provider_specific: HashMap::new(),
            credential_status: CredentialStatus::Missing,
        }
    }

    #[allow(deprecated)]
    fn make_profile() -> AgentProfile {
        AgentProfile {
            agent: crate::profile::AgentConfig {
                name: "test-agent".into(),
                model: "test-model".into(),
                description: String::new(),
                skippable: false,
                tags: vec![],
            },
            system: crate::profile::SystemConfig {
                prompt: "You are a test agent.".into(),
                file: None,
            },
            tools: crate::profile::ToolsConfig::default(),
            capabilities: crate::profile::CapabilitiesConfig {
                presets: vec!["coding".into()],
                bundles: Vec::new(),
            },
            behavior: crate::profile::BehaviorConfig::default(),
            handoff: crate::profile::HandoffConfig::default(),
            orchestrator: crate::profile::OrchestratorConfigProfile::default(),
            memory: crate::profile::MemoryConfigProfile::default(),
            plugins: None,
            prompt_spec: None,
        }
    }

    fn tool_names_for(profile: &AgentProfile) -> Vec<String> {
        let (tools, _) = resolve_profile_tools(profile, None, false).unwrap();
        tools.into_iter().map(|tool| tool.name).collect()
    }

    fn make_tool(name: &str) -> ToolDefinition {
        ToolDefinition {
            name: name.into(),
            description: "test".into(),
            parameters: serde_json::json!({}),
        }
    }

    #[test]
    fn assemble_fails_without_capabilities() {
        let mut profile = make_profile();
        profile.capabilities = crate::profile::CapabilitiesConfig::default();
        let result = assemble_agent(&profile, make_resolved(), None, None, &[]);

        assert!(result
            .err()
            .unwrap()
            .to_string()
            .contains("must declare [capabilities]"));
    }

    #[test]
    fn reader_profile_exposes_only_workspace_and_filesystem_tools() {
        let mut profile = make_profile();
        profile.capabilities = crate::profile::CapabilitiesConfig {
            presets: vec!["reader".into()],
            bundles: Vec::new(),
        };

        let names = tool_names_for(&profile);

        for expected in [
            "workspace_context",
            "read_file",
            "read_files",
            "grep",
            "list_directory",
            "find_files",
        ] {
            assert!(names.contains(&expected.to_string()), "{expected} missing");
        }
        for denied in [
            "write_file",
            "patch",
            "multi_patch",
            "bash",
            "todo_write",
            "web_search",
        ] {
            assert!(!names.contains(&denied.to_string()), "{denied} leaked");
        }
    }

    #[test]
    fn reader_profile_rejects_tools_enabled_multi_patch() {
        let mut profile = make_profile();
        profile.capabilities = crate::profile::CapabilitiesConfig {
            presets: vec!["reader".into()],
            bundles: Vec::new(),
        };
        profile.tools.enabled = vec!["multi_patch".into()];

        let result = resolve_profile_tools(&profile, None, false);

        assert!(result
            .err()
            .unwrap()
            .to_string()
            .contains("has been retired"));
    }

    #[test]
    fn reader_profile_tools_enabled_can_narrow_to_grep() {
        let mut profile = make_profile();
        profile.capabilities = crate::profile::CapabilitiesConfig {
            presets: vec!["reader".into()],
            bundles: Vec::new(),
        };
        profile.tools.enabled = vec!["grep".into()];

        let names = tool_names_for(&profile);

        assert_eq!(names, vec!["grep".to_string()]);
    }

    #[test]
    fn coding_profile_exposes_editing_shell_planning_and_web() {
        let profile = make_profile();

        let names = tool_names_for(&profile);

        for expected in ["write_file", "patch", "bash", "todo_write", "web_search"] {
            assert!(names.contains(&expected.to_string()), "{expected} missing");
        }
        assert!(
            !names.contains(&"multi_patch".to_string()),
            "multi_patch leaked"
        );
    }

    #[test]
    fn default_builtin_tool_inventory_excludes_multi_patch() {
        let built_in_names = default_tool_definitions()
            .into_iter()
            .map(|definition| definition.name)
            .collect::<BTreeSet<_>>();

        assert!(built_in_names.contains("patch"));
        assert!(!built_in_names.contains("multi_patch"));
    }

    #[test]
    fn unknown_capability_string_reports_type_and_value() {
        let mut profile = make_profile();
        profile.capabilities.presets = vec!["typo-preset".into()];

        let result = resolve_profile_tools(&profile, None, false);

        assert!(result
            .err()
            .unwrap()
            .to_string()
            .contains("unknown capability preset 'typo-preset'"));
    }

    #[test]
    fn registry_tool_can_be_added_only_when_requested() {
        let mut profile = make_profile();
        profile.tools.enabled = vec!["custom_tool".into()];
        let mut registry = ToolRegistry::new();
        registry.register(
            "custom_tool",
            lattice_tools::tool_registry::ToolHandler::Native(Arc::new(|_| Ok("ok".into()))),
            make_tool("custom_tool"),
        );

        let names = resolve_profile_tools(&profile, Some(&registry), false)
            .unwrap()
            .1;

        assert_eq!(names, BTreeSet::from(["custom_tool".to_string()]));
    }

    #[test]
    fn registry_builtin_name_cannot_bypass_capabilities() {
        let mut profile = make_profile();
        profile.capabilities = crate::profile::CapabilitiesConfig {
            presets: vec!["reader".into()],
            bundles: Vec::new(),
        };
        profile.tools.enabled = vec!["multi_patch".into()];
        let mut registry = ToolRegistry::new();
        registry.register(
            "multi_patch",
            lattice_tools::tool_registry::ToolHandler::Native(Arc::new(|_| Ok("ok".into()))),
            make_tool("multi_patch"),
        );

        let result = resolve_profile_tools(&profile, Some(&registry), false);

        assert!(result
            .err()
            .unwrap()
            .to_string()
            .contains("has been retired"));
    }

    #[test]
    fn registry_multi_patch_cannot_reenter_coding_profile() {
        let mut profile = make_profile();
        profile.tools.enabled = vec!["multi_patch".into()];
        let mut registry = ToolRegistry::new();
        registry.register(
            "multi_patch",
            lattice_tools::tool_registry::ToolHandler::Native(Arc::new(|_| Ok("ok".into()))),
            make_tool("multi_patch"),
        );

        let result = resolve_profile_tools(&profile, Some(&registry), false);

        assert!(result
            .err()
            .unwrap()
            .to_string()
            .contains("has been retired"));
    }

    #[test]
    fn assemble_without_spec_returns_runner_with_none_spec() {
        let profile = make_profile();
        let resolved = make_resolved();
        let factories: Vec<Box<dyn lattice_prompt::ProviderFactory>> = vec![];
        let assembled = assemble_agent(&profile, resolved, None, None, &factories).unwrap();
        assert!(assembled.prompt_spec.is_none());
    }

    #[test]
    fn assemble_with_spec_processes_content_and_sections() {
        let mut profile = make_profile();
        profile.prompt_spec = Some(lattice_prompt::PromptSpec {
            content: Some("You are {{system_prompt}} with extras.".to_string()),
            sections: vec![lattice_prompt::spec::PromptSectionSpec {
                id: "rules".to_string(),
                layer: lattice_prompt::Layer::Rules,
                channel: None,
                priority: 1,
                cache_scope: None,
                authority: None,
                source: None,
                content: Some("Follow these rules.".to_string()),
                template: None,
                budget: lattice_prompt::spec::BudgetSpec::Dynamic,
            }],
            ..Default::default()
        });
        let resolved = make_resolved();
        let factories: Vec<Box<dyn lattice_prompt::ProviderFactory>> = vec![];
        let assembled = assemble_agent(&profile, resolved, None, None, &factories).unwrap();
        assert!(assembled.prompt_spec.is_some());
        let spec = assembled.prompt_spec.as_ref().unwrap();
        assert_eq!(spec.sections.len(), 1);
    }

    #[test]
    fn assemble_with_factory_creates_dynamic_provider() {
        use lattice_prompt::spec::{BudgetSpec, ProviderKind};
        use lattice_prompt::Layer;

        let mut profile = make_profile();
        profile.prompt_spec = Some(lattice_prompt::PromptSpec {
            providers: vec![lattice_prompt::spec::PromptProviderSpec {
                id: "test-pi".to_string(),
                kind: ProviderKind::ProjectInstructions,
                layer: Layer::Rules,
                channel: None,
                priority: 0,
                cache_scope: None,
                budget: BudgetSpec::Dynamic,
                limit: None,
            }],
            ..Default::default()
        });
        let resolved = make_resolved();
        let factories: Vec<Box<dyn lattice_prompt::ProviderFactory>> = vec![Box::new(
            lattice_prompt::provider_factory::ProjectInstructionsFactory,
        )];
        let assembled = assemble_agent(&profile, resolved, None, None, &factories).unwrap();
        assert!(assembled.prompt_spec.is_some());
    }

    #[test]
    fn assemble_fails_when_factory_missing() {
        use lattice_prompt::spec::{BudgetSpec, ProviderKind};
        use lattice_prompt::Layer;

        let mut profile = make_profile();
        profile.prompt_spec = Some(lattice_prompt::PromptSpec {
            providers: vec![lattice_prompt::spec::PromptProviderSpec {
                id: "missing-factory".to_string(),
                kind: ProviderKind::Memory,
                layer: Layer::Memory,
                channel: None,
                priority: 5,
                cache_scope: None,
                budget: BudgetSpec::Dynamic,
                limit: None,
            }],
            ..Default::default()
        });
        let resolved = make_resolved();
        let factories: Vec<Box<dyn lattice_prompt::ProviderFactory>> = vec![];
        let result = assemble_agent(&profile, resolved, None, None, &factories);
        assert!(result.is_err());
        let err = result.err().unwrap();
        assert!(err
            .to_string()
            .contains("no ProviderFactory registered for Memory"));
    }

    #[test]
    fn assemble_with_spec_processes_hooks() {
        use lattice_prompt::spec::{BudgetSpec, PromptHookSpec, PromptTargetSpec};
        use lattice_prompt::Layer;

        let mut profile = make_profile();
        profile.prompt_spec = Some(lattice_prompt::PromptSpec {
            hooks: vec![PromptHookSpec {
                id: "test-hook".to_string(),
                point: "post_context_collect".to_string(),
                action: "add_section".to_string(),
                target: Some(PromptTargetSpec {
                    id: None,
                    layer: Some(Layer::Events),
                }),
                budget: Some(BudgetSpec::Fixed { tokens: Some(50) }),
                content: Some("hook injected".to_string()),
            }],
            ..Default::default()
        });
        let resolved = make_resolved();
        let factories: Vec<Box<dyn lattice_prompt::ProviderFactory>> = vec![];
        let assembled = assemble_agent(&profile, resolved, None, None, &factories).unwrap();
        assert!(assembled.prompt_spec.is_some());
    }

    #[test]
    fn assemble_with_invalid_hook_spec_fails() {
        use lattice_prompt::spec::PromptHookSpec;

        let mut profile = make_profile();
        profile.prompt_spec = Some(lattice_prompt::PromptSpec {
            hooks: vec![PromptHookSpec {
                id: "bad-hook".to_string(),
                point: "nonexistent_point".to_string(),
                action: "abort".to_string(),
                target: None,
                budget: None,
                content: None,
            }],
            ..Default::default()
        });
        let resolved = make_resolved();
        let factories: Vec<Box<dyn lattice_prompt::ProviderFactory>> = vec![];
        let result = assemble_agent(&profile, resolved, None, None, &factories);
        assert!(result.is_err());
        let err = result.err().unwrap();
        assert!(err.to_string().contains("hook build failed"));
    }
}

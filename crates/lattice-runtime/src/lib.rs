//! Deep runtime interface for LATTICE.
//!
//! This crate concentrates the construction knowledge for model routing,
//! profile registries, tool/plugin registries, memory, events, and pipeline
//! execution. Lower crates remain available for extension, but application
//! entrypoints should prefer [`Runtime`] so wiring policy has one locality.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::Stream;
use lattice_agent::audit::AuditLog;
use lattice_agent::hook::HookChain;
use lattice_agent::memory::Memory;
use lattice_agent::{Agent, ContextManager, LoopEvent};
use lattice_core::catalog::{ModelCatalogEntry, ResolvedModel};
use lattice_core::router::ModelRouter;
use lattice_core::streaming::StreamEvent;
use lattice_core::types::{FunctionCall, Message, Role, ToolCall, ToolDefinition};
use lattice_orchestrator::events::EventBus;
use lattice_orchestrator::{
    AgentRegistry, LatticeDir, MicroAgent, Orchestrator, Pipeline, PipelineRun,
};
use lattice_plugin::registry::PluginRegistry;
use lattice_prompt as prompt;
use lattice_sandbox::{
    approval_kind_from_approve_command, ApprovalKind, ApprovedRule, LandlockConfig,
    PolicyAuthority, RiskClass, SandboxConfig, ValidationError,
};
use lattice_session::{CompactBoundary, Session, SessionMessage, SessionStore};
use lattice_tools::{
    resolve_capabilities, tool_registry::ToolRegistry, CapabilitySelection, DefaultToolExecutor,
    ToolBundle,
};

pub mod handle;
pub mod preset;
pub mod turn;
pub mod turn_executor;
pub use handle::{AgentHandleId, AgentHandleKey, AgentHandleSnapshot, RuntimePluginOverlay};
pub use preset::coding_system_prompt;
pub use turn::{RuntimeTurnEvent, TurnCompletion, TurnId, TurnRequest, TurnStatus, TurnStream};
pub use turn_executor::RuntimeAgentTurnExecutor;

// --- Security types (CLI-facing) ---
#[derive(Clone, Debug)]
pub struct RuntimeSecurityConfig {
    pub sandbox_mode: String,
    pub read_allowlist: Vec<String>,
    pub write_allowlist: Vec<String>,
    pub command_allowlist: Vec<String>,
    pub hook_chain: bool,
    pub landlock: bool,
    pub audit: bool,
    pub audit_dir: Option<PathBuf>,
    pub max_command_timeout: Option<u32>,
    pub max_read_size: Option<usize>,
    pub max_write_size: Option<usize>,
    pub max_http_response_size: Option<usize>,
}

impl Default for RuntimeSecurityConfig {
    fn default() -> Self {
        Self {
            sandbox_mode: "project".into(),
            read_allowlist: vec![],
            write_allowlist: vec![],
            command_allowlist: vec![],
            hook_chain: false,
            landlock: false,
            audit: false,
            audit_dir: None,
            max_command_timeout: None,
            max_read_size: None,
            max_write_size: None,
            max_http_response_size: None,
        }
    }
}

#[derive(Clone)]
pub struct RuntimeSecurity {
    pub mode_label: String,
    pub sandbox: SandboxConfig,
    /// Dynamically-approved rules shared with the sandbox layer via `Arc<Mutex>`.
    /// Backed by the same `Arc` as `sandbox.approved_rules`, so once a rule is
    /// approved here, sandbox checks see it immediately.
    pub approved_rules: Arc<Mutex<Vec<ApprovedRule>>>,
    /// Policy authority for centralized dynamic approval decisions.
    /// Shares the same backing store as `approved_rules`. When wired into
    /// `sandbox.policy_authority`, sandbox checks delegate to this.
    pub policy_authority: PolicyAuthority,
    audit_log: Option<Arc<AuditLog>>,
}

impl RuntimeSecurity {
    /// Approve an operation rule after validation.
    ///
    /// Rejects overly broad or dangerous rules before adding them to the
    /// shared approved set. Once approved, the rule is immediately visible
    /// to sandbox check functions via the shared `Arc<Mutex<Vec<ApprovedRule>>>`
    /// and the `PolicyAuthority`.
    ///
    /// Bundle validation uses `ToolBundle::from_str` (tools-specific) because
    /// lattice-policy cannot depend on lattice-tools.
    pub fn approve_rule(&self, rule: ApprovedRule) -> Result<(), ValidationError> {
        // Validate bundles with lattice-tools before delegating to policy authority.
        // Policy authority does its own command/path validation on approve.
        if let ApprovalKind::Bundle { bundle_name } = &rule.kind {
            ToolBundle::from_str(bundle_name)
                .map_err(|_| ValidationError::UnknownBundle(bundle_name.clone()))?;
        }
        let shared_with_legacy = Arc::ptr_eq(&self.approved_rules, self.policy_authority.inner());
        self.policy_authority.approve(rule.clone(), None)?;
        if !shared_with_legacy {
            self.approved_rules
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(rule);
        }
        Ok(())
    }

    pub async fn reap_audit(&self) {
        if let Some(log) = &self.audit_log {
            log.reap_background_tasks().await;
        }
    }

    /// Execute a bash command in the given workdir, gated by sandbox policy.
    pub async fn execute_bash(&self, workdir: &Path, cmd: &str) -> Result<String, RuntimeError> {
        let project_root = workdir.to_string_lossy().to_string();
        let output = DefaultToolExecutor::execute_bash(&project_root, &self.sandbox, cmd)
            .await
            .map_err(RuntimeError::Config)?;
        let mut rendered = String::from_utf8_lossy(&output.stdout).to_string();
        if !output.stderr.is_empty() {
            if !rendered.is_empty() {
                rendered.push('\n');
            }
            rendered.push_str(&String::from_utf8_lossy(&output.stderr));
        }
        Ok(rendered)
    }
}

pub fn build_runtime_security(
    config: &RuntimeSecurityConfig,
    workdir: &Path,
) -> Result<RuntimeSecurity, RuntimeError> {
    let canonical_workdir = std::fs::canonicalize(workdir)
        .unwrap_or_else(|_| workdir.to_path_buf())
        .to_string_lossy()
        .to_string();

    let mode_label = config.sandbox_mode.clone();

    let audit_log = if config.audit {
        let dir = config
            .audit_dir
            .clone()
            .unwrap_or_else(|| workdir.join(".lattice-audit"));
        Some(Arc::new(AuditLog::new(dir)))
    } else {
        None
    };

    let hook_chain = if config.hook_chain {
        Some(Arc::new(HookChain::new(vec![Box::new(
            lattice_agent::hook::VetHook::new(PathBuf::from(&canonical_workdir)),
        )])))
    } else {
        None
    };

    let landlock = if config.landlock {
        Some(LandlockConfig::default())
    } else {
        None
    };

    // Shared approved_rules state — visible to both RuntimeSecurity and SandboxConfig.
    let approved_rules = Arc::new(Mutex::new(Vec::new()));
    let policy_authority = PolicyAuthority::from_shared_rules(Arc::clone(&approved_rules));

    let mut sandbox = match config.sandbox_mode.as_str() {
        "permissive" => SandboxConfig::permissive(),
        "project" | "strict" => SandboxConfig {
            read_allowlist: if config.read_allowlist.is_empty() {
                vec![canonical_workdir.clone()]
            } else {
                config.read_allowlist.clone()
            },
            write_allowlist: if config.write_allowlist.is_empty() {
                vec![canonical_workdir.clone()]
            } else {
                config.write_allowlist.clone()
            },
            command_allowlist: if config.command_allowlist.is_empty() {
                vec![
                    "cargo test".into(),
                    "cargo build".into(),
                    "cargo check".into(),
                    "cargo fmt".into(),
                    "cargo clippy".into(),
                    "git status".into(),
                    "git diff".into(),
                    "git log".into(),
                    "grep".into(),
                    "find".into(),
                    "ls".into(),
                ]
            } else {
                config.command_allowlist.clone()
            },
            hook: hook_chain.map(|hc| hc as Arc<dyn lattice_sandbox::hooks::CommandHook>),
            landlock,
            audit: audit_log.clone().map(|log| {
                Arc::new(lattice_agent::sandbox::AuditLogSink::new(log))
                    as Arc<dyn lattice_sandbox::audit::AuditSink>
            }),
            max_command_timeout: config.max_command_timeout.unwrap_or(30),
            max_read_size: config.max_read_size.unwrap_or(10 * 1024 * 1024),
            max_write_size: config.max_write_size.unwrap_or(1024 * 1024),
            max_http_response_size: config.max_http_response_size.unwrap_or(10 * 1024 * 1024),
            ..SandboxConfig::default()
        },
        mode => {
            return Err(RuntimeError::Config(format!(
                "unknown security.sandbox_mode '{}': expected 'project', 'strict', or 'permissive'",
                mode
            )));
        }
    };

    // Wire the shared approved_rules and policy_authority into the sandbox
    // so check functions delegate through PolicyAuthority.
    sandbox.approved_rules = Some(Arc::clone(&approved_rules));
    sandbox.policy_authority = Some(policy_authority.clone());

    Ok(RuntimeSecurity {
        mode_label,
        sandbox,
        approved_rules,
        policy_authority,
        audit_log,
    })
}

fn runtime_security_policy_key(security: &RuntimeSecurity) -> String {
    let sandbox = &security.sandbox;
    format!(
        "mode={};read={:?};write={:?};cmd={:?};hook={};landlock={};audit={};timeout={};read_max={};write_max={};http_max={}",
        security.mode_label,
        sandbox.read_allowlist,
        sandbox.write_allowlist,
        sandbox.command_allowlist,
        sandbox.hook.is_some(),
        sandbox.landlock.is_some(),
        sandbox.audit.is_some(),
        sandbox.max_command_timeout,
        sandbox.max_read_size,
        sandbox.max_write_size,
        sandbox.max_http_response_size,
    )
}

fn approve_task_state_proposal(
    security: &RuntimeSecurity,
    task_state: &mut lattice_agent::TaskState,
    selector: &str,
) -> Result<String, RuntimeError> {
    let selector = selector.trim();
    if selector != "last" && !is_approval_id(selector) {
        return Err(RuntimeError::Config(format!(
            "unknown approval selector '{selector}'; use 'last' or an ap-N id"
        )));
    }

    let proposal = if selector == "last" {
        task_state.latest_pending_approval()
    } else {
        task_state.find_pending_approval(selector)
    }
    .cloned()
    .ok_or_else(|| {
        RuntimeError::Config(format!("no pending approval proposal matches '{selector}'"))
    })?;

    let risk_class = parse_approval_risk_class(&proposal.risk_class)?;
    let kind = approval_kind_from_approve_command(&proposal.approve_command).map_err(|err| {
        RuntimeError::Config(format!(
            "malformed approval command for {}: {err}",
            proposal.id
        ))
    })?;
    security
        .approve_rule(ApprovedRule { kind, risk_class })
        .map_err(|err| {
            RuntimeError::Config(format!("approval rejected for {}: {err}", proposal.id))
        })?;

    if !task_state.mark_approval_resolved(&proposal.id) {
        return Err(RuntimeError::Config(format!(
            "proposal {} was already resolved",
            proposal.id
        )));
    }

    Ok(format!("Approved proposal {}", proposal.id))
}

fn is_approval_id(selector: &str) -> bool {
    selector
        .strip_prefix("ap-")
        .is_some_and(|suffix| !suffix.is_empty() && suffix.chars().all(|ch| ch.is_ascii_digit()))
}

fn parse_approval_risk_class(value: &str) -> Result<RiskClass, RuntimeError> {
    match value.trim() {
        "read" => Ok(RiskClass::Read),
        "mutate" => Ok(RiskClass::Mutate),
        "destructive" => Ok(RiskClass::Destructive),
        other => Err(RuntimeError::Config(format!(
            "unknown approval risk class '{other}'"
        ))),
    }
}

// --- AgentSpec (CLI-facing builder) ---
#[derive(Clone)]
pub struct AgentSpec {
    model: String,
    provider_override: Option<String>,
    system_prompt: Option<String>,
    messages: Vec<Message>,
    thinking_effort: Option<String>,
    project_root: Option<String>,
    cwd: Option<String>,
    security: Option<RuntimeSecurity>,
    capabilities: CapabilitySelection,
    task_state_seed: Option<lattice_agent::TaskState>,
}

impl AgentSpec {
    pub fn new(model: String) -> Self {
        Self {
            model,
            provider_override: None,
            system_prompt: None,
            messages: Vec::new(),
            thinking_effort: None,
            project_root: None,
            cwd: None,
            security: None,
            capabilities: CapabilitySelection::default(),
            task_state_seed: None,
        }
    }

    pub fn provider_override(mut self, override_: Option<String>) -> Self {
        self.provider_override = override_;
        self
    }

    pub fn system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(prompt.into());
        self
    }

    pub fn messages(mut self, messages: Vec<Message>) -> Self {
        self.messages = messages;
        self
    }

    pub fn thinking_effort(mut self, effort: Option<String>) -> Self {
        self.thinking_effort = effort;
        self
    }

    pub fn project_root(mut self, root: impl Into<String>) -> Self {
        self.project_root = Some(root.into());
        self
    }

    pub fn cwd(mut self, cwd: impl Into<String>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    pub fn security(mut self, security: Option<RuntimeSecurity>) -> Self {
        self.security = security;
        self
    }

    pub fn task_state_seed(mut self, seed: Option<lattice_agent::TaskState>) -> Self {
        self.task_state_seed = seed;
        self
    }

    pub fn capability_preset(mut self, preset: impl Into<String>) -> Self {
        let preset = preset.into();
        match preset.as_str() {
            "coding" | "code-agent" => {
                self.capabilities.bundles.extend([
                    ToolBundle::Workspace,
                    ToolBundle::Filesystem,
                    ToolBundle::Editing,
                    ToolBundle::Shell,
                    ToolBundle::Planning,
                    ToolBundle::Web,
                ]);
            }
            "reader" => {
                self.capabilities.bundles.push(ToolBundle::Filesystem);
            }
            "minimal" => { /* no extra bundles */ }
            other => tracing::warn!("unknown capability preset '{}' on AgentSpec", other),
        }
        self
    }

    pub fn capability_bundle(mut self, bundle: impl Into<String>) -> Self {
        let bundle = bundle.into();
        match ToolBundle::from_str(&bundle) {
            Ok(bundle) => self.capabilities.bundles.push(bundle),
            Err(_) => tracing::warn!("unknown capability bundle '{}' on AgentSpec", bundle),
        }
        self
    }

    pub fn minimal_capabilities(self) -> Self {
        self.capability_preset("minimal")
    }
}

pub struct BuiltAgent {
    pub agent: Agent,
    pub model: ResolvedModel,
    pub context_length: u32,
    pub tool_definitions: Vec<ToolDefinition>,
}

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error(transparent)]
    Core(#[from] lattice_core::LatticeError),
    #[error("runtime configuration error: {0}")]
    Config(String),
    #[error("agent assembly failed: {0}")]
    Assembly(String),
    #[error("agent handle not found: {0}")]
    HandleNotFound(String),
    #[error("agent handle busy: {0}")]
    HandleBusy(String),
    #[error("turn not found: {0}")]
    TurnNotFound(String),
}

#[derive(Clone)]
pub struct RuntimeConfig {
    pub name: String,
    pub event_capacity: usize,
    pub max_pipeline_iterations: usize,
    pub credentials: HashMap<String, String>,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            name: "lattice".into(),
            event_capacity: 256,
            max_pipeline_iterations: 20,
            credentials: HashMap::new(),
        }
    }
}

#[derive(Clone)]
pub struct RuntimeParts {
    pub agent_registry: AgentRegistry,
    pub plugin_registry: Arc<PluginRegistry>,
    pub tool_registry: Arc<ToolRegistry>,
    pub memory: Option<Arc<dyn Memory>>,
    pub session_store: Option<Arc<dyn SessionStore>>,
}

impl Default for RuntimeParts {
    fn default() -> Self {
        Self {
            agent_registry: AgentRegistry::default(),
            plugin_registry: Arc::new(PluginRegistry::default()),
            tool_registry: Arc::new(ToolRegistry::default()),
            memory: None,
            session_store: None,
        }
    }
}

#[derive(Clone)]
pub struct Runtime {
    config: RuntimeConfig,
    router: Arc<ModelRouter>,
    agent_registry: Arc<AgentRegistry>,
    plugin_registry: Arc<PluginRegistry>,
    tool_registry: Arc<ToolRegistry>,
    memory: Option<Arc<dyn Memory>>,
    session_store: Option<Arc<dyn SessionStore>>,
    event_bus: Arc<EventBus>,
    handles: Arc<Mutex<handle::AgentHandleRegistry>>,
}

impl Runtime {
    pub fn new(config: RuntimeConfig, parts: RuntimeParts) -> Self {
        let router = if config.credentials.is_empty() {
            ModelRouter::new()
        } else {
            ModelRouter::with_credentials(config.credentials.clone())
        };
        Self {
            event_bus: Arc::new(EventBus::new(config.event_capacity)),
            config,
            router: Arc::new(router),
            agent_registry: Arc::new(parts.agent_registry),
            plugin_registry: parts.plugin_registry,
            tool_registry: parts.tool_registry,
            memory: parts.memory,
            session_store: parts.session_store,
            handles: Arc::new(Mutex::new(handle::AgentHandleRegistry::default())),
        }
    }

    pub fn builder() -> RuntimeBuilder {
        RuntimeBuilder::default()
    }

    pub fn from_lattice_dir(
        root: &Path,
        global_agents_dir: Option<&Path>,
    ) -> Result<Self, RuntimeError> {
        let dir = match global_agents_dir {
            Some(global) => LatticeDir::discover_with_global(root, global),
            None => LatticeDir::discover(root),
        }
        .map_err(|e| RuntimeError::Config(e.to_string()))?;

        Ok(Self::builder()
            .agent_registry(dir.registry)
            .orchestrator_config(&dir.orchestrator_config)
            .build())
    }

    pub fn resolve(&self, model: &str) -> Result<ResolvedModel, RuntimeError> {
        self.resolve_with_provider(model, None)
    }

    pub fn resolve_with_provider(
        &self,
        model: &str,
        provider_override: Option<&str>,
    ) -> Result<ResolvedModel, RuntimeError> {
        Ok(self.router.resolve(model, provider_override)?)
    }

    pub fn inspect_model(&self, model: &str) -> Result<ResolvedModel, RuntimeError> {
        Ok(self.router.inspect_model(model)?)
    }

    pub fn list_models(&self) -> Vec<String> {
        self.router.list_models()
    }

    pub fn list_authenticated_models(&self) -> Vec<String> {
        self.router.list_authenticated_models()
    }

    pub fn register_model(&mut self, entry: ModelCatalogEntry) -> Result<(), RuntimeError> {
        match Arc::get_mut(&mut self.router) {
            Some(router) => {
                router.register_model(entry);
                Ok(())
            }
            None => Err(RuntimeError::Config(
                "custom models must be registered before cloning runtime handles".into(),
            )),
        }
    }

    pub async fn chat_complete(
        &self,
        model: &str,
        messages: &[Message],
        tools: &[ToolDefinition],
    ) -> Result<lattice_core::provider::ChatResponse, RuntimeError> {
        let resolved = self.resolve(model)?;
        Ok(lattice_core::chat_complete(&resolved, messages, tools).await?)
    }

    pub async fn stream_chat(
        &self,
        model: &str,
        messages: &[Message],
        tools: &[ToolDefinition],
    ) -> Result<Pin<Box<dyn Stream<Item = StreamEvent> + Send>>, RuntimeError> {
        let resolved = self.resolve(model)?;
        Ok(lattice_core::chat(&resolved, messages, tools).await?)
    }

    pub fn assemble_agent(
        &self,
        profile_name: &str,
    ) -> Result<lattice_orchestrator::AgentRunner, RuntimeError> {
        let profile = self
            .agent_registry
            .get(profile_name)
            .ok_or_else(|| RuntimeError::Config(format!("agent '{profile_name}' not found")))?
            .clone();
        let resolved = self.resolve(&profile.agent.model)?;

        // Build built-in factories (all 4 ProviderKind variants)
        let factories: Vec<Box<dyn lattice_prompt::ProviderFactory>> = vec![
            Box::new(lattice_prompt::provider_factory::ProjectInstructionsFactory),
            Box::new(
                lattice_agent::prompt::memory_provider::MemoryFactory::new()
                    .with_memory(self.memory.clone()),
            ),
            Box::new(lattice_orchestrator::ToolSchemaFactory::new(Arc::clone(
                &self.tool_registry,
            ))),
        ];
        // EventsFactory is feature-gated on blob-store
        #[cfg(feature = "blob-store")]
        let factories = {
            let mut f = factories;
            f.push(Box::new(lattice_agent::events_provider::EventsFactory));
            f
        };

        let assembled = lattice_orchestrator::assembly::assemble_agent(
            &profile,
            resolved,
            self.memory.clone(),
            Some(Arc::clone(&self.tool_registry)),
            &factories,
        )
        .map_err(|e| RuntimeError::Assembly(e.to_string()))?;
        Ok(assembled.runner)
    }

    pub async fn run_agent(
        &self,
        profile_name: &str,
        input: &str,
        max_turns: Option<u32>,
    ) -> Result<serde_json::Value, RuntimeError> {
        let mut runner = self.assemble_agent(profile_name)?;
        let turns = max_turns.unwrap_or_else(|| runner.profile.handoff.max_turns.unwrap_or(10));
        runner
            .run(input, turns)
            .await
            .map_err(|e| RuntimeError::Assembly(e.to_string()))
    }

    pub async fn run_agent_events(
        &self,
        profile_name: &str,
        input: &str,
        max_turns: Option<u32>,
    ) -> Result<Vec<LoopEvent>, RuntimeError> {
        let mut runner = self.assemble_agent(profile_name)?;
        let turns = max_turns.unwrap_or_else(|| runner.profile.handoff.max_turns.unwrap_or(10));
        Ok(runner.agent_mut().run(input, turns).await)
    }

    // ------------------------------------------------------------------
    // Handle lifecycle (Plan 16-02)
    // ------------------------------------------------------------------

    /// Get or create an agent handle keyed by session/workspace/profile/model/security.
    ///
    /// If a handle with an equivalent key already exists, its `last_used_at`
    /// is updated and the existing id is returned. Otherwise a new internal
    /// `Agent` is built, a fresh handle is allocated, and its id is returned.
    pub fn get_or_create_agent_handle(
        &self,
        spec: lattice_orchestrator::AgentTurnSpec,
        security: RuntimeSecurity,
    ) -> Result<AgentHandleId, RuntimeError> {
        self.get_or_create_agent_handle_for_session(spec, security, None)
    }

    /// Get or create an agent handle for a specific durable session.
    ///
    /// Session identity is part of the handle key so live conversation state,
    /// task state, approvals, and recent completions are never shared across
    /// distinct sessions that happen to use the same profile/model/workspace.
    pub fn get_or_create_agent_handle_for_session(
        &self,
        spec: lattice_orchestrator::AgentTurnSpec,
        security: RuntimeSecurity,
        session_id: Option<String>,
    ) -> Result<AgentHandleId, RuntimeError> {
        self.get_or_create_agent_handle_for_session_seeded(
            spec,
            security,
            session_id,
            Vec::new(),
            None,
        )
    }

    /// Get or create an agent handle for a durable session, seeding a newly
    /// created handle with persisted conversation and task-state metadata.
    ///
    /// Seeds are intentionally applied only when the handle does not already
    /// exist for the requested key. Reusing a live handle preserves its current
    /// in-memory state and ignores stale caller-provided seeds.
    pub fn get_or_create_agent_handle_for_session_seeded(
        &self,
        spec: lattice_orchestrator::AgentTurnSpec,
        security: RuntimeSecurity,
        session_id: Option<String>,
        seed_messages: Vec<Message>,
        task_state_seed: Option<lattice_agent::TaskState>,
    ) -> Result<AgentHandleId, RuntimeError> {
        let workspace = spec
            .cwd
            .as_deref()
            .or(spec.project_root.as_deref())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));

        let security_policy = runtime_security_policy_key(&security);

        let key = handle::AgentHandleKey::new(workspace, spec.model.clone(), security_policy)
            .with_session_id(session_id)
            .with_profile_name(Some(spec.profile_name.clone()))
            .with_provider_override(spec.provider_override.clone());

        let mut registry = self.handles.lock().unwrap();

        // Reuse existing handle
        if let Some(existing_id) = registry.by_key.get(&key).cloned() {
            if let Some(state) = registry.by_id.get_mut(&existing_id) {
                if state.active_turn || state.agent.is_some() {
                    state.last_used_at = Instant::now();
                    return Ok(existing_id);
                }
            }
            registry.by_key.remove(&key);
            registry.by_id.remove(&existing_id);
        }

        // Build internal agent
        let resolved =
            self.resolve_with_provider(&spec.model, spec.provider_override.as_deref())?;
        let mut agent = self.build_internal_agent(&spec, &resolved, &security)?;
        if !seed_messages.is_empty() {
            agent.seed_messages(seed_messages);
        }
        if let Some(task_state) = task_state_seed {
            *agent.task_state_mut() = task_state;
        }
        let context_length = resolved.context_length;

        let id = AgentHandleId::new();
        let state = handle::HandleState {
            id: id.clone(),
            key,
            spec,
            revision: 0,
            agent: Some(agent),
            model: resolved,
            context_length,
            tool_definitions: Vec::new(),
            security,
            active_turn: false,
            active_turn_id: None,
            active_task_abort: None,
            active_assistant_text: String::new(),
            cancellation_requested: false,
            plugin_overlay: None,
            recent_completions: VecDeque::new(),
            last_used_at: Instant::now(),
        };

        registry.by_key.insert(state.key.clone(), id.clone());
        registry.by_id.insert(id.clone(), state);

        Ok(id)
    }

    /// Return whether an existing handle still matches the requested build key.
    ///
    /// Frontends use this before reusing an opaque handle id supplied from local
    /// UI state. The check rejects stale handles and handles created for a
    /// different profile/session/model/security key.
    pub fn agent_handle_matches(
        &self,
        handle_id: &AgentHandleId,
        spec: &lattice_orchestrator::AgentTurnSpec,
        security: &RuntimeSecurity,
        session_id: Option<&str>,
    ) -> Result<bool, RuntimeError> {
        let workspace = spec
            .cwd
            .as_deref()
            .or(spec.project_root.as_deref())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let expected_key = handle::AgentHandleKey::new(
            workspace,
            spec.model.clone(),
            runtime_security_policy_key(security),
        )
        .with_session_id(session_id.map(str::to_string))
        .with_profile_name(Some(spec.profile_name.clone()))
        .with_provider_override(spec.provider_override.clone());

        let registry = self.handles.lock().unwrap();
        let state = registry
            .by_id
            .get(handle_id)
            .ok_or_else(|| RuntimeError::HandleNotFound(handle_id.to_string()))?;
        Ok((state.active_turn || state.agent.is_some()) && state.key == expected_key)
    }

    /// Rebuild a handle's internal `Agent` while preserving messages,
    /// `TaskState`, and security approvals. The handle id stays the same.
    pub fn rebuild_agent_handle(
        &self,
        handle_id: &AgentHandleId,
        spec: lattice_orchestrator::AgentTurnSpec,
        security: RuntimeSecurity,
    ) -> Result<AgentHandleSnapshot, RuntimeError> {
        let workspace = spec
            .cwd
            .as_deref()
            .or(spec.project_root.as_deref())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let new_key = handle::AgentHandleKey::new(
            workspace,
            spec.model.clone(),
            runtime_security_policy_key(&security),
        )
        .with_profile_name(Some(spec.profile_name.clone()))
        .with_provider_override(spec.provider_override.clone());

        let mut registry = self.handles.lock().unwrap();
        let (old_key, final_key, snapshot) = {
            let state = registry
                .by_id
                .get_mut(handle_id)
                .ok_or_else(|| RuntimeError::HandleNotFound(handle_id.to_string()))?;
            if state.active_turn {
                return Err(RuntimeError::HandleBusy(handle_id.to_string()));
            }
            let new_key = new_key.with_session_id(state.key.session_id.clone());
            let security_policy_changed = state.key.security_policy != new_key.security_policy;
            let old_key = state.key.clone();

            // Snapshot carry-forward state
            let messages = state
                .agent
                .as_ref()
                .map(|a| a.messages().to_vec())
                .unwrap_or_default();
            let task_state_snapshot = state
                .agent
                .as_ref()
                .map(|a| a.task_state().clone())
                .unwrap_or_default();

            // Build new internal agent
            let resolved =
                self.resolve_with_provider(&spec.model, spec.provider_override.as_deref())?;
            let mut new_agent = self.build_internal_agent(&spec, &resolved, &security)?;

            // Restore carry-forward state
            new_agent.seed_messages(messages);
            *new_agent.task_state_mut() = task_state_snapshot;

            let context_length = resolved.context_length;

            state.agent = Some(new_agent);
            state.model = resolved;
            state.context_length = context_length;
            state.tool_definitions = Vec::new();
            state.security = security;
            state.spec = spec.clone();
            state.key = new_key.clone();
            if security_policy_changed {
                state.plugin_overlay = None;
            }
            state.revision += 1;
            state.last_used_at = Instant::now();

            let plugin_overlay_name = state
                .plugin_overlay
                .as_ref()
                .map(|overlay| overlay.name.clone());
            let approved_rule_count = state
                .security
                .approved_rules
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .len();
            let snapshot = AgentHandleSnapshot {
                id: state.id.clone(),
                revision: state.revision,
                model: state.model.canonical_id.clone(),
                provider: state.model.provider.clone(),
                message_count: state
                    .agent
                    .as_ref()
                    .map(|a| a.messages().len())
                    .unwrap_or(0),
                task_state_empty: state
                    .agent
                    .as_ref()
                    .map(|a| {
                        let ts = a.task_state();
                        ts.todos.is_empty() && ts.verifications.is_empty()
                    })
                    .unwrap_or(true),
                active_turn: state.active_turn,
                recent_completion_count: state.recent_completions.len(),
                plugin_overlay_name,
                approved_rule_count,
            };
            (old_key, state.key.clone(), snapshot)
        };

        if old_key != final_key {
            registry.by_key.remove(&old_key);
            registry.by_key.insert(final_key, handle_id.clone());
        }

        Ok(snapshot)
    }

    /// Dispose a handle and remove all associated state.
    pub fn dispose_agent_handle(&self, handle_id: &AgentHandleId) -> Result<(), RuntimeError> {
        let mut registry = self.handles.lock().unwrap();
        let state = registry
            .by_id
            .remove(handle_id)
            .ok_or_else(|| RuntimeError::HandleNotFound(handle_id.to_string()))?;
        registry.by_key.remove(&state.key);
        Ok(())
    }

    pub fn set_agent_plugin_overlay(
        &self,
        handle_id: &AgentHandleId,
        overlay: Option<RuntimePluginOverlay>,
    ) -> Result<(), RuntimeError> {
        let mut registry = self.handles.lock().unwrap();
        let state = registry
            .by_id
            .get_mut(handle_id)
            .ok_or_else(|| RuntimeError::HandleNotFound(handle_id.to_string()))?;
        if state.active_turn {
            return Err(RuntimeError::HandleBusy(handle_id.to_string()));
        }
        self.apply_plugin_overlay_to_agent(state, overlay.as_ref())?;
        state.plugin_overlay = overlay;
        state.revision += 1;
        state.last_used_at = Instant::now();
        Ok(())
    }

    fn apply_plugin_overlay_to_agent(
        &self,
        state: &mut handle::HandleState,
        overlay: Option<&RuntimePluginOverlay>,
    ) -> Result<(), RuntimeError> {
        let agent = state.agent.as_mut().ok_or_else(|| {
            RuntimeError::Config("handle has no internal agent; rebuild required".into())
        })?;
        let project_root = state.key.workspace.to_string_lossy().to_string();
        match overlay {
            Some(overlay) => {
                agent.set_system_prompt(&format!(
                    "{}\n\nPlugin mode:\n{}",
                    crate::preset::coding_system_prompt(&state.key.workspace, &state.key.workspace),
                    overlay.system_prompt_delta
                ));
                agent.set_output_contract_delta(overlay.output_contract_delta.as_ref().map(
                    |contract| {
                        prompt::SystemPromptDelta::contract(contract.clone(), HashMap::new())
                    },
                ));
                agent.set_tools(overlay.tools.clone());
                let registry_access: Arc<dyn lattice_tools::RegistryToolAccess> =
                    Arc::clone(&self.tool_registry) as Arc<dyn lattice_tools::RegistryToolAccess>;
                let executor =
                    DefaultToolExecutor::new_with_registry(&project_root, Some(registry_access))
                        .map_err(RuntimeError::Config)?
                        .with_sandbox(state.security.sandbox.clone())
                        .with_authorized_tools(overlay.authorized_tool_names.clone());
                agent.set_tool_executor(Some(Box::new(executor)));
                state.tool_definitions = overlay.tools.clone();
            }
            None => {
                let capabilities = resolve_capabilities(&CapabilitySelection {
                    bundles: vec![
                        ToolBundle::Workspace,
                        ToolBundle::Filesystem,
                        ToolBundle::Editing,
                        ToolBundle::Shell,
                        ToolBundle::Planning,
                        ToolBundle::Web,
                    ],
                    enabled: Vec::new(),
                });
                agent.set_system_prompt(&crate::preset::coding_system_prompt(
                    &state.key.workspace,
                    &state.key.workspace,
                ));
                agent.set_output_contract_delta(None);
                agent.set_tools(capabilities.definitions.clone());
                let registry_access: Arc<dyn lattice_tools::RegistryToolAccess> =
                    Arc::clone(&self.tool_registry) as Arc<dyn lattice_tools::RegistryToolAccess>;
                let executor =
                    DefaultToolExecutor::new_with_registry(&project_root, Some(registry_access))
                        .map_err(RuntimeError::Config)?
                        .with_sandbox(state.security.sandbox.clone())
                        .with_authorized_tools(capabilities.tool_names.clone());
                agent.set_tool_executor(Some(Box::new(executor)));
                state.tool_definitions = capabilities.definitions;
            }
        }
        Ok(())
    }

    pub fn approve_handle_pending_proposal(
        &self,
        handle_id: &AgentHandleId,
        selector: &str,
    ) -> Result<String, RuntimeError> {
        let mut registry = self.handles.lock().unwrap();
        let state = registry
            .by_id
            .get_mut(handle_id)
            .ok_or_else(|| RuntimeError::HandleNotFound(handle_id.to_string()))?;
        if state.active_turn {
            return Err(RuntimeError::HandleBusy(handle_id.to_string()));
        }
        let security = state.security.clone();
        let agent = state.agent.as_mut().ok_or_else(|| {
            RuntimeError::Config("handle has no internal agent; rebuild required".into())
        })?;
        let mut task_state = agent.task_state_mut();
        approve_task_state_proposal(&security, &mut task_state, selector)
    }

    /// Reap idle handles that have been inactive for at least `idle_for`.
    /// Returns the number of handles reaped. Active handles are never reaped.
    pub fn reap_idle_agent_handles(&self, idle_for: Duration) -> usize {
        let mut registry = self.handles.lock().unwrap();
        let now = Instant::now();
        let to_remove: Vec<AgentHandleId> = registry
            .by_id
            .iter()
            .filter(|(_, s)| !s.active_turn && now.duration_since(s.last_used_at) >= idle_for)
            .map(|(id, _)| id.clone())
            .collect();

        let count = to_remove.len();
        for id in &to_remove {
            if let Some(state) = registry.by_id.remove(id) {
                registry.by_key.remove(&state.key);
            }
        }
        count
    }

    /// Return a public snapshot of a handle's metadata.
    pub fn agent_handle_snapshot(
        &self,
        handle_id: &AgentHandleId,
    ) -> Result<AgentHandleSnapshot, RuntimeError> {
        let registry = self.handles.lock().unwrap();
        let state = registry
            .by_id
            .get(handle_id)
            .ok_or_else(|| RuntimeError::HandleNotFound(handle_id.to_string()))?;
        let plugin_overlay_name = state
            .plugin_overlay
            .as_ref()
            .map(|overlay| overlay.name.clone());
        let approved_rule_count = state
            .security
            .approved_rules
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len();
        Ok(AgentHandleSnapshot {
            id: state.id.clone(),
            revision: state.revision,
            model: state.model.canonical_id.clone(),
            provider: state.model.provider.clone(),
            message_count: state
                .agent
                .as_ref()
                .map(|a| a.messages().len())
                .unwrap_or(0),
            task_state_empty: state
                .agent
                .as_ref()
                .map(|a| {
                    let ts = a.task_state();
                    ts.todos.is_empty() && ts.verifications.is_empty()
                })
                .unwrap_or(true),
            active_turn: state.active_turn,
            recent_completion_count: state.recent_completions.len(),
            plugin_overlay_name,
            approved_rule_count,
        })
    }

    /// Build an internal `Agent` from a lowered spec. This is a private helper
    /// — callers use `get_or_create_agent_handle` instead.
    fn build_internal_agent(
        &self,
        spec: &lattice_orchestrator::AgentTurnSpec,
        resolved: &ResolvedModel,
        security: &RuntimeSecurity,
    ) -> Result<Agent, RuntimeError> {
        let project_root = spec.project_root.clone().unwrap_or_else(|| ".".to_string());
        let cwd = spec.cwd.clone().unwrap_or_else(|| project_root.clone());

        #[allow(unused_mut)]
        let mut capabilities = spec.capabilities.clone();

        #[allow(deprecated)]
        let profile = lattice_orchestrator::AgentProfile {
            agent: lattice_orchestrator::AgentConfig {
                name: spec.profile_name.clone(),
                model: spec.model.clone(),
                description: String::new(),
                skippable: false,
                tags: Vec::new(),
            },
            system: lattice_orchestrator::SystemConfig {
                prompt: spec.system_prompt.clone().unwrap_or_default(),
                file: None,
            },
            capabilities,
            tools: spec.tools.clone(),
            behavior: spec.behavior.clone(),
            handoff: spec.handoff.clone(),
            orchestrator: lattice_orchestrator::OrchestratorConfigProfile::default(),
            memory: spec.memory.clone(),
            plugins: None,
            prompt_spec: spec.prompt_spec.clone(),
        };

        let factories: Vec<Box<dyn lattice_prompt::ProviderFactory>> = vec![
            Box::new(lattice_prompt::provider_factory::ProjectInstructionsFactory),
            Box::new(
                lattice_agent::prompt::memory_provider::MemoryFactory::new()
                    .with_memory(self.memory.clone()),
            ),
            Box::new(lattice_orchestrator::ToolSchemaFactory::new(Arc::clone(
                &self.tool_registry,
            ))),
        ];
        #[cfg(feature = "blob-store")]
        let factories = {
            let mut f = factories;
            f.push(Box::new(lattice_agent::events_provider::EventsFactory));
            f
        };

        let assembled = lattice_orchestrator::assembly::assemble_agent_with_options(
            &profile,
            resolved.clone(),
            self.memory.clone(),
            Some(Arc::clone(&self.tool_registry)),
            &factories,
            lattice_orchestrator::AgentAssemblyOptions {
                project_root: project_root.clone(),
                sandbox: Some(security.sandbox.clone()),
                allow_empty_capabilities: true,
            },
        )
        .map_err(|e| RuntimeError::Assembly(e.to_string()))?;

        let mut agent = assembled.runner.agent;
        if !spec
            .prompt_spec
            .as_ref()
            .map(|s| s.has_provider_kind(lattice_prompt::spec::ProviderKind::ProjectInstructions))
            .unwrap_or(false)
        {
            agent = agent.with_provider(lattice_prompt::ProjectInstructionsProvider::new(
                PathBuf::from(&project_root),
                PathBuf::from(&cwd),
            ));
        }
        // WorkspaceContextProvider is always added for project-aware agents.
        agent = agent.with_provider(lattice_prompt::WorkspaceContextProvider::new(
            PathBuf::from(&project_root),
            PathBuf::from(&cwd),
        ));
        agent = agent
            .with_task_state_provider()
            .with_thinking_effort(spec.thinking_effort.clone());

        Ok(agent)
    }

    pub async fn run_pipeline(&self, start_agent: &str, input: &str) -> PipelineRun {
        let mut pipeline = self.pipeline(&self.config.name);
        pipeline.run(start_agent, input).await
    }

    pub fn micro_agent(
        &self,
        profile_name: &str,
        bus: Arc<dyn Orchestrator>,
    ) -> Result<MicroAgent, RuntimeError> {
        let profile = self
            .agent_registry
            .get(profile_name)
            .ok_or_else(|| RuntimeError::Config(format!("agent '{profile_name}' not found")))?
            .clone();

        let mut agent = MicroAgent::new(profile, bus, self.memory.clone(), None, None)
            .with_model_router(Arc::clone(&self.router))
            .with_tool_registry(Arc::clone(&self.tool_registry));

        if !self.config.credentials.is_empty() {
            agent = agent.with_credentials(self.config.credentials.clone());
        }

        Ok(agent)
    }

    pub fn dry_run_pipeline(&self, start_agent: &str) -> lattice_orchestrator::DryRunReport {
        self.pipeline(&self.config.name).dry_run(start_agent)
    }

    pub fn pipeline(&self, name: &str) -> Pipeline {
        let mut pipeline = Pipeline::new(
            name,
            Arc::clone(&self.agent_registry),
            self.memory.clone(),
            Some(Arc::clone(&self.event_bus)),
        )
        .with_model_router(Arc::clone(&self.router))
        .with_plugin_registry(Arc::clone(&self.plugin_registry))
        .with_tool_registry(Arc::clone(&self.tool_registry))
        .with_max_pipeline_iterations(self.config.max_pipeline_iterations)
        .with_turn_executor(Arc::new(RuntimeAgentTurnExecutor::new(self.clone())));

        if !self.config.credentials.is_empty() {
            pipeline = pipeline.with_credentials(self.config.credentials.clone());
        }

        pipeline
    }

    pub fn router(&self) -> Arc<ModelRouter> {
        Arc::clone(&self.router)
    }

    pub fn agent_registry(&self) -> Arc<AgentRegistry> {
        Arc::clone(&self.agent_registry)
    }

    pub fn plugin_registry(&self) -> Arc<PluginRegistry> {
        Arc::clone(&self.plugin_registry)
    }

    pub fn tool_registry(&self) -> Arc<ToolRegistry> {
        Arc::clone(&self.tool_registry)
    }

    pub fn session_store(&self) -> Option<Arc<dyn SessionStore>> {
        self.session_store.as_ref().map(Arc::clone)
    }

    pub fn event_bus(&self) -> Arc<EventBus> {
        Arc::clone(&self.event_bus)
    }

    /// Build an agent from a [`AgentSpec`] (CLI-facing convenience).
    pub fn build_agent(&self, spec: AgentSpec) -> Result<BuiltAgent, RuntimeError> {
        let resolved =
            self.resolve_with_provider(&spec.model, spec.provider_override.as_deref())?;
        let mut agent = Agent::new(resolved.clone());
        // Detect coding-capable agent via explicit bundle membership.
        let has_all_coding_bundles = spec.capabilities.bundles.contains(&ToolBundle::Workspace)
            && spec.capabilities.bundles.contains(&ToolBundle::Filesystem)
            && spec.capabilities.bundles.contains(&ToolBundle::Editing)
            && spec.capabilities.bundles.contains(&ToolBundle::Shell)
            && spec.capabilities.bundles.contains(&ToolBundle::Planning)
            && spec.capabilities.bundles.contains(&ToolBundle::Web);

        let tool_definitions;
        if has_all_coding_bundles {
            crate::preset::apply_coding_preset(&mut agent, &spec, &resolved)?;
            tool_definitions = resolve_capabilities(&CapabilitySelection {
                bundles: vec![
                    ToolBundle::Workspace,
                    ToolBundle::Filesystem,
                    ToolBundle::Editing,
                    ToolBundle::Shell,
                    ToolBundle::Planning,
                    ToolBundle::Web,
                ],
                enabled: Vec::new(),
            })
            .definitions;
        } else {
            if spec.capabilities.bundles.is_empty() && spec.capabilities.enabled.is_empty() {
                return Err(RuntimeError::Config(
                    "AgentSpec must declare capabilities".into(),
                ));
            }
            let capabilities = resolve_capabilities(&spec.capabilities);
            tool_definitions = capabilities.definitions.clone();
            agent = agent.with_tools(tool_definitions.clone());

            let project_root = spec.project_root.clone().unwrap_or_else(|| ".".to_string());
            agent = agent.with_provider(prompt::ProjectInstructionsProvider::from_root(
                project_root.clone(),
            ));

            if let Some(security) = spec.security {
                let executor = DefaultToolExecutor::new(&project_root)
                    .map_err(RuntimeError::Config)?
                    .with_sandbox(security.sandbox)
                    .with_authorized_tools(capabilities.tool_names.clone());
                agent = agent.with_tool_executor(Box::new(executor));
            }
        }

        if let Some(prompt) = spec.system_prompt {
            agent.set_system_prompt(&prompt);
        }

        if !spec.messages.is_empty() {
            agent.seed_messages(spec.messages);
        }

        if let Some(effort) = spec.thinking_effort {
            agent = agent.with_thinking_effort(Some(effort));
        }

        let context_length = if resolved.context_length > 0 {
            resolved.context_length
        } else {
            128000
        };

        Ok(BuiltAgent {
            agent,
            model: resolved,
            context_length,
            tool_definitions,
        })
    }

    /// Compact a durable session into a new child session without calling the LLM.
    ///
    /// The first implementation intentionally uses the agent-side structured
    /// summary builder. That gives manual `/compact` a stable graph boundary
    /// before we swap the summary body to a model-generated one.
    pub fn compact_session(
        &self,
        mut session: Session,
        model: &str,
        provider_override: Option<&str>,
        recent_non_system: usize,
    ) -> Result<Session, RuntimeError> {
        let resolved = self.resolve_with_provider(model, provider_override)?;
        let parent_session_id = session.id.clone();
        let original_messages = session.messages.clone();
        let mut messages = messages_for_session(&session);
        let manager = ContextManager::new(&resolved);
        let report = manager.compact(&mut messages, recent_non_system, false);
        let retained_suffix = retained_session_suffix(&original_messages, recent_non_system);

        let boundary = CompactBoundary {
            mode: "manual".to_string(),
            parent_session_id: Some(parent_session_id),
            original_message_count: report.original_message_count,
            summarized_message_count: report.summarized_message_count,
            retained_message_count: report.retained_message_count,
            active_tokens_before: report.active_tokens_before,
            active_tokens_after: report.active_tokens_after,
            compacted_at: chrono::Local::now().to_rfc3339(),
        };

        session.parent_id = Some(session.id.clone());
        session.id = uuid::Uuid::new_v4().simple().to_string();
        session.model = resolved.canonical_id.clone();
        session.provider = resolved.provider.clone();
        session.compact_with_summary(report.summary_message.content, retained_suffix, boundary);
        session.normalize();
        Ok(session)
    }

    /// Compile prompt through the engine and return a dry-run report without calling the LLM.
    pub async fn dry_run_prompt(
        &self,
        model_name: &str,
        system_prompt: Option<&str>,
        prompt_text: &str,
        history: Vec<Message>,
    ) -> Result<DryRunReport, RuntimeError> {
        let resolved = self.resolve(model_name)?;

        let mut messages = Vec::new();
        if let Some(sp) = system_prompt {
            messages.push(Message {
                role: lattice_core::types::Role::System,
                content: sp.to_string(),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            });
        }
        for msg in history {
            messages.push(msg);
        }
        messages.push(Message {
            role: lattice_core::types::Role::User,
            content: prompt_text.to_string(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        });

        let total_tokens: usize = messages.iter().map(|m| m.content.len()).sum();

        Ok(DryRunReport {
            sections: vec![],
            budgets: vec![],
            messages,
            rendered: DryRunRendered { total_tokens },
            resolved,
        })
    }
}

pub fn messages_for_session(session: &Session) -> Vec<Message> {
    session
        .messages
        .iter()
        .map(|msg| {
            let role = match msg.role.to_ascii_lowercase().as_str() {
                "assistant" => Role::Assistant,
                "system" => Role::System,
                "tool" => Role::Tool,
                _ => Role::User,
            };
            Message {
                role,
                content: msg.content.clone(),
                reasoning_content: msg.reasoning_content.clone(),
                tool_calls: msg.tool_calls.as_ref().map(|calls| {
                    calls
                        .iter()
                        .map(|call| ToolCall {
                            id: call.id.clone(),
                            function: FunctionCall {
                                name: call.name.clone(),
                                arguments: call.arguments.clone(),
                            },
                        })
                        .collect()
                }),
                tool_call_id: msg.tool_call_id.clone(),
                name: msg.name.clone(),
            }
        })
        .collect()
}

fn retained_session_suffix(
    messages: &[SessionMessage],
    recent_non_system: usize,
) -> Vec<SessionMessage> {
    let non_system_indexes = messages
        .iter()
        .enumerate()
        .filter(|(_, message)| !message.role.eq_ignore_ascii_case("system"))
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let keep_count = recent_non_system.min(non_system_indexes.len());
    let keep_start = non_system_indexes.len().saturating_sub(keep_count);
    let first_keep_index = non_system_indexes
        .get(keep_start)
        .copied()
        .unwrap_or(messages.len());

    messages
        .iter()
        .enumerate()
        .filter(|(index, message)| {
            *index >= first_keep_index
                && !message.role.eq_ignore_ascii_case("system")
                && message.compact.is_none()
        })
        .map(|(_, message)| {
            let mut message = message.clone();
            message.compact = None;
            message
        })
        .collect()
}

#[derive(Clone)]
pub struct DryRunReport {
    pub sections: Vec<DryRunSection>,
    pub budgets: Vec<prompt::TokenBudget>,
    pub messages: Vec<Message>,
    pub rendered: DryRunRendered,
    pub resolved: ResolvedModel,
}

#[derive(Clone)]
pub struct DryRunSection {
    pub layer: prompt::Layer,
    pub tokens: usize,
    pub priority: u32,
}

#[derive(Clone)]
pub struct DryRunRendered {
    pub total_tokens: usize,
}

#[derive(Default)]
pub struct RuntimeBuilder {
    config: RuntimeConfig,
    parts: RuntimeParts,
    _plugin_dir: Option<PathBuf>,
    _plugin_watch_dir: Option<PathBuf>,
}

impl RuntimeBuilder {
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.config.name = name.into();
        self
    }

    pub fn event_capacity(mut self, capacity: usize) -> Self {
        self.config.event_capacity = capacity;
        self
    }

    pub fn max_pipeline_iterations(mut self, max: usize) -> Self {
        self.config.max_pipeline_iterations = max;
        self
    }

    pub fn credentials(mut self, credentials: HashMap<String, String>) -> Self {
        self.config.credentials = credentials;
        self
    }

    pub fn orchestrator_config(mut self, config: &lattice_orchestrator::OrchestratorToml) -> Self {
        self.config.event_capacity = config.subscriber_buffer;
        self
    }

    pub fn agent_registry(mut self, registry: AgentRegistry) -> Self {
        self.parts.agent_registry = registry;
        self
    }

    pub fn plugin_registry(mut self, registry: Arc<PluginRegistry>) -> Self {
        self.parts.plugin_registry = registry;
        self
    }

    pub fn tool_registry(mut self, registry: Arc<ToolRegistry>) -> Self {
        self.parts.tool_registry = registry;
        self
    }

    pub fn memory(mut self, memory: Arc<dyn Memory>) -> Self {
        self.parts.memory = Some(memory);
        self
    }

    pub fn session_store(mut self, store: Arc<dyn SessionStore>) -> Self {
        self.parts.session_store = Some(store);
        self
    }

    /// Load plugins from a directory into the plugin registry.
    pub fn load_plugin_dir(mut self, dir: PathBuf) -> Result<Self, RuntimeError> {
        if !dir.exists() {
            return Err(RuntimeError::Config(format!(
                "plugin directory '{}' does not exist",
                dir.display()
            )));
        }
        let bundles =
            lattice_plugin::watcher::load_registry_bundles(&dir, true).map_err(|err| {
                RuntimeError::Config(format!("failed to load plugin directory: {err}"))
            })?;
        for bundle in bundles {
            self.parts
                .plugin_registry
                .replace(bundle)
                .map_err(|err| RuntimeError::Config(format!("failed to register plugin: {err}")))?;
        }
        self._plugin_dir = Some(dir);
        Ok(self)
    }

    /// Watch a plugin directory for changes (requires load_plugin_dir first).
    pub fn watch_plugin_dir(mut self, dir: PathBuf) -> Result<Self, RuntimeError> {
        if !dir.exists() {
            return Err(RuntimeError::Config(format!(
                "plugin directory '{}' does not exist",
                dir.display()
            )));
        }
        self._plugin_watch_dir = Some(dir);
        Ok(self)
    }

    pub fn build(self) -> Runtime {
        Runtime::new(self.config, self.parts)
    }
}

pub struct RuntimeHandle {
    runtime: Arc<Runtime>,
    tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

impl RuntimeHandle {
    pub fn new(runtime: Runtime) -> Self {
        Self {
            runtime: Arc::new(runtime),
            tasks: Mutex::new(Vec::new()),
        }
    }

    pub fn runtime(&self) -> Arc<Runtime> {
        Arc::clone(&self.runtime)
    }

    pub fn track(&self, task: tokio::task::JoinHandle<()>) {
        self.tasks
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .push(task);
    }

    pub fn shutdown(&self) {
        let mut tasks = self.tasks.lock().unwrap_or_else(|err| err.into_inner());
        for task in tasks.drain(..) {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_agent::memory::InMemoryMemory;
    use lattice_core::catalog::CatalogProviderEntry;
    use lattice_orchestrator::InMemoryOrchestrator;

    #[test]
    fn runtime_owns_shared_registries_and_events() {
        let runtime = Runtime::builder()
            .memory(Arc::new(InMemoryMemory::new()))
            .build();

        assert!(runtime.agent_registry().list().is_empty());
        assert!(runtime.plugin_registry().is_empty());
        assert!(runtime.tool_registry().is_empty());
        let _rx = runtime.event_bus().subscribe();
    }

    #[test]
    fn builder_load_plugin_dir_registers_manifest_plugins() {
        let root = std::env::temp_dir().join(format!(
            "lattice-runtime-plugin-dir-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        let plugin_dir = root.join("local");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("plugin.toml"),
            r#"
[plugin]
name = "LocalRuntimePlugin"
description = "local runtime manifest"

[output]
parse = "text"
"#,
        )
        .unwrap();

        let runtime = Runtime::builder()
            .load_plugin_dir(root.clone())
            .unwrap()
            .build();

        let registry = runtime.plugin_registry();
        let plugin = registry
            .get("LocalRuntimePlugin")
            .expect("local manifest plugin should be registered");
        assert_eq!(plugin.meta.description, "local runtime manifest");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn pipeline_uses_runtime_owned_parts() {
        let plugin_registry = Arc::new(PluginRegistry::new());
        let tool_registry = Arc::new(ToolRegistry::new());
        let runtime = Runtime::builder()
            .name("test-runtime")
            .plugin_registry(Arc::clone(&plugin_registry))
            .tool_registry(Arc::clone(&tool_registry))
            .build();

        let pipeline = runtime.pipeline("test-pipeline");

        assert_eq!(pipeline.name, "test-pipeline");
        assert!(pipeline.model_router.is_some());
        assert!(Arc::ptr_eq(
            pipeline.plugin_registry.as_ref().unwrap(),
            &plugin_registry
        ));
        assert!(Arc::ptr_eq(
            pipeline.tool_registry.as_ref().unwrap(),
            &tool_registry
        ));
    }

    #[test]
    #[allow(deprecated)]
    fn micro_agent_uses_runtime_owned_parts() {
        let profile = lattice_orchestrator::AgentProfile {
            agent: lattice_orchestrator::AgentConfig {
                name: "agent".into(),
                model: "sonnet".into(),
                description: String::new(),
                skippable: false,
                tags: vec![],
            },
            system: lattice_orchestrator::SystemConfig {
                prompt: "test".into(),
                file: None,
            },
            capabilities: lattice_orchestrator::CapabilitiesConfig {
                presets: vec!["minimal".into()],
                bundles: Vec::new(),
            },
            tools: lattice_orchestrator::ToolsConfig::default(),
            behavior: lattice_orchestrator::BehaviorConfig::default(),
            handoff: lattice_orchestrator::HandoffConfig::default(),
            orchestrator: lattice_orchestrator::OrchestratorConfigProfile::default(),
            memory: lattice_orchestrator::MemoryConfigProfile::default(),
            plugins: None,
            prompt_spec: None,
        };
        let registry = lattice_orchestrator::AgentRegistry::from_profiles([profile]);
        let runtime = Runtime::builder().agent_registry(registry).build();
        let bus = Arc::new(InMemoryOrchestrator::with_defaults());

        let agent = runtime.micro_agent("agent", bus).unwrap();

        assert!(agent.model_router.is_some());
        assert!(agent.tool_registry.is_some());
    }

    #[test]
    fn runtime_exposes_session_store_facade() {
        let dir = std::env::temp_dir().join(format!(
            "lattice-runtime-session-store-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let store = Arc::new(lattice_session::FileSessionStore::new(&dir));
        let runtime = Runtime::builder().session_store(store).build();

        let sessions = runtime.session_store().expect("session store should exist");
        let mut session = lattice_session::Session::new("model".into(), "provider".into());
        session.id = "runtime-session".into();
        session.push_message("user", "hello runtime".into());
        sessions.save(&session).unwrap();

        assert_eq!(
            sessions.current_session_id().unwrap().as_deref(),
            Some("runtime-session")
        );
        assert_eq!(
            sessions.current().unwrap().unwrap().title.as_deref(),
            Some("hello runtime")
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn compact_session_creates_child_with_boundary_metadata() {
        let mut runtime = Runtime::builder().build();
        runtime
            .register_model(ModelCatalogEntry {
                canonical_id: "compact-test-model".to_string(),
                context_length: 8192,
                providers: vec![CatalogProviderEntry {
                    provider_id: "ollama".to_string(),
                    api_model_id: "compact-test-model".to_string(),
                    priority: 1,
                    credential_keys: HashMap::new(),
                    base_url: Some("http://localhost:11434".to_string()),
                    api_protocol: lattice_core::catalog::ApiProtocol::OpenAiChat,
                    provider_specific: HashMap::new(),
                }],
                aliases: vec![],
            })
            .unwrap();
        let mut session =
            lattice_session::Session::new("compact-test-model".into(), "ollama".into());
        session.id = "compact-parent".into();
        for index in 0..6 {
            session.push_message("user", format!("request {index}"));
            session.push_message("assistant", format!("answer {index}"));
        }

        let compacted = runtime
            .compact_session(session, "compact-test-model", None, 4)
            .expect("compact should succeed");

        assert_ne!(compacted.id, "compact-parent");
        assert_eq!(compacted.parent_id.as_deref(), Some("compact-parent"));
        assert!(compacted.messages[0]
            .content
            .contains("<lattice-compact-summary"));
        let boundary = compacted.messages[0].compact.as_ref().unwrap();
        assert_eq!(boundary.mode, "manual");
        assert_eq!(
            boundary.parent_session_id.as_deref(),
            Some("compact-parent")
        );
        assert!(boundary.summarized_message_count > 0);
    }

    #[test]
    fn messages_for_session_preserves_tool_fields() {
        let mut session = lattice_session::Session::new("model".into(), "provider".into());
        session.messages.push(lattice_session::SessionMessage {
            role: "assistant".into(),
            content: String::new(),
            reasoning_content: Some("thinking".into()),
            tool_calls: Some(vec![lattice_session::SessionToolCall {
                id: "call_1".into(),
                name: "grep".into(),
                arguments: r#"{"pattern":"x"}"#.into(),
            }]),
            tool_call_id: None,
            name: None,
            artifact_ids: Vec::new(),
            compact: None,
        });
        session.messages.push(lattice_session::SessionMessage {
            role: "tool".into(),
            content: "result".into(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: Some("call_1".into()),
            name: Some("grep".into()),
            artifact_ids: Vec::new(),
            compact: None,
        });

        let messages = messages_for_session(&session);

        assert_eq!(messages[0].role, Role::Assistant);
        assert_eq!(messages[0].reasoning_content.as_deref(), Some("thinking"));
        assert_eq!(
            messages[0].tool_calls.as_ref().unwrap()[0].function.name,
            "grep"
        );
        assert_eq!(messages[1].role, Role::Tool);
        assert_eq!(messages[1].tool_call_id.as_deref(), Some("call_1"));
    }

    fn register_test_model(runtime: &mut Runtime, name: &str) {
        runtime
            .register_model(ModelCatalogEntry {
                canonical_id: name.to_string(),
                context_length: 8192,
                providers: vec![CatalogProviderEntry {
                    provider_id: "ollama".to_string(),
                    api_model_id: name.to_string(),
                    priority: 1,
                    credential_keys: HashMap::new(),
                    base_url: Some("http://localhost:11434".to_string()),
                    api_protocol: lattice_core::catalog::ApiProtocol::OpenAiChat,
                    provider_specific: HashMap::new(),
                }],
                aliases: vec![],
            })
            .unwrap();
    }

    #[test]
    fn agent_spec_without_capabilities_fails_fast() {
        let mut runtime = Runtime::builder().build();
        register_test_model(&mut runtime, "missing-capabilities-test-model");

        let result = runtime.build_agent(AgentSpec::new("missing-capabilities-test-model".into()));

        assert!(
            matches!(result, Err(RuntimeError::Config(message)) if message.contains("AgentSpec must declare capabilities"))
        );
    }

    #[test]
    fn agent_spec_single_bundle_builds_with_expected_tools() {
        let mut runtime = Runtime::builder().build();
        register_test_model(&mut runtime, "minimal-test-model");

        let built = runtime
            .build_agent(
                AgentSpec::new("minimal-test-model".into())
                    .capability_bundle("workspace"),
            )
            .unwrap();

        assert!(!built.tool_definitions.is_empty());
    }

    #[test]
    fn coding_capable_bundles_require_runtime_security() {
        let mut runtime = Runtime::builder().build();
        register_test_model(&mut runtime, "code-agent-test-model");

        let result = runtime.build_agent(
            AgentSpec::new("code-agent-test-model".into()).capability_preset("coding"),
        );

        assert!(
            matches!(&result, Err(RuntimeError::Config(msg)) if msg.contains("require") && msg.contains("RuntimeSecurity")),
            "expected security error, got: {}",
            result.as_ref().err().map(|e| e.to_string()).unwrap_or_else(|| "Ok(_)".into())
        );
    }

    #[test]
    fn coding_capable_bundles_expose_workspace_editing_shell_planning_and_web() {
        let mut runtime = Runtime::builder().build();
        register_test_model(&mut runtime, "code-agent-test-model");
        let security =
            build_runtime_security(&RuntimeSecurityConfig::default(), Path::new(".")).unwrap();

        let built = runtime
            .build_agent(
                AgentSpec::new("code-agent-test-model".into())
                    .security(Some(security))
                    .capability_preset("coding"),
            )
            .unwrap();
        let names = built
            .tool_definitions
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>();

        for expected in [
            "workspace_context",
            "write_file",
            "patch",
            "bash",
            "todo_write",
            "web_search",
        ] {
            assert!(names.contains(&expected), "{expected} missing");
        }
        assert!(!names.contains(&"multi_patch"), "multi_patch leaked");
    }

    #[test]
    fn generic_agent_with_filesystem_bundle_does_not_expose_coding_tools() {
        let mut runtime = Runtime::builder().build();
        register_test_model(&mut runtime, "generic-agent-test-model");

        let built = runtime
            .build_agent(
                AgentSpec::new("generic-agent-test-model".into())
                    .capability_bundle("filesystem"),
            )
            .unwrap();
        let names = built
            .tool_definitions
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>();

        for excluded in ["patch", "bash", "todo_write", "web_search"] {
            assert!(!names.contains(&excluded), "{excluded} leaked");
        }
    }

    #[test]
    fn test_approve_valid_command_rule() {
        let shared = Arc::new(Mutex::new(Vec::new()));
        let security = RuntimeSecurity {
            mode_label: "strict".into(),
            sandbox: SandboxConfig::default(),
            approved_rules: Arc::clone(&shared),
            policy_authority: PolicyAuthority::from_shared_rules(Arc::clone(&shared)),
            audit_log: None,
        };
        let rule = ApprovedRule {
            kind: ApprovalKind::CommandPrefix {
                prefix: "cargo publish".to_string(),
            },
            risk_class: lattice_sandbox::RiskClass::Mutate,
        };
        assert!(
            security.approve_rule(rule).is_ok(),
            "valid command rule should be approved"
        );
        assert_eq!(security.approved_rules.lock().unwrap().len(), 1);
    }

    #[test]
    fn test_approve_exact_single_token_command_rule() {
        let shared = Arc::new(Mutex::new(Vec::new()));
        let security = RuntimeSecurity {
            mode_label: "strict".into(),
            sandbox: SandboxConfig::default(),
            approved_rules: Arc::clone(&shared),
            policy_authority: PolicyAuthority::from_shared_rules(Arc::clone(&shared)),
            audit_log: None,
        };
        let rule = ApprovedRule {
            kind: ApprovalKind::CommandExact {
                command: "make".to_string(),
            },
            risk_class: lattice_sandbox::RiskClass::Mutate,
        };

        security
            .approve_rule(rule)
            .expect("single-token exact command should be approved");
        assert_eq!(security.approved_rules.lock().unwrap().len(), 1);
    }

    #[test]
    fn test_approve_invalid_single_token_rejected() {
        let shared = Arc::new(Mutex::new(Vec::new()));
        let security = RuntimeSecurity {
            mode_label: "strict".into(),
            sandbox: SandboxConfig::default(),
            approved_rules: Arc::clone(&shared),
            policy_authority: PolicyAuthority::from_shared_rules(Arc::clone(&shared)),
            audit_log: None,
        };
        let rule = ApprovedRule {
            kind: ApprovalKind::CommandPrefix {
                prefix: "cargo".to_string(),
            },
            risk_class: lattice_sandbox::RiskClass::Mutate,
        };
        let result = security.approve_rule(rule);
        assert!(
            matches!(result, Err(ValidationError::TooFewTokens { .. })),
            "single-token rule should be rejected: {:?}",
            result
        );
    }

    #[test]
    fn test_approve_dangerous_pattern_rejected() {
        let shared = Arc::new(Mutex::new(Vec::new()));
        let security = RuntimeSecurity {
            mode_label: "strict".into(),
            sandbox: SandboxConfig::default(),
            approved_rules: Arc::clone(&shared),
            policy_authority: PolicyAuthority::from_shared_rules(Arc::clone(&shared)),
            audit_log: None,
        };
        let rule = ApprovedRule {
            kind: ApprovalKind::CommandPrefix {
                prefix: "rm -rf /tmp".to_string(),
            },
            risk_class: lattice_sandbox::RiskClass::Mutate,
        };
        let result = security.approve_rule(rule);
        assert!(
            matches!(result, Err(ValidationError::DangerousPattern { .. })),
            "dangerous pattern should be rejected: {:?}",
            result
        );
    }

    #[test]
    fn test_approved_rules_shared_with_sandbox() {
        let shared = Arc::new(Mutex::new(Vec::new()));
        let mut sandbox = SandboxConfig::default();
        sandbox.approved_rules = Some(Arc::clone(&shared));
        sandbox.policy_authority = Some(PolicyAuthority::from_shared_rules(Arc::clone(&shared)));
        let security = RuntimeSecurity {
            mode_label: "strict".into(),
            sandbox,
            approved_rules: Arc::clone(&shared),
            policy_authority: PolicyAuthority::from_shared_rules(Arc::clone(&shared)),
            audit_log: None,
        };

        let rule = ApprovedRule {
            kind: ApprovalKind::CommandPrefix {
                prefix: "cargo publish".to_string(),
            },
            risk_class: lattice_sandbox::RiskClass::Mutate,
        };
        security.approve_rule(rule).unwrap();

        // Both the RuntimeSecurity and the sandbox should see the same entry
        assert_eq!(security.approved_rules.lock().unwrap().len(), 1);
        assert_eq!(shared.lock().unwrap().len(), 1);
    }

    #[test]
    fn test_approve_rule_keeps_legacy_rules_in_sync_when_store_mismatches() {
        let legacy = Arc::new(Mutex::new(Vec::new()));
        let authority = PolicyAuthority::new();
        let security = RuntimeSecurity {
            mode_label: "strict".into(),
            sandbox: SandboxConfig::default(),
            approved_rules: Arc::clone(&legacy),
            policy_authority: authority.clone(),
            audit_log: None,
        };

        security
            .approve_rule(ApprovedRule {
                kind: ApprovalKind::CommandExact {
                    command: "make".to_string(),
                },
                risk_class: lattice_sandbox::RiskClass::Mutate,
            })
            .expect("exact command should be approved");

        assert_eq!(legacy.lock().unwrap().len(), 1);
        assert_eq!(authority.len(), 1);
    }

    // ============================================================================
    // Plan 12-04: RuntimeSecurity integration tests
    // ============================================================================

    #[test]
    fn test_runtime_security_approval_flow() {
        let workdir =
            std::env::temp_dir().join(format!("lattice-approval-flow-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&workdir);

        let security_config = RuntimeSecurityConfig {
            sandbox_mode: "project".into(),
            read_allowlist: vec![],
            write_allowlist: vec![],
            command_allowlist: vec!["cargo test".into()],
            hook_chain: false,
            landlock: false,
            audit: false,
            audit_dir: None,
            max_command_timeout: None,
            max_read_size: None,
            max_write_size: None,
            max_http_response_size: None,
        };
        let security =
            build_runtime_security(&security_config, &workdir).expect("build_runtime_security");

        // Phase 1: deny an unapproved command
        let result = security.sandbox.check_command("npm install");
        assert!(
            result.is_err(),
            "npm install should be denied (not in command_allowlist)"
        );

        // Phase 2: approve the rule
        security
            .approve_rule(ApprovedRule {
                kind: ApprovalKind::CommandPrefix {
                    prefix: "npm install".into(),
                },
                risk_class: lattice_sandbox::RiskClass::Mutate,
            })
            .expect("approve_rule should succeed for valid 2-token command");

        // Phase 3: re-check — now allowed by approved rule
        assert!(
            security
                .sandbox
                .check_command("npm install express")
                .is_ok(),
            "approved rule should allow npm install express"
        );

        let _ = std::fs::remove_dir_all(&workdir);
    }

    #[test]
    fn test_runtime_security_shared_state() {
        let workdir =
            std::env::temp_dir().join(format!("lattice-shared-state-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&workdir);

        let security_config = RuntimeSecurityConfig {
            sandbox_mode: "project".into(),
            command_allowlist: vec!["cargo test".into()],
            ..Default::default()
        };
        let security =
            build_runtime_security(&security_config, &workdir).expect("build_runtime_security");

        // Approve a rule via RuntimeSecurity
        security
            .approve_rule(ApprovedRule {
                kind: ApprovalKind::CommandPrefix {
                    prefix: "npm run build".into(),
                },
                risk_class: lattice_sandbox::RiskClass::Mutate,
            })
            .expect("approve_rule should succeed");

        // Both RuntimeSecurity.approved_rules and sandbox.approved_rules
        // should share the same Arc<Mutex<>> backing store.
        assert_eq!(
            security.approved_rules.lock().unwrap().len(),
            1,
            "RuntimeSecurity.approved_rules should have 1 entry"
        );
        assert!(
            security.sandbox.approved_rules.is_some(),
            "sandbox.approved_rules should be Some"
        );
        assert_eq!(
            security
                .sandbox
                .approved_rules
                .as_ref()
                .unwrap()
                .lock()
                .unwrap()
                .len(),
            1,
            "sandbox.approved_rules should have 1 entry (same Arc)"
        );
        // Verify they are the same Arc instance (pointer equality)
        assert!(Arc::ptr_eq(
            &security.approved_rules,
            security.sandbox.approved_rules.as_ref().unwrap()
        ));

        let _ = std::fs::remove_dir_all(&workdir);
    }

    #[test]
    fn test_runtime_security_rejects_broad_rules() {
        let workdir =
            std::env::temp_dir().join(format!("lattice-broad-rules-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&workdir);

        let security_config = RuntimeSecurityConfig {
            sandbox_mode: "project".into(),
            command_allowlist: vec!["cargo test".into()],
            ..Default::default()
        };
        let security =
            build_runtime_security(&security_config, &workdir).expect("build_runtime_security");

        // Single-token command rejected (TooFewTokens)
        let r = security.approve_rule(ApprovedRule {
            kind: ApprovalKind::CommandPrefix {
                prefix: "npm".into(),
            },
            risk_class: lattice_sandbox::RiskClass::Mutate,
        });
        assert!(
            matches!(r, Err(ValidationError::TooFewTokens { .. })),
            "single-token 'npm' should be TooFewTokens: {:?}",
            r
        );

        // Blocked top-level path rejected
        let r = security.approve_rule(ApprovedRule {
            kind: ApprovalKind::PathRecursive {
                root: std::path::PathBuf::from("/"),
                operation: lattice_sandbox::PathOperation::Read,
            },
            risk_class: lattice_sandbox::RiskClass::Read,
        });
        assert!(
            matches!(r, Err(ValidationError::BlockedTopLevel { .. })),
            "'/' should be BlockedTopLevel: {:?}",
            r
        );

        // Unknown bundle rejected
        let r = security.approve_rule(ApprovedRule {
            kind: ApprovalKind::Bundle {
                bundle_name: "nonexistent".into(),
            },
            risk_class: lattice_sandbox::RiskClass::Mutate,
        });
        assert!(
            matches!(r, Err(ValidationError::UnknownBundle { .. })),
            "'nonexistent' bundle should be UnknownBundle: {:?}",
            r
        );

        let _ = std::fs::remove_dir_all(&workdir);
    }

    #[test]
    fn test_cli_and_runtime_produce_same_security() {
        // PERM-04: Verify that build_runtime_security with the same config
        // produces the same sandbox properties regardless of caller (CLI or direct).
        let workdir =
            std::env::temp_dir().join(format!("lattice-same-security-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&workdir);

        let security_config = RuntimeSecurityConfig {
            sandbox_mode: "project".into(),
            read_allowlist: vec!["/custom/read/path".into()],
            write_allowlist: vec!["/custom/write/path".into()],
            command_allowlist: vec!["cargo test".into(), "git status".into()],
            hook_chain: false,
            landlock: false,
            audit: false,
            audit_dir: None,
            max_command_timeout: Some(60),
            max_read_size: Some(5 * 1024 * 1024),
            max_write_size: Some(512 * 1024),
            max_http_response_size: Some(2 * 1024 * 1024),
        };
        let security =
            build_runtime_security(&security_config, &workdir).expect("build_runtime_security");

        assert_eq!(security.mode_label, "project");
        assert_eq!(
            security.sandbox.read_allowlist,
            vec!["/custom/read/path"],
            "custom read_allowlist should be preserved"
        );
        assert_eq!(
            security.sandbox.write_allowlist,
            vec!["/custom/write/path"],
            "custom write_allowlist should be preserved"
        );
        assert_eq!(
            security.sandbox.command_allowlist,
            vec!["cargo test", "git status"],
            "custom command_allowlist should be preserved"
        );
        assert_eq!(security.sandbox.max_command_timeout, 60);
        assert_eq!(security.sandbox.max_read_size, 5 * 1024 * 1024);
        assert_eq!(security.sandbox.max_write_size, 512 * 1024);
        assert_eq!(security.sandbox.max_http_response_size, 2 * 1024 * 1024);
        assert!(
            security.sandbox.approved_rules.is_some(),
            "approved_rules should be wired in"
        );
        assert!(
            security
                .sandbox
                .approved_rules
                .as_ref()
                .unwrap()
                .lock()
                .unwrap()
                .is_empty(),
            "approved_rules should be empty on fresh build"
        );

        let _ = std::fs::remove_dir_all(&workdir);
    }

    // ---------------------------------------------------------------
    // Handle lifecycle tests (Plan 16-02)
    // ---------------------------------------------------------------

    fn make_test_spec(name: &str, model: &str) -> lattice_orchestrator::AgentTurnSpec {
        lattice_orchestrator::AgentTurnSpec {
            profile_name: name.into(),
            model: model.into(),
            provider_override: None,
            system_prompt: Some("You are a test agent.".into()),
            prompt_spec: None,
            capabilities: Default::default(),
            tools: Default::default(),
            behavior: Default::default(),
            handoff: Default::default(),
            memory: Default::default(),
            project_root: None,
            cwd: None,
            thinking_effort: None,
        }
    }

    fn make_test_security() -> RuntimeSecurity {
        let workdir =
            std::env::temp_dir().join(format!("lattice-handle-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&workdir);
        build_runtime_security(&RuntimeSecurityConfig::default(), &workdir).unwrap()
    }

    fn make_test_runtime() -> Runtime {
        let mut runtime = Runtime::builder().build();
        // Register a test model
        runtime
            .register_model(lattice_core::catalog::ModelCatalogEntry {
                canonical_id: "test-model".to_string(),
                context_length: 8192,
                providers: vec![lattice_core::catalog::CatalogProviderEntry {
                    provider_id: "ollama".to_string(),
                    api_model_id: "test-model".to_string(),
                    priority: 1,
                    credential_keys: HashMap::new(),
                    base_url: Some("http://localhost:11434".to_string()),
                    api_protocol: lattice_core::catalog::ApiProtocol::OpenAiChat,
                    provider_specific: HashMap::new(),
                }],
                aliases: vec![],
            })
            .unwrap();
        runtime
    }

    #[test]
    fn handle_registry_is_initialized() {
        let runtime = make_test_runtime();
        let registry = runtime.handles.lock().unwrap();
        assert!(registry.by_id.is_empty());
        assert!(registry.by_key.is_empty());
    }

    #[test]
    fn handle_lifecycle_get_or_create() {
        let runtime = make_test_runtime();
        let spec = make_test_spec("test-agent", "test-model");
        let security = make_test_security();

        let id1 = runtime
            .get_or_create_agent_handle(spec.clone(), security.clone())
            .unwrap();
        let id2 = runtime
            .get_or_create_agent_handle(spec.clone(), security.clone())
            .unwrap();

        // Same spec + security should return the same handle
        assert_eq!(id1, id2, "equivalent specs should reuse the same handle");
    }

    #[test]
    fn handle_lifecycle_different_security_produces_different_handles() {
        let runtime = make_test_runtime();
        let spec = make_test_spec("agent", "test-model");

        let workdir1 = std::env::temp_dir().join(format!("sec1-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&workdir1);
        let sec1 = build_runtime_security(&RuntimeSecurityConfig::default(), &workdir1).unwrap();

        let mut config2 = RuntimeSecurityConfig::default();
        config2.sandbox_mode = "permissive".into();
        let workdir2 = std::env::temp_dir().join(format!("sec2-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&workdir2);
        let sec2 = build_runtime_security(&config2, &workdir2).unwrap();

        let id1 = runtime
            .get_or_create_agent_handle(spec.clone(), sec1)
            .unwrap();
        let id2 = runtime
            .get_or_create_agent_handle(spec.clone(), sec2)
            .unwrap();

        assert_ne!(
            id1, id2,
            "different security policies should produce different handles"
        );
    }

    #[test]
    fn handle_lifecycle_different_sessions_produce_different_handles() {
        let runtime = make_test_runtime();
        let spec = make_test_spec("agent", "test-model");
        let security = make_test_security();

        let id1 = runtime
            .get_or_create_agent_handle_for_session(
                spec.clone(),
                security.clone(),
                Some("session-a".into()),
            )
            .unwrap();
        let id1_again = runtime
            .get_or_create_agent_handle_for_session(
                spec.clone(),
                security.clone(),
                Some("session-a".into()),
            )
            .unwrap();
        let id2 = runtime
            .get_or_create_agent_handle_for_session(spec, security, Some("session-b".into()))
            .unwrap();

        assert_eq!(id1, id1_again, "same session should reuse a handle");
        assert_ne!(id1, id2, "different sessions must not share a handle");
    }

    #[test]
    fn handle_lifecycle_security_allowlists_produce_different_handles() {
        let runtime = make_test_runtime();
        let spec = make_test_spec("agent", "test-model");
        let workdir =
            std::env::temp_dir().join(format!("lattice-security-key-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&workdir);

        let sec1 = build_runtime_security(&RuntimeSecurityConfig::default(), &workdir).unwrap();
        let sec2 = build_runtime_security(
            &RuntimeSecurityConfig {
                read_allowlist: vec![workdir.join("readonly").to_string_lossy().to_string()],
                ..Default::default()
            },
            &workdir,
        )
        .unwrap();

        let id1 = runtime
            .get_or_create_agent_handle(spec.clone(), sec1)
            .unwrap();
        let id2 = runtime.get_or_create_agent_handle(spec, sec2).unwrap();

        assert_ne!(
            id1, id2,
            "same sandbox mode with different allowlists must not share a handle"
        );
        let _ = std::fs::remove_dir_all(&workdir);
    }

    #[test]
    fn handle_rebuild_updates_lookup_key() {
        let mut runtime = make_test_runtime();
        register_test_model(&mut runtime, "test-model-2");
        let security = make_test_security();
        let spec1 = make_test_spec("agent", "test-model");
        let spec2 = make_test_spec("agent", "test-model-2");

        let id = runtime
            .get_or_create_agent_handle(spec1.clone(), security.clone())
            .unwrap();
        runtime
            .rebuild_agent_handle(&id, spec2.clone(), security.clone())
            .unwrap();

        let id_for_new_key = runtime
            .get_or_create_agent_handle(spec2, security.clone())
            .unwrap();
        let id_for_old_key = runtime.get_or_create_agent_handle(spec1, security).unwrap();

        assert_eq!(
            id, id_for_new_key,
            "rebuilt handle should be keyed by new spec"
        );
        assert_ne!(
            id, id_for_old_key,
            "old key should no longer point at rebuilt handle"
        );
    }

    #[test]
    fn handle_lifecycle_rebuilds_stale_handle_without_agent() {
        let runtime = make_test_runtime();
        let spec = make_test_spec("agent", "test-model");
        let security = make_test_security();

        let stale_id = runtime
            .get_or_create_agent_handle(spec.clone(), security.clone())
            .unwrap();
        {
            let mut registry = runtime.handles.lock().unwrap();
            let state = registry.by_id.get_mut(&stale_id).unwrap();
            state.agent = None;
            state.active_turn = false;
        }

        let rebuilt_id = runtime.get_or_create_agent_handle(spec, security).unwrap();

        assert_ne!(
            stale_id, rebuilt_id,
            "stale handle without an agent should be replaced"
        );
        assert!(runtime.agent_handle_snapshot(&stale_id).is_err());
        assert!(runtime.agent_handle_snapshot(&rebuilt_id).is_ok());
    }

    #[test]
    fn handle_lifecycle_dispose_removes_handle() {
        let runtime = make_test_runtime();
        let spec = make_test_spec("agent", "test-model");
        let security = make_test_security();

        let id = runtime.get_or_create_agent_handle(spec, security).unwrap();

        // Exists before dispose
        assert!(runtime.agent_handle_snapshot(&id).is_ok());

        runtime.dispose_agent_handle(&id).unwrap();

        // Gone after dispose
        assert!(matches!(
            runtime.agent_handle_snapshot(&id),
            Err(RuntimeError::HandleNotFound(_))
        ));
    }

    #[test]
    fn handle_lifecycle_reap_cleans_idle() {
        let runtime = make_test_runtime();
        let spec = make_test_spec("agent", "test-model");
        let security = make_test_security();

        let _id = runtime.get_or_create_agent_handle(spec, security).unwrap();

        // Reap with zero idle time should clean all inactive handles
        let reaped = runtime.reap_idle_agent_handles(Duration::ZERO);
        assert_eq!(reaped, 1, "should reap the idle handle");

        let registry = runtime.handles.lock().unwrap();
        assert!(registry.by_id.is_empty());
    }

    #[test]
    fn handle_lifecycle_snapshot_returns_metadata() {
        let runtime = make_test_runtime();
        let spec = make_test_spec("test-agent", "test-model");
        let security = make_test_security();

        let id = runtime.get_or_create_agent_handle(spec, security).unwrap();

        let snap = runtime.agent_handle_snapshot(&id).unwrap();
        assert_eq!(snap.model, "test-model");
        assert!(!snap.active_turn);
    }

    #[test]
    fn handle_rebuild_preserves_messages() {
        let runtime = make_test_runtime();
        let spec = make_test_spec("test-agent", "test-model");
        let security = make_test_security();

        let id = runtime
            .get_or_create_agent_handle(spec.clone(), security.clone())
            .unwrap();

        // Push a user message into the internal agent
        {
            let mut registry = runtime.handles.lock().unwrap();
            let state = registry.by_id.get_mut(&id).unwrap();
            state
                .agent
                .as_mut()
                .unwrap()
                .seed_messages(vec![lattice_core::types::Message {
                    role: lattice_core::types::Role::User,
                    content: "carry me".to_string(),
                    reasoning_content: None,
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                }]);
        }

        // Rebuild
        let snap = runtime.rebuild_agent_handle(&id, spec, security).unwrap();
        assert!(snap.revision > 0, "revision should increment on rebuild");
        assert!(snap.message_count >= 1, "messages should be preserved");

        // Verify messages survived
        let registry = runtime.handles.lock().unwrap();
        let state = registry.by_id.get(&id).unwrap();
        let msgs = state.agent.as_ref().unwrap().messages();
        assert!(
            msgs.iter().any(|m| m.content.contains("carry me")),
            "messages should survive rebuild"
        );
    }

    #[test]
    fn handle_rebuild_rejects_active_turn() {
        let runtime = make_test_runtime();
        let spec = make_test_spec("test-agent", "test-model");
        let security = make_test_security();

        let id = runtime
            .get_or_create_agent_handle(spec.clone(), security.clone())
            .unwrap();
        {
            let mut registry = runtime.handles.lock().unwrap();
            let state = registry.by_id.get_mut(&id).unwrap();
            state.active_turn = true;
            state.active_turn_id = Some(crate::turn::TurnId::new());
        }

        let result = runtime.rebuild_agent_handle(&id, spec, security);

        assert!(
            matches!(result, Err(RuntimeError::HandleBusy(handle)) if handle == id.to_string()),
            "active handles must not be rebuilt"
        );
    }

    #[test]
    fn handle_rebuild_preserves_task_state() {
        let runtime = make_test_runtime();
        let spec = make_test_spec("test-agent", "test-model");
        let security = make_test_security();

        let id = runtime
            .get_or_create_agent_handle(spec.clone(), security.clone())
            .unwrap();

        // Mark unverified work in task state
        {
            let mut registry = runtime.handles.lock().unwrap();
            let state = registry.by_id.get_mut(&id).unwrap();
            state
                .agent
                .as_mut()
                .unwrap()
                .task_state_mut()
                .mark_unverified("carry me".into());
        }

        // Rebuild
        runtime.rebuild_agent_handle(&id, spec, security).unwrap();

        // Verify task state survived
        let registry = runtime.handles.lock().unwrap();
        let state = registry.by_id.get(&id).unwrap();
        let ts = state.agent.as_ref().unwrap().task_state();
        assert!(
            ts.unverified_work.iter().any(|u| u.contains("carry me")),
            "task state should survive rebuild"
        );
    }

    #[test]
    fn handle_rebuild_preserves_security_approvals() {
        let runtime = make_test_runtime();
        let spec = make_test_spec("test-agent", "test-model");
        let security = make_test_security();

        // Approve a read path rule
        let workdir = std::env::temp_dir().join(format!("lattice-approval-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&workdir);
        security
            .approve_rule(lattice_sandbox::ApprovedRule {
                kind: lattice_sandbox::ApprovalKind::PathExact {
                    path: workdir.join("test.txt"),
                    operation: lattice_sandbox::PathOperation::Read,
                },
                risk_class: lattice_sandbox::RiskClass::Read,
            })
            .unwrap();

        let id = runtime
            .get_or_create_agent_handle(spec.clone(), security.clone())
            .unwrap();

        // Rebuild
        runtime
            .rebuild_agent_handle(&id, spec, security.clone())
            .unwrap();

        // Verify approvals survived (security is shared)
        let approved_count = security.approved_rules.lock().unwrap().len();
        assert!(approved_count >= 1, "approved rules should survive rebuild");
    }

    #[test]
    fn handle_approval_last_uses_authoritative_task_state() {
        let runtime = make_test_runtime();
        let spec = make_test_spec("test-agent", "test-model");
        let security = make_test_security();
        let id = runtime
            .get_or_create_agent_handle(spec, security.clone())
            .unwrap();

        {
            let mut registry = runtime.handles.lock().unwrap();
            let state = registry.by_id.get_mut(&id).unwrap();
            state
                .agent
                .as_mut()
                .unwrap()
                .task_state_mut()
                .record_approval_proposal(
                    "Allow cargo test".into(),
                    "/approve command \"cargo test\"".into(),
                    "mutate".into(),
                );
        }

        let message = runtime
            .approve_handle_pending_proposal(&id, "last")
            .expect("runtime approval should resolve through handle state");

        assert_eq!(message, "Approved proposal ap-1");
        assert_eq!(security.approved_rules.lock().unwrap().len(), 1);
        let registry = runtime.handles.lock().unwrap();
        let task_state = registry
            .by_id
            .get(&id)
            .unwrap()
            .agent
            .as_ref()
            .unwrap()
            .task_state();
        assert!(task_state.approval_proposals[0].resolved);
    }

    #[test]
    fn runtime_plugin_overlay_sets_and_clears_handle_overlay() {
        let runtime = make_test_runtime();
        let spec = make_test_spec("test-agent", "test-model");
        let security = make_test_security();
        let id = runtime.get_or_create_agent_handle(spec, security).unwrap();
        let overlay = RuntimePluginOverlay {
            name: "CodeReview".into(),
            system_prompt_delta: "review code".into(),
            output_contract_delta: Some("return JSON".into()),
            tools: vec![ToolDefinition::new(
                "plugin_echo".into(),
                "Echo plugin input".into(),
                serde_json::json!({"type": "object"}),
            )],
            authorized_tool_names: vec!["plugin_echo".into()],
        };

        runtime
            .set_agent_plugin_overlay(&id, Some(overlay.clone()))
            .expect("overlay should set");
        {
            let registry = runtime.handles.lock().unwrap();
            let stored = registry.by_id.get(&id).unwrap().plugin_overlay.as_ref();
            assert_eq!(stored, Some(&overlay));
        }

        runtime
            .set_agent_plugin_overlay(&id, None)
            .expect("overlay should clear");
        let registry = runtime.handles.lock().unwrap();
        assert!(registry.by_id.get(&id).unwrap().plugin_overlay.is_none());
    }

    #[tokio::test]
    async fn internal_agent_recovery_semantics_are_explicit() {
        let runtime = make_test_runtime();
        let spec = make_test_spec("test-agent", "test-model");
        let security = make_test_security();
        let id = runtime.get_or_create_agent_handle(spec, security).unwrap();
        {
            let mut registry = runtime.handles.lock().unwrap();
            let state = registry.by_id.get_mut(&id).unwrap();
            state.agent = None;
            state.active_turn = false;
            state.active_turn_id = None;
            state.cancellation_requested = false;
        }

        let err = match runtime
            .submit_turn(
                &id,
                TurnRequest {
                    input: "should fail before provider call".into(),
                    max_turns: 1,
                    persist_session: false,
                    session_id: None,
                    cancellation_id: None,
                },
            )
            .await
        {
            Ok(_) => panic!("missing internal agent should not submit a turn"),
            Err(err) => err,
        };

        assert!(
            matches!(err, RuntimeError::Config(message) if message.contains("handle has no internal agent; rebuild required"))
        );
        assert!(
            !runtime.agent_handle_snapshot(&id).unwrap().active_turn,
            "failed submit must not leave the handle busy"
        );
    }

    #[test]
    fn handle_lifecycle_reap_preserves_active_handle() {
        let runtime = make_test_runtime();
        let spec = make_test_spec("agent", "test-model");
        let security = make_test_security();

        let id = runtime.get_or_create_agent_handle(spec, security).unwrap();

        // Mark handle as active
        {
            let mut registry = runtime.handles.lock().unwrap();
            registry.by_id.get_mut(&id).unwrap().active_turn = true;
        }

        // Reap should skip active handle
        let reaped = runtime.reap_idle_agent_handles(Duration::ZERO);
        assert_eq!(reaped, 0, "active handle should not be reaped");
    }
}

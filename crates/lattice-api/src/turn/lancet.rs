//! Turn-scoped Lancet runtime composition and the CEP client-backed executor adapter.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use lancet_executor::policy::default::{DefaultPolicyHook, ExternalPolicyFile, PolicyConfig};
use lancet_executor::{
    build_in_toto_statement, impact_kind_for_method, impact_kind_label, invariants,
    register_production_method_registry, source_audit_range_hash, to_tool_definitions,
    wire_code_for_error, ActiveEnvelopeStore, AttestationRecord, AuditQueryFilters, BudgetLimits,
    CallAuditMetadata, CepAuditContext, CepAuditStore, CommandPolicy, LancetClient,
    LancetTransport, MethodRegistry, NetworkPolicyConfig, ObservationPolicyConfig,
    Phase21PolicyConfig, Phase21TurnContext, ServerRunConfig,
};
use lancet_protocol::catalog;
use lancet_protocol::envelope::BaseExecutionEnvelope;
use lancet_protocol::error::{
    invalid_params, jsonrpc, method_not_found, parse_error, CepErrorData,
};
use lancet_protocol::methods::audit::{AuditQuery, AuditReplay, AuditTail};
use lancet_protocol::methods::command::Run;
use lancet_protocol::methods::envelope::{Declare, SubmitIntent};
use lancet_protocol::methods::external::Fetch;
use lancet_protocol::methods::session::TodoWrite;
use lancet_protocol::methods::workspace::{
    FindFiles, Grep, ListDirectory, Patch, ReadFile, WriteFile,
};
use lancet_protocol::{CatalogMethod, CepError, CepMethod, Retriability, ToolEvent};
use lattice_agent::{
    AgentLoop, AgentLoopConfig, AgentTurnEvent, ToolCallExecutor, ToolExecutionError,
};
use lattice_llm::ToolCall;
use lattice_profile::{CepNetworkSection, CepObservationSection, CepSection};
use lattice_tooling::resolve_enabled_tools;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::RwLock;

use crate::session::ApiError;

/// Per-build construction metrics (test-only; each production build is exactly one pair + adapter).
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Phase20BuildMetrics {
    pub adapter_instances: usize,
    pub transport_pairs: usize,
}

/// Client-backed adapter that maps LLM-shape tool calls into typed CEP invocations.
pub struct LancetExecutorAdapter {
    client: LancetClient,
    catalog_names: HashSet<String>,
    enabled_names: HashSet<String>,
}

impl LancetExecutorAdapter {
    pub fn new(
        client: LancetClient,
        enabled_names: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        let catalog_names = catalog().iter().map(|d| d.name.to_string()).collect();
        let enabled_names = enabled_names.into_iter().map(Into::into).collect();
        Self {
            client,
            catalog_names,
            enabled_names,
        }
    }

    fn map_cep_error(cep: CepError) -> ToolExecutionError {
        ToolExecutionError {
            message: cep.message.clone(),
            cep_error: Some(lattice_agent::CepErrorPayload {
                code: adapter_wire_code(&cep),
                message: cep.message,
                data: serde_json::to_value(&cep.data).unwrap_or(Value::Null),
                retriability: retriability_label(&cep.retriability),
            }),
        }
    }

    fn catalog_method_names(&self) -> &HashSet<String> {
        &self.catalog_names
    }

    async fn dispatch_typed(&self, method: &str, params: Value) -> Result<Value, CepError> {
        match method {
            ReadFile::NAME => self.call_typed::<ReadFile>(params).await,
            Grep::NAME => self.call_typed::<Grep>(params).await,
            ListDirectory::NAME => self.call_typed::<ListDirectory>(params).await,
            FindFiles::NAME => self.call_typed::<FindFiles>(params).await,
            Fetch::NAME => self.call_typed::<Fetch>(params).await,
            TodoWrite::NAME => self.call_typed::<TodoWrite>(params).await,
            Run::NAME => self.call_typed::<Run>(params).await,
            WriteFile::NAME => self.call_typed::<WriteFile>(params).await,
            Patch::NAME => self.call_typed::<Patch>(params).await,
            Declare::NAME => self.call_typed::<Declare>(params).await,
            SubmitIntent::NAME => self.call_typed::<SubmitIntent>(params).await,
            AuditQuery::NAME => self.call_typed::<AuditQuery>(params).await,
            AuditTail::NAME => self.call_typed::<AuditTail>(params).await,
            AuditReplay::NAME => self.call_typed::<AuditReplay>(params).await,
            other => Err(method_not_found(other)),
        }
    }

    async fn call_typed<M>(&self, params: Value) -> Result<Value, CepError>
    where
        M: CatalogMethod,
        M::Params: DeserializeOwned + Serialize,
        M::Result: Serialize,
    {
        let typed = serde_json::from_value::<M::Params>(params)
            .map_err(|err| schema_mismatch_for_method::<M>(err))?;
        let result = self.client.call::<M>(typed).await?;
        serde_json::to_value(result).map_err(|err| {
            CepError::internal_error(format!("failed to serialize {} result: {err}", M::NAME))
        })
    }
}

#[async_trait]
impl ToolCallExecutor for LancetExecutorAdapter {
    async fn execute(&self, call: &ToolCall) -> Result<String, ToolExecutionError> {
        let params: Value = match serde_json::from_str(&call.function.arguments) {
            Ok(value) => value,
            Err(err) => {
                return Err(Self::map_cep_error(parse_error(format!(
                    "invalid tool arguments JSON: {err}"
                ))));
            }
        };

        if !self.catalog_method_names().contains(&call.function.name) {
            return Err(Self::map_cep_error(method_not_found(&call.function.name)));
        }

        if !self.enabled_names.contains(&call.function.name) {
            return Err(Self::map_cep_error(method_not_found(&call.function.name)));
        }

        match self.dispatch_typed(&call.function.name, params).await {
            Ok(value) => serde_json::to_string(&value).map_err(|err| {
                ToolExecutionError::simple(format!("failed to serialize tool result: {err}"))
            }),
            Err(cep) => Err(Self::map_cep_error(cep)),
        }
    }
}

/// Turn-scoped Lancet server lifecycle; dropped when the turn task completes.
pub(crate) struct LancetTurnRuntime {
    server_task: tokio::task::JoinHandle<()>,
    shutdown: tokio::sync::watch::Sender<bool>,
}

impl LancetTurnRuntime {
    pub async fn shutdown(&mut self) {
        let _ = self.shutdown.send(true);
        let task = std::mem::replace(&mut self.server_task, tokio::spawn(async {}));
        let _ = task.await;
    }

    #[cfg(test)]
    pub(crate) fn dummy() -> Self {
        Self {
            server_task: tokio::spawn(async {}),
            shutdown: tokio::sync::watch::channel(false).0,
        }
    }
}

impl Drop for LancetTurnRuntime {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
    }
}

/// Resolved production turn dependencies assembled by the API layer.
pub(crate) struct ProductionTurnDeps {
    pub agent_loop: Arc<AgentLoop>,
    pub tool_definitions: Vec<lattice_llm::ToolDefinition>,
    pub runtime: LancetTurnRuntime,
    pub lancet_client: LancetClient,
    pub audit: Arc<CepAuditStore>,
    pub audit_context: Arc<RwLock<CepAuditContext>>,
    #[cfg(test)]
    pub build_metrics: Phase20BuildMetrics,
}

/// Build catalog-derived tool definitions filtered by the profile enabled list.
pub(crate) fn resolve_catalog_tool_definitions(
    enabled: &[String],
) -> Result<Vec<lattice_llm::ToolDefinition>, ApiError> {
    let inventory = to_tool_definitions(catalog());
    let resolved = resolve_enabled_tools(enabled, &inventory).map_err(|err| {
        ApiError::BadRequest(format!(
            "unknown enabled tools {:?}; available catalog tools: {:?}",
            err.unknown, err.available
        ))
    })?;
    Ok(resolved.definitions)
}

/// Build executor-facing Phase 21 policy from profile `[cep]` and the runtime project root.
pub(crate) fn phase21_policy_from_profile(
    cep: &CepSection,
    runtime_project_root: &Path,
) -> Result<Phase21PolicyConfig, ApiError> {
    let runtime_root = std::fs::canonicalize(runtime_project_root).map_err(|err| {
        ApiError::BadRequest(format!(
            "cannot canonicalize project root '{}': {err}",
            runtime_project_root.display()
        ))
    })?;

    let effective_root = match cep.project_root.as_ref().and_then(|s| s.root.as_deref()) {
        Some(profile_root) => {
            let resolved = resolve_profile_path(&runtime_root, profile_root)?;
            canonicalize_existing(&resolved, "cep.project_root.root")?
        }
        None => runtime_root.clone(),
    };

    if !effective_root.starts_with(&runtime_root) {
        return Err(ApiError::BadRequest(format!(
            "cep.project_root.root '{}' resolves outside runtime project root",
            effective_root.display()
        )));
    }

    let mut observation = ObservationPolicyConfig::defaults_for_root(&effective_root);
    if let Some(section) = cep.observation.as_ref() {
        apply_observation_profile(section, &mut observation, &effective_root)?;
    }

    let mut network = NetworkPolicyConfig::safe_defaults();
    if let Some(section) = cep.network.as_ref() {
        apply_network_profile(section, &mut network);
    }

    let budget = BudgetLimits {
        max_single_read_bytes: observation.max_single_read_bytes,
        max_turn_observation_bytes: observation.max_turn_observation_bytes,
        max_grep_matches: observation.max_grep_matches,
        max_list_entries: observation.max_list_entries,
        max_find_paths: observation.max_find_paths,
        max_todo_ops: observation.max_todo_ops,
    };

    Ok(Phase21PolicyConfig {
        project_root: effective_root,
        observation,
        network,
        budget,
        command: command_policy_from_profile(cep),
    })
}

fn apply_observation_profile(
    section: &CepObservationSection,
    observation: &mut ObservationPolicyConfig,
    effective_root: &Path,
) -> Result<(), ApiError> {
    if !section.allowed_roots.is_empty() {
        let mut roots = Vec::with_capacity(section.allowed_roots.len());
        for root in &section.allowed_roots {
            let resolved = resolve_profile_path(effective_root, root)?;
            let canonical = canonicalize_existing(&resolved, "cep.observation.allowed_roots")?;
            if !canonical.starts_with(effective_root) {
                return Err(ApiError::BadRequest(format!(
                    "cep.observation.allowed_roots entry '{}' is outside effective project root",
                    root
                )));
            }
            roots.push(canonical);
        }
        observation.allowed_roots = roots;
    }
    if let Some(value) = section.max_single_read_bytes {
        observation.max_single_read_bytes = value;
    }
    if let Some(value) = section.max_turn_observation_bytes {
        observation.max_turn_observation_bytes = value;
    }
    if let Some(value) = section.max_grep_matches {
        observation.max_grep_matches = value;
    }
    if let Some(value) = section.max_list_entries {
        observation.max_list_entries = value;
    }
    if let Some(value) = section.max_find_paths {
        observation.max_find_paths = value;
    }
    if let Some(value) = section.max_todo_ops {
        observation.max_todo_ops = value;
    }
    Ok(())
}

fn apply_network_profile(section: &CepNetworkSection, network: &mut NetworkPolicyConfig) {
    if let Some(value) = section.allow_public_https {
        network.allow_public_https = value;
    }
    if let Some(value) = section.allow_localhost {
        network.allow_localhost = value;
    }
    if !section.allowed_hosts.is_empty() {
        network.allowed_hosts = section.allowed_hosts.clone();
    }
    if let Some(value) = section.max_web_response_bytes {
        network.max_web_response_bytes = value;
    }
}

/// Build `CommandPolicy` from the profile `[cep.command]` section.
///
/// Missing `[cep.command]` falls back to `CommandPolicy::defaults()`.
/// Partial fields use defaults for the unspecified ones (D-23).
fn command_policy_from_profile(cep: &CepSection) -> CommandPolicy {
    let defaults = CommandPolicy::defaults();
    match cep.command.as_ref() {
        Some(section) => CommandPolicy {
            default_timeout_ms: section
                .default_timeout_ms
                .unwrap_or(defaults.default_timeout_ms),
            max_timeout_ms: section.max_timeout_ms.unwrap_or(defaults.max_timeout_ms),
            max_output_bytes: section
                .max_output_bytes
                .unwrap_or(defaults.max_output_bytes),
            allow_inline_shell: section
                .allow_inline_shell
                .unwrap_or(defaults.allow_inline_shell),
            allowed_diagnostic_commands: section
                .allowed_diagnostic_commands
                .clone()
                .unwrap_or(defaults.allowed_diagnostic_commands),
            denied_commands: section
                .denied_commands
                .clone()
                .unwrap_or(defaults.denied_commands),
            allowed_env: section.allowed_env.clone().unwrap_or(defaults.allowed_env),
            inherited_env: section
                .inherited_env
                .clone()
                .unwrap_or(defaults.inherited_env),
        },
        None => defaults,
    }
}

/// Build unified `PolicyConfig` from profile inline sections and optional external policy file.
///
/// Merge precedence: compiled defaults < profile inline `[cep.*]` < external policy file (D-32).
pub(crate) fn load_policy_config(
    cep: &CepSection,
    runtime_project_root: &Path,
) -> Result<PolicyConfig, ApiError> {
    let runtime_root = std::fs::canonicalize(runtime_project_root).map_err(|err| {
        ApiError::BadRequest(format!(
            "cannot canonicalize project root '{}': {err}",
            runtime_project_root.display()
        ))
    })?;

    let effective_root = match cep.project_root.as_ref().and_then(|s| s.root.as_deref()) {
        Some(profile_root) => {
            let resolved = resolve_profile_path(&runtime_root, profile_root)?;
            canonicalize_existing(&resolved, "cep.project_root.root")?
        }
        None => runtime_root.clone(),
    };

    if !effective_root.starts_with(&runtime_root) {
        return Err(ApiError::BadRequest(format!(
            "cep.project_root.root '{}' resolves outside runtime project root",
            effective_root.display()
        )));
    }

    let phase21 = phase21_policy_from_profile(cep, runtime_project_root)?;
    let mut config = PolicyConfig {
        capability_sources: Default::default(),
        methods: Default::default(),
        command: phase21.command,
        network: phase21.network,
        budget: phase21.budget,
        observation: phase21.observation,
        project_root: phase21.project_root,
    };

    if let Some(policy_section) = &cep.policy {
        if let Some(path) = &policy_section.path {
            if path.contains('\0') {
                return Err(ApiError::BadRequest(
                    "cep.policy.path must not contain NUL bytes".into(),
                ));
            }
            if Path::new(path).is_absolute() {
                return Err(ApiError::BadRequest(format!(
                    "cep.policy.path must be relative, got absolute path: '{path}'"
                )));
            }
            if path.contains("..") {
                return Err(ApiError::BadRequest(format!(
                    "cep.policy.path must not contain '..' components: '{path}'"
                )));
            }
            let resolved = runtime_root.join(path);
            let canonical = std::fs::canonicalize(&resolved).map_err(|err| {
                ApiError::BadRequest(format!(
                    "cannot canonicalize cep.policy.path '{}': {err}",
                    resolved.display()
                ))
            })?;
            if !canonical.starts_with(&runtime_root) {
                return Err(ApiError::BadRequest(format!(
                    "cep.policy.path '{}' resolves outside runtime project root",
                    canonical.display()
                )));
            }
            let external = load_external_policy(&canonical)?;
            config
                .merge_external(&external)
                .map_err(ApiError::BadRequest)?;
        }
    }

    Ok(config)
}

/// Load and deserialize an external TOML policy file.
fn load_external_policy(path: &Path) -> Result<ExternalPolicyFile, ApiError> {
    let contents = std::fs::read_to_string(path).map_err(|err| {
        ApiError::BadRequest(format!(
            "cannot read external policy file '{}': {err}",
            path.display()
        ))
    })?;
    toml::from_str(&contents).map_err(|err| {
        ApiError::BadRequest(format!(
            "cannot parse external policy file '{}': {err}",
            path.display()
        ))
    })
}

fn resolve_profile_path(base: &Path, path: &str) -> Result<PathBuf, ApiError> {
    let candidate = PathBuf::from(path);
    let resolved = if candidate.is_absolute() {
        candidate
    } else {
        base.join(candidate)
    };
    Ok(resolved)
}

fn canonicalize_existing(path: &Path, field: &str) -> Result<PathBuf, ApiError> {
    if !path.exists() {
        return Err(ApiError::BadRequest(format!(
            "{field} '{}' does not exist",
            path.display()
        )));
    }
    std::fs::canonicalize(path).map_err(|err| {
        ApiError::BadRequest(format!(
            "cannot canonicalize {field} '{}': {err}",
            path.display()
        ))
    })
}

fn production_base_envelope_from_policy(
    project_root: &str,
    enabled_tools: &[String],
    policy: &Phase21PolicyConfig,
) -> Result<BaseExecutionEnvelope, ApiError> {
    let mut base_envelope =
        BaseExecutionEnvelope::for_workspace_root(project_root, "phase21 turn observation");
    if enabled_tools.iter().any(|name| name == TodoWrite::NAME) {
        base_envelope = base_envelope.with_session_state();
    }
    if enabled_tools.iter().any(|name| name == Fetch::NAME) {
        for host in &policy.network.allowed_hosts {
            let host = host.trim();
            if !host.is_empty() {
                base_envelope = base_envelope.with_fetch_host(host);
            }
        }
    }
    base_envelope = base_envelope.with_read_audit();
    base_envelope
        .validate_no_write_or_command()
        .map_err(|err| ApiError::Internal(format!("invalid base envelope: {err}")))?;
    Ok(base_envelope)
}

/// Build the production turn `BaseExecutionEnvelope` from profile policy and enabled tools.
#[cfg(test)]
pub(crate) fn production_base_envelope(
    project_root: &str,
    enabled_tools: &[String],
    cep: Option<&CepSection>,
) -> Result<BaseExecutionEnvelope, ApiError> {
    let policy = match cep {
        Some(cep_section) => phase21_policy_from_profile(cep_section, Path::new(project_root))?,
        None => Phase21PolicyConfig::defaults_for_project_root(project_root),
    };
    production_base_envelope_from_policy(project_root, enabled_tools, &policy)
}

/// Persist an unsigned in-toto attestation for the turn after audit flush (D-09).
pub(crate) fn generate_and_store_attestation(
    audit: &CepAuditStore,
    ctx: &CepAuditContext,
    turn_id: &str,
) -> Result<(), ApiError> {
    if turn_id.is_empty() {
        return Ok(());
    }
    if audit
        .attestation_for_turn(turn_id)
        .map_err(|err| ApiError::Internal(format!("attestation lookup: {err}")))?
        .is_some()
    {
        return Ok(());
    }

    let filters = AuditQueryFilters {
        session_id: Some(ctx.session_id.clone()),
        turn_id: Some(turn_id.to_string()),
        ..Default::default()
    };
    let rows = audit
        .query_rows(&filters)
        .map_err(|err| ApiError::Internal(format!("attestation query rows: {err}")))?;
    if rows.is_empty() {
        return Ok(());
    }

    let runtime_version = if ctx.runtime_version.is_empty() {
        "lattice"
    } else {
        ctx.runtime_version.as_str()
    };
    let statement = build_in_toto_statement(turn_id, &rows, runtime_version);
    let statement_json = serde_json::to_string(&statement)
        .map_err(|err| ApiError::Internal(format!("attestation statement serialize: {err}")))?;
    let range_hash = source_audit_range_hash(&rows);
    let session_id = rows
        .first()
        .map(|r| r.session_id.clone())
        .unwrap_or_else(|| ctx.session_id.clone());
    let generated_at = rows
        .iter()
        .filter_map(|r| r.ended_at.as_deref())
        .next_back()
        .unwrap_or_else(|| rows[0].started_at.as_str())
        .to_string();

    audit
        .insert_attestation(&AttestationRecord {
            turn_id: turn_id.to_string(),
            session_id,
            statement_json,
            predicate_type: statement.predicate_type.clone(),
            generated_at,
            source_audit_range_or_hash: range_hash,
        })
        .map_err(|err| ApiError::Internal(format!("attestation insert: {err}")))?;

    Ok(())
}

/// Construct one turn-scoped Lancet runtime and inject it into a fresh AgentLoop.
pub(crate) fn build_production_turn_deps(
    project_root: &str,
    enabled_tools: &[String],
    llm: Arc<dyn lattice_agent::LlmClient>,
    in_memory_audit: bool,
    cep: Option<&CepSection>,
    model_id: &str,
    runtime_version: &str,
) -> Result<ProductionTurnDeps, ApiError> {
    let tool_definitions = resolve_catalog_tool_definitions(enabled_tools)?;

    // Canonicalize the project root up front so the base envelope's ReadPath /
    // WritePath capability patterns are built from the same canonical path the
    // guard compares targets against. Otherwise a symlinked root (e.g. macOS
    // `/var` -> `/private/var` temp dirs) yields a raw pattern that fails to
    // prefix-match the canonicalized target, denying legitimate reads. Best
    // effort: fall back to the raw path if canonicalization fails, matching the
    // tolerance in `Phase21PolicyConfig::defaults_for_project_root`.
    let canonical_root = std::fs::canonicalize(project_root)
        .ok()
        .and_then(|p| p.to_str().map(str::to_owned));
    let project_root = canonical_root.as_deref().unwrap_or(project_root);

    let phase21_policy = match cep {
        Some(cep_section) => phase21_policy_from_profile(cep_section, Path::new(project_root))?,
        None => Phase21PolicyConfig::defaults_for_project_root(project_root),
    };
    let base_envelope =
        production_base_envelope_from_policy(project_root, enabled_tools, &phase21_policy)?;

    #[cfg(test)]
    let build_metrics = Phase20BuildMetrics {
        adapter_instances: 1,
        transport_pairs: 1,
    };

    let (client, server) = LancetTransport::pair(256);
    let audit = if in_memory_audit {
        Arc::new(
            CepAuditStore::in_memory()
                .map_err(|err| ApiError::Internal(format!("audit store setup: {err}")))?,
        )
    } else {
        let audit_path = Path::new(project_root)
            .join(".lattice")
            .join("cep_audit.db");
        Arc::new(
            CepAuditStore::open(&audit_path)
                .map_err(|err| ApiError::Internal(format!("audit store setup: {err}")))?,
        )
    };
    let audit_context = Arc::new(RwLock::new(CepAuditContext {
        model_id: model_id.to_string(),
        runtime_version: runtime_version.to_string(),
        ..Default::default()
    }));
    let phase21 = Arc::new(Phase21TurnContext::from_policy(phase21_policy));
    let active_envelope = Arc::new(ActiveEnvelopeStore::new());

    let policy_config = match cep {
        Some(cep_section) => load_policy_config(cep_section, Path::new(project_root))?,
        None => PolicyConfig::defaults_for_project_root(std::path::PathBuf::from(project_root)),
    };
    let default_hook = Arc::new(DefaultPolicyHook::new(policy_config));

    let invariants_dir = cep
        .and_then(|section| section.invariants.as_ref())
        .and_then(|section| section.dir.as_deref());
    let invariant_index = invariants::load_invariant_index_from_dir(
        &invariants::resolve_invariants_dir(Path::new(project_root), invariants_dir),
    );

    let server_config = ServerRunConfig {
        audit: Arc::clone(&audit),
        audit_context: Arc::clone(&audit_context),
        base_envelope: base_envelope.clone(),
        phase21: Arc::clone(&phase21),
        active_envelope,
        call_audit_metadata: CallAuditMetadata::bridge_defaults(),
        production_audit_routing: true,
        policy_hook: default_hook.clone(),
        default_policy_hook: Some(default_hook),
        cep_mode: cep
            .map(|c| c.mode.clone())
            .unwrap_or(lancet_protocol::envelope::CepMode::Enforce),
        invariant_index,
    };

    let mut registry = MethodRegistry::new();
    register_production_method_registry(&mut registry).map_err(|err| {
        ApiError::Internal(format!("production method registry registration: {err:?}"))
    })?;

    let server_shutdown = server.shutdown_handle();
    let server_task = tokio::spawn(async move {
        let _ = server.run(registry, server_config).await;
    });

    let enabled_names: HashSet<String> = tool_definitions
        .iter()
        .map(|tool| tool.name.clone())
        .collect();
    let lancet_client = client.clone();
    let adapter = Arc::new(LancetExecutorAdapter::new(client, enabled_names));
    let agent_loop = Arc::new(AgentLoop::new(AgentLoopConfig {
        llm,
        tool_executor: adapter,
    }));

    Ok(ProductionTurnDeps {
        agent_loop,
        tool_definitions,
        runtime: LancetTurnRuntime {
            server_task,
            shutdown: server_shutdown,
        },
        lancet_client,
        audit,
        audit_context,
        #[cfg(test)]
        build_metrics,
    })
}

/// Map a CEP transport notification into an internal agent turn event.
pub(crate) fn map_cep_tool_event(event: ToolEvent) -> Option<AgentTurnEvent> {
    let params = event.params;
    // Envelope lifecycle events don't carry call_id; others do.
    match event.method.as_str() {
        "cep.envelope_declared" => Some(AgentTurnEvent::CepEnvelopeDeclared {
            envelope_id: string_param(&params, "envelope_id")?,
            envelope_hash: string_param(&params, "envelope_hash")?,
        }),
        "cep.envelope_superseded" => Some(AgentTurnEvent::CepEnvelopeSuperseded {
            old_envelope_id: string_param(&params, "old_envelope_id")?,
            new_envelope_id: string_param(&params, "new_envelope_id")?,
            reason: string_param(&params, "reason"),
        }),
        "cep.envelope_missing" => Some(AgentTurnEvent::CepEnvelopeMissing {
            call_id: params.get("call_id").and_then(|v| v.as_u64())?,
            method: string_param(&params, "method")?,
            required_for: string_param(&params, "required_for")
                .unwrap_or_else(|| "workspace_mutation".into()),
        }),
        "cep.verify_failed" => Some(AgentTurnEvent::CepVerifyFailed {
            call_id: params.get("call_id").and_then(|v| v.as_u64())?,
            envelope_id: string_param(&params, "envelope_id").unwrap_or_else(|| "unknown".into()),
            verify_exit_code: params
                .get("verify_exit_code")
                .and_then(|v| v.as_i64())
                .map(|v| v as i32),
            diff_retained: params
                .get("diff_retained")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
        }),
        "cep.replan_required" => Some(AgentTurnEvent::CepReplanRequired {
            call_id: params.get("call_id").and_then(|v| v.as_u64())?,
            method: string_param(&params, "method")?,
            out_of_scope_paths: params
                .get("out_of_scope_paths")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
        }),
        _ => {
            let call_id = params.get("call_id").and_then(|v| v.as_u64())?;
            match event.method.as_str() {
                "cep.call_started" => Some(AgentTurnEvent::CepCallStarted {
                    call_id,
                    method: string_param(&params, "method")?,
                    impact_kind: string_param(&params, "impact_kind")
                        .unwrap_or_else(|| "unknown".into()),
                    audit_id: params.get("audit_id").and_then(|v| v.as_i64())?,
                    semantic_stage: string_param(&params, "semantic_stage")?,
                    guard_stage: string_param(&params, "guard_stage")?,
                }),
                "cep.call_finished" => {
                    let method = string_param(&params, "method")?;
                    Some(AgentTurnEvent::CepCallFinished {
                        call_id,
                        method: method.clone(),
                        impact_kind: impact_kind_for_method(&method)
                            .as_ref()
                            .map(impact_kind_label)
                            .unwrap_or("unknown")
                            .to_string(),
                        audit_id: params.get("audit_id").and_then(|v| v.as_i64())?,
                        ok: params.get("ok").and_then(|v| v.as_bool())?,
                        semantic_stage: string_param(&params, "semantic_stage")?,
                        guard_stage: string_param(&params, "guard_stage")?,
                        error_code: params
                            .get("error_code")
                            .and_then(|v| v.as_str())
                            .map(str::to_string),
                    })
                }
                "cep.bridge_metadata" => Some(AgentTurnEvent::CepGuardVerdict {
                    call_id,
                    method: string_param(&params, "method")?,
                    semantic_stage: string_param(&params, "semantic_stage")?,
                    guard_stage: string_param(&params, "guard_stage")?,
                    policy_enforced: params
                        .get("policy_enforced")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                    semantic_status: params
                        .get("semantic_status")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                }),
                _ => None,
            }
        }
    }
}

fn string_param(params: &Value, key: &str) -> Option<String> {
    params.get(key).and_then(|v| v.as_str()).map(str::to_string)
}

fn adapter_wire_code(cep: &CepError) -> i32 {
    if let CepErrorData::Validation(data) = &cep.data {
        if data.reason == "invalid params" {
            return jsonrpc::INVALID_PARAMS;
        }
    }
    wire_code_for_error(cep)
}

fn retriability_label(value: &Retriability) -> String {
    match value {
        Retriability::Permanent => "permanent".to_string(),
        Retriability::Transient => "transient".to_string(),
        Retriability::RequiresReplan => "requires_replan".to_string(),
        Retriability::Unknown => "unknown".to_string(),
    }
}

fn schema_mismatch_for_method<M: CepMethod>(err: serde_json::Error) -> CepError {
    let field = extract_json_field_path(&err).unwrap_or_else(|| "params".to_string());
    let mut cep = invalid_params(format!("invalid params for {}: {err}", M::NAME));
    if let CepErrorData::Validation(data) = &mut cep.data {
        data.field = field;
    }
    cep
}

fn extract_json_field_path(err: &serde_json::Error) -> Option<String> {
    let message = err.to_string();
    if let Some(start) = message.find("missing field `") {
        let rest = &message[start + "missing field `".len()..];
        if let Some(end) = rest.find('`') {
            return Some(rest[..end].to_string());
        }
    }
    if let Some(start) = message.find("invalid type: ") {
        if let Some(field_start) = message[..start].rfind('`') {
            let prefix = &message[field_start + 1..];
            if let Some(field_end) = prefix.find('`') {
                return Some(prefix[..field_end].to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod phase21_profile_policy {
    use super::*;
    use lattice_profile::CepSection;
    use tempfile::TempDir;

    #[test]
    fn phase21_profile_policy_defaults_when_cep_absent() {
        let dir = TempDir::new().unwrap();
        let policy =
            phase21_policy_from_profile(&CepSection::default(), dir.path()).expect("defaults");
        assert_eq!(policy.project_root, dir.path().canonicalize().unwrap());
        assert_eq!(
            policy.budget.max_turn_observation_bytes,
            policy.observation.max_turn_observation_bytes
        );
    }

    #[test]
    fn phase21_profile_policy_canonicalizes_observation_roots() {
        let dir = TempDir::new().unwrap();
        let sub = dir.path().join("src");
        std::fs::create_dir_all(&sub).unwrap();
        let cep = CepSection {
            observation: Some(CepObservationSection {
                allowed_roots: vec!["src".into()],
                ..Default::default()
            }),
            ..Default::default()
        };
        let policy = phase21_policy_from_profile(&cep, dir.path()).expect("policy");
        assert_eq!(
            policy.observation.allowed_roots,
            vec![sub.canonicalize().unwrap()]
        );
    }

    #[test]
    fn phase21_profile_policy_rejects_roots_outside_project() {
        let dir = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let cep = CepSection {
            observation: Some(CepObservationSection {
                allowed_roots: vec![outside.path().to_string_lossy().into_owned()],
                ..Default::default()
            }),
            ..Default::default()
        };
        let err = phase21_policy_from_profile(&cep, dir.path()).unwrap_err();
        assert!(matches!(err, ApiError::BadRequest(_)));
    }

    #[test]
    fn phase21_profile_policy_network_max_web_response_bytes() {
        let dir = TempDir::new().unwrap();
        let cep = CepSection {
            network: Some(CepNetworkSection {
                max_web_response_bytes: Some(2048),
                ..Default::default()
            }),
            ..Default::default()
        };
        let policy = phase21_policy_from_profile(&cep, dir.path()).expect("policy");
        assert_eq!(policy.network.max_web_response_bytes, 2048);
    }

    #[test]
    fn command_policy_from_profile_defaults_when_absent() {
        let cep = CepSection::default();
        let policy = command_policy_from_profile(&cep);
        let defaults = CommandPolicy::defaults();
        assert_eq!(policy.default_timeout_ms, defaults.default_timeout_ms);
        assert_eq!(policy.max_timeout_ms, defaults.max_timeout_ms);
        assert_eq!(policy.allow_inline_shell, defaults.allow_inline_shell);
    }

    #[test]
    fn command_policy_from_profile_applies_custom_values() {
        let cep = CepSection {
            command: Some(lattice_profile::CepCommandSection {
                default_timeout_ms: Some(30_000),
                max_timeout_ms: Some(60_000),
                max_output_bytes: Some(50_000),
                allow_inline_shell: Some(true),
                allowed_diagnostic_commands: Some(vec![vec!["mytool".into(), "info".into()]]),
                denied_commands: Some(vec![vec!["rm".into(), "-rf".into()]]),
                allowed_env: Some(vec!["MY_VAR".into()]),
                inherited_env: Some(vec!["PATH".into(), "HOME".into()]),
            }),
            ..Default::default()
        };
        let policy = command_policy_from_profile(&cep);
        assert_eq!(policy.default_timeout_ms, 30_000);
        assert_eq!(policy.max_timeout_ms, 60_000);
        assert_eq!(policy.max_output_bytes, 50_000);
        assert!(policy.allow_inline_shell);
        assert_eq!(
            policy.allowed_diagnostic_commands,
            vec![vec!["mytool".to_string(), "info".to_string()]]
        );
        assert_eq!(
            policy.denied_commands,
            vec![vec!["rm".to_string(), "-rf".to_string()]]
        );
        assert_eq!(policy.allowed_env, vec!["MY_VAR".to_string()]);
        assert_eq!(
            policy.inherited_env,
            vec!["PATH".to_string(), "HOME".to_string()]
        );
    }

    #[test]
    fn command_policy_from_profile_partial_uses_defaults_for_missing() {
        let cep = CepSection {
            command: Some(lattice_profile::CepCommandSection {
                default_timeout_ms: Some(10_000),
                ..Default::default()
            }),
            ..Default::default()
        };
        let policy = command_policy_from_profile(&cep);
        let defaults = CommandPolicy::defaults();
        assert_eq!(policy.default_timeout_ms, 10_000);
        assert_eq!(policy.max_timeout_ms, defaults.max_timeout_ms);
        assert_eq!(policy.allow_inline_shell, defaults.allow_inline_shell);
        assert_eq!(
            policy.allowed_diagnostic_commands,
            defaults.allowed_diagnostic_commands
        );
    }

    #[test]
    fn phase21_policy_from_profile_uses_command_policy_from_profile() {
        let dir = TempDir::new().unwrap();
        let cep = CepSection {
            command: Some(lattice_profile::CepCommandSection {
                default_timeout_ms: Some(45_000),
                ..Default::default()
            }),
            ..Default::default()
        };
        let policy = phase21_policy_from_profile(&cep, dir.path()).expect("policy");
        assert_eq!(policy.command.default_timeout_ms, 45_000);
    }

    #[test]
    fn phase21_policy_from_profile_older_profile_uses_command_defaults() {
        let dir = TempDir::new().unwrap();
        let cep = CepSection::default();
        let policy = phase21_policy_from_profile(&cep, dir.path()).expect("policy");
        assert_eq!(policy.command, CommandPolicy::defaults());
    }
}

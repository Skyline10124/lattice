// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Turn-scoped Cep runtime composition and the CEP client-backed executor adapter.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use lancet_authority::policy::default::{DefaultPolicyHook, ExternalPolicyFile, PolicyConfig};
use lancet_authority::{
    ActiveEnvelopeStore, BudgetLimits, CepPolicyConfig, CepTurnContext, CommandPolicy,
    NetworkPolicyConfig, ObservationPolicyConfig,
};
use lancet_catalog::catalog;
use lancet_catalog::methods::audit::{AuditObserve, AuditReplay};
use lancet_catalog::methods::command::Run;
use lancet_catalog::methods::envelope::Envelope;
use lancet_catalog::methods::external::Fetch;
use lancet_catalog::methods::session::TodoWrite;
use lancet_catalog::methods::workspace::{Create, ObserveWorkspace, Patch};
use lancet_catalog::{CatalogMethod, CepMethod};
use lancet_invariants as invariants;
use lancet_native::enforcement::isolation::{
    CommandIsolationLandlockBehavior, CommandIsolationMode, CommandIsolationRuntimeConfig,
    CommandIsolationUnsupportedHostBehavior,
};
use lancet_native::{
    build_in_toto_statement, impact_kind_for_method, impact_kind_label,
    register_production_method_registry, source_audit_range_hash, to_tool_definitions,
    wire_code_for_error, AttestationRecord, AuditQueryFilters, CepAuditContext, CepAuditStore,
    CepClient, CepTransport, MethodRegistry, ServerRunConfig,
};
use lancet_protocol::envelope::BaseExecutionEnvelope;
use lancet_protocol::error::{
    invalid_params, jsonrpc, method_not_found, parse_error, CepErrorData,
};
use lancet_protocol::{CepError, Retriability, ToolEvent};
use lattice_agent::{
    AgentLoop, AgentLoopConfig, AgentTurnEvent, ToolCallExecutor, ToolExecutionError,
};
use lattice_llm::ToolCall;
use lattice_profile::{
    LancetIsolationMode, LancetLandlockBehavior, LancetNetworkSection, LancetObservationSection,
    LancetSection, LancetUnsupportedHostBehavior,
};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::RwLock;

use super::enabled_tools::resolve_enabled_tools;
use crate::session::ApiError;

/// Per-build construction metrics (test-only; each production build is exactly one pair + adapter).
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CepRuntimeBuildMetrics {
    pub adapter_instances: usize,
    pub transport_pairs: usize,
}

/// Client-backed adapter that maps LLM-shape tool calls into typed CEP invocations.
pub struct CepExecutorAdapter {
    client: CepClient,
    catalog_names: HashSet<String>,
    enabled_names: HashSet<String>,
}

impl CepExecutorAdapter {
    pub fn new(
        client: CepClient,
        enabled_names: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        let catalog_names: HashSet<String> = catalog().iter().map(|d| d.name.to_string()).collect();
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
            ObserveWorkspace::NAME => self.call_typed::<ObserveWorkspace>(params).await,
            Create::NAME => self.call_typed::<Create>(params).await,
            Fetch::NAME => self.call_typed::<Fetch>(params).await,
            TodoWrite::NAME => self.call_typed::<TodoWrite>(params).await,
            Run::NAME => self.call_typed::<Run>(params).await,
            Patch::NAME => self.call_typed::<Patch>(params).await,
            Envelope::NAME => self.call_typed::<Envelope>(params).await,
            AuditObserve::NAME => self.call_typed::<AuditObserve>(params).await,
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
impl ToolCallExecutor for CepExecutorAdapter {
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

/// Turn-scoped Cep server lifecycle; dropped when the turn task completes.
pub(crate) struct CepTurnRuntime {
    server_task: tokio::task::JoinHandle<()>,
    shutdown: tokio::sync::watch::Sender<bool>,
}

impl CepTurnRuntime {
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

impl Drop for CepTurnRuntime {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
    }
}

/// Resolved production turn dependencies assembled by the API layer.
pub(crate) struct ProductionTurnDeps {
    pub agent_loop: Arc<AgentLoop>,
    pub tool_definitions: Vec<lattice_llm::ToolDefinition>,
    pub runtime: CepTurnRuntime,
    pub cep_client: CepClient,
    pub audit: Arc<CepAuditStore>,
    pub audit_context: Arc<RwLock<CepAuditContext>>,
    #[cfg(test)]
    pub build_metrics: CepRuntimeBuildMetrics,
}

/// Build catalog-derived tool definitions filtered by the profile enabled list.
pub(crate) fn resolve_catalog_tool_definitions(
    enabled: &[String],
) -> Result<Vec<lattice_llm::ToolDefinition>, ApiError> {
    let inventory = to_tool_definitions(catalog());
    let definitions = resolve_enabled_tools(enabled, &inventory).map_err(|err| {
        ApiError::BadRequest(format!(
            "unknown enabled tools {:?}; available catalog tools: {:?}",
            err.unknown, err.available
        ))
    })?;
    Ok(definitions)
}

/// Build executor-facing CEP policy from profile `[lancet]` and the runtime project root.
pub(crate) fn cep_policy_from_profile(
    lancet: &LancetSection,
    runtime_project_root: &Path,
) -> Result<CepPolicyConfig, ApiError> {
    let runtime_root = std::fs::canonicalize(runtime_project_root).map_err(|err| {
        ApiError::BadRequest(format!(
            "cannot canonicalize project root '{}': {err}",
            runtime_project_root.display()
        ))
    })?;

    let effective_root = match lancet.project_root.as_ref().and_then(|s| s.root.as_deref()) {
        Some(profile_root) => {
            let resolved = resolve_profile_path(&runtime_root, profile_root)?;
            canonicalize_existing(&resolved, "lancet.project_root.root")?
        }
        None => runtime_root.clone(),
    };

    if !effective_root.starts_with(&runtime_root) {
        return Err(ApiError::BadRequest(format!(
            "lancet.project_root.root '{}' resolves outside runtime project root",
            effective_root.display()
        )));
    }

    let mut observation = ObservationPolicyConfig::defaults_for_root(&effective_root);
    if let Some(section) = lancet.observation.as_ref() {
        apply_observation_profile(section, &mut observation, &effective_root)?;
    }

    let mut network = NetworkPolicyConfig::safe_defaults();
    if let Some(section) = lancet.network.as_ref() {
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

    Ok(CepPolicyConfig {
        project_root: effective_root,
        observation,
        network,
        budget,
        command: command_policy_from_profile(lancet),
        denied_effect_kinds: lancet
            .effects
            .as_ref()
            .map(|effects| effects.deny.iter().cloned().collect())
            .unwrap_or_default(),
    })
}

fn apply_observation_profile(
    section: &LancetObservationSection,
    observation: &mut ObservationPolicyConfig,
    effective_root: &Path,
) -> Result<(), ApiError> {
    if !section.allowed_roots.is_empty() {
        let mut roots = Vec::with_capacity(section.allowed_roots.len());
        for root in &section.allowed_roots {
            let resolved = resolve_profile_path(effective_root, root)?;
            let canonical = canonicalize_existing(&resolved, "lancet.observation.allowed_roots")?;
            if !canonical.starts_with(effective_root) {
                return Err(ApiError::BadRequest(format!(
                    "lancet.observation.allowed_roots entry '{}' is outside effective project root",
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

fn apply_network_profile(section: &LancetNetworkSection, network: &mut NetworkPolicyConfig) {
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

/// Build `CommandPolicy` from the profile `[lancet.command]` section.
///
/// Missing `[lancet.command]` falls back to `CommandPolicy::defaults()`.
/// Partial fields use defaults for the unspecified ones.
fn command_policy_from_profile(lancet: &LancetSection) -> CommandPolicy {
    let defaults = CommandPolicy::defaults();
    match lancet.command.as_ref() {
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

fn command_isolation_config_from_profile(
    lancet: &LancetSection,
    runtime_project_root: &Path,
) -> Result<CommandIsolationRuntimeConfig, ApiError> {
    let Some(section) = lancet.isolation.as_ref().and_then(|s| s.command.as_ref()) else {
        return Ok(CommandIsolationRuntimeConfig::default());
    };

    let runtime_root = std::fs::canonicalize(runtime_project_root).map_err(|err| {
        ApiError::BadRequest(format!(
            "cannot canonicalize project root '{}': {err}",
            runtime_project_root.display()
        ))
    })?;

    Ok(CommandIsolationRuntimeConfig {
        mode: command_isolation_mode_from_profile(section.mode),
        required_boundaries: section.required_boundaries.clone(),
        optional_boundaries: section.optional_boundaries.clone(),
        read_only_toolchain_paths: section
            .read_only_toolchain_paths
            .iter()
            .map(|path| resolve_runtime_config_path(&runtime_root, path))
            .collect(),
        scratch_paths: section
            .scratch_paths
            .iter()
            .map(|path| resolve_runtime_config_path(&runtime_root, path))
            .collect(),
        landlock: command_landlock_behavior_from_profile(section.landlock),
        unsupported_host: command_unsupported_host_behavior_from_profile(
            section.mode,
            section.unsupported_host,
        ),
        host_probe: None,
    })
}

fn command_isolation_mode_from_profile(mode: LancetIsolationMode) -> CommandIsolationMode {
    match mode {
        LancetIsolationMode::Enforce => CommandIsolationMode::Enforce,
        LancetIsolationMode::Audit => CommandIsolationMode::Audit,
        LancetIsolationMode::Off => CommandIsolationMode::Off,
    }
}

fn command_landlock_behavior_from_profile(
    behavior: LancetLandlockBehavior,
) -> CommandIsolationLandlockBehavior {
    match behavior {
        LancetLandlockBehavior::Auto => CommandIsolationLandlockBehavior::Auto,
        LancetLandlockBehavior::Off => CommandIsolationLandlockBehavior::Off,
    }
}

fn command_unsupported_host_behavior_from_profile(
    mode: LancetIsolationMode,
    behavior: LancetUnsupportedHostBehavior,
) -> CommandIsolationUnsupportedHostBehavior {
    match behavior {
        LancetUnsupportedHostBehavior::FailClosed => {
            CommandIsolationUnsupportedHostBehavior::FailClosed
        }
        LancetUnsupportedHostBehavior::AllowUnbounded => {
            CommandIsolationUnsupportedHostBehavior::AllowUnbounded
        }
        LancetUnsupportedHostBehavior::NotRequested => {
            CommandIsolationUnsupportedHostBehavior::NotRequested
        }
        LancetUnsupportedHostBehavior::ModeDefault => match mode {
            LancetIsolationMode::Enforce => CommandIsolationUnsupportedHostBehavior::FailClosed,
            LancetIsolationMode::Audit => CommandIsolationUnsupportedHostBehavior::AllowUnbounded,
            LancetIsolationMode::Off => CommandIsolationUnsupportedHostBehavior::NotRequested,
        },
    }
}

fn resolve_runtime_config_path(runtime_root: &Path, path: &str) -> PathBuf {
    let candidate = PathBuf::from(path);
    if candidate.is_absolute() {
        candidate
    } else {
        runtime_root.join(candidate)
    }
}

/// Build unified `PolicyConfig` from profile inline sections and optional external policy file.
///
/// Merge precedence: compiled defaults < profile inline `[lancet.*]` < external policy file.
fn cep_policy_from_policy_config(config: &PolicyConfig) -> CepPolicyConfig {
    CepPolicyConfig {
        project_root: config.project_root.clone(),
        observation: config.observation.clone(),
        network: config.network.clone(),
        budget: config.budget.clone(),
        command: config.command.clone(),
        denied_effect_kinds: config.denied_effect_kinds.clone(),
    }
}

pub(crate) fn load_policy_config(
    lancet: &LancetSection,
    runtime_project_root: &Path,
) -> Result<PolicyConfig, ApiError> {
    let runtime_root = std::fs::canonicalize(runtime_project_root).map_err(|err| {
        ApiError::BadRequest(format!(
            "cannot canonicalize project root '{}': {err}",
            runtime_project_root.display()
        ))
    })?;

    let effective_root = match lancet.project_root.as_ref().and_then(|s| s.root.as_deref()) {
        Some(profile_root) => {
            let resolved = resolve_profile_path(&runtime_root, profile_root)?;
            canonicalize_existing(&resolved, "lancet.project_root.root")?
        }
        None => runtime_root.clone(),
    };

    if !effective_root.starts_with(&runtime_root) {
        return Err(ApiError::BadRequest(format!(
            "lancet.project_root.root '{}' resolves outside runtime project root",
            effective_root.display()
        )));
    }

    let cep_context = cep_policy_from_profile(lancet, runtime_project_root)?;
    let mut config = PolicyConfig {
        capability_sources: Default::default(),
        methods: Default::default(),
        command: cep_context.command,
        network: cep_context.network,
        budget: cep_context.budget,
        observation: cep_context.observation,
        project_root: cep_context.project_root,
        denied_effect_kinds: cep_context.denied_effect_kinds,
    };

    if let Some(policy_section) = &lancet.policy {
        if let Some(path) = &policy_section.path {
            if path.contains('\0') {
                return Err(ApiError::BadRequest(
                    "lancet.policy.path must not contain NUL bytes".into(),
                ));
            }
            if Path::new(path).is_absolute() {
                return Err(ApiError::BadRequest(format!(
                    "lancet.policy.path must be relative, got absolute path: '{path}'"
                )));
            }
            if path.contains("..") {
                return Err(ApiError::BadRequest(format!(
                    "lancet.policy.path must not contain '..' components: '{path}'"
                )));
            }
            let resolved = runtime_root.join(path);
            let canonical = std::fs::canonicalize(&resolved).map_err(|err| {
                ApiError::BadRequest(format!(
                    "cannot canonicalize lancet.policy.path '{}': {err}",
                    resolved.display()
                ))
            })?;
            if !canonical.starts_with(&runtime_root) {
                return Err(ApiError::BadRequest(format!(
                    "lancet.policy.path '{}' resolves outside runtime project root",
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
    policy: &CepPolicyConfig,
) -> Result<BaseExecutionEnvelope, ApiError> {
    let mut base_envelope =
        BaseExecutionEnvelope::for_workspace_root(project_root, "cep_context turn observation");
    if enabled_tools
        .iter()
        .any(|name| name == TodoWrite::NAME || name == Envelope::NAME)
    {
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
    lancet: Option<&LancetSection>,
) -> Result<BaseExecutionEnvelope, ApiError> {
    let policy = match lancet {
        Some(lancet_section) => cep_policy_from_profile(lancet_section, Path::new(project_root))?,
        None => CepPolicyConfig::defaults_for_project_root(project_root),
    };
    production_base_envelope_from_policy(project_root, enabled_tools, &policy)
}

/// Persist an unsigned in-toto attestation for the turn after audit flush.
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

/// Construct one turn-scoped Cep runtime and inject it into a fresh AgentLoop.
pub(crate) fn build_production_turn_deps(
    project_root: &str,
    enabled_tools: &[String],
    llm: Arc<dyn lattice_agent::LlmClient>,
    in_memory_audit: bool,
    lancet: Option<&LancetSection>,
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
    // tolerance in `CepPolicyConfig::defaults_for_project_root`.
    let canonical_root = std::fs::canonicalize(project_root)
        .ok()
        .and_then(|p| p.to_str().map(str::to_owned));
    let project_root = canonical_root.as_deref().unwrap_or(project_root);

    let policy_config = match lancet {
        Some(lancet_section) => load_policy_config(lancet_section, Path::new(project_root))?,
        None => PolicyConfig::defaults_for_project_root(std::path::PathBuf::from(project_root)),
    };
    let command_isolation = match lancet {
        Some(lancet_section) => {
            command_isolation_config_from_profile(lancet_section, Path::new(project_root))?
        }
        None => CommandIsolationRuntimeConfig::default(),
    };
    let cep_policy = cep_policy_from_policy_config(&policy_config);
    let base_envelope =
        production_base_envelope_from_policy(project_root, enabled_tools, &cep_policy)?;

    #[cfg(test)]
    let build_metrics = CepRuntimeBuildMetrics {
        adapter_instances: 1,
        transport_pairs: 1,
    };

    let (client, server) = CepTransport::pair(256);
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
    let cep_context = Arc::new(CepTurnContext::from_policy(cep_policy));
    let active_envelope = Arc::new(ActiveEnvelopeStore::new());

    let default_hook = Arc::new(DefaultPolicyHook::new(policy_config));

    let invariants_dir = lancet
        .and_then(|section| section.invariants.as_ref())
        .and_then(|section| section.dir.as_deref());
    let invariant_index = invariants::load_invariant_index_from_dir(
        &invariants::resolve_invariants_dir(Path::new(project_root), invariants_dir),
    );

    let server_config = ServerRunConfig {
        audit: Arc::clone(&audit),
        audit_context: Arc::clone(&audit_context),
        base_envelope: base_envelope.clone(),
        cep_context: Arc::clone(&cep_context),
        active_envelope,
        policy_hook: default_hook.clone(),
        default_policy_hook: Some(default_hook),
        cep_mode: lancet
            .map(|c| c.mode.clone())
            .unwrap_or(lancet_protocol::envelope::CepMode::Enforce),
        command_isolation,
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
    let cep_client = client.clone();
    let adapter = Arc::new(CepExecutorAdapter::new(client, enabled_names));
    let agent_loop = Arc::new(AgentLoop::new(AgentLoopConfig {
        llm,
        tool_executor: adapter,
    }));

    Ok(ProductionTurnDeps {
        agent_loop,
        tool_definitions,
        runtime: CepTurnRuntime {
            server_task,
            shutdown: server_shutdown,
        },
        cep_client,
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
mod cep_context_profile_policy {
    use super::*;
    use lancet_catalog::methods::command::RunParams;
    use lancet_catalog::methods::envelope::{
        DeclareParams, Envelope, EnvelopeParams, EnvelopeResult,
    };
    use lancet_protocol::envelope::{
        Capability, CepMode, Constraints, ExecutionEnvelope, ImpactKind, Intent, Scope,
    };
    use lattice_profile::LancetSection;
    use std::collections::{BTreeMap, BTreeSet};
    use tempfile::TempDir;

    #[test]
    fn cep_context_profile_policy_defaults_when_cep_absent() {
        let dir = TempDir::new().unwrap();
        let policy =
            cep_policy_from_profile(&LancetSection::default(), dir.path()).expect("defaults");
        assert_eq!(policy.project_root, dir.path().canonicalize().unwrap());
        assert_eq!(
            policy.budget.max_turn_observation_bytes,
            policy.observation.max_turn_observation_bytes
        );
    }

    #[test]
    fn cep_context_profile_policy_canonicalizes_observation_roots() {
        let dir = TempDir::new().unwrap();
        let sub = dir.path().join("src");
        std::fs::create_dir_all(&sub).unwrap();
        let cep = LancetSection {
            observation: Some(LancetObservationSection {
                allowed_roots: vec!["src".into()],
                ..Default::default()
            }),
            ..Default::default()
        };
        let policy = cep_policy_from_profile(&cep, dir.path()).expect("policy");
        assert_eq!(
            policy.observation.allowed_roots,
            vec![sub.canonicalize().unwrap()]
        );
    }

    #[test]
    fn cep_context_profile_policy_rejects_roots_outside_project() {
        let dir = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let cep = LancetSection {
            observation: Some(LancetObservationSection {
                allowed_roots: vec![outside.path().to_string_lossy().into_owned()],
                ..Default::default()
            }),
            ..Default::default()
        };
        let err = cep_policy_from_profile(&cep, dir.path()).unwrap_err();
        assert!(matches!(err, ApiError::BadRequest(_)));
    }

    #[test]
    fn cep_context_profile_policy_network_max_web_response_bytes() {
        let dir = TempDir::new().unwrap();
        let cep = LancetSection {
            network: Some(LancetNetworkSection {
                max_web_response_bytes: Some(2048),
                ..Default::default()
            }),
            ..Default::default()
        };
        let policy = cep_policy_from_profile(&cep, dir.path()).expect("policy");
        assert_eq!(policy.network.max_web_response_bytes, 2048);
    }

    #[test]
    fn command_policy_from_profile_defaults_when_absent() {
        let cep = LancetSection::default();
        let policy = command_policy_from_profile(&cep);
        let defaults = CommandPolicy::defaults();
        assert_eq!(policy.default_timeout_ms, defaults.default_timeout_ms);
        assert_eq!(policy.max_timeout_ms, defaults.max_timeout_ms);
        assert_eq!(policy.allow_inline_shell, defaults.allow_inline_shell);
    }

    #[test]
    fn command_policy_from_profile_applies_custom_values() {
        let cep = LancetSection {
            command: Some(lattice_profile::LancetCommandSection {
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
        let cep = LancetSection {
            command: Some(lattice_profile::LancetCommandSection {
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
    fn cep_policy_from_profile_uses_command_policy_from_profile() {
        let dir = TempDir::new().unwrap();
        let cep = LancetSection {
            command: Some(lattice_profile::LancetCommandSection {
                default_timeout_ms: Some(45_000),
                ..Default::default()
            }),
            ..Default::default()
        };
        let policy = cep_policy_from_profile(&cep, dir.path()).expect("policy");
        assert_eq!(policy.command.default_timeout_ms, 45_000);
    }

    #[test]
    fn cep_policy_from_profile_older_profile_uses_command_defaults() {
        let dir = TempDir::new().unwrap();
        let cep = LancetSection::default();
        let policy = cep_policy_from_profile(&cep, dir.path()).expect("policy");
        assert_eq!(policy.command, CommandPolicy::defaults());
    }

    #[test]
    fn cep_mode_integration_profile_command_isolation_maps_independently_from_lancet_mode_off() {
        let dir = TempDir::new().unwrap();
        let cep = LancetSection {
            mode: CepMode::Off,
            isolation: Some(lattice_profile::LancetIsolationSection {
                command: Some(lattice_profile::LancetCommandIsolationSection {
                    mode: lattice_profile::LancetIsolationMode::Enforce,
                    required_boundaries: vec![
                        lancet_protocol::BoundaryDimension::Network,
                        lancet_protocol::BoundaryDimension::Landlock,
                    ],
                    optional_boundaries: vec![lancet_protocol::BoundaryDimension::Filesystem],
                    read_only_toolchain_paths: vec!["/usr/bin".into()],
                    scratch_paths: vec!["build/tmp".into()],
                    landlock: lattice_profile::LancetLandlockBehavior::Auto,
                    unsupported_host: lattice_profile::LancetUnsupportedHostBehavior::ModeDefault,
                }),
                extensions: BTreeMap::new(),
            }),
            ..Default::default()
        };

        let config = command_isolation_config_from_profile(&cep, dir.path())
            .expect("command isolation runtime config");

        assert_eq!(
            config.mode,
            lancet_native::enforcement::isolation::CommandIsolationMode::Enforce
        );
        assert_eq!(
            config.required_boundaries,
            vec![
                lancet_protocol::BoundaryDimension::Network,
                lancet_protocol::BoundaryDimension::Landlock,
            ]
        );
        assert_eq!(
            config.optional_boundaries,
            vec![lancet_protocol::BoundaryDimension::Filesystem]
        );
        assert_eq!(
            config.read_only_toolchain_paths,
            vec![PathBuf::from("/usr/bin")]
        );
        assert_eq!(config.scratch_paths, vec![dir.path().join("build/tmp")]);
        assert_eq!(
            config.unsupported_host,
            lancet_native::enforcement::isolation::CommandIsolationUnsupportedHostBehavior::FailClosed
        );
    }

    #[test]
    fn cep_policy_from_policy_config_preserves_external_command_overrides() {
        let dir = TempDir::new().unwrap();
        let external_path = dir.path().join("policy.toml");
        std::fs::write(
            &external_path,
            r#"
[command]
default_timeout_ms = 1000
max_timeout_ms = 1000
max_output_bytes = 64
allowed_env = []
inherited_env = []
"#,
        )
        .unwrap();

        let cep = LancetSection {
            policy: Some(lattice_profile::LancetPolicySection {
                path: Some("policy.toml".into()),
            }),
            ..Default::default()
        };
        let policy_config = load_policy_config(&cep, dir.path()).expect("external policy");
        let cep_policy = cep_policy_from_policy_config(&policy_config);

        assert_eq!(cep_policy.command.max_timeout_ms, 1000);
        assert_eq!(cep_policy.command.max_output_bytes, 64);
        assert!(cep_policy.command.allowed_env.is_empty());
        assert!(cep_policy.command.inherited_env.is_empty());
    }

    #[tokio::test]
    async fn command_run_enforces_external_timeout_policy() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("policy.toml"),
            r#"
[command]
max_timeout_ms = 1
allowed_diagnostic_commands = [["echo"]]
"#,
        )
        .unwrap();

        let cep = LancetSection {
            mode: CepMode::Off,
            policy: Some(lattice_profile::LancetPolicySection {
                path: Some("policy.toml".into()),
            }),
            ..Default::default()
        };
        let enabled = vec![Run::NAME.to_string(), Envelope::NAME.to_string()];
        let fake_llm = Arc::new(lattice_agent::testing::FakeStreamingLlmClient::builder().build());
        let mut deps = build_production_turn_deps(
            dir.path().to_str().unwrap(),
            &enabled,
            fake_llm,
            true,
            Some(&cep),
            "test-model",
            "test-runtime",
        )
        .expect("turn deps");

        deps.cep_client
            .call::<Envelope>(EnvelopeParams::Declare(DeclareParams {
                envelope: ExecutionEnvelope {
                    intent: Intent {
                        description: "external policy command timeout test".into(),
                    },
                    capabilities: BTreeSet::from([Capability::RunCommand {
                        argv_allowlist: vec!["echo".into()],
                    }]),
                    scope: Scope::default(),
                    impact: vec![],
                    constraints: Constraints::default(),
                    verify: None,
                    on_failure: None,
                    extensions: serde_json::Map::new(),
                },
                supersede: false,
                reason: Some("external policy command timeout test".into()),
            }))
            .await
            .map(|result| match result {
                EnvelopeResult::Declare(result) => result,
                EnvelopeResult::DeriveIntent(_) => panic!("unexpected derive_intent result"),
            })
            .expect("declare RunCommand envelope");

        let params = RunParams {
            argv: vec!["echo".into(), "external-policy-timeout".into()],
            cwd: None,
            env: BTreeMap::new(),
            timeout_ms: Some(150_000),
            declared_impact: Some(ImpactKind::CommandExecution),
        };
        let err = deps
            .cep_client
            .call::<Run>(params)
            .await
            .expect_err("external max_timeout_ms must be enforced");
        assert!(
            err.message.contains("timeout") || err.message.contains("exceeds"),
            "unexpected error: {}",
            err.message
        );

        deps.runtime.shutdown().await;
    }
}

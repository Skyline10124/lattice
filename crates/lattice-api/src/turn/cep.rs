// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! LATTICE↔LANCET turn seam: the CEP client-backed executor adapter, the
//! `lattice_profile` → LANCET policy translation, and turn-runtime assembly over
//! [`lancet_api`].
//!
//! Runtime composition itself lives in `lancet-api`. This module owns only the
//! pieces that depend on LATTICE types: the [`ToolCallExecutor`] bridge, the
//! profile-to-policy translation, and CEP-event → agent-event mapping.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use lancet_api::{AuditBacking, LancetError, LancetRuntime, LancetRuntimeConfig};
use lancet_authority::policy::default::{ExternalPolicyFile, PolicyConfig, WorkspacePolicy};
use lancet_authority::{
    BudgetLimits, CepPolicyConfig, CommandPolicy, NetworkPolicyConfig, ObservationPolicyConfig,
};
use lancet_catalog::catalog;
use lancet_catalog::methods::audit::{AuditObserve, AuditReplay};
use lancet_catalog::methods::command::Run;
use lancet_catalog::methods::external::Fetch;
use lancet_catalog::methods::session::TodoWrite;
use lancet_catalog::methods::workspace::{Batch, Create, Edit, ObserveWorkspace};
use lancet_catalog::{CatalogMethod, CepMethod};
use lancet_native::enforcement::isolation::{
    CommandIsolationLandlockBehavior, CommandIsolationMode, CommandIsolationRuntimeConfig,
    CommandIsolationUnsupportedHostBehavior,
};
use lancet_native::{
    CepAuditContext, CepAuditStore, CepClient, DirectVerifyConfig, ExecutionRouting, WorkspacePath,
    impact_kind_for_method, impact_kind_label, wire_code_for_error,
};
use lancet_protocol::error::{
    CepErrorData, invalid_params, jsonrpc, method_not_found, parse_error,
};
use lancet_protocol::{CepError, Retriability, ToolEvent};
use lattice_agent::{
    AgentLoop, AgentLoopConfig, AgentTurnEvent, ToolCallExecutor, ToolExecutionError,
};
use lattice_llm::ToolCall;
use lattice_profile::{
    LancetIsolationMode, LancetLandlockBehavior, LancetNetworkSection, LancetObservationSection,
    LancetSection, LancetUnsupportedHostBehavior, LoadedAgentProfile,
};
#[cfg(test)]
use lattice_profile::{LancetVerifySection, LancetWorkspacePolicySection};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::sync::RwLock;

use crate::session::ApiError;
use crate::turn::types::RunTurnRequest;
use crate::turn::upper::{UpperLease, UpperPrepareError, prepare_upper, resolve_upper_config};

/// Map a [`LancetError`] from the runtime layer onto the API error vocabulary.
fn map_lancet_err(err: LancetError) -> ApiError {
    match err {
        LancetError::Config(msg) => ApiError::BadRequest(msg),
        LancetError::UnknownTools { unknown, available } => ApiError::BadRequest(format!(
            "unknown enabled tools {unknown:?}; available catalog tools: {available:?}"
        )),
        LancetError::Internal(msg) => ApiError::Internal(msg),
    }
}

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
            Edit::NAME => self.call_typed::<Edit>(params).await,
            Batch::NAME => self.call_typed::<Batch>(params).await,
            Fetch::NAME => self.call_typed::<Fetch>(params).await,
            TodoWrite::NAME => self.call_typed::<TodoWrite>(params).await,
            Run::NAME => self.call_typed::<Run>(params).await,
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

/// Resolved production turn dependencies assembled by the API layer.
///
/// The LANCET runtime (server lifecycle, CEP client, audit) comes from
/// [`lancet_api::LancetRuntime`]; this struct adds the LATTICE-side
/// [`AgentLoop`] wired to a [`CepExecutorAdapter`].
pub(crate) struct ProductionTurnDeps {
    pub agent_loop: Arc<AgentLoop>,
    pub tool_definitions: Vec<lattice_llm::ToolDefinition>,
    pub runtime: lancet_api::ServerLifecycle,
    pub cep_client: CepClient,
    pub audit: Arc<CepAuditStore>,
    pub audit_context: Arc<RwLock<CepAuditContext>>,
    pub upper_lease: Option<UpperLease>,
    #[cfg(test)]
    pub build_metrics: CepRuntimeBuildMetrics,
}

pub(crate) struct ProductionTurnDepsInput<'a> {
    pub request: &'a RunTurnRequest,
    pub project_root: &'a str,
    pub enabled_tools: &'a [String],
    pub llm: Arc<dyn lattice_agent::LlmClient>,
    pub lancet: Option<&'a LancetSection>,
    pub model_id: &'a str,
    pub runtime_version: &'a str,
    pub loaded_profile: &'a LoadedAgentProfile,
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

fn workspace_policy_from_lancet(lancet: &LancetSection) -> Result<WorkspacePolicy, ApiError> {
    let mut workspace = WorkspacePolicy::default();
    if let Some(section) = lancet.workspace.as_ref() {
        workspace.allowed_write_paths = section.allowed_write_paths.clone();
    }
    workspace.validate().map_err(|err| {
        ApiError::BadRequest(format!("lancet.workspace.allowed_write_paths: {err}"))
    })?;
    Ok(workspace)
}

/// Lower `[lancet.verify]` into immutable Direct verify configuration.
pub(crate) fn direct_verify_from_profile(
    lancet: &LancetSection,
    _runtime_project_root: &Path,
) -> Result<Option<DirectVerifyConfig>, ApiError> {
    let Some(section) = lancet.verify.as_ref() else {
        return Ok(None);
    };
    if section.argv.is_empty() {
        return Err(ApiError::BadRequest(
            "lancet.verify.argv must not be empty when the section is present".into(),
        ));
    }
    if section.timeout_ms == Some(0) {
        return Err(ApiError::BadRequest(
            "lancet.verify.timeout_ms must be positive when set".into(),
        ));
    }
    let cwd = match section.cwd.as_deref() {
        Some(path) => WorkspacePath::parse(path).map_err(|_| {
            ApiError::BadRequest(format!("lancet.verify.cwd: invalid relative path '{path}'"))
        })?,
        None => WorkspacePath::project_root(),
    };
    Ok(Some(DirectVerifyConfig {
        argv: section.argv.clone(),
        cwd,
        timeout_ms: section.timeout_ms,
        expected_exit_code: section.expected_exit_code.unwrap_or(0),
    }))
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

fn to_llm_tool_definition(tool: lancet_api::ToolDefinition) -> lattice_llm::ToolDefinition {
    lattice_llm::ToolDefinition::new(tool.name, tool.description, tool.parameters)
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
            inline_shell_max_script_bytes: section
                .inline_shell_max_script_bytes
                .unwrap_or(defaults.inline_shell_max_script_bytes),
            inline_shell_max_commands: section
                .inline_shell_max_commands
                .unwrap_or(defaults.inline_shell_max_commands),
            inline_shell_max_words_per_command: section
                .inline_shell_max_words_per_command
                .unwrap_or(defaults.inline_shell_max_words_per_command),
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
        workspace: workspace_policy_from_lancet(lancet)?,
        project_root: cep_context.project_root,
        denied_effect_kinds: cep_context.denied_effect_kinds,
    };

    if let Some(policy_section) = &lancet.policy
        && let Some(path) = &policy_section.path
    {
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

/// Translate the optional profile `[lancet]` section into a fully-resolved
/// LANCET policy and command-isolation configuration.
///
/// This is the LATTICE-side half of the seam: it knows about `LancetSection`
/// and produces the LANCET-native types `lancet-api` consumes.
fn resolve_lancet_policy(
    lancet: Option<&LancetSection>,
    project_root: &str,
) -> Result<(PolicyConfig, CommandIsolationRuntimeConfig), ApiError> {
    let policy = match lancet {
        Some(section) => load_policy_config(section, Path::new(project_root))?,
        None => PolicyConfig::defaults_for_project_root(PathBuf::from(project_root)),
    };
    let command_isolation = match lancet {
        Some(section) => command_isolation_config_from_profile(section, Path::new(project_root))?,
        None => CommandIsolationRuntimeConfig::default(),
    };
    Ok((policy, command_isolation))
}

/// Build a turn-scoped LANCET runtime and wrap its client in an [`AgentLoop`].
///
/// Translation of the profile into policy happens here; runtime composition is
/// delegated to [`lancet_api::LancetRuntime::build`].
pub(crate) async fn build_production_turn_deps(
    input: ProductionTurnDepsInput<'_>,
) -> Result<ProductionTurnDeps, ApiError> {
    let ProductionTurnDepsInput {
        request,
        project_root,
        enabled_tools,
        llm,
        lancet,
        model_id,
        runtime_version,
        loaded_profile,
    } = input;
    // Canonicalize the project root up front so the profile policy and the
    // runtime share one canonical root. Falls back to the raw path on failure,
    // matching `CepPolicyConfig::defaults_for_project_root`'s tolerance.
    let canonical_root = std::fs::canonicalize(project_root)
        .ok()
        .and_then(|p| p.to_str().map(str::to_owned));
    let project_root = canonical_root.as_deref().unwrap_or(project_root);

    let (policy, command_isolation) = resolve_lancet_policy(lancet, project_root)?;
    let cep_mode = lancet
        .map(|c| c.mode.clone())
        .unwrap_or(lancet_protocol::policy::CepMode::Enforce);
    let in_memory_audit = request.test_llm.is_some() && request.test_persist_audit != Some(false);
    let audit = if in_memory_audit {
        AuditBacking::InMemory
    } else {
        AuditBacking::external_for_project(Path::new(project_root))
            .map_err(|err| ApiError::Internal(format!("resolve external audit path: {err}")))?
    };

    // Phase 36 Plan 11: pass None for baseline_oid to maintain backward compatibility.
    // Future lifecycle orchestration will inject the preflight baseline from
    // resolve_turn_target when Upper-backed turns are activated.
    let upper_config = resolve_upper_config(loaded_profile, Path::new(project_root), enabled_tools, None)
        .map_err(|err| match err {
            UpperPrepareError::MissingWorkspaceSection
            | UpperPrepareError::MissingWorkspaceSource => {
                ApiError::BadRequest(
                    "Upper-backed tools require an explicit [workspace] section with a real image or template source".into(),
                )
            }
            other => ApiError::BadRequest(other.to_string()),
        })?;
    let mut upper_lease: Option<UpperLease> = None;
    let execution = if upper_config.requires_upper {
        match prepare_upper(&upper_config, request.workspace_override.clone()).await {
            Ok(prepared) => {
                let upper_context = prepared.backend.upper_context().clone();
                upper_lease = Some(prepared.lease);
                Some(ExecutionRouting {
                    backend: Arc::new(prepared.backend),
                    upper: upper_context,
                })
            }
            Err(err) => return Err(ApiError::BadRequest(err.to_string())),
        }
    } else {
        None
    };

    let runtime_result = LancetRuntime::build(LancetRuntimeConfig {
        project_root: PathBuf::from(project_root),
        enabled_tools: enabled_tools.to_vec(),
        policy,
        command_isolation,
        cep_mode,
        audit,
        model_id: model_id.to_string(),
        runtime_version: runtime_version.to_string(),
        execution,
        direct_verify: lancet
            .map(|section| direct_verify_from_profile(section, Path::new(project_root)))
            .transpose()?
            .flatten(),
    })
    .map_err(map_lancet_err);

    let runtime = match runtime_result {
        Ok(runtime) => runtime,
        Err(err) => {
            if let Some(mut lease) = upper_lease.take() {
                let _ = lease.destroy().await;
            }
            return Err(err);
        }
    };

    let parts = runtime.into_parts();

    let enabled_names: HashSet<String> = parts
        .tool_definitions
        .iter()
        .map(|tool| tool.name.clone())
        .collect();
    let tool_definitions = parts
        .tool_definitions
        .into_iter()
        .map(to_llm_tool_definition)
        .collect();
    let adapter = Arc::new(CepExecutorAdapter::new(parts.client.clone(), enabled_names));
    let agent_loop = Arc::new(AgentLoop::new(AgentLoopConfig {
        llm,
        tool_executor: adapter,
    }));

    Ok(ProductionTurnDeps {
        agent_loop,
        tool_definitions,
        runtime: parts.lifecycle,
        cep_client: parts.client,
        audit: parts.audit,
        audit_context: parts.audit_context,
        upper_lease,
        #[cfg(test)]
        build_metrics: CepRuntimeBuildMetrics {
            adapter_instances: 1,
            transport_pairs: 1,
        },
    })
}

#[cfg(test)]
pub(crate) async fn build_envelope_turn_deps_for_test(
    project_root: &str,
    enabled_tools: &[String],
    llm: Arc<dyn lattice_agent::LlmClient>,
    in_memory_audit: bool,
    lancet: Option<&LancetSection>,
    model_id: &str,
    runtime_version: &str,
) -> Result<ProductionTurnDeps, ApiError> {
    let canonical_root = std::fs::canonicalize(project_root)
        .ok()
        .and_then(|p| p.to_str().map(str::to_owned));
    let project_root = canonical_root.as_deref().unwrap_or(project_root);
    let (policy, command_isolation) = resolve_lancet_policy(lancet, project_root)?;
    let cep_mode = lancet
        .map(|c| c.mode.clone())
        .unwrap_or(lancet_protocol::policy::CepMode::Enforce);
    let audit = if in_memory_audit {
        AuditBacking::InMemory
    } else {
        AuditBacking::default_under(project_root)
    };
    let runtime = LancetRuntime::build(LancetRuntimeConfig {
        project_root: PathBuf::from(project_root),
        enabled_tools: enabled_tools.to_vec(),
        policy,
        command_isolation,
        cep_mode,
        audit,
        model_id: model_id.to_string(),
        runtime_version: runtime_version.to_string(),
        execution: None,
        direct_verify: None,
    })
    .map_err(map_lancet_err)?;
    let parts = runtime.into_parts();
    let enabled_names: HashSet<String> = parts
        .tool_definitions
        .iter()
        .map(|tool| tool.name.clone())
        .collect();
    let tool_definitions = parts
        .tool_definitions
        .into_iter()
        .map(to_llm_tool_definition)
        .collect();
    let adapter = Arc::new(CepExecutorAdapter::new(parts.client.clone(), enabled_names));
    let agent_loop = Arc::new(AgentLoop::new(AgentLoopConfig {
        llm,
        tool_executor: adapter,
    }));
    Ok(ProductionTurnDeps {
        agent_loop,
        tool_definitions,
        runtime: parts.lifecycle,
        cep_client: parts.client,
        audit: parts.audit,
        audit_context: parts.audit_context,
        upper_lease: None,
        build_metrics: CepRuntimeBuildMetrics {
            adapter_instances: 1,
            transport_pairs: 1,
        },
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
    if let CepErrorData::Validation(data) = &cep.data
        && data.reason == "invalid params"
    {
        return jsonrpc::INVALID_PARAMS;
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
    if let Some(start) = message.find("invalid type: ")
        && let Some(field_start) = message[..start].rfind('`')
    {
        let prefix = &message[field_start + 1..];
        if let Some(field_end) = prefix.find('`') {
            return Some(prefix[..field_end].to_string());
        }
    }
    None
}

#[cfg(test)]
mod cep_context_profile_policy {
    use super::*;
    use lancet_catalog::methods::command::RunParams;
    use lancet_protocol::policy::CepMode;
    use lancet_protocol::policy::{
        PolicyAuditContext, PolicyAuthorityInput, PolicyBudgetSnapshot, PolicyDecision, PolicyHook,
        PolicyInput,
    };
    use lattice_profile::LancetSection;
    use serde_json::json;
    use std::collections::BTreeMap;
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
                inline_shell_max_script_bytes: Some(2_048),
                inline_shell_max_commands: Some(8),
                inline_shell_max_words_per_command: Some(32),
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
        assert_eq!(policy.inline_shell_max_script_bytes, 2_048);
        assert_eq!(policy.inline_shell_max_commands, 8);
        assert_eq!(policy.inline_shell_max_words_per_command, 32);
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
            policy.inline_shell_max_script_bytes,
            defaults.inline_shell_max_script_bytes
        );
        assert_eq!(
            policy.inline_shell_max_commands,
            defaults.inline_shell_max_commands
        );
        assert_eq!(
            policy.inline_shell_max_words_per_command,
            defaults.inline_shell_max_words_per_command
        );
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
    fn workspace_policy_from_profile_lowers_allowed_write_paths() {
        let dir = TempDir::new().unwrap();
        let cep = LancetSection {
            workspace: Some(LancetWorkspacePolicySection {
                allowed_write_paths: vec!["src/**".into()],
            }),
            ..Default::default()
        };
        let config = load_policy_config(&cep, dir.path()).expect("policy config");
        assert_eq!(config.workspace.allowed_write_paths, vec!["src/**"]);
    }

    #[test]
    fn workspace_policy_from_profile_rejects_git_paths() {
        let dir = TempDir::new().unwrap();
        let cep = LancetSection {
            workspace: Some(LancetWorkspacePolicySection {
                allowed_write_paths: vec![".git/config".into()],
            }),
            ..Default::default()
        };
        let err = load_policy_config(&cep, dir.path()).unwrap_err();
        assert!(
            err.to_string()
                .contains("lancet.workspace.allowed_write_paths"),
            "{err}"
        );
    }

    #[test]
    fn direct_verify_from_profile_lowers_typed_argv() {
        let dir = TempDir::new().unwrap();
        let cep = LancetSection {
            verify: Some(LancetVerifySection {
                argv: vec!["cargo".into(), "test".into()],
                cwd: Some("crates/foo".into()),
                timeout_ms: Some(30_000),
                expected_exit_code: Some(0),
            }),
            ..Default::default()
        };
        let verify = direct_verify_from_profile(&cep, dir.path())
            .expect("verify")
            .expect("present");
        assert_eq!(verify.argv, vec!["cargo", "test"]);
        assert_eq!(verify.cwd.as_str(), "crates/foo");
        assert!(verify.validate().is_ok());
    }

    #[test]
    fn workspace_policy_from_profile_absent_defaults_empty() {
        let dir = TempDir::new().unwrap();
        let config = load_policy_config(&LancetSection::default(), dir.path()).expect("policy");
        assert!(config.workspace.allowed_write_paths.is_empty());
    }

    #[test]
    fn workspace_policy_shared_by_mutate_and_command_in_policy_config() {
        let dir = TempDir::new().unwrap();
        let cep = LancetSection {
            workspace: Some(LancetWorkspacePolicySection {
                allowed_write_paths: vec!["src/**".into()],
            }),
            ..Default::default()
        };
        let config = load_policy_config(&cep, dir.path()).expect("policy");
        let hook = lancet_authority::policy::default::DefaultPolicyHook::new(config);
        let mutate = PolicyInput {
            method: "mutate.create".into(),
            params: json!({"path": "src/new.rs", "content": "x"}),
            authority: PolicyAuthorityInput,
            audit_context: PolicyAuditContext {
                turn_id: "t".into(),
                session_id: "s".into(),
                call_id: Some(1),
                budget_snapshot: PolicyBudgetSnapshot {
                    turn_observation_bytes_used: 0,
                    turn_observation_bytes_limit: 65536,
                    todo_ops_used: 0,
                    todo_ops_limit: 10,
                },
            },
        };
        assert_eq!(hook.decide(&mutate), PolicyDecision::Allowed);
        let region = hook.config().workspace.authorized_region_for(
            "mutate.create",
            None,
            &hook.config().to_cep_policy_config(),
        );
        let command_region = hook.config().workspace.authorized_region_for(
            "exec.command_run",
            Some(&lancet_authority::policy::command::EffectiveImpact::WorkspaceMutation),
            &hook.config().to_cep_policy_config(),
        );
        assert_eq!(
            region.write_path_prefixes,
            command_region.write_path_prefixes
        );
    }
}

#[cfg(test)]
mod phase34_turn_wiring {
    use super::*;
    use lancet_api::audit_db_path_for_project_in;
    use lancet_audit::{AuditOutcome, CallAuditMetadata};
    use lancet_catalog::methods::session::TodoWrite;
    use lancet_cep::ToolExposure;
    use lattice_agent::testing::FakeStreamingLlmClient;
    use lattice_profile::{
        AgentProfile, AgentSection, LancetSection, LlmSection, LoadedAgentProfile, PromptSection,
        ToolsSection,
    };
    use tempfile::TempDir;

    fn host_only_profile(enabled: Vec<String>) -> LoadedAgentProfile {
        LoadedAgentProfile {
            path: PathBuf::from("/tmp/agent.toml"),
            package_dir: PathBuf::from("/tmp"),
            profile: AgentProfile {
                schema_version: 1,
                agent: AgentSection {
                    name: "t".into(),
                    model: "m".into(),
                    description: None,
                },
                prompt: PromptSection {
                    system: Some("sys".into()),
                    system_path: None,
                },
                tools: ToolsSection { enabled },
                llm: LlmSection::default(),
                lancet: LancetSection::default(),
                workspace: None,
            },
            system_prompt: "sys".into(),
        }
    }

    #[tokio::test]
    async fn phase34_external_audit_preserves_lower() {
        let state = TempDir::new().unwrap();
        unsafe {
            std::env::set_var("XDG_STATE_HOME", state.path());
        }
        let project = TempDir::new().unwrap();
        let legacy_dir = project.path().join(".lattice");
        std::fs::create_dir_all(&legacy_dir).unwrap();
        let legacy_path = legacy_dir.join("cep_audit.db");
        let legacy_store = CepAuditStore::open(&legacy_path).unwrap();
        let ctx = CepAuditContext {
            turn_id: "legacy".into(),
            session_id: "legacy".into(),
            ..Default::default()
        };
        let start = legacy_store
            .start_call(
                &ctx,
                1,
                TodoWrite::NAME,
                &CallAuditMetadata::native_policy_enforced(Some("started")),
            )
            .unwrap();
        legacy_store
            .finalize_call(
                start,
                AuditOutcome::Success {
                    summary: "legacy".into(),
                },
                None,
                None,
            )
            .unwrap();
        let before = std::fs::read(&legacy_path).unwrap();

        let llm: Arc<dyn lattice_agent::LlmClient> =
            Arc::new(FakeStreamingLlmClient::builder().build());
        let profile = host_only_profile(vec![TodoWrite::NAME.to_string()]);
        let request = RunTurnRequest {
            input: "hi".into(),
            session_id: "s".into(),
            loaded_profile: Some(profile.clone()),
            cancel_flag: None,
            test_llm: Some(llm.clone()),
            test_project_root: Some(project.path().to_string_lossy().into()),
            test_persist_audit: Some(false),
            workspace_override: None,
        };
        let enabled_tools = [TodoWrite::NAME.to_string()];
        let deps = build_production_turn_deps(ProductionTurnDepsInput {
            request: &request,
            project_root: project.path().to_str().unwrap(),
            enabled_tools: &enabled_tools,
            llm,
            lancet: None,
            model_id: "m",
            runtime_version: "v",
            loaded_profile: &profile,
        })
        .await
        .expect("direct deps");

        deps.cep_client
            .call::<TodoWrite>(lancet_catalog::methods::session::TodoWriteParams {
                todos: vec![lancet_catalog::methods::session::TodoItem {
                    content: "direct".into(),
                    status: lancet_catalog::methods::session::TodoStatus::Pending,
                }],
            })
            .await
            .expect("todo write");

        let external_path = audit_db_path_for_project_in(state.path(), project.path()).unwrap();
        let external = CepAuditStore::open(&external_path).unwrap();
        assert!(external.row_count().unwrap() >= 2);

        let after = std::fs::read(&legacy_path).unwrap();
        assert_eq!(before, after);
    }

    #[tokio::test]
    async fn phase34_upper_lifecycle_host_only_has_no_lease() {
        let dir = TempDir::new().unwrap();
        let llm: Arc<dyn lattice_agent::LlmClient> =
            Arc::new(FakeStreamingLlmClient::builder().build());
        let profile = host_only_profile(vec![TodoWrite::NAME.to_string()]);
        let request = RunTurnRequest {
            input: "hi".into(),
            session_id: "s".into(),
            loaded_profile: Some(profile.clone()),
            cancel_flag: None,
            test_llm: Some(llm.clone()),
            test_project_root: None,
            test_persist_audit: None,
            workspace_override: None,
        };
        let enabled_tools = [TodoWrite::NAME.to_string()];
        let deps = build_production_turn_deps(ProductionTurnDepsInput {
            request: &request,
            project_root: dir.path().to_str().unwrap(),
            enabled_tools: &enabled_tools,
            llm,
            lancet: None,
            model_id: "m",
            runtime_version: "v",
            loaded_profile: &profile,
        })
        .await
        .expect("host-only direct deps");
        assert!(deps.upper_lease.is_none());
    }

    #[test]
    fn catalog_exposure_omits_unknown_envelope_method() {
        // session.envelope was removed from the catalog; attempting to enable
        // it now produces a ToolExposureError listing it as unknown.
        let err = ToolExposure::from_catalog_enabled(&[
            ObserveWorkspace::NAME.to_string(),
            "session.envelope".to_string(),
        ])
        .expect_err("session.envelope is no longer a catalog method");
        assert!(err.unknown.contains(&"session.envelope".to_string()));
    }
}

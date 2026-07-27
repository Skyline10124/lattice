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
    BudgetLimits, CepPolicyConfig, NetworkPolicyConfig, ObservationPolicyConfig,
};
use lancet_native::{
    CepAuditContext, CepAuditStore, CepClient, impact_kind_for_method, impact_kind_label,
    wire_code_for_error,
};
use lancet_protocol::error::{CepErrorData, jsonrpc, method_not_found, parse_error};
use lancet_protocol::{CepError, Retriability, ToolEvent};
use lattice_agent::{
    AgentLoop, AgentLoopConfig, AgentTurnEvent, ToolCallExecutor, ToolExecutionError,
};
use lattice_llm::ToolCall;
#[cfg(test)]
use lattice_profile::LancetWorkspacePolicySection;
use lattice_profile::{LancetNetworkSection, LancetObservationSection, LancetSection};
use serde_json::Value;
use tokio::sync::RwLock;

use crate::session::ApiError;
use crate::turn::types::RunTurnRequest;

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

/// Client-backed adapter that maps LLM-shape tool calls into CEP invocations.
///
/// D.5: the available set is driven by runtime definitions (native catalog
/// methods + Component Tools projected from generated descriptors), not by
/// a hardcoded catalog membership check. All methods — native and Component
/// alike — go through `CepClient::call_raw`, so the server-side dispatch
/// split (D.3) handles routing. The adapter does not hand-write method
/// matches or schemas; params/result validation is done by the native typed
/// registry or by generated descriptor validators on the server side.
pub struct CepExecutorAdapter {
    client: CepClient,
    available_names: HashSet<String>,
    enabled_names: HashSet<String>,
}

impl CepExecutorAdapter {
    /// Construct from a CEP client, the runtime-projected tool definitions
    /// (available set), and the enabled-tool names.
    ///
    /// `definitions` is the combined native + Component projection from
    /// `LancetRuntime::build`; `enabled_names` is the subset the profile
    /// allows for this turn. A tool call is accepted only if its name is in
    /// both sets.
    pub fn new(
        client: CepClient,
        definitions: &[lancet_api::ToolDefinition],
        enabled_names: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            client,
            available_names: definitions.iter().map(|d| d.name.clone()).collect(),
            enabled_names: enabled_names.into_iter().map(Into::into).collect(),
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
}

#[async_trait]
impl ToolCallExecutor for CepExecutorAdapter {
    async fn execute(&self, call: &ToolCall) -> Result<String, ToolExecutionError> {
        let name = &call.function.name;
        if !self.available_names.contains(name) || !self.enabled_names.contains(name) {
            return Err(Self::map_cep_error(method_not_found(name)));
        }
        let params: Value = serde_json::from_str(&call.function.arguments).map_err(|e| {
            Self::map_cep_error(parse_error(format!("invalid tool arguments JSON: {e}")))
        })?;
        let value = self
            .client
            .call_raw(name.clone(), params)
            .await
            .map_err(Self::map_cep_error)?;
        serde_json::to_string(&value)
            .map_err(|e| ToolExecutionError::simple(format!("serialize tool result: {e}")))
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
        denied_effect_kinds: lancet
            .effects
            .as_ref()
            .map(|effects| effects.deny.iter().cloned().collect())
            .unwrap_or_default(),
    })
}

fn workspace_policy_from_profile(lancet: &LancetSection) -> Result<WorkspacePolicy, ApiError> {
    let mut workspace = WorkspacePolicy::default();
    if let Some(section) = lancet.workspace.as_ref() {
        workspace.denied_write_patterns = section.denied_write_patterns.clone();
        workspace.included_ignored_read_patterns = section.included_ignored_read_patterns.clone();
        workspace.denied_read_patterns = section.denied_read_patterns.clone();
    }
    workspace
        .validate()
        .map_err(|err| ApiError::BadRequest(format!("lancet.workspace: {err}")))?;
    Ok(workspace)
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
        network: cep_context.network,
        budget: cep_context.budget,
        observation: cep_context.observation,
        workspace: workspace_policy_from_profile(lancet)?,
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
/// LANCET policy configuration.
///
/// This is the LATTICE-side half of the seam: it knows about `LancetSection`
/// and produces the LANCET-native types `lancet-api` consumes.
fn resolve_lancet_policy(
    lancet: Option<&LancetSection>,
    project_root: &str,
) -> Result<PolicyConfig, ApiError> {
    match lancet {
        Some(section) => load_policy_config(section, Path::new(project_root)),
        None => Ok(PolicyConfig::defaults_for_project_root(PathBuf::from(
            project_root,
        ))),
    }
}

/// Build a turn-scoped LANCET runtime and wrap its client in an [`AgentLoop`].
///
/// Translation of the profile into policy happens here; runtime composition is
/// delegated to [`lancet_api::LancetRuntime::build`], which constructs the
/// one concrete Direct-Local execution routing internally (ADR 0003). There is
/// no Upper lifecycle, workspace override, or externally supplied execution
/// backend.
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
    } = input;
    // Canonicalize the project root up front so the profile policy and the
    // runtime share one canonical root. Falls back to the raw path on failure,
    // matching `CepPolicyConfig::defaults_for_project_root`'s tolerance.
    let canonical_root = std::fs::canonicalize(project_root)
        .ok()
        .and_then(|p| p.to_str().map(str::to_owned));
    let project_root = canonical_root.as_deref().unwrap_or(project_root);

    let policy = resolve_lancet_policy(lancet, project_root)?;
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

    let runtime = LancetRuntime::build(LancetRuntimeConfig {
        project_root: PathBuf::from(project_root),
        enabled_tools: enabled_tools.to_vec(),
        policy,
        cep_mode,
        audit,
        model_id: model_id.to_string(),
        runtime_version: runtime_version.to_string(),
    })
    .map_err(map_lancet_err)?;

    let parts = runtime.into_parts();

    let enabled_names: HashSet<String> = parts
        .tool_definitions
        .iter()
        .map(|tool| tool.name.clone())
        .collect();
    let adapter = Arc::new(CepExecutorAdapter::new(
        parts.client.clone(),
        &parts.tool_definitions,
        enabled_names,
    ));
    let tool_definitions = parts
        .tool_definitions
        .into_iter()
        .map(to_llm_tool_definition)
        .collect();
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
    let policy = resolve_lancet_policy(lancet, project_root)?;
    let cep_mode = lancet
        .map(|c| c.mode.clone())
        .unwrap_or(lancet_protocol::policy::CepMode::Enforce);
    let audit = if in_memory_audit {
        AuditBacking::InMemory
    } else {
        AuditBacking::external_for_project(Path::new(project_root))
            .map_err(|err| ApiError::Internal(format!("resolve external audit path: {err}")))?
    };
    let runtime = LancetRuntime::build(LancetRuntimeConfig {
        project_root: PathBuf::from(project_root),
        enabled_tools: enabled_tools.to_vec(),
        policy,
        cep_mode,
        audit,
        model_id: model_id.to_string(),
        runtime_version: runtime_version.to_string(),
    })
    .map_err(map_lancet_err)?;
    let parts = runtime.into_parts();
    let enabled_names: HashSet<String> = parts
        .tool_definitions
        .iter()
        .map(|tool| tool.name.clone())
        .collect();
    let adapter = Arc::new(CepExecutorAdapter::new(
        parts.client.clone(),
        &parts.tool_definitions,
        enabled_names,
    ));
    let tool_definitions = parts
        .tool_definitions
        .into_iter()
        .map(to_llm_tool_definition)
        .collect();
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
        build_metrics: CepRuntimeBuildMetrics {
            adapter_instances: 1,
            transport_pairs: 1,
        },
    })
}

/// Map a CEP transport notification into an internal agent turn event.
pub(crate) fn map_cep_tool_event(event: ToolEvent) -> Option<AgentTurnEvent> {
    let params = event.params;
    let call_id = params.get("call_id").and_then(|v| v.as_u64())?;
    match event.method.as_str() {
        "cep.call_started" => Some(AgentTurnEvent::CepCallStarted {
            call_id,
            method: string_param(&params, "method")?,
            impact_kind: string_param(&params, "impact_kind").unwrap_or_else(|| "unknown".into()),
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

#[cfg(test)]
mod cep_context_profile_policy {
    use super::*;
    use lancet_protocol::policy::{
        PolicyAuditContext, PolicyAuthorityInput, PolicyBudgetSnapshot, PolicyDecision, PolicyHook,
        PolicyInput,
    };
    use lattice_profile::LancetSection;
    use serde_json::json;
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
    fn workspace_policy_from_profile_lowers_denied_write_patterns() {
        let dir = TempDir::new().unwrap();
        let cep = LancetSection {
            workspace: Some(LancetWorkspacePolicySection {
                denied_write_patterns: vec!["src/**".into()],
                ..Default::default()
            }),
            ..Default::default()
        };
        let config = load_policy_config(&cep, dir.path()).expect("policy config");
        assert_eq!(config.workspace.denied_write_patterns, vec!["src/**"]);
    }

    #[test]
    fn workspace_policy_from_profile_rejects_traversal_patterns() {
        let dir = TempDir::new().unwrap();
        let cep = LancetSection {
            workspace: Some(LancetWorkspacePolicySection {
                denied_write_patterns: vec!["../etc/passwd".into()],
                ..Default::default()
            }),
            ..Default::default()
        };
        let err = load_policy_config(&cep, dir.path()).unwrap_err();
        assert!(err.to_string().contains("lancet.workspace"), "{err}");
    }

    #[test]
    fn workspace_policy_from_profile_lowers_read_visibility_patterns() {
        let dir = TempDir::new().unwrap();
        let cep = LancetSection {
            workspace: Some(LancetWorkspacePolicySection {
                denied_write_patterns: vec!["src/**".into()],
                included_ignored_read_patterns: vec![".env".into()],
                denied_read_patterns: vec!["secrets/**".into()],
            }),
            ..Default::default()
        };
        let config = load_policy_config(&cep, dir.path()).expect("policy config");
        assert_eq!(
            config.workspace.included_ignored_read_patterns,
            vec![".env"]
        );
        assert_eq!(config.workspace.denied_read_patterns, vec!["secrets/**"]);
    }

    #[test]
    fn workspace_policy_from_profile_absent_defaults_empty() {
        let dir = TempDir::new().unwrap();
        let config = load_policy_config(&LancetSection::default(), dir.path()).expect("policy");
        assert!(config.workspace.denied_write_patterns.is_empty());
    }

    #[test]
    fn workspace_policy_applies_denied_patterns_to_mutate_region_in_policy_config() {
        let dir = TempDir::new().unwrap();
        let cep = LancetSection {
            workspace: Some(LancetWorkspacePolicySection {
                denied_write_patterns: vec!["src/**".into()],
                ..Default::default()
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
        let region = hook
            .config()
            .workspace
            .authorized_region_for("mutate.create", &hook.config().to_cep_policy_config());
        assert!(!region.denied_write_path_prefixes.is_empty());
    }
}

#[cfg(test)]
mod turn_wiring {
    use super::*;
    use lancet_audit::{AuditOutcome, CallAuditMetadata};
    use lancet_catalog::CepMethod;
    use lancet_catalog::methods::session::TodoWrite;
    use lancet_catalog::methods::workspace::ObserveWorkspace;
    use lancet_cep::ToolExposure;
    use lattice_agent::testing::FakeStreamingLlmClient;
    use lattice_profile::{
        AgentProfile, AgentSection, LancetSection, LlmSection, LoadedAgentProfile, PromptSection,
        ToolsSection,
    };
    use tempfile::TempDir;

    fn test_profile(enabled: Vec<String>) -> LoadedAgentProfile {
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
            },
            system_prompt: "sys".into(),
        }
    }

    fn init_git_repo(root: &std::path::Path) {
        let mut files: Vec<(String, String)> = Vec::new();
        if let Ok(entries) = std::fs::read_dir(root) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    let content = std::fs::read_to_string(&path).unwrap_or_default();
                    files.push((name, content));
                }
            }
        }
        let refs: Vec<(&str, &str)> = files
            .iter()
            .map(|(p, c)| (p.as_str(), c.as_str()))
            .collect();
        let _ = lattice_session::test_utils::init_fixture_repo(root, &refs);
    }

    #[tokio::test]
    async fn external_audit_preserves_lower() {
        let _xdg = lancet_audit::ScopedXdgStateHome::new();
        let project = TempDir::new().unwrap();
        init_git_repo(project.path());
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
        let profile = test_profile(vec![TodoWrite::NAME.to_string()]);
        let request = RunTurnRequest {
            input: "hi".into(),
            session_id: "s".into(),
            loaded_profile: Some(profile.clone()),
            cancel_flag: None,
            test_llm: Some(llm.clone()),
            test_project_root: Some(project.path().to_string_lossy().into()),
            test_persist_audit: Some(false),
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

        deps.audit.await_pending().await;

        let external_path = lancet_api::audit_db_path_for_project(project.path()).unwrap();
        let external = CepAuditStore::open(&external_path).unwrap();
        assert!(external.row_count().unwrap() >= 2);

        let after = std::fs::read(&legacy_path).unwrap();
        assert_eq!(before, after);
    }

    // #37: `upper_lifecycle_session_tool_creates_lease` was removed — the
    // Upper lease it asserted on no longer exists. The Direct-Local runtime
    // builds its own execution routing internally, so an equivalent smoke
    // test is `runtime_config_defaults_have_no_execution_field` in
    // `lancet-api::runtime`.

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

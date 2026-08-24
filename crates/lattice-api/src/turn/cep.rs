// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! LATTICE↔LANCET turn seam: profile/policy translation, runtime assembly,
//! and CEP-event projection over [`lancet_api`].

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lancet_api::{
    LancetError, LancetRuntime, LancetRuntimeConfig, WorkspaceHistory, WorkspaceHistoryBacking,
    workspace_history_backing,
};
use lancet_authority::policy::default::{ExternalPolicyFile, PolicyConfig, WorkspacePolicy};
use lancet_authority::{CepPolicyConfig, NetworkPolicyConfig, ObservationPolicyConfig};
use lancet_native::CepClient;
#[cfg(test)]
use lancet_native::wire_code_for_error;
use lancet_protocol::ToolEvent;
#[cfg(test)]
use lancet_protocol::error::{CepErrorData, jsonrpc};
#[cfg(test)]
use lancet_protocol::{CepError, Retriability};
#[cfg(test)]
use lattice_profile::LancetWorkspacePolicySection;
use lattice_profile::{LancetNetworkSection, LancetObservationSection, LancetSection};
use serde_json::Value;

use crate::session::ApiError;
use crate::turn::execute::AgentTurnEvent;
use crate::turn::types::RunTurnRequest;

/// Structured CEP error metadata preserved for API/CLI observation.
#[derive(Debug, Clone, PartialEq)]
pub struct CepErrorPayload {
    pub code: i32,
    pub message: String,
    pub data: serde_json::Value,
    pub retriability: String,
}

/// Tool execution failure with optional structured CEP metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolExecutionError {
    pub message: String,
    pub cep_error: Option<CepErrorPayload>,
}

impl ToolExecutionError {
    pub fn simple(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            cep_error: None,
        }
    }
}

impl From<String> for ToolExecutionError {
    fn from(message: String) -> Self {
        Self::simple(message)
    }
}

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
    pub client_instances: usize,
    pub transport_pairs: usize,
}

/// Resolved production turn dependencies assembled by the API layer.
///
/// The LANCET runtime (server lifecycle and bound CEP client) plus the
/// one-attempt model driver and descriptor-pinned Tool catalog consumed by
/// the production TurnCoordinator.
pub(crate) struct ProductionTurnDeps {
    pub model_driver: lattice_agent::ModelStepDriver,
    pub tool_catalog: Vec<lancet_api::ToolDefinition>,
    pub runtime: lancet_api::ServerLifecycle,
    pub cep_client: CepClient,
    /// Private durable history of successfully installed Atomic Local
    /// Mutations (#63). The live turn path reads its current object id once
    /// the agent loop is done and records it on the Turn commit.
    pub workspace_history: Arc<WorkspaceHistory>,
    #[cfg(test)]
    pub build_metrics: CepRuntimeBuildMetrics,
}

pub(crate) struct ProductionTurnDepsInput<'a> {
    pub request: &'a RunTurnRequest,
    pub project_root: &'a str,
    /// The binding validated from SessionState. Production turns must use this
    /// key to locate history; the path-derived fallback is only for direct
    /// LANCET fixture construction that has no SessionApi composition context.
    pub project_store_key: Option<lattice_session::ProjectStoreKey>,
    pub state_layout: Option<lattice_session::StateLayout>,
    pub enabled_tools: &'a [String],
    pub llm: Arc<dyn lattice_agent::LlmClient>,
    pub lancet: Option<&'a LancetSection>,
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

    let mut observation = ObservationPolicyConfig::defaults();
    if let Some(section) = lancet.observation.as_ref() {
        apply_observation_profile(section, &mut observation)?;
    }

    let mut network = NetworkPolicyConfig::safe_defaults();
    if let Some(section) = lancet.network.as_ref() {
        apply_network_profile(section, &mut network);
    }

    // No turn-budget copy is assembled here: it is derived from the observation
    // limits by `CepPolicyConfig::budget` (#121).
    Ok(CepPolicyConfig {
        project_root: effective_root,
        observation,
        network,
        denied_effect_kinds: lancet
            .effects
            .as_ref()
            .map(|effects| effects.deny.iter().cloned().collect())
            .unwrap_or_default(),
        // Component Tool admission needs `[tools].enabled` as well as
        // `[lancet.capabilities]`, so it is lowered from the whole profile at
        // runtime assembly; this executor-facing config leaves it fail-closed.
        capability_admission: Default::default(),
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

/// Lower `[lancet.observation]` limits onto the authority observation policy.
///
/// Limits only. Observation *scope* comes from the effective project root
/// (`[lancet.project_root].root`) and the `[lancet.workspace]` read-visibility
/// rules; the retired `allowed_roots` key is rejected at profile load (#121).
fn apply_observation_profile(
    section: &LancetObservationSection,
    observation: &mut ObservationPolicyConfig,
) -> Result<(), ApiError> {
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
        methods: Default::default(),
        network: cep_context.network,
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

/// Build a turn-scoped LANCET runtime and wrap its client in a sequencer.
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
        project_store_key,
        state_layout,
        enabled_tools,
        llm,
        lancet,
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
    let workspace_history = match (state_layout, project_store_key) {
        (Some(layout), Some(key)) => WorkspaceHistoryBacking::for_key(layout, key),
        (None, None) => workspace_history_backing(Path::new(project_root))
            .map_err(|err| ApiError::Internal(format!("resolve workspace history path: {err}")))?,
        _ => {
            return Err(ApiError::Internal(
                "Session project-store layout and ProjectStoreKey must be supplied together".into(),
            ));
        }
    };

    // #28 B8/D6: the profile is the single admission authority — `[tools].enabled`
    // is the stage-1 capability grant, `[lancet.capabilities]` subtracts. Without
    // a loaded profile nothing is admitted (fail closed).
    let capability_admission = request
        .loaded_profile
        .as_ref()
        .map(|loaded| loaded.profile.capability_admission_policy())
        .unwrap_or_default();

    let runtime = LancetRuntime::build(LancetRuntimeConfig {
        enabled_tools: enabled_tools.to_vec(),
        policy,
        capability_admission,
        cep_mode,
        invocations: workspace_history.clone(),
        workspace_history,
    })
    .map_err(map_lancet_err)?;

    let parts = runtime.into_parts();

    let tool_catalog = parts.tool_definitions;
    Ok(ProductionTurnDeps {
        model_driver: lattice_agent::ModelStepDriver::new(llm),
        tool_catalog,
        runtime: parts.lifecycle,
        cep_client: parts.client,
        workspace_history: parts.workspace_history,
        #[cfg(test)]
        build_metrics: CepRuntimeBuildMetrics {
            client_instances: 1,
            transport_pairs: 1,
        },
    })
}

/// Map a CEP transport notification into an internal agent turn event.
pub(crate) fn map_cep_tool_event(event: ToolEvent) -> Option<AgentTurnEvent> {
    let params = event.params;
    let invocation_id = params
        .get("invocation_id")
        .and_then(|value| value.as_str())
        .and_then(|raw| lancet_protocol::InvocationId::parse(raw).ok())?;
    match event.method.as_str() {
        "cep.call_started" => Some(AgentTurnEvent::CepCallStarted {
            invocation_id,
            method: string_param(&params, "method")?,
            semantic_stage: string_param(&params, "semantic_stage")?,
            guard_stage: string_param(&params, "guard_stage")?,
        }),
        "cep.call_finished" => Some(AgentTurnEvent::CepCallFinished {
            invocation_id,
            method: string_param(&params, "method")?,
            ok: params.get("ok").and_then(|value| value.as_bool())?,
            semantic_stage: string_param(&params, "semantic_stage")?,
            guard_stage: string_param(&params, "guard_stage")?,
            error_code: params
                .get("error_code")
                .and_then(|value| value.as_str())
                .map(str::to_string),
        }),
        _ => None,
    }
}

fn string_param(params: &Value, key: &str) -> Option<String> {
    params.get(key).and_then(|v| v.as_str()).map(str::to_string)
}

#[cfg(test)]
pub(super) fn test_wire_code(cep: &CepError) -> i32 {
    if let CepErrorData::Validation(data) = &cep.data
        && data.reason == "invalid params"
    {
        return jsonrpc::INVALID_PARAMS;
    }
    wire_code_for_error(cep)
}

#[cfg(test)]
pub(super) fn retriability_label(value: &Retriability) -> String {
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
    use lattice_profile::LancetSection;
    use tempfile::TempDir;

    #[test]
    fn cep_context_profile_policy_defaults_when_cep_absent() {
        let dir = TempDir::new().unwrap();
        let policy =
            cep_policy_from_profile(&LancetSection::default(), dir.path()).expect("defaults");
        assert_eq!(policy.project_root, dir.path().canonicalize().unwrap());
        assert_eq!(policy.budget(), policy.observation.budget_limits());
    }

    /// #121: `[lancet.observation]` lowers limits only. Observation scope is the
    /// effective project root, which is what secure path resolution, Guard L1,
    /// and the `VisibilityEngine` actually enforce. The two tests this replaced
    /// asserted that a retired `allowed_roots` key was canonicalized into a
    /// field no read decision ever consulted.
    #[test]
    fn cep_context_profile_observation_lowers_limits_only() {
        let dir = TempDir::new().unwrap();
        let cep = LancetSection {
            observation: Some(LancetObservationSection {
                max_single_read_bytes: Some(4096),
                max_todo_ops: Some(7),
                ..Default::default()
            }),
            ..Default::default()
        };
        let policy = cep_policy_from_profile(&cep, dir.path()).expect("policy");
        assert_eq!(policy.observation.max_single_read_bytes, 4096);
        assert_eq!(policy.budget().max_todo_ops, 7);
        assert_eq!(policy.project_root, dir.path().canonicalize().unwrap());
    }

    /// Observation scope narrowing goes through `[lancet.project_root].root`,
    /// the one root every read path resolves against.
    #[test]
    fn cep_context_profile_project_root_narrows_observation_scope() {
        let dir = TempDir::new().unwrap();
        let sub = dir.path().join("src");
        std::fs::create_dir_all(&sub).unwrap();
        let cep = LancetSection {
            project_root: Some(lattice_profile::LancetProjectRootSection {
                root: Some("src".into()),
            }),
            ..Default::default()
        };
        let policy = cep_policy_from_profile(&cep, dir.path()).expect("policy");
        assert_eq!(policy.project_root, sub.canonicalize().unwrap());
    }

    /// `[lancet.project_root].root` is now the only way to narrow observation
    /// scope, so its containment check is the one that must hold: a root
    /// resolving outside the runtime project root fails the turn rather than
    /// widening the Agent's surface. Both the `CepPolicyConfig` and the
    /// `PolicyConfig` assembly paths enforce it.
    #[test]
    fn cep_context_profile_rejects_project_root_outside_runtime_root() {
        let dir = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let cep = LancetSection {
            project_root: Some(lattice_profile::LancetProjectRootSection {
                root: Some(outside.path().to_string_lossy().into_owned()),
            }),
            ..Default::default()
        };

        let err = cep_policy_from_profile(&cep, dir.path()).unwrap_err();
        assert!(
            matches!(&err, ApiError::BadRequest(message) if message.contains("outside runtime project root")),
            "got: {err:?}"
        );

        let err = load_policy_config(&cep, dir.path()).unwrap_err();
        assert!(
            matches!(&err, ApiError::BadRequest(message) if message.contains("outside runtime project root")),
            "got: {err:?}"
        );
    }

    /// Traversal is rejected the same way, so the containment check cannot be
    /// walked around with `..`.
    #[test]
    fn cep_context_profile_rejects_project_root_traversal() {
        let dir = TempDir::new().unwrap();
        let sub = dir.path().join("src");
        std::fs::create_dir_all(&sub).unwrap();
        let cep = LancetSection {
            project_root: Some(lattice_profile::LancetProjectRootSection {
                root: Some("../".into()),
            }),
            ..Default::default()
        };

        let err = cep_policy_from_profile(&cep, sub.as_path()).unwrap_err();
        assert!(matches!(err, ApiError::BadRequest(_)), "got: {err:?}");
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
        // mutate.create/edit are Component Tools (#31): DefaultPolicyHook no longer
        // resolves them. The profile write-deny list still shapes the mutate
        // authorized region that the Tool Host enforces.
        let region = config
            .workspace
            .authorized_region_for("mutate.create", &config.to_cep_policy_config());
        assert!(
            region
                .denied_write_path_prefixes
                .iter()
                .any(|p| p.ends_with("src")),
            "profile denied_write_patterns must reach the mutate region: {:?}",
            region.denied_write_path_prefixes
        );
    }
}

#[cfg(test)]
mod turn_wiring {
    use super::*;

    #[test]
    fn maps_typed_lifecycle_events_without_numeric_audit_ids() {
        let invocation = "00000000-0000-4000-8000-000000000001";
        let event = lancet_protocol::jsonrpc::Notification::new(
            "cep.call_finished".to_string(),
            serde_json::json!({
                "invocation_id": invocation,
                "method": "fetch.web",
                "ok": true,
                "semantic_stage": "cep_native",
                "guard_stage": "cep_policy_lifecycle",
                "error_code": null
            }),
        );
        assert!(matches!(
            map_cep_tool_event(event),
            Some(AgentTurnEvent::CepCallFinished { invocation_id, method, ok: true, .. })
                if invocation_id.to_string() == invocation && method == "fetch.web"
        ));
    }

    use lancet_cep::ToolExposure;
    use lattice_agent::testing::FakeStreamingLlmClient;
    use lattice_profile::LoadedAgentProfile;
    use tempfile::TempDir;

    // #37: `upper_lifecycle_session_tool_creates_lease` was removed — the
    // Upper lease it asserted on no longer exists. The Direct-Local runtime
    // builds its own execution routing internally, so an equivalent smoke
    // test is `runtime_config_defaults_have_no_execution_field` in
    // `lancet-api::runtime`.

    #[test]
    fn catalog_exposure_omits_unknown_envelope_method() {
        // session.envelope was removed from the catalog; attempting to enable
        // it now produces a ToolExposureError listing it as unknown.
        //
        // The companion name must be a *native catalog* method, because that is
        // the only set `from_catalog_enabled` resolves against. It used to be
        // `observe.workspace`; since #30 that name would land in `unknown` too
        // and the assertion would no longer isolate `session.envelope`.
        let err = ToolExposure::from_catalog_enabled(&[
            "mutate.create".to_string(),
            "session.envelope".to_string(),
        ])
        .expect_err("session.envelope is no longer a catalog method");
        assert!(err.unknown.contains(&"session.envelope".to_string()));
    }

    /// Write a real `agent.toml` under `dir` and load it through the profile
    /// loader — the production entry point, not a hand-built struct.
    fn write_and_load_profile(dir: &std::path::Path, capabilities: &str) -> LoadedAgentProfile {
        let path = dir.join("agent.toml");
        std::fs::write(
            &path,
            format!(
                r#"schema_version = 1

[agent]
name = "t"
model = "m"

[prompt]
system = "sys"

[tools]
enabled = ["{tool}"]
{capabilities}"#,
                // The embedded Component Tool: naming it here is what proves the
                // production available set is native catalog + Component
                // descriptors, and that a profile can enable a Component Tool at
                // all (#29 criterion 5).
                tool = "session.todo_write",
            ),
        )
        .unwrap();
        lattice_profile::load_agent_profile(&path).expect("profile loads")
    }

    async fn build_deps_for(
        profile: &LoadedAgentProfile,
        project: &TempDir,
    ) -> Result<ProductionTurnDeps, ApiError> {
        let layout = lattice_session::StateLayout::in_state_root(&project.path().join("state"));
        let project_key = crate::turn::test_support::test_project_key(project.path());
        crate::stores::create_project_stores(&layout, project_key)
            .expect("create fixture project stores");
        let llm: Arc<dyn lattice_agent::LlmClient> =
            Arc::new(FakeStreamingLlmClient::builder().build());
        let request = RunTurnRequest {
            input: "hi".into(),
            session_id: "s".into(),
            loaded_profile: Some(profile.clone()),
            cancel_flag: None,
            test_llm: Some(llm.clone()),
            test_project_root: Some(project.path().to_string_lossy().into()),
        };
        let enabled_tools = ["session.todo_write".to_string()];
        build_production_turn_deps(ProductionTurnDepsInput {
            request: &request,
            project_root: project.path().to_str().unwrap(),
            project_store_key: Some(project_key),
            state_layout: Some(layout),
            enabled_tools: &enabled_tools,
            llm,
            lancet: Some(&profile.profile.lancet),
        })
        .await
    }

    /// #28 B2/B8: the profile's `[lancet.capabilities]` is what production
    /// admission runs on. A deny entry naming no composed capability is a
    /// startup error (C2) — the only decision observable end to end while the
    /// production component catalog is empty — so this pins the whole chain
    /// `agent.toml` → `LoadedAgentProfile` → `CapabilityAdmissionPolicy` →
    /// `ServerRunConfig`. Lose the profile lowering and the fail-closed
    /// default silently takes over with an empty deny list.
    #[tokio::test]
    async fn profile_capabilities_section_reaches_component_admission() {
        let project = TempDir::new().unwrap();
        let profile = write_and_load_profile(
            project.path(),
            "\n[lancet.capabilities]\ndeny = [\"session.events\"]\n",
        );
        assert_eq!(
            profile.profile.capability_admission_policy().enabled_tools,
            std::collections::BTreeSet::from(["session.todo_write".to_string()])
        );

        let Err(err) = build_deps_for(&profile, &project).await else {
            panic!("a deny entry no composed capability declares must refuse startup");
        };
        assert!(
            format!("{err:?}").contains("session.events"),
            "the startup error must name the profile's deny entry: {err:?}"
        );
    }

    /// Control for the above: the same profile without the section builds,
    /// so the error there is the deny entry and not the fixture.
    #[tokio::test]
    async fn profile_without_capabilities_section_builds() {
        let project = TempDir::new().unwrap();
        let profile = write_and_load_profile(project.path(), "");
        assert!(
            profile.profile.lancet.capabilities.is_none(),
            "control fixture must carry no [lancet.capabilities]"
        );
        build_deps_for(&profile, &project)
            .await
            .expect("an empty deny list admits startup");
    }
}

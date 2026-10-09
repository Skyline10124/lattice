// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! LATTICE↔LANCET turn seam: profile/policy translation, runtime assembly,
//! and CEP-event projection over [`lancet_api`].

#[cfg(test)]
use lancet_api::WorkspaceHistory;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lancet_api::{
    ComponentRuntime, LancetError, LancetRuntime, LancetRuntimeConfig, WorkspaceHistoryBacking,
};
use lancet_authority::policy::default::{ExternalPolicyFile, PolicyConfig, WorkspacePolicy};
use lancet_authority::{CepPolicyConfig, NetworkPolicyConfig, ObservationPolicyConfig};
use lancet_protocol::ToolEvent;
#[cfg(test)]
use lattice_profile::LancetWorkspacePolicySection;
use lattice_profile::{LancetNetworkSection, LancetObservationSection, LancetSection};
use serde_json::Value;

use crate::session::ApiError;
use crate::turn::types::{RunTurnRequest, TurnEvent};

/// Map a [`LancetError`] from the runtime layer onto the API error vocabulary.
fn map_lancet_err(err: LancetError, enabled: &[String]) -> ApiError {
    match err {
        LancetError::Config(msg) => ApiError::BadRequest(msg),
        LancetError::UnknownTools { available, .. } => {
            ApiError::Configuration(crate::ConfigRejection::unknown_tools(enabled, available))
        }
        LancetError::Internal(msg) => ApiError::Internal(msg),
    }
}

/// Resolved production turn dependencies assembled by the API layer.
///
/// The generated Component runtime plus one-attempt model driver and
/// descriptor-pinned Tool catalog consumed by the production TurnCoordinator.
pub(crate) struct ProductionTurnDeps {
    pub model_driver: lattice_agent::ModelStepDriver,
    pub tool_catalog: Vec<lancet_api::ToolDefinition>,
    pub runtime: LancetRuntime,
    pub events: tokio::sync::mpsc::UnboundedReceiver<ToolEvent>,
}

pub(crate) struct ProductionTurnDepsInput<'a> {
    pub request: &'a RunTurnRequest,
    pub project_root: &'a str,
    /// Project identity validated from SessionState.
    pub project_store_key: lattice_session::ProjectStoreKey,
    pub state_layout: lattice_session::StateLayout,
    pub enabled_tools: &'a [String],
    pub tls_roots_der: &'a [Vec<u8>],
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
        network: cep_context.network,
        observation: cep_context.observation,
        workspace: workspace_policy_from_profile(lancet)?,
        project_root: cep_context.project_root,
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
/// This is the LATTICE-side seam: it knows `LancetSection` and produces the
/// resolved policy types consumed by `lancet-api`.
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

/// 组装 Turn 专属运行时；路径、外部策略、领域存储打开及 Component 组成全部在阻塞池。
/// 配置仍由既有 profile 转换，执行器和 saga 不增加第二条路径。
pub(crate) async fn build_production_turn_deps(
    input: ProductionTurnDepsInput<'_>,
) -> Result<ProductionTurnDeps, ApiError> {
    let ProductionTurnDepsInput {
        request,
        project_root,
        project_store_key,
        state_layout,
        enabled_tools,
        tls_roots_der,
        llm,
        lancet,
    } = input;
    let project_root = PathBuf::from(project_root);
    let enabled_tools = enabled_tools.to_vec();
    let tls_roots_der = tls_roots_der.to_vec();
    let lancet = lancet.cloned();
    let capability_admission = request
        .loaded_profile
        .as_ref()
        .map(|loaded| loaded.profile.capability_admission_policy())
        .unwrap_or_default();
    tokio::task::spawn_blocking(move || {
        let canonical_root = std::fs::canonicalize(&project_root).map_err(|err| {
            ApiError::BadRequest(format!("cannot canonicalize Local Workspace root: {err}"))
        })?;
        let project_root = canonical_root.to_str().ok_or_else(|| {
            ApiError::BadRequest(
                "Local Workspace root must be valid UTF-8 for the runtime policy path".into(),
            )
        })?;
        let policy = resolve_lancet_policy(lancet.as_ref(), project_root)?;
        let workspace_history = WorkspaceHistoryBacking::for_key(state_layout, project_store_key);
        ComponentRuntime::shared()
            .map_err(|err| ApiError::Internal(format!("component composition: {err}")))?;
        let mut runtime = LancetRuntime::build(LancetRuntimeConfig {
            local_workspace_root: canonical_root,
            enabled_tools: enabled_tools.clone(),
            policy,
            capability_admission,
            tls_roots_der,
            workspace_history,
        })
        .map_err(|error| map_lancet_err(error, &enabled_tools))?;
        let tool_catalog = runtime.tool_definitions().to_vec();
        let events = runtime.take_events();
        Ok(ProductionTurnDeps {
            model_driver: lattice_agent::ModelStepDriver::new(llm),
            tool_catalog,
            runtime,
            events,
        })
    })
    .await
    .map_err(|err| ApiError::Internal(format!("Turn dependency worker failed: {err}")))?
}

/// Map a CEP transport notification into an internal agent turn event.
pub(crate) fn map_cep_tool_event(event: ToolEvent) -> Option<TurnEvent> {
    let params = event.params;
    let invocation_id = params
        .get("invocation_id")
        .and_then(|value| value.as_str())
        .and_then(|raw| lancet_protocol::InvocationId::parse(raw).ok())?;
    match event.method.as_str() {
        "cep.call_started" => Some(TurnEvent::CepCallStarted {
            invocation_id,
            method: string_param(&params, "method")?,
            semantic_stage: string_param(&params, "semantic_stage")?,
            guard_stage: string_param(&params, "guard_stage")?,
        }),
        "cep.call_finished" => Some(TurnEvent::CepCallFinished {
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
mod cep_context_profile_policy {
    use super::*;
    use lattice_profile::LancetSection;
    use tempfile::TempDir;

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
                "semantic_stage": "cell_component",
                "guard_stage": "cell_admission",
                "error_code": null
            }),
        );
        assert!(matches!(
            map_cep_tool_event(event),
            Some(TurnEvent::CepCallFinished { invocation_id, method, ok: true, .. })
                if invocation_id.to_string() == invocation && method == "fetch.web"
        ));
    }

    use lattice_agent::testing::FakeStreamingLlmClient;
    use lattice_profile::LoadedAgentProfile;
    use tempfile::TempDir;

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
                // A manifest-owned Component name proves the production available set
                // comes from generated descriptors and remains profile-selectable.
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
        let project_key = lattice_session::ProjectStoreKey::derive(project.path()).unwrap();
        WorkspaceHistory::create(WorkspaceHistoryBacking::for_key(
            layout.clone(),
            project_key,
        ))
        .expect("create fixture workspace history");
        let llm: Arc<dyn lattice_agent::LlmClient> =
            Arc::new(FakeStreamingLlmClient::builder().build());
        let request = RunTurnRequest {
            input: "hi".into(),
            session_id: None,
            agent: Some(profile.profile.agent.name.clone()),
            loaded_profile: Some(profile.clone()),
            cancel_flag: None,
            test_llm: Some((llm.clone(), lattice_llm::ProviderKind::OpenAiCompatible)),
            test_project_root: Some(project.path().to_string_lossy().into()),
        };
        let enabled_tools = ["session.todo_write".to_string()];
        build_production_turn_deps(ProductionTurnDepsInput {
            request: &request,
            project_root: project.path().to_str().unwrap(),
            project_store_key: project_key,
            state_layout: layout,
            enabled_tools: &enabled_tools,
            tls_roots_der: &[],
            llm,
            lancet: Some(&profile.profile.lancet),
        })
        .await
    }

    /// The profile capability policy reaches generated Component admission.
    /// Unknown deny entries fail startup instead of silently retaining authority.
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

    #[test]
    fn unknown_tool_rejection_reports_positions_without_names() {
        let sentinel = "sk-unknown-tool-82ace195";
        let enabled = vec![sentinel.into(), "observe.workspace".into(), sentinel.into()];
        let error = map_lancet_err(
            LancetError::UnknownTools {
                unknown: vec![sentinel.into()],
                available: vec!["observe.workspace".into()],
            },
            &enabled,
        );
        let local = crate::local::LocalError::from(error);
        assert_eq!(local.kind, crate::local::LocalErrorKind::BadRequest);
        assert!(local.message.contains("[tools].enabled"));
        assert!(local.message.contains("items [1, 3] (1-based)"));
        assert!(
            local
                .message
                .contains("available catalog tools: observe.workspace")
        );
        assert!(!local.message.contains(sentinel));
    }
}

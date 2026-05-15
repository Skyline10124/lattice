use lattice_profile::GenericAgentSpec;
use lattice_session::{
    ProfileSnapshot, ProfileSnapshotDraft, ProfileSnapshotSummary, ProfileSourceAudit,
};

use crate::{Runtime, RuntimeError, RuntimeInvocationContext};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileSourceFingerprint(pub Option<String>);

#[derive(Clone)]
pub struct AgentSessionRequest {
    pub spec: GenericAgentSpec,
    pub invocation: RuntimeInvocationContext,
    pub session_id: Option<String>,
    pub source_label: String,
    pub source_fingerprint: Option<String>,
}

#[derive(Clone)]
pub struct RuntimeAgentSession {
    pub spec: GenericAgentSpec,
    pub invocation: RuntimeInvocationContext,
    pub session_id: Option<String>,
    pub profile_snapshot: ProfileSnapshot,
    pub source_label: String,
    pub source_fingerprint: Option<String>,
}

impl Runtime {
    pub fn prepare_agent_session(
        &self,
        request: AgentSessionRequest,
    ) -> Result<RuntimeAgentSession, RuntimeError> {
        let profile_snapshot = profile_snapshot_from_request(&request)?;
        Ok(RuntimeAgentSession {
            spec: request.spec,
            invocation: request.invocation,
            session_id: request.session_id,
            profile_snapshot,
            source_label: request.source_label,
            source_fingerprint: request.source_fingerprint,
        })
    }
}

fn profile_snapshot_from_request(
    request: &AgentSessionRequest,
) -> Result<ProfileSnapshot, RuntimeError> {
    let canonical_spec = serde_json::to_value(&request.spec)
        .map_err(|err| RuntimeError::Config(format!("profile spec serialization failed: {err}")))?;
    ProfileSnapshot::from_draft(ProfileSnapshotDraft {
        source: ProfileSourceAudit::new(
            request.source_label.clone(),
            request.source_fingerprint.clone(),
        ),
        canonical_spec,
        summary: ProfileSnapshotSummary {
            profile_name: request.spec.profile_name.clone(),
            source_label: request.source_label.clone(),
            model: request.spec.model.clone(),
            tool_bundles: request.spec.tools.bundles.clone(),
            enabled_tools: request.spec.tools.enabled.clone(),
            security_mode: Some(request.spec.security.sandbox_mode.clone()),
            context: serde_json::json!({
                "project_instructions": request.spec.context.project_instructions,
                "workspace_context": request.spec.context.workspace_context,
            }),
            behavior: serde_json::json!({
                "max_turns": request.spec.behavior.max_turns,
                "reasoning_effort": request.spec.behavior.reasoning_effort,
            }),
        },
        captured_at: None,
    })
    .map_err(|err| RuntimeError::Config(format!("profile snapshot creation failed: {err}")))
}

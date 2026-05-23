use lattice_agent::{AgentTurnRequest, AgentTurnStatus};
use lattice_llm::{ModelRouter, ModelTarget};
use lattice_session::HeadTranscript;

use super::types::{RunTurnRequest, RunTurnStatus};

/// Resolve model alias and system prompt from the request.
pub(crate) fn resolve_model_and_prompt(
    request: &RunTurnRequest,
) -> (String, Option<String>, String) {
    let (model, system_prompt) = if let Some(ref lp) = request.loaded_profile {
        // Model override: request.model takes precedence over profile default
        let model = request
            .model
            .clone()
            .unwrap_or_else(|| lp.profile.agent.model.clone());
        let system_prompt = if lp.system_prompt.is_empty() {
            None
        } else {
            Some(lp.system_prompt.clone())
        };
        (model, system_prompt)
    } else {
        (
            request
                .model
                .clone()
                .unwrap_or_else(|| "sonnet".to_string()),
            None,
        )
    };

    let provider = ModelRouter::new()
        .resolve(&ModelTarget::new(&model))
        .map(|rm| rm.provider.as_str().to_string())
        .unwrap_or_else(|_| "unknown".to_string());

    (model, system_prompt, provider)
}

/// Build an AgentTurnRequest from the API request, session context, and resolved model/prompt.
pub(crate) fn build_agent_request(
    request: &RunTurnRequest,
    session_id: &str,
    transcript: HeadTranscript,
    model: String,
    system_prompt: Option<String>,
    provider: Option<String>,
) -> AgentTurnRequest {
    let enabled_tools = request
        .loaded_profile
        .as_ref()
        .map(|lp| lp.profile.tools.enabled.clone())
        .unwrap_or_default();

    let project_root = request
        .loaded_profile
        .as_ref()
        .map(|lp| lp.package_dir.to_string_lossy().to_string())
        .unwrap_or_else(|| {
            std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| "/".to_string())
        });

    AgentTurnRequest {
        session_id: session_id.to_string(),
        transcript,
        user_input: request.input.clone(),
        system_prompt,
        model,
        provider,
        enabled_tools,
        project_root,
        cancel_flag: request.cancel_flag.clone(),
        timeout: None,
    }
}

/// Map agent turn status to API turn status.
pub(crate) fn map_status(status: &AgentTurnStatus) -> RunTurnStatus {
    match status {
        AgentTurnStatus::Completed => RunTurnStatus::Completed,
        AgentTurnStatus::Error => RunTurnStatus::Error,
        AgentTurnStatus::Cancelled => RunTurnStatus::Cancelled,
    }
}

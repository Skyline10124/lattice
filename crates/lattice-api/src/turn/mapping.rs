use lattice_agent::{AgentTurnRequest, AgentTurnStatus, LlmRetryConfig};
use lattice_llm::{ModelRouter, ModelTarget, ToolDefinition};
use lattice_session::HeadTranscript;

use super::types::{RunTurnRequest, RunTurnStatus};

/// Resolve model alias and system prompt from the request.
pub(crate) fn resolve_model_and_prompt(
    request: &RunTurnRequest,
) -> (String, Option<String>, String) {
    let (model, system_prompt) = if let Some(ref lp) = request.loaded_profile {
        let model = lp.profile.agent.model.clone();
        let system_prompt = if lp.system_prompt.is_empty() {
            None
        } else {
            Some(lp.system_prompt.clone())
        };
        (model, system_prompt)
    } else {
        ("sonnet".to_string(), None)
    };

    let provider = ModelRouter::new()
        .resolve(&ModelTarget::new(&model))
        .map(|rm| rm.provider.as_str().to_string())
        .unwrap_or_else(|_| "unknown".to_string());

    (model, system_prompt, provider)
}

pub(crate) fn resolve_project_root(request: &RunTurnRequest) -> String {
    if let Some(root) = &request.test_project_root {
        return root.clone();
    }
    std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "/".to_string())
}

/// Build an AgentTurnRequest from the API request, session context, and resolved model/prompt.
pub(crate) fn build_agent_request(
    request: &RunTurnRequest,
    session_id: &str,
    transcript: HeadTranscript,
    model: String,
    system_prompt: Option<String>,
    provider: Option<String>,
    tool_definitions: Vec<ToolDefinition>,
) -> AgentTurnRequest {
    AgentTurnRequest {
        session_id: session_id.to_string(),
        transcript,
        user_input: request.input.clone(),
        system_prompt,
        model,
        provider,
        tool_definitions,
        cancel_flag: request.cancel_flag.clone(),
        timeout: None,
        llm_retry: llm_retry_config_from_request(request),
    }
}

fn llm_retry_config_from_request(request: &RunTurnRequest) -> LlmRetryConfig {
    request
        .loaded_profile
        .as_ref()
        .map(|loaded| {
            let retry = &loaded.profile.llm.retry;
            LlmRetryConfig {
                max_retries: retry.max_retries,
                initial_delay_ms: retry.initial_delay_ms,
                max_delay_ms: retry.max_delay_ms,
            }
        })
        .unwrap_or_default()
}

/// Map agent turn status to API turn status.
pub(crate) fn map_status(status: &AgentTurnStatus) -> RunTurnStatus {
    match status {
        AgentTurnStatus::Completed => RunTurnStatus::Completed,
        AgentTurnStatus::Error => RunTurnStatus::Error,
        AgentTurnStatus::Cancelled => RunTurnStatus::Cancelled,
    }
}

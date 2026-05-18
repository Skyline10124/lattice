use lattice_agent::{AgentToolResultSummary as AgentToolSummary, AgentTurnStatus};
use lattice_llm::{ModelRouter, ModelTarget};
use lattice_session::HeadTranscript;

use super::types::{RunTurnRequest, RunTurnStatus, ToolResultSummary};
use lattice_agent::AgentTurnRequest;

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
        .map(|rm| format!("{:?}", rm.provider).to_lowercase())
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
                .unwrap_or_default()
        });

    AgentTurnRequest {
        session_id: session_id.to_string(),
        transcript,
        user_input: request.input.clone(),
        system_prompt,
        model,
        provider: None,
        enabled_tools,
        project_root,
        cancel_flag: None,
        timeout: None,
    }
}

/// Convert agent tool result summaries to API tool result summaries.
pub(crate) fn map_tool_results(summaries: &[AgentToolSummary]) -> Vec<ToolResultSummary> {
    summaries
        .iter()
        .map(|s| ToolResultSummary {
            tool_call_id: s.tool_call_id.clone(),
            name: s.name.clone(),
            ok: s.ok,
            output_len: s.output_len,
            error_summary: s.error_summary.clone(),
        })
        .collect()
}

/// Map agent turn status to API turn status.
pub(crate) fn map_status(status: &AgentTurnStatus) -> RunTurnStatus {
    match status {
        AgentTurnStatus::Completed => RunTurnStatus::Completed,
        AgentTurnStatus::Failed => RunTurnStatus::Failed,
        AgentTurnStatus::Cancelled => RunTurnStatus::Cancelled,
    }
}

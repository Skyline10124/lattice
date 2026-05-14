//! Runtime-backed implementation of the orchestrator AgentTurnExecutor trait.
//!
//! This bridges the orchestrator's lower contract (`AgentTurnExecutor`)
//! to the runtime's handle/turn/completion API so Pipeline standard
//! profiles execute through runtime-owned handles.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use futures::StreamExt;

use lattice_orchestrator::json_output::{
    parse_assistant_output, CompiledOutputSchema, MAX_SCHEMA_RETRIES,
};
use lattice_orchestrator::{
    AgentTurnExecutionError, AgentTurnExecutor, AgentTurnOutput, AgentTurnRequest, AgentTurnSpec,
    AgentTurnStatus,
};
use tracing::warn;

use crate::turn::{TurnRequest, TurnStatus};
use crate::{Runtime, RuntimeError, RuntimeSecurity, RuntimeSecurityConfig};

// ---------------------------------------------------------------------------
// RuntimeAgentTurnExecutor
// ---------------------------------------------------------------------------

/// Runtime-backed turn executor. Implements the orchestrator's
/// `AgentTurnExecutor` trait by delegating to `Runtime::submit_turn`
/// and querying the resulting `TurnCompletion`.
pub struct RuntimeAgentTurnExecutor {
    runtime: Runtime,
    security_config: RuntimeSecurityConfig,
    /// Cache of `RuntimeSecurity` by a stable workspace+policy key so
    /// approvals survive across handle reuse/rebuild.
    security_by_key: Arc<Mutex<HashMap<String, RuntimeSecurity>>>,
}

impl RuntimeAgentTurnExecutor {
    pub fn new(runtime: Runtime) -> Self {
        Self {
            runtime,
            security_config: RuntimeSecurityConfig::default(),
            security_by_key: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn with_security_config(mut self, config: RuntimeSecurityConfig) -> Self {
        self.security_config = config;
        self
    }

    async fn execute_runtime_turn(
        &self,
        spec: AgentTurnSpec,
        request: &AgentTurnRequest,
        input: String,
        security: RuntimeSecurity,
    ) -> Result<crate::turn::TurnCompletion, AgentTurnExecutionError> {
        let handle_id = self
            .runtime
            .get_or_create_agent_handle_for_session(spec, security, request.session_id.clone())
            .map_err(map_runtime_error)?;

        let turn_request = TurnRequest {
            input,
            max_turns: request.max_turns,
            persist_session: request.persist_session,
            session_id: request.session_id.clone(),
            cancellation_id: request.cancellation_id.clone(),
        };

        let stream = self
            .runtime
            .submit_turn(&handle_id, turn_request)
            .await
            .map_err(map_runtime_error)?;

        tokio::pin!(stream);
        while let Some(_event) = stream.next().await {
            // Drain so the runtime stores the terminal completion.
        }

        self.runtime
            .latest_turn_completion(&handle_id)
            .map_err(map_runtime_error)?
            .ok_or_else(|| {
                AgentTurnExecutionError::Runtime("turn completed but no completion stored".into())
            })
    }
}

#[async_trait::async_trait]
impl AgentTurnExecutor for RuntimeAgentTurnExecutor {
    async fn run_turn(
        &self,
        spec: AgentTurnSpec,
        request: AgentTurnRequest,
    ) -> Result<AgentTurnOutput, AgentTurnExecutionError> {
        // Build or reuse RuntimeSecurity keyed by workspace + policy
        let workspace = spec
            .cwd
            .as_deref()
            .or(spec.project_root.as_deref())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));

        let security_key = runtime_security_cache_key(&self.security_config, &workspace);

        let security = {
            let mut cache = self.security_by_key.lock().unwrap();
            if let Some(sec) = cache.get(&security_key) {
                sec.clone()
            } else {
                let sec = crate::build_runtime_security(&self.security_config, &workspace)
                    .map_err(|e| AgentTurnExecutionError::Config(e.to_string()))?;
                cache.insert(security_key.clone(), sec.clone());
                sec
            }
        };

        let schema = spec
            .handoff
            .output_schema
            .as_ref()
            .map(|schema| CompiledOutputSchema::compile(&spec.profile_name, schema))
            .transpose()
            .map_err(AgentTurnExecutionError::Config)?;
        let require_json = schema.is_some();
        let mut input = request.input.clone();

        for attempt in 0..=MAX_SCHEMA_RETRIES {
            let completion = self
                .execute_runtime_turn(spec.clone(), &request, input.clone(), security.clone())
                .await?;

            match completion.status {
                TurnStatus::Completed => {}
                TurnStatus::Cancelled => {
                    return Err(AgentTurnExecutionError::Cancelled(
                        completion.error.unwrap_or_else(|| "turn cancelled".into()),
                    ));
                }
                TurnStatus::Error => {
                    return Err(AgentTurnExecutionError::Runtime(
                        completion.error.unwrap_or_else(|| "turn failed".into()),
                    ));
                }
            }

            let assistant_text = completion.assistant_text.clone();
            let json_output = parse_assistant_output(&assistant_text, require_json)
                .map_err(AgentTurnExecutionError::Runtime)?;

            if let Some(ref schema) = schema {
                let errors = schema.validation_errors(&json_output);
                if errors.is_empty() {
                    return Ok(AgentTurnOutput {
                        assistant_text,
                        json_output,
                        status: AgentTurnStatus::Completed,
                        model: completion.model,
                        provider: completion.provider,
                    });
                }

                warn!(
                    "Output validation failed for {} (attempt {}/{}):\n{}",
                    spec.profile_name,
                    attempt + 1,
                    MAX_SCHEMA_RETRIES + 1,
                    errors.join("\n")
                );

                if attempt < MAX_SCHEMA_RETRIES {
                    input = schema.correction_hint(&request.input, &errors);
                    continue;
                }

                return Err(AgentTurnExecutionError::Runtime(format!(
                    "Agent '{}' output still fails schema validation after {} retries",
                    spec.profile_name, MAX_SCHEMA_RETRIES
                )));
            }

            return Ok(AgentTurnOutput {
                assistant_text,
                json_output,
                status: AgentTurnStatus::Completed,
                model: completion.model,
                provider: completion.provider,
            });
        }

        Err(AgentTurnExecutionError::Runtime(
            "schema retry loop exited unexpectedly".into(),
        ))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn map_runtime_error(e: RuntimeError) -> AgentTurnExecutionError {
    match e {
        RuntimeError::HandleNotFound(id) => {
            AgentTurnExecutionError::Config(format!("handle not found: {id}"))
        }
        RuntimeError::HandleBusy(id) => AgentTurnExecutionError::Busy(id),
        RuntimeError::Config(msg) => AgentTurnExecutionError::Config(msg),
        _ => AgentTurnExecutionError::Runtime(e.to_string()),
    }
}

fn runtime_security_cache_key(
    config: &RuntimeSecurityConfig,
    workspace: &std::path::Path,
) -> String {
    let workspace = std::fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
    format!("workspace={};config={:?}", workspace.display(), config)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_orchestrator::json_output::looks_like_error;

    #[test]
    fn looks_like_error_detects_provider_errors() {
        assert!(looks_like_error(
            r#"{"error":{"message":"invalid_api_key"}}"#
        ));
        assert!(looks_like_error(
            "503 service unavailable: upstream overloaded"
        ));
        assert!(looks_like_error("Network error: connection reset by peer"));
        assert!(looks_like_error("context_length_exceeded: prompt too long"));
    }

    #[test]
    fn looks_like_error_does_not_flag_normal_text() {
        assert!(!looks_like_error(
            "Here is the requested implementation summary."
        ));
        assert!(!looks_like_error(r#"{"answer": "this is fine"}"#));
    }

    #[test]
    fn parse_assistant_output_json() {
        let v = parse_assistant_output(r#"{"key": "value"}"#, false).unwrap();
        assert_eq!(v["key"], "value");
    }

    #[test]
    fn parse_assistant_output_strips_fence() {
        let v = parse_assistant_output("```json\n{\"a\":1}\n```", false).unwrap();
        assert_eq!(v["a"], 1);
    }

    #[test]
    fn parse_assistant_output_falls_back_to_raw_text() {
        let v = parse_assistant_output("just some text, not json", false).unwrap();
        assert_eq!(v["_raw_text"], true);
        assert_eq!(v["content"], "just some text, not json");
    }
}

//! Collect a [`RunTurnResult`] by draining a live [`TurnEvent`] stream.
//!
//! Production [`super::run_turn`] uses this module so the sync facade shares
//! the same execution core as [`super::stream_turn`].

use super::types::{
    RunTurnResult, RunTurnStage, RunTurnStatus, ToolResultSummary, TurnEvent, TurnUsageSummary,
};
use crate::session::ApiError;

const SYNTHETIC_STAGES: &[RunTurnStage] = &[
    RunTurnStage::Profile,
    RunTurnStage::Session,
    RunTurnStage::Agent,
    RunTurnStage::Llm,
    RunTurnStage::Tools,
    RunTurnStage::Commit,
    RunTurnStage::Render,
];

/// Accumulates public turn events into a synchronous [`RunTurnResult`].
pub(super) struct RunTurnCollector {
    session_id: Option<String>,
    commit_id: Option<String>,
    status: Option<RunTurnStatus>,
    usage_total: Option<TurnUsageSummary>,
    assistant_output: String,
    round_tokens: String,
    tool_results: Vec<ToolResultSummary>,
    commit_error: Option<ApiError>,
}

impl RunTurnCollector {
    pub(super) fn new() -> Self {
        Self {
            session_id: None,
            commit_id: None,
            status: None,
            usage_total: None,
            assistant_output: String::new(),
            round_tokens: String::new(),
            tool_results: Vec::new(),
            commit_error: None,
        }
    }

    pub(super) fn observe(&mut self, event: TurnEvent) -> Result<(), ApiError> {
        match event {
            TurnEvent::TurnStarted { session_id, .. } => {
                self.session_id = Some(session_id);
            }
            TurnEvent::TokenDelta { content } => {
                self.round_tokens.push_str(&content);
            }
            TurnEvent::ModelFinished {
                finish_reason,
                usage,
            } => {
                if let Some(u) = usage {
                    self.add_usage(u);
                }
                if finish_reason == "stop" || finish_reason == "length" {
                    self.assistant_output = std::mem::take(&mut self.round_tokens);
                } else {
                    self.round_tokens.clear();
                }
            }
            TurnEvent::ToolCallFinished {
                id,
                name,
                ok,
                output_len,
                error_summary,
            } => {
                self.tool_results.push(ToolResultSummary {
                    tool_call_id: id,
                    name,
                    ok,
                    output_len,
                    error_summary,
                });
            }
            TurnEvent::TurnFailed { .. } | TurnEvent::TurnCancelled { .. } => {
                if !self.round_tokens.is_empty() {
                    self.assistant_output = std::mem::take(&mut self.round_tokens);
                }
            }
            TurnEvent::TurnCommitted {
                session_id,
                commit_id,
                status,
            } => {
                self.session_id = Some(session_id);
                self.commit_id = Some(commit_id);
                self.status = Some(status);
            }
            TurnEvent::CommitFailed { reason } => {
                self.commit_error = Some(map_commit_failed(reason));
            }
            TurnEvent::TurnCommitting
            | TurnEvent::ModelStarted { .. }
            | TurnEvent::ReasoningDelta { .. }
            | TurnEvent::ToolCallStarted { .. }
            | TurnEvent::CepCallStarted { .. }
            | TurnEvent::CepCallFinished { .. }
            | TurnEvent::CepEnvelopeDeclared { .. }
            | TurnEvent::CepEnvelopeSuperseded { .. }
            | TurnEvent::CepEnvelopeMissing { .. }
            | TurnEvent::CepVerifyFailed { .. }
            | TurnEvent::CepReplanRequired { .. } => {}
        }
        Ok(())
    }

    pub(super) fn finish(self) -> Result<RunTurnResult, ApiError> {
        if let Some(err) = self.commit_error {
            return Err(err);
        }

        let session_id = self
            .session_id
            .ok_or_else(|| ApiError::Internal("turn ended without TurnStarted".into()))?;
        let commit_id = self
            .commit_id
            .ok_or_else(|| ApiError::Internal("turn ended without TurnCommitted".into()))?;
        let status = self
            .status
            .ok_or_else(|| ApiError::Internal("turn ended without committed status".into()))?;

        Ok(RunTurnResult {
            session_id,
            commit_id,
            assistant_output: self.assistant_output,
            status,
            usage: normalize_usage(self.usage_total),
            tool_results: self.tool_results,
            stages: SYNTHETIC_STAGES.to_vec(),
        })
    }

    fn add_usage(&mut self, usage: TurnUsageSummary) {
        match &mut self.usage_total {
            None => self.usage_total = Some(usage),
            Some(total) => {
                total.prompt_tokens += usage.prompt_tokens;
                total.completion_tokens += usage.completion_tokens;
                total.total_tokens += usage.total_tokens;
            }
        }
    }
}

fn normalize_usage(usage: Option<TurnUsageSummary>) -> Option<TurnUsageSummary> {
    usage.and_then(|u| {
        if u.prompt_tokens == 0 && u.completion_tokens == 0 && u.total_tokens == 0 {
            None
        } else {
            Some(u)
        }
    })
}

fn map_commit_failed(reason: String) -> ApiError {
    if reason.starts_with("conflict:") {
        ApiError::Conflict(reason.trim_start_matches("conflict:").trim().to_string())
    } else {
        ApiError::Internal(reason)
    }
}

/// Reconstruct a [`RunTurnResult`] from a collected public event stream.
pub fn collect_turn_result_from_events(
    events: impl IntoIterator<Item = TurnEvent>,
) -> Result<RunTurnResult, ApiError> {
    let mut collector = RunTurnCollector::new();
    for event in events {
        collector.observe(event)?;
    }
    collector.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drain(events: Vec<TurnEvent>) -> Result<RunTurnResult, ApiError> {
        let mut collector = RunTurnCollector::new();
        for event in events {
            collector.observe(event)?;
        }
        collector.finish()
    }

    #[test]
    fn completed_content_only() {
        let result = drain(vec![
            TurnEvent::TurnStarted {
                session_id: "s1".to_string(),
                parent_commit_id: None,
                branch: Some("main".to_string()),
            },
            TurnEvent::TokenDelta {
                content: "Hello, world!".to_string(),
            },
            TurnEvent::ModelFinished {
                finish_reason: "stop".to_string(),
                usage: Some(TurnUsageSummary {
                    prompt_tokens: 10,
                    completion_tokens: 5,
                    total_tokens: 15,
                }),
            },
            TurnEvent::TurnCommitting,
            TurnEvent::TurnCommitted {
                session_id: "s1".to_string(),
                commit_id: "c1".to_string(),
                status: RunTurnStatus::Completed,
            },
        ])
        .unwrap();

        assert_eq!(result.session_id, "s1");
        assert_eq!(result.commit_id, "c1");
        assert_eq!(result.assistant_output, "Hello, world!");
        assert_eq!(result.status, RunTurnStatus::Completed);
        assert_eq!(
            result.usage,
            Some(TurnUsageSummary {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            })
        );
        assert_eq!(result.stages, SYNTHETIC_STAGES);
    }

    #[test]
    fn multi_round_tool_loop_uses_final_stop_tokens() {
        let result = drain(vec![
            TurnEvent::TurnStarted {
                session_id: "s1".to_string(),
                parent_commit_id: None,
                branch: None,
            },
            TurnEvent::TokenDelta {
                content: "calling tool".to_string(),
            },
            TurnEvent::ModelFinished {
                finish_reason: "tool_calls".to_string(),
                usage: Some(TurnUsageSummary {
                    prompt_tokens: 10,
                    completion_tokens: 5,
                    total_tokens: 15,
                }),
            },
            TurnEvent::ToolCallFinished {
                id: "c1".to_string(),
                name: "read_file".to_string(),
                ok: true,
                output_len: 42,
                error_summary: None,
            },
            TurnEvent::TokenDelta {
                content: "done".to_string(),
            },
            TurnEvent::ModelFinished {
                finish_reason: "stop".to_string(),
                usage: Some(TurnUsageSummary {
                    prompt_tokens: 20,
                    completion_tokens: 8,
                    total_tokens: 28,
                }),
            },
            TurnEvent::TurnCommitting,
            TurnEvent::TurnCommitted {
                session_id: "s1".to_string(),
                commit_id: "c1".to_string(),
                status: RunTurnStatus::Completed,
            },
        ])
        .unwrap();

        assert_eq!(result.assistant_output, "done");
        assert_eq!(result.tool_results.len(), 1);
        assert_eq!(
            result.usage,
            Some(TurnUsageSummary {
                prompt_tokens: 30,
                completion_tokens: 13,
                total_tokens: 43,
            })
        );
    }

    #[test]
    fn error_after_partial_tokens() {
        let result = drain(vec![
            TurnEvent::TurnStarted {
                session_id: "s1".to_string(),
                parent_commit_id: None,
                branch: None,
            },
            TurnEvent::TokenDelta {
                content: "partial".to_string(),
            },
            TurnEvent::TurnFailed {
                reason: "provider error".to_string(),
            },
            TurnEvent::TurnCommitting,
            TurnEvent::TurnCommitted {
                session_id: "s1".to_string(),
                commit_id: "c1".to_string(),
                status: RunTurnStatus::Error,
            },
        ])
        .unwrap();

        assert_eq!(result.status, RunTurnStatus::Error);
        assert_eq!(result.assistant_output, "partial");
    }

    #[test]
    fn cancelled_keeps_partial_output() {
        let result = drain(vec![
            TurnEvent::TurnStarted {
                session_id: "s1".to_string(),
                parent_commit_id: None,
                branch: None,
            },
            TurnEvent::TokenDelta {
                content: "part".to_string(),
            },
            TurnEvent::TurnCancelled {
                reason: Some("cancelled by caller".to_string()),
            },
            TurnEvent::TurnCommitting,
            TurnEvent::TurnCommitted {
                session_id: "s1".to_string(),
                commit_id: "c1".to_string(),
                status: RunTurnStatus::Cancelled,
            },
        ])
        .unwrap();

        assert_eq!(result.status, RunTurnStatus::Cancelled);
        assert_eq!(result.assistant_output, "part");
    }

    #[test]
    fn commit_failed_returns_error() {
        let err = drain(vec![
            TurnEvent::TurnStarted {
                session_id: "s1".to_string(),
                parent_commit_id: None,
                branch: None,
            },
            TurnEvent::CommitFailed {
                reason: "conflict: expected parent 'a' but HEAD is 'b'".to_string(),
            },
        ])
        .unwrap_err();

        assert!(matches!(err, ApiError::Conflict(_)));
    }

    #[test]
    fn missing_turn_committed_returns_error() {
        let err = drain(vec![TurnEvent::TurnStarted {
            session_id: "s1".to_string(),
            parent_commit_id: None,
            branch: None,
        }])
        .unwrap_err();

        assert!(matches!(err, ApiError::Internal(_)));
    }
}

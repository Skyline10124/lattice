// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Collect a [`RunTurnResult`] by draining a live [`TurnEvent`] stream.
//!
//! Production [`super::run_turn`] uses this module so the sync facade shares
//! the same execution core as [`super::stream_turn`].

use super::types::{
    RunTurnResult, RunTurnStage, RunTurnStatus, ToolResultSummary, TurnEvent, TurnUsageSummary,
};
use crate::session::ApiError;
use lattice_session::WorkspaceDisposition;

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
    disposition: Option<WorkspaceDisposition>,
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
            disposition: None,
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
                    // WR-02: preserve assistant content emitted alongside
                    // tool_calls by accumulating it into `assistant_output`
                    // before clearing the round buffer. A subsequent
                    // stop/length round overwrites with the final answer, but
                    // if the turn ends on a tool_calls round (no final stop),
                    // this content is no longer silently discarded.
                    self.assistant_output.push_str(&self.round_tokens);
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
                disposition,
            } => {
                self.session_id = Some(session_id);
                self.commit_id = Some(commit_id);
                self.status = Some(status);
                self.disposition = Some(disposition);
            }
            TurnEvent::CommitFailed { reason } => {
                self.commit_error = Some(map_commit_failed(reason));
            }
            TurnEvent::TurnCommitting
            | TurnEvent::ModelStarted { .. }
            | TurnEvent::LlmRequestError { .. }
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
        let disposition = self
            .disposition
            .ok_or_else(|| ApiError::Internal("turn ended without committed disposition".into()))?;

        Ok(RunTurnResult {
            session_id,
            commit_id,
            assistant_output: self.assistant_output,
            status,
            disposition,
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
                disposition: WorkspaceDisposition::NotApplicable,
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
                disposition: WorkspaceDisposition::NotApplicable,
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

    /// WR-02: content emitted alongside tool_calls must not be silently
    /// discarded when the turn ends on a tool_calls round (no final stop).
    #[test]
    fn tool_call_round_content_preserved_when_turn_ends_on_tool_calls() {
        let result = drain(vec![
            TurnEvent::TurnStarted {
                session_id: "s1".to_string(),
                parent_commit_id: None,
                branch: None,
            },
            TurnEvent::TokenDelta {
                content: "thinking before tools".to_string(),
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
            TurnEvent::TurnCommitting,
            TurnEvent::TurnCommitted {
                session_id: "s1".to_string(),
                commit_id: "c1".to_string(),
                status: RunTurnStatus::Completed,
                disposition: WorkspaceDisposition::NotApplicable,
            },
        ])
        .unwrap();

        // Without the WR-02 accumulation, `assistant_output` would be empty
        // because the tool_calls branch cleared `round_tokens` and no stop
        // round ever overwrote it.
        assert_eq!(
            result.assistant_output, "thinking before tools",
            "content emitted alongside tool_calls must be preserved when the turn ends on a tool_calls round"
        );
        assert_eq!(result.tool_results.len(), 1);
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
                disposition: WorkspaceDisposition::NotApplicable,
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
                disposition: WorkspaceDisposition::NotApplicable,
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

    /// RED: collector must capture and propagate non-default disposition from TurnCommitted.
    /// This test will fail until the collector captures disposition from the event.
    #[test]
    fn turn_event_disposition_collector_captures_promoted() {
        let result = drain(vec![
            TurnEvent::TurnStarted {
                session_id: "s1".to_string(),
                parent_commit_id: None,
                branch: Some("main".to_string()),
            },
            TurnEvent::TurnCommitting,
            TurnEvent::TurnCommitted {
                session_id: "s1".to_string(),
                commit_id: "c1".to_string(),
                status: RunTurnStatus::Completed,
                disposition: WorkspaceDisposition::Promoted,
            },
        ])
        .unwrap();

        assert_eq!(
            result.disposition,
            WorkspaceDisposition::Promoted,
            "collector must propagate disposition from TurnCommitted"
        );
    }

    /// RED: same status with different dispositions must be distinguishable.
    #[test]
    fn turn_event_disposition_independence_same_status() {
        let result_pending = drain(vec![
            TurnEvent::TurnStarted {
                session_id: "s1".to_string(),
                parent_commit_id: None,
                branch: None,
            },
            TurnEvent::TurnCommitting,
            TurnEvent::TurnCommitted {
                session_id: "s1".to_string(),
                commit_id: "c1".to_string(),
                status: RunTurnStatus::Completed,
                disposition: WorkspaceDisposition::PendingVerification,
            },
        ])
        .unwrap();

        let result_promoted = drain(vec![
            TurnEvent::TurnStarted {
                session_id: "s2".to_string(),
                parent_commit_id: None,
                branch: None,
            },
            TurnEvent::TurnCommitting,
            TurnEvent::TurnCommitted {
                session_id: "s2".to_string(),
                commit_id: "c2".to_string(),
                status: RunTurnStatus::Completed,
                disposition: WorkspaceDisposition::Promoted,
            },
        ])
        .unwrap();

        assert_eq!(result_pending.status, result_promoted.status);
        assert_ne!(
            result_pending.disposition, result_promoted.disposition,
            "same status must allow different dispositions"
        );
    }

    /// Parity matrix: for each terminal status, verify disposition is preserved
    /// through the collector independently from status.
    #[test]
    fn disposition_parity_matrix_status_x_disposition() {
        let cases: Vec<(RunTurnStatus, WorkspaceDisposition)> = vec![
            (RunTurnStatus::Completed, WorkspaceDisposition::Promoted),
            (
                RunTurnStatus::Completed,
                WorkspaceDisposition::PendingWorktree,
            ),
            (
                RunTurnStatus::Completed,
                WorkspaceDisposition::NotApplicable,
            ),
            (RunTurnStatus::Error, WorkspaceDisposition::Rejected),
            (
                RunTurnStatus::Error,
                WorkspaceDisposition::PendingVerification,
            ),
            (RunTurnStatus::Error, WorkspaceDisposition::NotApplicable),
            (RunTurnStatus::Cancelled, WorkspaceDisposition::Aborted),
            (
                RunTurnStatus::Cancelled,
                WorkspaceDisposition::PendingConflict,
            ),
            (
                RunTurnStatus::Cancelled,
                WorkspaceDisposition::NotApplicable,
            ),
        ];

        for (status, disposition) in cases {
            let result = drain(vec![
                TurnEvent::TurnStarted {
                    session_id: "s1".to_string(),
                    parent_commit_id: None,
                    branch: None,
                },
                TurnEvent::TurnCommitting,
                TurnEvent::TurnCommitted {
                    session_id: "s1".to_string(),
                    commit_id: "c1".to_string(),
                    status: status.clone(),
                    disposition: disposition.clone(),
                },
            ])
            .unwrap_or_else(|e| panic!("drain failed for {status:?}+{disposition:?}: {e}"));

            assert_eq!(
                result.status, status,
                "status mismatch for {status:?}+{disposition:?}"
            );
            assert_eq!(
                result.disposition, disposition,
                "disposition mismatch for {status:?}+{disposition:?}"
            );
        }
    }

    /// Collector must not derive disposition from status.
    /// Verifies that the same status can produce different dispositions.
    #[test]
    fn collector_no_disposition_inference_from_status() {
        let make_result = |disposition: WorkspaceDisposition| -> crate::turn::types::RunTurnResult {
            drain(vec![
                TurnEvent::TurnStarted {
                    session_id: "s1".to_string(),
                    parent_commit_id: None,
                    branch: None,
                },
                TurnEvent::TurnCommitting,
                TurnEvent::TurnCommitted {
                    session_id: "s1".to_string(),
                    commit_id: "c1".to_string(),
                    status: RunTurnStatus::Completed,
                    disposition,
                },
            ])
            .expect("drain must succeed")
        };

        let promoted = make_result(WorkspaceDisposition::Promoted);
        let pending = make_result(WorkspaceDisposition::PendingPromotion);
        let not_applicable = make_result(WorkspaceDisposition::NotApplicable);

        // All have the same status
        assert_eq!(promoted.status, pending.status);
        assert_eq!(pending.status, not_applicable.status);

        // But different dispositions
        assert_ne!(promoted.disposition, pending.disposition);
        assert_ne!(pending.disposition, not_applicable.disposition);
        assert_ne!(promoted.disposition, not_applicable.disposition);
    }
}

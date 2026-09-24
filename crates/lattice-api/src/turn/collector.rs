// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 从公开 TurnEvent 收集 RunTurnResult。
//!
//! 测试辅助入口 run_turn 使用此收集器；后台执行的接受与完成仍走独立回执。

use super::types::{RunTurnResult, RunTurnStage, ToolResultSummary, TurnEvent, TurnUsageSummary};
use crate::session::ApiError;
use lattice_session::pending::{ClosureOrigin, TurnOutcome, WorkspaceHistoryCheckpoint};
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
    outcome: Option<TurnOutcome>,
    origin: Option<ClosureOrigin>,
    workspace_history_checkpoint: Option<WorkspaceHistoryCheckpoint>,
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
            outcome: None,
            origin: None,
            workspace_history_checkpoint: None,
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
                    // preserve assistant content emitted alongside
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
                outcome,
                origin,
                workspace_history_checkpoint,
            } => {
                self.session_id = Some(session_id);
                self.commit_id = Some(commit_id);
                self.outcome = Some(outcome);
                self.origin = Some(origin);
                self.workspace_history_checkpoint = Some(workspace_history_checkpoint);
            }
            TurnEvent::CommitFailed { reason } => {
                self.commit_error = Some(map_commit_failed(reason));
            }
            TurnEvent::EvidenceDurabilityFailed { attempts } => {
                self.commit_error = Some(ApiError::EvidenceDurabilityFailed(
                    crate::session::EvidenceDurabilityFailed { attempts },
                ));
            }
            TurnEvent::RecoveryRequired { reason } => {
                self.commit_error = Some(ApiError::RecoveryRequired(reason));
            }
            TurnEvent::TurnCommitting
            | TurnEvent::ModelStarted { .. }
            | TurnEvent::LlmRequestError { .. }
            | TurnEvent::ReasoningDelta { .. }
            | TurnEvent::ToolCallStarted { .. }
            | TurnEvent::CepCallStarted { .. }
            | TurnEvent::CepCallFinished { .. } => {}
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
        let outcome = self
            .outcome
            .ok_or_else(|| ApiError::Internal("turn ended without committed outcome".into()))?;
        let origin = self
            .origin
            .ok_or_else(|| ApiError::Internal("turn ended without closure origin".into()))?;
        let workspace_history_checkpoint = self.workspace_history_checkpoint.ok_or_else(|| {
            ApiError::Internal("turn ended without workspace-history checkpoint".into())
        })?;

        Ok(RunTurnResult {
            session_id,
            commit_id,
            assistant_output: self.assistant_output,
            outcome,
            origin,
            workspace_history_checkpoint,
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
    use lattice_session::pending::{
        ClosureOrigin, TurnFinishReason, TurnOutcome, WorkspaceHistoryCheckpoint,
    };

    #[test]
    fn collector_propagates_typed_terminal_facts() {
        let mut collector = RunTurnCollector::new();
        collector
            .observe(TurnEvent::TurnStarted {
                session_id: "s".into(),
                turn_id: "00000000-0000-4000-8000-000000000001".into(),
                parent_commit_id: None,
                branch: None,
            })
            .unwrap();
        collector
            .observe(TurnEvent::TurnCommitted {
                session_id: "s".into(),
                commit_id: "c".into(),
                outcome: TurnOutcome::Completed {
                    finish_reason: TurnFinishReason::Stop,
                },
                origin: ClosureOrigin::Live,
                workspace_history_checkpoint: WorkspaceHistoryCheckpoint::Empty,
            })
            .unwrap();
        let result = collector.finish().unwrap();
        assert!(matches!(result.outcome, TurnOutcome::Completed { .. }));
        assert_eq!(result.origin, ClosureOrigin::Live);
        assert_eq!(
            result.workspace_history_checkpoint,
            WorkspaceHistoryCheckpoint::Empty
        );
    }

    #[test]
    fn collector_returns_typed_evidence_durability_failure() {
        let error =
            collect_turn_result_from_events([TurnEvent::EvidenceDurabilityFailed { attempts: 4 }])
                .expect_err("durability failure must not become a generic commit error");
        assert!(matches!(
            error,
            ApiError::EvidenceDurabilityFailed(crate::session::EvidenceDurabilityFailed {
                attempts: 4
            })
        ));
    }

    #[test]
    fn collector_returns_typed_invocation_reconciliation_requirement() {
        let invocation_id =
            lancet_protocol::InvocationId::parse("00000000-0000-4000-8000-000000000001").unwrap();
        let reason =
            crate::session::RecoveryRequired::InvocationReconciliationRequired { invocation_id };
        let error = collect_turn_result_from_events([TurnEvent::RecoveryRequired { reason }])
            .expect_err("reconciliation must not become a generic commit error");
        assert!(matches!(
            error,
            ApiError::RecoveryRequired(
                crate::session::RecoveryRequired::InvocationReconciliationRequired {
                    invocation_id: observed
                }
            ) if observed == invocation_id
        ));
    }
}

// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Native bridge for Guard L1 policy enforcement.
//!
//! Pure authority semantics live in `lancet-authority`; this module keeps
//! the handler-facing bridge that builds policy input from `HandlerContext`,
//! emits native events, records durable metadata, and enforces the decision.

pub use lancet_authority::guard::{
    PolicyEvaluation, WritePathPattern, check_write_scope, decision_to_guard_verdict,
    evaluate_policy, map_decision_to_cep_error, normalize_write_path_pattern,
};

use crate::enforcement::preflight::approval_request_metadata;
use crate::server::registry::HandlerContext;
use lancet_protocol::CepError;
use lancet_protocol::policy::{
    PolicyAuditContext, PolicyAuthorityInput, PolicyBudgetSnapshot, PolicyDecision, PolicyInput,
};
use serde::Serialize;

/// Build a `PolicyInput` from handler context, method name, and params.
///
/// Assembles the effective envelope snapshot, audit context (including budget
/// snapshot), and serialized params into the public policy input contract.
pub async fn build_policy_input(
    ctx: &HandlerContext,
    method: &str,
    params: &impl Serialize,
) -> Result<PolicyInput, CepError> {
    let authority = PolicyAuthorityInput;

    let budget = ctx.cep_context.budget.snapshot();
    let budget_snapshot = PolicyBudgetSnapshot {
        turn_observation_bytes_used: budget.turn_observation_bytes_used as u64,
        turn_observation_bytes_limit: budget.max_turn_observation_bytes as u64,
        todo_ops_used: budget.todo_ops_used as u64,
        todo_ops_limit: budget.max_todo_ops as u64,
    };

    let audit_context = PolicyAuditContext {
        turn_id: ctx.turn_id.to_string(),
        session_id: ctx.session_id.to_string(),
        invocation_id: Some(ctx.invocation_id),
        budget_snapshot,
    };

    Ok(PolicyInput {
        method: method.to_string(),
        params: serde_json::to_value(params).map_err(|e| {
            CepError::internal_error(format!("failed to serialize policy input params: {e}"))
        })?,
        authority,
        audit_context,
    })
}

/// Evaluate policy, emit the guard verdict event, and enforce the decision.
/// This is the single native entry point for handlers to route through the
/// authority policy system.
pub async fn evaluate_and_enforce_policy(
    ctx: &HandlerContext,
    method: &str,
    params: &impl Serialize,
) -> Result<PolicyEvaluation, CepError> {
    let input = build_policy_input(ctx, method, params).await?;
    let evaluation = evaluate_policy(
        &*ctx.policy_hook,
        ctx.default_policy_hook.as_deref(),
        &input,
    )?;
    let verdict = decision_to_guard_verdict(&evaluation.decision);
    ctx.events
        .emit(
            "cep.guard_verdict",
            serde_json::json!({ "method": method, "verdict": verdict }),
        )
        .await;

    match &evaluation.decision {
        PolicyDecision::Allowed => {
            if evaluation.evidence.is_some() {
                let hook = ctx
                    .default_policy_hook
                    .as_ref()
                    .ok_or_else(|| CepError::internal_error("missing default policy hook"))?;
                let region = hook
                    .config()
                    .workspace
                    .authorized_region_for(method, &ctx.cep_context.policy);
                ctx.authorized_region.set(region);
            }
        }
        PolicyDecision::Denied { .. } => {}
        PolicyDecision::HumanReview { .. } => {
            let approval_request = approval_request_metadata(method, &input, &evaluation);
            if let Some(request) = approval_request {
                ctx.events
                    .emit(
                        "cep.approval_required",
                        serde_json::json!({ "method": method, "approval_request": request }),
                    )
                    .await;
            }
        }
    }

    map_decision_to_cep_error(&evaluation.decision, method)?;
    Ok(evaluation)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::registry::{Cancellation, EventSink};
    use lancet_authority::policy::CepTurnContext;
    use lancet_authority::policy::default::{DefaultPolicyHook, PolicyConfig};
    use lancet_protocol::policy::{PolicyHook, PolicyReasonCode};
    use std::sync::Arc;
    use tempfile::TempDir;
    use tokio::sync::{mpsc, watch};

    fn test_handler_context(
        root: &str,
    ) -> (HandlerContext, mpsc::Receiver<lancet_protocol::ToolEvent>) {
        let (event_tx, event_rx) = mpsc::channel(32);
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        let default_hook = Arc::new(DefaultPolicyHook::new(
            PolicyConfig::defaults_for_project_root(std::path::PathBuf::from(root)),
        ));
        (
            HandlerContext {
                invocation_id: lancet_protocol::InvocationId::parse(
                    "00000000-0000-4000-8000-000000000001",
                )
                .expect("invocation id"),
                events: EventSink::new(
                    event_tx,
                    lancet_protocol::InvocationId::parse("00000000-0000-4000-8000-000000000001")
                        .expect("invocation id"),
                ),
                cancellation: Cancellation::new(cancel_rx),
                cep_context: Arc::new(CepTurnContext::defaults_for_project_root(root)),
                session_id: lancet_protocol::SessionId::parse("00000000000000000000000000000001")
                    .expect("session id"),
                turn_id: lancet_protocol::TurnId::parse("00000000-0000-4000-8000-000000000001")
                    .expect("turn id"),
                effect_ledger: lancet_authority::effect::ledger::EffectLedger::new(),
                authorized_region: lancet_authority::effect::ledger::AuthorizedRegionSlot::new(),
                policy_hook: default_hook.clone() as Arc<dyn lancet_protocol::policy::PolicyHook>,
                default_policy_hook: Some(default_hook),
                cep_mode: lancet_protocol::policy::CepMode::Off,
            },
            event_rx,
        )
    }

    #[tokio::test]
    async fn build_policy_input_includes_session_id_and_budget() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let (ctx, _events) = test_handler_context(root);
        let input = build_policy_input(
            &ctx,
            "observe.workspace",
            &serde_json::json!({"mode": "read_file", "path": "src/lib.rs"}),
        )
        .await
        .unwrap();
        assert_eq!(input.method, "observe.workspace");
        assert_eq!(
            input.audit_context.turn_id,
            "00000000-0000-4000-8000-000000000001"
        );
        assert_eq!(
            input.audit_context.session_id,
            "00000000000000000000000000000001"
        );
        assert!(
            input
                .audit_context
                .budget_snapshot
                .turn_observation_bytes_limit
                > 0
        );
    }

    struct HumanReviewHook;

    impl PolicyHook for HumanReviewHook {
        fn decide(&self, _input: &PolicyInput) -> PolicyDecision {
            PolicyDecision::HumanReview {
                reason_code: PolicyReasonCode::NonDeterminable,
                message: "external approval required".into(),
            }
        }
    }

    #[tokio::test]
    async fn human_review_records_approval_ready_evidence() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let (mut ctx, mut events) = test_handler_context(root);
        ctx.policy_hook = Arc::new(HumanReviewHook);
        ctx.default_policy_hook = None;
        let err = match evaluate_and_enforce_policy(
            &ctx,
            "observe.workspace",
            &serde_json::json!({"mode": "list_directory", "path": "."}),
        )
        .await
        {
            Ok(_) => panic!("human review is not auto-approved"),
            Err(err) => err,
        };

        assert_eq!(err.code, lancet_protocol::CepErrorCode::GuardDenied);

        let guard_event = events.recv().await.expect("guard verdict event");
        assert_eq!(guard_event.method, "cep.guard_verdict");
        let approval_event = events.recv().await.expect("approval event");
        assert_eq!(approval_event.method, "cep.approval_required");
        assert_eq!(
            approval_event.params["approval_request"]["method"],
            serde_json::json!("observe.workspace")
        );
    }
}

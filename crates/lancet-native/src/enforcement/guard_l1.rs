// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Native bridge for Guard L1 policy enforcement.
//!
//! Pure authority semantics live in `lancet-authority`; this module keeps
//! the handler-facing bridge that builds policy input from `HandlerContext`,
//! emits native events, records durable metadata, and enforces the decision.

pub use lancet_authority::guard::{
    PolicyEvaluation, WritePathPattern, build_effective_envelope_snapshot, check_write_scope,
    decision_to_guard_verdict, evaluate_policy, extract_write_path_patterns,
    has_run_command_capability, map_decision_to_cep_error, normalize_write_path_pattern,
};

use crate::enforcement::preflight::{
    approval_request_metadata, policy_denial_preflight_metadata, policy_metadata,
};
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
    let authority = match ctx.authority_mode {
        lancet_protocol::policy::AuthorityMode::Envelope => {
            let active_record = ctx.active_envelope.active_record().await;
            let envelope_snapshot =
                build_effective_envelope_snapshot(&ctx.base_envelope, active_record.as_ref());
            PolicyAuthorityInput::Envelope { envelope_snapshot }
        }
        lancet_protocol::policy::AuthorityMode::Direct => PolicyAuthorityInput::Direct,
    };

    let audit_snapshot = ctx.audit_context.read().await;
    let budget = ctx.cep_context.budget.snapshot();
    let budget_snapshot = PolicyBudgetSnapshot {
        turn_observation_bytes_used: budget.turn_observation_bytes_used as u64,
        turn_observation_bytes_limit: budget.max_turn_observation_bytes as u64,
        todo_ops_used: budget.todo_ops_used as u64,
        todo_ops_limit: budget.max_todo_ops as u64,
    };

    let audit_context = PolicyAuditContext {
        turn_id: audit_snapshot.turn_id.clone(),
        session_id: audit_snapshot.session_id.clone(),
        call_id: Some(ctx.call_id),
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

/// Evaluate policy, emit guard verdict event, record metadata, and enforce the decision.
///
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
            let mut authorized_region = None;
            if let Some(evidence) = &evaluation.evidence {
                let region = match ctx.authority_mode {
                    lancet_protocol::policy::AuthorityMode::Envelope => {
                        let matched_cap = evidence
                            .capability_match
                            .as_ref()
                            .and_then(|cm| cm.matched.as_ref());
                        lancet_authority::effect::region::AuthorizedEffectRegion::from_matched_capability(
                            matched_cap,
                            &ctx.cep_context.policy,
                        )
                    }
                    lancet_protocol::policy::AuthorityMode::Direct => {
                        let command_impact = evidence.command_classification.as_ref();
                        let hook = ctx.default_policy_hook.as_ref().ok_or_else(|| {
                            CepError::internal_error("missing default policy hook")
                        })?;
                        hook.config().workspace.authorized_region_for(
                            method,
                            command_impact,
                            &ctx.cep_context.policy,
                        )
                    }
                };
                ctx.authorized_region.set(region);
                authorized_region = ctx.authorized_region.get();
            }
            ctx.record_durable_metadata(policy_metadata(
                method,
                &evaluation,
                authorized_region.as_ref(),
            ));
        }
        PolicyDecision::Denied { reason_code, .. } => {
            ctx.record_durable_metadata(policy_metadata(method, &evaluation, None));
            if ctx.authority_mode == lancet_protocol::policy::AuthorityMode::Direct {
                use lancet_audit::RejectionRecord;
                use lancet_audit::RejectionStage;
                ctx.record_rejection(RejectionRecord {
                    rejection_stage: RejectionStage::PolicyGate,
                    denial_source: "cep_policy".into(),
                });
                ctx.record_routing_metadata();
            }
            if let Some(metadata) = policy_denial_preflight_metadata(method, &input, &evaluation) {
                ctx.record_durable_metadata(metadata);
            }
            ctx.record_durable_metadata(serde_json::json!({
                "method": method,
                "denial_source": "cep_policy",
                "policy_enforced": true,
                "backend_invoked": false,
                "reason_code": format!("{:?}", reason_code),
            }));
        }
        PolicyDecision::HumanReview { reason_code, .. } => {
            let approval_request = approval_request_metadata(method, &input, &evaluation);
            ctx.record_durable_metadata(policy_metadata(method, &evaluation, None));
            ctx.record_durable_metadata(serde_json::json!({
                "method": method,
                "denial_source": "cep_policy",
                "policy_enforced": true,
                "backend_invoked": false,
                "reason_code": format!("{:?}", reason_code),
                "approval_request": approval_request.clone(),
            }));
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
    use crate::audit::CepAuditContext;
    use crate::server::registry::{Cancellation, EventSink};
    use lancet_authority::policy::CepTurnContext;
    use lancet_authority::policy::default::{DefaultPolicyHook, PolicyConfig};
    use lancet_protocol::envelope::BaseExecutionEnvelope;
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
                call_id: 1,
                events: EventSink::new(event_tx, 1),
                cancellation: Cancellation::new(cancel_rx),
                base_envelope: BaseExecutionEnvelope::for_workspace_root(root, "policy test"),
                cep_context: Arc::new(CepTurnContext::defaults_for_project_root(root)),
                active_envelope: Arc::new(
                    lancet_authority::envelope_store::ActiveEnvelopeStore::new(),
                ),
                turn_id: "test-turn".into(),
                durable_metadata:
                    crate::server::call_durable_metadata::CallDurableMetadata::default(),
                effect_ledger: lancet_authority::effect::ledger::EffectLedger::new(),
                authorized_region: lancet_authority::effect::ledger::AuthorizedRegionSlot::new(),
                policy_hook: default_hook.clone() as Arc<dyn PolicyHook>,
                default_policy_hook: Some(default_hook),
                audit_context: Arc::new(tokio::sync::RwLock::new(CepAuditContext {
                    turn_id: "test-turn".into(),
                    session_id: "test-session".into(),
                    ..Default::default()
                })),
                audit: Arc::new(crate::audit::CepAuditStore::in_memory().expect("audit store")),
                cep_mode: lancet_protocol::envelope::CepMode::Off,
                command_isolation:
                    crate::enforcement::isolation::CommandIsolationRuntimeConfig::default(),
                authority_mode: lancet_protocol::policy::AuthorityMode::default(),
                execution: None,
                direct_verify: None,
                routing_invocation: crate::execution_backend::RoutingInvocationState::default(),
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
        assert_eq!(input.audit_context.turn_id, "test-turn");
        assert_eq!(input.audit_context.session_id, "test-session");
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
        let durable = ctx.durable_metadata.clone();

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
        let metadata: serde_json::Value =
            serde_json::from_str(&durable.take_json().expect("durable metadata")).unwrap();
        assert_eq!(metadata["backend_invoked"], false);
        assert_eq!(metadata["cep_policy"]["decision"], "human_review");
        assert_eq!(
            metadata["approval_request"]["status"],
            serde_json::json!("required")
        );
        assert_eq!(
            metadata["approval_request"]["retry_hint"],
            serde_json::json!("obtain_human_approval_or_replan")
        );

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

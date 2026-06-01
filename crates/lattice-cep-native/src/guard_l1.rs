//! Native bridge for Guard L1 policy enforcement.
//!
//! Pure authority semantics live in `lattice-cep-authority`; this module keeps
//! the handler-facing bridge that builds policy input from `HandlerContext`,
//! emits native events, records durable metadata, and enforces the decision.

pub use lattice_cep_authority::guard::{
    build_effective_envelope_snapshot, check_write_scope, decision_to_guard_verdict,
    evaluate_policy, extract_write_path_patterns, has_run_command_capability,
    map_decision_to_cep_error, normalize_write_path_pattern, PolicyEvaluation, WritePathPattern,
};

use crate::registry::HandlerContext;
use lattice_cep_protocol::policy::{
    PolicyAuditContext, PolicyBudgetSnapshot, PolicyDecision, PolicyInput,
};
use lattice_cep_protocol::CepError;
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
    let active_record = ctx.active_envelope.active_record().await;
    let envelope_snapshot =
        build_effective_envelope_snapshot(&ctx.base_envelope, active_record.as_ref());

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
        envelope_snapshot,
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
            if let Some(evidence) = &evaluation.evidence {
                ctx.record_durable_metadata(serde_json::json!({
                    "cep_policy": {
                        "capability_match": evidence.capability_match.is_some(),
                        "budget_remaining": evidence.budget_remaining,
                    }
                }));

                let matched_cap = evidence
                    .capability_match
                    .as_ref()
                    .and_then(|cm| cm.matched.as_ref());
                let region =
                    lattice_cep_authority::effect::region::AuthorizedEffectRegion::from_matched_capability(
                        matched_cap,
                        &ctx.cep_context.policy,
                    );
                ctx.authorized_region.set(region);
            }
        }
        PolicyDecision::Denied { reason_code, .. }
        | PolicyDecision::HumanReview { reason_code, .. } => {
            ctx.record_durable_metadata(serde_json::json!({
                "method": method,
                "denial_source": "cep_policy",
                "policy_enforced": true,
                "backend_invoked": false,
                "reason_code": format!("{:?}", reason_code),
            }));
        }
    }

    map_decision_to_cep_error(&evaluation.decision, method)?;
    Ok(evaluation)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::CepAuditContext;
    use crate::policy::default::{DefaultPolicyHook, PolicyConfig};
    use crate::policy::CepTurnContext;
    use crate::registry::{Cancellation, EventSink};
    use lattice_cep_protocol::envelope::BaseExecutionEnvelope;
    use lattice_cep_protocol::policy::PolicyHook;
    use std::sync::Arc;
    use tempfile::TempDir;
    use tokio::sync::{mpsc, watch};

    fn test_handler_context(
        root: &str,
    ) -> (
        HandlerContext,
        mpsc::Receiver<lattice_cep_protocol::ToolEvent>,
    ) {
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
                active_envelope: Arc::new(crate::envelope_store::ActiveEnvelopeStore::new()),
                turn_id: "test-turn".into(),
                durable_metadata: crate::call_durable_metadata::CallDurableMetadata::default(),
                effect_ledger: crate::effect::ledger::EffectLedger::new(),
                authorized_region: crate::effect::ledger::AuthorizedRegionSlot::new(),
                policy_hook: default_hook.clone() as Arc<dyn PolicyHook>,
                default_policy_hook: Some(default_hook),
                audit_context: Arc::new(tokio::sync::RwLock::new(CepAuditContext {
                    turn_id: "test-turn".into(),
                    session_id: "test-session".into(),
                    ..Default::default()
                })),
                audit: Arc::new(crate::audit::CepAuditStore::in_memory().expect("audit store")),
                invariant_index: Arc::new(crate::invariants::InvariantCardIndex::new(vec![])),
                cep_mode: lattice_cep_protocol::envelope::CepMode::Off,
            },
            event_rx,
        )
    }

    #[tokio::test]
    async fn build_policy_input_includes_session_id_and_budget() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let (ctx, _events) = test_handler_context(root);
        let input = build_policy_input(&ctx, "workspace.read_file", &serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(input.method, "workspace.read_file");
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
}

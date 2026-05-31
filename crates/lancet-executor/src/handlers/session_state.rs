//! Native CEP `session.todo_write` CEP handler.

use crate::effect::ledger::{EffectSource, ObservedEffect, ObservedScope};
use crate::guard::evaluate_and_enforce_policy;
use crate::registry::{HandlerContext, MethodRegistry};
use lancet_protocol::envelope::ImpactKind;
use lancet_protocol::error::ValidationErrorData;
use lancet_protocol::method::RequiredCapability;
use lancet_protocol::methods::session::{
    TodoItem, TodoStatus, TodoWrite, TodoWriteParams, TodoWriteResult,
};
use lancet_protocol::{CepError, CepErrorCode, CepErrorData, CepMethod, Retriability};
use serde_json::json;

/// Register native `session.todo_write`.
pub fn register_session_state_methods(registry: &mut MethodRegistry) -> Result<(), CepError> {
    registry.register_method::<TodoWrite, _, _>(handle_todo_write)
}

async fn handle_todo_write(
    params: TodoWriteParams,
    ctx: HandlerContext,
) -> Result<TodoWriteResult, CepError> {
    let _evaluation = evaluate_and_enforce_policy(&ctx, TodoWrite::NAME, &params).await?;

    if let Err(err) = validate_todos(&params.todos) {
        emit_denial_metadata(&ctx, 0, "validation").await;
        return Err(err);
    }

    let op_count = params.todos.len();
    if let Err(budget_err) = ctx.cep_context.budget.try_charge_todo_ops(op_count) {
        emit_denial_metadata(&ctx, op_count, "cep_budget").await;
        return Err(budget_err.to_cep_error());
    }

    emit_success_metadata(&ctx, op_count).await;

    ctx.record_effect(ObservedEffect {
        kind: ImpactKind::SessionStateMutation,
        scope: ObservedScope::SessionState,
        required_capability: RequiredCapability::MutateSessionState,
        source: EffectSource::HandlerBackend,
    });

    Ok(TodoWriteResult {
        todos: params.todos,
    })
}

fn validate_todos(todos: &[TodoItem]) -> Result<(), CepError> {
    let mut in_progress = 0usize;
    for (index, todo) in todos.iter().enumerate() {
        if todo.content.trim().is_empty() {
            return Err(validation_error(format!(
                "todo {} has empty content",
                index + 1
            )));
        }
        if matches!(todo.status, TodoStatus::InProgress) {
            in_progress += 1;
        }
    }
    if in_progress > 1 {
        return Err(validation_error(
            "todo_write allows at most one in_progress todo",
        ));
    }
    Ok(())
}

fn validation_error(reason: impl Into<String>) -> CepError {
    let reason = reason.into();
    CepError::new(
        CepErrorCode::TypeError,
        reason.clone(),
        CepErrorData::Validation(ValidationErrorData {
            field: "todos".into(),
            reason,
        }),
        Retriability::Permanent,
    )
}

async fn emit_success_metadata(ctx: &HandlerContext, op_count: usize) {
    let metadata = json!({
        "method": TodoWrite::NAME,
        "op_count": op_count,
        "policy_enforced": true,
        "backend_invoked": true,
        "budget_charged": 0,
    });
    ctx.record_durable_metadata(metadata.clone());
    ctx.events
        .emit(
            "cep.native_session_metadata",
            json!({ "method": TodoWrite::NAME, "metadata": metadata }),
        )
        .await;
}

async fn emit_denial_metadata(ctx: &HandlerContext, op_count: usize, denial_source: &str) {
    let metadata = json!({
        "method": TodoWrite::NAME,
        "op_count": op_count,
        "denial_source": denial_source,
        "policy_enforced": true,
        "backend_invoked": false,
        "budget_charged": 0,
    });
    ctx.record_durable_metadata(metadata.clone());
    ctx.events
        .emit(
            "cep.native_session_metadata",
            json!({ "method": TodoWrite::NAME, "metadata": metadata }),
        )
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::CepTurnContext;
    use crate::registry::{Cancellation, EventSink};
    use lancet_protocol::envelope::BaseExecutionEnvelope;
    use std::sync::Arc;
    use tempfile::TempDir;
    use tokio::sync::{mpsc, watch};

    fn test_registry() -> MethodRegistry {
        let mut registry = MethodRegistry::new();
        register_session_state_methods(&mut registry).expect("register session.todo_write");
        registry
    }

    fn ctx_for(
        cep_context: Arc<CepTurnContext>,
    ) -> (HandlerContext, mpsc::Receiver<lancet_protocol::ToolEvent>) {
        let (event_tx, event_rx) = mpsc::channel(16);
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        let default_hook = Arc::new(crate::policy::default::DefaultPolicyHook::new(
            crate::policy::default::PolicyConfig::defaults_for_project_root(
                std::path::PathBuf::from("."),
            ),
        ));
        (
            HandlerContext {
                call_id: 9,
                events: EventSink::new(event_tx, 9),
                cancellation: Cancellation::new(cancel_rx),
                base_envelope: BaseExecutionEnvelope::for_workspace_root(".", "todo test")
                    .with_session_state(),
                cep_context,
                active_envelope: Arc::new(crate::envelope_store::ActiveEnvelopeStore::new()),
                turn_id: "test-turn".into(),
                durable_metadata: crate::call_durable_metadata::CallDurableMetadata::default(),
                effect_ledger: crate::effect::ledger::EffectLedger::new(),
                authorized_region: crate::effect::ledger::AuthorizedRegionSlot::new(),
                policy_hook: default_hook.clone() as Arc<dyn lancet_protocol::policy::PolicyHook>,
                default_policy_hook: Some(default_hook),
                audit_context: Arc::new(tokio::sync::RwLock::new(crate::audit::CepAuditContext {
                    turn_id: "test-turn".into(),
                    session_id: "test-session".into(),
                    ..Default::default()
                })),
                audit: Arc::new(crate::audit::CepAuditStore::in_memory().expect("audit store")),
                invariant_index: Arc::new(crate::invariants::InvariantCardIndex::new(vec![])),
                cep_mode: lancet_protocol::envelope::CepMode::Off,
            },
            event_rx,
        )
    }

    #[tokio::test]
    async fn native_todo_write_succeeds_without_workspace_envelope() {
        let cep_context = Arc::new(CepTurnContext::defaults_for_project_root(
            TempDir::new().unwrap().path(),
        ));
        let registry = test_registry();
        let (ctx, mut events) = ctx_for(cep_context);

        let params = TodoWriteParams {
            todos: vec![
                TodoItem {
                    content: "ship native handler".into(),
                    status: TodoStatus::InProgress,
                },
                TodoItem {
                    content: "verify audit".into(),
                    status: TodoStatus::Pending,
                },
            ],
        };
        let result = registry
            .dispatch(TodoWrite::NAME, serde_json::to_value(&params).unwrap(), ctx)
            .await
            .unwrap();
        let parsed: TodoWriteResult = serde_json::from_value(result).unwrap();
        assert_eq!(parsed.todos.len(), 2);

        let guard_event = events.recv().await.unwrap();
        assert_eq!(guard_event.method, "cep.guard_verdict");

        let event = events.recv().await.unwrap();
        assert_eq!(event.method, "cep.native_session_metadata");
        assert_eq!(event.params["metadata"]["op_count"], 2);
        assert_eq!(event.params["metadata"]["budget_charged"], 0);
        let metadata = event.params["metadata"].to_string();
        assert!(!metadata.contains("ship native"));
    }

    #[tokio::test]
    async fn native_todo_write_rejects_over_max_todo_ops() {
        let mut policy = crate::policy::CepPolicyConfig::defaults_for_project_root(
            TempDir::new().unwrap().path(),
        );
        policy.observation.max_todo_ops = 1;
        policy.budget.max_todo_ops = 1;
        let cep_context = Arc::new(CepTurnContext::from_policy(policy));
        let registry = test_registry();

        let item = TodoWriteParams {
            todos: vec![TodoItem {
                content: "first".into(),
                status: TodoStatus::Pending,
            }],
        };
        let (ctx_ok, _events) = ctx_for(Arc::clone(&cep_context));
        registry
            .dispatch(
                TodoWrite::NAME,
                serde_json::to_value(&item).unwrap(),
                ctx_ok,
            )
            .await
            .unwrap();

        let (ctx_denied, mut events) = ctx_for(cep_context);
        let err = registry
            .dispatch(
                TodoWrite::NAME,
                serde_json::to_value(&item).unwrap(),
                ctx_denied,
            )
            .await
            .unwrap_err();
        assert_eq!(err.code, CepErrorCode::Reserved);

        let guard_event = events.recv().await.unwrap();
        assert_eq!(guard_event.method, "cep.guard_verdict");
    }

    #[tokio::test]
    async fn native_todo_write_rejects_many_todos_in_one_call_over_cap() {
        let mut policy = crate::policy::CepPolicyConfig::defaults_for_project_root(
            TempDir::new().unwrap().path(),
        );
        policy.observation.max_todo_ops = 1;
        policy.budget.max_todo_ops = 1;
        let cep_context = Arc::new(CepTurnContext::from_policy(policy));
        let registry = test_registry();
        let (ctx, mut events) = ctx_for(cep_context);

        let params = TodoWriteParams {
            todos: vec![
                TodoItem {
                    content: "a".into(),
                    status: TodoStatus::Pending,
                },
                TodoItem {
                    content: "b".into(),
                    status: TodoStatus::Pending,
                },
            ],
        };
        let err = registry
            .dispatch(TodoWrite::NAME, serde_json::to_value(&params).unwrap(), ctx)
            .await
            .unwrap_err();
        assert_eq!(err.code, CepErrorCode::Reserved);

        let guard_event = events.recv().await.unwrap();
        assert_eq!(guard_event.method, "cep.guard_verdict");
    }

    #[tokio::test]
    async fn native_todo_write_rejects_two_in_progress() {
        let cep_context = Arc::new(CepTurnContext::defaults_for_project_root(
            TempDir::new().unwrap().path(),
        ));
        let registry = test_registry();
        let (ctx, _events) = ctx_for(cep_context);

        let params = TodoWriteParams {
            todos: vec![
                TodoItem {
                    content: "a".into(),
                    status: TodoStatus::InProgress,
                },
                TodoItem {
                    content: "b".into(),
                    status: TodoStatus::InProgress,
                },
            ],
        };
        let err = registry
            .dispatch(TodoWrite::NAME, serde_json::to_value(&params).unwrap(), ctx)
            .await
            .unwrap_err();
        assert_eq!(err.code, CepErrorCode::TypeError);
    }
}

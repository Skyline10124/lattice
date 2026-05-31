//! Native `audit.query` and `audit.replay` handlers (PROV-02 / PROV-04).

use crate::audit::{AuditError, AuditQueryFilters};
use crate::guard::evaluate_and_enforce_policy;
use crate::provenance::{project_rows, project_turn_trace};
use crate::registry::{HandlerContext, MethodRegistry};
use lancet_protocol::envelope::ImpactKind;
use lancet_protocol::error::ValidationErrorData;
use lancet_protocol::method::RequiredCapability;
use lancet_protocol::methods::audit::{
    AuditQuery, AuditQueryParams, AuditQueryResult, AuditReplay, AuditReplayParams,
    AuditReplayResult, AuditTail, AuditTailParams, AuditTailResult,
};
use lancet_protocol::provenance::InTotoStatement;
use lancet_protocol::{CepError, CepErrorCode, CepErrorData, CepMethod, Retriability};
use serde_json::json;

/// Maximum rows emitted per `audit.tail` invocation (T-26-05b DoS mitigation).
pub const AUDIT_TAIL_MAX_ROWS: usize = 1000;

/// Register `audit.query`, `audit.tail`, and `audit.replay`.
pub fn register_audit_methods(registry: &mut MethodRegistry) -> Result<(), CepError> {
    registry.register_method::<AuditQuery, _, _>(handle_audit_query)?;
    registry.register_method::<AuditTail, _, _>(handle_audit_tail)?;
    registry.register_method::<AuditReplay, _, _>(handle_audit_replay)?;
    Ok(())
}

async fn handle_audit_query(
    params: AuditQueryParams,
    ctx: HandlerContext,
) -> Result<AuditQueryResult, CepError> {
    let _evaluation = evaluate_and_enforce_policy(&ctx, AuditQuery::NAME, &params).await?;

    let session_id = effective_session_id(&ctx, params.session_id.as_deref()).await;
    let filters = audit_query_filters_from_params(&params, &session_id);

    let rows = ctx.audit.query_rows(&filters).map_err(audit_error_to_cep)?;

    let row_count = rows.len();
    let fragment = project_rows(&rows);

    let filter_summary = serde_json::to_string(&json!({
        "session_id": session_id,
        "method": params.method,
        "turn_id": params.turn_id,
        "verdict": params.verdict,
        "row_count": row_count,
    }))
    .unwrap_or_default();

    ctx.record_durable_metadata(json!({
        "row_count": row_count,
        "filter_summary": filter_summary,
    }));

    ctx.record_effect(crate::effect::ledger::ObservedEffect {
        kind: ImpactKind::AuditObservation,
        scope: crate::effect::ledger::ObservedScope::SessionState,
        required_capability: RequiredCapability::ReadAudit,
        source: crate::effect::ledger::EffectSource::HandlerBackend,
    });

    Ok(AuditQueryResult {
        fragment,
        row_count,
    })
}

async fn handle_audit_tail(
    params: AuditTailParams,
    ctx: HandlerContext,
) -> Result<AuditTailResult, CepError> {
    let _evaluation = evaluate_and_enforce_policy(&ctx, AuditTail::NAME, &params).await?;

    let session_id = effective_session_id(&ctx, None).await;
    let since_filter = if params.since_audit_id > 0 {
        Some(params.since_audit_id + 1)
    } else {
        None
    };
    let filters = AuditQueryFilters {
        session_id: Some(session_id.clone()),
        since_audit_id: since_filter,
        ..Default::default()
    };

    let rows: Vec<_> = ctx
        .audit
        .query_rows(&filters)
        .map_err(audit_error_to_cep)?
        .into_iter()
        .filter(|row| !row.method.starts_with("audit."))
        .collect();

    let capped = rows.len() > AUDIT_TAIL_MAX_ROWS;
    let emit_rows = if capped {
        &rows[..AUDIT_TAIL_MAX_ROWS]
    } else {
        &rows[..]
    };

    for row in emit_rows {
        ctx.events
            .emit(
                "audit.tail_row",
                json!({
                    "audit_id": row.audit_id,
                    "method": row.method,
                    "impact_kind": row.impact_kind,
                    "summary": tail_row_summary(row),
                }),
            )
            .await;
    }

    let last_audit_id = emit_rows
        .last()
        .map(|r| r.audit_id)
        .unwrap_or(params.since_audit_id);

    ctx.record_durable_metadata(json!({
        "row_count": emit_rows.len(),
        "last_audit_id": last_audit_id,
        "done": !capped,
        "session_id": session_id,
    }));

    ctx.record_effect(crate::effect::ledger::ObservedEffect {
        kind: ImpactKind::AuditObservation,
        scope: crate::effect::ledger::ObservedScope::SessionState,
        required_capability: RequiredCapability::ReadAudit,
        source: crate::effect::ledger::EffectSource::HandlerBackend,
    });

    Ok(AuditTailResult {
        last_audit_id,
        done: !capped,
    })
}

fn tail_row_summary(row: &crate::audit::AuditRow) -> String {
    row.result_or_error
        .clone()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("{} (in progress)", row.method))
}

async fn handle_audit_replay(
    params: AuditReplayParams,
    ctx: HandlerContext,
) -> Result<AuditReplayResult, CepError> {
    let _evaluation = evaluate_and_enforce_policy(&ctx, AuditReplay::NAME, &params).await?;

    if params.turn_id.trim().is_empty() {
        return Err(validation_error("turn_id", "turn_id is required"));
    }

    let session_id = effective_session_id(&ctx, None).await;
    let filters = AuditQueryFilters {
        session_id: Some(session_id.clone()),
        turn_id: Some(params.turn_id.clone()),
        ..Default::default()
    };

    let rows = ctx.audit.query_rows(&filters).map_err(audit_error_to_cep)?;

    if rows.is_empty() {
        return Err(CepError::new(
            CepErrorCode::StaleContext,
            format!(
                "no audit rows for turn_id={} in session {}",
                params.turn_id, session_id
            ),
            CepErrorData::Validation(ValidationErrorData {
                field: "turn_id".into(),
                reason: "no matching audit rows".into(),
            }),
            Retriability::Permanent,
        ));
    }

    let runtime = rows
        .iter()
        .find_map(|r| r.runtime_version.as_deref().filter(|s| !s.is_empty()))
        .unwrap_or("lattice")
        .to_string();

    let trace = project_turn_trace(&params.turn_id, &rows);
    let statement = Some(crate::provenance::build_in_toto_statement(
        &params.turn_id,
        &rows,
        &runtime,
    ));

    let stored_statement_match = match ctx.audit.attestation_for_turn(&params.turn_id) {
        Ok(Some(record)) => {
            stored_statement_matches_derived(&record.statement_json, statement.as_ref())
        }
        _ => None,
    };

    ctx.record_durable_metadata(json!({
        "turn_id": params.turn_id,
        "row_count": rows.len(),
        "stored_statement_match": stored_statement_match,
        "source_audit_range_or_hash": crate::provenance::source_audit_range_hash(&rows),
    }));

    ctx.record_effect(crate::effect::ledger::ObservedEffect {
        kind: ImpactKind::AuditObservation,
        scope: crate::effect::ledger::ObservedScope::SessionState,
        required_capability: RequiredCapability::ReadAudit,
        source: crate::effect::ledger::EffectSource::HandlerBackend,
    });

    Ok(AuditReplayResult { trace, statement })
}

fn audit_query_filters_from_params(
    params: &AuditQueryParams,
    session_id: &str,
) -> AuditQueryFilters {
    AuditQueryFilters {
        session_id: Some(session_id.to_string()),
        turn_id: params.turn_id.clone(),
        method: params.method.clone(),
        impact_kind: params
            .impact_kind
            .as_ref()
            .map(|k| crate::audit::impact_kind_label(k).to_string()),
        capability: params.capability.clone(),
        verdict: params.verdict.clone(),
        since_audit_id: params.since_audit_id,
        until_audit_id: params.until_audit_id,
        envelope_id: params.envelope_id.clone(),
        started_after: None,
        started_before: None,
    }
}

async fn effective_session_id(ctx: &HandlerContext, param_override: Option<&str>) -> String {
    #[cfg(test)]
    if let Some(s) = param_override {
        return s.to_string();
    }
    #[cfg(not(test))]
    let _ = param_override;
    ctx.audit_context.read().await.session_id.clone()
}

fn stored_statement_matches_derived(
    stored_json: &str,
    derived: Option<&InTotoStatement>,
) -> Option<bool> {
    let derived = derived?;
    let stored: InTotoStatement = serde_json::from_str(stored_json).ok()?;
    Some(stored.predicate_type == derived.predicate_type && stored.predicate == derived.predicate)
}

fn audit_error_to_cep(err: AuditError) -> CepError {
    CepError::internal_error(format!("audit store error: {err}"))
}

fn validation_error(field: &str, reason: impl Into<String>) -> CepError {
    let reason = reason.into();
    CepError::new(
        CepErrorCode::TypeError,
        reason.clone(),
        CepErrorData::Validation(ValidationErrorData {
            field: field.into(),
            reason,
        }),
        Retriability::Permanent,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::{CallAuditMetadata, CepAuditContext, CepAuditStore};
    use crate::native_registry::register_production_method_registry;
    use crate::policy::Phase21TurnContext;
    use crate::transport::{LancetTransport, ServerRunConfig};
    use lancet_protocol::envelope::BaseExecutionEnvelope;
    use lancet_protocol::methods::audit::{AuditTail, AuditTailParams};
    use lancet_protocol::methods::workspace::ReadFile;
    use std::sync::Arc;
    use tempfile::TempDir;

    fn registry_with_audit() -> MethodRegistry {
        let mut registry = MethodRegistry::new();
        register_production_method_registry(&mut registry).unwrap();
        registry
    }

    fn seed_read_row(audit: &CepAuditStore, session_id: &str, turn_id: &str) {
        let ctx = CepAuditContext {
            turn_id: turn_id.into(),
            session_id: session_id.into(),
            model_id: "test-model".into(),
            runtime_version: "lattice-test/1".into(),
        };
        let start = audit
            .start_call(
                &ctx,
                1,
                ReadFile::NAME,
                &CallAuditMetadata::native_policy_enforced(Some("started")),
            )
            .unwrap();
        audit
            .finalize_call(
                start,
                crate::audit::AuditOutcome::Success {
                    summary: "read ok".into(),
                },
                Some("native_handler"),
                Some(r#"{"content_hash":"sha256:abc","canonical_target":"hello.txt"}"#),
            )
            .unwrap();
    }

    #[tokio::test]
    async fn audit_query_returns_fragment() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("hello.txt"), "hi").unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = registry_with_audit();

        let mut config = ServerRunConfig::in_memory_for_tests();
        config.base_envelope =
            BaseExecutionEnvelope::for_workspace_root(root, "audit query test").with_read_audit();
        config.phase21 = Arc::new(Phase21TurnContext::defaults_for_project_root(root));
        config.production_audit_routing = true;
        {
            let mut snap = config.audit_context.write().await;
            snap.session_id = "sess-audit".into();
            snap.turn_id = "turn-seed".into();
        }

        seed_read_row(&config.audit, "sess-audit", "turn-seed");

        let (client, server) = LancetTransport::pair(8);
        let server_task = tokio::spawn(async move {
            server.run(registry, config).await.unwrap();
        });

        let result = client
            .call::<AuditQuery>(AuditQueryParams {
                session_id: Some("sess-audit".into()),
                ..Default::default()
            })
            .await
            .unwrap();

        assert!(result.row_count >= 1);
        assert!(!result.fragment.activities.is_empty());
        assert!(!result.fragment.agents.is_empty());

        drop(client);
        server_task.abort();
    }

    #[tokio::test]
    async fn audit_replay_returns_trace_for_turn() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = registry_with_audit();

        let mut config = ServerRunConfig::in_memory_for_tests();
        config.base_envelope =
            BaseExecutionEnvelope::for_workspace_root(root, "audit replay test").with_read_audit();
        config.phase21 = Arc::new(Phase21TurnContext::defaults_for_project_root(root));
        config.production_audit_routing = true;
        {
            let mut snap = config.audit_context.write().await;
            snap.session_id = "sess-replay".into();
            snap.turn_id = "turn-replay".into();
        }

        seed_read_row(&config.audit, "sess-replay", "turn-replay");

        let (client, server) = LancetTransport::pair(8);
        let server_task = tokio::spawn(async move {
            server.run(registry, config).await.unwrap();
        });

        let result = client
            .call::<AuditReplay>(AuditReplayParams {
                turn_id: "turn-replay".into(),
            })
            .await
            .unwrap();

        assert_eq!(result.trace.turn_id, "turn-replay");
        assert!(!result.trace.calls.is_empty());
        assert!(result.statement.is_some());

        drop(client);
        server_task.abort();
    }

    #[tokio::test]
    async fn audit_replay_empty_turn_errors() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = registry_with_audit();

        let mut config = ServerRunConfig::in_memory_for_tests();
        config.base_envelope =
            BaseExecutionEnvelope::for_workspace_root(root, "audit replay empty").with_read_audit();
        config.phase21 = Arc::new(Phase21TurnContext::defaults_for_project_root(root));
        {
            let mut snap = config.audit_context.write().await;
            snap.session_id = "sess-empty".into();
        }

        let (client, server) = LancetTransport::pair(8);
        let server_task = tokio::spawn(async move {
            server.run(registry, config).await.unwrap();
        });

        let err = client
            .call::<AuditReplay>(AuditReplayParams {
                turn_id: "missing-turn".into(),
            })
            .await
            .unwrap_err();
        assert_eq!(err.code, CepErrorCode::StaleContext);

        drop(client);
        server_task.abort();
    }

    #[test]
    fn production_registry_registers_audit_query_and_replay() {
        let registry = registry_with_audit();
        assert!(registry.contains(AuditQuery::NAME));
        assert!(registry.contains(AuditReplay::NAME));
        assert!(registry.contains(AuditTail::NAME));
    }

    fn seed_rows(audit: &CepAuditStore, session_id: &str, turn_id: &str, count: usize) {
        for i in 0..count {
            let ctx = CepAuditContext {
                turn_id: turn_id.into(),
                session_id: session_id.into(),
                model_id: "test-model".into(),
                runtime_version: "lattice-test/1".into(),
            };
            let start = audit
                .start_call(
                    &ctx,
                    i as u64 + 1,
                    ReadFile::NAME,
                    &CallAuditMetadata::native_policy_enforced(Some("started")),
                )
                .unwrap();
            audit
                .finalize_call(
                    start,
                    crate::audit::AuditOutcome::Success {
                        summary: format!("read ok {i}"),
                    },
                    Some("native_handler"),
                    Some(r#"{"content_hash":"sha256:abc"}"#),
                )
                .unwrap();
        }
    }

    #[tokio::test]
    async fn audit_tail_streams_rows_in_order() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = registry_with_audit();

        let mut config = ServerRunConfig::in_memory_for_tests();
        config.base_envelope =
            BaseExecutionEnvelope::for_workspace_root(root, "audit tail test").with_read_audit();
        config.phase21 = Arc::new(Phase21TurnContext::defaults_for_project_root(root));
        config.production_audit_routing = true;
        {
            let mut snap = config.audit_context.write().await;
            snap.session_id = "sess-tail".into();
            snap.turn_id = "turn-tail".into();
        }

        seed_rows(&config.audit, "sess-tail", "turn-tail", 3);

        let (client, server) = LancetTransport::pair(16);
        let events = client.events();
        let server_task = tokio::spawn(async move {
            server.run(registry, config).await.unwrap();
        });

        let call = client.call::<AuditTail>(AuditTailParams { since_audit_id: 0 });
        let mut collected = Vec::new();
        let collect = async {
            loop {
                let Some(event) = events.lock().await.recv().await else {
                    break;
                };
                let finished = event.method == "cep.call_finished";
                collected.push(event);
                if finished {
                    break;
                }
            }
        };

        let (result, _) = tokio::join!(call, collect);
        let tail_result = result.unwrap();
        assert!(tail_result.done);
        assert_eq!(tail_result.last_audit_id, 3);

        let methods: Vec<&str> = collected.iter().map(|e| e.method.as_str()).collect();
        assert_eq!(methods[0], "cep.call_started");
        assert_eq!(methods.last().copied(), Some("cep.call_finished"));
        let tail_rows: Vec<_> = collected
            .iter()
            .filter(|e| e.method == "audit.tail_row")
            .collect();
        assert_eq!(tail_rows.len(), 3);

        assert_eq!(tail_rows[0].params["audit_id"], 1);
        assert_eq!(tail_rows[1].params["audit_id"], 2);
        assert_eq!(tail_rows[2].params["audit_id"], 3);

        for event in collected.iter().filter(|e| e.method != "cep.guard_verdict") {
            assert_eq!(event.params["call_id"], 1);
        }

        drop(client);
        server_task.abort();
    }
}

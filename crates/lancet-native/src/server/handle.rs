// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Per-call dispatch path: durable admission and Component/native execution.

#[path = "handle_effect.rs"]
mod effect;
#[path = "handle_helpers.rs"]
mod helpers;

use self::effect::apply_effect_check;
use self::helpers::{
    admission_denied_error, call_cancelled_error, emit_cep_call_finished, emit_cep_call_started,
};
use super::hosts::{
    is_component_method, network_fetch_host, workspace_mutation_host, workspace_observation_host,
};
use super::transport_config::ServerRunConfig;
use crate::server::evidence_respond::triple_from_ledger;
use crate::server::registry::{HandlerContext, MethodRegistry};
use crate::workspace::LocalWorkspace;
use lancet_audit::invocation_store::InvocationStoreError;
use lancet_dispatch::ServerCall;
use lancet_protocol::error::StateErrorData;
use lancet_protocol::{CepError, CepErrorCode, CepErrorData, Retriability};
use lancet_runtime::{CellEvidenceLedger, ComponentCallContext, admit_tool};
use serde_json::Value;
use std::sync::Arc;

fn admission_task_error() -> CepError {
    CepError::internal_error("invocation admission unavailable")
}

fn admission_error(err: InvocationStoreError) -> CepError {
    match err {
        InvocationStoreError::InvalidTransition { .. } => CepError::new(
            CepErrorCode::ResourceConflict,
            "invocation admission rejected",
            CepErrorData::State(StateErrorData {
                resource: "invocation".into(),
                reason: "duplicate or non-reserved admission".into(),
            }),
            Retriability::Permanent,
        ),
        InvocationStoreError::NotFound { .. }
        | InvocationStoreError::Corruption { .. }
        | InvocationStoreError::InvalidReservation { .. } => {
            CepError::invalid_params("invocation binding rejected")
        }
        _ => CepError::internal_error("invocation store admission failed"),
    }
}

pub(super) async fn handle_request(
    registry: MethodRegistry,
    call: ServerCall,
    config: ServerRunConfig,
    workspace: Arc<LocalWorkspace>,
) {
    let ServerRunConfig {
        invocations,
        test_state_guard: _,
        cep_context,
        policy_hook,
        default_policy_hook,
        cep_mode,
        components,
        workspace_history,
    } = config;

    let binding = call.binding().clone();
    let invocation_id = binding.invocation_id;
    let method = call.method().to_string();
    let params = call.params().clone();
    let events = call.event_sink();
    let cancellation = call.cancellation();

    if cancellation.is_cancelled() {
        call.respond(Err(call_cancelled_error(&method)));
        return;
    }

    if !binding.matches_request(&method, &params) {
        call.respond(Err(CepError::invalid_params(
            "invocation binding does not match request method/arguments",
        )));
        return;
    }

    let admission_store = Arc::clone(&invocations);
    let admission_binding = binding.clone();
    let admission =
        tokio::task::spawn_blocking(move || admission_store.admit_reserved(&admission_binding))
            .await;
    match admission {
        Ok(Ok(())) => {}
        Ok(Err(err)) => {
            call.respond(Err(admission_error(err)));
            return;
        }
        Err(_join_error) => {
            call.respond(Err(admission_task_error()));
            return;
        }
    }
    call.mark_dispatch_started();
    #[cfg(feature = "test-crash-barriers")]
    stop_after_dispatch_started(&invocations, invocation_id);
    let is_component = is_component_method(components.as_deref(), &method);
    let (semantic_stage, guard_stage, policy_enforced) = if is_component {
        ("cell_component", "cell_admission", true)
    } else {
        ("cep_native", "cep_policy_lifecycle", true)
    };
    emit_cep_call_started(
        &events,
        invocation_id,
        &method,
        semantic_stage,
        guard_stage,
        policy_enforced,
    )
    .await;

    let effect_ledger = lancet_authority::effect::ledger::EffectLedger::new();
    let authorized_region_slot = lancet_authority::effect::ledger::AuthorizedRegionSlot::new();
    let session_id = binding.identity.session_id;
    let turn_id = binding.identity.turn_id;
    // D.3: Component/native dispatch split. If the method name is in the
    // closed Component catalog, dispatch to the Component runtime; otherwise
    // fall through to the existing typed registry. The split happens after
    // audit start and before finalize, so Component and native calls share one
    // durable audit path.
    let component_evidence = CellEvidenceLedger::new();

    // Pre-dispatch effect-kind deny (issue #24 slice 3): compare the method's
    // static declared effect kinds with `denied_effect_kinds` before invoking
    // any handler/backend. Post-dispatch observed-effect checking remains
    // defense in depth below.
    if let Some(descriptor) = lancet_catalog::catalog()
        .iter()
        .find(|descriptor| descriptor.name == method)
        && let Err(denial) = lancet_authority::effect::checker::check_declared_effect_kinds_denied(
            &method,
            &descriptor.effect,
            &cep_context.policy.denied_effect_kinds,
        )
    {
        let final_result: Result<Value, CepError> = Err(denial);
        emit_cep_call_finished(
            &events,
            invocation_id,
            &method,
            &final_result,
            semantic_stage,
            guard_stage,
            policy_enforced,
        )
        .await;
        call.respond_with_evidence(
            final_result,
            triple_from_ledger(&component_evidence, &method),
        );
        return;
    }

    // #28 D1: Cell capability admission is the sole authorization for a
    // Component Tool and runs before the runtime is entered. It is
    // unconditional — `CepMode` gates only the legacy effect-region checks
    // below (D11). Both outcomes record their `AdmissionEvidence`, so a
    // refused call still finalizes with the "why" in its audit row.
    let dispatch_result = match components
        .as_ref()
        .and_then(|rt| rt.descriptor(&method).map(|descriptor| (rt, descriptor)))
    {
        Some((rt, descriptor)) => {
            match admit_tool(descriptor, &cep_context.policy.capability_admission) {
                Ok(admitted) => {
                    component_evidence.set_admission(admitted.evidence.clone());
                    let workspace_host = workspace_observation_host(
                        &admitted.descriptor,
                        &workspace,
                        &cep_context,
                        default_policy_hook.as_deref(),
                    );
                    let mutate_host = workspace_mutation_host(
                        &admitted.descriptor,
                        &method,
                        &workspace,
                        &cep_context,
                        default_policy_hook.as_deref(),
                        Arc::clone(&workspace_history),
                    );
                    let network_host =
                        network_fetch_host(&admitted.descriptor, &method, &cep_context);
                    let comp_ctx = ComponentCallContext {
                        invocation_id,
                        turn_id: turn_id.to_string(),
                        events: events.clone(),
                        cancellation: cancellation.clone(),
                        evidence: component_evidence.clone(),
                        admitted: Arc::new(admitted),
                        workspace: workspace_host,
                        mutate: mutate_host,
                        network: network_host,
                    };
                    rt.call(&method, params, comp_ctx).await
                }
                Err(denial) => {
                    component_evidence.set_admission(denial.evidence.clone());
                    Err(admission_denied_error(&method, &denial))
                }
            }
        }
        None => {
            let ctx = HandlerContext {
                invocation_id,
                events: events.clone(),
                cancellation,
                cep_context: Arc::clone(&cep_context),
                session_id,
                turn_id,
                effect_ledger: effect_ledger.clone(),
                authorized_region: authorized_region_slot.clone(),
                policy_hook: Arc::clone(&policy_hook),
                default_policy_hook: default_policy_hook.clone(),
                cep_mode: cep_mode.clone(),
            };
            registry.dispatch(&method, params, ctx).await
        }
    };

    let observed_effects = effect_ledger.snapshot();

    let final_result = apply_effect_check(
        &method,
        dispatch_result,
        &observed_effects,
        &authorized_region_slot,
        cep_mode,
        &cep_context.policy.denied_effect_kinds,
    );

    emit_cep_call_finished(
        &events,
        invocation_id,
        &method,
        &final_result,
        semantic_stage,
        guard_stage,
        policy_enforced,
    )
    .await;

    call.respond_with_evidence(
        final_result,
        triple_from_ledger(&component_evidence, &method),
    );
}

#[cfg(feature = "test-crash-barriers")]
fn stop_after_dispatch_started(
    invocations: &lancet_audit::invocation_store::InvocationStore,
    invocation_id: lancet_protocol::InvocationId,
) {
    if std::env::var("LATTICE_TEST_TURN_BARRIER").ok().as_deref() != Some("dispatch_started") {
        return;
    }
    let view = invocations
        .pinned_view()
        .expect("pin dispatch barrier ledger");
    let record = view
        .snapshot
        .invocations
        .get(&invocation_id)
        .expect("dispatch barrier invocation record");
    assert!(
        matches!(
            record.state,
            lancet_audit::invocation_store::InvocationState::DispatchStarted
        ),
        "dispatch barrier must observe DispatchStarted"
    );
    let ack_path = crash_barrier_ack_path();
    let mut context_path = ack_path.clone().into_os_string();
    context_path.push(".context");
    let context: serde_json::Value = serde_json::from_slice(
        &std::fs::read(std::path::PathBuf::from(context_path))
            .expect("read dispatch barrier session context"),
    )
    .expect("parse dispatch barrier session context");
    assert_eq!(
        context
            .get("invocation_id")
            .and_then(serde_json::Value::as_str),
        Some(invocation_id.to_string().as_str()),
        "dispatch barrier context names another invocation"
    );
    let facts = serde_json::json!({
        "barrier": "dispatch_started",
        "invocation_id": invocation_id,
        "ledger_oid": view.commit_oid.to_string(),
        "record": record,
        "session_context": context["session"].clone(),
    });
    write_crash_barrier_ack(&facts);
    // SAFETY: test-only Linux feature; the harness verifies the fsynced ack
    // before SIGKILLing the process group.
    let result = unsafe { libc::raise(libc::SIGSTOP) };
    panic!("dispatch crash barrier resumed unexpectedly ({result})");
}

#[cfg(feature = "test-crash-barriers")]
fn crash_barrier_ack_path() -> std::path::PathBuf {
    std::path::PathBuf::from(
        std::env::var_os("LATTICE_TEST_TURN_BARRIER_ACK").expect("dispatch crash barrier ack path"),
    )
}
#[cfg(feature = "test-crash-barriers")]
fn write_crash_barrier_ack(facts: &serde_json::Value) {
    use std::io::Write;

    let path = crash_barrier_ack_path();
    assert!(
        path.is_absolute(),
        "crash barrier ack path must be absolute"
    );
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&path)
        .expect("create dispatch crash barrier ack");
    serde_json::to_writer(&mut file, facts).expect("write dispatch crash barrier facts");
    file.write_all(b"\n").expect("terminate dispatch ack");
    file.sync_all().expect("fsync dispatch crash barrier ack");
    std::fs::File::open(path.parent().expect("ack parent"))
        .expect("open dispatch ack parent")
        .sync_all()
        .expect("fsync dispatch ack parent");
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admission_errors_never_echo_internal_details_or_paths() {
        let invalid = admission_error(InvocationStoreError::InvalidReservation {
            detail: "/private/state/lattice/invocations.git secret".into(),
        });
        assert_eq!(invalid.message, "invocation binding rejected");
        assert!(!invalid.message.contains('/'));

        let conflict = admission_error(InvocationStoreError::InvalidTransition {
            id: lancet_protocol::InvocationId::parse("00000000-0000-4000-8000-000000000001")
                .unwrap(),
            expected: "reserved at /private/state".into(),
            actual: "dispatch_started".into(),
        });
        assert_eq!(conflict.message, "invocation admission rejected");
        assert!(!conflict.message.contains('/'));

        let store = admission_error(InvocationStoreError::Serde(
            serde_json::from_str::<serde_json::Value>("{").unwrap_err(),
        ));
        assert_eq!(store.code, CepErrorCode::ExecutionFailed);
        assert_eq!(store.message, "invocation store admission failed");
        assert!(!serde_json::to_string(&store).unwrap().contains('/'));
    }

    #[test]
    fn admission_join_error_has_a_stable_redacted_public_classification() {
        let error = admission_task_error();
        assert_eq!(error.code, CepErrorCode::ExecutionFailed);
        assert_eq!(error.message, "invocation admission unavailable");
        assert!(error.message.len() < 64);
    }
}

// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Production composition tests (#29).
//!
//! These exercise `ComponentRuntime::from_embedded()` — the constructor a built
//! runtime actually uses — rather than the `test-utils` fixture. What they prove
//! is that the generated host composition really embeds the default Tool Bundle:
//! the registration is linked, the pinned identity matches the committed bytes,
//! and the composition gate accepts it.

use std::collections::BTreeSet;
use std::sync::Arc;

use lancet_protocol::cell::{CtxOp, ExtImpact, Trust};
use lancet_protocol::evidence::{CapabilityAdmissionPolicy, EffectSource};
use lancet_protocol::{CepErrorCode, InvocationId};
use lancet_runtime::{Cancellation, EventSink};
use lancet_runtime::{
    CellEvidenceLedger, ComponentCallContext, ComponentRuntime, ExecutionLimits, admit_tool,
};
use tokio::sync::{mpsc, watch};

/// The default distribution's five built-in Components after #32.
const EXPECTED_BUILTIN_TOOLS: [&str; 5] = [
    "fetch.web",
    "mutate.create",
    "mutate.edit",
    "observe.workspace",
    "session.todo_write",
];

fn policy() -> CapabilityAdmissionPolicy {
    CapabilityAdmissionPolicy {
        enabled_tools: BTreeSet::from(["session.todo_write".to_string()]),
        denied_capabilities: BTreeSet::new(),
        max_boundary: Trust::In,
    }
}

fn call_ctx(
    rt: &ComponentRuntime,
) -> (
    ComponentCallContext,
    CellEvidenceLedger,
    watch::Sender<bool>,
) {
    let (tx, _rx) = mpsc::unbounded_channel();
    let (cancel_tx, cancel_rx) = watch::channel(false);
    let descriptor = rt
        .descriptor("session.todo_write")
        .expect("the embedded Tool is in the closed catalog");
    let admitted = admit_tool(descriptor, &policy()).expect("admitted");
    let evidence = CellEvidenceLedger::new();
    evidence.set_admission(admitted.evidence.clone());
    let invocation_id = InvocationId::parse("00000000-0000-4000-8000-000000000001").unwrap();
    let ctx = ComponentCallContext {
        invocation_id,
        turn_id: "test-turn".into(),
        events: EventSink::new(tx, invocation_id),
        cancellation: Cancellation::new(cancel_rx),
        evidence: evidence.clone(),
        admitted: Arc::new(admitted),
        // These fixtures exercise control-only Tools; a call with no workspace
        // authority refuses every observation operation (#30).
        workspace: None,
        mutate: None,
        network: None,
    };
    (ctx, evidence, cancel_tx)
}

/// The production constructor composes the built-in Tool Bundle. This is the
/// single fact #29 turns on: before this slice `EXPECTED_COMPONENTS` was empty
/// and `from_embedded()` composed nothing.
#[test]
fn from_embedded_composes_the_default_tool_bundle() {
    let rt = ComponentRuntime::from_embedded().expect("production composition succeeds");
    let names: Vec<&str> = rt.descriptors().map(|d| d.name.as_str()).collect();
    assert_eq!(names, EXPECTED_BUILTIN_TOOLS);
    assert!(rt.descriptor("session.todo_write").is_some());
}

/// The embedded descriptor is the manifest's, in full: contract, description,
/// version, scope, and capability all come from the one authority (#29.1).
#[test]
fn the_embedded_descriptor_is_the_manifest_contract() {
    let rt = ComponentRuntime::from_embedded().expect("composition");
    let descriptor = rt.descriptor("session.todo_write").expect("descriptor");

    assert_eq!(descriptor.version, "0.1.0");
    assert_eq!(
        descriptor.description,
        "Record the current task plan as a concise todo list for multi-step work."
    );

    // #29.4: the descriptor declares only the effect that actually happens —
    // the result entering context. There is no session store behind this Tool,
    // so no `mutate` capability may appear.
    assert_eq!(descriptor.capabilities.len(), 1);
    let capability = &descriptor.capabilities[0];
    assert_eq!(capability.name, "session.result");
    assert_eq!(capability.cell.ctx_op, CtxOp::Alloc);
    assert_eq!(capability.cell.ext_impact, ExtImpact::None);
    assert_eq!(capability.cell.scope, "session");
    assert!(
        descriptor
            .capabilities
            .iter()
            .all(|cap| cap.cell.ctx_op != CtxOp::Mutate),
        "no fictional host session mutation may be declared: {:?}",
        descriptor.capabilities
    );

    // The scope is host-controlled (`in`) and served by the session channel.
    assert_eq!(descriptor.scopes.len(), 1);
    assert_eq!(descriptor.scopes[0].name, "session");
    assert_eq!(descriptor.scopes[0].trust, Trust::In);

    // The params schema is the input contract, not a hand-written host copy.
    let todos = &descriptor.params_schema["properties"]["todos"];
    assert_eq!(todos["type"], "array");
    assert_eq!(
        descriptor.params_schema["additionalProperties"],
        serde_json::json!(false)
    );
}

/// A valid call round-trips through the real embedded Component and records the
/// one effect the descriptor declares, with the host's own measure.
#[tokio::test]
async fn embedded_tool_validates_and_echoes_the_plan() {
    let rt = ComponentRuntime::from_embedded().expect("composition");
    let (ctx, evidence, _cancel) = call_ctx(&rt);

    let params = serde_json::json!({
        "todos": [
            {"content": "first step", "status": "in_progress"},
            {"content": "second step", "status": "pending"}
        ]
    });
    let result = rt
        .call("session.todo_write", params.clone(), ctx)
        .await
        .expect("a valid plan is accepted");

    // The Tool returns its canonical validated plan.
    assert_eq!(result, params);

    let (admission, effects, denied) = evidence.snapshot();
    assert_eq!(
        admission.expect("admission evidence").approved_capabilities,
        vec!["session.result".to_string()]
    );
    // Exactly one effect: the result entering context. No operation effect,
    // because this Tool performs no gated host operation at all.
    assert_eq!(effects.effects.len(), 1);
    assert_eq!(effects.effects[0].source, EffectSource::Result);
    assert_eq!(effects.effects[0].cell.ctx_op, CtxOp::Alloc);
    assert_eq!(
        effects.effects[0].context_measure,
        Some(lancet_protocol::canonical::canonical_json(&result).len() as u64),
        "the host measures the canonical result bytes itself"
    );
    assert!(denied.denied.is_empty());
}

/// A schema refusal must not echo the caller's data.
///
/// The error message reaches `cep_audit_entries.result_or_error` through
/// `compact_error_summary`, and audit records paths, hashes and object ids — never
/// content (root `AGENTS.md`). `jsonschema`'s `Display` embeds the offending
/// instance, which for this Tool is the user's whole todo list.
#[tokio::test]
async fn a_schema_refusal_does_not_leak_todo_content() {
    let rt = ComponentRuntime::from_embedded().expect("composition");
    const SECRET: &str = "SECRET-refactor-the-auth-bypass";

    // Two refusal shapes: over the ceiling (fails on the array) and a blank entry
    // (fails inside an item), so both a container-level and an item-level
    // violation are covered.
    let over_ceiling: Vec<serde_json::Value> = (0..101)
        .map(|i| serde_json::json!({"content": format!("{SECRET}-{i}"), "status": "pending"}))
        .collect();
    let blank_entry = vec![
        serde_json::json!({"content": SECRET, "status": "pending"}),
        serde_json::json!({"content": "   ", "status": "pending"}),
    ];

    for (name, todos) in [("over ceiling", over_ceiling), ("blank entry", blank_entry)] {
        let (ctx, _evidence, _cancel) = call_ctx(&rt);
        let err = match rt
            .call(
                "session.todo_write",
                serde_json::json!({"todos": todos}),
                ctx,
            )
            .await
        {
            Ok(value) => panic!("{name} must be refused, got {value}"),
            Err(err) => err,
        };
        assert_eq!(err.code, CepErrorCode::SchemaMismatch, "{name}");
        assert!(
            !err.message.contains(SECRET),
            "{name}: the refusal leaked todo content into the error message, which is \
             persisted as the audit row's result_or_error: {}",
            err.message
        );
        let rendered = serde_json::to_string(&err).expect("error serializes");
        assert!(
            !rendered.contains(SECRET),
            "{name}: the refusal leaked todo content into the structured payload: {rendered}"
        );
        // It must still say enough to act on: the method and where it failed.
        assert!(
            err.message.contains("session.todo_write") && err.message.contains("schema"),
            "{name}: the refusal must still name the method and the violation: {}",
            err.message
        );
    }
}

/// Cancellation: a call cancelled before it starts never enters the Component
/// and reports `CallCancelled`, with the admission evidence it already had.
#[tokio::test]
async fn precancelled_call_reports_cancelled_and_keeps_admission_evidence() {
    let rt = ComponentRuntime::from_embedded().expect("composition");
    let (ctx, evidence, cancel) = call_ctx(&rt);
    cancel.send(true).expect("cancel");

    let err = rt
        .call(
            "session.todo_write",
            serde_json::json!({"todos": [{"content": "a", "status": "pending"}]}),
            ctx,
        )
        .await
        .expect_err("a cancelled call fails");
    assert_eq!(err.code, CepErrorCode::CallCancelled);

    let (admission, effects, _denied) = evidence.snapshot();
    assert!(
        admission.is_some(),
        "cancellation ends the call, it does not unmake its admission evidence"
    );
    assert!(effects.effects.is_empty());
}

/// Abort-class coverage against the real embedded Component: a call that runs out
/// of fuel is reported as `ToolResourceExhausted`, not as a success or an opaque
/// trap, and it records no effect.
///
/// This guest cannot trap on its own — it has no panic path and no unreachable —
/// so the resource-limit route is the reachable abort for it. The mapping itself
/// is shared with every Tool (`error_map.rs`), but asserting it here proves the
/// production Component goes through it rather than around it.
#[tokio::test]
async fn resource_exhaustion_is_reported_and_records_no_effect() {
    let mut rt = ComponentRuntime::from_embedded().expect("composition");
    rt.set_limits(ExecutionLimits {
        fuel: 0,
        ..ExecutionLimits::default()
    });
    let (ctx, evidence, _cancel) = call_ctx(&rt);

    let err = rt
        .call(
            "session.todo_write",
            serde_json::json!({"todos": [{"content": "starved", "status": "pending"}]}),
            ctx,
        )
        .await
        .expect_err("a call with no fuel cannot complete");
    assert_eq!(err.code, CepErrorCode::ToolResourceExhausted);

    let (admission, effects, denied) = evidence.snapshot();
    assert!(
        admission.is_some(),
        "the abort does not unmake the admission evidence"
    );
    assert!(
        effects.effects.is_empty(),
        "an aborted call must not record a result effect: {effects:?}"
    );
    assert!(denied.denied.is_empty());
}

/// A Tool outside the turn's enabled set is refused by admission, before the
/// runtime is entered — there is no path that runs it anyway.
#[test]
fn unenabled_embedded_tool_is_not_admitted() {
    let rt = ComponentRuntime::from_embedded().expect("composition");
    let descriptor = rt.descriptor("session.todo_write").expect("descriptor");
    let denial = admit_tool(descriptor, &CapabilityAdmissionPolicy::default())
        .expect_err("the fail-closed default enables no tool");
    assert!(
        denial.to_string().contains("not enabled"),
        "unexpected denial: {denial}"
    );
    assert!(denial.evidence.approved_capabilities.is_empty());
}

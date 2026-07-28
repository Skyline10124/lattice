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

use lancet_dispatch::{Cancellation, EventSink};
use lancet_protocol::CepErrorCode;
use lancet_protocol::cell::{CtxOp, ExtImpact, Trust};
use lancet_protocol::evidence::{CapabilityAdmissionPolicy, EffectSource};
use lancet_runtime::{CellEvidenceLedger, ComponentCallContext, ComponentRuntime, admit_tool};
use tokio::sync::{mpsc, watch};

/// The default distribution's built-in Tools, as this slice leaves them. #30–#32
/// add the remaining four; until then this list is the closed catalog a built
/// runtime carries, and pinning it here makes an accidental addition or removal
/// a test failure rather than a silent change of the shipped catalog.
const EXPECTED_BUILTIN_TOOLS: [&str; 1] = ["session.todo_write"];

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
    let (tx, _rx) = mpsc::channel(64);
    let (cancel_tx, cancel_rx) = watch::channel(false);
    let descriptor = rt
        .descriptor("session.todo_write")
        .expect("the embedded Tool is in the closed catalog");
    let admitted = admit_tool(descriptor, &policy()).expect("admitted");
    let evidence = CellEvidenceLedger::new();
    evidence.set_admission(admitted.evidence.clone());
    let ctx = ComponentCallContext {
        call_id: 1,
        turn_id: "test-turn".into(),
        events: EventSink::new(tx, 1),
        cancellation: Cancellation::new(cancel_rx),
        evidence: evidence.clone(),
        admitted: Arc::new(admitted),
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
    assert!(rt.contains("session.todo_write"));
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

    // Validate/echo behavior, unchanged from the native handler.
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

/// Every validation rule the native handler enforced is now a host-side schema
/// refusal, and each one still fails closed (#29.3).
#[tokio::test]
async fn invalid_input_is_refused_before_the_component_runs() {
    let rt = ComponentRuntime::from_embedded().expect("composition");

    let cases: [(&str, serde_json::Value); 7] = [
        (
            "blank content",
            serde_json::json!({"todos": [{"content": "   ", "status": "pending"}]}),
        ),
        (
            "empty content",
            serde_json::json!({"todos": [{"content": "", "status": "pending"}]}),
        ),
        (
            "two in_progress",
            serde_json::json!({"todos": [
                {"content": "a", "status": "in_progress"},
                {"content": "b", "status": "in_progress"}
            ]}),
        ),
        (
            "unknown status",
            serde_json::json!({"todos": [{"content": "a", "status": "blocked"}]}),
        ),
        (
            "extra field on a todo",
            serde_json::json!({"todos": [{"content": "a", "status": "pending", "owner": "x"}]}),
        ),
        ("missing todos", serde_json::json!({"plan": []})),
        (
            "todos is not an array",
            serde_json::json!({"todos": "first"}),
        ),
    ];

    for (name, params) in cases {
        let (ctx, evidence, _cancel) = call_ctx(&rt);
        let err = match rt.call("session.todo_write", params.clone(), ctx).await {
            Ok(value) => panic!("{name} must be refused, got {value}"),
            Err(err) => err,
        };
        assert_eq!(
            err.code,
            CepErrorCode::SchemaMismatch,
            "{name} must be a structured schema refusal: {err:?}"
        );
        // A refused call never reaches the Component, so it records no effect.
        let (_admission, effects, denied) = evidence.snapshot();
        assert!(
            effects.effects.is_empty(),
            "{name} recorded an effect: {effects:?}"
        );
        assert!(denied.denied.is_empty(), "{name} recorded a denial");
    }
}

/// The refusals above, checked for their error class and for producing no
/// effect: a rejected call must not record one.
#[tokio::test]
async fn refused_input_produces_no_effect() {
    let rt = ComponentRuntime::from_embedded().expect("composition");
    let (ctx, evidence, _cancel) = call_ctx(&rt);

    let err = rt
        .call(
            "session.todo_write",
            serde_json::json!({"todos": [
                {"content": "a", "status": "in_progress"},
                {"content": "b", "status": "in_progress"}
            ]}),
            ctx,
        )
        .await
        .expect_err("two in_progress todos are refused");
    assert_eq!(err.code, CepErrorCode::SchemaMismatch);

    let (_admission, effects, denied) = evidence.snapshot();
    assert!(
        effects.effects.is_empty(),
        "a refused call recorded an effect: {effects:?}"
    );
    assert!(denied.denied.is_empty());
}

/// A todo list over the compiled ceiling is refused. The bound lives in the
/// manifest schema, so it is part of the descriptor rather than a runtime knob.
#[tokio::test]
async fn over_the_todo_ceiling_is_refused() {
    let rt = ComponentRuntime::from_embedded().expect("composition");
    let descriptor = rt.descriptor("session.todo_write").expect("descriptor");
    let max = descriptor.params_schema["properties"]["todos"]["maxItems"]
        .as_u64()
        .expect("the descriptor declares a maxItems ceiling");

    let todos: Vec<serde_json::Value> = (0..=max)
        .map(|i| serde_json::json!({"content": format!("item {i}"), "status": "pending"}))
        .collect();
    let (ctx, _evidence, _cancel) = call_ctx(&rt);
    let err = rt
        .call(
            "session.todo_write",
            serde_json::json!({"todos": todos}),
            ctx,
        )
        .await
        .expect_err("one more than the ceiling is refused");
    assert_eq!(err.code, CepErrorCode::SchemaMismatch);

    // Exactly at the ceiling is accepted, so the bound is the declared one.
    let todos: Vec<serde_json::Value> = (0..max)
        .map(|i| serde_json::json!({"content": format!("item {i}"), "status": "pending"}))
        .collect();
    let (ctx, _evidence, _cancel) = call_ctx(&rt);
    rt.call(
        "session.todo_write",
        serde_json::json!({"todos": todos}),
        ctx,
    )
    .await
    .expect("the ceiling itself is allowed");
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

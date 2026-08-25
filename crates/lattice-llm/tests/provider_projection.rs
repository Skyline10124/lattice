// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Provider replay/projection seam contract tests over fixed official-wire
//! fixtures (GitHub #142). No live network: every fixture is a reviewable
//! byte-fixed official response body.

use lattice_conversation::{
    AttemptInterruption, LimitKind, MAX_ASSISTANT_CONTENT_BYTES, MAX_MODEL_ATTEMPT_BYTES,
    MAX_REASONING_CONTENT_BYTES, MAX_TOOL_CALL_ARGUMENTS_BYTES, MessageSemantic, NativeMessage,
    OpaqueProviderState, ProviderOrigin, UnboundToolCall, replay_state_digest, semantic_digest,
};
use lattice_llm::provider_projection::{
    ProjectionError, native_to_wire, project_cross_provider, wire_to_native,
};
use serde_json::Value;

fn fixture(name: &str) -> Value {
    let path = format!(
        "{}/tests/fixtures/projection/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    let bytes = std::fs::read_to_string(&path).expect("fixture readable");
    serde_json::from_str(&bytes).expect("fixture is valid JSON")
}

fn openai_message(fixture: &Value) -> Value {
    fixture["choices"][0]["message"].clone()
}

fn gemini_content(fixture: &Value) -> Value {
    fixture["candidates"][0]["content"].clone()
}

fn anthropic_message(fixture: &Value) -> Value {
    serde_json::json!({
        "role": fixture["role"].clone(),
        "content": fixture["content"].clone(),
    })
}

fn tool_call_parts(message: &NativeMessage<UnboundToolCall>) -> Vec<(usize, &UnboundToolCall)> {
    message.tool_calls().collect()
}

// ─── OpenAI-compatible ────────────────────────────────────────────────────

#[test]
fn openai_part_order_and_real_ids() {
    let native = wire_to_native(
        ProviderOrigin::OpenAiCompatible,
        &fixture("openai_assistant_tool_calls.json"),
    )
    .expect("projection succeeds");

    assert_eq!(
        native.provider_origin,
        Some(ProviderOrigin::OpenAiCompatible)
    );
    assert_eq!(native.parts.len(), 4);
    assert!(matches!(
        &native.parts[0].semantic,
        MessageSemantic::Reasoning { .. }
    ));
    assert!(matches!(
        &native.parts[1].semantic,
        MessageSemantic::Text { .. }
    ));
    let calls = tool_call_parts(&native);
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].0, 2);
    assert_eq!(calls[1].0, 3);
    assert_eq!(
        calls[0].1.provider_call_id.as_deref(),
        Some("call_9f8e7d6c5b")
    );
    assert_eq!(
        calls[1].1.provider_call_id.as_deref(),
        Some("call_1a2b3c4d5e")
    );
    assert_eq!(calls[0].1.name, "get_weather");
    assert_eq!(calls[1].1.name, "get_time");
}

#[test]
fn openai_same_provider_replay_is_exact() {
    let fixture = fixture("openai_assistant_tool_calls.json");
    let native = wire_to_native(ProviderOrigin::OpenAiCompatible, &fixture).expect("projection");
    let replayed =
        native_to_wire(ProviderOrigin::OpenAiCompatible, &native).expect("replay succeeds");
    assert_eq!(replayed, openai_message(&fixture));
}

#[test]
fn openai_missing_provider_id_is_none_not_synthesized() {
    let fixture = fixture("openai_assistant_missing_id.json");
    let native = wire_to_native(ProviderOrigin::OpenAiCompatible, &fixture).expect("projection");
    let calls = tool_call_parts(&native);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].1.provider_call_id, None);

    let replayed = native_to_wire(ProviderOrigin::OpenAiCompatible, &native).expect("replay");
    let serialized = serde_json::to_string(&replayed).expect("serialize");
    assert!(
        !serialized.contains("tc_"),
        "no synthetic tc_* ids: {serialized}"
    );
    assert_eq!(replayed, openai_message(&fixture));
}

// ─── Anthropic ────────────────────────────────────────────────────────────

#[test]
fn anthropic_part_order_and_signature_state() {
    let native = wire_to_native(
        ProviderOrigin::Anthropic,
        &fixture("anthropic_thinking_tool_use.json"),
    )
    .expect("projection succeeds");

    assert_eq!(native.parts.len(), 4);
    // Wire order: thinking, text, tool_use, tool_use.
    assert!(matches!(
        &native.parts[0].semantic,
        MessageSemantic::Reasoning { .. }
    ));
    assert_eq!(
        native.parts[0]
            .replay_state
            .as_ref()
            .map(OpaqueProviderState::as_str),
        Some("ErUBCkYIAxgCIkD3c2FmZGVmNTQ2Ny0xMjM0LWFiY2QtZXhhbXBsZXNpZ25hdHVyZQ==")
    );
    assert!(matches!(
        &native.parts[1].semantic,
        MessageSemantic::Text { .. }
    ));
    let calls = tool_call_parts(&native);
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].0, 2);
    assert_eq!(calls[1].0, 3);
    assert_eq!(
        calls[0].1.provider_call_id.as_deref(),
        Some("toolu_01AbCdEfGh")
    );
    assert_eq!(
        calls[1].1.provider_call_id.as_deref(),
        Some("toolu_02BcDeFgHi")
    );
}

#[test]
fn anthropic_same_provider_replay_is_exact() {
    for name in [
        "anthropic_thinking_tool_use.json",
        "anthropic_text_only.json",
    ] {
        let fixture = fixture(name);
        let native = wire_to_native(ProviderOrigin::Anthropic, &fixture).expect("projection");
        let replayed = native_to_wire(ProviderOrigin::Anthropic, &native).expect("replay");
        assert_eq!(replayed, anthropic_message(&fixture), "fixture {name}");
    }
}

#[test]
fn anthropic_thinking_without_signature_cannot_replay() {
    // Fail closed: the signature is protocol-required replay state and is
    // never fabricated.
    let mut native = wire_to_native(
        ProviderOrigin::Anthropic,
        &fixture("anthropic_thinking_tool_use.json"),
    )
    .expect("projection");
    native.parts[0].replay_state = None;
    let result = native_to_wire(ProviderOrigin::Anthropic, &native);
    assert!(matches!(
        result,
        Err(ProjectionError::MissingReplayState {
            provider: ProviderOrigin::Anthropic,
            part_ordinal: 0,
        })
    ));
}

// ─── Gemini ───────────────────────────────────────────────────────────────

#[test]
fn gemini_thought_signatures_stay_in_place() {
    let native = wire_to_native(
        ProviderOrigin::Gemini,
        &fixture("gemini_thought_signature.json"),
    )
    .expect("projection succeeds");

    assert_eq!(native.parts.len(), 3);
    // Part 0: thought text with its signature.
    assert!(matches!(
        &native.parts[0].semantic,
        MessageSemantic::Reasoning { .. }
    ));
    assert_eq!(
        native.parts[0]
            .replay_state
            .as_ref()
            .map(OpaqueProviderState::as_str),
        Some("CigIARABGkh4dGh5YnJpZC1zaWduYXR1cmUtb24tdGhvdWdodA==")
    );
    // Part 1: empty text carrying a signature — preserved, not filtered.
    assert!(matches!(
        &native.parts[1].semantic,
        MessageSemantic::Text { text } if text.is_empty()
    ));
    assert_eq!(
        native.parts[1]
            .replay_state
            .as_ref()
            .map(OpaqueProviderState::as_str),
        Some("CigIARABGkh4ZW1wdHktdGV4dC1wYXJ0LXNpZ25hdHVyZQ==")
    );
    // Part 2: functionCall with a real id and its own signature.
    let calls = tool_call_parts(&native);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, 2);
    assert_eq!(
        calls[0].1.provider_call_id.as_deref(),
        Some("fc-2026-08-13-001")
    );
    assert_eq!(
        native.parts[2]
            .replay_state
            .as_ref()
            .map(OpaqueProviderState::as_str),
        Some("CigIARABGkh4c2lnbmF0dXJlLW9uLWZ1bmN0aW9uLWNhbGw=")
    );
}

#[test]
fn gemini_same_provider_replay_is_exact() {
    for name in [
        "gemini_thought_signature.json",
        "gemini_function_call_no_id.json",
    ] {
        let fixture = fixture(name);
        let native = wire_to_native(ProviderOrigin::Gemini, &fixture).expect("projection");
        let replayed = native_to_wire(ProviderOrigin::Gemini, &native).expect("replay");
        assert_eq!(replayed, gemini_content(&fixture), "fixture {name}");
    }
}

#[test]
fn gemini_function_call_without_id_is_none_not_synthesized() {
    let fixture = fixture("gemini_function_call_no_id.json");
    let native = wire_to_native(ProviderOrigin::Gemini, &fixture).expect("projection");
    let calls = tool_call_parts(&native);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].1.provider_call_id, None);
    let serialized = serde_json::to_string(&native).expect("serialize");
    assert!(!serialized.contains("tc_"), "no synthetic tc_* ids");

    let parameterless = serde_json::json!({
        "candidates": [{
            "content": {
                "role": "model",
                "parts": [{"functionCall": {"name": "ping"}}],
            }
        }],
    });
    let native = wire_to_native(ProviderOrigin::Gemini, &parameterless).expect("projection");
    assert_eq!(
        tool_call_parts(&native)[0].1.arguments,
        serde_json::json!({}),
        "an omitted Gemini args field is the documented empty object",
    );
}

// ─── Cross-provider projection ────────────────────────────────────────────

#[test]
fn cross_provider_projection_never_leaks_source_state() {
    let source = wire_to_native(
        ProviderOrigin::Anthropic,
        &fixture("anthropic_thinking_tool_use.json"),
    )
    .expect("projection");
    let source_semantic = semantic_digest(&source);
    let source_replay = replay_state_digest(&source);

    // A (Anthropic) → B (Gemini): semantics cross, opaque state does not.
    let projected = project_cross_provider(&source, ProviderOrigin::Gemini);
    assert_eq!(projected.provider_origin, Some(ProviderOrigin::Gemini));
    for part in &projected.parts {
        assert_eq!(part.replay_state, None);
    }
    for (_, call) in projected.tool_calls() {
        assert_eq!(call.provider_call_id, None);
    }
    // Reasoning does not cross providers (ADR 0005 §7), so the projection
    // carries no Reasoning part and its semantic digest necessarily differs
    // from a source that had one.
    assert!(
        !projected
            .parts
            .iter()
            .any(|part| matches!(part.semantic, MessageSemantic::Reasoning { .. })),
        "projection must not carry source reasoning"
    );
    assert_ne!(semantic_digest(&projected), source_semantic);
    assert_ne!(replay_state_digest(&projected), source_replay);

    // No source opaque byte survives in any serialization of the projection.
    let serialized = serde_json::to_string(&projected).expect("serialize");
    for leaked in [
        "ErUBCkYIAxgCIkD3c2FmZGVmNTQ2Ny0xMjM0LWFiY2QtZXhhbXBsZXNpZ25hdHVyZQ==",
        "toolu_01AbCdEfGh",
        "toolu_02BcDeFgHi",
    ] {
        assert!(
            !serialized.contains(leaked),
            "projection leaks source state {leaked:?}"
        );
    }

    // B → A recovery: the retained original message still replays exactly.
    let replayed = native_to_wire(ProviderOrigin::Anthropic, &source).expect("replay");
    assert_eq!(
        replayed,
        anthropic_message(&fixture("anthropic_thinking_tool_use.json"))
    );
    assert_eq!(replay_state_digest(&source), source_replay);
}

#[test]
fn cross_provider_gemini_to_openai_drops_signatures() {
    let source = wire_to_native(
        ProviderOrigin::Gemini,
        &fixture("gemini_thought_signature.json"),
    )
    .expect("projection");
    let projected = project_cross_provider(&source, ProviderOrigin::OpenAiCompatible);
    let serialized = serde_json::to_string(&projected).expect("serialize");
    assert!(!serialized.contains("thoughtSignature"));
    assert!(
        !serialized.contains("CigIARAB"),
        "no signature bytes survive"
    );
    // The Gemini source carries a thought part, which does not cross.
    assert!(
        !projected
            .parts
            .iter()
            .any(|part| matches!(part.semantic, MessageSemantic::Reasoning { .. })),
        "projection must not carry source reasoning"
    );
    assert_ne!(semantic_digest(&projected), semantic_digest(&source));
}

#[test]
fn cross_provider_projection_preserves_non_reasoning_semantics() {
    // A source without reasoning crosses losslessly: dropping reasoning is the
    // only semantic difference the projection introduces.
    let source = wire_to_native(
        ProviderOrigin::Anthropic,
        &fixture("anthropic_text_only.json"),
    )
    .expect("projection");
    let projected = project_cross_provider(&source, ProviderOrigin::Gemini);
    assert_eq!(semantic_digest(&projected), semantic_digest(&source));
    assert_eq!(projected.parts.len(), source.parts.len());
}

#[test]
fn cross_provider_wire_never_carries_source_reasoning() {
    // The regression this pins: reasoning used to survive projection and be
    // written verbatim into the target provider's reasoning channel
    // (`reasoning_content` for OpenAI, a `thought: true` part for Gemini).
    // ADR 0005 §7: "cross-provider projection does not replay source
    // reasoning". Asserting on the native struct alone missed this, so the
    // assertion has to happen on the emitted wire.
    let cases: [(ProviderOrigin, &str, ProviderOrigin, &str); 3] = [
        (
            ProviderOrigin::Anthropic,
            "anthropic_thinking_tool_use.json",
            ProviderOrigin::OpenAiCompatible,
            "Need the weather first, then the time; both are independent calls.",
        ),
        (
            ProviderOrigin::Anthropic,
            "anthropic_thinking_tool_use.json",
            ProviderOrigin::Gemini,
            "Need the weather first, then the time; both are independent calls.",
        ),
        (
            ProviderOrigin::Gemini,
            "gemini_thought_signature.json",
            ProviderOrigin::OpenAiCompatible,
            "Plan: check weather, then time.",
        ),
    ];

    for (source_origin, fixture_name, target, reasoning_text) in cases {
        let source =
            wire_to_native(source_origin, &fixture(fixture_name)).expect("source projection");
        // Precondition: the source really does carry that reasoning text.
        assert!(
            source.parts.iter().any(|part| matches!(
                &part.semantic,
                MessageSemantic::Reasoning { text } if text == reasoning_text
            )),
            "fixture {fixture_name} must carry the reasoning under test"
        );

        let projected = project_cross_provider(&source, target);
        let wire = native_to_wire(target, &projected).expect("target wire");
        let serialized = serde_json::to_string(&wire).expect("serialize");

        assert!(
            !serialized.contains(reasoning_text),
            "{source_origin:?} → {target:?} leaked source reasoning into the wire: {serialized}"
        );
        assert!(
            !serialized.contains("reasoning_content"),
            "{source_origin:?} → {target:?} emitted a reasoning channel: {serialized}"
        );
        assert!(
            !serialized.contains("\"thought\""),
            "{source_origin:?} → {target:?} emitted a thought part: {serialized}"
        );
    }
}

// ─── Fail-closed boundaries ───────────────────────────────────────────────

#[test]
fn same_provider_replay_requires_matching_origin() {
    let native = wire_to_native(
        ProviderOrigin::Gemini,
        &fixture("gemini_function_call_no_id.json"),
    )
    .expect("projection");
    let result = native_to_wire(ProviderOrigin::Anthropic, &native);
    assert!(matches!(
        result,
        Err(ProjectionError::WrongOrigin {
            expected: ProviderOrigin::Anthropic,
            found: Some(ProviderOrigin::Gemini),
        })
    ));
}

#[test]
fn malformed_wire_is_rejected() {
    let garbage = serde_json::json!({"unexpected": true});
    assert!(matches!(
        wire_to_native(ProviderOrigin::Anthropic, &garbage),
        Err(ProjectionError::MalformedWire { .. })
    ));
    assert!(matches!(
        wire_to_native(ProviderOrigin::OpenAiCompatible, &garbage),
        Err(ProjectionError::MalformedWire { .. })
    ));
    assert!(matches!(
        wire_to_native(ProviderOrigin::Gemini, &garbage),
        Err(ProjectionError::MalformedWire { .. })
    ));
}

#[test]
fn invalid_tool_call_on_the_wire_rejects_the_whole_message() {
    // The single tool_use carries non-object input semantics through a
    // malformed arguments channel: here a duplicate provider call id forces
    // the whole message to fail closed.
    let mut fixture = fixture("anthropic_thinking_tool_use.json");
    fixture["content"][3]["id"] = serde_json::json!("toolu_01AbCdEfGh");
    let result = wire_to_native(ProviderOrigin::Anthropic, &fixture);
    assert!(matches!(
        result,
        Err(ProjectionError::InvalidToolCall(
            lattice_conversation::ToolCallValidation::DuplicateToolCallId { .. }
        ))
    ));
}

// ─── Anthropic text-block signature guard (P2-1) ──────────────────────────

#[test]
fn anthropic_text_part_with_replay_state_is_rejected() {
    // `signature` belongs only to thinking blocks. Emitting it on a text block
    // would write a field the reader silently drops on the way back, so the
    // writer fails closed instead.
    let message: NativeMessage<UnboundToolCall> = NativeMessage::new(
        lattice_conversation::MessageRole::Assistant,
        Some(ProviderOrigin::Anthropic),
        vec![lattice_conversation::MessagePart {
            semantic: MessageSemantic::Text {
                text: "hi".to_string(),
            },
            replay_state: Some(
                OpaqueProviderState::new("not-an-anthropic-text-field").expect("state"),
            ),
        }],
    );
    assert!(matches!(
        native_to_wire(ProviderOrigin::Anthropic, &message),
        Err(ProjectionError::UnsupportedPart {
            provider: ProviderOrigin::Anthropic,
            part_ordinal: 0,
        })
    ));
}

// ─── OpenAI writer lossiness, explicitly pinned (P2-2, P2-4) ──────────────

#[test]
fn openai_replay_normalizes_non_canonical_arguments_spelling() {
    // ADR 0005 §7 requires the canonical arguments object for execution, so
    // replay is semantically identical but not byte-identical when the
    // provider spelled its arguments non-canonically. The checked-in fixtures
    // are all canonical, so this is the case they cannot cover.
    let wire = serde_json::json!({"choices": [{"message": {
        "role": "assistant",
        "content": Value::Null,
        "tool_calls": [{
            "id": "call_1",
            "type": "function",
            "function": {"name": "t", "arguments": "{\"b\": 2, \"a\": 1}"}
        }]
    }}]});
    let native = wire_to_native(ProviderOrigin::OpenAiCompatible, &wire).expect("projection");
    let replayed = native_to_wire(ProviderOrigin::OpenAiCompatible, &native).expect("replay");

    let original_args = wire["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"]
        .as_str()
        .expect("original arguments");
    let replayed_args = replayed["tool_calls"][0]["function"]["arguments"]
        .as_str()
        .expect("replayed arguments");

    // Not byte-identical …
    assert_ne!(replayed_args, original_args);
    assert_eq!(replayed_args, r#"{"a":1,"b":2}"#);
    // … but semantically identical.
    let original_value: Value = serde_json::from_str(original_args).expect("original json");
    let replayed_value: Value = serde_json::from_str(replayed_args).expect("replayed json");
    assert_eq!(original_value, replayed_value);
}

#[test]
fn openai_writer_merges_interleaved_text_parts() {
    // The OpenAI wire has one `content` string, so interleaving is not
    // representable and merges. Pinned so the loss is a known property rather
    // than a silent surprise.
    let wire = serde_json::json!({"role": "assistant", "content": [
        {"type": "text", "text": "BEFORE"},
        {"type": "tool_use", "id": "toolu_1", "name": "t", "input": {"a": 1}},
        {"type": "text", "text": "AFTER"}
    ]});
    let source = wire_to_native(ProviderOrigin::Anthropic, &wire).expect("projection");
    assert_eq!(source.parts.len(), 3);

    let projected = project_cross_provider(&source, ProviderOrigin::OpenAiCompatible);
    let replayed = native_to_wire(ProviderOrigin::OpenAiCompatible, &projected).expect("replay");
    assert_eq!(replayed["content"], serde_json::json!("BEFOREAFTER"));
}

// ─── Protocol hard limits enforced at the seam (P1-1) ─────────────────────

fn limit_kind(result: Result<NativeMessage<UnboundToolCall>, ProjectionError>) -> LimitKind {
    match result {
        Err(ProjectionError::OutputLimitExceeded {
            interruption: AttemptInterruption::OutputLimitExceeded { kind, .. },
            ..
        }) => kind,
        Ok(_) => panic!("oversized wire must not produce a message"),
        Err(other) => panic!("expected OutputLimitExceeded, got {other:?}"),
    }
}

fn openai_content_wire(content: String) -> Value {
    serde_json::json!({"choices": [{"message": {
        "role": "assistant",
        "content": content,
    }}]})
}

fn openai_reasoning_wire(reasoning: String) -> Value {
    serde_json::json!({"choices": [{"message": {
        "role": "assistant",
        "content": Value::Null,
        "reasoning_content": reasoning,
    }}]})
}

#[test]
fn assistant_content_limit_is_enforced_at_the_seam() {
    let exact = openai_content_wire("x".repeat(MAX_ASSISTANT_CONTENT_BYTES));
    let native = wire_to_native(ProviderOrigin::OpenAiCompatible, &exact).expect("exact boundary");
    assert_eq!(native.parts.len(), 1);

    let over = openai_content_wire("x".repeat(MAX_ASSISTANT_CONTENT_BYTES + 1));
    assert_eq!(
        limit_kind(wire_to_native(ProviderOrigin::OpenAiCompatible, &over)),
        LimitKind::AssistantContent
    );
}

#[test]
fn reasoning_content_limit_is_enforced_at_the_seam() {
    let exact = openai_reasoning_wire("r".repeat(MAX_REASONING_CONTENT_BYTES));
    wire_to_native(ProviderOrigin::OpenAiCompatible, &exact).expect("exact boundary");

    let over = openai_reasoning_wire("r".repeat(MAX_REASONING_CONTENT_BYTES + 1));
    assert_eq!(
        limit_kind(wire_to_native(ProviderOrigin::OpenAiCompatible, &over)),
        LimitKind::ReasoningContent
    );
}

#[test]
fn attempt_total_limit_is_enforced_at_the_seam() {
    // 2 MiB assistant + 2 MiB reasoning + 12 MiB of tool arguments spread over
    // two calls (the per-call budget is 8 MiB and resets per call, so only the
    // 16 MiB aggregate can trip here).
    let filler =
        MAX_MODEL_ATTEMPT_BYTES - MAX_ASSISTANT_CONTENT_BYTES - MAX_REASONING_CONTENT_BYTES;
    let overhead = r#"{"a":""}"#.len();
    let per_call = filler / 2;
    assert!(per_call <= MAX_TOOL_CALL_ARGUMENTS_BYTES);

    let build = |extra: usize| {
        let first = per_call - overhead;
        let second = filler - per_call - overhead + extra;
        serde_json::json!({"choices": [{"message": {
            "role": "assistant",
            "content": "x".repeat(MAX_ASSISTANT_CONTENT_BYTES),
            "reasoning_content": "r".repeat(MAX_REASONING_CONTENT_BYTES),
            "tool_calls": [
                {
                    "id": "call_1",
                    "type": "function",
                    "function": {
                        "name": "t",
                        "arguments": format!("{{\"a\":\"{}\"}}", "z".repeat(first)),
                    }
                },
                {
                    "id": "call_2",
                    "type": "function",
                    "function": {
                        "name": "t",
                        "arguments": format!("{{\"a\":\"{}\"}}", "z".repeat(second)),
                    }
                }
            ]
        }}]})
    };

    let exact = build(0);
    wire_to_native(ProviderOrigin::OpenAiCompatible, &exact).expect("exact attempt boundary");

    let over = build(1);
    assert_eq!(
        limit_kind(wire_to_native(ProviderOrigin::OpenAiCompatible, &over)),
        LimitKind::ModelAttemptTotal
    );
}

#[test]
fn tool_arguments_limit_is_enforced_for_anthropic_and_gemini() {
    // Every provider projector meters, not just the OpenAI one.
    let oversized = "z".repeat(MAX_TOOL_CALL_ARGUMENTS_BYTES);
    let anthropic = serde_json::json!({"role": "assistant", "content": [
        {"type": "tool_use", "id": "toolu_1", "name": "t", "input": {"a": oversized}}
    ]});
    assert_eq!(
        limit_kind(wire_to_native(ProviderOrigin::Anthropic, &anthropic)),
        LimitKind::ToolCallArguments
    );

    let gemini = serde_json::json!({"candidates": [{"content": {"role": "model", "parts": [
        {"functionCall": {"name": "t", "args": {"a": oversized}}}
    ]}}]});
    assert_eq!(
        limit_kind(wire_to_native(ProviderOrigin::Gemini, &gemini)),
        LimitKind::ToolCallArguments
    );
}

// ─── #139-B Commit 2: bound closures, ToolResult encoding, usage ──────────

use lattice_conversation::{
    BoundToolCall, InvocationId, MessagePart, MessageRole, ModelMessageAddress, SessionId,
    ToolCallAddress, ToolResultOutcome, TurnId,
};
use lattice_llm::provider_projection::{tool_results_to_wire, wire_usage};

fn invocation(index: usize) -> InvocationId {
    InvocationId::parse(&format!("00000000-0000-4000-8000-{index:012x}")).unwrap()
}

fn bind_message(message: &NativeMessage<UnboundToolCall>) -> NativeMessage<BoundToolCall> {
    let mut bound_parts = Vec::new();
    let mut next_invocation = 0usize;
    for part in &message.parts {
        let semantic = match &part.semantic {
            MessageSemantic::ToolCall { call } => {
                let invocation_id = invocation(next_invocation);
                next_invocation += 1;
                let call = call.clone();
                let address = ToolCallAddress {
                    message: ModelMessageAddress {
                        session_id: SessionId::parse("3f8b2c1a7d4e4f5a8b9c0d1e2f3a4b5c").unwrap(),
                        turn_id: TurnId::parse("3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c").unwrap(),
                        model_step_ordinal: 0,
                    },
                    part_ordinal: 0,
                };
                MessageSemantic::ToolCall {
                    call: call.bind(invocation_id, address),
                }
            }
            MessageSemantic::Text { text } => MessageSemantic::Text { text: text.clone() },
            MessageSemantic::Reasoning { text } => {
                MessageSemantic::Reasoning { text: text.clone() }
            }
            MessageSemantic::ToolResult {
                invocation_id,
                name,
                outcome,
                content,
            } => MessageSemantic::ToolResult {
                invocation_id: *invocation_id,
                name: name.clone(),
                outcome: *outcome,
                content: content.clone(),
            },
        };
        bound_parts.push(MessagePart {
            semantic,
            replay_state: part.replay_state.clone(),
        });
    }
    NativeMessage::new(message.role, message.provider_origin, bound_parts)
}

#[test]
fn bound_closure_vocabulary_replays_identically_to_unbound() {
    // The persisted closure vocabulary is `NativeMessage<BoundToolCall>`;
    // same-provider replay must be byte-identical to the unbound replay
    // because the provider-visible fields are the same typestate accessors.
    let cases = [
        (
            ProviderOrigin::OpenAiCompatible,
            fixture("openai_assistant_tool_calls.json"),
        ),
        (
            ProviderOrigin::Anthropic,
            anthropic_message(&fixture("anthropic_thinking_tool_use.json")),
        ),
        (
            ProviderOrigin::Gemini,
            fixture("gemini_thought_signature.json"),
        ),
    ];
    for (origin, wire_message) in cases {
        let unbound = wire_to_native(origin, &wire_message).expect("projection");
        let bound = bind_message(&unbound);
        let unbound_wire = native_to_wire(origin, &unbound).expect("unbound replay");
        let bound_wire = native_to_wire(origin, &bound).expect("bound replay");
        assert_eq!(unbound_wire, bound_wire);
    }
}

#[test]
fn cross_provider_projection_drops_bound_provider_ids_and_replay_state() {
    let native = wire_to_native(
        ProviderOrigin::Anthropic,
        &anthropic_message(&fixture("anthropic_thinking_tool_use.json")),
    )
    .expect("projection");
    let bound = bind_message(&native);
    let projected = project_cross_provider(&bound, ProviderOrigin::OpenAiCompatible);
    assert_eq!(
        projected.provider_origin,
        Some(ProviderOrigin::OpenAiCompatible)
    );
    for part in &projected.parts {
        assert!(part.replay_state.is_none(), "replay state must not cross");
        if let MessageSemantic::ToolCall { call } = &part.semantic {
            assert_eq!(call.provider_call_id, None, "provider id must not cross");
        }
        assert!(
            !matches!(part.semantic, MessageSemantic::Reasoning { .. }),
            "reasoning must not cross"
        );
    }
    // The bound identity survives: this is still a BoundToolCall message.
    let tool_calls: Vec<_> = projected.tool_calls().collect();
    assert_eq!(tool_calls.len(), 2);
    assert_eq!(tool_calls[0].1.invocation_id, invocation(0));
}

fn tool_closure(
    invocation_id: InvocationId,
    name: &str,
    outcome: ToolResultOutcome,
    content: Option<&str>,
) -> MessagePart<BoundToolCall> {
    MessagePart::semantic_only(MessageSemantic::ToolResult {
        invocation_id,
        name: name.to_string(),
        outcome,
        content: content.map(str::to_string),
    })
}

#[test]
fn openai_tool_results_encode_against_the_official_fixture() {
    let native = wire_to_native(
        ProviderOrigin::OpenAiCompatible,
        &fixture("openai_assistant_tool_calls.json"),
    )
    .expect("projection");
    let bound = bind_message(&native);
    let tool = NativeMessage::new(
        MessageRole::Tool,
        Some(ProviderOrigin::OpenAiCompatible),
        vec![
            tool_closure(
                invocation(0),
                "get_weather",
                ToolResultOutcome::Succeeded,
                Some(r#"{"city":"Tokyo","temperature":21}"#),
            ),
            tool_closure(
                invocation(1),
                "get_time",
                ToolResultOutcome::NotExecuted,
                None,
            ),
        ],
    );
    let encoded =
        tool_results_to_wire(ProviderOrigin::OpenAiCompatible, &bound, &tool).expect("encoding");
    let expected = fixture("openai_tool_result_request.json");
    assert_eq!(serde_json::Value::Array(encoded), expected);
}

#[test]
fn anthropic_tool_results_merge_into_one_user_turn_with_is_error() {
    let native = wire_to_native(
        ProviderOrigin::Anthropic,
        &anthropic_message(&fixture("anthropic_thinking_tool_use.json")),
    )
    .expect("projection");
    let bound = bind_message(&native);
    let tool = NativeMessage::new(
        MessageRole::Tool,
        Some(ProviderOrigin::Anthropic),
        vec![
            tool_closure(
                invocation(0),
                "get_weather",
                ToolResultOutcome::Failed,
                Some("permission denied by policy"),
            ),
            tool_closure(
                invocation(1),
                "get_time",
                ToolResultOutcome::NotExecuted,
                None,
            ),
        ],
    );
    let encoded = tool_results_to_wire(ProviderOrigin::Anthropic, &bound, &tool).expect("encoding");
    assert_eq!(encoded.len(), 1);
    assert_eq!(encoded[0], fixture("anthropic_tool_result_request.json"));
}

#[test]
fn gemini_tool_results_encode_without_any_provider_id() {
    let native = wire_to_native(
        ProviderOrigin::Gemini,
        &fixture("gemini_function_call_no_id.json"),
    )
    .expect("projection");
    let bound = bind_message(&native);
    assert!(
        bound
            .tool_calls()
            .next()
            .unwrap()
            .1
            .provider_call_id
            .is_none()
    );
    let tool = NativeMessage::new(
        MessageRole::Tool,
        Some(ProviderOrigin::Gemini),
        vec![tool_closure(
            invocation(0),
            "get_weather",
            ToolResultOutcome::Succeeded,
            Some(r#"{"city":"Paris","temperature":18}"#),
        )],
    );
    let encoded = tool_results_to_wire(ProviderOrigin::Gemini, &bound, &tool).expect("encoding");
    assert_eq!(encoded.len(), 1);
    assert_eq!(encoded[0], fixture("gemini_tool_result_request.json"));
}

#[test]
fn id_bearing_wires_fail_typed_when_the_call_had_no_provider_id() {
    // A missing provider call ID is a valid conversation fact; OpenAI and
    // Anthropic result shapes cannot encode it, so the next request fails
    // with a typed error instead of a synthesized ID.
    let native = wire_to_native(
        ProviderOrigin::Gemini,
        &fixture("gemini_function_call_no_id.json"),
    )
    .expect("projection");
    let bound = bind_message(&native);
    let tool = NativeMessage::new(
        MessageRole::Tool,
        Some(ProviderOrigin::Gemini),
        vec![tool_closure(
            invocation(0),
            "get_weather",
            ToolResultOutcome::Succeeded,
            Some("{}"),
        )],
    );
    for origin in [ProviderOrigin::OpenAiCompatible, ProviderOrigin::Anthropic] {
        assert!(matches!(
            tool_results_to_wire(origin, &bound, &tool),
            Err(ProjectionError::ToolResultWithoutProviderCallId { invocation_id, .. })
                if invocation_id == invocation(0)
        ));
    }
}

#[test]
fn unmatched_tool_result_closure_fails_closed() {
    let native = wire_to_native(
        ProviderOrigin::OpenAiCompatible,
        &fixture("openai_assistant_tool_calls.json"),
    )
    .expect("projection");
    let bound = bind_message(&native);
    let stranger = NativeMessage::new(
        MessageRole::Tool,
        Some(ProviderOrigin::OpenAiCompatible),
        vec![tool_closure(
            invocation(9),
            "get_weather",
            ToolResultOutcome::Succeeded,
            Some("{}"),
        )],
    );
    assert!(matches!(
        tool_results_to_wire(ProviderOrigin::OpenAiCompatible, &bound, &stranger),
        Err(ProjectionError::UnmatchedToolResult { .. })
    ));
}

#[test]
fn wire_usage_extracts_official_blocks() {
    let openai = wire_usage(
        ProviderOrigin::OpenAiCompatible,
        &fixture("openai_usage.json"),
    )
    .expect("usage")
    .expect("present");
    assert_eq!(
        (
            openai.prompt_tokens,
            openai.completion_tokens,
            openai.total_tokens
        ),
        (1201, 34, 1235)
    );

    let anthropic = wire_usage(ProviderOrigin::Anthropic, &fixture("anthropic_usage.json"))
        .expect("usage")
        .expect("present");
    assert_eq!(
        (
            anthropic.prompt_tokens,
            anthropic.completion_tokens,
            anthropic.total_tokens
        ),
        (517, 62, 579)
    );

    let gemini = wire_usage(ProviderOrigin::Gemini, &fixture("gemini_usage.json"))
        .expect("usage")
        .expect("present");
    assert_eq!(
        (
            gemini.prompt_tokens,
            gemini.completion_tokens,
            gemini.total_tokens
        ),
        (811, 27, 838)
    );
}

#[test]
fn missing_usage_is_unknown_not_zero() {
    // The tool-call fixtures carry no usage blocks at all.
    for (origin, wire) in [
        (
            ProviderOrigin::OpenAiCompatible,
            openai_message(&fixture("openai_assistant_tool_calls.json")),
        ),
        (
            ProviderOrigin::Anthropic,
            anthropic_message(&fixture("anthropic_thinking_tool_use.json")),
        ),
        (
            ProviderOrigin::Gemini,
            gemini_content(&fixture("gemini_function_call_no_id.json")),
        ),
    ] {
        assert_eq!(wire_usage(origin, &wire), Ok(None), "usage must be unknown");
    }
}

#[test]
fn malformed_usage_fails_typed() {
    let openai = serde_json::json!({"usage": {"prompt_tokens": "many"}});
    assert!(matches!(
        wire_usage(ProviderOrigin::OpenAiCompatible, &openai),
        Err(ProjectionError::MalformedUsage { .. })
    ));
    let anthropic = serde_json::json!({"usage": {"input_tokens": 1.5, "output_tokens": 2}});
    assert!(matches!(
        wire_usage(ProviderOrigin::Anthropic, &anthropic),
        Err(ProjectionError::MalformedUsage { .. })
    ));
    let gemini = serde_json::json!({"usageMetadata": {"promptTokenCount": null}});
    assert!(matches!(
        wire_usage(ProviderOrigin::Gemini, &gemini),
        Err(ProjectionError::MalformedUsage { .. })
    ));
}

#[test]
fn tool_arguments_exact_boundary_is_accepted_for_every_provider() {
    // 8 MiB of textual arguments lands exactly on the per-call limit; every
    // projector must accept it. Plus-one interruption for Anthropic/Gemini is
    // covered above; here OpenAI joins the family.
    let overhead = r#"{"a":""}"#.len();
    let filler = MAX_TOOL_CALL_ARGUMENTS_BYTES - overhead;

    let openai = serde_json::json!({"choices": [{"message": {
        "role": "assistant",
        "content": Value::Null,
        "tool_calls": [{"id": "call_1", "type": "function", "function": {
            "name": "t",
            "arguments": format!("{{\"a\":\"{}\"}}", "z".repeat(filler)),
        }}]
    }}]});
    wire_to_native(ProviderOrigin::OpenAiCompatible, &openai).expect("exact boundary");

    let openai_over = serde_json::json!({"choices": [{"message": {
        "role": "assistant",
        "content": Value::Null,
        "tool_calls": [{"id": "call_1", "type": "function", "function": {
            "name": "t",
            "arguments": format!("{{\"a\":\"{}\"}}", "z".repeat(filler + 1)),
        }}]
    }}]});
    assert_eq!(
        limit_kind(wire_to_native(
            ProviderOrigin::OpenAiCompatible,
            &openai_over
        )),
        LimitKind::ToolCallArguments
    );

    let anthropic_input = serde_json::json!({"a": "z".repeat(filler)});
    let anthropic = serde_json::json!({"role": "assistant", "content": [
        {"type": "tool_use", "id": "toolu_1", "name": "t", "input": anthropic_input}
    ]});
    wire_to_native(ProviderOrigin::Anthropic, &anthropic).expect("exact boundary");

    let gemini_args = serde_json::json!({"a": "z".repeat(filler)});
    let gemini = serde_json::json!({"candidates": [{"content": {"role": "model", "parts": [
        {"functionCall": {"name": "t", "args": gemini_args}}
    ]}}]});
    wire_to_native(ProviderOrigin::Gemini, &gemini).expect("exact boundary");
}

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

// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Provider replay/projection seam contract tests over fixed official-wire
//! fixtures (GitHub #142). No live network: every fixture is a reviewable
//! byte-fixed official response body.

use lattice_conversation::{
    MessageSemantic, NativeMessage, OpaqueProviderState, ProviderOrigin, UnboundToolCall,
    replay_state_digest, semantic_digest,
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
    assert_eq!(semantic_digest(&projected), source_semantic);
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
    assert_eq!(semantic_digest(&projected), semantic_digest(&source));
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

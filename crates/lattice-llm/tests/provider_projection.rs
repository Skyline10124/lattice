// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Provider replay/projection seam contract tests over fixed official-wire
//! fixtures (GitHub #142). No live network: every fixture is a reviewable
//! byte-fixed official response body.

use lattice_conversation::{
    AttemptInterruption, LimitKind, MAX_ASSISTANT_CONTENT_BYTES, MAX_MODEL_ATTEMPT_BYTES,
    MAX_REASONING_CONTENT_BYTES, MAX_TOOL_CALL_ARGUMENTS_BYTES, MAX_TOOL_CALLS_PER_BATCH,
    MessageSemantic, NativeMessage, OpaqueProviderState, Protocol, ReplayOrigin, UnboundToolCall,
};
use lattice_llm::protocol::projection::{
    ProjectionError, native_to_wire, project_cross_provider, wire_to_native,
};
use serde_json::Value;

// Compare the fields themselves: these projections only support seam assertions.
fn semantic_view(message: &NativeMessage<UnboundToolCall>) -> Value {
    let mut message = message.clone();
    message.provider_origin = None;
    for part in &mut message.parts {
        part.replay_state = None;
        if let MessageSemantic::ToolCall { call } = &mut part.semantic {
            call.provider_call_id = None;
        }
    }
    serde_json::to_value(message).unwrap()
}

fn replay_view(message: &NativeMessage<UnboundToolCall>) -> Value {
    serde_json::json!({
        "provider_origin": message.provider_origin,
        "parts": message.parts.iter().map(|part| {
            let id = match &part.semantic {
                MessageSemantic::ToolCall { call } => call.provider_call_id.as_deref(),
                _ => None,
            };
            (id, &part.replay_state)
        }).collect::<Vec<_>>()
    })
}
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
fn provider_projection_preserves_part_order_ids_and_signatures() {
    enum SemanticCheck {
        Reasoning,
        Text,
        EmptyText,
    }
    struct Row {
        name: &'static str,
        provider: Protocol,
        fixture: &'static str,
        origin: Option<ReplayOrigin>,
        parts_len: usize,
        semantic_checks: &'static [(usize, SemanticCheck)],
        replay_state_checks: &'static [(usize, &'static str)],
        calls: &'static [(usize, &'static str, Option<&'static str>)],
    }
    for row in [
        Row {
            name: "openai_order_and_ids",
            provider: Protocol::ChatCompletions,
            fixture: "openai_assistant_tool_calls.json",
            origin: Some(replay_origin(Protocol::ChatCompletions)),
            parts_len: 4,
            semantic_checks: &[(0, SemanticCheck::Reasoning), (1, SemanticCheck::Text)],
            replay_state_checks: &[],
            calls: &[
                (2, "call_9f8e7d6c5b", Some("get_weather")),
                (3, "call_1a2b3c4d5e", Some("get_time")),
            ],
        },
        Row {
            name: "anthropic_order_signature_and_ids",
            provider: Protocol::Messages,
            fixture: "anthropic_thinking_tool_use.json",
            origin: None,
            parts_len: 4,
            // Wire order: thinking, text, tool_use, tool_use.
            semantic_checks: &[(0, SemanticCheck::Reasoning), (1, SemanticCheck::Text)],
            replay_state_checks: &[(
                0,
                "ErUBCkYIAxgCIkD3c2FmZGVmNTQ2Ny0xMjM0LWFiY2QtZXhhbXBsZXNpZ25hdHVyZQ==",
            )],
            calls: &[(2, "toolu_01AbCdEfGh", None), (3, "toolu_02BcDeFgHi", None)],
        },
        Row {
            name: "gemini_signatures_at_original_ordinals",
            provider: Protocol::GenerateContent,
            fixture: "gemini_thought_signature.json",
            origin: None,
            parts_len: 3,
            semantic_checks: &[
                // Part 0: thought text with its signature.
                (0, SemanticCheck::Reasoning),
                // Part 1: empty text carrying a signature — preserved, not filtered.
                (1, SemanticCheck::EmptyText),
            ],
            replay_state_checks: &[
                (0, "CigIARABGkh4dGh5YnJpZC1zaWduYXR1cmUtb24tdGhvdWdodA=="),
                (1, "CigIARABGkh4ZW1wdHktdGV4dC1wYXJ0LXNpZ25hdHVyZQ=="),
                // Part 2: functionCall with a real id and its own signature.
                (2, "CigIARABGkh4c2lnbmF0dXJlLW9uLWZ1bmN0aW9uLWNhbGw="),
            ],
            calls: &[(2, "fc-2026-08-13-001", None)],
        },
    ] {
        let name = row.name;
        let native = wire_to_native(&replay_origin(row.provider), &fixture(row.fixture))
            .unwrap_or_else(|error| panic!("{name}: projection failed: {error:?}"));
        if let Some(origin) = row.origin {
            assert_eq!(
                native.provider_origin,
                Some(origin),
                "{name}: provider origin"
            );
        }
        assert_eq!(native.parts.len(), row.parts_len, "{name}: part count");
        for (ordinal, check) in row.semantic_checks {
            let semantic = &native.parts[*ordinal].semantic;
            let matches = match check {
                SemanticCheck::Reasoning => matches!(semantic, MessageSemantic::Reasoning { .. }),
                SemanticCheck::Text => matches!(semantic, MessageSemantic::Text { .. }),
                SemanticCheck::EmptyText => {
                    matches!(semantic, MessageSemantic::Text { text } if text.is_empty())
                }
            };
            assert!(matches, "{name}: part {ordinal} semantic");
        }
        for (ordinal, signature) in row.replay_state_checks {
            assert_eq!(
                native.parts[*ordinal]
                    .replay_state
                    .as_ref()
                    .map(OpaqueProviderState::as_str),
                Some(*signature),
                "{name}: part {ordinal} replay state"
            );
        }
        let calls = tool_call_parts(&native);
        assert_eq!(calls.len(), row.calls.len(), "{name}: call count");
        for (index, (ordinal, id, call_name)) in row.calls.iter().enumerate() {
            assert_eq!(
                calls[index].0, *ordinal,
                "{name}: call {index} part ordinal"
            );
            assert_eq!(
                calls[index].1.provider_call_id.as_deref(),
                Some(*id),
                "{name}: call {index} provider id"
            );
            if let Some(call_name) = call_name {
                assert_eq!(calls[index].1.name, *call_name, "{name}: call {index} name");
            }
        }
    }
}

#[test]
fn same_provider_replay_is_exact() {
    for (name, provider, extract) in [
        (
            "openai_assistant_tool_calls.json",
            Protocol::ChatCompletions,
            openai_message as fn(&Value) -> Value,
        ),
        (
            "anthropic_thinking_tool_use.json",
            Protocol::Messages,
            anthropic_message,
        ),
        (
            "anthropic_text_only.json",
            Protocol::Messages,
            anthropic_message,
        ),
        (
            "gemini_thought_signature.json",
            Protocol::GenerateContent,
            gemini_content,
        ),
        (
            "gemini_function_call_no_id.json",
            Protocol::GenerateContent,
            gemini_content,
        ),
    ] {
        let fixture = fixture(name);
        let native = wire_to_native(&replay_origin(provider), &fixture)
            .unwrap_or_else(|error| panic!("{name}: projection failed: {error:?}"));
        let replayed = native_to_wire(&replay_origin(provider), &native)
            .unwrap_or_else(|error| panic!("{name}: replay failed: {error:?}"));
        assert_eq!(replayed, extract(&fixture), "{name}");
    }
}

#[test]
fn openai_missing_provider_id_is_none_not_synthesized() {
    let fixture = fixture("openai_assistant_missing_id.json");
    let native =
        wire_to_native(&replay_origin(Protocol::ChatCompletions), &fixture).expect("projection");
    let calls = tool_call_parts(&native);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].1.provider_call_id, None);

    let replayed =
        native_to_wire(&replay_origin(Protocol::ChatCompletions), &native).expect("replay");
    let serialized = serde_json::to_string(&replayed).expect("serialize");
    assert!(
        !serialized.contains("tc_"),
        "no synthetic tc_* ids: {serialized}"
    );
    assert_eq!(replayed, openai_message(&fixture));
}

// ─── Anthropic ────────────────────────────────────────────────────────────

#[test]
fn anthropic_thinking_without_signature_cannot_replay() {
    // Fail closed: the signature is protocol-required replay state and is
    // never fabricated.
    let mut native = wire_to_native(
        &replay_origin(Protocol::Messages),
        &fixture("anthropic_thinking_tool_use.json"),
    )
    .expect("projection");
    native.parts[0].replay_state = None;
    let result = native_to_wire(&replay_origin(Protocol::Messages), &native);
    assert!(matches!(
        result,
        Err(ProjectionError::MissingReplayState {
            provider: Protocol::Messages,
            part_ordinal: 0,
        })
    ));
}

// ─── Gemini ───────────────────────────────────────────────────────────────

#[test]
fn gemini_function_call_without_id_is_none_not_synthesized() {
    let fixture = fixture("gemini_function_call_no_id.json");
    let native =
        wire_to_native(&replay_origin(Protocol::GenerateContent), &fixture).expect("projection");
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
    let native = wire_to_native(&replay_origin(Protocol::GenerateContent), &parameterless)
        .expect("projection");
    assert_eq!(
        tool_call_parts(&native)[0].1.arguments,
        serde_json::json!({}),
        "an omitted Gemini args field is the documented empty object",
    );
}

// ─── Cross-provider projection ────────────────────────────────────────────

#[test]
fn replay_origin_matrix_separates_opaque_state_from_call_ids() {
    let protocols = [
        Protocol::ChatCompletions,
        Protocol::Messages,
        Protocol::GenerateContent,
    ];
    for (protocol, fixture_name) in [
        (
            Protocol::ChatCompletions,
            "openai_assistant_tool_calls.json",
        ),
        (Protocol::Messages, "anthropic_thinking_tool_use.json"),
        (Protocol::GenerateContent, "gemini_thought_signature.json"),
    ] {
        let original_origin = replay_origin(protocol);
        let source = wire_to_native(&original_origin, &fixture(fixture_name)).unwrap();
        let original_wire = native_to_wire(&original_origin, &source).unwrap();
        let source_ids: Vec<_> = source
            .tool_calls()
            .map(|(_, call)| call.provider_call_id.as_deref())
            .collect();
        let opaque: Vec<_> = source
            .parts
            .iter()
            .flat_map(|part| {
                let mut materials = Vec::new();
                if let MessageSemantic::Reasoning { text } = &part.semantic {
                    materials.push(text.as_str());
                }
                if let Some(state) = &part.replay_state {
                    materials.push(state.as_str());
                }
                materials
            })
            .collect();
        let bound = bind_message(&source);
        let results = bound
            .tool_calls()
            .map(|(_, call)| {
                success(
                    call.invocation_id,
                    &call.name,
                    serde_json::json!({"ok":true}),
                )
            })
            .collect();
        let history = [ConversationItem::Step(StepContent {
            assistant: bound,
            results,
        })];
        let original_request =
            lattice_llm::protocol::projection::request_messages_to_wire(&original_origin, &history)
                .unwrap()
                .messages;
        for target_protocol in protocols {
            for provider in ["test", "other"] {
                let target = ReplayOrigin {
                    protocol: target_protocol,
                    provider_id: provider.into(),
                };
                let same_provider = target_protocol == protocol && provider == "test";
                let same_protocol = target_protocol == protocol;
                let projected = project_cross_provider(&source, &target);
                assert_eq!(
                    projected
                        .parts
                        .iter()
                        .any(|p| matches!(p.semantic, MessageSemantic::Reasoning { .. })),
                    same_provider,
                    "{original_origin:?} -> {target:?}"
                );
                assert_eq!(
                    projected.parts.iter().any(|p| p.replay_state.is_some()),
                    same_provider && protocol != Protocol::ChatCompletions
                );
                let calls: Vec<_> = projected
                    .tool_calls()
                    .map(|(_, call)| call.provider_call_id.as_deref())
                    .collect();
                assert_eq!(
                    calls,
                    if same_protocol {
                        source_ids.clone()
                    } else {
                        vec![None; source_ids.len()]
                    }
                );
                let projected_wire = native_to_wire(&target, &projected).unwrap();
                if same_provider {
                    assert_eq!(projected_wire, original_wire);
                } else {
                    let serialized = projected_wire.to_string();
                    for material in &opaque {
                        assert!(
                            !serialized.contains(material),
                            "opaque material leaked: {original_origin:?} -> {target:?}"
                        );
                    }
                }
                let encoded =
                    lattice_llm::protocol::projection::request_messages_to_wire(&target, &history);
                if !same_protocol && target_protocol != Protocol::GenerateContent {
                    assert!(matches!(
                        encoded,
                        Err(ProjectionError::ToolResultWithoutProviderCallId { .. })
                    ));
                } else {
                    let encoded = encoded.unwrap();
                    let messages = &encoded.messages;
                    let mut expected_assistant = projected_wire.clone();
                    if target_protocol == Protocol::ChatCompletions {
                        expected_assistant
                            .as_object_mut()
                            .unwrap()
                            .remove("reasoning_content");
                    }
                    assert_eq!(messages[0], expected_assistant);
                    if !same_provider {
                        let serialized = serde_json::to_string(messages).unwrap();
                        for material in &opaque {
                            assert!(!serialized.contains(material));
                        }
                    }
                    match target_protocol {
                        Protocol::ChatCompletions => {
                            for (index, id) in source_ids.iter().enumerate() {
                                assert_eq!(messages[0]["tool_calls"][index]["id"], id.unwrap());
                                assert_eq!(messages[index + 1]["tool_call_id"], id.unwrap());
                            }
                        }
                        Protocol::Messages => {
                            let calls: Vec<_> = messages[0]["content"]
                                .as_array()
                                .unwrap()
                                .iter()
                                .filter(|part| part["type"] == "tool_use")
                                .collect();
                            for (index, id) in source_ids.iter().enumerate() {
                                assert_eq!(calls[index]["id"], id.unwrap());
                                assert_eq!(
                                    messages[1]["content"][index]["tool_use_id"],
                                    id.unwrap()
                                );
                            }
                        }
                        Protocol::GenerateContent => {
                            let calls: Vec<_> = messages[0]["parts"]
                                .as_array()
                                .unwrap()
                                .iter()
                                .filter_map(|part| part.get("functionCall"))
                                .collect();
                            for (index, call) in calls.iter().enumerate() {
                                assert_eq!(
                                    call.get("id").and_then(Value::as_str),
                                    if same_protocol {
                                        source_ids[index]
                                    } else {
                                        None
                                    }
                                );
                                assert_eq!(
                                    messages[1]["parts"][index]["functionResponse"]["name"],
                                    call["name"]
                                );
                                assert!(
                                    messages[1]["parts"][index]["functionResponse"]
                                        .get("id")
                                        .is_none()
                                );
                            }
                        }
                        _ => unreachable!(),
                    }
                }
                let restored = project_cross_provider(&source, &original_origin);
                assert_eq!(
                    native_to_wire(&original_origin, &restored).unwrap(),
                    original_wire
                );
                assert_eq!(
                    lattice_llm::protocol::projection::request_messages_to_wire(
                        &original_origin,
                        &history
                    )
                    .unwrap()
                    .messages,
                    original_request
                );
            }
        }
    }
}

#[test]
fn cross_provider_projection_never_leaks_source_state() {
    let source = wire_to_native(
        &replay_origin(Protocol::Messages),
        &fixture("anthropic_thinking_tool_use.json"),
    )
    .expect("projection");
    let source_semantic = semantic_view(&source);
    let source_replay = replay_view(&source);

    // A (Anthropic) → B (Gemini): semantics cross, opaque state does not.
    let projected = project_cross_provider(&source, &replay_origin(Protocol::GenerateContent));
    assert_eq!(
        projected.provider_origin,
        Some(replay_origin(Protocol::GenerateContent))
    );
    for part in &projected.parts {
        assert_eq!(part.replay_state, None);
    }
    for (_, call) in projected.tool_calls() {
        assert_eq!(call.provider_call_id, None);
    }
    // Reasoning does not cross providers (ADR 0005 §7), so the projection
    // carries no Reasoning part and its semantic fields necessarily differ
    // from a source that had one.
    assert!(
        !projected
            .parts
            .iter()
            .any(|part| matches!(part.semantic, MessageSemantic::Reasoning { .. })),
        "projection must not carry source reasoning"
    );
    assert_ne!(semantic_view(&projected), source_semantic);
    assert_ne!(replay_view(&projected), source_replay);

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
    let replayed = native_to_wire(&replay_origin(Protocol::Messages), &source).expect("replay");
    assert_eq!(
        replayed,
        anthropic_message(&fixture("anthropic_thinking_tool_use.json"))
    );
    assert_eq!(replay_view(&source), source_replay);
}

#[test]
fn cross_provider_gemini_to_openai_drops_signatures() {
    let source = wire_to_native(
        &replay_origin(Protocol::GenerateContent),
        &fixture("gemini_thought_signature.json"),
    )
    .expect("projection");
    let projected = project_cross_provider(&source, &replay_origin(Protocol::ChatCompletions));
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
    assert_ne!(semantic_view(&projected), semantic_view(&source));
}

#[test]
fn cross_provider_projection_preserves_non_reasoning_semantics() {
    // A source without reasoning crosses losslessly: dropping reasoning is the
    // only semantic difference the projection introduces.
    let source = wire_to_native(
        &replay_origin(Protocol::Messages),
        &fixture("anthropic_text_only.json"),
    )
    .expect("projection");
    let projected = project_cross_provider(&source, &replay_origin(Protocol::GenerateContent));
    assert_eq!(semantic_view(&projected), semantic_view(&source));
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
    let cases: [(Protocol, &str, Protocol, &str); 3] = [
        (
            Protocol::Messages,
            "anthropic_thinking_tool_use.json",
            Protocol::ChatCompletions,
            "Need the weather first, then the time; both are independent calls.",
        ),
        (
            Protocol::Messages,
            "anthropic_thinking_tool_use.json",
            Protocol::GenerateContent,
            "Need the weather first, then the time; both are independent calls.",
        ),
        (
            Protocol::GenerateContent,
            "gemini_thought_signature.json",
            Protocol::ChatCompletions,
            "Plan: check weather, then time.",
        ),
    ];

    for (source_origin, fixture_name, target, reasoning_text) in cases {
        let source = wire_to_native(&replay_origin(source_origin), &fixture(fixture_name))
            .expect("source projection");
        // Precondition: the source really does carry that reasoning text.
        assert!(
            source.parts.iter().any(|part| matches!(
                &part.semantic,
                MessageSemantic::Reasoning { text } if text == reasoning_text
            )),
            "fixture {fixture_name} must carry the reasoning under test"
        );

        let projected = project_cross_provider(&source, &replay_origin(target));
        let wire = native_to_wire(&replay_origin(target), &projected).expect("target wire");
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
        &replay_origin(Protocol::GenerateContent),
        &fixture("gemini_function_call_no_id.json"),
    )
    .expect("projection");
    let result = native_to_wire(&replay_origin(Protocol::Messages), &native);
    assert!(matches!(
        result,
        Err(ProjectionError::WrongOrigin {
            expected, found,
        }) if expected == replay_origin(Protocol::Messages) && found == Some(replay_origin(Protocol::GenerateContent))
    ));
}

#[test]
fn malformed_wire_is_rejected() {
    let garbage = serde_json::json!({"unexpected": true});
    assert!(matches!(
        wire_to_native(&replay_origin(Protocol::Messages), &garbage),
        Err(ProjectionError::MalformedWire { .. })
    ));
    assert!(matches!(
        wire_to_native(&replay_origin(Protocol::ChatCompletions), &garbage),
        Err(ProjectionError::MalformedWire { .. })
    ));
    assert!(matches!(
        wire_to_native(&replay_origin(Protocol::GenerateContent), &garbage),
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
    let result = wire_to_native(&replay_origin(Protocol::Messages), &fixture);
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
        Some(replay_origin(Protocol::Messages)),
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
        native_to_wire(&replay_origin(Protocol::Messages), &message),
        Err(ProjectionError::UnsupportedPart {
            provider: Protocol::Messages,
            part_ordinal: 0,
        })
    ));
}

// ─── OpenAI writer lossiness, explicitly pinned (P2-2, P2-4) ──────────────

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
    let source = wire_to_native(&replay_origin(Protocol::Messages), &wire).expect("projection");
    assert_eq!(source.parts.len(), 3);

    let projected = project_cross_provider(&source, &replay_origin(Protocol::ChatCompletions));
    let replayed =
        native_to_wire(&replay_origin(Protocol::ChatCompletions), &projected).expect("replay");
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
    let native =
        wire_to_native(&replay_origin(Protocol::ChatCompletions), &exact).expect("exact boundary");
    assert_eq!(native.parts.len(), 1);

    let over = openai_content_wire("x".repeat(MAX_ASSISTANT_CONTENT_BYTES + 1));
    assert_eq!(
        limit_kind(wire_to_native(
            &replay_origin(Protocol::ChatCompletions),
            &over
        )),
        LimitKind::AssistantContent
    );
}

#[test]
fn reasoning_content_limit_is_enforced_at_the_seam() {
    let exact = openai_reasoning_wire("r".repeat(MAX_REASONING_CONTENT_BYTES));
    wire_to_native(&replay_origin(Protocol::ChatCompletions), &exact).expect("exact boundary");

    let over = openai_reasoning_wire("r".repeat(MAX_REASONING_CONTENT_BYTES + 1));
    assert_eq!(
        limit_kind(wire_to_native(
            &replay_origin(Protocol::ChatCompletions),
            &over
        )),
        LimitKind::ReasoningContent
    );
}

#[test]
fn attempt_total_limit_is_enforced_at_the_seam() {
    // 2 MiB assistant + 2 MiB reasoning + 12 MiB of tool arguments spread over
    // half-limit calls (the per-call budget resets per call, so only the
    // 16 MiB aggregate can trip here).
    let filler =
        MAX_MODEL_ATTEMPT_BYTES - MAX_ASSISTANT_CONTENT_BYTES - MAX_REASONING_CONTENT_BYTES;
    let overhead = r#"{"a":""}"#.len();
    let per_call = MAX_TOOL_CALL_ARGUMENTS_BYTES / 2;
    let calls = filler / per_call;
    assert_eq!(calls * per_call, filler);
    assert!(calls <= MAX_TOOL_CALLS_PER_BATCH);

    let build = |extra: usize| {
        let tool_calls: Vec<_> = (0..calls)
            .map(|i| {
                let metadata = 1 + format!("call_{}", i + 1).len();
                let len = per_call - overhead - metadata + if i + 1 == calls { extra } else { 0 };
                serde_json::json!({
                    "id": format!("call_{}", i + 1),
                    "type": "function",
                    "function": {
                        "name": "t",
                        "arguments": format!("{{\"a\":\"{}\"}}", "z".repeat(len)),
                    }
                })
            })
            .collect();
        serde_json::json!({"choices": [{"message": {
            "role": "assistant",
            "content": "x".repeat(MAX_ASSISTANT_CONTENT_BYTES),
            "reasoning_content": "r".repeat(MAX_REASONING_CONTENT_BYTES),
            "tool_calls": tool_calls,
        }}]})
    };

    let exact = build(0);
    wire_to_native(&replay_origin(Protocol::ChatCompletions), &exact)
        .expect("exact attempt boundary");

    let over = build(1);
    assert_eq!(
        limit_kind(wire_to_native(
            &replay_origin(Protocol::ChatCompletions),
            &over
        )),
        LimitKind::ModelAttemptTotal
    );
}

#[test]
fn tool_argument_limits_apply_to_every_provider() {
    let filler = MAX_TOOL_CALL_ARGUMENTS_BYTES - r#"{"a":""}"#.len();
    for (name, provider, filler_bytes, accepted) in [
        // Every provider projector meters, not just the OpenAI one.
        (
            "anthropic_original_oversized_arguments",
            Protocol::Messages,
            MAX_TOOL_CALL_ARGUMENTS_BYTES,
            false,
        ),
        (
            "gemini_original_oversized_arguments",
            Protocol::GenerateContent,
            MAX_TOOL_CALL_ARGUMENTS_BYTES,
            false,
        ),
        // 1 MiB of textual arguments lands exactly on the per-call limit.
        (
            "openai_exact_arguments_boundary",
            Protocol::ChatCompletions,
            filler,
            true,
        ),
        (
            "openai_arguments_boundary_plus_one",
            Protocol::ChatCompletions,
            filler + 1,
            false,
        ),
        (
            "anthropic_exact_arguments_boundary",
            Protocol::Messages,
            filler,
            true,
        ),
        (
            "gemini_exact_arguments_boundary",
            Protocol::GenerateContent,
            filler,
            true,
        ),
    ] {
        let arguments = "z".repeat(filler_bytes);
        let wire = match provider {
            Protocol::ChatCompletions => serde_json::json!({"choices": [{"message": {
                "role": "assistant", "content": Value::Null,
                "tool_calls": [{"id": "call_1", "type": "function", "function": {
                    "name": "t", "arguments": format!("{{\"a\":\"{arguments}\"}}"),
                }}],
            }}]}),
            Protocol::Messages => serde_json::json!({"role": "assistant", "content": [
                {"type": "tool_use", "id": "toolu_1", "name": "t", "input": {"a": arguments}}
            ]}),
            Protocol::GenerateContent => {
                serde_json::json!({"candidates": [{"content": {"role": "model", "parts": [
                    {"functionCall": {"name": "t", "args": {"a": arguments}}}
                ]}}]})
            }
            _ => unreachable!("fixture selects an implemented protocol"),
        };
        let result = wire_to_native(&replay_origin(provider), &wire);
        if accepted {
            result.unwrap_or_else(|error| panic!("{name}: exact boundary rejected: {error:?}"));
        } else {
            let kind = match result {
                Err(ProjectionError::OutputLimitExceeded {
                    interruption: AttemptInterruption::OutputLimitExceeded { kind, .. },
                    ..
                }) => kind,
                Ok(_) => panic!("{name}: oversized wire must not produce a message"),
                Err(other) => panic!("{name}: expected OutputLimitExceeded, got {other:?}"),
            };
            assert_eq!(kind, LimitKind::ToolCallArguments, "{name}");
        }
    }
}

// ─── #139-B Commit 2: bound closures, ToolResult encoding, usage ──────────

use lattice_conversation::{
    BoundToolCall, InvocationId, MessagePart, MessageRole, ModelMessageAddress, SessionId,
    ToolCallAddress, ToolResultOutcome, TurnId,
};
use lattice_llm::protocol::projection::{tool_results_to_wire, wire_usage};

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
        };
        bound_parts.push(MessagePart {
            semantic,
            replay_state: part.replay_state.clone(),
        });
    }
    NativeMessage::new(message.role, message.provider_origin.clone(), bound_parts)
}

#[test]
fn bound_closure_vocabulary_replays_identically_to_unbound() {
    // The persisted closure vocabulary is `NativeMessage<BoundToolCall>`;
    // same-provider replay must be byte-identical to the unbound replay
    // because the provider-visible fields are the same typestate accessors.
    let cases = [
        (
            Protocol::ChatCompletions,
            fixture("openai_assistant_tool_calls.json"),
        ),
        (
            Protocol::Messages,
            anthropic_message(&fixture("anthropic_thinking_tool_use.json")),
        ),
        (
            Protocol::GenerateContent,
            fixture("gemini_thought_signature.json"),
        ),
    ];
    for (origin, wire_message) in cases {
        let unbound = wire_to_native(&replay_origin(origin), &wire_message).expect("projection");
        let bound = bind_message(&unbound);
        let unbound_wire =
            native_to_wire(&replay_origin(origin), &unbound).expect("unbound replay");
        let bound_wire = native_to_wire(&replay_origin(origin), &bound).expect("bound replay");
        assert_eq!(unbound_wire, bound_wire);
    }
}

#[test]
fn cross_provider_projection_drops_bound_provider_ids_and_replay_state() {
    let native = wire_to_native(
        &replay_origin(Protocol::Messages),
        &anthropic_message(&fixture("anthropic_thinking_tool_use.json")),
    )
    .expect("projection");
    let bound = bind_message(&native);
    let projected = project_cross_provider(&bound, &replay_origin(Protocol::ChatCompletions));
    assert_eq!(
        projected.provider_origin,
        Some(replay_origin(Protocol::ChatCompletions))
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

use lattice_conversation::{
    ConversationItem, IndeterminateCause, ResultPayload, StepContent, StoredToolResult,
    SuccessContent, canonical_json, tool_content_digest,
};

fn success(invocation_id: InvocationId, name: &str, value: Value) -> StoredToolResult {
    let bytes = canonical_json(&value);
    StoredToolResult {
        invocation_id,
        name: name.into(),
        payload: ResultPayload::Succeeded(SuccessContent::Json {
            value,
            digest: tool_content_digest(&bytes),
            byte_length: bytes.len() as u64,
        }),
    }
}

#[test]
fn structured_steps_require_exact_pairing() {
    let origin = Protocol::ChatCompletions;
    let assistant = bind_message(
        &wire_to_native(
            &replay_origin(origin),
            &fixture("openai_assistant_tool_calls.json"),
        )
        .unwrap(),
    );
    let results = vec![
        success(invocation(0), "get_weather", serde_json::json!({})),
        success(invocation(1), "get_time", serde_json::json!({})),
    ];
    for case in 0..8 {
        let mut step = StepContent {
            assistant: assistant.clone(),
            results: results.clone(),
        };
        match case {
            0 => {
                step.results.pop();
            }
            1 => step.results.push(results[0].clone()),
            2 => step.results[1] = results[0].clone(),
            3 => step.results.swap(0, 1),
            4 => step.results[0].name = "wrong".into(),
            5 => step.assistant.role = MessageRole::Tool,
            6 => step.results[0].invocation_id = invocation(9),
            _ => {
                if let ResultPayload::Succeeded(SuccessContent::Json { digest, .. }) =
                    &mut step.results[0].payload
                {
                    *digest = [0; 32];
                }
            }
        }
        assert!(
            lattice_llm::protocol::projection::request_messages_to_wire(
                &replay_origin(origin),
                &[ConversationItem::Step(step)]
            )
            .is_err(),
            "case {case}"
        );
    }
    for role in [MessageRole::Tool, MessageRole::Assistant] {
        let mut message = assistant.clone();
        message.role = role;
        assert!(
            lattice_llm::protocol::projection::request_messages_to_wire(
                &replay_origin(origin),
                &[ConversationItem::Message(message)]
            )
            .is_err()
        );
    }
}

#[test]
fn structured_result_outcomes_and_large_success() {
    use lattice_conversation::{CepError, CepErrorCode, CepErrorData, Retriability};
    let mut rejected = CepError::new(
        CepErrorCode::ExecutionFailed,
        "界".repeat(2000),
        serde_json::from_value::<CepErrorData>(
            serde_json::json!({"kind":"execution","operation":"internal","detail":null}),
        )
        .unwrap(),
        Retriability::Transient,
    );
    rejected.code = CepErrorCode::CapabilityNotGranted;
    let variants = [
        success(
            invocation(0),
            "get_weather",
            serde_json::json!({"z":1,"a":2}),
        )
        .payload,
        ResultPayload::Failed(Box::new(CepError::new(
            CepErrorCode::ExecutionFailed,
            "failed",
            serde_json::from_value::<CepErrorData>(
                serde_json::json!({"kind":"execution","operation":"get_weather","detail":null}),
            )
            .unwrap(),
            Retriability::Permanent,
        ))),
        ResultPayload::Failed(Box::new(rejected.clone())),
        ResultPayload::Indeterminate(IndeterminateCause::Cancelled),
        ResultPayload::NotExecuted,
        ResultPayload::Indeterminate(IndeterminateCause::NoResult),
        ResultPayload::Succeeded(SuccessContent::OutputTooLarge {
            digest: [0xab; 32],
            byte_length: 65537,
        }),
    ];
    for origin in [
        Protocol::ChatCompletions,
        Protocol::Messages,
        Protocol::GenerateContent,
    ] {
        let assistant = bind_message(
            &wire_to_native(
                &replay_origin(Protocol::GenerateContent),
                &fixture("gemini_function_call_no_id.json"),
            )
            .unwrap(),
        );
        let mut assistant = assistant;
        if let MessageSemantic::ToolCall { call } = &mut assistant.parts[0].semantic {
            call.provider_call_id = Some("real-id".into());
        }
        for (index, payload) in variants.iter().enumerate() {
            let results = [StoredToolResult {
                invocation_id: invocation(0),
                name: "get_weather".into(),
                payload: payload.clone(),
            }];
            let wire = tool_results_to_wire(origin, &assistant, &results).unwrap();
            let text = match origin {
                Protocol::ChatCompletions => wire[0]["content"].as_str().unwrap().to_string(),
                Protocol::Messages => wire[0]["content"][0]["content"]
                    .as_str()
                    .unwrap()
                    .to_string(),
                Protocol::GenerateContent => {
                    let response = &wire[0]["parts"][0]["functionResponse"]["response"];
                    if payload.outcome() == ToolResultOutcome::Failed {
                        response["error"].as_str().unwrap().into()
                    } else {
                        response.to_string()
                    }
                }
                _ => unreachable!("fixture selects an implemented protocol"),
            };
            let json: Value = serde_json::from_str(&text).unwrap();
            match index {
                0 => assert_eq!(json, serde_json::json!({"a":2,"z":1})),
                1 => assert_eq!(
                    json,
                    serde_json::json!({"error":{"code":CepErrorCode::ExecutionFailed.as_i32(),"message":"failed"}})
                ),
                2 => assert_eq!(
                    json,
                    serde_json::json!({"error":{"code":CepErrorCode::CapabilityNotGranted.as_i32(),"message":"界".repeat(1365)}})
                ),
                3 => assert_eq!(
                    json,
                    serde_json::json!({"outcome":"indeterminate","cause":"cancelled"})
                ),
                4 => assert_eq!(json, serde_json::json!({"outcome":"not_executed"})),
                5 => assert_eq!(
                    json,
                    serde_json::json!({"outcome":"indeterminate","cause":"no_result"})
                ),
                _ => assert_eq!(json["outcome"], "succeeded"),
            }
            if index == 6 {
                assert_eq!(json["content_unavailable"], "output_too_large");
                assert_eq!(json["digest"], "ab".repeat(32));
                assert_eq!(json["byte_length"], 65537);
            }
        }
    }
}

#[test]
fn structured_result_fixtures_and_missing_ids() {
    for (origin, source, expected) in [
        (
            Protocol::ChatCompletions,
            "openai_assistant_tool_calls.json",
            "openai_tool_result_request.json",
        ),
        (
            Protocol::Messages,
            "anthropic_thinking_tool_use.json",
            "anthropic_tool_result_request.json",
        ),
        (
            Protocol::GenerateContent,
            "gemini_function_call_no_id.json",
            "gemini_tool_result_request.json",
        ),
    ] {
        let bound =
            bind_message(&wire_to_native(&replay_origin(origin), &fixture(source)).unwrap());
        let mut results = vec![success(
            invocation(0),
            "get_weather",
            serde_json::json!({"city":if origin == Protocol::GenerateContent {"Paris"} else {"Tokyo"}, "temperature":if origin == Protocol::GenerateContent {18} else {21}}),
        )];
        if origin != Protocol::GenerateContent {
            results.push(StoredToolResult {
                invocation_id: invocation(1),
                name: "get_time".into(),
                payload: ResultPayload::NotExecuted,
            });
        }
        if origin == Protocol::Messages {
            results[0].payload = ResultPayload::Failed(Box::new(serde_json::from_value(serde_json::json!({"code":"execution_failed","message":"permission denied by policy","data":{"kind":"execution","operation":"get_weather","detail":null},"retriability":"permanent"})).unwrap()));
        }
        let encoded = tool_results_to_wire(origin, &bound, &results).unwrap();
        assert_eq!(
            if origin == Protocol::ChatCompletions {
                Value::Array(encoded)
            } else {
                encoded[0].clone()
            },
            fixture(expected)
        );
        if origin == Protocol::GenerateContent {
            for target in [Protocol::ChatCompletions, Protocol::Messages] {
                assert!(matches!(
                    tool_results_to_wire(target, &bound, &results),
                    Err(ProjectionError::ToolResultWithoutProviderCallId { .. })
                ));
            }
        }
        let step = StepContent {
            assistant: bound,
            results,
        };
        let saved = step.clone();
        let projected = lattice_llm::protocol::projection::request_messages_to_wire(
            &replay_origin(Protocol::GenerateContent),
            &[ConversationItem::Step(step.clone())],
        )
        .unwrap();
        assert!(
            !serde_json::to_string(&projected.messages)
                .unwrap()
                .contains("toolu_")
        );
        assert_eq!(step, saved);
    }
}

#[test]
fn wire_usage_distinguishes_present_absent_and_malformed_blocks() {
    let cases = [
        (
            Protocol::ChatCompletions,
            fixture("openai_usage.json"),
            serde_json::json!([1201, null, null, 34, null]),
        ),
        (
            Protocol::Messages,
            fixture("anthropic_usage.json"),
            serde_json::json!([517, null, null, 62, null]),
        ),
        (
            Protocol::GenerateContent,
            fixture("gemini_usage.json"),
            serde_json::json!([811, 0, null, 27, 0]),
        ),
        (
            Protocol::ChatCompletions,
            serde_json::json!({"usage":{
                "prompt_tokens":1,"completion_tokens":2,"prompt_cache_hit_tokens":99,
                "prompt_tokens_details":{"cached_tokens":0,"cache_creation_input_tokens":8,"cache_write_tokens":3},
                "completion_tokens_details":{"reasoning_tokens":9}
            }}),
            serde_json::json!([1, 0, 8, 2, 9]),
        ),
        (
            Protocol::ChatCompletions,
            serde_json::json!({"usage":{"prompt_cache_hit_tokens":7}}),
            serde_json::json!([null, 7, null, null, null]),
        ),
        (
            Protocol::ChatCompletions,
            serde_json::json!({"usage":{"prompt_tokens":10,"completion_tokens":1,"prompt_tokens_details":{"cached_tokens":2,"cache_write_tokens":3}}}),
            serde_json::json!([10, 2, 3, 1, null]),
        ),
        (
            Protocol::ChatCompletions,
            serde_json::json!({"usage":{"prompt_tokens_details":{"cache_write_tokens":0}}}),
            serde_json::json!([null, null, 0, null, null]),
        ),
        (
            Protocol::ChatCompletions,
            serde_json::json!({"usage":{"prompt_tokens_details":{"cache_creation_input_tokens":0,"cache_write_tokens":9}}}),
            serde_json::json!([null, null, 0, null, null]),
        ),
        (
            Protocol::Messages,
            serde_json::json!({"usage":{"input_tokens":10,"cache_creation_input_tokens":20,"cache_read_input_tokens":30,"output_tokens":0}}),
            serde_json::json!([60, 30, 20, 0, null]),
        ),
        (
            Protocol::Messages,
            serde_json::json!({"usage":{"cache_read_input_tokens":7}}),
            serde_json::json!([7, 7, null, null, null]),
        ),
        (
            Protocol::GenerateContent,
            serde_json::json!({"usageMetadata":{"promptTokenCount":10,"toolUsePromptTokenCount":3,"cachedContentTokenCount":5,"candidatesTokenCount":4,"thoughtsTokenCount":6,"totalTokenCount":999}}),
            serde_json::json!([13, 5, null, 10, 6]),
        ),
        (
            Protocol::GenerateContent,
            serde_json::json!({"usageMetadata":{"toolUsePromptTokenCount":0,"thoughtsTokenCount":2}}),
            serde_json::json!([null, 0, null, 2, 2]),
        ),
    ];
    for (protocol, wire, expected) in cases {
        let value = serde_json::to_value(wire_usage(protocol, &wire).unwrap().unwrap()).unwrap();
        assert_eq!(
            serde_json::json!([
                value["input"],
                value["cache_read"],
                value["cache_write"],
                value["output"],
                value["reasoning"]
            ]),
            expected,
            "{protocol:?}: {wire}"
        );
    }
    for (protocol, key, field) in [
        (Protocol::ChatCompletions, "usage", "prompt_tokens"),
        (Protocol::Messages, "usage", "input_tokens"),
        (
            Protocol::GenerateContent,
            "usageMetadata",
            "promptTokenCount",
        ),
    ] {
        for wire in [
            serde_json::json!({}),
            serde_json::json!({key:null}),
            serde_json::json!({key:{}}),
            serde_json::json!({key:{field:null}}),
        ] {
            assert_eq!(wire_usage(protocol, &wire), Ok(None), "{wire}");
        }
        for invalid in [
            serde_json::json!("many"),
            serde_json::json!(-1),
            serde_json::json!(1.5),
            serde_json::from_str("18446744073709551616").unwrap(),
        ] {
            let wire = serde_json::json!({key:{field:invalid}});
            assert!(
                matches!(
                    wire_usage(protocol, &wire),
                    Err(ProjectionError::MalformedUsage { .. })
                ),
                "{wire}"
            );
        }
        assert!(matches!(
            wire_usage(protocol, &serde_json::json!({key:[]})),
            Err(ProjectionError::MalformedUsage { .. })
        ));
    }
}

fn replay_origin(protocol: Protocol) -> ReplayOrigin {
    ReplayOrigin {
        protocol,
        provider_id: "test".into(),
    }
}

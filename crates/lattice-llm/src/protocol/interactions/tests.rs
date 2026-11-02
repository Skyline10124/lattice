// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::reply_stream::ReplyDelivery;
use crate::{AttemptReport, LlmEvent, ReplyOutcome, ReplyStop, StreamStop};
use lattice_conversation::{MessageSemantic, TokenUsage};

fn origin() -> ReplayOrigin {
    ReplayOrigin {
        protocol: Protocol::Interactions,
        provider_id: "test".into(),
    }
}

fn completed(status: &str) -> Value {
    json!({"event_type":"interaction.completed","interaction":{"status":status}})
}

fn start(index: usize, step: Value) -> Value {
    json!({"event_type":"step.start","index":index,"step":step})
}

fn delta(index: usize, delta: Value) -> Value {
    json!({"event_type":"step.delta","index":index,"delta":delta})
}

fn stop(index: usize) -> Value {
    json!({"event_type":"step.stop","index":index})
}

fn collect(events: Vec<Value>) -> (Vec<LlmEvent>, ReplyOutcome, AttemptReport) {
    let mut parser = InteractionsSseParser::new();
    let mut delivery = ReplyDelivery::new(Some(origin()));
    let mut live = Vec::new();
    for value in events {
        for event in parser.parse_chunk(value["event_type"].as_str(), &value.to_string()) {
            if let Some(event) = delivery.accept(event) {
                if let LlmEvent::Finished { outcome, report } = event {
                    return (live, outcome, report);
                }
                live.push(event);
            }
        }
    }
    let LlmEvent::Finished { outcome, report } = delivery
        .accept(ProtocolEvent::Failed(StreamStop::UnexpectedEof))
        .unwrap()
    else {
        panic!("finished")
    };
    (live, outcome, report)
}

fn official_events() -> Vec<Value> {
    include_str!("../../../tests/fixtures/projection/interactions_stream.sse")
        .trim()
        .split("\n\n")
        .filter_map(|frame| {
            let data = frame.lines().find_map(|line| line.strip_prefix("data: "))?;
            if data == "[DONE]" {
                return None;
            }
            let name = frame.lines().find_map(|line| line.strip_prefix("event: "));
            let value: Value = serde_json::from_str(data).unwrap();
            assert_eq!(name, value["event_type"].as_str());
            Some(value)
        })
        .collect()
}

#[test]
fn named_official_stream_replays_steps_and_preserves_cumulative_usage() {
    let events = official_events();
    let (live, outcome, report) = collect(events.clone());
    let ReplyOutcome::Complete { message } = outcome else {
        panic!("complete: {outcome:?}")
    };
    let expected: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/projection/interactions_output.json"
    ))
    .unwrap();
    assert_eq!(
        crate::protocol::projection::native_to_wire(&origin(), message.as_message()).unwrap(),
        expected["steps"]
    );
    assert_eq!(report.finish_reason.as_deref(), Some("tool_calls"));
    let expected_usage = Some(TokenUsage {
        input: Some(12),
        cache_read: Some(4),
        cache_write: None,
        output: Some(8),
        reasoning: Some(3),
    });
    assert_eq!(report.usage, expected_usage);
    assert_eq!(live.len(), 3);
    let usage_stop = events
        .iter()
        .position(|event| event["event_type"] == "step.stop" && event.get("usage").is_some())
        .unwrap();
    let (_, interrupted, eof_report) = collect(events[..=usage_stop].to_vec());
    assert_eq!(
        interrupted,
        ReplyOutcome::Interrupted {
            reason: ReplyStop::Stream(StreamStop::UnexpectedEof)
        }
    );
    assert_eq!(eof_report.usage, expected_usage);
    assert_eq!(eof_report.finish_reason, None);
    let mut parser = InteractionsSseParser::new();
    assert!(parser.parse_chunk(Some("done"), "[DONE]").is_empty());
    let unknown = events
        .iter()
        .find(|event| event["event_type"] == "keepalive")
        .unwrap();
    assert!(
        parser
            .parse_chunk(Some("different-name"), &unknown.to_string())
            .is_empty()
    );
    for (created_has_id, completed_has_id) in
        [(true, true), (true, false), (false, true), (false, false)]
    {
        let mut variant = events.clone();
        if created_has_id {
            variant[0]["interaction"]["id"] = "fixture_i".into();
            variant[0]["interaction"]["status"] = "in_progress".into();
        }
        if completed_has_id {
            variant.last_mut().unwrap()["interaction"]["id"] = "fixture_i".into();
        }
        variant.last_mut().unwrap()["interaction"]["steps"] = expected["steps"].clone();
        variant.last_mut().unwrap()["interaction"]["steps"][0]["content"] =
            json!("snapshot-is-not-delivered");
        assert_eq!(
            collect(variant),
            (
                live.clone(),
                ReplyOutcome::Complete {
                    message: message.clone()
                },
                report.clone()
            )
        );
    }
}

#[test]
fn text_and_reasoning_resumed_after_other_parts_keep_arrival_order() {
    let (live, outcome, _) = collect(vec![
        start(0, json!({"type":"model_output"})),
        delta(0, json!({"type":"text","text":"a"})),
        start(1, json!({"type":"thought"})),
        delta(
            1,
            json!({"type":"thought_summary","content":{"type":"text","text":"r"}}),
        ),
        delta(0, json!({"type":"text","text":"b"})),
        delta(
            1,
            json!({"type":"thought_summary","content":{"type":"text","text":"s"}}),
        ),
        delta(
            1,
            json!({"type":"thought_signature","signature":"final-signature"}),
        ),
        stop(0),
        stop(1),
        completed("completed"),
    ]);
    assert_eq!(live.len(), 4);
    let ReplyOutcome::Complete { message } = outcome else {
        panic!("complete")
    };
    let parts: Vec<_> = message
        .as_message()
        .parts
        .iter()
        .filter_map(|part| match &part.semantic {
            MessageSemantic::Text { text } if !text.is_empty() => Some(("text", text.as_str())),
            MessageSemantic::Reasoning { text } if !text.is_empty() => {
                Some(("reasoning", text.as_str()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        parts,
        [
            ("text", "a"),
            ("reasoning", "r"),
            ("text", "b"),
            ("reasoning", "s")
        ]
    );
    assert_eq!(
        crate::protocol::projection::native_to_wire(&origin(), message.as_message()).unwrap(),
        json!([
            {"type":"model_output","content":[{"type":"text","text":"ab"}]},
            {"type":"thought","signature":"final-signature","summary":[{"type":"text","text":"r"},{"type":"text","text":"s"}]}
        ])
    );
}

#[test]
fn malformed_sequences_and_content_stop_without_payload_diagnostics() {
    let call = json!({"type":"function_call","id":"a","name":"get_weather","arguments":{}});
    let unsupported = |kind: &str| {
        StreamStop::Protocol(crate::ReplyProtocolError::UnsupportedContentBlock {
            protocol: "interactions".into(),
            index: 0,
            block_type: kind.into(),
        })
    };
    let malformed = |detail: &str| {
        StreamStop::Provider(crate::ProviderFailure {
            message: format!("malformed Interactions wire: {detail}"),
            retryable: false,
        })
    };
    for (name, events, expected) in [
        (
            "invalid created status",
            vec![json!({"event_type":"interaction.created","interaction":{"status":"future"}})],
            malformed("invalid interaction status"),
        ),
        (
            "missing status update",
            vec![json!({"event_type":"interaction.status_update"})],
            malformed("invalid interaction status"),
        ),
        (
            "duplicate created without status",
            vec![
                json!({"event_type":"interaction.created","interaction":{"object":"interaction","model":"fixture"}}),
                json!({"event_type":"interaction.created","interaction":{"object":"interaction","model":"fixture"}}),
            ],
            malformed("duplicate interaction created"),
        ),
        (
            "unknown step",
            vec![start(
                0,
                json!({"type":"future-step","content":[{"type":"text","text":"private-payload"}]}),
            )],
            unsupported("future-step"),
        ),
        (
            "unknown delta",
            vec![
                start(0, json!({"type":"model_output"})),
                delta(0, json!({"type":"future-delta","text":"private-payload"})),
            ],
            unsupported("future-delta"),
        ),
        (
            "unknown content",
            vec![
                start(0, json!({"type":"thought"})),
                delta(
                    0,
                    json!({"type":"thought_summary","content":{"type":"image","text":"private-payload"}}),
                ),
            ],
            unsupported("image"),
        ),
        (
            "duplicate start",
            vec![start(0, call.clone()), start(0, call.clone())],
            malformed("duplicate step start"),
        ),
        (
            "missing start",
            vec![stop(0)],
            malformed("step has not begun"),
        ),
        (
            "duplicate stop",
            vec![start(0, call.clone()), stop(0), stop(0)],
            malformed("step already stopped"),
        ),
        (
            "delta after stop",
            vec![
                start(0, call.clone()),
                stop(0),
                delta(
                    0,
                    json!({"type":"arguments_delta","arguments":"private-payload"}),
                ),
            ],
            malformed("step already stopped"),
        ),
        (
            "wrong kind",
            vec![
                start(0, call.clone()),
                delta(0, json!({"type":"text","text":"private-payload"})),
            ],
            malformed("delta does not match step type"),
        ),
        (
            "bad known fields",
            vec![
                start(0, call.clone()),
                delta(
                    0,
                    json!({"type":"arguments_delta","arguments":{"text":"private-payload"}}),
                ),
            ],
            malformed("invalid event fields"),
        ),
        (
            "negative index",
            vec![json!({"event_type":"step.start","index":-1,"step":{"type":"model_output"}})],
            malformed("invalid event fields"),
        ),
        (
            "open step",
            vec![start(0, call.clone()), completed("requires_action")],
            malformed("completed interaction has open steps"),
        ),
        (
            "missing call",
            vec![completed("requires_action")],
            malformed("interaction status does not match function calls"),
        ),
        (
            "unexpected call",
            vec![start(0, call.clone()), stop(0), completed("completed")],
            malformed("interaction status does not match function calls"),
        ),
        (
            "initial arguments conflict",
            vec![
                start(
                    0,
                    json!({"type":"function_call","id":"a","name":"get_weather","arguments":{"x":1}}),
                ),
                delta(0, json!({"type":"arguments_delta","arguments":"{}"})),
            ],
            malformed("arguments delta conflicts with initial arguments"),
        ),
        (
            "id mismatch",
            vec![
                json!({"event_type":"interaction.created","interaction":{"id":"one","status":"in_progress"}}),
                json!({"event_type":"interaction.completed","interaction":{"id":"two","status":"completed"}}),
            ],
            malformed("interaction id mismatch"),
        ),
        (
            "snapshot type mismatch",
            vec![
                start(0, json!({"type":"model_output"})),
                stop(0),
                json!({"event_type":"interaction.completed","interaction":{"status":"completed","steps":[{"type":"thought"}]}}),
            ],
            malformed("completed steps differ from streamed steps"),
        ),
        (
            "snapshot count mismatch",
            vec![
                start(0, json!({"type":"model_output"})),
                stop(0),
                json!({"event_type":"interaction.completed","interaction":{"status":"completed","steps":[{"type":"model_output"},{"type":"model_output"}]}}),
            ],
            malformed("completed steps differ from streamed steps"),
        ),
    ] {
        let mut parser = InteractionsSseParser::new();
        let mut delivery = ReplyDelivery::new(Some(origin()));
        let (bad, prefix) = events.split_last().unwrap();
        for value in prefix {
            for event in parser.parse_chunk(value["event_type"].as_str(), &value.to_string()) {
                assert!(
                    !matches!(delivery.accept(event), Some(LlmEvent::Finished { .. })),
                    "{name}: valid prefix must not stop"
                );
            }
        }
        let output: Vec<_> = parser
            .parse_chunk(bad["event_type"].as_str(), &bad.to_string())
            .into_iter()
            .filter_map(|event| delivery.accept(event))
            .collect();
        let [LlmEvent::Finished { outcome, report }] = output.as_slice() else {
            panic!("{name}: bad event must stop immediately: {output:?}")
        };
        assert_eq!(
            outcome,
            &ReplyOutcome::Interrupted {
                reason: ReplyStop::Stream(expected)
            },
            "{name}"
        );
        assert_eq!(report.finish_reason, None, "{name}");
        assert!(
            !format!("{outcome:?}").contains("private-payload"),
            "{name}"
        );
        if let ReplyOutcome::Interrupted {
            reason: ReplyStop::Stream(StreamStop::Provider(failure)),
        } = outcome
        {
            assert!(!failure.retryable, "{name}");
        }
    }
    let (_, outcome, _) = collect(vec![
        start(0, json!({"type":"model_output"})),
        stop(0),
        json!({"event_type":"interaction.completed","interaction":{"status":"completed","steps":[]}}),
    ]);
    assert!(matches!(outcome, ReplyOutcome::Complete { .. }));
    let mut parser = InteractionsSseParser::new();
    assert_eq!(
        parser
            .parse_chunk(Some("step.stop"), &start(0, call).to_string())
            .as_slice(),
        [ProtocolEvent::Failed(malformed(
            "event name differs from event_type"
        ))]
    );
}

#[test]
fn only_successful_interaction_statuses_complete_and_errors_use_codes() {
    for status in [
        "failed",
        "cancelled",
        "incomplete",
        "requires_continuation",
        "in_progress",
        "future",
    ] {
        let mut terminal = completed(status);
        terminal["interaction"]["usage"] =
            json!({"total_output_tokens":2,"total_thought_tokens":3});
        let mut events = vec![
            json!({"event_type":"interaction.status_update","status":"in_progress","usage":{"total_input_tokens":7}}),
        ];
        if status == "incomplete" {
            events.extend([
                start(0, json!({"type":"model_output"})),
                delta(0, json!({"type":"text","text":"partial reply"})),
            ]);
        }
        events.push(terminal);
        let (_, outcome, report) = collect(events);
        if status == "incomplete" {
            let ReplyOutcome::Complete { message } = outcome else {
                panic!("incomplete must save an open step: {outcome:?}")
            };
            assert_eq!(
                crate::protocol::projection::native_to_wire(&origin(), message.as_message())
                    .unwrap(),
                json!([{"type":"model_output","content":[{"type":"text","text":"partial reply"}]}])
            );
            assert_eq!(report.finish_reason.as_deref(), Some("incomplete"));
        } else {
            match status {
                "cancelled" | "requires_continuation" => assert_eq!(
                    outcome,
                    ReplyOutcome::Interrupted {
                        reason: ReplyStop::Stream(StreamStop::Provider(crate::ProviderFailure {
                            message: format!("Interactions status: {status}"),
                            retryable: false,
                        }))
                    },
                    "{status}"
                ),
                _ => assert!(
                    matches!(outcome, ReplyOutcome::Interrupted { .. }),
                    "{status}"
                ),
            }
            assert_eq!(report.finish_reason, None);
        }
        assert_eq!(report.usage.as_ref().unwrap().input, Some(7));
        assert_eq!(report.usage.as_ref().unwrap().output, Some(5));
    }
    for (code, retry) in [
        ("api_error", true),
        ("rate_limit_exceeded", true),
        ("too_many_requests", true),
        ("service_unavailable", true),
        ("aborted", true),
        ("deadline_exceeded", true),
        ("quota_exceeded", false),
        ("authentication", false),
        ("invalid_request", false),
        ("missing_thought_signature", false),
        ("future", false),
        ("https://example.test/api_error", false),
    ] {
        for failed in [false, true] {
            let error = json!({"code":code,"message":"api_error rate_limit_exceeded"});
            let event = if failed {
                json!({"event_type":"interaction.completed","interaction":{"status":"failed","errors":[error]}})
            } else {
                json!({"event_type":"error","error":error})
            };
            let (_, outcome, _) = collect(vec![event]);
            assert!(
                matches!(outcome,ReplyOutcome::Interrupted { reason:ReplyStop::Stream(StreamStop::Provider(failure)) } if failure.retryable==retry),
                "{code}/{failed}"
            );
        }
    }
}

#[test]
fn empty_parameters_remain_distinct_from_missing_or_malformed_streamed_arguments() {
    for arguments in [None, Some(""), Some("[]"), Some("{"), Some("{\"x\":1}")] {
        let mut events = vec![start(
            0,
            json!({"type":"function_call","id":"a","name":"get_weather","arguments":{}}),
        )];
        if let Some(arguments) = arguments {
            events.push(delta(
                0,
                json!({"type":"arguments_delta","arguments":arguments}),
            ));
        }
        for status in ["requires_action", "incomplete"] {
            let mut input = events.clone();
            if status == "requires_action" {
                input.push(stop(0));
            }
            input.push(completed(status));
            let (_, outcome, report) = collect(input);
            let accepted = matches!(arguments, None | Some("{\"x\":1}"));
            assert_eq!(
                matches!(outcome, ReplyOutcome::Complete { .. }),
                accepted,
                "{status}/{arguments:?}"
            );
            if accepted {
                assert_eq!(
                    report.finish_reason.as_deref(),
                    Some(if status == "incomplete" {
                        "incomplete"
                    } else {
                        "tool_calls"
                    })
                );
            } else {
                assert!(
                    matches!(
                        outcome,
                        ReplyOutcome::Rejected {
                            rejection: lattice_conversation::AttemptRejection::InvalidToolCall { .. }
                        }
                    ),
                    "{status}/{arguments:?}: {outcome:?}"
                );
            }
        }
    }
}

#[tokio::test]
async fn encoded_signature_limit_applies_before_step_stop() {
    use crate::reply_stream::{StreamControl, assemble_reply_stream};
    use futures::{StreamExt, channel::mpsc, stream};
    use lattice_conversation::MAX_OPAQUE_REPLAY_STATE_BYTES;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let overhead = serde_json::to_string(&ReplayState {
        step: 0,
        part: PartKey {
            kind: ContentKind::Empty,
            index: 0,
        },
        anchor: Some(Anchor {
            kind: "thought".into(),
            signature: Some(String::new()),
        }),
    })
    .unwrap()
    .len();
    for (signature, complete) in [
        ("s".repeat(MAX_OPAQUE_REPLAY_STATE_BYTES - overhead), true),
        (
            "s".repeat(MAX_OPAQUE_REPLAY_STATE_BYTES - overhead + 1),
            false,
        ),
        (
            "\"".repeat((MAX_OPAQUE_REPLAY_STATE_BYTES - overhead) / 2 + 1),
            false,
        ),
    ] {
        let consumed = Arc::new(AtomicUsize::new(0));
        let count = consumed.clone();
        let (source, input) = mpsc::unbounded::<Value>();
        let mut parser = InteractionsSseParser::new();
        let input = input.flat_map(move |value| {
            count.fetch_add(1, Ordering::SeqCst);
            stream::iter(parser.parse_chunk(value["event_type"].as_str(), &value.to_string()))
        });
        let mut output =
            assemble_reply_stream(Some(origin()), Box::pin(input), StreamControl::default());
        source
            .unbounded_send(start(0, json!({"type":"thought"})))
            .unwrap();
        source
            .unbounded_send(delta(
                0,
                json!({"type":"thought_signature","signature":signature}),
            ))
            .unwrap();
        assert_eq!(output.next().await, Some(LlmEvent::Started));
        if complete {
            source.unbounded_send(stop(0)).unwrap();
            source.unbounded_send(completed("completed")).unwrap();
        }
        let event = tokio::time::timeout(std::time::Duration::from_secs(5), output.next())
            .await
            .expect("signature must stop without step.stop")
            .expect("one terminal event");
        let LlmEvent::Finished { outcome, report } = event else {
            panic!("signature must finish: {event:?}")
        };
        assert_eq!(matches!(outcome, ReplyOutcome::Complete { .. }), complete);
        if !complete {
            assert!(matches!(
                outcome,
                ReplyOutcome::Interrupted {
                    reason: ReplyStop::Stream(StreamStop::Protocol(
                        crate::ReplyProtocolError::ReplayStateTooLarge { .. }
                    ))
                }
            ));
            assert_eq!(report.finish_reason, None);
            assert_eq!(consumed.load(Ordering::SeqCst), 2);
            source.unbounded_send(stop(0)).unwrap();
            source.unbounded_send(completed("completed")).unwrap();
        }
        assert_eq!(output.next().await, None);
        assert_eq!(
            consumed.load(Ordering::SeqCst),
            if complete { 4 } else { 2 }
        );
    }
}

#[test]
fn request_keeps_tool_schema_and_omits_unselected_controls() {
    let mut request = ChatRequest::new(Vec::new(), crate::ModelTarget::new("test", "model"));
    request.tools.push(crate::ToolDefinition::new(
        "tool",
        "desc",
        json!({"type":"object","additionalProperties":false,"properties":{"x":{"const":"x"}}}),
    ));
    let body = build_request_json(&request, None, Some(true), &origin()).unwrap();
    assert_eq!(
        body["generation_config"],
        json!({"thinking_summaries":"auto"})
    );
    assert_eq!(body["tools"][0]["parameters"], request.tools[0].parameters);
    for field in [
        "temperature",
        "top_p",
        "max_output_tokens",
        "previous_interaction_id",
        "continuation_token",
        "background",
        "prompt_cache_key",
        "cachedContent",
        "thinking_summaries",
    ] {
        assert!(body.get(field).is_none());
    }
}

#[test]
fn replay_rejects_orphan_conflicting_and_nonsequential_group_metadata() {
    use lattice_conversation::OpaqueProviderState;
    let wire: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/projection/interactions_output.json"
    ))
    .unwrap();
    let native = crate::protocol::projection::wire_to_native(&origin(), &wire).unwrap();
    for (ordinal, replace) in [
        (1, json!({"step":1,"part":{"kind":"summary","index":0}})),
        (2, json!({"step":1,"part":{"kind":"summary","index":9}})),
        (
            1,
            json!({"step":1,"part":{"kind":"summary","index":0},"anchor":{"type":"model_output"}}),
        ),
    ] {
        let mut malformed = native.clone();
        malformed.parts[ordinal].replay_state =
            Some(OpaqueProviderState::new(replace.to_string()).unwrap());
        assert!(
            crate::protocol::projection::native_to_wire(&origin(), &malformed).is_err(),
            "{replace}"
        );
    }
}

#[test]
fn full_wire_replies_obey_shared_content_batch_and_total_limits() {
    use crate::protocol::projection::{ProjectionError, wire_to_native};
    use lattice_conversation::{
        LimitKind, MAX_ASSISTANT_CONTENT_BYTES, MAX_REASONING_CONTENT_BYTES,
    };
    for (kind, size, expected) in [
        ("model_output", MAX_ASSISTANT_CONTENT_BYTES, None),
        (
            "model_output",
            MAX_ASSISTANT_CONTENT_BYTES + 1,
            Some(LimitKind::AssistantContent),
        ),
        (
            "thought",
            MAX_REASONING_CONTENT_BYTES + 1,
            Some(LimitKind::ReasoningContent),
        ),
    ] {
        let field = if kind == "thought" {
            "summary"
        } else {
            "content"
        };
        let mut step = json!({"type":kind});
        step[field] = json!([{"type":"text","text":"x".repeat(size)}]);
        let result = wire_to_native(&origin(), &json!({"status":"completed","steps":[step]}));
        if let Some(expected) = expected {
            assert!(
                matches!(result,Err(ProjectionError::OutputLimitExceeded { interruption:lattice_conversation::AttemptInterruption::OutputLimitExceeded { kind, .. }, .. }) if kind==expected)
            );
        } else {
            assert!(result.is_ok());
        }
    }
    let calls = |count: usize, size: usize| {
        (0..count).map(|index| json!({"type":"function_call","id":format!("call_{index}"),"name":"t","arguments":{"a":"x".repeat(size)}})).collect::<Vec<_>>()
    };
    let result = wire_to_native(
        &origin(),
        &json!({"status":"requires_action","steps":calls(33,0)}),
    );
    assert!(matches!(result, Err(ProjectionError::InvalidToolCall(_))));
    let result = wire_to_native(
        &origin(),
        &json!({"status":"requires_action","steps":calls(32,512*1024)}),
    );
    assert!(matches!(
        result,
        Err(ProjectionError::OutputLimitExceeded {
            interruption: lattice_conversation::AttemptInterruption::OutputLimitExceeded {
                kind: LimitKind::ModelAttemptTotal,
                ..
            },
            ..
        })
    ));
}

#[tokio::test]
async fn eof_done_marker_and_control_paths_deliver_one_terminal_with_usage() {
    use crate::reply_stream::{StreamControl, assemble_reply_stream};
    use futures::{StreamExt, stream};
    use std::sync::{Arc, atomic::AtomicBool};
    for mode in ["eof", "done", "cancel", "deadline"] {
        let mut parser = InteractionsSseParser::new();
        let mut input = parser.parse_chunk(None,&json!({"event_type":"interaction.status_update","status":"requires_action","usage":{"total_input_tokens":8}}).to_string());
        if mode == "done" {
            input.extend(parser.parse_chunk(Some("done"), "[DONE]"));
        }
        let input: crate::reply_stream::ProtocolStream = if mode == "cancel" || mode == "deadline" {
            Box::pin(stream::iter(input).chain(stream::pending()))
        } else {
            Box::pin(stream::iter(input))
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let control = StreamControl {
            cancel: (mode == "cancel").then(|| cancel.clone()),
            deadline: (mode == "deadline")
                .then(|| tokio::time::Instant::now() + std::time::Duration::from_millis(20)),
        };
        let mut stream = assemble_reply_stream(Some(origin()), input, control);
        assert!(matches!(stream.next().await, Some(LlmEvent::Started)));
        if mode == "cancel" {
            // 先让用量进入包装器，再由独立任务取消等待中的流。
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                cancel.store(true, std::sync::atomic::Ordering::Release);
            });
        }
        let events: Vec<_> = stream.collect().await;
        assert_eq!(events.len(), 1);
        let LlmEvent::Finished {
            outcome:
                ReplyOutcome::Interrupted {
                    reason: ReplyStop::Stream(reason),
                },
            report,
        } = &events[0]
        else {
            panic!("{mode}: finished")
        };
        assert_eq!(report.usage.as_ref().unwrap().input, Some(8));
        assert!(matches!(
            (mode, reason),
            ("cancel", StreamStop::Cancelled)
                | ("deadline", StreamStop::Deadline)
                | ("eof" | "done", StreamStop::UnexpectedEof)
        ));
    }
}

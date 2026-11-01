// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::reply_stream::ReplyDelivery;
use crate::{AttemptReport, LlmEvent, ReplyOutcome};

fn origin() -> ReplayOrigin {
    ReplayOrigin {
        protocol: Protocol::Responses,
        provider_id: "test".into(),
    }
}
fn message(id: &str, phase: &str, text: &str) -> Value {
    json!({"type":"message","id":id,"role":"assistant","status":"completed","phase":phase,"content":[{"type":"output_text","text":text,"annotations":[],"logprobs":[]}]})
}
fn response(output: Vec<Value>) -> Value {
    json!({"type":"response.completed","response":{"status":"completed","output":output}})
}
fn collect(events: Vec<Value>) -> (Vec<LlmEvent>, ReplyOutcome, AttemptReport) {
    let mut parser = ResponsesSseParser::new();
    let mut delivery = ReplyDelivery::new(Some(origin()));
    let mut live = Vec::new();
    for value in events {
        for event in parser.parse_chunk(value["type"].as_str(), &value.to_string()) {
            if let Some(event) = delivery.accept(event) {
                if let LlmEvent::Finished { outcome, report } = event {
                    return (live, outcome, report);
                }
                live.push(event);
            }
        }
    }
    panic!("script must terminate")
}

#[test]
fn request_controls_preserve_session_tools_metadata_and_effort() {
    let mut request = ChatRequest::new(Vec::new(), crate::ModelTarget::new("test", "model"));
    request.client_request_id = Some("session-id".into());
    request.tools.push(crate::ToolDefinition::new(
        "observe__workspace",
        "read",
        json!({"type":"object","properties":{"path":{"type":"string"}}}),
    ));
    for model_reasoning in [Some(false), Some(true), None] {
        for effort in [None, Some(Effort::Off), Some(Effort::High)] {
            let plan = effort.map(|effort| ReasoningPlan::Effort {
                effort,
                enable_toggle: false,
            });
            let body = build_request_json(&request, plan, model_reasoning, &origin()).unwrap();
            assert_eq!(
                body.get("include").is_some(),
                model_reasoning != Some(false)
            );
            assert_eq!(body["prompt_cache_key"], "session-id");
            assert_eq!(body["stream"], true);
            assert_eq!(body["store"], false);
            assert_eq!(
                body["tools"][0],
                json!({"type":"function","name":"observe__workspace","description":"read","parameters":request.tools[0].parameters,"strict":false})
            );
            assert_eq!(body.get("reasoning").cloned(), effort.map(|effort| json!({"effort":if effort==Effort::Off {"none"} else {effort.as_str()}})));
            for key in [
                "summary",
                "temperature",
                "top_p",
                "max_output_tokens",
                "previous_response_id",
                "background",
            ] {
                assert!(body.get(key).is_none());
            }
        }
    }
}

#[test]
fn official_output_round_trips_item_boundaries_phase_cipher_and_call_ids() {
    let wire: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/projection/responses_output.json"
    ))
    .unwrap();
    let native = crate::protocol::projection::wire_to_native(&origin(), &wire).unwrap();
    assert_eq!(native.parts.len(), 5);
    assert_eq!(
        native
            .tool_calls()
            .map(|(_, call)| call.provider_call_id.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("call_a"), Some("call_b")]
    );
    assert_eq!(
        crate::protocol::projection::native_to_wire(&origin(), &native).unwrap(),
        wire["output"]
    );
    let target = ReplayOrigin {
        protocol: Protocol::Responses,
        provider_id: "another-provider".into(),
    };
    let projected = crate::protocol::projection::project_cross_provider(&native, &target);
    let plain = crate::protocol::projection::native_to_wire(&target, &projected).unwrap();
    for item in plain
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["type"] == "message")
    {
        assert_eq!(item["content"][0]["type"], "input_text");
        assert!(item.get("id").is_none() && item.get("phase").is_none());
    }
    let (live, outcome, _) = collect(vec![json!({"type":"response.completed","response":wire})]);
    let ReplyOutcome::Complete { message } = outcome else {
        panic!("complete")
    };
    assert_eq!(message.as_message(), &native);
    assert_eq!(live.len(), 3);
}

#[test]
fn interleaved_text_and_reasoning_keep_arrival_order_and_ignore_done_snapshots() {
    let text = message("msg", "commentary", "beforeafter");
    let reasoning = json!({"type":"reasoning","id":"rs","summary":[{"type":"summary_text","text":"think"}],"encrypted_content":"final"});
    let events = vec![
        json!({"type":"response.output_item.added","output_index":0,"item":message("msg","commentary","")}),
        json!({"type":"response.content_part.added","output_index":0,"item_id":"msg","content_index":0,"part":{"type":"output_text","text":""}}),
        json!({"type":"response.output_text.delta","output_index":0,"item_id":"msg","content_index":0,"delta":"before"}),
        json!({"type":"response.in_progress","response":{"status":"in_progress","output":[message("msg","commentary","before")]}}),
        json!({"type":"response.output_item.added","output_index":1,"item":{"type":"reasoning","id":"rs","encrypted_content":"unfinished","summary":[]}}),
        json!({"type":"response.reasoning_summary_part.added","output_index":1,"item_id":"rs","summary_index":0,"part":{"type":"summary_text","text":""}}),
        json!({"type":"response.reasoning_summary_text.delta","output_index":1,"item_id":"rs","summary_index":0,"delta":"think"}),
        json!({"type":"response.output_text.delta","output_index":0,"item_id":"msg","content_index":0,"delta":"after"}),
        json!({"type":"response.output_text.done","output_index":0,"item_id":"msg","content_index":0,"text":"beforeafter"}),
        json!({"type":"response.content_part.done","output_index":0,"item_id":"msg","content_index":0,"part":{"type":"output_text","text":"beforeafter"}}),
        json!({"type":"response.output_item.done","output_index":0,"item":text}),
        json!({"type":"response.output_item.done","output_index":1,"item":reasoning}),
        response(vec![text.clone(), reasoning.clone()]),
    ];
    let baseline = collect(events.clone());
    for extra in [
        json!({"type":"keepalive"}),
        json!({"type":"response.future_state","response":"private-payload","delta":{"text":"private-payload"},"output_index":"private-payload"}),
        json!({"type":"response.future_state.done","item":false,"part":17,"text":["private-payload"]}),
    ] {
        let mut parser = ResponsesSseParser::new();
        assert!(
            parser
                .parse_chunk(extra["type"].as_str(), &extra.to_string())
                .is_empty()
        );
        let mut with_extra = events.clone();
        with_extra.insert(3, extra);
        assert_eq!(collect(with_extra), baseline);
    }
    let (live, outcome, _) = baseline;
    let ReplyOutcome::Complete { message } = outcome else {
        panic!("complete")
    };
    assert_eq!(
        live,
        vec![
            LlmEvent::Delta(crate::LiveDelta::Text("before".into())),
            LlmEvent::Delta(crate::LiveDelta::Reasoning("think".into())),
            LlmEvent::Delta(crate::LiveDelta::Text("after".into()))
        ]
    );
    assert_eq!(message.as_message().parts.len(), 3);
    assert_eq!(
        crate::protocol::projection::native_to_wire(&origin(), message.as_message()).unwrap(),
        json!([text, reasoning])
    );
}

#[test]
fn reasoning_content_and_summary_use_separate_indices_and_keep_live_order() {
    let item = json!({"type":"reasoning","id":"rs","summary":[{"type":"summary_text","text":"summary"}],"content":[{"type":"reasoning_text","text":"inner"}],"encrypted_content":"final"});
    let (live, outcome, _) = collect(vec![
        json!({"type":"response.output_item.added","output_index":0,"item":{"type":"reasoning","id":"rs","summary":[]}}),
        json!({"type":"response.reasoning_summary_part.added","output_index":0,"item_id":"rs","summary_index":0,"part":{"type":"summary_text","text":""}}),
        json!({"type":"response.reasoning_summary_text.delta","output_index":0,"item_id":"rs","summary_index":0,"delta":"sum"}),
        json!({"type":"response.content_part.added","output_index":0,"item_id":"rs","content_index":0,"part":{"type":"reasoning_text","text":""}}),
        json!({"type":"response.reasoning_text.delta","output_index":0,"item_id":"rs","content_index":0,"delta":"inner"}),
        json!({"type":"response.reasoning_summary_text.delta","output_index":0,"item_id":"rs","summary_index":0,"delta":"mary"}),
        json!({"type":"response.reasoning_text.done","output_index":0,"item_id":"rs","content_index":0,"text":"inner"}),
        json!({"type":"response.content_part.done","output_index":0,"item_id":"rs","content_index":0,"part":{"type":"reasoning_text","text":"inner"}}),
        json!({"type":"response.reasoning_summary_part.done","output_index":0,"item_id":"rs","summary_index":0,"part":{"type":"summary_text","text":"summary"}}),
        json!({"type":"response.output_item.done","output_index":0,"item":item}),
        response(vec![item.clone()]),
    ]);
    let ReplyOutcome::Complete { message } = outcome else {
        panic!("complete")
    };
    assert_eq!(
        live,
        ["sum", "inner", "mary"]
            .map(|text| LlmEvent::Delta(crate::LiveDelta::Reasoning(text.into())))
    );
    assert_eq!(message.as_message().parts.len(), 3);
    assert_eq!(
        crate::protocol::projection::native_to_wire(&origin(), message.as_message()).unwrap(),
        json!([item])
    );
}

#[test]
fn interleaved_arguments_done_snapshots_and_invalid_batch_use_the_common_accumulator() {
    for invalid in [false, true] {
        let mut events = Vec::new();
        let mut output = Vec::new();
        for index in 0..2 {
            let item = json!({"type":"function_call","id":format!("item{index}"),"call_id":format!("call{index}"),"name":"observe__workspace","arguments":""});
            events.push(
                json!({"type":"response.output_item.added","output_index":index,"item":item}),
            );
        }
        for (index, delta) in [
            (0, "{\"a\":"),
            (1, "{\"b\":"),
            (0, "1}"),
            (1, if invalid { "oops" } else { "2}" }),
        ] {
            events.push(json!({"type":"response.function_call_arguments.delta","output_index":index,"item_id":format!("item{index}"),"delta":delta}));
        }
        for index in 0..2 {
            let args = if index == 0 {
                "{\"a\":1}"
            } else if invalid {
                "{\"b\":oops"
            } else {
                "{\"b\":2}"
            };
            let item = json!({"type":"function_call","id":format!("item{index}"),"call_id":format!("call{index}"),"name":"observe__workspace","arguments":args});
            events.push(json!({"type":"response.function_call_arguments.done","output_index":index,"item_id":format!("item{index}"),"arguments":args}));
            events
                .push(json!({"type":"response.output_item.done","output_index":index,"item":item}));
            output.push(item);
        }
        events.push(response(output));
        let (_, outcome, report) = collect(events);
        if invalid {
            assert!(matches!(outcome, ReplyOutcome::Rejected { .. }));
        } else {
            let ReplyOutcome::Complete { message } = outcome else {
                panic!("complete")
            };
            assert_eq!(
                message
                    .as_message()
                    .tool_calls()
                    .map(|(_, call)| call.arguments.clone())
                    .collect::<Vec<_>>(),
                vec![json!({"a":1}), json!({"b":2})]
            );
            assert_eq!(report.finish_reason.as_deref(), Some("tool_calls"));
        }
    }
}

#[test]
fn empty_reasoning_and_refusal_preserve_their_wire_types() {
    let output = vec![
        json!({"type":"reasoning","id":"rs","summary":[],"encrypted_content":"secret"}),
        json!({"type":"message","id":"msg","role":"assistant","status":"completed","content":[{"type":"refusal","refusal":"no"}]}),
    ];
    let (_, outcome, _) = collect(vec![response(output.clone())]);
    let ReplyOutcome::Complete { message } = outcome else {
        panic!("complete")
    };
    assert!(
        matches!(&message.as_message().parts[0].semantic,lattice_conversation::MessageSemantic::Reasoning {text} if text.is_empty())
    );
    assert_eq!(
        crate::protocol::projection::native_to_wire(&origin(), message.as_message()).unwrap(),
        json!(output)
    );
}

#[test]
fn empty_reasoning_stays_before_calls_and_late_summary_does_not_backfill_it() {
    use lattice_conversation::MessageSemantic;
    for late_summary in [false, true] {
        let mut reasoning =
            json!({"type":"reasoning","id":"rs","summary":[],"encrypted_content":"final"});
        if late_summary {
            reasoning["summary"] = json!([{"type":"summary_text","text":"late"}]);
        }
        let call = json!({"type":"function_call","id":"fc","call_id":"call","name":"observe__workspace","arguments":"{}"});
        let (_, outcome, _) = collect(vec![
            json!({"type":"response.output_item.added","output_index":0,"item":{"type":"reasoning","id":"rs","summary":[]}}),
            json!({"type":"response.output_item.added","output_index":1,"item":call}),
            json!({"type":"response.output_item.done","output_index":0,"item":reasoning}),
            json!({"type":"response.output_item.done","output_index":1,"item":call}),
            response(vec![reasoning.clone(), call.clone()]),
        ]);
        let ReplyOutcome::Complete { message } = outcome else {
            panic!("complete")
        };
        let parts = &message.as_message().parts;
        assert!(
            matches!(&parts[0].semantic, MessageSemantic::Reasoning { text } if text.is_empty())
        );
        assert!(matches!(
            &parts[1].semantic,
            MessageSemantic::ToolCall { .. }
        ));
        assert_eq!(parts.len(), if late_summary { 3 } else { 2 });
        if late_summary {
            assert!(
                matches!(&parts[2].semantic, MessageSemantic::Reasoning { text } if text == "late")
            );
        }
        assert_eq!(
            crate::protocol::projection::native_to_wire(&origin(), message.as_message()).unwrap(),
            json!([reasoning, call])
        );
    }
}

#[test]
fn failures_keep_usage_and_do_not_turn_incomplete_into_length() {
    for (kind, detail) in [
        (
            "response.incomplete",
            json!({"incomplete_details":{"reason":"max_output_tokens"}}),
        ),
        (
            "response.failed",
            json!({"error":{"code":"insufficient_quota","message":"no credit"}}),
        ),
    ] {
        let mut failed = json!({"status":"failed","usage":{"input_tokens":11,"output_tokens":3}});
        failed
            .as_object_mut()
            .unwrap()
            .extend(detail.as_object().unwrap().clone());
        let (_, outcome, report) = collect(vec![json!({"type":kind,"response":failed})]);
        assert!(matches!(outcome, ReplyOutcome::Interrupted { .. }));
        assert_eq!(report.usage.unwrap().input, Some(11));
        assert_eq!(report.finish_reason, None);
    }
    for (code, retryable) in [
        ("server_error", true),
        ("rate_limit_exceeded", true),
        ("insufficient_quota", false),
        ("context_length_exceeded", false),
        ("invalid_request_error", false),
        ("unknown", false),
    ] {
        let (_, outcome, _) = collect(vec![
            json!({"type":"error","code":code,"message":"server_error retry me"}),
        ]);
        assert!(
            matches!(outcome,ReplyOutcome::Interrupted {reason:crate::ReplyStop::Stream(crate::StreamStop::Provider(failure))} if failure.retryable==retryable)
        );
    }
}

#[test]
fn cumulative_usage_replaces_values_and_bad_updates_keep_the_last_snapshot() {
    let usage = json!({"input_tokens":100,"input_tokens_details":{"cached_tokens":60,"cache_write_tokens":10},"output_tokens":20,"output_tokens_details":{"reasoning_tokens":5}});
    let event = |usage| json!({"type":"response.in_progress","response":{"status":"in_progress","output":[],"usage":usage}});
    for invalid in [
        json!(-1),
        json!(1.5),
        json!("bad"),
        serde_json::from_str("18446744073709551616").unwrap(),
    ] {
        let (_, outcome, report) = collect(vec![
            event(usage.clone()),
            event(usage.clone()),
            event(json!({"input_tokens":0,"output_tokens":0})),
            event(json!({"input_tokens":90,"output_tokens":invalid})),
        ]);
        assert!(matches!(outcome, ReplyOutcome::Interrupted { .. }));
        let usage = report.usage.unwrap();
        assert_eq!(usage.input, Some(0));
        assert_eq!(usage.output, Some(0));
        assert_eq!(usage.cache_read, Some(60));
        assert_eq!(usage.cache_write, Some(10));
        assert_eq!(usage.reasoning, Some(5));
    }
}

#[test]
fn responses_full_wire_uses_shared_body_reasoning_and_total_limits() {
    use crate::protocol::projection::wire_to_native;
    use lattice_conversation::{AttemptInterruption, LimitKind};
    for (output,kind) in [
        (vec![message("msg","final_answer",&"a".repeat(lattice_conversation::MAX_ASSISTANT_CONTENT_BYTES+1))],LimitKind::AssistantContent),
        (vec![json!({"type":"reasoning","id":"rs","summary":[{"type":"summary_text","text":"r".repeat(lattice_conversation::MAX_REASONING_CONTENT_BYTES+1)}]})],LimitKind::ReasoningContent),
        ((0..30).map(|index| json!({"type":"function_call","id":format!("item{index}"),"call_id":format!("call{index}"),"name":"tool","arguments":format!("{{\"a\":\"{}\"}}","a".repeat(600*1024))})).collect(),LimitKind::ModelAttemptTotal),
    ] {
        let wire=json!({"status":"completed","output":output});
        assert!(matches!(wire_to_native(&origin(),&wire),Err(ProjectionError::OutputLimitExceeded {interruption:AttemptInterruption::OutputLimitExceeded {kind:actual,..},..}) if actual==kind));
    }
}

#[test]
fn unknown_content_and_invalid_sequences_fail_without_exposing_payload() {
    use crate::reply::{ReplyProtocolError, ReplyStop, StreamStop};
    let unsupported = |kind: &str| {
        StreamStop::Protocol(ReplyProtocolError::UnsupportedContentBlock {
            protocol: "responses".into(),
            index: 0,
            block_type: kind.into(),
        })
    };
    let invalid = |detail: &str| {
        StreamStop::Provider(crate::ProviderFailure {
            message: format!("malformed Responses wire: {detail}"),
            retryable: false,
        })
    };
    let begin = json!({"type":"response.output_item.added","output_index":0,"item":{"type":"message","id":"msg","status":"in_progress","role":"assistant","content":[]}});
    let content = json!({"type":"response.content_part.added","output_index":0,"item_id":"msg","content_index":0,"part":{"type":"output_text","text":"","annotations":[],"logprobs":[]}});
    let mut cases = vec![
        (
            vec![json!({"type":"new.delta","delta":"private-payload"})],
            unsupported("new.delta"),
        ),
        (
            vec![
                json!({"type":"response.output_item.added","output_index":0,"item":{"type":"image_generation_call","id":"ig_0","status":"in_progress","result":"private-payload"}}),
            ],
            unsupported("image_generation_call"),
        ),
        (
            vec![
                begin.clone(),
                json!({"type":"response.content_part.added","output_index":0,"item_id":"msg","content_index":0,"part":{"type":"output_image","text":"private-payload"}}),
            ],
            unsupported("output_image"),
        ),
        (
            vec![
                begin.clone(),
                content.clone(),
                json!({"type":"response.output_text.delta","output_index":0,"item_id":"msg","content_index":0,"delta":{"text":"private-payload"}}),
            ],
            invalid("invalid event fields"),
        ),
        (
            vec![
                json!({"type":"response.output_text.delta","output_index":0,"item_id":"missing","content_index":0,"delta":"private-payload"}),
            ],
            invalid("item has not begun"),
        ),
        (
            vec![
                json!({"type":"response.completed","response":{"status":"incomplete","output":[]}}),
            ],
            invalid("response status is not completed"),
        ),
        (
            vec![
                json!({"type":"response.in_progress","response":{"status":"in_progress","output":[message("msg","commentary","private-payload")]}}),
            ],
            invalid("snapshot contains unbegun item"),
        ),
        (
            vec![
                begin.clone(),
                content.clone(),
                json!({"type":"response.output_item.done","output_index":0,"item":{"type":"message","id":"msg","status":"completed","role":"assistant","content":[]}}),
            ],
            invalid("final item omits content"),
        ),
        (
            vec![
                json!({"type":"response.output_item.added","output_index":1,"item":{"type":"message","id":"msg","status":"in_progress","role":"assistant","content":[]}}),
                response(vec![]),
            ],
            invalid("response omits output item"),
        ),
    ];
    for item_id in [Some("another-item"), None] {
        let mut delta = json!({"type":"response.output_text.delta","output_index":0,"content_index":0,"delta":"private-payload"});
        if let Some(id) = item_id {
            delta["item_id"] = id.into();
        }
        cases.push((
            vec![begin.clone(), content.clone(), delta],
            invalid("item id mismatch"),
        ));
    }
    for (events, expected) in cases {
        let (_, outcome, _) = collect(events);
        assert_eq!(
            outcome,
            ReplyOutcome::Interrupted {
                reason: ReplyStop::Stream(expected)
            }
        );
        assert!(!format!("{outcome:?}").contains("private-payload"));
    }
    let mut parser = ResponsesSseParser::new();
    assert!(parser.parse_chunk(None, "[DONE]").is_empty());
}

#[test]
fn encoded_replay_state_limits_include_json_escaping_and_envelope() {
    let empty = ReplayState {
        item: 0,
        part: PartKey {
            kind: ContentKind::Empty,
            index: 0,
        },
        anchor: Some(Anchor {
            kind: "reasoning".into(),
            id: Some("rs".into()),
            phase: None,
            encrypted_content: Some(String::new()),
        }),
    };
    let room = lattice_conversation::MAX_OPAQUE_REPLAY_STATE_BYTES
        - serde_json::to_string(&empty).unwrap().len();
    for (cipher, accepted) in [
        ("a".repeat(room), true),
        ("a".repeat(room + 1), false),
        ("\n".repeat(room), false),
    ] {
        let (_, outcome, _) = collect(vec![response(vec![
            json!({"type":"reasoning","id":"rs","summary":[],"encrypted_content":cipher}),
        ])]);
        assert_eq!(matches!(outcome, ReplyOutcome::Complete { .. }), accepted);
    }
}

#[tokio::test]
async fn item_argument_and_done_markers_require_response_completed() {
    use crate::reply::{ReplyStop, StreamStop};
    use crate::reply_stream::{StreamControl, assemble_reply_stream};
    use futures::{StreamExt, stream};
    let call = json!({"type":"function_call","id":"fc","call_id":"call","name":"observe__workspace","arguments":"{}"});
    for tail in [
        json!({"type":"response.function_call_arguments.done","output_index":0,"item_id":"fc","arguments":"{}"}).to_string(),
        json!({"type":"response.output_item.done","output_index":0,"item":call}).to_string(),
        "[DONE]".into(),
        response(vec![call.clone()]).to_string(),
    ] {
        let complete = tail.contains("response.completed");
        let mut parser = ResponsesSseParser::new();
        let mut input = parser.parse_chunk(None, &json!({"type":"response.created","response":{"status":"in_progress","output":[],"usage":{"input_tokens":8}}}).to_string());
        input.extend(parser.parse_chunk(None, &json!({"type":"response.output_item.added","output_index":0,"item":call}).to_string()));
        input.extend(parser.parse_chunk(None, &tail));
        if complete {
            input.extend(parser.parse_chunk(None, &response(vec![call.clone()]).to_string()));
        }
        let events = assemble_reply_stream(Some(origin()), Box::pin(stream::iter(input)), StreamControl::default()).collect::<Vec<_>>().await;
        assert_eq!(events.iter().filter(|event| matches!(event, LlmEvent::Finished { .. })).count(), 1);
        let Some(LlmEvent::Finished { outcome, report }) = events.last() else {
            panic!("finished")
        };
        assert_eq!(report.usage.as_ref().unwrap().input, Some(8));
        if complete {
            assert!(matches!(outcome, ReplyOutcome::Complete { .. }));
        } else {
            assert!(matches!(outcome, ReplyOutcome::Interrupted { reason: ReplyStop::Stream(StreamStop::UnexpectedEof) }));
            assert_eq!(report.finish_reason, None);
        }
    }
}

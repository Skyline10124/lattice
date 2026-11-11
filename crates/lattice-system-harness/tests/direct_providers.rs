// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 已实现协议的工具往返、缓存请求与尝试计量。
//!
//! 场景经统一入口配置 endpoint、模型与凭据，fake provider 按请求协议回复流。
//! 工具用例通过 `LATTICE_TEST_TOOLS` 注册确定性假工具，校验工具定义投影、调用与结果往返。

mod support;

#[cfg(feature = "fake-tools")]
use lattice_system_harness::{AssertError, CapturedRequest, ScenarioEvidence};
use lattice_system_harness::{
    FakeLlmServer, ProviderFixture, ProviderProtocol, ProviderTarget, ScenarioSpec, ScenarioWorld,
    SendExecution, SessionSendDriver,
};
use support::{assert_clean_send, driver, sequence};
#[cfg(feature = "fake-tools")]
use support::{finish_scenario, require, tool_call, tool_ok};

fn responses_fixture() -> ProviderFixture<'static> {
    ProviderFixture {
        protocol: ProviderProtocol::Responses,
        target: ProviderTarget::Custom("responses-fixture"),
        model: "responses-model",
    }
}

fn interactions_fixture() -> ProviderFixture<'static> {
    ProviderFixture {
        protocol: ProviderProtocol::Interactions,
        target: ProviderTarget::Custom("interactions-fixture"),
        model: "interactions-model",
    }
}

fn interactions_native(
    steps: Vec<serde_json::Value>,
    status: &str,
) -> lattice_system_harness::SseScript {
    use serde_json::json;
    let mut events = vec![
        json!({"event_type":"interaction.created","interaction":{"object":"interaction","model":"interactions-model"}}),
        json!({"event_type":"interaction.status_update","status":"in_progress"}),
    ];
    for (index, step) in steps.iter().enumerate() {
        let mut initial = step.clone();
        match step["type"].as_str().unwrap() {
            "model_output" => {
                initial.as_object_mut().unwrap().remove("content");
            }
            "thought" => {
                initial.as_object_mut().unwrap().remove("summary");
                initial.as_object_mut().unwrap().remove("signature");
            }
            "function_call" => initial["arguments"] = json!({}),
            _ => unreachable!(),
        }
        events.push(json!({"event_type":"step.start","index":index,"step":initial}));
        match step["type"].as_str().unwrap() {
            "model_output" => {
                for content in step["content"].as_array().unwrap() {
                    events.push(json!({"event_type":"step.delta","index":index,"delta":{"type":"text","text":content["text"]}}));
                }
            }
            "thought" => {
                for content in step["summary"].as_array().into_iter().flatten() {
                    events.push(json!({"event_type":"step.delta","index":index,"delta":{"type":"thought_summary","content":content}}));
                }
                events.push(json!({"event_type":"step.delta","index":index,"delta":{"type":"thought_signature","signature":step["signature"]}}));
            }
            "function_call" => events.push(json!({"event_type":"step.delta","index":index,"delta":{"type":"arguments_delta","arguments":step["arguments"].to_string()}})),
            _ => unreachable!(),
        }
        events.push(json!({"event_type":"step.stop","index":index}));
    }
    events.push(json!({"event_type":"keepalive","interaction":false,"delta":{"type":"future"}}));
    events.push(json!({"event_type":"interaction.completed","interaction":{"status":status,"usage":{"total_input_tokens":10,"total_tool_use_tokens":2,"total_cached_tokens":4,"total_output_tokens":5,"total_thought_tokens":3},"created":"2026-11-02T13:51:51Z","updated":"2026-11-02T13:51:51Z","service_tier":"standard","object":"interaction","model":"interactions-model"}}));
    lattice_system_harness::SseScript::NativeEvents {
        events: events
            .into_iter()
            .map(|value| {
                (
                    Some(value["event_type"].as_str().unwrap().to_owned()),
                    value,
                )
            })
            .chain(std::iter::once((Some("done".into()), json!("[DONE]"))))
            .collect(),
    }
}

#[cfg(feature = "fake-tools")]
#[test]
fn interactions_replays_thoughts_and_two_tool_results_after_backend_restart() {
    use lattice_system_harness::{FAKE_API_KEY, SseScript};
    use serde_json::{Value, json};
    let fixture = interactions_fixture();
    let provider = Provider {
        name: "interactions",
        fixture,
        assistant_role: "assistant",
    };
    let (world, spec) = provider_world(&provider);
    world.set_env("LATTICE_TEST_TOOLS", "read_note,read_second");
    let steps = vec![
        json!({"type":"model_output","content":[{"type":"text","text":"Inspecting files."}]}),
        json!({"type":"thought","signature":"fixture-thought-signature","summary":[{"type":"text","text":"Check both."},{"type":"text","text":"Read results."}]}),
        json!({"type":"thought","signature":"fixture-signature-only"}),
        json!({"type":"function_call","id":"call_a","name":"read_note","arguments":{"action":"read","value":{"note":"sentinel-interactions"}}}),
        json!({"type":"function_call","id":"call_b","name":"read_second","arguments":{"action":"read","value":{"note":"second-interactions-sentinel"}}}),
    ];
    let final_step =
        json!({"type":"model_output","content":[{"type":"text","text":"Both files inspected."}]});
    let server = FakeLlmServer::start(sequence(vec![
        interactions_native(steps.clone(), "requires_action"),
        interactions_native(vec![final_step.clone()], "completed"),
        SseScript::plain("Reopened."),
    ]))
    .unwrap();
    world.wire_provider(fixture, &server, FAKE_API_KEY).unwrap();
    let path = world.home().join(".lattice/providers.toml");
    let source = std::fs::read_to_string(&path).unwrap();
    std::fs::write(path,format!("{source}\n[providers.interactions-fixture.models.interactions-model]\nreasoning=true\n[providers.interactions-fixture.models.interactions-model.cost]\ninput=2\noutput=10\ncache_read=0.5\ncache_write=3\n")).unwrap();
    let driver = driver();
    let send = SessionSendDriver::new(&driver, &world, &spec, "interactions");
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let first = send.send("read both files").unwrap();
        evidence.record_command("Interactions tool turn", &first.command);
        assert!(first.has_completed() && first.command.is_clean_exit(0));
        assert_eq!(first.slots().len(), 2);
        assert!(
            first
                .slots()
                .iter()
                .all(|slot| slot.outcome == lattice_system_harness::SlotOutcome::Succeeded)
        );
        assert!(!first.reply_text().contains("fixture-thought-signature"));
        assert!(!first.reply_text().contains("fixture-signature-only"));
        let sid = send.session_id().unwrap();
        let detail = send
            .run_command(&[
                "--json",
                "session",
                "turn",
                first.accepted_turn_id().unwrap(),
            ])
            .unwrap();
        evidence.record_command("Interactions saved turn", &detail);
        let turn: Value = serde_json::from_slice(&detail.stdout).unwrap();
        for step in turn["steps"].as_array().unwrap() {
            let attempt = &step["attempts"][0];
            assert_eq!(
                attempt["origin"],
                json!({"protocol":"interactions","provider_id":"interactions-fixture"})
            );
            assert_eq!(
                attempt["usage"],
                json!({"input":12,"cache_read":4,"cache_write":null,"output":8,"reasoning":3})
            );
            assert_eq!(attempt["usage_kind"], "partial");
            assert_eq!(attempt["cost"]["exact"], false);
        }
        let captures = server.captured_requests();
        assert_eq!(captures.len(), 2);
        let input = captures[1].raw["input"].as_array().unwrap();
        assert_eq!(&input[1..6], &steps);
        for (index, id, tool_name, sentinel) in [
            (6, "call_a", "read_note", "sentinel-interactions"),
            (7, "call_b", "read_second", "second-interactions-sentinel"),
        ] {
            assert_eq!(
                input[index],
                json!({
                    "type": "function_result",
                    "call_id": id,
                    "name": tool_name,
                    "result": format!("{{\"note\":\"{sentinel}\"}}"),
                })
            );
        }
        let expected_tools = json!([
            {
                "type": "function",
                "name": "read_note",
                "description": "测试假工具 read_note",
                "parameters": {"type": "object", "additionalProperties": true},
            },
            {
                "type": "function",
                "name": "read_second",
                "description": "测试假工具 read_second",
                "parameters": {"type": "object", "additionalProperties": true},
            },
        ]);
        for capture in &captures {
            assert_eq!(capture.provider, "interactions");
            assert_eq!(
                capture.raw["system_instruction"],
                "You are a concise test agent."
            );
            assert_eq!(capture.raw["store"], false);
            assert_eq!(capture.raw["stream"], true);
            assert_eq!(
                capture.raw["generation_config"],
                json!({"thinking_summaries":"auto"})
            );
            assert_eq!(capture.raw["tools"], expected_tools);
            for key in [
                "max_output_tokens",
                "previous_interaction_id",
                "background",
                "temperature",
                "top_p",
                "prompt_cache_key",
                "cachedContent",
                "thinking_summaries",
            ] {
                assert!(capture.raw.get(key).is_none(), "{key}");
            }
        }
        let stopped = send.run_command(&["daemon", "stop"]).unwrap();
        evidence.record_command("stop before Interactions replay", &stopped);
        assert!(stopped.is_clean_exit(0));
        let second = send.send("continue after restart").unwrap();
        evidence.record_command("Interactions restarted append", &second.command);
        assert!(second.has_completed() && second.command.is_clean_exit(0));
        let captures = server.captured_requests();
        assert_eq!(captures.len(), 3);
        let reopened = captures[2].raw["input"].as_array().unwrap();
        assert_eq!(&reopened[..input.len()], input);
        assert_eq!(reopened[8], final_step);
        assert_eq!(reopened[9]["content"][0]["text"], "continue after restart");
        let mut watcher = support::Watcher::open(&world, spec.step_timeout(), sid);
        let view = watcher.initial();
        assert_eq!(
            view.head.as_ref().unwrap().provider.as_deref(),
            Some("interactions-fixture")
        );
        let user = *view
            .entries
            .iter()
            .find(|(_, entry)| entry.user_ordinal() == Some(2))
            .unwrap()
            .0;
        let history = support::history(&world, spec.step_timeout(), sid, user, 100).unwrap();
        assert_eq!(
            history,
            view.entries
                .range(..user)
                .map(|(_, entry)| entry.raw.clone())
                .collect::<Vec<_>>()
        );
        let usage = send
            .run_command(&["--json", "session", "usage", sid])
            .unwrap();
        evidence.record_command("Interactions usage after append", &usage);
        let usage: Value = serde_json::from_slice(&usage.stdout).unwrap();
        assert_eq!(usage["path_total"]["attempts"], 3);
    }));
    evidence.record_captures(server.captured_requests());
    support::finish_recorded(evidence, result);
}

#[test]
fn interactions_failures_preserve_usage_and_incomplete_saves_reply() {
    use lattice_system_harness::{FAKE_API_KEY, SseScript};
    use serde_json::{Value, json};
    let spec = ScenarioSpec::direct_local("interactions-failed-and-incomplete");
    let world = ScenarioWorld::new(&spec.name).unwrap();
    world.budget(&spec);
    let fixture = interactions_fixture();
    world.write_project_file(".lattice/agents/interactions/agent.toml",&format!("schema_version=1\n[agent]\nname='interactions'\nmodel='{}'\n[prompt]\nsystem='test'\n[llm.retry]\nmax_retries=1\ninitial_delay_ms=1\nmax_delay_ms=1\n",fixture.model_ref().unwrap())).unwrap();
    world.init_git_project().unwrap();
    let values = vec![
        json!({"event_type":"interaction.created","interaction":{"object":"interaction","model":"interactions-model"}}),
        json!({"event_type":"step.start","index":0,"step":{"type":"model_output"}}),
        json!({"event_type":"step.delta","index":0,"delta":{"type":"text","text":"partial"},"metadata":{"total_usage":{"total_input_tokens":8,"total_tool_use_tokens":2}}}),
        json!({"event_type":"step.stop","index":0}),
        json!({"event_type":"step.start","index":1,"step":{"type":"function_call","id":"call","name":"read_note","arguments":{}}}),
        json!({"event_type":"step.delta","index":1,"delta":{"type":"arguments_delta","arguments":"{\"action\":\"read\"}"}}),
        json!({"event_type":"step.stop","index":1}),
        json!({"event_type":"interaction.status_update","status":"requires_action"}),
        json!({"event_type":"interaction.completed","interaction":{"status":"failed","errors":[{"code":"invalid_request","message":"fixture-failed"}],"usage":{"total_output_tokens":2,"total_thought_tokens":3}}}),
    ];
    let incomplete = vec![
        json!({"event_type":"interaction.created","interaction":{"object":"interaction","model":"interactions-model"}}),
        json!({"event_type":"step.start","index":0,"step":{"type":"model_output"}}),
        json!({"event_type":"step.delta","index":0,"delta":{"type":"text","text":"saved incomplete"}}),
        json!({"event_type":"interaction.completed","interaction":{"status":"incomplete","usage":{"total_input_tokens":9,"total_output_tokens":4}}}),
    ];
    let scripts = [values, incomplete]
        .into_iter()
        .map(|values| SseScript::NativeEvents {
            events: values
                .into_iter()
                .map(|value| {
                    (
                        Some(value["event_type"].as_str().unwrap().to_owned()),
                        value,
                    )
                })
                .chain(std::iter::once((Some("done".into()), json!("[DONE]"))))
                .collect(),
        })
        .collect();
    let (server, gate) =
        FakeLlmServer::start_segmented(sequence(scripts), spec.total_timeout().unwrap()).unwrap();
    world.wire_provider(fixture, &server, FAKE_API_KEY).unwrap();
    let path = world.home().join(".lattice/providers.toml");
    let source = std::fs::read_to_string(&path).unwrap();
    std::fs::write(path,format!("{source}\n[providers.interactions-fixture.models.interactions-model.cost]\ninput=2\noutput=10\n")).unwrap();
    let driver = driver();
    let send = SessionSendDriver::new(&driver, &world, &spec, "interactions");
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut running = send.spawn("stop failed").unwrap();
        for frame in 1..=8 {
            gate.wait_until_stalled(frame, running.remaining_timeout(spec.step_timeout()))
                .unwrap();
            gate.release_next(running.remaining_timeout(spec.step_timeout()))
                .unwrap();
        }
        gate.wait_until_stalled(9, running.remaining_timeout(spec.step_timeout()))
            .unwrap();
        assert!(!running.has_exited().unwrap());
        assert_eq!(server.captured_requests().len(), 1);
        let list = send.run_command(&["--json", "session", "list"]).unwrap();
        evidence.record_command("Interactions in-flight session", &list);
        assert!(list.is_clean_exit(0));
        let sessions: Value = serde_json::from_slice(&list.stdout).unwrap();
        let [session] = sessions.as_array().unwrap().as_slice() else {
            panic!("{sessions}")
        };
        let mut watcher = support::Watcher::open(
            &world,
            running.remaining_timeout(spec.step_timeout()),
            session["session_id"].as_str().unwrap(),
        );
        let view = watcher.initial();
        assert!(view.head.as_ref().unwrap().owner);
        let steps: Vec<_> = view
            .entries
            .values()
            .filter_map(|entry| entry.step())
            .collect();
        assert_eq!(steps.len(), 1);
        assert!(steps[0].slots.is_empty());
        drop(watcher);
        gate.release(running.remaining_timeout(spec.step_timeout()))
            .unwrap();
        let sent = send.wait_spawned(running).unwrap();
        evidence.record_command("Interactions failed turn", &sent.command);
        assert!(sent.command.is_clean_exit(1));
        assert!(sent.slots().is_empty());
        let detail = send
            .run_command(&[
                "--json",
                "session",
                "turn",
                sent.accepted_turn_id().unwrap(),
            ])
            .unwrap();
        evidence.record_command("Interactions failed detail", &detail);
        let turn: Value = serde_json::from_slice(&detail.stdout).unwrap();
        let attempt = &turn["steps"][0]["attempts"][0];
        assert_eq!(attempt["usage"]["input"], 10);
        assert_eq!(attempt["usage"]["output"], 5);
        assert_eq!(attempt["usage"]["reasoning"], 3);
        assert_eq!(attempt["usage_kind"], "partial");
        assert_eq!(attempt["cost"]["exact"], false);
        assert_eq!(
            attempt["end"]["cause"],
            json!({"cause":"provider_error","retryable":false})
        );
        assert!(
            attempt["end"]["diagnostic"]
                .as_str()
                .unwrap()
                .contains("fixture-failed")
        );
        assert_eq!(turn["steps"][0]["attempts"].as_array().unwrap().len(), 1);
        assert!(
            sent.entries()
                .iter()
                .filter_map(|entry| entry.step())
                .all(|step| step.reply.is_empty() && step.slots.is_empty())
        );
        assert_eq!(server.captured_requests().len(), 1);
        let saved = send.send("save incomplete reply").unwrap();
        evidence.record_command("Interactions saved incomplete turn", &saved.command);
        assert!(saved.has_completed() && saved.command.is_clean_exit(0));
        assert_eq!(saved.reply_text(), "saved incomplete");
        assert!(saved.slots().is_empty());
        let detail = send
            .run_command(&[
                "--json",
                "session",
                "turn",
                saved.accepted_turn_id().unwrap(),
            ])
            .unwrap();
        evidence.record_command("Interactions saved incomplete detail", &detail);
        let turn: Value = serde_json::from_slice(&detail.stdout).unwrap();
        assert_eq!(turn["steps"].as_array().unwrap().len(), 1);
        let attempts = turn["steps"][0]["attempts"].as_array().unwrap();
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0]["end"]["end"], "completed");
        assert_eq!(attempts[0]["usage"]["input"], 9);
        assert_eq!(attempts[0]["usage"]["output"], 4);
        let stopped = send.run_command(&["daemon", "stop"]).unwrap();
        evidence.record_command("stop after Interactions incomplete", &stopped);
        assert!(stopped.is_clean_exit(0));
        world.daemon_departed().unwrap();
        let reopened = send
            .run_command(&["--json", "session", "log", send.session_id().unwrap()])
            .unwrap();
        evidence.record_command("reopen Interactions incomplete reply", &reopened);
        assert!(reopened.is_clean_exit(0));
        let mut watcher =
            support::Watcher::open(&world, spec.step_timeout(), send.session_id().unwrap());
        let text: Vec<_> = watcher
            .initial()
            .entries
            .values()
            .filter_map(|entry| entry.step())
            .flat_map(|step| &step.reply)
            .map(|segment| segment.text.as_str())
            .collect();
        assert_eq!(text, ["saved incomplete"]);
        assert_eq!(server.captured_requests().len(), 2);
    }));
    evidence.record_captures(server.captured_requests());
    support::finish_recorded(evidence, result);
}

fn responses_native(output: Vec<serde_json::Value>) -> lattice_system_harness::SseScript {
    use serde_json::json;
    let mut response = json!({
        "id":"resp_fixture", "object":"response", "created_at":1741290958,
        "status":"in_progress", "completed_at":null, "background":false,
        "error":null, "incomplete_details":null, "instructions":"You are a concise test agent.",
        "max_output_tokens":null, "model":"responses-model", "output":[],
        "parallel_tool_calls":true, "previous_response_id":null,
        "reasoning":{"effort":null,"summary":null}, "store":false,
        "temperature":1.0, "text":{"format":{"type":"text"}},
        "tool_choice":"auto", "tools":[], "top_p":1.0, "truncation":"disabled",
        "usage":null, "user":null, "metadata":{},
    });
    let mut events = vec![
        json!({"type":"response.created","response":response}),
        json!({"type":"response.in_progress","response":response}),
    ];
    for (index, item) in output.iter().enumerate() {
        let mut start = item.clone();
        match start["type"].as_str().unwrap() {
            "message" => {
                start["content"] = json!([]);
                start["status"] = json!("in_progress");
            }
            "reasoning" => {
                start["summary"] = json!([]);
                start["encrypted_content"] = json!("unfinished");
            }
            "function_call" => start["arguments"] = json!(""),
            _ => unreachable!(),
        }
        events.push(json!({"type":"response.output_item.added","output_index":index,"item":start}));
        if item["type"] == "message" {
            for (content_index, part) in item["content"].as_array().unwrap().iter().enumerate() {
                assert_eq!(part["type"], "output_text");
                let mut empty = part.clone();
                empty["text"] = json!("");
                events.push(json!({"type":"response.content_part.added","output_index":index,"item_id":item["id"],"content_index":content_index,"part":empty}));
                events.push(json!({"type":"response.output_text.delta","output_index":index,"item_id":item["id"],"content_index":content_index,"delta":part["text"],"logprobs":[]}));
                events.push(json!({"type":"response.output_text.done","output_index":index,"item_id":item["id"],"content_index":content_index,"text":part["text"],"logprobs":[]}));
                events.push(json!({"type":"response.content_part.done","output_index":index,"item_id":item["id"],"content_index":content_index,"part":part}));
            }
        } else if item["type"] == "reasoning" {
            for (summary_index, part) in item["summary"].as_array().unwrap().iter().enumerate() {
                let mut empty = part.clone();
                empty["text"] = json!("");
                events.push(json!({"type":"response.reasoning_summary_part.added","output_index":index,"item_id":item["id"],"summary_index":summary_index,"part":empty}));
                events.push(json!({"type":"response.reasoning_summary_text.delta","output_index":index,"item_id":item["id"],"summary_index":summary_index,"delta":part["text"]}));
                events.push(json!({"type":"response.reasoning_summary_text.done","output_index":index,"item_id":item["id"],"summary_index":summary_index,"text":part["text"]}));
                events.push(json!({"type":"response.reasoning_summary_part.done","output_index":index,"item_id":item["id"],"summary_index":summary_index,"part":part}));
            }
        }
        events.push(json!({"type":"response.output_item.done","output_index":index,"item":item}));
    }
    response["status"] = json!("completed");
    response["completed_at"] = json!(1741290960);
    response["output"] = json!(output);
    for item in response["output"].as_array_mut().unwrap() {
        if item["type"] == "reasoning" {
            item["encrypted_content"] = "x"
                .repeat(item["encrypted_content"].as_str().unwrap().len())
                .into();
        }
    }
    response["usage"] = json!({"input_tokens":100,"input_tokens_details":{"cached_tokens":60,"cache_write_tokens":10},"output_tokens":20,"output_tokens_details":{"reasoning_tokens":5}});
    events.push(json!({"type":"response.completed","response":response}));
    lattice_system_harness::SseScript::NativeEvents {
        events: events
            .into_iter()
            .enumerate()
            .map(|(sequence, mut value)| {
                let name = value["type"].as_str().unwrap().to_owned();
                value["sequence_number"] = json!(sequence);
                (Some(name), value)
            })
            .collect(),
    }
}

#[cfg(feature = "fake-tools")]
#[test]
fn responses_replays_phase_reasoning_and_two_tool_results_after_backend_restart() {
    use lattice_system_harness::FAKE_API_KEY;
    use serde_json::{Value, json};
    let fixture = responses_fixture();
    let provider = Provider {
        name: "responses",
        fixture,
        assistant_role: "assistant",
    };
    let (world, spec) = provider_world(&provider);
    world.set_env("LATTICE_TEST_TOOLS", "read_note,read_second");
    let output = vec![
        json!({"type":"message","id":"msg_comment","role":"assistant","status":"completed","phase":"commentary","content":[{"type":"output_text","text":"Inspecting two files.","annotations":[],"logprobs":[]}]}),
        json!({"type":"reasoning","id":"rs","summary":[{"type":"summary_text","text":"Check both inputs."}],"encrypted_content":"fixture-final-cipher"}),
        json!({"type":"function_call","id":"item_a","call_id":"call_a","name":"read_note","arguments":"{\"action\":\"read\",\"value\":{\"note\":\"sentinel-responses\"}}"}),
        json!({"type":"function_call","id":"item_b","call_id":"call_b","name":"read_second","arguments":"{\"action\":\"read\",\"value\":{\"note\":\"second-sentinel\"}}"}),
    ];
    let final_item = json!({"type":"message","id":"msg_final","role":"assistant","status":"completed","phase":"final_answer","content":[{"type":"output_text","text":"Both files inspected.","annotations":[],"logprobs":[]}]});
    let server = FakeLlmServer::start(sequence(vec![
        responses_native(output.clone()),
        responses_native(vec![final_item.clone()]),
        lattice_system_harness::SseScript::plain("Reopened."),
    ]))
    .unwrap();
    world.wire_provider(fixture, &server, FAKE_API_KEY).unwrap();
    let config = world.home().join(".lattice/providers.toml");
    let source = std::fs::read_to_string(&config).unwrap();
    std::fs::write(config,format!("{source}\n[providers.responses-fixture.models.responses-model]\nreasoning=true\n[providers.responses-fixture.models.responses-model.cost]\ninput=2\noutput=10\ncache_read=0.5\ncache_write=3\n")).unwrap();
    let driver = driver();
    let send = SessionSendDriver::new(&driver, &world, &spec, "responses");
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let first = send.send("read both files").unwrap();
        evidence.record_command("Responses tool turn", &first.command);
        assert!(first.has_completed() && first.command.is_clean_exit(0));
        assert_eq!(first.slots().len(), 2);
        assert!(
            first
                .slots()
                .iter()
                .all(|slot| slot.outcome == lattice_system_harness::SlotOutcome::Succeeded)
        );
        assert!(!first.reply_text().contains("fixture-final-cipher"));
        let sid = send.session_id().unwrap();
        let detail = send
            .run_command(&[
                "--json",
                "session",
                "turn",
                first.accepted_turn_id().unwrap(),
            ])
            .unwrap();
        evidence.record_command("Responses saved turn", &detail);
        let turn: Value = serde_json::from_slice(&detail.stdout).unwrap();
        for step in turn["steps"].as_array().unwrap() {
            assert_eq!(
                step["attempts"][0]["origin"],
                json!({"protocol":"responses","provider_id":"responses-fixture"})
            );
            assert_eq!(
                step["attempts"][0]["usage"],
                json!({"input":100,"cache_read":60,"cache_write":10,"output":20,"reasoning":5})
            );
            assert_eq!(step["attempts"][0]["usage_kind"], "complete");
            assert_eq!(step["attempts"][0]["cost"]["exact"], true);
        }
        let captures = server.captured_requests();
        assert_eq!(captures.len(), 2);
        let inputs = captures[1].raw["input"].as_array().unwrap();
        assert_eq!(&inputs[1..5], &output);
        assert_eq!(
            inputs[5],
            json!({
                "type": "function_call_output",
                "call_id": "call_a",
                "output": "{\"note\":\"sentinel-responses\"}",
            })
        );
        assert_eq!(
            inputs[6],
            json!({
                "type": "function_call_output",
                "call_id": "call_b",
                "output": "{\"note\":\"second-sentinel\"}",
            })
        );
        let expected_tools = json!([
            {
                "type": "function",
                "name": "read_note",
                "description": "测试假工具 read_note",
                "parameters": {"type": "object", "additionalProperties": true},
                "strict": false,
            },
            {
                "type": "function",
                "name": "read_second",
                "description": "测试假工具 read_second",
                "parameters": {"type": "object", "additionalProperties": true},
                "strict": false,
            },
        ]);
        for capture in &captures {
            assert_eq!(capture.provider, "responses");
            assert_eq!(capture.raw["instructions"], "You are a concise test agent.");
            assert_eq!(capture.raw["store"], false);
            assert_eq!(capture.raw["stream"], true);
            assert_eq!(capture.raw["prompt_cache_key"], sid);
            assert_eq!(capture.raw["tools"], expected_tools);
            assert_eq!(
                capture.raw["include"],
                json!(["reasoning.encrypted_content"])
            );
            assert_eq!(capture.raw["reasoning"], json!({"summary":"auto"}));
            for key in [
                "max_output_tokens",
                "previous_response_id",
                "background",
                "temperature",
                "top_p",
            ] {
                assert!(capture.raw.get(key).is_none());
            }
        }
        let stopped = send.run_command(&["daemon", "stop"]).unwrap();
        evidence.record_command("stop before replay", &stopped);
        assert!(stopped.is_clean_exit(0));
        let second = send.send("continue after restart").unwrap();
        evidence.record_command("Responses restarted append", &second.command);
        assert!(second.has_completed() && second.command.is_clean_exit(0));
        let captures = server.captured_requests();
        assert_eq!(captures.len(), 3);
        let reopened = captures[2].raw["input"].as_array().unwrap();
        assert_eq!(&reopened[..inputs.len()], inputs);
        assert_eq!(reopened[7], final_item);
        assert_eq!(reopened[8]["content"][0]["text"], "continue after restart");
        assert_eq!(captures[2].raw["prompt_cache_key"], sid);
        assert_eq!(
            captures[2].raw["include"],
            json!(["reasoning.encrypted_content"])
        );
        assert_eq!(captures[2].raw["reasoning"], json!({"summary":"auto"}));
        let mut watcher = support::Watcher::open(&world, spec.step_timeout(), sid);
        let view = watcher.initial();
        assert_eq!(
            view.head.as_ref().unwrap().provider.as_deref(),
            Some("responses-fixture")
        );
        let current_user = *view
            .entries
            .iter()
            .find(|(_, entry)| entry.user_ordinal() == Some(2))
            .unwrap()
            .0;
        let history =
            support::history(&world, spec.step_timeout(), sid, current_user, 100).unwrap();
        assert_eq!(
            history,
            view.entries
                .range(..current_user)
                .map(|(_, entry)| entry.raw.clone())
                .collect::<Vec<_>>()
        );
        assert_eq!(history.len(), 3);
        let usage = send
            .run_command(&["--json", "session", "usage", sid])
            .unwrap();
        evidence.record_command("Responses usage after append", &usage);
        let usage: Value = serde_json::from_slice(&usage.stdout).unwrap();
        assert_eq!(usage["path_total"]["attempts"], 3);
    }));
    evidence.record_captures(server.captured_requests());
    support::finish_recorded(evidence, result);
}

#[test]
fn responses_failures_preserve_usage_and_incomplete_saves_only_valid_replies() {
    use lattice_system_harness::{FAKE_API_KEY, SseScript};
    use serde_json::{Value, json};
    let spec = ScenarioSpec::direct_local("responses-failure-and-incomplete");
    let world = ScenarioWorld::new(&spec.name).unwrap();
    world.budget(&spec);
    let fixture = responses_fixture();
    world.write_project_file(".lattice/agents/responses/agent.toml",&format!("schema_version=1\n[agent]\nname='responses'\nmodel='{}'\n[prompt]\nsystem='test'\n[llm.retry]\nmax_retries=1\ninitial_delay_ms=1\nmax_delay_ms=1\n",fixture.model_ref().unwrap())).unwrap();
    world.init_git_project().unwrap();
    let events = vec![
        json!({"type":"response.created","response":{"status":"in_progress","output":[],"usage":{"input_tokens":8}}}),
        json!({"type":"response.output_item.added","output_index":0,"item":{"type":"message","role":"assistant","id":"msg","content":[]}}),
        json!({"type":"response.content_part.added","output_index":0,"item_id":"msg","content_index":0,"part":{"type":"output_text","text":""}}),
        json!({"type":"response.output_text.delta","output_index":0,"item_id":"msg","content_index":0,"delta":"partial"}),
        json!({"type":"response.output_item.added","output_index":1,"item":{"type":"function_call","id":"item","call_id":"call","name":"read_note","arguments":""}}),
        json!({"type":"response.function_call_arguments.delta","output_index":1,"item_id":"item","delta":"{\"action\":\"read\"}"}),
        json!({"type":"response.failed","response":{"status":"failed","error":{"code":"insufficient_quota","message":"no credit"},"usage":{"output_tokens":3}}}),
    ];
    let native = |events: Vec<Value>| SseScript::NativeEvents {
        events: events
            .into_iter()
            .enumerate()
            .map(|(sequence, mut value)| {
                let name = value["type"].as_str().unwrap().to_owned();
                value["sequence_number"] = json!(sequence);
                (Some(name), value)
            })
            .collect(),
    };
    let (server, gate) = FakeLlmServer::start_segmented(
        sequence(vec![
            native(events),
            native(vec![
                json!({"type":"response.output_item.added","output_index":0,"item":{"type":"message","role":"assistant","id":"saved","status":"in_progress","content":[]}}),
                json!({"type":"response.content_part.added","output_index":0,"item_id":"saved","content_index":0,"part":{"type":"output_text","text":""}}),
                json!({"type":"response.output_text.delta","output_index":0,"item_id":"saved","content_index":0,"delta":"saved incomplete"}),
                json!({"type":"response.output_item.done","output_index":0,"item":{"type":"message","role":"assistant","id":"saved","status":"incomplete","content":[{"type":"output_text","text":"saved incomplete"}]}}),
                json!({"type":"response.incomplete","response":{"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"output":[{"type":"message","role":"assistant","id":"saved","status":"incomplete","content":[{"type":"output_text","text":"saved incomplete"}]}],"usage":{"input_tokens":9,"output_tokens":4}}}),
            ]),
            native(vec![
                json!({"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","id":"truncated","call_id":"truncated-call","name":"read_note","arguments":""}}),
                json!({"type":"response.function_call_arguments.delta","output_index":0,"item_id":"truncated","delta":"{\"action\":"}),
                json!({"type":"response.output_item.done","output_index":0,"item":{"type":"function_call","id":"truncated","call_id":"truncated-call","name":"read_note","status":"incomplete","arguments":"{\"action\":"}}),
                json!({"type":"response.incomplete","response":{"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"}}}),
            ]),
            native(vec![
                json!({"type":"response.completed","response":{"status":"completed","output":[{"type":"function_call","id":"missing-call-id","name":"read_note","arguments":"{\"action\":\"read\"}"}]}}),
            ]),
        ]),
        spec.total_timeout().unwrap(),
    )
    .unwrap();
    world.wire_provider(fixture, &server, FAKE_API_KEY).unwrap();
    let config = world.home().join(".lattice/providers.toml");
    let source = std::fs::read_to_string(&config).unwrap();
    std::fs::write(config,format!("{source}\n[providers.responses-fixture.models.responses-model.cost]\ninput=2\noutput=10\n")).unwrap();
    let driver = driver();
    let send = SessionSendDriver::new(&driver, &world, &spec, "responses");
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut running = send.spawn("stop failed response").unwrap();
        for frame in 1..=6 {
            gate.wait_until_stalled(frame, running.remaining_timeout(spec.step_timeout()))
                .unwrap();
            gate.release_next(running.remaining_timeout(spec.step_timeout()))
                .unwrap();
        }
        gate.wait_until_stalled(7, running.remaining_timeout(spec.step_timeout()))
            .unwrap();
        assert!(!running.has_exited().unwrap());
        assert_eq!(server.captured_requests().len(), 1);
        gate.release(running.remaining_timeout(spec.step_timeout()))
            .unwrap();
        let sent = send.wait_spawned(running).unwrap();
        evidence.record_command("Responses failed Turn", &sent.command);
        assert!(sent.command.is_clean_exit(1));
        assert!(sent.slots().is_empty());
        let detail = send
            .run_command(&[
                "--json",
                "session",
                "turn",
                sent.accepted_turn_id().unwrap(),
            ])
            .unwrap();
        evidence.record_command("Responses failed detail", &detail);
        let turn: Value = serde_json::from_slice(&detail.stdout).unwrap();
        let attempt = &turn["steps"][0]["attempts"][0];
        assert_eq!(attempt["usage"]["input"], 8);
        assert_eq!(attempt["usage"]["output"], 3);
        assert_eq!(attempt["usage_kind"], "partial");
        assert_eq!(attempt["cost"]["exact"], false);
        assert_eq!(
            attempt["end"]["cause"],
            json!({"cause":"provider_error","retryable":false})
        );
        assert!(
            attempt["end"]["diagnostic"]
                .as_str()
                .unwrap()
                .contains("no credit")
        );
        assert_eq!(turn["steps"][0]["attempts"].as_array().unwrap().len(), 1);
        assert!(
            sent.entries()
                .iter()
                .filter_map(|entry| entry.step())
                .all(|step| step.reply.is_empty() && step.slots.is_empty())
        );
        assert_eq!(server.captured_requests().len(), 1);

        let saved = send.send("save incomplete reply").unwrap();
        evidence.record_command("Responses saved incomplete Turn", &saved.command);
        assert!(saved.has_completed() && saved.command.is_clean_exit(0));
        assert_eq!(saved.reply_text(), "saved incomplete");
        assert!(saved.slots().is_empty());
        let detail = send
            .run_command(&[
                "--json",
                "session",
                "turn",
                saved.accepted_turn_id().unwrap(),
            ])
            .unwrap();
        evidence.record_command("Responses saved incomplete detail", &detail);
        let turn: Value = serde_json::from_slice(&detail.stdout).unwrap();
        assert_eq!(turn["steps"].as_array().unwrap().len(), 1);
        assert_eq!(turn["steps"][0]["attempts"].as_array().unwrap().len(), 1);
        assert_eq!(turn["steps"][0]["attempts"][0]["usage"]["input"], 9);
        assert_eq!(turn["steps"][0]["attempts"][0]["usage"]["output"], 4);
        assert_eq!(turn["steps"][0]["attempts"][0]["end"]["end"], "completed");
        assert_eq!(server.captured_requests().len(), 2);

        let stopped = send.run_command(&["daemon", "stop"]).unwrap();
        evidence.record_command("stop after saved incomplete reply", &stopped);
        assert!(stopped.is_clean_exit(0));
        let rejected = send.send("reject truncated arguments").unwrap();
        evidence.record_command("Responses incomplete invalid tool Turn", &rejected.command);
        assert!(rejected.command.is_clean_exit(1));
        assert!(rejected.slots().is_empty());
        assert!(
            rejected
                .entries()
                .iter()
                .filter_map(|entry| entry.step())
                .all(|step| step.reply.is_empty() && step.slots.is_empty())
        );
        let detail = send
            .run_command(&[
                "--json",
                "session",
                "turn",
                rejected.accepted_turn_id().unwrap(),
            ])
            .unwrap();
        evidence.record_command("Responses incomplete invalid tool detail", &detail);
        let turn: Value = serde_json::from_slice(&detail.stdout).unwrap();
        assert_eq!(turn["steps"].as_array().unwrap().len(), 1);
        assert_eq!(turn["steps"][0]["attempts"].as_array().unwrap().len(), 1);
        assert_eq!(
            turn["steps"][0]["attempts"][0]["end"]["cause"],
            json!({"cause":"invalid_batch"})
        );
        let captures = server.captured_requests();
        assert_eq!(captures.len(), 3);
        assert!(
            captures[2].raw["input"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["id"] == "saved"
                    && item["content"][0]["text"] == "saved incomplete")
        );
        let malformed = send.send("reject missing call_id").unwrap();
        evidence.record_command("Responses missing call_id Turn", &malformed.command);
        assert!(malformed.command.is_clean_exit(1));
        assert!(malformed.slots().is_empty());
        let steps = malformed
            .entries()
            .into_iter()
            .filter(|entry| entry.step().is_some())
            .collect::<Vec<_>>();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].raw["content"].get("reply"), Some(&Value::Null));
        let detail = send
            .run_command(&[
                "--json",
                "session",
                "turn",
                malformed.accepted_turn_id().unwrap(),
            ])
            .unwrap();
        evidence.record_command("Responses missing call_id detail", &detail);
        let turn: Value = serde_json::from_slice(&detail.stdout).unwrap();
        assert_eq!(turn["steps"].as_array().unwrap().len(), 1);
        assert_eq!(turn["steps"][0]["attempts"].as_array().unwrap().len(), 1);
        let attempt = &turn["steps"][0]["attempts"][0];
        assert_eq!(
            attempt["end"]["cause"],
            json!({"cause":"provider_error","retryable":false})
        );
        assert_eq!(
            attempt["end"]["diagnostic"],
            "malformed Responses wire: function call missing call_id"
        );
        assert_eq!(server.captured_requests().len(), 4);
    }));
    evidence.record_captures(server.captured_requests());
    support::finish_recorded(evidence, result);
}

#[cfg(feature = "fake-tools")]
struct Provider {
    name: &'static str,
    fixture: ProviderFixture<'static>,
    /// 该协议在 wire 上表示助手轮次的 role。
    assistant_role: &'static str,
}

#[cfg(feature = "fake-tools")]
const PROVIDERS: [Provider; 3] = [
    Provider {
        name: "openai",
        fixture: ProviderFixture {
            protocol: ProviderProtocol::ChatCompletions,
            target: ProviderTarget::Builtin("alibaba"),
            model: "qwen-fixture",
        },
        assistant_role: "assistant",
    },
    Provider {
        name: "anthropic",
        fixture: ProviderFixture {
            protocol: ProviderProtocol::AnthropicMessages,
            target: ProviderTarget::Builtin("anthropic"),
            model: "claude-sonnet-4-6",
        },
        assistant_role: "assistant",
    },
    Provider {
        name: "gemini",
        fixture: ProviderFixture {
            protocol: ProviderProtocol::GenerateContent,
            target: ProviderTarget::Custom("google-generate"),
            model: "gemini-2.5-flash",
        },
        assistant_role: "model",
    },
];

#[cfg(feature = "fake-tools")]
fn provider_world(provider: &Provider) -> (ScenarioWorld, ScenarioSpec) {
    let spec = ScenarioSpec::direct_local(format!("direct-provider-{}", provider.name));
    let world = ScenarioWorld::new(&spec.name).expect("world");
    let model = provider.fixture.model_ref().unwrap();
    let toml = format!(
        "schema_version = 1\n\n\
         [agent]\nname = \"{name}\"\nmodel = \"{model}\"\n\n\
         [prompt]\nsystem = \"You are a concise test agent.\"\n",
        name = provider.name,
    );
    world
        .write_project_file(
            &format!(".lattice/agents/{}/agent.toml", provider.name),
            &toml,
        )
        .expect("write agent.toml");
    world.init_git_project().expect("init git");
    (world, spec)
}

/// 提取第二次请求里按各协议格式投影的工具结果载荷。
#[cfg(feature = "fake-tools")]
fn projected_tool_result(
    provider: &Provider,
    capture: &CapturedRequest,
) -> Result<serde_json::Value, AssertError> {
    let (list, last) = match provider.name {
        "openai" | "anthropic" => ("messages", capture.raw["messages"].as_array()),
        _ => ("contents", capture.raw["contents"].as_array()),
    };
    let last = last
        .and_then(|items| items.last())
        .ok_or_else(|| AssertError::StdoutMissing {
            expected: format!("{list} in second request"),
        })?;
    require(
        last["role"]
            == if provider.name == "openai" {
                "tool"
            } else {
                "user"
            },
        format!("工具结果轮次的 role 不匹配: {last}"),
    )?;
    let block = match provider.name {
        "openai" => last["content"].clone(),
        "anthropic" => {
            let block = &last["content"][0];
            require(
                block["type"] == "tool_result" && block["tool_use_id"] == "call-1",
                format!("Anthropic tool_result 必须回指 tool_use call-1: {block}"),
            )?;
            block["content"].clone()
        }
        _ => {
            let response = &last["parts"][0]["functionResponse"];
            require(
                response["name"] == capture.tool_names[0].as_str(),
                format!("Gemini functionResponse 必须写明被调工具名: {response}"),
            )?;
            response["response"].clone()
        }
    };
    Ok(block)
}

#[cfg(feature = "fake-tools")]
fn with_usage(
    provider: &Provider,
    script: lattice_system_harness::SseScript,
) -> lattice_system_harness::SseScript {
    use lattice_system_harness::{NativeUsage, SseScript};
    use serde_json::json;
    let usage = match provider.name {
        "openai" => NativeUsage::ChatCompletions(
            json!({"prompt_tokens":100,"completion_tokens":20,"prompt_tokens_details":{"cached_tokens":60,"cache_write_tokens":10},"completion_tokens_details":{"reasoning_tokens":5}}),
        ),
        "anthropic" => NativeUsage::Messages {
            start: json!({"input_tokens":30,"cache_read_input_tokens":60,"cache_creation_input_tokens":10}),
            delta: json!({"input_tokens":0,"cache_read_input_tokens":0,"cache_creation_input_tokens":0,"output_tokens":20}),
        },
        _ => NativeUsage::GenerateContent(
            json!({"promptTokenCount":90,"toolUsePromptTokenCount":10,"cachedContentTokenCount":60,"candidatesTokenCount":15,"thoughtsTokenCount":5}),
        ),
    };
    SseScript::WithUsage {
        script: Box::new(script),
        usage,
    }
}

#[cfg(feature = "fake-tools")]
fn cache_controls(wire: &serde_json::Value) -> Vec<(String, serde_json::Value)> {
    fn collect(wire: &serde_json::Value, path: &str, found: &mut Vec<(String, serde_json::Value)>) {
        match wire {
            serde_json::Value::Object(fields) => {
                for (name, value) in fields {
                    let name_in_path = name.replace('~', "~0").replace('/', "~1");
                    let path = format!("{path}/{name_in_path}");
                    if name == "cache_control" {
                        found.push((path.clone(), value.clone()));
                    }
                    collect(value, &path, found);
                }
            }
            serde_json::Value::Array(values) => {
                for (index, value) in values.iter().enumerate() {
                    collect(value, &format!("{path}/{index}"), found);
                }
            }
            _ => {}
        }
    }
    let mut found = Vec::new();
    collect(wire, "", &mut found);
    found.sort_by(|a, b| a.0.cmp(&b.0));
    found
}

#[cfg(feature = "fake-tools")]
#[test]
fn protocols_cache_and_usage_survive_tool_round_trip_and_backend_restart() {
    let driver = driver();
    for provider in &PROVIDERS {
        let (world, spec) = provider_world(provider);
        world.set_env("LATTICE_TEST_TOOLS", "read_note");
        let sentinel = format!("sentinel-{}", provider.name);
        let call_args = serde_json::json!({
            "action": "read",
            "value": {"note": sentinel},
        })
        .to_string();
        let key = format!("{}-fixture-key", provider.name);
        let server = FakeLlmServer::start(sequence(
            vec![
                tool_call("call-1", "read_note", &call_args),
                lattice_system_harness::SseScript::PlainText {
                    chunks: vec!["answer-".into(), provider.name.into()],
                    finish_reason: "stop".into(),
                },
            ]
            .into_iter()
            .map(|script| with_usage(provider, script))
            .collect(),
        ))
        .expect("fake provider");
        server.expect_api_key(&key);
        world
            .wire_provider(provider.fixture, &server, &key)
            .unwrap();
        let id = match provider.fixture.target {
            ProviderTarget::Builtin(id) | ProviderTarget::Custom(id) => id,
        };
        let configuration = world.home().join(".lattice/providers.toml");
        let source = std::fs::read_to_string(&configuration).unwrap();
        let write_price = if provider.name == "gemini" {
            ""
        } else {
            "cache_write = 3\n"
        };
        std::fs::write(&configuration, format!("{source}\n[providers.{id}.models.\"{model}\"]\neffort=['off']\n[providers.{id}.models.\"{model}\".cost]\ninput = 2\noutput = 10\ncache_read = 0.5\n{write_price}", model=provider.fixture.model)).unwrap();
        let cached = match provider.name {
            // Chat 的 off 来自 effort 元数据，避免按 provider 的 toggle features 推导。
            "openai" => Some(serde_json::json!({"alibaba":{"models":{
                provider.fixture.model:{"id":provider.fixture.model,"reasoning_options":[{"type":"effort","values":["none"]}]}
            }}})),
            "anthropic" => Some(serde_json::json!({"anthropic":{"models":{
                provider.fixture.model:{"id":provider.fixture.model,"limit":{"output":8192}}
            }}})),
            _ => None,
        };
        if let Some(cached) = cached {
            let cache = world.home().join(".lattice/cache");
            std::fs::create_dir_all(&cache).unwrap();
            std::fs::write(
                cache.join("models.json"),
                serde_json::to_vec(&cached).unwrap(),
            )
            .unwrap();
        }
        let scenario_evidence =
            ScenarioEvidence::new(&world, &spec.name, [key.clone()]).expect("evidence");

        let send = SessionSendDriver::new(&driver, &world, &spec, provider.name);
        let target = provider.fixture.model_ref().unwrap();
        let command = send
            .run_command(&[
                "--json",
                "session",
                "send",
                "read note",
                "--agent",
                provider.name,
                "--model",
                &target,
                "--effort",
                "off",
            ])
            .expect("turn");
        let parsed = support::parse_send_stdout(&command.stdout, command.exit_code());
        let outcome = SendExecution { command, parsed };
        let captures = server.captured_requests();
        let tid = outcome.accepted_turn_id().unwrap();
        let detail = send
            .run_command(&["--json", "session", "turn", tid])
            .unwrap();
        let stopped = send.run_command(&["daemon", "stop"]).unwrap();
        let reopened = send
            .run_command(&["--json", "session", "turn", tid])
            .unwrap();

        let result: Result<(), AssertError> = (|| {
            assert_clean_send(&outcome)?;
            require(tool_ok(&outcome.parsed, "read_note"), "read_note 执行成功")?;
            require(outcome.has_completed(), "turn 已完成")?;
            let stdout = String::from_utf8_lossy(&outcome.command.stdout);
            require(
                stdout.contains(&format!("answer-{}", provider.name)),
                "最终文本按序流式输出",
            )?;
            require(
                captures.len() == 2,
                format!("预期 2 次模型请求，实际为 {}", captures.len()),
            )?;
            require(
                detail.is_clean_exit(0) && stopped.is_clean_exit(0) && reopened.is_clean_exit(0),
                "查询、停止与重开均成功",
            )?;
            let saved: serde_json::Value = serde_json::from_slice(&detail.stdout).unwrap();
            require(
                saved == serde_json::from_slice::<serde_json::Value>(&reopened.stdout).unwrap(),
                "重开后用量计量保持不变",
            )?;
            require(
                server.captured_requests().len() == 2,
                "只读查询不触发模型请求",
            )?;
            for step in saved["steps"].as_array().unwrap() {
                let attempt = &step["attempts"][0];
                let expected = serde_json::json!({"input":100,"cache_read":60,"cache_write":if provider.name=="gemini" {None} else {Some(10)},"output":20,"reasoning":if provider.name=="anthropic" {None} else {Some(5)}});
                let expected_fee = if provider.name == "gemini" {
                    0.00031
                } else {
                    0.00032
                };
                require(
                    attempt["usage"] == expected
                        && attempt["usage_kind"] == "complete"
                        && attempt["cost"]["exact"] == true
                        && attempt["cost"]["usd"]
                            .as_f64()
                            .is_some_and(|fee| (fee - expected_fee).abs() < 1e-15),
                    format!("原生用量已归一化并持久化: {attempt}"),
                )?;
            }
            require(
                saved["usage"]["partial_attempts"] == 0
                    && saved["usage"]["kind"] == "complete"
                    && saved["usage"]["input"]["known"] == 200
                    && saved["usage"]["output"]["known"] == 40
                    && saved["usage"]["cost"]["known"]["exact"] == true
                    && saved["usage"]["cost"]["unknown_attempts"] == 0,
                "两次完整尝试聚合出精确费用",
            )?;
            let schema = serde_json::json!({"type": "object", "additionalProperties": true});
            let expected_tools = match provider.name {
                "openai" => serde_json::json!([{
                    "type": "function",
                    "function": {
                        "name": "read_note",
                        "description": "测试假工具 read_note",
                        "parameters": schema,
                    },
                }]),
                "anthropic" => serde_json::json!([{
                    "name": "read_note",
                    "description": "测试假工具 read_note",
                    "input_schema": schema,
                }]),
                _ => serde_json::json!([{
                    "functionDeclarations": [{
                        "name": "read_note",
                        "description": "测试假工具 read_note",
                        "parametersJsonSchema": schema,
                    }],
                }]),
            };
            for (index, capture) in captures.iter().enumerate() {
                let paths = match provider.name {
                    "anthropic" => vec!["/cache_control", "/system/0/cache_control"],
                    "openai" if index == 0 => vec![
                        "/messages/0/content/0/cache_control",
                        "/messages/1/content/0/cache_control",
                    ],
                    "openai" => vec![
                        "/messages/0/content/0/cache_control",
                        "/messages/3/content/0/cache_control",
                    ],
                    _ => vec![],
                };
                let expected: Vec<_> = paths
                    .into_iter()
                    .map(|path| (path.to_string(), serde_json::json!({"type":"ephemeral"})))
                    .collect();
                let actual = cache_controls(&capture.raw);
                require(
                    actual == expected,
                    format!("缓存断点集合不匹配: 预期 {expected:?}, 实际 {actual:?}"),
                )?;
                if provider.name == "anthropic" {
                    require(
                        capture.raw["max_tokens"] == 8192,
                        "Messages 输出上限应来自缓存元数据",
                    )?;
                }
                require(
                    capture.provider == provider.name,
                    format!("请求未命中 {} 路由: {}", provider.name, capture.provider),
                )?;
                require(
                    capture.model == provider.fixture.model,
                    format!("冻结模型应为 {}: {}", provider.fixture.model, capture.model),
                )?;
                require(
                    capture.tool_names == ["read_note"] && capture.raw["tools"] == expected_tools,
                    format!("假工具定义投影不匹配: {}", capture.raw["tools"]),
                )?;
                let disabled = match provider.name {
                    "openai" => capture.raw["reasoning_effort"] == serde_json::json!("none"),
                    "anthropic" => {
                        capture.raw["thinking"] == serde_json::json!({"type":"disabled"})
                            && capture.raw.get("output_config").is_none()
                    }
                    "gemini" => {
                        capture.raw["generationConfig"]["thinkingConfig"]
                            == serde_json::json!({"thinkingBudget":0})
                    }
                    _ => unreachable!(),
                };
                require(
                    disabled,
                    format!("CLI off 必须传到每次 {} 请求", provider.name),
                )?;
            }
            require(
                captures[1].message_roles
                    == if provider.name == "openai" {
                        vec!["system", "user", "assistant", "tool"]
                    } else {
                        vec!["user", provider.assistant_role, "user"]
                    },
                format!("第二次请求角色序列异常: {:?}", captures[1].message_roles),
            )?;
            let projected = projected_tool_result(provider, &captures[1])?;
            let expected_projected = match provider.name {
                "openai" => serde_json::json!([{
                    "type": "text",
                    "text": format!("{{\"note\":\"{sentinel}\"}}"),
                    "cache_control": {"type": "ephemeral"},
                }]),
                "anthropic" => serde_json::json!(format!("{{\"note\":\"{sentinel}\"}}")),
                "gemini" => serde_json::json!({"note": sentinel}),
                _ => unreachable!(),
            };
            require(
                projected == expected_projected,
                format!("工具结果投影不匹配: 预期 {expected_projected}, 实际 {projected}"),
            )?;
            Ok(())
        })();

        finish_scenario(
            scenario_evidence,
            &[
                ("provider turn", &outcome.command),
                ("turn detail", &detail),
                ("stop backend", &stopped),
                ("reopened turn", &reopened),
            ],
            captures,
            result,
        );
    }
}

#[test]
fn gemini_prompt_block_stops_without_retry_and_preserves_usage() {
    use lattice_system_harness::{FAKE_API_KEY, SseScript};
    use serde_json::json;

    let driver = driver();
    for (name, with_usage) in [
        ("gemini-prompt-block", true),
        ("gemini-prompt-block-without-usage", false),
    ] {
        let spec = ScenarioSpec::direct_local(name);
        let world = ScenarioWorld::new(&spec.name).unwrap();
        world.budget(&spec);
        let fixture = ProviderFixture {
            protocol: ProviderProtocol::GenerateContent,
            target: ProviderTarget::Custom("google-generate"),
            model: "gemini-2.5-flash",
        };
        world
            .write_agent_profile("blocked", &fixture.model_ref().unwrap())
            .unwrap();
        let profile = ".lattice/agents/blocked/agent.toml";
        let source = std::fs::read_to_string(world.project_dir().join(profile)).unwrap();
        world
            .write_project_file(
                profile,
                &format!(
                    "{source}\n[llm.retry]\nmax_retries = 1\n\
                     initial_delay_ms = 1\nmax_delay_ms = 1\n"
                ),
            )
            .unwrap();
        world.init_git_project().unwrap();
        let mut frame = json!({
            "promptFeedback": {
                "blockReason": "SAFETY",
                "safetyRatings": [{"category":"HARM_CATEGORY_HARASSMENT","probability":"HIGH"}]
            }
        });
        if with_usage {
            frame["usageMetadata"] = json!({"promptTokenCount":8,"totalTokenCount":8});
        }
        let server = FakeLlmServer::start(SseScript::HttpResponse {
            status: 200,
            content_type: "text/event-stream".into(),
            headers: vec![],
            body: format!("data: {frame}\n\n"),
        })
        .unwrap();
        world.wire_provider(fixture, &server, FAKE_API_KEY).unwrap();
        let configuration = world.home().join(".lattice/providers.toml");
        let source = std::fs::read_to_string(&configuration).unwrap();
        std::fs::write(
            configuration,
            format!(
                "{source}\n[providers.google-generate.models.\"{}\".cost]\n\
                 input = 2\noutput = 10\n",
                fixture.model
            ),
        )
        .unwrap();

        let send = SessionSendDriver::new(&driver, &world, &spec, "blocked");
        let mut evidence = support::evidence(&world, &spec);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let outcome = send.send("blocked prompt").unwrap();
            evidence.record_command("prompt block", &outcome.command);
            assert_clean_send(&outcome).unwrap();
            support::assert_accepted_and_closed(&outcome).unwrap();
            assert!(outcome.command.is_clean_exit(1));
            let detail = send
                .run_command(&[
                    "--json",
                    "session",
                    "turn",
                    outcome.accepted_turn_id().unwrap(),
                ])
                .unwrap();
            evidence.record_command("blocked turn detail", &detail);
            assert!(detail.is_clean_exit(0));
            let turn: serde_json::Value = serde_json::from_slice(&detail.stdout).unwrap();
            assert_eq!(turn["summary"]["outcome"], "failed");
            let steps = turn["steps"].as_array().unwrap();
            assert_eq!(steps.len(), 1);
            let attempts = steps[0]["attempts"].as_array().unwrap();
            assert_eq!(attempts.len(), 1);
            let attempt = &attempts[0];
            assert_eq!(attempt["end"]["end"], "stopped");
            assert_eq!(
                attempt["end"]["cause"],
                json!({"cause":"provider_error","retryable":false})
            );
            assert!(
                attempt["end"]["diagnostic"]
                    .as_str()
                    .unwrap()
                    .contains("blockReason=SAFETY")
            );
            assert!(attempt["retry_delay_ms"].is_null());
            assert_eq!(attempt["usage_kind"], "partial");
            if with_usage {
                assert_eq!(attempt["usage"]["input"], 8);
                assert!(attempt["usage"]["output"].is_null());
                assert_eq!(attempt["cost"]["exact"], false);
            } else {
                assert_eq!(attempt["usage"], serde_json::Value::Null);
                assert_eq!(attempt["cost"], serde_json::Value::Null);
            }
            assert_eq!(server.captured_requests().len(), 1);
        }));
        evidence.record_captures(server.captured_requests());
        support::finish_recorded(evidence, result);
    }
}

#[cfg(feature = "fake-tools")]
#[test]
fn implemented_builtin_protocols_accept_and_record_the_protocol_actually_sent() {
    use lattice_system_harness::{FAKE_API_KEY, SseScript};
    use serde_json::json;
    for (id, protocol, wire, stored) in [
        (
            "openai",
            ProviderProtocol::Responses,
            "responses",
            "responses",
        ),
        ("xai", ProviderProtocol::Responses, "responses", "responses"),
        (
            "google",
            ProviderProtocol::Interactions,
            "interactions",
            "interactions",
        ),
    ] {
        let spec = ScenarioSpec::direct_local(format!("builtin-{id}"));
        let world = ScenarioWorld::new(&spec.name).unwrap();
        world.budget(&spec);
        world.set_env("LATTICE_TEST_TOOLS", "read_note");
        let fixture = ProviderFixture {
            protocol,
            target: ProviderTarget::Builtin(id),
            model: "fixture-model",
        };
        world.write_project_file(".lattice/agents/builtin/agent.toml", &format!(
            "schema_version=1\n[agent]\nname='builtin'\nmodel='{}'\n[prompt]\nsystem='builtin system'\n",fixture.model_ref().unwrap()
        )).unwrap();
        world.init_git_project().unwrap();
        let server = FakeLlmServer::start(sequence(vec![
            tool_call(
                "builtin-call",
                "read_note",
                r#"{"action":"read","value":{"note":"builtin sentinel"}}"#,
            ),
            SseScript::plain("builtin works"),
            SseScript::plain("builtin reopened"),
        ]))
        .unwrap();
        world.wire_provider(fixture, &server, FAKE_API_KEY).unwrap();
        if id == "openai" {
            world
                .wire_provider(
                    ProviderFixture {
                        protocol: ProviderProtocol::Responses,
                        target: ProviderTarget::Builtin("xai"),
                        model: "fixture-model",
                    },
                    &server,
                    FAKE_API_KEY,
                )
                .unwrap();
        }
        let driver = driver();
        let send = SessionSendDriver::new(&driver, &world, &spec, "builtin");
        let mut evidence = support::evidence(&world, &spec);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let sent = send.send("builtin").unwrap();
            evidence.record_command("builtin tool Turn", &sent.command);
            assert!(sent.command.is_clean_exit(0) && sent.has_completed());
            assert_eq!(sent.slots().len(), 1);
            assert!(tool_ok(&sent.parsed, "read_note"));
            let turn = support::live_turn(&sent.parsed);
            assert_eq!(turn["steps"].as_array().unwrap().len(), 2);
            for step in turn["steps"].as_array().unwrap() {
                assert_eq!(
                    step["attempts"][0]["origin"],
                    json!({"provider_id":id,"protocol":stored})
                );
            }
            let sid = send.session_id().unwrap();
            let stop = send.run_command(&["daemon", "stop"]).unwrap();
            evidence.record_command("stop builtin backend", &stop);
            assert!(stop.is_clean_exit(0));
            let reopened = send.send("continue builtin after restart").unwrap();
            evidence.record_command("builtin reopened append", &reopened.command);
            assert!(reopened.command.is_clean_exit(0) && reopened.has_completed());
            assert_eq!(
                support::live_turn(&reopened.parsed)["steps"][0]["attempts"][0]["origin"],
                json!({"provider_id":id,"protocol":stored})
            );
            let captures = server.captured_requests();
            assert_eq!(captures.len(), 3);
            for capture in &captures {
                assert_eq!(capture.provider, wire);
                assert_eq!(capture.raw["stream"], true);
                assert_eq!(capture.raw["store"], false);
                assert_eq!(capture.tool_names, ["read_note"]);
                assert_eq!(capture.raw["tools"][0]["name"], "read_note");
                assert_eq!(
                    capture.raw["tools"][0]["description"],
                    "测试假工具 read_note"
                );
                assert_eq!(
                    capture.raw["tools"][0]["parameters"],
                    json!({"type": "object", "additionalProperties": true})
                );
                assert_eq!(
                    capture.raw[if id == "google" {
                        "system_instruction"
                    } else {
                        "instructions"
                    }],
                    "builtin system"
                );
                for field in [
                    "messages",
                    "contents",
                    "previous_response_id",
                    "previous_interaction_id",
                    "include",
                    "reasoning",
                    "generation_config",
                    "max_output_tokens",
                    "background",
                ] {
                    assert!(capture.raw.get(field).is_none(), "{id}: {field}");
                }
                if id != "google" {
                    assert_eq!(capture.raw["prompt_cache_key"], sid);
                    assert_eq!(capture.raw["tools"][0]["strict"], false);
                }
            }
            let input = captures[1].raw["input"].as_array().unwrap();
            assert!(input.iter().any(|item| item["type"] == "function_call"
                && item["name"] == "read_note"
                && item[if id == "google" { "id" } else { "call_id" }] == "builtin-call"));
            assert!(input.iter().any(|item| item["type"]
                == if id == "google" {
                    "function_result"
                } else {
                    "function_call_output"
                }
                && item["call_id"] == "builtin-call"
                && item.to_string().contains("builtin sentinel")));
            let replayed = captures[2].raw["input"].as_array().unwrap();
            assert_eq!(&replayed[..input.len()], input);
            assert!(
                replayed
                    .iter()
                    .any(|item| item.to_string().contains("builtin works"))
            );
            assert!(
                replayed
                    .last()
                    .unwrap()
                    .to_string()
                    .contains("continue builtin after restart")
            );
            if id == "openai" {
                let switched = send
                    .run_command(&[
                        "--json",
                        "session",
                        "send",
                        "same protocol",
                        "--session",
                        sid,
                        "--model",
                        "xai/fixture-model",
                    ])
                    .unwrap();
                evidence.record_command("builtin Responses provider switch", &switched);
                assert!(switched.is_clean_exit(0), "{}", switched.summary());
                let parsed = support::parse_send_stdout(&switched.stdout, switched.exit_code());
                assert_eq!(
                    support::live_turn(&parsed)["steps"][0]["attempts"][0]["origin"],
                    json!({"provider_id":"xai","protocol":"responses"})
                );
                let switched = server.captured_requests();
                assert_eq!(switched.len(), 4);
                assert!(switched[3].raw.get("reasoning").is_none());
                assert!(
                    switched[3].raw["input"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|item| item["type"] == "function_call_output"
                            && item["call_id"] == "builtin-call")
                );
            }
        }));
        evidence.record_captures(server.captured_requests());
        support::finish_recorded(evidence, result);
    }
}

#[test]
fn builtin_text_history_projects_across_protocols() {
    use lattice_system_harness::{FAKE_API_KEY, SseScript};
    use serde_json::{Value, json};
    let spec = ScenarioSpec::direct_local("builtin-text-projection");
    let world = ScenarioWorld::new(&spec.name).unwrap();
    world.budget(&spec);
    world
        .write_agent_profile("builtin-text", "openai-chat/current")
        .unwrap();
    world.init_git_project().unwrap();
    let server = FakeLlmServer::start(sequence(vec![
        SseScript::plain("legacy chat answer"),
        responses_native(vec![
            json!({"type":"reasoning","id":"private-reasoning","summary":[{"type":"summary_text","text":"private response reasoning"}],"encrypted_content":"private-cipher"}),
            json!({"type":"message","id":"answer","role":"assistant","status":"completed","content":[{"type":"output_text","text":"first answer","annotations":[],"logprobs":[]}]}),
        ]),
        interactions_native(vec![
            json!({"type":"thought","signature":"private-google-signature","summary":[{"type":"text","text":"private interaction thought"}]}),
            json!({"type":"model_output","content":[{"type":"text","text":"second answer"}]}),
        ], "completed"),
        SseScript::plain("third answer"),
    ])).unwrap();
    world
        .wire_provider(
            ProviderFixture {
                protocol: ProviderProtocol::ChatCompletions,
                target: ProviderTarget::Custom("openai-chat"),
                model: "current",
            },
            &server,
            FAKE_API_KEY,
        )
        .unwrap();
    for (id, protocol) in [
        ("openai", ProviderProtocol::Responses),
        ("xai", ProviderProtocol::Responses),
        ("google", ProviderProtocol::Interactions),
    ] {
        world
            .wire_provider(
                ProviderFixture {
                    protocol,
                    target: ProviderTarget::Builtin(id),
                    model: "current",
                },
                &server,
                FAKE_API_KEY,
            )
            .unwrap();
    }
    let driver = driver();
    let send = SessionSendDriver::new(&driver, &world, &spec, "builtin-text");
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let legacy = send.send("legacy input").unwrap();
        evidence.record_command("legacy Chat text source", &legacy.command);
        assert!(legacy.command.is_clean_exit(0) && legacy.has_completed());
        assert!(legacy.slots().is_empty());
        assert_eq!(legacy.reply_text(), "legacy chat answer");
        assert_eq!(
            support::live_turn(&legacy.parsed)["steps"][0]["attempts"][0]["origin"],
            json!({"provider_id":"openai-chat","protocol":"chat-completions"})
        );
        let sid = send.session_id().unwrap();
        let command = send
            .run_command(&[
                "--json",
                "session",
                "send",
                "first input",
                "--session",
                sid,
                "--model",
                "openai/current",
            ])
            .unwrap();
        let first = SendExecution {
            parsed: support::parse_send_stdout(&command.stdout, command.exit_code()),
            command,
        };
        evidence.record_command("builtin text source", &first.command);
        assert!(first.command.is_clean_exit(0) && first.has_completed());
        assert!(first.slots().is_empty());
        assert_eq!(first.reply_text(), "first answer");
        assert_eq!(
            support::live_turn(&first.parsed)["steps"][0]["attempts"][0]["origin"],
            json!({"provider_id":"openai","protocol":"responses"})
        );
        let sid = send.session_id().unwrap();
        let first_turn = first.accepted_turn_id().unwrap();
        let before = send
            .run_command(&["--json", "session", "turn", first_turn])
            .unwrap();
        evidence.record_command("source before text projection", &before);
        for (model, protocol, text) in [
            ("google/current", "interactions", "second input"),
            ("xai/current", "responses", "third input"),
        ] {
            let output = send
                .run_command(&[
                    "--json",
                    "session",
                    "send",
                    text,
                    "--session",
                    sid,
                    "--model",
                    model,
                ])
                .unwrap();
            evidence.record_command(format!("builtin text target {model}"), &output);
            assert!(output.is_clean_exit(0), "{}", output.summary());
            let parsed = support::parse_send_stdout(&output.stdout, output.exit_code());
            assert_eq!(
                support::live_turn(&parsed)["steps"][0]["attempts"][0]["origin"]["protocol"],
                protocol
            );
        }
        let all_captures = server.captured_requests();
        assert_eq!(all_captures.len(), 4);
        assert_eq!(all_captures[0].provider, "openai");
        assert_eq!(all_captures[1].provider, "responses");
        assert!(all_captures[1].raw.get("include").is_none());
        assert!(all_captures[1].raw.get("reasoning").is_none());
        assert_eq!(
            all_captures[1].raw["input"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|item| item["role"] == "assistant")
                .collect::<Vec<_>>(),
            vec![&json!({"type":"message","role":"assistant","content":[
                {"type":"output_text","text":"legacy chat answer","annotations":[]}
            ]})]
        );
        let captures = &all_captures[1..];
        assert_eq!(captures.len(), 3);
        assert!(
            captures[1].raw["input"]
                .to_string()
                .contains("first answer")
        );
        assert!(
            captures[2].raw["input"]
                .to_string()
                .contains("first answer")
        );
        assert!(
            captures[2].raw["input"]
                .to_string()
                .contains("second answer")
        );
        for capture in &captures[1..] {
            let wire = capture.raw["input"].to_string();
            for material in [
                "private-cipher",
                "private response reasoning",
                "private-google-signature",
                "private interaction thought",
            ] {
                assert!(
                    !wire.contains(material),
                    "跨协议文本投影不能泄露 {material}"
                );
            }
        }
        let after = send
            .run_command(&["--json", "session", "turn", first_turn])
            .unwrap();
        evidence.record_command("source after text projection", &after);
        assert_eq!(
            serde_json::from_slice::<Value>(&before.stdout).unwrap(),
            serde_json::from_slice::<Value>(&after.stdout).unwrap()
        );
    }));
    evidence.record_captures(server.captured_requests());
    support::finish_recorded(evidence, result);
}

#[cfg(feature = "fake-tools")]
#[test]
fn same_protocol_provider_switch_preserves_tool_ids_and_restores_source_reasoning() {
    use lattice_system_harness::{FAKE_API_KEY, SseScript};
    use serde_json::json;
    let spec = ScenarioSpec::direct_local("provider-replay-switch");
    let world = ScenarioWorld::new(&spec.name).unwrap();
    world.budget(&spec);
    world.set_env("LATTICE_TEST_TOOLS", "read_note");
    world.write_project_file(".lattice/agents/replay/agent.toml",
        "schema_version=1\n[agent]\nname='replay'\nmodel='openai-chat/first'\n[prompt]\nsystem='Read the requested file.'\n").unwrap();
    world.init_git_project().unwrap();
    let server = FakeLlmServer::start(sequence(vec![
        SseScript::Chunks { chunks: vec![
            json!({"choices":[{"delta":{"reasoning_content":"source-a-reasoning"}}]}),
            json!({"choices":[{"delta":{"content":"read first"}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"replay-call","type":"function","function":{"name":"read_note","arguments":"{\"action\":\"read\",\"value\":{\"note\":\"replay sentinel\"}}"}}]}}]}),
            json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}),
        ]},
        SseScript::plain("first done"),
        SseScript::Chunks { chunks: vec![
            json!({"choices":[{"delta":{"reasoning_content":"source-b-reasoning"}}]}),
            json!({"choices":[{"delta":{"content":"second done"},"finish_reason":"stop"}]}),
        ]},
        SseScript::plain("generate done"),
        SseScript::plain("back to first"),
        SseScript::plain("cross protocol done"),
        SseScript::plain("cross protocol done"),
        SseScript::plain("cross protocol done"),
        SseScript::plain("cross protocol done"),
    ])).unwrap();
    for (model, target) in [
        ("first", ProviderTarget::Custom("openai-chat")),
        ("second", ProviderTarget::Builtin("deepseek")),
    ] {
        world
            .wire_provider(
                ProviderFixture {
                    protocol: ProviderProtocol::ChatCompletions,
                    target,
                    model,
                },
                &server,
                FAKE_API_KEY,
            )
            .unwrap();
    }
    world
        .wire_provider(
            ProviderFixture {
                protocol: ProviderProtocol::GenerateContent,
                target: ProviderTarget::Custom("google-generate"),
                model: "third",
            },
            &server,
            FAKE_API_KEY,
        )
        .unwrap();
    world
        .wire_provider(interactions_fixture(), &server, FAKE_API_KEY)
        .unwrap();
    for (id, protocol) in [
        ("openai", ProviderProtocol::Responses),
        ("xai", ProviderProtocol::Responses),
        ("google", ProviderProtocol::Interactions),
    ] {
        world
            .wire_provider(
                ProviderFixture {
                    protocol,
                    target: ProviderTarget::Builtin(id),
                    model: "current",
                },
                &server,
                FAKE_API_KEY,
            )
            .unwrap();
    }
    let path = world.home().join(".lattice/providers.toml");
    let config = std::fs::read_to_string(&path).unwrap();
    std::fs::write(
        path,
        format!(
            "{config}\n[providers.openai-chat.features]\nreasoning_replay='reasoning-content'\n"
        ),
    )
    .unwrap();
    let driver = driver();
    let send = SessionSendDriver::new(&driver, &world, &spec, "replay");
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let first = send.send("read notes").unwrap();
        evidence.record_command("first provider with Tool", &first.command);
        assert!(first.has_completed() && first.command.is_clean_exit(0));
        assert!(first.parsed.malformed_lines.is_empty());
        let sid = send.session_id().unwrap();
        let choose = |evidence: &mut ScenarioEvidence, id: &str, input: &str, model: &str| {
            let wait = world.budget(&spec).cap_timeout(spec.step_timeout());
            let configured = support::ipc(
                &world,
                wait,
                support::configure_request(
                    &world,
                    json!({"existing":sid}),
                    None,
                    Some(model),
                    None,
                ),
            );
            assert_eq!(configured["response"], "configure", "{configured}");
            let request = support::submit_request(&world, id, sid, input);
            let accepted = support::ipc(&world, wait, request);
            assert_eq!(accepted["response"], "acceptance", "{accepted}");
            assert_eq!(accepted["value"]["turn"]["turn_id"], id);
            let mut watcher = support::Watcher::open(&world, wait, sid);
            watcher.until(
                std::time::Instant::now() + wait,
                "selected model closes",
                |view| {
                    support::turn_closed(view, id)
                        && view.head.as_ref().is_some_and(|head| !head.owner)
                },
            );
            let head = watcher.view.head.as_ref().unwrap();
            let (provider, model) = model.split_once('/').unwrap();
            assert_eq!(head.provider.as_deref(), Some(provider));
            assert_eq!(head.model.as_deref(), Some(model));
            let turn = support::turn_lookup(&world, wait, id);
            assert_eq!(turn["value"]["turn"]["summary"]["outcome"], "completed");
            evidence
                .record_json(&format!("choice-{input}"), &turn)
                .unwrap();
        };
        choose(
            &mut evidence,
            "00000000-0000-4000-8000-000000000002",
            "switch provider",
            "deepseek/second",
        );
        choose(
            &mut evidence,
            "00000000-0000-4000-8000-000000000003",
            "cross protocol",
            "google-generate/third",
        );
        let rewind = support::cli(&world, &spec, &["--json", "session", "rewind", sid, "2"]);
        evidence.record_command("rewind to the original Tool step", &rewind);
        assert!(rewind.is_clean_exit(0), "{}", rewind.summary());
        let stop = support::cli(&world, &spec, &["daemon", "stop"]);
        evidence.record_command("stop before reopening", &stop);
        assert!(stop.is_clean_exit(0));
        world.daemon_departed().unwrap();
        let reopened = support::cli(&world, &spec, &["--json", "session", "log", sid]);
        evidence.record_command("reopen persisted Session before IPC selection", &reopened);
        assert!(reopened.is_clean_exit(0), "{}", reopened.summary());
        choose(
            &mut evidence,
            "00000000-0000-4000-8000-000000000004",
            "return to source",
            "openai-chat/first",
        );
        let captures = server.captured_requests();
        assert_eq!(captures.len(), 5);
        for capture in [&captures[1], &captures[2], &captures[4]] {
            let messages = capture.raw["messages"].as_array().unwrap();
            assert!(
                messages
                    .iter()
                    .any(|m| m["tool_calls"][0]["id"] == "replay-call")
            );
            assert!(messages.iter().any(|m| m["tool_call_id"] == "replay-call"
                && m["content"].as_str().unwrap().contains("replay sentinel")));
        }
        assert!(captures[1].raw.to_string().contains("source-a-reasoning"));
        assert!(!captures[2].raw.to_string().contains("source-a-reasoning"));
        let cross_wire = captures[3].raw.to_string();
        assert!(cross_wire.contains("replay sentinel"));
        assert!(cross_wire.contains("read_note"));
        for material in [
            "source-a-reasoning",
            "source-b-reasoning",
            "thoughtSignature",
            "signature",
            "reasoning_content",
            "\"thought\":true",
        ] {
            assert!(
                !cross_wire.contains(material),
                "cross protocol leaked {material}"
            );
        }
        let parts: Vec<_> = captures[3].raw["contents"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|item| item["parts"].as_array().unwrap())
            .collect();
        assert!(
            parts
                .iter()
                .any(|part| part["functionCall"]["id"] == "replay-call")
        );
        assert!(parts.iter().any(|part| {
            part["functionResponse"]["id"] == "replay-call"
                && part["functionResponse"]
                    .to_string()
                    .contains("replay sentinel")
        }));
        let restored = &captures[4].raw["messages"];
        assert_eq!(restored.as_array().unwrap().len(), 5);
        assert_eq!(restored[1]["content"], "read notes");
        assert_eq!(restored[2]["reasoning_content"], "source-a-reasoning");
        assert_eq!(restored[4]["content"], "return to source");
        for excluded in [
            "source-b-reasoning",
            "first done",
            "switch provider",
            "second done",
            "cross protocol",
            "generate done",
        ] {
            assert!(
                !restored.to_string().contains(excluded),
                "rewound history includes {excluded}"
            );
        }
        let source_turn = first.accepted_turn_id().unwrap();
        let before = send
            .run_command(&["--json", "session", "turn", source_turn])
            .unwrap();
        evidence.record_command("source history before Interactions projection", &before);
        for (index, model) in [
            "interactions-fixture/interactions-model",
            "openai/current",
            "xai/current",
            "google/current",
        ]
        .into_iter()
        .enumerate()
        {
            let sent = support::cli(
                &world,
                &spec,
                &[
                    "--json",
                    "session",
                    "send",
                    "cross protocol",
                    "--session",
                    sid,
                    "--model",
                    model,
                ],
            );
            evidence.record_command(
                format!("{model} sends original call IDs across protocols"),
                &sent,
            );
            assert!(sent.is_clean_exit(0), "{model}: {}", sent.summary());
            let parsed = support::parse_send_stdout(&sent.stdout, sent.exit_code());
            assert_eq!(parsed.closed_result().unwrap().2, "completed");
            let turn = support::live_turn(&parsed);
            assert_eq!(turn["steps"].as_array().unwrap().len(), 1);
            let step = &turn["steps"][0];
            assert_eq!(step["attempts"].as_array().unwrap().len(), 1);
            assert!(step["slots"].as_array().unwrap().is_empty());
            let captures = server.captured_requests();
            assert_eq!(captures.len(), 6 + index);
            let capture = captures.last().unwrap();
            let items = capture.raw["input"].as_array().unwrap();
            let interactions =
                model.starts_with("google/") || model.starts_with("interactions-fixture/");
            assert!(items.iter().any(|item| item["type"] == "function_call"
                && item[if interactions { "id" } else { "call_id" }] == "replay-call"));
            assert!(items.iter().any(|item| item["type"]
                == if interactions {
                    "function_result"
                } else {
                    "function_call_output"
                }
                && item["call_id"] == "replay-call"
                && item.to_string().contains("replay sentinel")));
            for material in [
                "source-a-reasoning",
                "source-b-reasoning",
                "thoughtSignature",
                "signature",
                "reasoning_content",
            ] {
                assert!(
                    !capture.raw.to_string().contains(material),
                    "{model}: leaked {material}"
                );
            }
        }
        let after = send
            .run_command(&["--json", "session", "turn", source_turn])
            .unwrap();
        evidence.record_command("source history after Interactions projection", &after);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&before.stdout).unwrap(),
            serde_json::from_slice::<serde_json::Value>(&after.stdout).unwrap()
        );
    }));
    evidence.record_captures(server.captured_requests());
    support::finish_recorded(evidence, result);
}

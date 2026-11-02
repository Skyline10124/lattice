// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 已实现协议的工具往返、缓存请求与尝试计量。
//!
//! 场景经统一入口配置 endpoint、模型与凭据，fake provider 按请求协议回复流。
//! 每轮先调用 `observe__workspace`，再把工具结果按该协议投影，最后回复文本。

mod support;

use lattice_system_harness::{
    AssertError, CapturedRequest, FakeLlmServer, ProviderFixture, ProviderProtocol, ProviderTarget,
    ScenarioEvidence, ScenarioSpec, ScenarioWorld, SendExecution, SessionSendDriver,
};
use support::{assert_clean_send, driver, finish_scenario, require, sequence, tool_call, tool_ok};

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
    world
        .write_project_file("second.txt", "second-interactions-sentinel")
        .unwrap();
    let steps = vec![
        json!({"type":"model_output","content":[{"type":"text","text":"Inspecting files."}]}),
        json!({"type":"thought","signature":"fixture-thought-signature","summary":[{"type":"text","text":"Check both."},{"type":"text","text":"Read results."}]}),
        json!({"type":"thought","signature":"fixture-signature-only"}),
        json!({"type":"function_call","id":"call_a","name":"observe__workspace","arguments":{"request":{"mode":"read_file","path":"notes.txt"}}}),
        json!({"type":"function_call","id":"call_b","name":"observe__workspace","arguments":{"request":{"mode":"read_file","path":"second.txt"}}}),
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
        for (index, id, sentinel) in [
            (6, "call_a", "sentinel-interactions"),
            (7, "call_b", "second-interactions-sentinel"),
        ] {
            assert_eq!(input[index]["type"], "function_result");
            assert_eq!(input[index]["call_id"], id);
            assert_eq!(input[index]["name"], "observe__workspace");
            assert!(input[index]["result"].as_str().unwrap().contains(sentinel));
        }
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
            assert_eq!(capture.raw["tools"][0]["name"], "observe__workspace");
            assert_eq!(
                capture.raw["tools"][0]["parameters"],
                captures[0].raw["tools"][0]["parameters"]
            );
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
        assert_eq!(view.head.as_ref().unwrap().provider, "interactions-fixture");
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
    world.write_project_file(".lattice/agents/interactions/agent.toml",&format!("schema_version=1\n[agent]\nname='interactions'\nmodel='{}'\n[prompt]\nsystem='test'\n[tools]\nenabled=['observe__workspace']\n[llm.retry]\nmax_retries=1\ninitial_delay_ms=1\nmax_delay_ms=1\n",fixture.model_ref().unwrap())).unwrap();
    world
        .write_project_file("notes.txt", "not executed")
        .unwrap();
    world.init_git_project().unwrap();
    let values = vec![
        json!({"event_type":"interaction.created","interaction":{"object":"interaction","model":"interactions-model"}}),
        json!({"event_type":"step.start","index":0,"step":{"type":"model_output"}}),
        json!({"event_type":"step.delta","index":0,"delta":{"type":"text","text":"partial"},"metadata":{"total_usage":{"total_input_tokens":8,"total_tool_use_tokens":2}}}),
        json!({"event_type":"step.stop","index":0}),
        json!({"event_type":"step.start","index":1,"step":{"type":"function_call","id":"call","name":"observe__workspace","arguments":{}}}),
        json!({"event_type":"step.delta","index":1,"delta":{"type":"arguments_delta","arguments":"{\"request\":{\"mode\":\"read_file\",\"path\":\"notes.txt\"}}"}}),
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
    world
        .write_project_file("second.txt", "second-sentinel")
        .unwrap();
    let output = vec![
        json!({"type":"message","id":"msg_comment","role":"assistant","status":"completed","phase":"commentary","content":[{"type":"output_text","text":"Inspecting two files.","annotations":[],"logprobs":[]}]}),
        json!({"type":"reasoning","id":"rs","summary":[{"type":"summary_text","text":"Check both inputs."}],"encrypted_content":"fixture-final-cipher"}),
        json!({"type":"function_call","id":"item_a","call_id":"call_a","name":"observe__workspace","arguments":"{\"request\":{\"mode\":\"read_file\",\"path\":\"notes.txt\"}}"}),
        json!({"type":"function_call","id":"item_b","call_id":"call_b","name":"observe__workspace","arguments":"{\"request\":{\"mode\":\"read_file\",\"path\":\"second.txt\"}}"}),
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
        assert_eq!(inputs[5]["type"], "function_call_output");
        assert_eq!(inputs[5]["call_id"], "call_a");
        assert!(
            inputs[5]["output"]
                .as_str()
                .unwrap()
                .contains("sentinel-responses")
        );
        assert_eq!(inputs[6]["call_id"], "call_b");
        assert!(
            inputs[6]["output"]
                .as_str()
                .unwrap()
                .contains("second-sentinel")
        );
        for capture in &captures {
            assert_eq!(capture.provider, "responses");
            assert_eq!(capture.raw["instructions"], "You are a concise test agent.");
            assert_eq!(capture.raw["store"], false);
            assert_eq!(capture.raw["stream"], true);
            assert_eq!(capture.raw["prompt_cache_key"], sid);
            assert_eq!(capture.raw["tools"][0]["strict"], false);
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
        assert_eq!(view.head.as_ref().unwrap().provider, "responses-fixture");
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
    world.write_project_file(".lattice/agents/responses/agent.toml",&format!("schema_version=1\n[agent]\nname='responses'\nmodel='{}'\n[prompt]\nsystem='test'\n[tools]\nenabled=['observe__workspace']\n[llm.retry]\nmax_retries=1\ninitial_delay_ms=1\nmax_delay_ms=1\n",fixture.model_ref().unwrap())).unwrap();
    world
        .write_project_file("notes.txt", "not executed")
        .unwrap();
    world.init_git_project().unwrap();
    let events = vec![
        json!({"type":"response.created","response":{"status":"in_progress","output":[],"usage":{"input_tokens":8}}}),
        json!({"type":"response.output_item.added","output_index":0,"item":{"type":"message","role":"assistant","id":"msg","content":[]}}),
        json!({"type":"response.content_part.added","output_index":0,"item_id":"msg","content_index":0,"part":{"type":"output_text","text":""}}),
        json!({"type":"response.output_text.delta","output_index":0,"item_id":"msg","content_index":0,"delta":"partial"}),
        json!({"type":"response.output_item.added","output_index":1,"item":{"type":"function_call","id":"item","call_id":"call","name":"observe__workspace","arguments":""}}),
        json!({"type":"response.function_call_arguments.delta","output_index":1,"item_id":"item","delta":"{\"request\":{\"mode\":\"read_file\",\"path\":\"notes.txt\"}}"}),
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
                json!({"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","id":"truncated","call_id":"truncated-call","name":"observe__workspace","arguments":""}}),
                json!({"type":"response.function_call_arguments.delta","output_index":0,"item_id":"truncated","delta":"{\"request\":"}),
                json!({"type":"response.output_item.done","output_index":0,"item":{"type":"function_call","id":"truncated","call_id":"truncated-call","name":"observe__workspace","status":"incomplete","arguments":"{\"request\":"}}),
                json!({"type":"response.incomplete","response":{"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"}}}),
            ]),
            native(vec![
                json!({"type":"response.completed","response":{"status":"completed","output":[{"type":"function_call","id":"missing-call-id","name":"observe__workspace","arguments":"{\"request\":{\"mode\":\"read_file\",\"path\":\"notes.txt\"}}"}]}}),
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

struct Provider {
    name: &'static str,
    fixture: ProviderFixture<'static>,
    /// Role the provider uses for the assistant turn on the wire.
    assistant_role: &'static str,
}

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

fn provider_world(provider: &Provider) -> (ScenarioWorld, ScenarioSpec) {
    let spec = ScenarioSpec::direct_local(format!("direct-provider-{}", provider.name));
    let world = ScenarioWorld::new(&spec.name).expect("world");
    let model = provider.fixture.model_ref().unwrap();
    let toml = format!(
        "schema_version = 1\n\n\
         [agent]\nname = \"{name}\"\nmodel = \"{model}\"\n\n\
         [prompt]\nsystem = \"You are a concise test agent.\"\n\n\
         [tools]\nenabled = [\"observe__workspace\"]\n",
        name = provider.name,
    );
    world
        .write_project_file(
            &format!(".lattice/agents/{}/agent.toml", provider.name),
            &toml,
        )
        .expect("write agent.toml");
    world
        .write_project_file("notes.txt", &format!("sentinel-{}\n", provider.name))
        .expect("write notes");
    world.init_git_project().expect("init git");
    (world, spec)
}

/// The Tool result the product sent back in the second request, in the
/// provider's own shape, as text for content assertions.
fn projected_tool_result(
    provider: &Provider,
    capture: &CapturedRequest,
) -> Result<String, AssertError> {
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
        format!("tool result turn is not a user turn: {last}"),
    )?;
    let block = match provider.name {
        "openai" => last["content"].clone(),
        "anthropic" => {
            let block = &last["content"][0];
            require(
                block["type"] == "tool_result" && block["tool_use_id"] == "call-1",
                format!("Anthropic tool_result must answer tool_use call-1: {block}"),
            )?;
            block["content"].clone()
        }
        _ => {
            let response = &last["parts"][0]["functionResponse"];
            require(
                response["name"] == capture.tool_names[0].as_str(),
                format!("Gemini functionResponse must name the called Tool: {response}"),
            )?;
            response["response"].clone()
        }
    };
    Ok(block.to_string())
}

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

#[test]
fn protocols_cache_and_usage_survive_tool_round_trip_and_backend_restart() {
    let driver = driver();
    for provider in &PROVIDERS {
        let (world, spec) = provider_world(provider);
        let key = format!("{}-fixture-key", provider.name);
        let server = FakeLlmServer::start(sequence(
            vec![
                tool_call(
                    "call-1",
                    "observe__workspace",
                    r#"{"request":{"mode":"read_file","path":"notes.txt"}}"#,
                ),
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
                "read notes.txt",
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
            require(
                tool_ok(&outcome.parsed, "observe__workspace"),
                "observe__workspace ok",
            )?;
            require(outcome.has_completed(), "turn completed")?;
            let stdout = String::from_utf8_lossy(&outcome.command.stdout);
            require(
                stdout.contains(&format!("answer-{}", provider.name)),
                "final text streamed in order",
            )?;
            require(
                captures.len() == 2,
                format!("expected 2 model requests, got {}", captures.len()),
            )?;
            require(
                detail.is_clean_exit(0) && stopped.is_clean_exit(0) && reopened.is_clean_exit(0),
                "query, stop and reopen succeed",
            )?;
            let saved: serde_json::Value = serde_json::from_slice(&detail.stdout).unwrap();
            require(
                saved == serde_json::from_slice::<serde_json::Value>(&reopened.stdout).unwrap(),
                "reopened metering is unchanged",
            )?;
            require(
                server.captured_requests().len() == 2,
                "queries do not issue model requests",
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
                    format!("native usage normalized and saved: {attempt}"),
                )?;
            }
            require(
                saved["usage"]["partial_attempts"] == 0
                    && saved["usage"]["kind"] == "complete"
                    && saved["usage"]["input"]["known"] == 200
                    && saved["usage"]["output"]["known"] == 40
                    && saved["usage"]["cost"]["known"]["exact"] == true
                    && saved["usage"]["cost"]["unknown_attempts"] == 0,
                "two complete attempts aggregate with exact fees",
            )?;
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
                    format!("complete cache breakpoint set: expected {expected:?}, got {actual:?}"),
                )?;
                if provider.name == "anthropic" {
                    require(
                        capture.raw["max_tokens"] == 8192,
                        "Messages output limit comes from cached metadata",
                    )?;
                }
                require(
                    capture.provider == provider.name,
                    format!(
                        "request reached the {} route: {}",
                        provider.name, capture.provider
                    ),
                )?;
                require(
                    capture.model == provider.fixture.model,
                    format!("frozen model {}: {}", provider.fixture.model, capture.model),
                )?;
                require(!capture.tool_names.is_empty(), "tool declarations sent")?;
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
                    format!("CLI off must reach every {} request", provider.name),
                )?;
            }
            require(
                captures[1].message_roles
                    == if provider.name == "openai" {
                        vec!["system", "user", "assistant", "tool"]
                    } else {
                        vec!["user", provider.assistant_role, "user"]
                    },
                format!("second request history: {:?}", captures[1].message_roles),
            )?;
            let projected = projected_tool_result(provider, &captures[1])?;
            require(
                projected.contains(&format!("sentinel-{}", provider.name)),
                format!("Tool result must carry the file content: {projected}"),
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
        let fixture = PROVIDERS[2].fixture;
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

#[test]
fn implemented_builtin_protocols_record_origins_and_project_plain_history_to_responses() {
    use lattice_system_harness::{FAKE_API_KEY, SseScript};
    for (id, protocol, wire, stored) in [
        (
            "openai",
            ProviderProtocol::ChatCompletions,
            "openai",
            "chat-completions",
        ),
        (
            "xai",
            ProviderProtocol::ChatCompletions,
            "openai",
            "chat-completions",
        ),
        (
            "google",
            ProviderProtocol::GenerateContent,
            "gemini",
            "generate-content",
        ),
    ] {
        let spec = ScenarioSpec::direct_local(format!("builtin-{id}"));
        let world = ScenarioWorld::new(&spec.name).unwrap();
        world.budget(&spec);
        world.init_git_project().unwrap();
        let fixture = ProviderFixture {
            protocol,
            target: ProviderTarget::Builtin(id),
            model: "fixture-model",
        };
        world
            .write_agent_profile("builtin", &fixture.model_ref().unwrap())
            .unwrap();
        let server = FakeLlmServer::start(sequence(vec![
            SseScript::plain("builtin works"),
            SseScript::plain("responses works"),
        ]))
        .unwrap();
        world.wire_provider(fixture, &server, FAKE_API_KEY).unwrap();
        let responses = responses_fixture();
        world
            .wire_provider(responses, &server, FAKE_API_KEY)
            .unwrap();
        let driver = driver();
        let send = SessionSendDriver::new(&driver, &world, &spec, "builtin");
        let mut evidence = support::evidence(&world, &spec);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let sent = send.send("builtin").unwrap();
            evidence.record_command("builtin Turn", &sent.command);
            assert!(sent.has_completed() && sent.command.is_clean_exit(0));
            assert_eq!(sent.reply_text(), "builtin works");
            assert!(sent.slots().is_empty());
            let captures = server.captured_requests();
            assert_eq!(captures.len(), 1);
            assert_eq!(captures[0].provider, wire);
            let turn = support::live_turn(&sent.parsed);
            assert_eq!(
                turn["steps"][0]["attempts"][0]["origin"],
                serde_json::json!({"provider_id":id,"protocol":stored})
            );
            evidence.record_captures(captures);
            let sid = send.session_id().unwrap();
            let command = send
                .run_command(&[
                    "--json",
                    "session",
                    "send",
                    "continue",
                    "--session",
                    sid,
                    "--model",
                    &responses.model_ref().unwrap(),
                ])
                .unwrap();
            evidence.record_command("switch plain history to Responses", &command);
            let switched = SendExecution {
                parsed: support::parse_send_stdout(&command.stdout, command.exit_code()),
                command,
            };
            let turn = support::live_turn(&switched.parsed);
            assert!(
                switched.has_completed() && switched.command.is_clean_exit(0),
                "{}\n{turn}",
                switched.command.summary()
            );
            assert!(switched.parsed.malformed_lines.is_empty());
            assert_eq!(switched.reply_text(), "responses works");
            assert!(switched.slots().is_empty());
            let captures = server.captured_requests();
            assert_eq!(captures.len(), 2);
            assert_eq!(captures[1].provider, "responses");
            assert!(captures[1].raw.get("include").is_none());
            assert!(captures[1].raw.get("reasoning").is_none());
            assert_eq!(
                captures[1].raw["input"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|item| item["role"] == "assistant")
                    .collect::<Vec<_>>(),
                vec![
                    &serde_json::json!({"type":"message","role":"assistant","content":[
                        {"type":"output_text","text":"builtin works","annotations":[]}
                    ]})
                ]
            );
            assert_eq!(
                turn["steps"][0]["attempts"][0]["origin"],
                serde_json::json!({"provider_id":"responses-fixture","protocol":"responses"})
            );
            evidence.record_captures(captures);
        }));
        support::finish_recorded(evidence, result);
    }
}

#[test]
fn same_protocol_provider_switch_preserves_tool_ids_and_restores_source_reasoning() {
    use lattice_system_harness::{FAKE_API_KEY, SseScript};
    use serde_json::json;
    let spec = ScenarioSpec::direct_local("provider-replay-switch");
    let world = ScenarioWorld::new(&spec.name).unwrap();
    world.budget(&spec);
    world
        .write_project_file("notes.txt", "replay sentinel\n")
        .unwrap();
    world.write_project_file(".lattice/agents/replay/agent.toml",
        "schema_version=1\n[agent]\nname='replay'\nmodel='openai/first'\n[prompt]\nsystem='Read the requested file.'\n[tools]\nenabled=['observe__workspace']\n").unwrap();
    world.init_git_project().unwrap();
    let server = FakeLlmServer::start(sequence(vec![
        SseScript::Chunks { chunks: vec![
            json!({"choices":[{"delta":{"reasoning_content":"source-a-reasoning"}}]}),
            json!({"choices":[{"delta":{"content":"read first"}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"replay-call","type":"function","function":{"name":"observe__workspace","arguments":"{\"request\":{\"mode\":\"read_file\",\"path\":\"notes.txt\"}}"}}]}}]}),
            json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}),
        ]},
        SseScript::plain("first done"),
        SseScript::Chunks { chunks: vec![
            json!({"choices":[{"delta":{"reasoning_content":"source-b-reasoning"}}]}),
            json!({"choices":[{"delta":{"content":"second done"},"finish_reason":"stop"}]}),
        ]},
        SseScript::plain("generate done"),
        SseScript::plain("back to first"),
    ])).unwrap();
    for (id, model) in [("openai", "first"), ("deepseek", "second")] {
        world
            .wire_provider(
                ProviderFixture {
                    protocol: ProviderProtocol::ChatCompletions,
                    target: ProviderTarget::Builtin(id),
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
    let path = world.home().join(".lattice/providers.toml");
    let config = std::fs::read_to_string(&path).unwrap();
    std::fs::write(
        path,
        format!("{config}\n[providers.openai.features]\nreasoning_replay='reasoning-content'\n"),
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
            let mut request =
                support::submit_request(&world, id, json!({"existing":sid}), input, None);
            request["request"]["model"] = json!(model);
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
            assert_eq!(head.provider, provider);
            assert_eq!(head.model, model);
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
            "openai/first",
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
        assert!(cross_wire.contains("observe__workspace"));
        for material in [
            "replay-call",
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
        let failed = support::cli(
            &world,
            &spec,
            &[
                "--json",
                "session",
                "send",
                "cross protocol to Interactions",
                "--session",
                sid,
                "--model",
                "interactions-fixture/interactions-model",
            ],
        );
        evidence.record_command(
            "Interactions missing call ID rejects before dispatch",
            &failed,
        );
        assert!(failed.is_clean_exit(1));
        let failed = support::parse_send_stdout(&failed.stdout, failed.exit_code());
        assert_eq!(failed.closed_result().unwrap().2, "failed");
        let entries = failed.entries();
        let step = &entries.last().unwrap().raw["step"];
        assert_eq!(step["attempts"].as_array().unwrap().len(), 1);
        assert_eq!(
            step["attempts"][0]["end"]["cause"],
            json!({"cause":"provider_error","retryable":false})
        );
        assert_eq!(
            step["attempts"][0]["end"]["diagnostic"],
            "provider request projection failed"
        );
        assert!(step["slots"].as_array().unwrap().is_empty());
        assert_eq!(server.captured_requests().len(), 5);
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

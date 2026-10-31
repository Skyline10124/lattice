// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 三个现有协议的工具往返、缓存请求与尝试计量。
//!
//! 场景经统一入口配置 endpoint、模型与凭据，fake provider 按请求协议回复流。
//! 每轮先调用 `observe__workspace`，再把工具结果按该协议投影，最后回复文本。

mod support;

use lattice_system_harness::{
    AssertError, CapturedRequest, FakeLlmServer, ProviderFixture, ProviderProtocol, ProviderTarget,
    ScenarioEvidence, ScenarioSpec, ScenarioWorld, SessionSendDriver,
};
use support::{assert_clean_send, driver, finish_scenario, require, sequence, tool_call, tool_ok};

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
        if provider.name == "anthropic" {
            let cache = world.home().join(".lattice/cache");
            std::fs::create_dir_all(&cache).unwrap();
            std::fs::write(
                cache.join("models.json"),
                serde_json::to_vec(&serde_json::json!({"anthropic":{"models":{
                    provider.fixture.model:{"id":provider.fixture.model,"limit":{"output":8192}}
                }}}))
                .unwrap(),
            )
            .unwrap();
        }
        let scenario_evidence =
            ScenarioEvidence::new(&world, &spec.name, [key.clone()]).expect("evidence");

        let send = SessionSendDriver::new(&driver, &world, &spec, provider.name);
        let outcome = send.send("read notes.txt").expect("turn");
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
                require(
                    attempt["usage"] == expected
                        && attempt["usage_kind"] == "partial"
                        && attempt["cost"].is_null(),
                    format!("native usage normalized and saved: {attempt}"),
                )?;
            }
            require(
                saved["usage"]["partial_attempts"] == 2
                    && saved["usage"]["input"]["known"] == 200
                    && saved["usage"]["output"]["known"] == 40
                    && saved["usage"]["cost"]["unknown_attempts"] == 2,
                "two attempts aggregate with unknown fees",
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
fn implemented_builtin_protocols_accept_and_record_the_protocol_actually_sent() {
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
        let server = FakeLlmServer::start(SseScript::plain("builtin works")).unwrap();
        world.wire_provider(fixture, &server, FAKE_API_KEY).unwrap();
        let driver = driver();
        let send = SessionSendDriver::new(&driver, &world, &spec, "builtin");
        let mut evidence = support::evidence(&world, &spec);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let sent = send.send("builtin").unwrap();
            evidence.record_command("builtin Turn", &sent.command);
            assert!(sent.command.is_clean_exit(0));
            let captures = server.captured_requests();
            assert_eq!(captures.len(), 1);
            assert_eq!(captures[0].provider, wire);
            let turn = support::live_turn(&sent.parsed);
            assert_eq!(
                turn["steps"][0]["attempts"][0]["origin"],
                serde_json::json!({"provider_id":id,"protocol":stored})
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
    world
        .write_agent_profile("second", "deepseek/second")
        .unwrap();
    world
        .write_agent_profile("third", "google-generate/third")
        .unwrap();
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
        let second = support::cli(
            &world,
            &spec,
            &[
                "--json",
                "session",
                "send",
                "switch provider",
                "--session",
                sid,
                "--agent",
                "second",
            ],
        );
        evidence.record_command("same protocol provider switch", &second);
        assert!(second.is_clean_exit(0), "{}", second.summary());
        let second_parsed = support::parse_send_stdout(&second.stdout, second.exit_code());
        support::assert_accepted_and_closed(&second_parsed).unwrap();
        assert!(second_parsed.malformed_lines.is_empty());
        let cross = support::cli(
            &world,
            &spec,
            &[
                "--json",
                "session",
                "send",
                "cross protocol",
                "--session",
                sid,
                "--agent",
                "third",
            ],
        );
        evidence.record_command("cross protocol to generateContent", &cross);
        assert!(cross.is_clean_exit(0), "{}", cross.summary());
        let cross_parsed = support::parse_send_stdout(&cross.stdout, cross.exit_code());
        support::assert_accepted_and_closed(&cross_parsed).unwrap();
        assert!(cross_parsed.malformed_lines.is_empty());
        let rewind = support::cli(&world, &spec, &["--json", "session", "rewind", sid, "2"]);
        evidence.record_command("rewind to the original Tool step", &rewind);
        assert!(rewind.is_clean_exit(0), "{}", rewind.summary());
        let stop = support::cli(&world, &spec, &["daemon", "stop"]);
        evidence.record_command("stop before reopening", &stop);
        assert!(stop.is_clean_exit(0));
        world.daemon_departed().unwrap();
        let third = support::cli(
            &world,
            &spec,
            &[
                "--json",
                "session",
                "send",
                "return to source",
                "--session",
                sid,
                "--agent",
                "replay",
            ],
        );
        evidence.record_command("reopen and restore source", &third);
        assert!(third.is_clean_exit(0), "{}", third.summary());
        let third_parsed = support::parse_send_stdout(&third.stdout, third.exit_code());
        support::assert_accepted_and_closed(&third_parsed).unwrap();
        assert!(third_parsed.malformed_lines.is_empty());
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
    }));
    evidence.record_captures(server.captured_requests());
    support::finish_recorded(evidence, result);
}

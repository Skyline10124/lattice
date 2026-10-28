// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Direct-Local 的 Anthropic Messages 与 generateContent 协议场景。
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

const PROVIDERS: [Provider; 2] = [
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
        "anthropic" => ("messages", capture.raw["messages"].as_array()),
        _ => ("contents", capture.raw["contents"].as_array()),
    };
    let last = last
        .and_then(|items| items.last())
        .ok_or_else(|| AssertError::StdoutMissing {
            expected: format!("{list} in second request"),
        })?;
    require(
        last["role"] == "user",
        format!("tool result turn is not a user turn: {last}"),
    )?;
    let block = match provider.name {
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

#[test]
fn anthropic_and_gemini_turns_call_a_tool_and_project_the_result_back() {
    let driver = driver();
    for provider in &PROVIDERS {
        let (world, spec) = provider_world(provider);
        let key = format!("{}-fixture-key", provider.name);
        let server = FakeLlmServer::start(sequence(vec![
            tool_call(
                "call-1",
                "observe__workspace",
                r#"{"request":{"mode":"read_file","path":"notes.txt"}}"#,
            ),
            lattice_system_harness::SseScript::PlainText {
                chunks: vec!["answer-".into(), provider.name.into()],
                finish_reason: "stop".into(),
            },
        ]))
        .expect("fake provider");
        server.expect_api_key(&key);
        world
            .wire_provider(provider.fixture, &server, &key)
            .unwrap();
        let scenario_evidence =
            ScenarioEvidence::new(&world, &spec.name, [key.clone()]).expect("evidence");

        let send = SessionSendDriver::new(&driver, &world, &spec, provider.name);
        let outcome = send.send("read notes.txt").expect("turn");
        let captures = server.captured_requests();

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
            for capture in &captures {
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
                captures[1].message_roles == ["user", provider.assistant_role, "user"],
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
            &[("provider turn", &outcome.command)],
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

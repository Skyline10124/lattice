// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Direct-Local Anthropic and Gemini protocol scenarios.
//!
//! The product reaches each provider through its formal base URL setting
//! (`ANTHROPIC_BASE_URL` / `GEMINI_BASE_URL`) and credential; the fake provider
//! answers in that provider's streaming protocol. Each scenario is a two-step
//! Turn: the model calls `observe.workspace`, the Tool result is projected back
//! in the provider's own shape, and the model then answers in text.

mod support;

use lattice_system_harness::{
    AssertError, CapturedRequest, FakeLlmServer, ScenarioEvidence, ScenarioSpec, ScenarioWorld,
    SessionSendDriver,
};
use support::{assert_clean_send, driver, finish_scenario, require, sequence, tool_call, tool_ok};

struct Provider {
    name: &'static str,
    alias: &'static str,
    model: &'static str,
    base_url_env: &'static str,
    key_env: &'static str,
    /// Role the provider uses for the assistant turn on the wire.
    assistant_role: &'static str,
}

const PROVIDERS: [Provider; 2] = [
    Provider {
        name: "anthropic",
        alias: "sonnet",
        model: "claude-sonnet-4-6",
        base_url_env: "ANTHROPIC_BASE_URL",
        key_env: "ANTHROPIC_API_KEY",
        assistant_role: "assistant",
    },
    Provider {
        name: "gemini",
        alias: "gemini",
        model: "gemini-2.5-flash",
        base_url_env: "GEMINI_BASE_URL",
        key_env: "GEMINI_API_KEY",
        assistant_role: "model",
    },
];

fn provider_world(provider: &Provider) -> (ScenarioWorld, ScenarioSpec) {
    let spec = ScenarioSpec::direct_local(format!("direct-provider-{}", provider.name));
    let world = ScenarioWorld::new(&spec.name).expect("world");
    let toml = format!(
        "schema_version = 1\n\n\
         [agent]\nname = \"{name}\"\nmodel = \"{alias}\"\n\n\
         [prompt]\nsystem = \"You are a concise test agent.\"\n\n\
         [tools]\nenabled = [\"observe.workspace\"]\n",
        name = provider.name,
        alias = provider.alias,
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
                "observe.workspace",
                r#"{"mode":"read_file","path":"notes.txt"}"#,
            ),
            lattice_system_harness::SseScript::PlainText {
                chunks: vec!["answer-".into(), provider.name.into()],
                finish_reason: "stop".into(),
            },
        ]))
        .expect("fake provider");
        server.expect_api_key(&key);
        world.set_env(provider.base_url_env, server.base_url());
        world.set_env(provider.key_env, &key);
        let scenario_evidence =
            ScenarioEvidence::new(&world, &spec.name, [key.clone()]).expect("evidence");

        let send = SessionSendDriver::new(&driver, &world, &spec, provider.name);
        let outcome = send.send("read notes.txt").expect("turn");
        let captures = server.captured_requests();

        let result: Result<(), AssertError> = (|| {
            assert_clean_send(&outcome)?;
            require(
                tool_ok(&outcome.parsed, "observe.workspace"),
                "observe.workspace ok",
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
                    capture.model == provider.model,
                    format!("frozen model {}: {}", provider.model, capture.model),
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

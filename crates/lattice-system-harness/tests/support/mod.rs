// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

#![allow(dead_code)]

use std::process::Output;

use lattice_system_harness::{
    AssertError, CapturedRequest, CommandOutcome, FAKE_API_KEY, FakeLlmServer, LatticeCommand,
    LatticeDriver, ResponseGate, ScenarioEvidence, ScenarioSpec, ScenarioWorld, SendExecution,
    SendOutcome, SseScript, Termination, TurnEventLine, failure_dir,
};
use serde_json::Value;

pub(crate) fn driver() -> LatticeDriver {
    LatticeDriver::from_env().expect("LATTICE_BIN must point to a built lattice binary")
}

pub(crate) fn assert_clean_send(execution: &SendExecution) -> Result<(), AssertError> {
    if !matches!(&execution.command.termination, Termination::Exited(Some(_))) {
        return Err(AssertError::StdoutMissing {
            expected: format!(
                "session send did not exit normally: {:?}",
                execution.command.termination
            ),
        });
    }
    if !execution.malformed_lines.is_empty() {
        return Err(AssertError::StdoutMissing {
            expected: format!(
                "session send emitted malformed JSONL: {:?}",
                execution.malformed_lines
            ),
        });
    }
    Ok(())
}

pub(crate) fn require(condition: bool, expected: impl Into<String>) -> Result<(), AssertError> {
    if condition {
        Ok(())
    } else {
        Err(AssertError::StdoutMissing {
            expected: expected.into(),
        })
    }
}

pub(crate) fn evidence(world: &ScenarioWorld, spec: &ScenarioSpec) -> ScenarioEvidence {
    ScenarioEvidence::new(world, failure_dir(&spec.name), [FAKE_API_KEY.to_string()])
        .expect("scenario evidence")
}

pub(crate) fn finish_scenario(
    mut evidence: ScenarioEvidence,
    commands: &[(&str, &CommandOutcome)],
    captures: Vec<CapturedRequest>,
    result: Result<(), AssertError>,
) {
    for (summary, outcome) in commands {
        evidence.record_command(*summary, outcome);
    }
    evidence.record_captures(captures);
    evidence
        .finish(result.is_err())
        .unwrap_or_else(|error| panic!("scenario evidence failed: {error}"));
    result.unwrap_or_else(|error| panic!("{error}"));
}

pub(crate) fn wire_fake_llm(
    world: &ScenarioWorld,
    agent: &str,
    script: SseScript,
) -> FakeLlmServer {
    let server = FakeLlmServer::start(script).expect("start fake llm");
    world.set_env("OPENAI_COMPATIBLE_BASE_URL", server.base_url());
    world.set_env("OPENAI_COMPATIBLE_MODEL", agent);
    world.set_env("OPENAI_COMPATIBLE_API_KEY", FAKE_API_KEY);
    server
}

pub(crate) fn wire_gated_fake_llm(
    world: &ScenarioWorld,
    agent: &str,
    script: SseScript,
) -> (FakeLlmServer, ResponseGate) {
    let (server, gate) = FakeLlmServer::start_gated(script).expect("start gated fake llm");
    world.set_env("OPENAI_COMPATIBLE_BASE_URL", server.base_url());
    world.set_env("OPENAI_COMPATIBLE_MODEL", agent);
    world.set_env("OPENAI_COMPATIBLE_API_KEY", FAKE_API_KEY);
    (server, gate)
}

pub(crate) fn direct_world(
    prefix: &str,
    agent: &str,
    tools: &str,
    extra_profile: &str,
) -> (ScenarioWorld, ScenarioSpec) {
    let spec = ScenarioSpec::direct_local(prefix);
    let world = ScenarioWorld::new(&spec.name).expect("world");
    let toml = format!(
        "schema_version = 1\n\n\
         [agent]\nname = \"{agent}\"\nmodel = \"compat:{agent}\"\n\n\
         [prompt]\nsystem = \"You are a concise test agent.\"\n\n\
         [tools]\nenabled = {tools}\n{extra_profile}"
    );
    world
        .write_project_file(&format!(".lattice/agents/{agent}/agent.toml"), &toml)
        .expect("write agent.toml");
    world.init_git_project().expect("init git");
    (world, spec)
}

pub(crate) fn last_tool_content(capture: &CapturedRequest) -> Option<String> {
    capture
        .raw
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|messages| {
            messages
                .iter()
                .rev()
                .find(|message| message.get("role").and_then(Value::as_str) == Some("tool"))
                .and_then(|message| message.get("content").and_then(Value::as_str))
                .map(str::to_string)
        })
}

pub(crate) fn tool_result_value(capture: &CapturedRequest) -> Option<Value> {
    let content = last_tool_content(capture)?;
    if content.starts_with('{') {
        serde_json::from_str(&content).ok()
    } else {
        None
    }
}

pub(crate) fn tool_failed(outcome: &SendOutcome, name: &str) -> bool {
    outcome.events.iter().any(|event| {
        matches!(
            event,
            TurnEventLine::ToolCallFinished {
                name: event_name,
                ok: false,
                ..
            } if event_name == name
        )
    })
}

pub(crate) fn tool_ok(outcome: &SendOutcome, name: &str) -> bool {
    outcome.events.iter().any(|event| {
        matches!(
            event,
            TurnEventLine::ToolCallFinished {
                name: event_name,
                ok: true,
                ..
            } if event_name == name
        )
    })
}

/// True when a failed CEP call for `method` finished with `error_code`.
pub(crate) fn finished_with_code(outcome: &SendOutcome, method: &str, code: &str) -> bool {
    outcome.events.iter().any(|event| match event {
        TurnEventLine::Unknown { type_name, raw } if type_name == "cep_call_finished" => {
            raw.get("method").and_then(Value::as_str) == Some(method)
                && raw.get("ok").and_then(Value::as_bool) == Some(false)
                && raw.get("error_code").and_then(Value::as_str) == Some(code)
        }
        _ => false,
    })
}

pub(crate) fn tool_call(id: &str, name: &str, args: &str) -> SseScript {
    SseScript::ToolCall {
        call_id: id.into(),
        tool_name: name.into(),
        arguments_json: args.into(),
    }
}

pub(crate) fn sequence(scripts: Vec<SseScript>) -> SseScript {
    SseScript::Sequence { scripts }
}

pub(crate) fn cli(world: &ScenarioWorld, spec: &ScenarioSpec, args: &[&str]) -> Output {
    driver()
        .run(
            world,
            &LatticeCommand {
                args: args.iter().map(|arg| arg.to_string()).collect(),
                timeout: Some(spec.step_timeout()),
            },
        )
        .expect("drive lattice")
        .output
}

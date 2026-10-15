// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

#![allow(dead_code)]

#[cfg(feature = "crash-barriers")]
pub(crate) mod crash;
use lattice_system_harness::{
    AssertError, CapturedRequest, CommandOutcome, FAKE_API_KEY, FakeLlmServer, HarnessIntervention,
    LatticeCommand, LatticeDriver, ProcessTermination, ResponseGate, ScenarioEvidence,
    ScenarioSpec, ScenarioWorld, SendExecution, SendOutcome, SseScript, TurnEventLine,
};
use serde_json::Value;

pub(crate) fn driver() -> LatticeDriver {
    LatticeDriver::from_env().expect("LATTICE_BIN must point to a built lattice binary")
}

pub(crate) fn assert_clean_send(execution: &SendExecution) -> Result<(), AssertError> {
    if execution.command.clean_exit_code().is_none() {
        return Err(AssertError::StdoutMissing {
            expected: format!(
                "session send did not exit cleanly: {}",
                execution.command.summary()
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

/// 接受与持久关闭快照必须对应同一 Turn，且 summary 来自公开读取。
pub(crate) fn assert_accepted_and_closed(outcome: &SendOutcome) -> Result<(), AssertError> {
    let id = outcome
        .accepted_turn_id()
        .ok_or_else(|| AssertError::StdoutMissing {
            expected: "durable acceptance acknowledgement".into(),
        })?;
    let accepted = outcome.events.iter().position(|event| matches!(event, TurnEventLine::Unknown { type_name, .. } if type_name == "acceptance")).unwrap();
    let closed = outcome.events.iter().position(|event| {
        matches!(event,
        TurnEventLine::Unknown { type_name, raw } if type_name == "snapshot"
        && raw["view"]["selected_turn"]["turn_id"].as_str() == Some(id)
        && raw["view"]["selected_turn"]["summary"]["progress"]["progress"] == "closed")
    });
    require(
        closed.is_some_and(|closed| accepted < closed),
        "closed snapshot must follow acceptance",
    )
}

/// 不要求瞬时事件重放；公开快照中的持久 summary 必须晚于指定进度。
pub(crate) fn assert_committed_view_after(
    outcome: &SendOutcome,
    after: &str,
) -> Result<(), AssertError> {
    let anchor = outcome
        .events
        .iter()
        .rposition(|event| event.type_name() == after)
        .ok_or_else(|| AssertError::StdoutMissing {
            expected: format!("missing '{after}'"),
        })?;
    let closed = outcome.events.iter().rposition(|event| {
        matches!(event,
        TurnEventLine::Unknown { type_name, raw } if type_name == "snapshot"
        && raw["view"]["selected_turn"]["summary"]["outcome"] == "completed")
    });
    require(
        closed.is_some_and(|closed| anchor < closed),
        format!("completed snapshot must follow '{after}'"),
    )?;
    require(
        outcome
            .closed_result()
            .is_some_and(|(_, _, result)| result == "completed"),
        "durable completed summary",
    )
}

pub(crate) fn assert_forced_kill(outcome: &CommandOutcome) {
    assert_eq!(
        outcome.termination,
        Some(ProcessTermination::Signaled(libc::SIGKILL))
    );
    assert_eq!(outcome.interventions, vec![HarnessIntervention::ForcedKill]);
    assert!(outcome.cleanup.forced);
    assert!(outcome.cleanup.succeeded(), "{:?}", outcome.cleanup);
}

/// 直接经 IPC 提交时的信任根：harness CA 的 DER，与提交端从 `SSL_CERT_FILE` 读到的相同。
pub(crate) fn harness_roots_der() -> Value {
    serde_json::json!([lattice_system_harness::TlsIdentity::shared()
        .expect("harness TLS identity")
        .ca_der()])
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
    ScenarioEvidence::new(world, &spec.name, [FAKE_API_KEY.to_string()]).expect("scenario evidence")
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
    let primary = result.err().map(|error| error.to_string());
    evidence
        .finish_report(primary)
        .unwrap_or_else(|report| panic!("{report}"));
}

/// 复用 PTY 的公开读取采集，保留整棵树及其中每个 Turn 的操作员投影。
pub(crate) fn record_session(
    session: &lattice_system_harness::SessionSendDriver<'_>,
    evidence: &mut ScenarioEvidence,
    sid: &str,
    parameters: Value,
) {
    let log = session
        .run_command(&["--json", "session", "log", sid])
        .unwrap();
    evidence.record_command("--json session log", &log);
    assert!(log.is_clean_exit(0));
    let tree: Value = serde_json::from_slice(&log.stdout).unwrap();
    for args in [
        vec!["session", "log", sid],
        vec!["--json", "session", "list"],
        vec!["--json", "audit", "--tail"],
        vec!["--json", "lancet", "status"],
    ] {
        let out = session.run_command(&args).unwrap();
        evidence.record_command(args.join(" "), &out);
        assert!(out.is_clean_exit(0));
    }
    for node in tree["nodes"].as_array().unwrap() {
        if let Some(tid) = node["turn_start"]["turn_id"].as_str() {
            for args in [
                vec!["--json", "session", "turn", tid],
                vec!["--json", "audit", "replay", "--session", sid, "--turn", tid],
            ] {
                let out = session.run_command(&args).unwrap();
                evidence.record_command(args.join(" "), &out);
                assert!(out.is_clean_exit(0));
            }
        }
    }
    evidence.record_json("state-summary", &tree).unwrap();
    evidence.record_json("parameters", &parameters).unwrap();
}

/// 失败时写入失败报告并 panic；成功时只排空场景后台，不写产物。
pub(crate) fn finish_recorded(evidence: ScenarioEvidence, result: std::thread::Result<()>) {
    let primary = result.err().map(|error| {
        error
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| error.downcast_ref::<&str>().map(|text| text.to_string()))
            .unwrap_or_else(|| "scenario panicked with a non-text payload".into())
    });
    evidence
        .finish_report(primary)
        .unwrap_or_else(|report| panic!("{report}"));
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

pub(crate) fn cli(world: &ScenarioWorld, spec: &ScenarioSpec, args: &[&str]) -> CommandOutcome {
    driver()
        .run(
            world,
            &LatticeCommand {
                args: args.iter().map(|arg| arg.to_string()).collect(),
                timeout: Some(spec.step_timeout()),
            },
        )
        .expect("drive lattice")
}

// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 通过接受回执、固定结果、公开 log/status 与 provider capture 核验两次 Turn。
//! 无 Gap 时保留流事件顺序检查；有 Gap 时不要求补播，持久正文和后续模型上下文仍须精确一致。
//! 缺少凭据在接受前拒绝，缺失 Session 仅返回结构化错误，不产生模型或 Tool 活动。

mod support;

use lattice_system_harness::{
    AssertError, CapturedRequest, ScenarioSpec, ScenarioWorld, SendExitCode, SendOutcome,
    SessionSendDriver, SseScript, TurnEventLine, assert_event_subsequence, derive_send_exit_code,
    parse_send_stdout,
};
use serde_json::Value;
use support::{assert_clean_send, driver, evidence, finish_scenario, require, wire_fake_llm};

/// A Direct-Local project world with a fake-LLM-backed agent profile.
fn turn_world(prefix: &str, agent: &str) -> (ScenarioWorld, ScenarioSpec) {
    let spec = ScenarioSpec::direct_local(prefix);
    let world = ScenarioWorld::new(&spec.name).expect("world");
    let toml = format!(
        "schema_version = 1\n\n[agent]\nname = \"{agent}\"\nmodel = \"compat:{agent}\"\n\n[prompt]\nsystem = \"You are a concise test agent. Reply briefly.\"\n\n[tools]\nenabled = []\n"
    );
    world
        .write_project_file(&format!(".lattice/agents/{agent}/agent.toml"), &toml)
        .expect("write agent.toml");
    world.init_git_project().expect("init git project");
    (world, spec)
}

/// `(role, content)` pairs of a captured Chat Completions request, in wire
/// order. Non-string content (e.g. part arrays) is compared by its JSON text.
fn message_parts(capture: &CapturedRequest) -> Vec<(String, String)> {
    capture
        .raw
        .get("messages")
        .and_then(Value::as_array)
        .map(|messages| {
            messages
                .iter()
                .map(|message| {
                    let role = message
                        .get("role")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let content = match message.get("content") {
                        Some(Value::String(text)) => text.clone(),
                        Some(other) => other.to_string(),
                        None => String::new(),
                    };
                    (role, content)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 回执确认接受，固定视图确认身份与结果；Gap 不生成虚构的模型或提交事件。
fn assert_completed_turn(outcome: &SendOutcome, session_id: &str) -> Result<(), AssertError> {
    support::assert_accepted_and_closed(outcome)?;
    let view = outcome
        .events
        .iter()
        .rev()
        .find_map(|event| match event {
            TurnEventLine::Unknown { type_name, raw } if type_name == "snapshot" => {
                Some(&raw["view"])
            }
            _ => None,
        })
        .expect("fixed closure snapshot");
    require(
        view["session"]["session_id"] == session_id && view["head"]["branch"]["name"] == "main",
        "fixed Session identity and attached branch",
    )?;
    let (committed_session, commit_id, result) =
        outcome.committed_result().expect("committed result");
    require(
        committed_session == session_id && !commit_id.is_empty() && result == "completed",
        "completed result identity",
    )?;
    require(
        view["requested_turn"]["provider"] == "openai_compatible"
            && view["requested_turn"]["model"] == "compat:dl-contract",
        "frozen routed provider and model",
    )?;
    let has_gap = outcome.events.iter().any(
        |event| matches!(event, TurnEventLine::Unknown { type_name, .. } if type_name == "gap"),
    );
    if !has_gap {
        assert_event_subsequence(
            outcome,
            &[
                "turn_started",
                "model_started",
                "token_delta",
                "model_finished",
                "turn_committing",
                "turn_committed",
            ],
        )
        .map_err(|expected| AssertError::StdoutMissing { expected })?;
    }
    require(
        !outcome.events.iter().any(|event| {
            matches!(
                event,
                TurnEventLine::TurnFailed { .. }
                    | TurnEventLine::TurnCancelled { .. }
                    | TurnEventLine::CommitFailed { .. }
            )
        }),
        "no failure event on successful Turn",
    )?;
    require(
        outcome.exit_code == Some(SendExitCode::Committed)
            && derive_send_exit_code(&outcome.events) == SendExitCode::Committed,
        "completion and exit 0 agree",
    )
}

/// 公开固定结果身份用于与 log/status 交叉核验。
fn committed_id(outcome: &SendOutcome) -> String {
    outcome
        .committed_result()
        .expect("fixed committed result")
        .1
        .to_string()
}

#[test]
fn direct_turn_completed_reopen_contract() {
    let driver = driver();
    let (world, spec) = turn_world("direct-turn-contract", "dl-contract");
    let server = wire_fake_llm(
        &world,
        "dl-contract",
        SseScript::Sequence {
            scripts: vec![
                SseScript::plain("first answer"),
                SseScript::plain("second answer"),
            ],
        },
    );
    let scenario_evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-contract", "main")
        .expect("create session");
    let session_id = send.session_id().to_string();
    let sid = session_id.as_str();

    let first = send.send("first prompt").expect("first turn drives");
    let log_after_first = send
        .run_command(&["--json", "session", "log", sid])
        .expect("session log after first turn");
    let second = send.send("second prompt").expect("second turn drives");
    let log_after_second = send
        .run_command(&["--json", "session", "log", sid])
        .expect("session log after second turn");
    let human_log = send
        .run_command(&["session", "log", sid])
        .expect("human session log after second turn");
    let status_output = send
        .run_command(&["--json", "session", "status", sid])
        .expect("session status --json");
    let missing = send
        .run_command(&["--json", "session", "send", "missing-session-id", "hello"])
        .expect("missing-session send drives");
    let captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        // --- stream contract: both turns complete with the required shape ---
        assert_clean_send(&first)?;
        assert_clean_send(&second)?;
        assert_completed_turn(&first.parsed, &session_id)?;
        assert_completed_turn(&second.parsed, &session_id)?;

        // --- message assembly: request one is system + current user input ---
        require(
            captures.len() == 2,
            format!("expected one LLM request per turn, got: {captures:?}"),
        )?;
        let first_parts = message_parts(&captures[0]);
        require(
            captures[0].model == "dl-contract"
                && first_parts.len() == 2
                && first_parts[0].0 == "system"
                && first_parts[1].0 == "user"
                && first_parts[1].1.contains("first prompt"),
            format!("first request not system+user with current input: {first_parts:?}"),
        )?;

        // --- reopen context: request two carries exactly the prior
        // user/assistant pair before the new user message ---
        let second_parts = message_parts(&captures[1]);
        let roles: Vec<&str> = second_parts.iter().map(|(role, _)| role.as_str()).collect();
        require(
            roles == ["system", "user", "assistant", "user"],
            format!("second request roles not transcript+input: {roles:?}"),
        )?;
        require(
            second_parts[1].1 == "first prompt"
                && second_parts[2].1 == "first answer"
                && second_parts[3].1 == "second prompt",
            format!("second request lacked exact prior pair before new input: {second_parts:?}"),
        )?;

        // --- log/status: both commit IDs, count two, HEAD main ---
        let log_stdout = String::from_utf8_lossy(&log_after_second.stdout);
        let log_json: Value = serde_json::from_str(log_stdout.trim()).map_err(|error| {
            AssertError::StdoutMissing {
                expected: format!("session log --json not JSON: {error}"),
            }
        })?;
        let turns = log_json
            .get("turns")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let first_commit = committed_id(&first.parsed);
        let second_commit = committed_id(&second.parsed);
        let logged_ids: Vec<&str> = turns
            .iter()
            .filter_map(|turn| turn.get("commit_id").and_then(Value::as_str))
            .collect();
        require(
            turns.len() == 2
                && logged_ids.contains(&first_commit.as_str())
                && logged_ids.contains(&second_commit.as_str()),
            format!("session log did not report both commits: {logged_ids:?}"),
        )?;
        let human_log_stdout = String::from_utf8_lossy(&human_log.stdout);
        require(
            human_log.is_clean_exit(0)
                && human_log_stdout.contains("turn 0")
                && human_log_stdout.contains("first prompt"),
            format!("human session log omitted committed turn: {human_log_stdout}"),
        )?;

        let status_stdout = String::from_utf8_lossy(&status_output.stdout);
        let status_json: Value = serde_json::from_str(status_stdout.trim()).map_err(|error| {
            AssertError::StdoutMissing {
                expected: format!("session status --json not JSON: {error}"),
            }
        })?;
        require(
            status_json.get("commit_count").and_then(Value::as_u64) == Some(2),
            format!("status commit_count != 2: {status_json}"),
        )?;
        require(
            status_json
                .pointer("/head/branch/name")
                .and_then(Value::as_str)
                == Some("main"),
            format!("status HEAD not branch main: {status_json}"),
        )?;

        // --- first-turn content unchanged after the second delta commit ---
        let first_log_stdout = String::from_utf8_lossy(&log_after_first.stdout);
        let first_log_json: Value =
            serde_json::from_str(first_log_stdout.trim()).map_err(|error| {
                AssertError::StdoutMissing {
                    expected: format!("first session log --json not JSON: {error}"),
                }
            })?;
        let first_entry_before = first_log_json
            .get("turns")
            .and_then(Value::as_array)
            .and_then(|turns| turns.first())
            .cloned();
        require(
            first_entry_before.is_some() && first_entry_before == turns.first().cloned(),
            "first-turn log entry changed after the second commit",
        )?;

        // --- missing session: exit 2 with no JSONL Turn events ---
        let missing_outcome = parse_send_stdout(&missing.stdout, missing.exit_code());
        require(
            missing_outcome.exit_code == Some(SendExitCode::InputError),
            format!(
                "missing session did not exit 2: code={:?} termination={:?}",
                missing.exit_code(),
                missing.termination
            ),
        )?;
        require(
            matches!(missing_outcome.events.as_slice(), [TurnEventLine::Unknown { type_name, raw }] if type_name == "error" && raw["kind"] == "not_found"),
            "missing session must emit only a typed lookup error, no Turn activity",
        )?;
        Ok(())
    })();

    finish_scenario(
        scenario_evidence,
        &[
            ("first session send", &first.command),
            ("session log after first turn", &log_after_first),
            ("second session send", &second.command),
            ("session log after second turn", &log_after_second),
            ("human session log after second turn", &human_log),
            ("session status --json", &status_output),
            ("missing-session send", &missing),
        ],
        captures,
        result,
    );
}

/// 凭据冻结发生在接受前；路由仍由公开 resolve 验证，不为观察事件运行 provider。
#[test]
fn direct_turn_sonnet_routes_but_missing_credentials_reject_before_acceptance() {
    let driver = driver();
    let spec = ScenarioSpec::direct_local("direct-turn-sonnet-route");
    let world = ScenarioWorld::new(&spec.name).expect("world");
    world.write_agent_profile("dl-sonnet", "sonnet").unwrap();
    world.init_git_project().unwrap();
    let scenario_evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-sonnet", "main").unwrap();
    let resolved = send.run_command(&["--json", "resolve", "sonnet"]).unwrap();
    let outcome = send.send("route sonnet").unwrap();
    let status = send
        .run_command(&["--json", "session", "status", send.session_id()])
        .unwrap();
    let id = outcome
        .events
        .iter()
        .find_map(|event| match event {
            TurnEventLine::Unknown { type_name, raw } if type_name == "submission" => {
                raw["turn_id"].as_str()
            }
            _ => None,
        })
        .expect("preallocated request identity");
    let query = send
        .run_command(&["--json", "session", "turn", send.session_id(), id])
        .unwrap();
    let result = (|| {
        assert_clean_send(&outcome)?;
        let routing: Value = serde_json::from_slice(&resolved.stdout).unwrap();
        require(
            resolved.is_clean_exit(0)
                && routing["provider"] == "Anthropic"
                && routing["alias"] == "sonnet"
                && routing["api_key_env"] == "ANTHROPIC_API_KEY"
                && routing["credential_status"] == "Missing",
            "sonnet resolves to Anthropic with missing credentials",
        )?;
        require(
            outcome.command.is_clean_exit(2)
                && outcome.accepted_turn_id().is_none()
                && outcome.committed_result().is_none(),
            "missing credential rejects before durable acceptance",
        )?;
        require(outcome.events.iter().all(|event| matches!(event, TurnEventLine::Unknown { type_name, raw } if type_name == "submission" || (type_name == "error" && raw["kind"] == "bad_request"))), "no model or Tool activity before credential rejection")?;
        let record: Value = serde_json::from_str(
            std::str::from_utf8(&query.stdout)
                .unwrap()
                .lines()
                .next()
                .unwrap(),
        )
        .unwrap();
        require(
            query.is_clean_exit(0)
                && record["view"]["active_turn"].is_null()
                && record["view"]["requested_turn"].is_null(),
            "credential rejection must not leave active or pinned Turn",
        )?;
        let status_json: Value = serde_json::from_slice(&status.stdout).unwrap();
        require(
            status.is_clean_exit(0) && status_json["commit_count"] == 0,
            "credential rejection leaves no committed Turn",
        )
    })();
    finish_scenario(
        scenario_evidence,
        &[
            ("resolve sonnet", &resolved),
            ("reject missing credentials", &outcome.command),
            ("status after rejection", &status),
            ("query rejected identity", &query),
        ],
        Vec::new(),
        result,
    );
}

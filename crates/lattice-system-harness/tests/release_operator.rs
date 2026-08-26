// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Release-binary operator matrix over the authoritative Git InvocationStore.
//!
//! This target is feature-gated and executed only by `cargo xtask rollout`.

mod support;

use std::collections::HashSet;
use std::process::Output;
use std::time::Duration;

use lattice_system_harness::{
    AssertError, CommandOutcome, FixtureHttpServer, HttpFixture, SendExecution, SessionSendDriver,
    SseScript, TurnEventLine,
};
use serde_json::Value;
use support::{
    assert_clean_send, cli, direct_world, driver, evidence, finish_scenario, require, sequence,
    tool_call, wire_fake_llm,
};

fn parse_json(output: &Output) -> Value {
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(stdout.trim())
        .unwrap_or_else(|error| panic!("expected JSON output, got {stdout:?}: {error}"))
}

fn rows(value: &Value) -> &[Value] {
    value
        .get("rows")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_else(|| panic!("expected rows: {value}"))
}

fn replay_calls(value: &Value) -> &[Value] {
    value
        .pointer("/trace/calls")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_else(|| panic!("expected trace.calls: {value}"))
}

fn method(row: &Value) -> Option<&str> {
    row.pointer("/record/binding/method")
        .and_then(Value::as_str)
}

fn invocation_id(row: &Value) -> Option<&str> {
    row.get("invocation_id").and_then(Value::as_str)
}

fn terminal_succeeded(row: &Value) -> bool {
    row.pointer("/record/state/terminal/outcome")
        .and_then(Value::as_str)
        == Some("succeeded")
}

fn row_matches(row: &Value, session_id: &str, turn_id: &str, expected_method: &str) -> bool {
    invocation_id(row).is_some_and(|id| id.len() == 36)
        && method(row) == Some(expected_method)
        && row
            .pointer("/record/binding/session_id")
            .and_then(Value::as_str)
            == Some(session_id)
        && row
            .pointer("/record/binding/turn_id")
            .and_then(Value::as_str)
            == Some(turn_id)
        && row
            .pointer("/record/binding/descriptor_identity")
            .and_then(Value::as_str)
            .is_some_and(|digest| digest.len() == 64)
        && row
            .pointer("/record/binding/arguments_digest")
            .and_then(Value::as_str)
            .is_some_and(|digest| digest.len() == 64)
        && terminal_succeeded(row)
        && row
            .pointer("/record/state/terminal/evidence/admission/tool")
            .and_then(Value::as_str)
            == Some(expected_method)
        && row.get("metadata_json").is_none()
}

fn turn_id_of(turn: &SendExecution) -> String {
    turn.events
        .iter()
        .find_map(|event| match event {
            TurnEventLine::TurnStarted { turn_id, .. } => Some(turn_id.clone()),
            _ => None,
        })
        .expect("turn id")
}

fn fetch_network_profile() -> String {
    "\n[lancet.network]\nallow_localhost = true\nallowed_hosts = [\"localhost\"]\nmax_web_response_bytes = 1024\n".to_string()
}

fn assert_single_release_case(
    world: &lattice_system_harness::ScenarioWorld,
    spec: &lattice_system_harness::ScenarioSpec,
    session_id: &str,
    turn_id: &str,
    expected_method: &str,
) -> Result<(), AssertError> {
    let query = cli(
        world,
        spec,
        &[
            "--json",
            "audit",
            "query",
            "--session",
            session_id,
            "--turn",
            turn_id,
            "--method",
            expected_method,
            "--verdict",
            "succeeded",
        ],
    );
    require(
        query.status.success(),
        format!(
            "release {expected_method} query failed: {}",
            String::from_utf8_lossy(&query.stderr)
        ),
    )?;
    let query_json = parse_json(&query);
    let current = rows(&query_json);
    require(
        current.len() == 1 && row_matches(&current[0], session_id, turn_id, expected_method),
        format!("release {expected_method} query mismatch: {query_json}"),
    )?;
    let invocation = invocation_id(&current[0]).expect("invocation id");
    let tip = query_json
        .get("ledger_oid")
        .and_then(Value::as_str)
        .expect("ledger oid");

    let pinned = cli(
        world,
        spec,
        &[
            "--json",
            "audit",
            "query",
            "--session",
            session_id,
            "--at",
            tip,
        ],
    );
    require(pinned.status.success(), "release pinned query failed")?;
    let pinned_json = parse_json(&pinned);
    require(
        pinned_json.get("ledger_oid").and_then(Value::as_str) == Some(tip)
            && rows(&pinned_json)
                .first()
                .and_then(|row| invocation_id(row))
                == Some(invocation),
        format!("release pinned query mismatch: {pinned_json}"),
    )?;

    let tail = cli(
        world,
        spec,
        &[
            "--json",
            "audit",
            "query",
            "--session",
            session_id,
            "--turn",
            turn_id,
            "--method",
            expected_method,
            "--tail",
        ],
    );
    require(tail.status.success(), "release tail failed")?;
    let tail_json = parse_json(&tail);
    let transitions = rows(&tail_json);
    require(
        transitions.len() == 3
            && transitions.iter().all(|row| {
                invocation_id(row) == Some(invocation)
                    && method(row) == Some(expected_method)
                    && row
                        .get("ledger_oid")
                        .and_then(Value::as_str)
                        .is_some_and(|oid| oid.len() == 40)
            })
            && terminal_succeeded(&transitions[2]),
        format!("release {expected_method} tail mismatch: {tail_json}"),
    )?;
    let cursor = tail_json
        .get("last_cursor")
        .and_then(Value::as_str)
        .expect("last cursor");
    let continuation = cli(
        world,
        spec,
        &[
            "--json",
            "audit",
            "query",
            "--session",
            session_id,
            "--tail",
            "--since",
            cursor,
        ],
    );
    require(
        continuation.status.success(),
        "release tail continuation failed",
    )?;
    let continuation_json = parse_json(&continuation);
    require(
        rows(&continuation_json).is_empty()
            && continuation_json.get("last_cursor").and_then(Value::as_str) == Some(cursor)
            && continuation_json.get("done").and_then(Value::as_bool) == Some(true),
        format!("release tail continuation mismatch: {continuation_json}"),
    )?;

    let replay = cli(
        world,
        spec,
        &[
            "--json",
            "audit",
            "replay",
            "--session",
            session_id,
            "--turn",
            turn_id,
        ],
    );
    require(replay.status.success(), "release replay failed")?;
    let replay_json = parse_json(&replay);
    let calls = replay_calls(&replay_json);
    require(
        replay_json
            .pointer("/trace/ledger_oid")
            .and_then(Value::as_str)
            == Some(tip)
            && calls.len() == 1
            && invocation_id(&calls[0]) == Some(invocation)
            && row_matches(&calls[0], session_id, turn_id, expected_method),
        format!("release replay mismatch: {replay_json}"),
    )?;

    let status = cli(world, spec, &["--json", "lancet", "status"]);
    require(status.status.success(), "release status failed")?;
    let status_json = parse_json(&status);
    let recent = status_json
        .get("recent_calls")
        .and_then(Value::as_array)
        .expect("recent calls");
    require(
        status_json.get("ledger_oid").and_then(Value::as_str) == Some(tip)
            && status_json.get("last_verdict").and_then(Value::as_str) == Some("succeeded")
            && recent.len() <= 20
            && recent
                .iter()
                .find(|row| method(row) == Some(expected_method))
                .is_some_and(|row| row_matches(row, session_id, turn_id, expected_method)),
        format!("release status mismatch: {status_json}"),
    )?;
    Ok(())
}

fn release_native_case() {
    let driver = driver();
    let web = FixtureHttpServer::start(HttpFixture::plain("release native fixture"))
        .expect("fixture HTTP server");
    let fetch_url = web.url_path("/release-native");
    let fetch_args = format!(r#"{{"url":"{fetch_url}"}}"#);
    let (world, spec) = direct_world(
        "release-operator-native",
        "release-op-native",
        "[\"fetch.web\"]",
        &fetch_network_profile(),
    );
    let server = wire_fake_llm(
        &world,
        "release-op-native",
        sequence(vec![
            tool_call("release-native", "fetch.web", &fetch_args),
            SseScript::plain("release native complete"),
        ]),
    );
    let scenario_evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "release-op-native", "main")
        .expect("session");
    let session_id = send.session_id().to_string();
    let turn = send.send("release native matrix").expect("native turn");
    let turn_id = turn_id_of(&turn);
    let captures = server.captured_requests();
    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&turn)?;
        require(
            turn.has_committed_completed(),
            "release native turn must complete",
        )?;
        web.wait_for_requests(1, Duration::from_secs(5))
            .map_err(|error| AssertError::StdoutMissing {
                expected: format!("release native fetch missing: {error}"),
            })?;
        assert_single_release_case(&world, &spec, &session_id, &turn_id, "fetch.web")
    })();
    finish_scenario(
        scenario_evidence,
        &[("release native matrix", &turn.command)],
        captures,
        result,
    );
}

fn release_component_case() {
    let driver = driver();
    let (world, spec) = direct_world(
        "release-operator-component",
        "release-op-component",
        "[\"session.todo_write\"]",
        "",
    );
    let server = wire_fake_llm(
        &world,
        "release-op-component",
        sequence(vec![
            tool_call(
                "release-component",
                "session.todo_write",
                r#"{"todos":[{"content":"release component","status":"pending"}]}"#,
            ),
            SseScript::plain("release component complete"),
        ]),
    );
    let scenario_evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "release-op-component", "main")
        .expect("session");
    let session_id = send.session_id().to_string();
    let turn = send
        .send("release Component matrix")
        .expect("Component turn");
    let turn_id = turn_id_of(&turn);
    let captures = server.captured_requests();
    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&turn)?;
        require(
            turn.has_committed_completed(),
            "release Component turn must complete",
        )?;
        assert_single_release_case(&world, &spec, &session_id, &turn_id, "session.todo_write")
    })();
    finish_scenario(
        scenario_evidence,
        &[("release Component matrix", &turn.command)],
        captures,
        result,
    );
}

fn release_mixed_case() {
    let driver = driver();
    let web = FixtureHttpServer::start(HttpFixture::plain("release mixed fixture"))
        .expect("fixture HTTP server");
    let fetch_url = web.url_path("/release-mixed");
    let fetch_args = format!(r#"{{"url":"{fetch_url}"}}"#);
    let (world, spec) = direct_world(
        "release-operator-mixed",
        "release-op-mixed",
        "[\"fetch.web\",\"session.todo_write\"]",
        &fetch_network_profile(),
    );
    let mut scripts = Vec::new();
    for index in 0..11 {
        scripts.push(tool_call(
            &format!("release-mixed-native-{index}"),
            "fetch.web",
            &fetch_args,
        ));
        scripts.push(tool_call(
            &format!("release-mixed-component-{index}"),
            "session.todo_write",
            &format!(r#"{{"todos":[{{"content":"release mixed {index}","status":"pending"}}]}}"#),
        ));
        scripts.push(SseScript::plain(format!("release mixed {index} complete")));
    }
    let server = wire_fake_llm(&world, "release-op-mixed", sequence(scripts));
    let scenario_evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "release-op-mixed", "main")
        .expect("session");
    let session_id = send.session_id().to_string();
    let turns: Vec<_> = (0..11)
        .map(|index| {
            send.send(&format!("release mixed turn {index}"))
                .expect("mixed turn")
        })
        .collect();
    let last_turn_id = turn_id_of(turns.last().expect("last turn"));
    let captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        for turn in &turns {
            assert_clean_send(turn)?;
            require(
                turn.has_committed_completed(),
                "release mixed turn must complete",
            )?;
        }
        web.wait_for_requests(11, Duration::from_secs(5))
            .map_err(|error| AssertError::StdoutMissing {
                expected: format!("release mixed fetch count mismatch: {error}"),
            })?;

        let query = cli(
            &world,
            &spec,
            &["--json", "audit", "query", "--session", &session_id],
        );
        require(query.status.success(), "release mixed query failed")?;
        let query_json = parse_json(&query);
        let current = rows(&query_json);
        require(
            current.len() == 22,
            format!("expected 22 rows: {query_json}"),
        )?;
        let all_ids: HashSet<&str> = current.iter().filter_map(invocation_id).collect();
        require(
            all_ids.len() == 22
                && current.iter().all(|row| {
                    let row_method = method(row).expect("method");
                    terminal_succeeded(row)
                        && row
                            .pointer("/record/state/terminal/evidence/admission/tool")
                            .and_then(Value::as_str)
                            == Some(row_method)
                }),
            format!("release mixed row mismatch: {query_json}"),
        )?;

        let replay = cli(
            &world,
            &spec,
            &[
                "--json",
                "audit",
                "replay",
                "--session",
                &session_id,
                "--turn",
                &last_turn_id,
            ],
        );
        require(replay.status.success(), "release mixed replay failed")?;
        let replay_json = parse_json(&replay);
        require(
            replay_calls(&replay_json)
                .iter()
                .filter_map(method)
                .collect::<Vec<_>>()
                == vec!["fetch.web", "session.todo_write"],
            format!("release mixed replay order mismatch: {replay_json}"),
        )?;

        let status = cli(&world, &spec, &["--json", "lancet", "status"]);
        require(status.status.success(), "release mixed status failed")?;
        let status_json = parse_json(&status);
        let recent = status_json
            .get("recent_calls")
            .and_then(Value::as_array)
            .expect("recent calls");
        let recent_ids: HashSet<&str> = recent.iter().filter_map(invocation_id).collect();
        require(
            recent.len() == 20
                && recent_ids.len() == 20
                && recent_ids.is_subset(&all_ids)
                && all_ids.difference(&recent_ids).count() == 2
                && recent
                    .iter()
                    .filter(|row| method(row) == Some("fetch.web"))
                    .count()
                    == 10
                && recent
                    .iter()
                    .filter(|row| method(row) == Some("session.todo_write"))
                    .count()
                    == 10
                && recent.iter().all(terminal_succeeded)
                && status_json.get("last_verdict").and_then(Value::as_str) == Some("succeeded"),
            format!("release mixed bounded status mismatch: {status_json}"),
        )?;
        Ok(())
    })();

    let summaries: Vec<String> = (0..turns.len())
        .map(|index| format!("release mixed turn {index}"))
        .collect();
    let commands: Vec<(&str, &CommandOutcome)> = summaries
        .iter()
        .zip(&turns)
        .map(|(summary, turn)| (summary.as_str(), &turn.command))
        .collect();
    finish_scenario(scenario_evidence, &commands, captures, result);
}

#[test]
fn release_operator_native_component_mixed_matrix() {
    release_native_case();
    release_component_case();
    release_mixed_case();
}

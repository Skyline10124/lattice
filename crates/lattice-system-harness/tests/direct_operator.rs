// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Direct-Local operator paths over the authoritative Git InvocationStore.

mod support;

use std::collections::HashSet;
use std::time::Duration;

use lattice_system_harness::{
    AssertError, CommandOutcome, FixtureHttpServer, HttpFixture, SendExecution, SessionSendDriver,
    SseScript,
};
use serde_json::Value;
use support::{
    assert_clean_send, cli, direct_world, driver, evidence, finish_scenario, require, sequence,
    tool_call, wire_fake_llm,
};

fn parse_json(output: &CommandOutcome) -> Value {
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

fn terminal_evidence_matches(row: &Value, expected_method: &str) -> bool {
    row.pointer("/record/state/terminal/evidence/admission/tool")
        .and_then(Value::as_str)
        == Some(expected_method)
}

fn current_row_matches(
    row: &Value,
    session_id: &str,
    turn_id: &str,
    expected_method: &str,
) -> bool {
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
        && terminal_evidence_matches(row, expected_method)
}

fn row_for_method<'a>(rows: &'a [Value], expected_method: &str) -> &'a Value {
    rows.iter()
        .find(|row| method(row) == Some(expected_method))
        .unwrap_or_else(|| panic!("missing {expected_method}: {rows:?}"))
}

fn methods(rows: &[Value]) -> Vec<&str> {
    rows.iter().filter_map(method).collect()
}

fn turn_id_of(turn: &SendExecution) -> String {
    // 有损观察通道可能淘汰 `turn_started`；请求身份以持久接受回执为准。
    turn.accepted_turn_id()
        .expect("durable acceptance acknowledgement")
        .to_string()
}

fn fetch_network_profile() -> String {
    "\n[lancet.network]\nallow_localhost = true\nallowed_hosts = [\"localhost\"]\nmax_web_response_bytes = 1024\n\n[lancet.capabilities]\nmax_boundary = \"ex\"\n".to_string()
}

fn assert_single_operator_case(
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
        query.is_clean_exit(0),
        format!(
            "{expected_method} query failed: {}",
            String::from_utf8_lossy(&query.stderr)
        ),
    )?;
    let query_json = parse_json(&query);
    let query_rows = rows(&query_json);
    require(
        query_json.get("row_count").and_then(Value::as_u64) == Some(1)
            && query_rows.len() == 1
            && current_row_matches(&query_rows[0], session_id, turn_id, expected_method),
        format!("{expected_method} query mismatch: {query_json}"),
    )?;
    let invocation = invocation_id(&query_rows[0]).expect("invocation id");
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
    require(pinned.is_clean_exit(0), "pinned query failed")?;
    let pinned_json = parse_json(&pinned);
    require(
        pinned_json.get("ledger_oid").and_then(Value::as_str) == Some(tip)
            && rows(&pinned_json)
                .first()
                .and_then(|row| invocation_id(row))
                == Some(invocation),
        format!("pinned query mismatch: {pinned_json}"),
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
    require(tail.is_clean_exit(0), "tail failed")?;
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
            && transitions[0]
                .pointer("/record/state")
                .and_then(Value::as_str)
                == Some("reserved")
            && transitions[1]
                .pointer("/record/state")
                .and_then(Value::as_str)
                == Some("dispatch_started")
            && terminal_succeeded(&transitions[2]),
        format!("{expected_method} tail mismatch: {tail_json}"),
    )?;
    let cursor = tail_json
        .get("last_cursor")
        .and_then(Value::as_str)
        .expect("tail cursor");
    let continued = cli(
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
    require(continued.is_clean_exit(0), "tail continuation failed")?;
    let continued_json = parse_json(&continued);
    require(
        rows(&continued_json).is_empty()
            && continued_json.get("last_cursor").and_then(Value::as_str) == Some(cursor)
            && continued_json.get("done").and_then(Value::as_bool) == Some(true),
        format!("exclusive tail cursor mismatch: {continued_json}"),
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
    require(replay.is_clean_exit(0), "replay failed")?;
    let replay_json = parse_json(&replay);
    let calls = replay_calls(&replay_json);
    require(
        replay_json
            .pointer("/trace/ledger_oid")
            .and_then(Value::as_str)
            == Some(tip)
            && calls.len() == 1
            && invocation_id(&calls[0]) == Some(invocation)
            && current_row_matches(&calls[0], session_id, turn_id, expected_method),
        format!("{expected_method} replay mismatch: {replay_json}"),
    )?;

    let status = cli(world, spec, &["--json", "lancet", "status"]);
    require(status.is_clean_exit(0), "lancet status failed")?;
    let status_json = parse_json(&status);
    let recent = status_json
        .get("recent_calls")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("status recent_calls: {status_json}"));
    require(
        status_json.get("ledger_oid").and_then(Value::as_str) == Some(tip)
            && status_json.get("last_verdict").and_then(Value::as_str) == Some("succeeded")
            && recent.len() <= 20
            && current_row_matches(
                row_for_method(recent, expected_method),
                session_id,
                turn_id,
                expected_method,
            ),
        format!("{expected_method} status mismatch: {status_json}"),
    )?;
    Ok(())
}

#[test]
fn direct_operator_fetch_terminal_contract() {
    let driver = driver();
    let web = FixtureHttpServer::start(HttpFixture::plain("fetch operator fixture"))
        .expect("fixture HTTP server");
    let fetch_url = web.url_path("/fetch-operator");
    let (world, spec) = direct_world(
        "direct-operator-fetch",
        "dl-op-fetch",
        "[\"fetch.web\"]",
        &fetch_network_profile(),
    );
    let fetch_args = format!(r#"{{"url":"{fetch_url}"}}"#);
    let server = wire_fake_llm(
        &world,
        "dl-op-fetch",
        sequence(vec![
            tool_call("n-only", "fetch.web", &fetch_args),
            SseScript::plain("fetch complete"),
        ]),
    );
    let scenario_evidence = evidence(&world, &spec);
    let send =
        SessionSendDriver::new(&driver, &world, &spec, "dl-op-fetch", "main").expect("session");
    let session_id = send.session_id().to_string();
    let turn = send.send("run fetch operator fixture").expect("fetch turn");
    let turn_id = turn_id_of(&turn);
    let captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&turn)?;
        require(turn.has_committed_completed(), "fetch turn must complete")?;
        web.wait_for_requests(1, Duration::from_secs(5))
            .map_err(|error| AssertError::StdoutMissing {
                expected: format!("fetch fixture did not receive a request: {error}"),
            })?;
        assert_single_operator_case(&world, &spec, &session_id, &turn_id, "fetch.web")
    })();
    finish_scenario(
        scenario_evidence,
        &[("fetch operator turn", &turn.command)],
        captures,
        result,
    );
}

#[test]
fn direct_operator_component_terminal_contract() {
    let driver = driver();
    let (world, spec) = direct_world(
        "direct-operator-component",
        "dl-op-component",
        "[\"session.todo_write\"]",
        "",
    );
    let server = wire_fake_llm(
        &world,
        "dl-op-component",
        sequence(vec![
            tool_call(
                "c-only",
                "session.todo_write",
                r#"{"todos":[{"content":"component operator","status":"pending"}]}"#,
            ),
            SseScript::plain("component complete"),
        ]),
    );
    let scenario_evidence = evidence(&world, &spec);
    let send =
        SessionSendDriver::new(&driver, &world, &spec, "dl-op-component", "main").expect("session");
    let session_id = send.session_id().to_string();
    let turn = send
        .send("run Component operator fixture")
        .expect("Component turn");
    let turn_id = turn_id_of(&turn);
    let captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&turn)?;
        require(
            turn.has_committed_completed(),
            "Component turn must complete",
        )?;
        assert_single_operator_case(&world, &spec, &session_id, &turn_id, "session.todo_write")
    })();
    finish_scenario(
        scenario_evidence,
        &[("Component operator turn", &turn.command)],
        captures,
        result,
    );
}

#[test]
fn direct_operator_mixed_query_tail_replay_and_status() {
    let driver = driver();
    let web = FixtureHttpServer::start(HttpFixture::plain("operator fetch fixture"))
        .expect("fixture HTTP server");
    let fetch_url = web.url_path("/operator-matrix");
    let fetch_args = format!(r#"{{"url":"{fetch_url}"}}"#);
    let (world, spec) = direct_world(
        "direct-operator-cross",
        "dl-op",
        "[\"fetch.web\",\"session.todo_write\"]",
        &fetch_network_profile(),
    );
    let server = wire_fake_llm(
        &world,
        "dl-op",
        sequence(vec![
            tool_call("n1", "fetch.web", &fetch_args),
            tool_call(
                "c1",
                "session.todo_write",
                r#"{"todos":[{"content":"operator matrix","status":"pending"}]}"#,
            ),
            SseScript::plain("mixed complete"),
        ]),
    );
    let scenario_evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-op", "main").expect("session");
    let session_id = send.session_id().to_string();
    let turn = send.send("run mixed operator matrix").expect("mixed turn");
    let turn_id = turn_id_of(&turn);
    let captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&turn)?;
        require(turn.has_committed_completed(), "mixed turn must complete")?;
        web.wait_for_requests(1, Duration::from_secs(5))
            .map_err(|error| AssertError::StdoutMissing {
                expected: format!("fetch fixture did not receive a request: {error}"),
            })?;

        let query = cli(
            &world,
            &spec,
            &["--json", "audit", "query", "--session", &session_id],
        );
        require(query.is_clean_exit(0), "mixed query failed")?;
        let query_json = parse_json(&query);
        let current = rows(&query_json);
        require(
            current.len() == 2
                && methods(current) == vec!["fetch.web", "session.todo_write"]
                && current.iter().all(|row| {
                    let row_method = method(row).expect("method");
                    current_row_matches(row, &session_id, &turn_id, row_method)
                }),
            format!("mixed query mismatch: {query_json}"),
        )?;

        let tail = cli(
            &world,
            &spec,
            &[
                "--json",
                "audit",
                "query",
                "--session",
                &session_id,
                "--turn",
                &turn_id,
                "--tail",
            ],
        );
        require(tail.is_clean_exit(0), "mixed tail failed")?;
        let tail_json = parse_json(&tail);
        let transitions = rows(&tail_json);
        require(
            transitions.len() == 6
                && transitions
                    .iter()
                    .filter(|row| method(row) == Some("fetch.web"))
                    .count()
                    == 3
                && transitions
                    .iter()
                    .filter(|row| method(row) == Some("session.todo_write"))
                    .count()
                    == 3,
            format!("mixed tail mismatch: {tail_json}"),
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
                &turn_id,
            ],
        );
        require(replay.is_clean_exit(0), "mixed replay failed")?;
        let replay_json = parse_json(&replay);
        require(
            methods(replay_calls(&replay_json)) == vec!["fetch.web", "session.todo_write"],
            format!("mixed replay order mismatch: {replay_json}"),
        )?;

        let status = cli(&world, &spec, &["--json", "lancet", "status"]);
        require(status.is_clean_exit(0), "mixed status failed")?;
        let status_json = parse_json(&status);
        let recent = status_json
            .get("recent_calls")
            .and_then(Value::as_array)
            .expect("recent calls");
        require(
            recent.len() == 2
                && row_for_method(recent, "fetch.web")
                    .pointer("/record/state/terminal/evidence/admission/tool")
                    .and_then(Value::as_str)
                    == Some("fetch.web")
                && row_for_method(recent, "session.todo_write")
                    .pointer("/record/state/terminal/evidence/admission/tool")
                    .and_then(Value::as_str)
                    == Some("session.todo_write")
                && status_json.get("last_verdict").and_then(Value::as_str) == Some("succeeded"),
            format!("mixed status mismatch: {status_json}"),
        )?;
        Ok(())
    })();
    finish_scenario(
        scenario_evidence,
        &[("mixed component turn", &turn.command)],
        captures,
        result,
    );
}

#[test]
fn direct_operator_status_bounds_twenty_newest_invocations() {
    let driver = driver();
    let web = FixtureHttpServer::start(HttpFixture::plain("bounded status fixture"))
        .expect("fixture HTTP server");
    let fetch_url = web.url_path("/bounded-status");
    let fetch_args = format!(r#"{{"url":"{fetch_url}"}}"#);
    let (world, mut spec) = direct_world(
        "direct-operator-status-bound",
        "dl-op-bound",
        "[\"fetch.web\",\"session.todo_write\"]",
        &fetch_network_profile(),
    );
    // One send durably finalizes 22 Component invocations.
    spec.step_timeout_secs = 60;
    let mut calls = Vec::new();
    for index in 0..11 {
        calls.push((
            format!("bound-fetch-{index}"),
            "fetch.web".to_string(),
            fetch_args.clone(),
        ));
        calls.push((
            format!("bound-component-{index}"),
            "session.todo_write".to_string(),
            format!(r#"{{"todos":[{{"content":"bounded {index}","status":"pending"}}]}}"#),
        ));
    }
    let server = wire_fake_llm(
        &world,
        "dl-op-bound",
        sequence(vec![
            SseScript::ToolCalls { calls },
            SseScript::plain("bounded turn complete"),
        ]),
    );
    let scenario_evidence = evidence(&world, &spec);
    let send =
        SessionSendDriver::new(&driver, &world, &spec, "dl-op-bound", "main").expect("session");
    let session_id = send.session_id().to_string();
    let turn = send.send("run bounded turn").expect("turn");
    let captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&turn)?;
        require(turn.has_committed_completed(), "bounded turn must complete")?;
        web.wait_for_requests(11, Duration::from_secs(5))
            .map_err(|error| AssertError::StdoutMissing {
                expected: format!("fetch fixture did not receive 11 requests: {error}"),
            })?;

        let query = cli(
            &world,
            &spec,
            &["--json", "audit", "query", "--session", &session_id],
        );
        require(query.is_clean_exit(0), "bounded query failed")?;
        let query_json = parse_json(&query);
        let all = rows(&query_json);
        require(all.len() == 22, format!("expected 22 rows: {query_json}"))?;

        let status = cli(&world, &spec, &["--json", "lancet", "status"]);
        require(status.is_clean_exit(0), "bounded status failed")?;
        let status_json = parse_json(&status);
        let recent = status_json
            .get("recent_calls")
            .and_then(Value::as_array)
            .expect("recent calls");
        let recent_ids: HashSet<&str> = recent.iter().filter_map(invocation_id).collect();
        let all_ids: HashSet<&str> = all.iter().filter_map(invocation_id).collect();
        require(
            recent.len() == 20
                && recent_ids.len() == 20
                && all_ids.len() == 22
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
            format!("bounded status mismatch: {status_json}"),
        )?;
        Ok(())
    })();

    finish_scenario(
        scenario_evidence,
        &[("bounded turn", &turn.command)],
        captures,
        result,
    );
}

#[test]
fn direct_operator_unknown_typed_session_is_empty() {
    let driver = driver();
    let (world, spec) = direct_world("direct-operator-unknown-sess", "dl-op-unk", "[]", "");
    let _server = wire_fake_llm(&world, "dl-op-unk", SseScript::plain("seed"));
    let send =
        SessionSendDriver::new(&driver, &world, &spec, "dl-op-unk", "main").expect("session");
    let seed = send.send("seed turn").expect("seed");
    assert_clean_send(&seed).expect("seed clean");
    let scenario_evidence = evidence(&world, &spec);
    let query = cli(
        &world,
        &spec,
        &[
            "--json",
            "audit",
            "query",
            "--session",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ],
    );
    let result: Result<(), AssertError> = (|| {
        require(query.is_clean_exit(0), "unknown session query must succeed")?;
        let json = parse_json(&query);
        require(
            json.get("row_count").and_then(Value::as_u64) == Some(0),
            format!("unknown session must return no rows: {json}"),
        )?;
        Ok(())
    })();
    finish_scenario(
        scenario_evidence,
        &[("unknown session query", &seed.command)],
        Vec::new(),
        result,
    );
}

#[test]
fn direct_operator_unknown_typed_turn_fails() {
    let driver = driver();
    let (world, spec) = direct_world("direct-operator-unknown-turn", "dl-op-ut", "[]", "");
    let _server = wire_fake_llm(&world, "dl-op-ut", SseScript::plain("seed"));
    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-op-ut", "main").expect("session");
    let seed = send.send("seed turn").expect("seed");
    assert_clean_send(&seed).expect("seed clean");
    let session_id = send.session_id().to_string();
    let scenario_evidence = evidence(&world, &spec);
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
            "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
        ],
    );
    let result: Result<(), AssertError> = (|| {
        require(
            replay.clean_exit_code().is_some_and(|code| code != 0),
            "unknown turn replay must fail",
        )?;
        // `--json` 的错误以 typed error record 走 stdout（见 report_error），stderr 保持为空。
        let stdout = String::from_utf8_lossy(&replay.stdout);
        let error: Value = serde_json::from_str(stdout.trim()).unwrap_or(Value::Null);
        require(
            error.get("type").and_then(Value::as_str) == Some("error"),
            format!("unknown turn replay must emit a typed error record: {stdout:?}"),
        )?;
        // 具体原因保持不变，但请求的 TurnId 不回显（lattice-cli audit 的前缀白名单）。
        require(
            error
                .get("message")
                .and_then(Value::as_str)
                .is_some_and(|message| message.contains("no invocations for the requested turn")),
            format!("unexpected replay diagnostic: {error}"),
        )?;
        require(
            !stdout.contains("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"),
            format!("replay diagnostic must not echo the requested TurnId: {stdout:?}"),
        )?;
        Ok(())
    })();
    finish_scenario(
        scenario_evidence,
        &[
            ("seed turn send", &seed.command),
            ("unknown turn replay", &replay),
        ],
        Vec::new(),
        result,
    );
}

// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Direct-Local operator paths: audit query/replay, lancet status, session
//! status/log cross-validation (issue #46 (Codeberg #24) slice 6).
//!
//! A single real turn emits native `fetch.web` and Component
//! `session.todo_write` calls. The scenario checks their individual and mixed
//! query/tail/replay/status representations through the public binary.

mod support;

use std::process::Output;
use std::time::Duration;

use lattice_system_harness::{
    AssertError, FixtureHttpServer, HttpFixture, SendExecution, SessionSendDriver, SseScript,
    TurnEventLine,
};
use serde_json::Value;
use support::{
    assert_clean_send, cli, direct_world, driver, evidence, finish_scenario, require, sequence,
    tool_call, wire_fake_llm,
};

/// Parse JSON output (one object per command, not JSONL).
fn parse_json(out: &Output) -> Value {
    let s = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str(s.trim()).unwrap_or_else(|e| {
        panic!(
            "expected JSON output, got {:?}: {e}",
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

fn operator_rows(value: &Value) -> &[Value] {
    value
        .get("rows")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_else(|| panic!("expected operator rows: {value}"))
}

fn row_for_method<'a>(rows: &'a [Value], method: &str) -> &'a Value {
    rows.iter()
        .find(|row| row.get("method").and_then(Value::as_str) == Some(method))
        .unwrap_or_else(|| panic!("missing {method} row: {rows:?}"))
}

fn row_methods(rows: &[Value]) -> Vec<&str> {
    rows.iter()
        .filter_map(|row| row.get("method").and_then(Value::as_str))
        .collect()
}

/// Read the call rows embedded in an operator replay trace.
fn replay_calls(value: &Value) -> &[Value] {
    value
        .get("trace")
        .and_then(|trace| trace.get("calls"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_else(|| panic!("expected replay trace.calls: {value}"))
}

/// Extract the turn_id from a turn_started event in a SendExecution.
fn turn_id_of(outcome: &SendExecution) -> Option<String> {
    outcome.events.iter().find_map(|e| match e {
        TurnEventLine::TurnStarted { turn_id, .. } => Some(turn_id.clone()),
        _ => None,
    })
}

fn fetch_network_profile() -> String {
    "\n[lancet.network]\nallow_localhost = true\nallowed_hosts = [\"localhost\"]\nmax_web_response_bytes = 1024\n".to_string()
}

fn row_has_expected_evidence(row: &Value, expect_evidence: bool) -> bool {
    if expect_evidence {
        row.pointer("/evidence/outcome").and_then(Value::as_str) == Some("completed")
    } else {
        row.get("evidence").is_some_and(Value::is_null)
    }
}

struct SingleOperatorExpectation<'a> {
    session_id: &'a str,
    turn_id: &'a str,
    method: &'a str,
    impact: &'a str,
    capability: Option<&'a str>,
    verdict: &'a str,
    expect_evidence: bool,
}

fn assert_single_operator_case(
    world: &lattice_system_harness::ScenarioWorld,
    spec: &lattice_system_harness::ScenarioSpec,
    expectation: SingleOperatorExpectation<'_>,
) -> Result<(), AssertError> {
    let SingleOperatorExpectation {
        session_id,
        turn_id,
        method,
        impact,
        capability,
        verdict,
        expect_evidence,
    } = expectation;
    let mut query_args = vec![
        "--json",
        "audit",
        "query",
        "--session",
        session_id,
        "--turn",
        turn_id,
        "--method",
        method,
        "--impact",
        impact,
        "--verdict",
        "success",
    ];
    if let Some(capability) = capability {
        query_args.extend(["--capability", capability]);
    }
    let query = cli(world, spec, &query_args);
    require(
        query.status.success(),
        format!(
            "{method} filtered query failed: {}",
            String::from_utf8_lossy(&query.stderr)
        ),
    )?;
    let query_json = parse_json(&query);
    let rows = operator_rows(&query_json);
    require(
        query_json.get("row_count").and_then(Value::as_u64) == Some(1)
            && rows.len() == 1
            && rows[0].get("method").and_then(Value::as_str) == Some(method)
            && row_has_expected_evidence(&rows[0], expect_evidence),
        format!("{method} query contract mismatch: {query_json}"),
    )?;
    let audit_id = rows[0]
        .get("audit_id")
        .and_then(Value::as_i64)
        .expect("query audit_id");

    // Query pagination uses the inclusive `--since` lower bound. The public
    // query remains capped by the adapter while preserving this row exactly.
    let cursor = audit_id.to_string();
    let page = cli(
        world,
        spec,
        &[
            "--json",
            "audit",
            "query",
            "--session",
            session_id,
            "--since",
            &cursor,
        ],
    );
    require(page.status.success(), format!("{method} query page failed"))?;
    let page_json = parse_json(&page);
    let page_rows = operator_rows(&page_json);
    require(
        page_json.get("row_count").and_then(Value::as_u64) == Some(1)
            && page_rows.len() == 1
            && page_rows[0].get("audit_id").and_then(Value::as_i64) == Some(audit_id),
        format!("{method} query page/limit mismatch: {page_json}"),
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
            "--tail",
            "--since",
            "0",
        ],
    );
    require(tail.status.success(), format!("{method} tail failed"))?;
    let tail_json = parse_json(&tail);
    let tail_rows = operator_rows(&tail_json);
    require(
        tail_rows.len() == 1
            && tail_rows[0].get("method").and_then(Value::as_str) == Some(method)
            && row_has_expected_evidence(&tail_rows[0], expect_evidence),
        format!("{method} tail contract mismatch: {tail_json}"),
    )?;

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
            &cursor,
        ],
    );
    require(
        continued.status.success(),
        format!("{method} tail continuation failed"),
    )?;
    let continued_json = parse_json(&continued);
    require(
        operator_rows(&continued_json).is_empty()
            && continued_json.get("last_audit_id").and_then(Value::as_i64) == Some(audit_id),
        format!("{method} exclusive tail cursor mismatch: {continued_json}"),
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
    require(replay.status.success(), format!("{method} replay failed"))?;
    let replay_json = parse_json(&replay);
    let calls = replay_calls(&replay_json);
    require(
        calls.len() == 1
            && calls[0].get("method").and_then(Value::as_str) == Some(method)
            && row_has_expected_evidence(&calls[0], expect_evidence),
        format!("{method} replay contract mismatch: {replay_json}"),
    )?;

    let status = cli(world, spec, &["--json", "lancet", "status"]);
    require(status.status.success(), format!("{method} status failed"))?;
    let status_json = parse_json(&status);
    let recent = status_json
        .get("recent_calls")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("status recent_calls: {status_json}"));
    let entry = row_for_method(recent, method);
    require(
        recent.len() <= 20
            && entry.get("verdict").and_then(Value::as_str) == Some(verdict)
            && entry.get("duration_ms").and_then(Value::as_i64).is_some()
            && row_has_expected_evidence(entry, expect_evidence),
        format!("{method} bounded status contract mismatch: {status_json}"),
    )?;

    Ok(())
}

#[test]
fn direct_operator_native_without_evidence_contract() {
    let driver = driver();
    let web = FixtureHttpServer::start(HttpFixture::plain("native operator fixture"))
        .expect("fixture HTTP server");
    let fetch_url = web.url_path("/native-operator");
    let (world, spec) = direct_world(
        "direct-operator-native",
        "dl-op-native",
        "[\"fetch.web\"]",
        &fetch_network_profile(),
    );
    let fetch_args = format!(r#"{{"url":"{fetch_url}"}}"#);
    let server = wire_fake_llm(
        &world,
        "dl-op-native",
        sequence(vec![
            tool_call("n-only", "fetch.web", &fetch_args),
            SseScript::plain("native complete"),
        ]),
    );
    let scenario_evidence = evidence(&world, &spec);
    let send =
        SessionSendDriver::new(&driver, &world, &spec, "dl-op-native", "main").expect("session");
    let sid = send.session_id().to_string();
    let turn = send
        .send("run native operator fixture")
        .expect("native turn");
    let turn_id = turn_id_of(&turn).expect("turn id");
    let captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&turn)?;
        require(turn.has_committed_completed(), "native turn must complete")?;
        web.wait_for_requests(1, Duration::from_secs(5))
            .map_err(|error| AssertError::StdoutMissing {
                expected: format!("fetch fixture did not receive a request: {error}"),
            })?;
        assert_single_operator_case(
            &world,
            &spec,
            SingleOperatorExpectation {
                session_id: &sid,
                turn_id: &turn_id,
                method: "fetch.web",
                impact: "external_observation",
                capability: Some("fetch_url"),
                verdict: "native_handler",
                expect_evidence: false,
            },
        )
    })();

    finish_scenario(
        scenario_evidence,
        &[("native operator turn", &turn.command)],
        captures,
        result,
    );
}

#[test]
fn direct_operator_component_with_evidence_contract() {
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
    let sid = send.session_id().to_string();
    let turn = send
        .send("run Component operator fixture")
        .expect("Component turn");
    let turn_id = turn_id_of(&turn).expect("turn id");
    let captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&turn)?;
        require(
            turn.has_committed_completed(),
            "Component turn must complete",
        )?;
        assert_single_operator_case(
            &world,
            &spec,
            SingleOperatorExpectation {
                session_id: &sid,
                turn_id: &turn_id,
                method: "session.todo_write",
                impact: "unknown",
                capability: None,
                verdict: "component_handler",
                expect_evidence: true,
            },
        )
    })();

    finish_scenario(
        scenario_evidence,
        &[("Component operator turn", &turn.command)],
        captures,
        result,
    );
}

/// N/C/M public-binary matrix: native fetch has no Cell evidence, the
/// Component call has table-backed evidence, and every operator surface keeps
/// both records in its ordinary finalized-row collection.
#[test]
fn direct_operator_audit_session_lancet_cross_validate() {
    let driver = driver();
    let web = FixtureHttpServer::start(HttpFixture::plain("operator fetch fixture"))
        .expect("fixture HTTP server");
    let fetch_url = web.url_path("/operator-matrix");
    let (world, spec) = direct_world(
        "direct-operator-cross",
        "dl-op",
        "[\"fetch.web\",\"session.todo_write\"]",
        &fetch_network_profile(),
    );
    let fetch_args = format!(r#"{{"url":"{fetch_url}"}}"#);
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
            SseScript::plain("native and Component calls complete"),
        ]),
    );
    let scenario_evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-op", "main").expect("session");
    let sid = send.session_id().to_string();

    let turn = send
        .send("run native and Component operator matrix")
        .expect("mixed turn");
    let turn_id = turn_id_of(&turn).expect("turn id");
    let captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&turn)?;
        require(turn.has_committed_completed(), "mixed turn must complete")?;
        web.wait_for_requests(1, Duration::from_secs(5))
            .map_err(|error| AssertError::StdoutMissing {
                expected: format!("fetch fixture did not receive a request: {error}"),
            })?;

        // M query: the ordinary row set contains both classes and no raw
        // metadata. Query order is the audit-id order used by tail/replay.
        let q = cli(
            &world,
            &spec,
            &["--json", "audit", "query", "--session", &sid],
        );
        require(
            q.status.success(),
            format!("audit query failed: {}", String::from_utf8_lossy(&q.stderr)),
        )?;
        let query_obj = parse_json(&q);
        let row_count = query_obj
            .get("row_count")
            .and_then(Value::as_u64)
            .expect("row_count");
        let rows = operator_rows(&query_obj);
        require(
            row_count as usize == rows.len(),
            format!("query row_count must equal rows.len: {query_obj}"),
        )?;
        let methods = row_methods(rows);
        require(
            methods.contains(&"fetch.web"),
            format!("audit rows must include native fetch.web: {methods:?}"),
        )?;
        require(
            methods.contains(&"session.todo_write"),
            format!("audit rows must include Component session.todo_write: {methods:?}"),
        )?;
        let native = row_for_method(rows, "fetch.web");
        let component = row_for_method(rows, "session.todo_write");
        let component_audit_id = component
            .get("audit_id")
            .and_then(Value::as_i64)
            .expect("Component audit id");
        require(
            native.get("evidence").is_some_and(Value::is_null),
            format!("N query row must serialize evidence=null: {native}"),
        )?;
        require(
            component
                .pointer("/evidence/outcome")
                .and_then(Value::as_str)
                == Some("completed"),
            format!("C query row must serialize structured evidence: {component}"),
        )?;
        require(
            component.get("metadata_json").is_none(),
            format!("query must not expose metadata_json: {component}"),
        )?;

        // N query filters remain row filters even when the native row has no
        // evidence pointer.
        let qn = cli(
            &world,
            &spec,
            &[
                "--json",
                "audit",
                "query",
                "--session",
                &sid,
                "--turn",
                &turn_id,
                "--method",
                "fetch.web",
                "--impact",
                "external_observation",
                "--capability",
                "fetch_url",
                "--verdict",
                "success",
            ],
        );
        require(qn.status.success(), "native filtered audit query failed")?;
        let native_query = parse_json(&qn);
        let native_filtered = operator_rows(&native_query);
        require(
            native_filtered.len() == 1
                && native_filtered[0].get("method").and_then(Value::as_str) == Some("fetch.web")
                && native_filtered[0]
                    .get("evidence")
                    .is_some_and(Value::is_null),
            format!("N filters must retain fetch.web with null evidence: {native_filtered:?}"),
        )?;

        // C query filters retain the Component's persisted impact label and
        // structured evidence. The legacy capability filter remains a native
        // metadata filter, so the Component proof uses its applicable filters.
        let qc = cli(
            &world,
            &spec,
            &[
                "--json",
                "audit",
                "query",
                "--session",
                &sid,
                "--turn",
                &turn_id,
                "--method",
                "session.todo_write",
                "--impact",
                "unknown",
                "--verdict",
                "success",
            ],
        );
        require(qc.status.success(), "Component filtered audit query failed")?;
        let component_query = parse_json(&qc);
        let component_filtered = operator_rows(&component_query);
        require(
            component_filtered.len() == 1
                && component_filtered[0].get("method").and_then(Value::as_str)
                    == Some("session.todo_write")
                && component_filtered[0]
                    .pointer("/evidence/outcome")
                    .and_then(Value::as_str)
                    == Some("completed"),
            format!("C filters must retain structured evidence: {component_filtered:?}"),
        )?;

        // Query pagination keeps the inclusive lower-bound row and applies the
        // adapter's database cap before materialization.
        let component_cursor = component_audit_id.to_string();
        let page = cli(
            &world,
            &spec,
            &[
                "--json",
                "audit",
                "query",
                "--session",
                &sid,
                "--since",
                &component_cursor,
            ],
        );
        require(page.status.success(), "mixed query page failed")?;
        let page_json = parse_json(&page);
        let page_rows = operator_rows(&page_json);
        require(
            page_json.get("row_count").and_then(Value::as_u64) == Some(1)
                && page_rows.len() == 1
                && page_rows[0].get("audit_id").and_then(Value::as_i64) == Some(component_audit_id)
                && page_rows[0].get("method").and_then(Value::as_str) == Some("session.todo_write"),
            format!("mixed query page/limit mismatch: {page_json}"),
        )?;

        // M tail: both rows appear in audit-id order, with the same optional
        // evidence shape as query.
        let qt = cli(
            &world,
            &spec,
            &[
                "--json",
                "audit",
                "query",
                "--session",
                &sid,
                "--tail",
                "--since",
                "0",
            ],
        );
        require(qt.status.success(), "audit query --tail failed")?;
        let query_t = parse_json(&qt);
        let tail_rows = operator_rows(&query_t);
        let tail_native = row_for_method(tail_rows, "fetch.web");
        let tail_component = row_for_method(tail_rows, "session.todo_write");
        require(
            tail_native.get("evidence").is_some_and(Value::is_null)
                && tail_component
                    .pointer("/evidence/outcome")
                    .and_then(Value::as_str)
                    == Some("completed"),
            format!("M tail must retain N/C evidence shape: {query_t}"),
        )?;
        let native_audit_id = native
            .get("audit_id")
            .and_then(Value::as_i64)
            .expect("native audit id");
        let native_cursor = native_audit_id.to_string();
        let continued = cli(
            &world,
            &spec,
            &[
                "--json",
                "audit",
                "query",
                "--session",
                &sid,
                "--tail",
                "--since",
                &native_cursor,
            ],
        );
        require(continued.status.success(), "tail continuation failed")?;
        let continued_query = parse_json(&continued);
        let continued_rows = operator_rows(&continued_query);
        require(
            continued_rows.iter().all(|row| {
                row.get("audit_id")
                    .and_then(Value::as_i64)
                    .is_some_and(|id| id > native_audit_id)
            }) && row_methods(continued_rows).contains(&"session.todo_write"),
            format!("tail cursor must be exclusive without skipping C: {continued_rows:?}"),
        )?;

        // M replay uses the same turn, preserving audit-id order and both
        // evidence variants without a statement/projection surface.
        let rp = cli(
            &world,
            &spec,
            &[
                "--json",
                "audit",
                "replay",
                "--session",
                &sid,
                "--turn",
                &turn_id,
            ],
        );
        require(
            rp.status.success(),
            format!(
                "audit replay failed: {}",
                String::from_utf8_lossy(&rp.stderr)
            ),
        )?;
        let replay = parse_json(&rp);
        let replay_turn = replay
            .get("trace")
            .and_then(|t| t.get("turn_id"))
            .and_then(Value::as_str)
            .unwrap_or("");
        require(
            replay_turn == turn_id.as_str(),
            format!(
                "replay trace.turn_id mismatch: expected {turn_id}, got {replay_turn:?}: {replay}"
            ),
        )?;
        let calls = replay_calls(&replay);
        let replay_ids: Vec<i64> = calls
            .iter()
            .filter_map(|call| call.get("audit_id").and_then(Value::as_i64))
            .collect();
        let mut sorted_ids = replay_ids.clone();
        sorted_ids.sort();
        require(
            replay_ids == sorted_ids,
            format!("replay must retain audit-id order: {calls:?}"),
        )?;
        require(
            row_for_method(calls, "fetch.web")
                .get("evidence")
                .is_some_and(Value::is_null)
                && row_for_method(calls, "session.todo_write")
                    .pointer("/evidence/outcome")
                    .and_then(Value::as_str)
                    == Some("completed"),
            format!("M replay must retain N/C evidence shape: {replay}"),
        )?;

        // M status: bounded recent calls do not filter by evidence presence.
        let ls = cli(&world, &spec, &["--json", "lancet", "status"]);
        require(
            ls.status.success(),
            format!(
                "lancet status failed: {}",
                String::from_utf8_lossy(&ls.stderr)
            ),
        )?;
        let status = parse_json(&ls);
        let recent_calls = status
            .get("recent_calls")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("status recent_calls: {status}"));
        let status_native = row_for_method(recent_calls, "fetch.web");
        let status_component = row_for_method(recent_calls, "session.todo_write");
        require(
            recent_calls.len() <= 20
                && status_native.get("evidence").is_some_and(Value::is_null)
                && status_native.get("verdict").and_then(Value::as_str) == Some("native_handler")
                && status_native
                    .get("duration_ms")
                    .and_then(Value::as_i64)
                    .is_some()
                && status_component
                    .pointer("/evidence/outcome")
                    .and_then(Value::as_str)
                    == Some("completed")
                && status_component.get("verdict").and_then(Value::as_str)
                    == Some("component_handler")
                && status_component
                    .get("duration_ms")
                    .and_then(Value::as_i64)
                    .is_some()
                && status.get("last_guard_verdict").and_then(Value::as_str)
                    == Some("component_handler"),
            format!("M status must retain N/C recent calls: {status}"),
        )?;

        // session status --json agrees on session id and agent.
        let ss = cli(&world, &spec, &["--json", "session", "status", &sid]);
        require(ss.status.success(), "session status failed")?;
        let sstatus = parse_json(&ss);
        require(
            sstatus.get("session_id").and_then(Value::as_str) == Some(sid.as_str()),
            format!("session status session_id mismatch: {sstatus}"),
        )?;
        require(
            sstatus.get("active_agent").and_then(Value::as_str) == Some("dl-op"),
            format!("session status agent mismatch: {sstatus}"),
        )?;

        // Session log remains independent of the operator DTO change.
        let sl = cli(&world, &spec, &["--json", "session", "log", &sid]);
        require(sl.status.success(), "session log failed")?;
        let slog = parse_json(&sl);
        let turns = slog
            .get("turns")
            .and_then(Value::as_array)
            .expect("session log turns array");
        require(
            !turns.is_empty(),
            format!("session log must show the mixed turn: {}", turns.len()),
        )?;

        Ok(())
    })();

    finish_scenario(
        scenario_evidence,
        &[("mixed native/component turn", &turn.command)],
        captures,
        result,
    );
}

#[test]
fn direct_operator_status_bounds_twenty_newest_mixed_calls() {
    let driver = driver();
    let web = FixtureHttpServer::start(HttpFixture::plain("bounded status fixture"))
        .expect("fixture HTTP server");
    let fetch_url = web.url_path("/bounded-status");
    let fetch_args = format!(r#"{{"url":"{fetch_url}"}}"#);
    let (world, spec) = direct_world(
        "direct-operator-status-bound",
        "dl-op-bound",
        "[\"fetch.web\",\"session.todo_write\"]",
        &fetch_network_profile(),
    );
    let mut scripts = Vec::new();
    for index in 0..11 {
        scripts.push(tool_call(
            &format!("bound-native-{index}"),
            "fetch.web",
            &fetch_args,
        ));
        scripts.push(tool_call(
            &format!("bound-component-{index}"),
            "session.todo_write",
            &format!(r#"{{"todos":[{{"content":"bounded {index}","status":"pending"}}]}}"#),
        ));
        scripts.push(SseScript::plain(format!("bounded turn {index} complete")));
    }
    let server = wire_fake_llm(&world, "dl-op-bound", sequence(scripts));
    let scenario_evidence = evidence(&world, &spec);
    let send =
        SessionSendDriver::new(&driver, &world, &spec, "dl-op-bound", "main").expect("session");
    let sid = send.session_id().to_string();
    let turns: Vec<_> = (0..11)
        .map(|index| {
            send.send(&format!("run bounded mixed operator turn {index}"))
                .expect("mixed turn")
        })
        .collect();
    let captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        for turn in &turns {
            assert_clean_send(turn)?;
            require(
                turn.has_committed_completed(),
                "bounded mixed turn must complete",
            )?;
        }
        web.wait_for_requests(11, Duration::from_secs(5))
            .map_err(|error| AssertError::StdoutMissing {
                expected: format!("fetch fixture did not receive 11 requests: {error}"),
            })?;

        let query = cli(
            &world,
            &spec,
            &["--json", "audit", "query", "--session", &sid],
        );
        require(query.status.success(), "bounded fixture query failed")?;
        let query_json = parse_json(&query);
        let all_rows = operator_rows(&query_json);
        require(
            query_json.get("row_count").and_then(Value::as_u64) == Some(22) && all_rows.len() == 22,
            format!("bounded fixture must create 22 finalized rows: {query_json}"),
        )?;

        let status = cli(&world, &spec, &["--json", "lancet", "status"]);
        require(status.status.success(), "bounded fixture status failed")?;
        let status_json = parse_json(&status);
        let recent = status_json
            .get("recent_calls")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("status recent_calls: {status_json}"));
        require(
            recent.len() == 20,
            format!("status must expose exactly the newest 20 of 22 rows: {status_json}"),
        )?;
        let recent_ids: Vec<i64> = recent
            .iter()
            .map(|row| {
                row.get("audit_id")
                    .and_then(Value::as_i64)
                    .expect("recent audit_id")
            })
            .collect();
        require(
            recent_ids.windows(2).all(|pair| pair[0] > pair[1]),
            format!("recent calls must be newest-first: {recent_ids:?}"),
        )?;
        let omitted_oldest: Vec<i64> = all_rows[..2]
            .iter()
            .map(|row| {
                row.get("audit_id")
                    .and_then(Value::as_i64)
                    .expect("audit_id")
            })
            .collect();
        require(
            omitted_oldest
                .iter()
                .all(|audit_id| !recent_ids.contains(audit_id)),
            format!("status must omit the two oldest rows: {recent_ids:?}"),
        )?;
        require(
            recent.iter().all(|row| {
                let method = row.get("method").and_then(Value::as_str);
                let verdict = row.get("verdict").and_then(Value::as_str);
                let duration = row.get("duration_ms").and_then(Value::as_i64);
                match method {
                    Some("fetch.web") => {
                        verdict == Some("native_handler")
                            && duration.is_some()
                            && row_has_expected_evidence(row, false)
                    }
                    Some("session.todo_write") => {
                        verdict == Some("component_handler")
                            && duration.is_some()
                            && row_has_expected_evidence(row, true)
                    }
                    _ => false,
                }
            }) && recent
                .iter()
                .filter(|row| row.get("method").and_then(Value::as_str) == Some("fetch.web"))
                .count()
                == 10
                && recent
                    .iter()
                    .filter(|row| {
                        row.get("method").and_then(Value::as_str) == Some("session.todo_write")
                    })
                    .count()
                    == 10
                && status_json
                    .get("last_guard_verdict")
                    .and_then(Value::as_str)
                    == Some("component_handler"),
            format!("bounded status must preserve ten N and ten C rows: {status_json}"),
        )?;

        Ok(())
    })();

    let summaries: Vec<String> = (0..turns.len())
        .map(|index| format!("bounded mixed turn {index}"))
        .collect();
    let commands: Vec<(&str, &lattice_system_harness::CommandOutcome)> = summaries
        .iter()
        .zip(&turns)
        .map(|(summary, turn)| (summary.as_str(), &turn.command))
        .collect();
    finish_scenario(scenario_evidence, &commands, captures, result);
}

/// An unknown session query returns an empty success (no rows), not a panic.
#[test]
fn direct_operator_audit_query_unknown_session_empty() {
    let driver = driver();
    let (world, spec) = direct_world("direct-operator-unknown-sess", "dl-op-unk", "[]", "");
    // Seed the audit store with one real turn so the store exists, then query
    // a different (unknown) session.
    let _server = wire_fake_llm(&world, "dl-op-unk", SseScript::plain("seed"));
    let send =
        SessionSendDriver::new(&driver, &world, &spec, "dl-op-unk", "main").expect("session");
    let seed = send.send("seed turn").expect("seed");
    assert_clean_send(&seed).expect("seed turn clean");
    let scenario_evidence = evidence(&world, &spec);

    let q = cli(
        &world,
        &spec,
        &[
            "--json",
            "audit",
            "query",
            "--session",
            "nonexistent-session",
        ],
    );
    let result: Result<(), AssertError> = (|| {
        require(
            q.status.success(),
            format!(
                "audit query unknown session must exit 0: {}",
                String::from_utf8_lossy(&q.stderr)
            ),
        )?;
        let query_obj = parse_json(&q);
        let row_count = query_obj
            .get("row_count")
            .and_then(Value::as_u64)
            .unwrap_or(999);
        require(
            row_count == 0,
            format!("unknown session must return zero rows: {query_obj}"),
        )?;
        Ok(())
    })();

    finish_scenario(
        scenario_evidence,
        &[("audit query unknown session", &seed.command)],
        Vec::new(),
        result,
    );
}

/// Replay of an unknown turn id is an explicit nonzero failure, not a panic.
#[test]
fn direct_operator_audit_replay_unknown_turn_fails() {
    let driver = driver();
    let (world, spec) = direct_world("direct-operator-unknown-turn", "dl-op-ut", "[]", "");
    let _server = wire_fake_llm(&world, "dl-op-ut", SseScript::plain("seed"));
    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-op-ut", "main").expect("session");
    let seed = send.send("seed turn").expect("seed");
    assert_clean_send(&seed).expect("seed turn clean");
    let sid = send.session_id().to_string();
    let scenario_evidence = evidence(&world, &spec);

    let rp = cli(
        &world,
        &spec,
        &[
            "--json",
            "audit",
            "replay",
            "--session",
            &sid,
            "--turn",
            "no-such-turn",
        ],
    );
    let result: Result<(), AssertError> = (|| {
        require(
            !rp.status.success(),
            "audit replay unknown turn must exit nonzero",
        )?;
        let err = String::from_utf8_lossy(&rp.stderr);
        require(
            err.contains("no audit rows for turn_id"),
            format!("audit replay unknown-turn diagnostic mismatch: {err}"),
        )?;
        Ok(())
    })();

    finish_scenario(
        scenario_evidence,
        &[("audit replay unknown turn", &seed.command)],
        Vec::new(),
        result,
    );
}

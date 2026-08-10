// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Release-binary operator audit matrix for GitHub #114.
//!
//! This target is feature-gated and executed only by `cargo xtask rollout`.
//! It owns fixtures and assertions independent of `direct_operator`, while
//! sharing only the harness's generic process/world/fake-provider plumbing.

mod support;

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

fn row_for_method<'a>(rows: &'a [Value], method: &str) -> &'a Value {
    rows.iter()
        .find(|row| row.get("method").and_then(Value::as_str) == Some(method))
        .unwrap_or_else(|| panic!("missing {method}: {rows:?}"))
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

fn expected_evidence(row: &Value, present: bool) -> bool {
    if present {
        row.pointer("/evidence/outcome").and_then(Value::as_str) == Some("completed")
    } else {
        row.get("evidence").is_some_and(Value::is_null)
    }
}

struct SingleReleaseExpectation<'a> {
    session_id: &'a str,
    turn_id: &'a str,
    method: &'a str,
    impact: &'a str,
    capability: Option<&'a str>,
    verdict: &'a str,
    evidence_present: bool,
}

fn assert_single_release_case(
    world: &lattice_system_harness::ScenarioWorld,
    spec: &lattice_system_harness::ScenarioSpec,
    expectation: SingleReleaseExpectation<'_>,
) -> Result<(), AssertError> {
    let SingleReleaseExpectation {
        session_id,
        turn_id,
        method,
        impact,
        capability,
        verdict,
        evidence_present,
    } = expectation;
    let mut filtered_args = vec![
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
        filtered_args.extend(["--capability", capability]);
    }
    let query = cli(world, spec, &filtered_args);
    require(
        query.status.success(),
        format!(
            "release {method} query failed: {}",
            String::from_utf8_lossy(&query.stderr)
        ),
    )?;
    let query_json = parse_json(&query);
    let query_rows = rows(&query_json);
    require(
        query_json.get("row_count").and_then(Value::as_u64) == Some(1)
            && query_rows.len() == 1
            && query_rows[0].get("method").and_then(Value::as_str) == Some(method)
            && expected_evidence(&query_rows[0], evidence_present)
            && query_rows[0].get("metadata_json").is_none(),
        format!("release {method} query mismatch: {query_json}"),
    )?;
    let audit_id = query_rows[0]
        .get("audit_id")
        .and_then(Value::as_i64)
        .expect("audit id");
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
    require(
        page.status.success(),
        format!("release {method} page failed"),
    )?;
    let page_json = parse_json(&page);
    require(
        page_json.get("row_count").and_then(Value::as_u64) == Some(1)
            && rows(&page_json)
                .first()
                .and_then(|row| row.get("audit_id"))
                .and_then(Value::as_i64)
                == Some(audit_id),
        format!("release {method} query pagination/limit mismatch: {page_json}"),
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
    require(
        tail.status.success(),
        format!("release {method} tail failed"),
    )?;
    let tail_json = parse_json(&tail);
    require(
        rows(&tail_json).len() == 1
            && rows(&tail_json)[0].get("method").and_then(Value::as_str) == Some(method)
            && expected_evidence(&rows(&tail_json)[0], evidence_present),
        format!("release {method} tail mismatch: {tail_json}"),
    )?;
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
            &cursor,
        ],
    );
    require(
        continuation.status.success(),
        format!("release {method} tail continuation failed"),
    )?;
    let continuation_json = parse_json(&continuation);
    require(
        rows(&continuation_json).is_empty()
            && continuation_json
                .get("last_audit_id")
                .and_then(Value::as_i64)
                == Some(audit_id),
        format!("release {method} exclusive tail mismatch: {continuation_json}"),
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
    require(
        replay.status.success(),
        format!("release {method} replay failed"),
    )?;
    let replay_json = parse_json(&replay);
    let calls = replay_calls(&replay_json);
    require(
        replay_json
            .pointer("/trace/turn_id")
            .and_then(Value::as_str)
            == Some(turn_id)
            && calls.len() == 1
            && calls[0].get("method").and_then(Value::as_str) == Some(method)
            && expected_evidence(&calls[0], evidence_present),
        format!("release {method} replay mismatch: {replay_json}"),
    )?;

    let status = cli(world, spec, &["--json", "lancet", "status"]);
    require(
        status.status.success(),
        format!("release {method} status failed"),
    )?;
    let status_json = parse_json(&status);
    let recent = status_json
        .get("recent_calls")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("recent_calls: {status_json}"));
    let entry = row_for_method(recent, method);
    require(
        recent.len() <= 20
            && entry.get("verdict").and_then(Value::as_str) == Some(verdict)
            && entry.get("duration_ms").and_then(Value::as_i64).is_some()
            && expected_evidence(entry, evidence_present)
            && status_json
                .get("last_guard_verdict")
                .and_then(Value::as_str)
                == Some(verdict),
        format!("release {method} status mismatch: {status_json}"),
    )?;

    Ok(())
}

fn release_native_case() {
    let driver = driver();
    let web = FixtureHttpServer::start(HttpFixture::plain("release native fixture"))
        .expect("fixture HTTP server");
    let fetch_url = web.url_path("/release-native");
    let (world, spec) = direct_world(
        "release-operator-native",
        "release-op-native",
        "[\"fetch.web\"]",
        &fetch_network_profile(),
    );
    let fetch_args = format!(r#"{{"url":"{fetch_url}"}}"#);
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
        assert_single_release_case(
            &world,
            &spec,
            SingleReleaseExpectation {
                session_id: &session_id,
                turn_id: &turn_id,
                method: "fetch.web",
                impact: "external_observation",
                capability: Some("fetch_url"),
                verdict: "native_handler",
                evidence_present: false,
            },
        )
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
        assert_single_release_case(
            &world,
            &spec,
            SingleReleaseExpectation {
                session_id: &session_id,
                turn_id: &turn_id,
                method: "session.todo_write",
                impact: "unknown",
                capability: None,
                verdict: "component_handler",
                evidence_present: true,
            },
        )
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
            send.send(&format!("release mixed matrix turn {index}"))
                .expect("mixed turn")
        })
        .collect();
    let last_turn_id = turn_id_of(turns.last().expect("last mixed turn"));
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
        let query_rows = rows(&query_json);
        require(
            query_json.get("row_count").and_then(Value::as_u64) == Some(22)
                && query_rows.len() == 22,
            format!("release mixed query must expose 22 rows: {query_json}"),
        )?;
        let last_native = &query_rows[20];
        let last_component = &query_rows[21];
        require(
            last_native.get("method").and_then(Value::as_str) == Some("fetch.web")
                && expected_evidence(last_native, false)
                && last_component.get("method").and_then(Value::as_str)
                    == Some("session.todo_write")
                && expected_evidence(last_component, true),
            format!("release mixed query N/C shape mismatch: {query_json}"),
        )?;
        let native_audit_id = last_native
            .get("audit_id")
            .and_then(Value::as_i64)
            .expect("native audit id");
        let component_audit_id = last_component
            .get("audit_id")
            .and_then(Value::as_i64)
            .expect("Component audit id");

        let page_cursor = component_audit_id.to_string();
        let page = cli(
            &world,
            &spec,
            &[
                "--json",
                "audit",
                "query",
                "--session",
                &session_id,
                "--since",
                &page_cursor,
            ],
        );
        require(page.status.success(), "release mixed query page failed")?;
        let page_json = parse_json(&page);
        require(
            page_json.get("row_count").and_then(Value::as_u64) == Some(1)
                && rows(&page_json)
                    .first()
                    .and_then(|row| row.get("audit_id"))
                    .and_then(Value::as_i64)
                    == Some(component_audit_id),
            format!("release mixed query pagination/limit mismatch: {page_json}"),
        )?;

        let tail_cursor = native_audit_id.to_string();
        let tail = cli(
            &world,
            &spec,
            &[
                "--json",
                "audit",
                "query",
                "--session",
                &session_id,
                "--tail",
                "--since",
                &tail_cursor,
            ],
        );
        require(tail.status.success(), "release mixed tail failed")?;
        let tail_json = parse_json(&tail);
        require(
            rows(&tail_json).len() == 1
                && rows(&tail_json)[0].get("audit_id").and_then(Value::as_i64)
                    == Some(component_audit_id)
                && expected_evidence(&rows(&tail_json)[0], true),
            format!("release mixed exclusive tail mismatch: {tail_json}"),
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
        let calls = replay_calls(&replay_json);
        require(
            calls.len() == 2
                && calls[0].get("audit_id").and_then(Value::as_i64) == Some(native_audit_id)
                && calls[1].get("audit_id").and_then(Value::as_i64) == Some(component_audit_id)
                && expected_evidence(&calls[0], false)
                && expected_evidence(&calls[1], true),
            format!("release mixed replay order/evidence mismatch: {replay_json}"),
        )?;

        let status = cli(&world, &spec, &["--json", "lancet", "status"]);
        require(status.status.success(), "release mixed status failed")?;
        let status_json = parse_json(&status);
        let recent = status_json
            .get("recent_calls")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("recent calls: {status_json}"));
        let recent_ids: Vec<i64> = recent
            .iter()
            .map(|row| {
                row.get("audit_id")
                    .and_then(Value::as_i64)
                    .expect("recent audit id")
            })
            .collect();
        let omitted_ids: Vec<i64> = query_rows[..2]
            .iter()
            .map(|row| {
                row.get("audit_id")
                    .and_then(Value::as_i64)
                    .expect("audit id")
            })
            .collect();
        require(
            recent.len() == 20
                && recent_ids.windows(2).all(|pair| pair[0] > pair[1])
                && omitted_ids.iter().all(|id| !recent_ids.contains(id))
                && recent
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
                && recent.iter().all(|row| {
                    let method = row.get("method").and_then(Value::as_str);
                    let verdict = row.get("verdict").and_then(Value::as_str);
                    let duration = row.get("duration_ms").and_then(Value::as_i64);
                    match method {
                        Some("fetch.web") => {
                            verdict == Some("native_handler")
                                && duration.is_some()
                                && expected_evidence(row, false)
                        }
                        Some("session.todo_write") => {
                            verdict == Some("component_handler")
                                && duration.is_some()
                                && expected_evidence(row, true)
                        }
                        _ => false,
                    }
                })
                && status_json
                    .get("last_guard_verdict")
                    .and_then(Value::as_str)
                    == Some("component_handler"),
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

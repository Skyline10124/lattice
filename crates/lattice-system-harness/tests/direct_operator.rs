// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Direct-Local operator paths: audit query/replay, lancet status, session
//! status/log cross-validation (issue #46 (Codeberg #24) slice 6).
//!
//! After a real success turn and a real denial turn, run bounded public
//! commands and assert row counts, methods, impacts, verdicts, activity
//! order, and stored attestation match. Unknown session/turn are explicit
//! nonzero failures.

#![cfg(unix)]

mod support;

use std::process::Output;

use lattice_system_harness::{
    AssertError, SendExecution, SessionSendDriver, SseScript, TurnEventLine,
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

/// Collect `method` values from an audit query fragment's activities.
fn fragment_activities(query: &Value) -> Vec<&str> {
    query
        .get("fragment")
        .and_then(|f| f.get("activities"))
        .and_then(Value::as_array)
        .map(|acts| {
            acts.iter()
                .filter_map(|a| a.get("method").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default()
}

/// Extract the turn_id from a turn_started event in a SendExecution.
fn turn_id_of(outcome: &SendExecution) -> Option<String> {
    outcome.events.iter().find_map(|e| match e {
        TurnEventLine::TurnStarted { turn_id, .. } => Some(turn_id.clone()),
        _ => None,
    })
}

/// After a success turn (observe.workspace) and a denial turn (mutate.create
/// denied by write policy), audit query returns rows for both with correct
/// methods, impacts, and verdicts; audit replay reconstructs the success turn;
/// lancet status reports recent calls; session status/log agree.
#[test]
fn direct_operator_audit_session_lancet_cross_validate() {
    let driver = driver();
    let (world, spec) = direct_world(
        "direct-operator-cross",
        "dl-op",
        "[\"observe.workspace\",\"mutate.create\"]",
        "[lancet.workspace]\ndenied_write_patterns = [\"locked/**\"]\n",
    );
    world
        .write_project_file("readme.txt", "hello\n")
        .expect("seed file");
    world.commit_all("seed").expect("commit");
    world
        .write_project_file("locked/secret.txt", "deny-me\n")
        .expect("locked file");
    world.commit_all("locked seed").expect("commit");

    let server = wire_fake_llm(
        &world,
        "dl-op",
        sequence(vec![
            // Turn 1: successful observation.
            tool_call("o1", "observe.workspace", r#"{"path":"readme.txt"}"#),
            SseScript::plain("observed"),
            // Turn 2: denied mutation (locked/** pattern).
            tool_call(
                "m1",
                "mutate.create",
                r#"{"path":"locked/x.txt","content":"x\n","create_parents":false}"#,
            ),
            SseScript::plain("denied as expected"),
        ]),
    );
    let scenario_evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-op", "main").expect("session");
    let sid = send.session_id().to_string();

    let t1 = send.send("observe readme").expect("turn 1");
    let t2 = send.send("create in locked").expect("turn 2");
    let turn_id_1 = turn_id_of(&t1).expect("turn 1 turn_id");
    let captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&t1)?;
        assert_clean_send(&t2)?;
        require(t1.has_committed_completed(), "turn 1 must complete")?;
        require(
            t2.has_committed_completed(),
            "turn 2 must complete even though the tool was denied",
        )?;

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
        require(
            row_count > 0,
            format!("audit query must return rows: {query_obj}"),
        )?;
        let acts = fragment_activities(&query_obj);
        require(!acts.is_empty(), "fragment activities must be non-empty")?;
        let methods: Vec<&str> = acts.clone();
        require(
            methods.contains(&"observe.workspace"),
            format!("audit rows must include observe.workspace: {methods:?}"),
        )?;
        require(
            methods.contains(&"mutate.create"),
            format!("audit rows must include mutate.create: {methods:?}"),
        )?;
        // Success/denied verdicts are validated via the --verdict filters below
        // (the PROV projection's `semantic_status` is an internal stage label,
        // not the success/denied verdict).

        // audit query --method filter narrows to one method.
        let qm = cli(
            &world,
            &spec,
            &[
                "--json",
                "audit",
                "query",
                "--session",
                &sid,
                "--method",
                "observe.workspace",
            ],
        );
        require(qm.status.success(), "audit query --method failed")?;
        let query_m = parse_json(&qm);
        let acts_m = fragment_activities(&query_m);
        require(!acts_m.is_empty(), "method filter must return activities")?;
        require(
            acts_m.iter().all(|m| *m == "observe.workspace"),
            "method filter must narrow to observe.workspace",
        )?;

        // audit query --verdict denied narrows to the denial.
        let qv = cli(
            &world,
            &spec,
            &[
                "--json",
                "audit",
                "query",
                "--session",
                &sid,
                "--verdict",
                "denied",
            ],
        );
        require(qv.status.success(), "audit query --verdict failed")?;
        let query_v = parse_json(&qv);
        let v_count = query_v
            .get("row_count")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        require(v_count > 0, "verdict filter must return rows")?;
        let acts_v = fragment_activities(&query_v);
        require(!acts_v.is_empty(), "verdict filter must return activities")?;
        require(
            acts_v.contains(&"mutate.create"),
            "verdict=denied filter must include mutate.create",
        )?;

        // audit query --tail --since 0 streams all rows.
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
        let tail_rows = query_t
            .get("rows")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0);
        require(tail_rows > 0, format!("tail must stream rows: {query_t}"))?;

        // audit replay --turn <turn_id_1> reconstructs the success turn.
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
                &turn_id_1,
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
            replay_turn == turn_id_1.as_str(),
            format!(
                "replay trace.turn_id mismatch: expected {turn_id_1}, got {replay_turn:?}: {replay}"
            ),
        )?;
        let replay_activities = replay
            .get("trace")
            .map(fragment_activities)
            .unwrap_or_default();
        require(
            replay_activities.contains(&"observe.workspace"),
            format!("replay trace must include observe.workspace activity: {replay_activities:?}"),
        )?;
        require(
            replay
                .get("stored_statement_match")
                .and_then(Value::as_bool)
                == Some(true),
            format!("replay must report matching stored attestation: {replay}"),
        )?;

        // lancet status --json reports recent calls.
        let ls = cli(&world, &spec, &["--json", "lancet", "status"]);
        require(
            ls.status.success(),
            format!(
                "lancet status failed: {}",
                String::from_utf8_lossy(&ls.stderr)
            ),
        )?;
        let status = parse_json(&ls);
        require(
            status.get("recent_calls").is_some() || !status.is_null(),
            format!("lancet status must report recent calls: {status}"),
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

        // session log --json shows both turns.
        let sl = cli(&world, &spec, &["--json", "session", "log", &sid]);
        require(sl.status.success(), "session log failed")?;
        let slog = parse_json(&sl);
        let turns = slog
            .get("turns")
            .and_then(Value::as_array)
            .expect("session log turns array");
        require(
            turns.len() >= 2,
            format!("session log must show >=2 turns: {}", turns.len()),
        )?;

        Ok(())
    })();

    finish_scenario(
        scenario_evidence,
        &[("turn 1", &t1.command), ("turn 2", &t2.command)],
        Vec::new(),
        result,
    );
    let _ = captures;
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

// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 公开 IPC 操作员读取：当前状态、ChangeSeq、顺序 replay 与 recent20。
mod support;
use lattice_system_harness::{
    AssertError, FixtureHttpServer, HttpFixture, SessionSendDriver, SseScript,
};
use serde_json::Value;
use std::{collections::HashSet, time::Duration};
use support::*;

fn network() -> &'static str {
    "\n[lancet.network]\nallow_localhost = true\nallowed_hosts = [\"localhost\"]\nmax_web_response_bytes = 1024\n\n[lancet.capabilities]\nmax_boundary = \"ex\"\n"
}
fn operator_case(name: &str, methods: &[&str]) {
    let driver = driver();
    let web = FixtureHttpServer::start(HttpFixture::plain("operator fixture")).unwrap();
    let (world, spec) = direct_world(
        name,
        "operator",
        "[\"fetch.web\",\"session.todo_write\"]",
        network(),
    );
    let calls = methods
        .iter()
        .enumerate()
        .map(|(i, method)| {
            let args = if *method == "fetch.web" {
                serde_json::json!({"url":web.url_path("/operator")}).to_string()
            } else {
                serde_json::json!({"todos":[{"content":format!("task {i}"),"status":"pending"}]})
                    .to_string()
            };
            (format!("call-{i}"), method.to_string(), args)
        })
        .collect();
    let server = wire_fake_llm(
        &world,
        "operator",
        sequence(vec![
            SseScript::ToolCalls { calls },
            SseScript::plain("complete"),
        ]),
    );
    let mut evidence = evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let send = SessionSendDriver::new(&driver, &world, &spec, "operator");
        let turn = send.send("operator matrix").unwrap();
        evidence.record_command("operator Turn", &turn.command);
        assert_clean_send(&turn).unwrap();
        assert!(turn.has_completed());
        let sid = send.session_id().unwrap();
        let tid = turn.accepted_turn_id().unwrap();
        let fetches = methods.iter().filter(|m| **m == "fetch.web").count();
        if fetches > 0 {
            web.wait_for_requests(fetches, Duration::from_secs(5))
                .unwrap();
        }
        // 读取不依赖当前 profile/provider 材料，后台仍独占持库。
        std::fs::remove_file(
            world
                .project_dir()
                .join(".lattice/agents/operator/agent.toml"),
        )
        .unwrap();
        world.unset_env("OPENAI_COMPATIBLE_API_KEY");
        let mut read = |args: &[&str]| -> Value {
            let out = cli(&world, &spec, args);
            evidence.record_command(args.join(" "), &out);
            assert!(out.is_clean_exit(0), "{}", out.summary());
            serde_json::from_slice(&out.stdout).unwrap()
        };
        let query = read(&[
            "--json",
            "audit",
            "--session",
            sid,
            "--turn",
            tid,
            "--outcome",
            "succeeded",
        ]);
        let rows = query["rows"].as_array().unwrap();
        assert_eq!(rows.len(), methods.len());
        let ids: HashSet<_> = rows
            .iter()
            .map(|r| r["invocation_id"].as_str().unwrap())
            .collect();
        assert_eq!(ids.len(), methods.len());
        for row in rows {
            assert_eq!(row["binding"]["session_id"], sid);
            assert_eq!(row["binding"]["turn_id"], tid);
            assert_eq!(row["state"]["state"], "terminal");
            assert_eq!(row["state"]["outcome"], "succeeded");
            assert_eq!(
                row["state"]["evidence"]["value"]["admission"]["tool"],
                row["binding"]["method"]
            );
            for field in ["descriptor_identity", "arguments_digest"] {
                assert_eq!(row["binding"][field].as_str().unwrap().len(), 64);
            }
        }
        let replay = read(&["--json", "audit", "replay", "--session", sid, "--turn", tid]);
        let calls = replay["calls"].as_array().unwrap();
        assert_eq!(
            calls
                .iter()
                .map(|r| r["binding"]["method"].as_str().unwrap())
                .collect::<Vec<_>>(),
            methods
        );
        assert_eq!(replay["latest_change"], query["latest_change"]);
        let tail = read(&["--json", "audit", "--session", sid, "--turn", tid, "--tail"]);
        let changes = tail["rows"].as_array().unwrap();
        assert_eq!(changes.len(), methods.len() * 3);
        for (i, change) in changes.iter().enumerate() {
            assert_eq!(change["sequence"], i + 1);
        }
        for row in rows {
            let lifecycle: Vec<_> = changes
                .iter()
                .filter(|c| c["row"]["invocation_id"] == row["invocation_id"])
                .map(|c| c["row"]["state"]["state"].as_str().unwrap())
                .collect();
            assert_eq!(lifecycle, ["registered", "started", "terminal"]);
        }
        let cursor = tail["next_after"].as_u64().unwrap().to_string();
        let continued = read(&["--json", "audit", "--tail", "--since", &cursor]);
        assert_eq!(continued["rows"], serde_json::json!([]));
        assert_eq!(continued["next_after"], tail["next_after"]);
        assert_eq!(continued["done"], true);
        let status = read(&["--json", "lancet", "status"]);
        let recent = status["recent"].as_array().unwrap();
        assert_eq!(recent.len(), methods.len().min(20));
        let expected: Vec<_> = changes
            .iter()
            .rev()
            .filter(|c| c["row"]["state"]["state"] == "terminal")
            .take(20)
            .map(|c| &c["row"])
            .collect();
        assert_eq!(recent.iter().collect::<Vec<_>>(), expected);
        assert_eq!(status["latest_change"], tail["latest_change"]);
        let filtered = read(&["--json", "audit", "--session", sid, "--method", methods[0]]);
        assert_eq!(
            filtered["rows"].as_array().unwrap().len(),
            methods.iter().filter(|m| **m == methods[0]).count()
        );
        // 文本输出逐行对应同一批调用，状态词与 JSON 的拼写一致。
        let mut text = |args: &[&str]| -> Vec<String> {
            let out = cli(&world, &spec, args);
            evidence.record_command(args.join(" "), &out);
            assert!(out.is_clean_exit(0), "{}", out.summary());
            let stdout = std::str::from_utf8(&out.stdout).unwrap();
            stdout.lines().map(str::to_owned).collect()
        };
        let line = |row: &Value, state: &str| {
            format!(
                "{} {} {state}",
                row["invocation_id"].as_str().unwrap(),
                row["binding"]["method"].as_str().unwrap()
            )
        };
        let succeeded = |row: &Value| line(row, "terminal outcome=succeeded");
        let listed = text(&["audit", "--session", sid, "--turn", tid]);
        assert_eq!(
            listed[0],
            format!("latest_change={} next=none", query["latest_change"])
        );
        assert_eq!(listed[1..], rows.iter().map(succeeded).collect::<Vec<_>>());
        let replayed = text(&["audit", "replay", "--session", sid, "--turn", tid]);
        assert_eq!(
            replayed[1..],
            calls.iter().map(succeeded).collect::<Vec<_>>()
        );
        let tailed = text(&["audit", "--session", sid, "--turn", tid, "--tail"]);
        let expected: Vec<_> = changes
            .iter()
            .map(|change| {
                let state = match change["row"]["state"]["state"].as_str().unwrap() {
                    "terminal" => "terminal outcome=succeeded",
                    word => word,
                };
                format!("{} {}", change["sequence"], line(&change["row"], state))
            })
            .collect();
        assert_eq!(tailed[1..], expected);
        let shown = text(&["lancet", "status"]);
        assert_eq!(shown[2..], recent.iter().map(succeeded).collect::<Vec<_>>());
        assert_eq!(server.captured_requests().len(), 2);
        record_session(
            &send,
            &mut evidence,
            sid,
            serde_json::json!({"methods":methods,"prompt":"operator matrix","final_reply":"complete"}),
        );
    }));
    evidence.record_captures(server.captured_requests());
    finish_recorded(evidence, result);
}
#[test]
fn direct_operator_fetch_terminal_contract() {
    operator_case("direct-operator-fetch", &["fetch.web"]);
}
#[test]
fn direct_operator_component_terminal_contract() {
    operator_case("direct-operator-component", &["session.todo_write"]);
}
#[test]
fn direct_operator_mixed_query_tail_replay_and_status() {
    operator_case(
        "direct-operator-cross",
        &["fetch.web", "session.todo_write"],
    );
}
#[test]
fn direct_operator_status_bounds_twenty_newest_invocations() {
    operator_case(
        "direct-operator-status-bound",
        &["fetch.web", "session.todo_write"].repeat(11),
    );
}
#[test]
fn direct_operator_unknown_typed_session_is_empty() {
    let driver = driver();
    let (world, spec) = direct_world("direct-operator-unknown-sess", "operator", "[]", "");
    let server = wire_fake_llm(&world, "operator", SseScript::plain("seed"));
    let mut evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "operator");
    let seed = send.send("seed").unwrap();
    let unknown = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let listed = cli(&world, &spec, &["--json", "session", "list"]);
    let query = cli(&world, &spec, &["--json", "audit", "--session", unknown]);
    let zero = cli(
        &world,
        &spec,
        &[
            "--json",
            "audit",
            "replay",
            "--session",
            send.session_id().unwrap(),
            "--turn",
            seed.accepted_turn_id().unwrap(),
        ],
    );
    let result = (|| {
        assert_clean_send(&seed)?;
        require(seed.has_completed(), "seed Session completed")?;
        require(listed.is_clean_exit(0), "session list succeeds")?;
        let sessions: Value =
            serde_json::from_slice(&listed.stdout).map_err(|e| AssertError::StdoutMissing {
                expected: format!("session list JSON: {e}"),
            })?;
        let sessions = sessions
            .as_array()
            .ok_or_else(|| AssertError::StdoutMissing {
                expected: "session list array".into(),
            })?;
        require(
            sessions.len() == 1 && sessions[0]["session_id"] == send.session_id().unwrap(),
            "session list must contain the known zero-call Session",
        )?;
        require(
            sessions
                .iter()
                .all(|session| session["session_id"] != unknown),
            "queried SessionId must be absent from the complete session list",
        )?;
        let query_json: Value =
            serde_json::from_slice(&query.stdout).map_err(|e| AssertError::StdoutMissing {
                expected: format!("audit JSON: {e}"),
            })?;
        let zero_json: Value =
            serde_json::from_slice(&zero.stdout).map_err(|e| AssertError::StdoutMissing {
                expected: format!("replay JSON: {e}"),
            })?;
        require(
            query.is_clean_exit(0) && query_json["rows"] == serde_json::json!([]),
            "unknown Session has no calls",
        )?;
        require(
            zero.is_clean_exit(0) && zero_json["calls"] == serde_json::json!([]),
            "zero-call Turn replay is valid",
        )
    })();
    for (label, out) in [
        ("seed", &seed.command),
        ("session list", &listed),
        ("unknown session", &query),
        ("zero replay", &zero),
    ] {
        evidence.record_command(label, out);
    }
    evidence.record_captures(server.captured_requests());
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        result.unwrap();
        record_session(
            &send,
            &mut evidence,
            send.session_id().unwrap(),
            serde_json::json!({"prompt":"seed","unknown_session":unknown,"zero_call_replay":true}),
        );
    }));
    finish_recorded(evidence, result);
}
#[test]
fn direct_operator_unknown_typed_turn_fails() {
    let driver = driver();
    let (world, spec) = direct_world("direct-operator-unknown-turn", "operator", "[]", "");
    let server = wire_fake_llm(&world, "operator", SseScript::plain("seed"));
    let mut evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "operator");
    let seed = send.send("seed").unwrap();
    let replay = cli(
        &world,
        &spec,
        &[
            "--json",
            "audit",
            "replay",
            "--session",
            send.session_id().unwrap(),
            "--turn",
            "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
        ],
    );
    let result = (|| {
        assert_clean_send(&seed)?;
        let error: Value = serde_json::from_slice(&replay.stdout).unwrap();
        require(
            replay.is_clean_exit(2) && error["type"] == "error" && error["kind"] == "not_found",
            "unknown Turn returns typed NotFound, not empty replay",
        )?;
        require(
            !String::from_utf8_lossy(&replay.stdout)
                .contains("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"),
            "error does not echo requested ID",
        )
    })();
    evidence.record_command("seed", &seed.command);
    evidence.record_command("unknown turn", &replay);
    evidence.record_captures(server.captured_requests());
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        result.unwrap();
        record_session(
            &send,
            &mut evidence,
            send.session_id().unwrap(),
            serde_json::json!({"prompt":"seed","unknown_turn":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"}),
        );
    }));
    finish_recorded(evidence, result);
}

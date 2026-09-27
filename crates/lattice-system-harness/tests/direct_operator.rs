// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 公开 IPC 操作员读取：当前状态、ChangeSeq、顺序 replay 与 recent20。
mod support;
use lattice_system_harness::{FixtureHttpServer, HttpFixture, SessionSendDriver, SseScript};
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
        assert_eq!(server.captured_requests().len(), 2);
    }));
    evidence.record_captures(server.captured_requests());
    evidence
        .finish_report(result.err().map(|p| {
            p.downcast_ref::<String>()
                .cloned()
                .unwrap_or_else(|| "operator assertion failed".into())
        }))
        .unwrap();
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
    let evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "operator");
    let seed = send.send("seed").unwrap();
    let query = cli(
        &world,
        &spec,
        &[
            "--json",
            "audit",
            "--session",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ],
    );
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
        require(
            query.is_clean_exit(0)
                && serde_json::from_slice::<Value>(&query.stdout).unwrap()["rows"]
                    == serde_json::json!([]),
            "unknown Session has no calls",
        )?;
        require(
            zero.is_clean_exit(0)
                && serde_json::from_slice::<Value>(&zero.stdout).unwrap()["calls"]
                    == serde_json::json!([]),
            "zero-call Turn replay is valid",
        )
    })();
    finish_scenario(
        evidence,
        &[
            ("seed", &seed.command),
            ("unknown session", &query),
            ("zero replay", &zero),
        ],
        server.captured_requests(),
        result,
    );
}
#[test]
fn direct_operator_unknown_typed_turn_fails() {
    let driver = driver();
    let (world, spec) = direct_world("direct-operator-unknown-turn", "operator", "[]", "");
    let server = wire_fake_llm(&world, "operator", SseScript::plain("seed"));
    let evidence = evidence(&world, &spec);
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
    finish_scenario(
        evidence,
        &[("seed", &seed.command), ("unknown turn", &replay)],
        server.captured_requests(),
        result,
    );
}

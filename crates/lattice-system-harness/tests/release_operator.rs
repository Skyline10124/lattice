// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0
//! Release operator 矩阵；由 cargo xtask rollout 使用实际 release 二进制执行。
mod support;
use lattice_system_harness::{FixtureHttpServer, HttpFixture, SessionSendDriver, SseScript};
use serde_json::Value;
use std::{collections::HashSet, time::Duration};
use support::*;
fn release_case(name: &str, methods: &[&str], turns: usize) {
    let driver = driver();
    let web = FixtureHttpServer::start(HttpFixture::plain("release fixture")).unwrap();
    let (world, spec) = direct_world(
        name,
        "release-operator",
        "[\"fetch__web\",\"session__todo_write\"]",
        "\n[lancet.network]\nallow_localhost = true\nallowed_hosts = [\"localhost\"]\nmax_web_response_bytes = 1024\n\n[lancet.capabilities]\nmax_boundary = \"ex\"\n",
    );
    let mut scripts = Vec::new();
    for i in 0..turns {
        for method in methods {
            let arguments = if *method == "fetch__web" {
                serde_json::json!({"url":web.url_path("/release")})
            } else {
                serde_json::json!({"todos":[{"content":format!("release {i}"),"status":"pending"}]})
            };
            scripts.push(tool_call(
                &format!("{method}-{i}"),
                method,
                &arguments.to_string(),
            ));
        }
        scripts.push(SseScript::plain(format!("release {i} complete")));
    }
    let server = wire_fake_llm(&world, "release-operator", sequence(scripts));
    let mut evidence = evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let send = SessionSendDriver::new(&driver, &world, &spec, "release-operator");
        let mut tids = Vec::new();
        for i in 0..turns {
            let turn = send.send(&format!("release Turn {i}")).unwrap();
            evidence.record_command(format!("Turn {i}"), &turn.command);
            assert_clean_send(&turn).unwrap();
            assert!(turn.has_completed());
            tids.push(turn.accepted_turn_id().unwrap().to_owned());
        }
        let sid = send.session_id().unwrap();
        let fetches = methods.iter().filter(|m| **m == "fetch__web").count() * turns;
        if fetches > 0 {
            web.wait_for_requests(fetches, Duration::from_secs(5))
                .unwrap();
        }
        let mut read = |args: &[&str]| -> Value {
            let out = cli(&world, &spec, args);
            evidence.record_command(args.join(" "), &out);
            assert!(out.is_clean_exit(0), "{}", out.summary());
            serde_json::from_slice(&out.stdout).unwrap()
        };
        let query = read(&[
            "--json",
            "audit",
            "query",
            "--session",
            sid,
            "--outcome",
            "succeeded",
        ]);
        let rows = query["rows"].as_array().unwrap();
        assert_eq!(rows.len(), methods.len() * turns);
        let ids: HashSet<_> = rows
            .iter()
            .map(|r| r["invocation_id"].as_str().unwrap())
            .collect();
        assert_eq!(ids.len(), rows.len());
        for row in rows {
            assert_eq!(row["binding"]["session_id"], sid);
            assert!(tids.contains(&row["binding"]["turn_id"].as_str().unwrap().to_owned()));
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
        let tail = read(&["--json", "audit", "query", "--session", sid, "--tail"]);
        let changes = tail["rows"].as_array().unwrap();
        assert_eq!(changes.len(), rows.len() * 3);
        assert_eq!(tail["latest_change"], query["latest_change"]);
        for (i, change) in changes.iter().enumerate() {
            assert_eq!(change["sequence"], i + 1);
        }
        for row in rows {
            let states: Vec<_> = changes
                .iter()
                .filter(|c| c["row"]["invocation_id"] == row["invocation_id"])
                .map(|c| c["row"]["state"]["state"].as_str().unwrap())
                .collect();
            assert_eq!(states, ["registered", "started", "terminal"]);
        }
        let cursor = tail["next_after"].as_u64().unwrap().to_string();
        let continuation = read(&[
            "--json",
            "audit",
            "query",
            "--session",
            sid,
            "--tail",
            "--since",
            &cursor,
        ]);
        assert_eq!(continuation["rows"], serde_json::json!([]));
        assert_eq!(continuation["next_after"], tail["next_after"]);
        assert_eq!(continuation["done"], true);
        for tid in &tids {
            let replay = read(&["--json", "audit", "replay", "--session", sid, "--turn", tid]);
            assert_eq!(replay["latest_change"], query["latest_change"]);
            let calls = replay["calls"].as_array().unwrap();
            assert_eq!(
                calls
                    .iter()
                    .map(|r| r["binding"]["method"].as_str().unwrap())
                    .collect::<Vec<_>>(),
                methods
            );
            for row in calls {
                assert!(rows.contains(row));
            }
        }
        let status = read(&["--json", "lancet", "status"]);
        assert_eq!(status["latest_change"], tail["latest_change"]);
        let recent = status["recent"].as_array().unwrap();
        let expected: Vec<_> = changes
            .iter()
            .rev()
            .filter(|c| c["row"]["state"]["state"] == "terminal")
            .take(20)
            .map(|c| &c["row"])
            .collect();
        assert_eq!(recent.iter().collect::<Vec<_>>(), expected);
        assert_eq!(recent.len(), rows.len().min(20));
        if turns == 11 {
            for method in methods {
                assert_eq!(
                    recent
                        .iter()
                        .filter(|r| r["binding"]["method"] == *method)
                        .count(),
                    10
                );
            }
        }
        assert_eq!(
            server.captured_requests().len(),
            turns * (methods.len() + 1)
        );
    }));
    evidence.record_captures(server.captured_requests());
    evidence
        .finish_report(result.err().map(|p| {
            p.downcast_ref::<String>()
                .cloned()
                .unwrap_or_else(|| "release operator assertion".into())
        }))
        .unwrap();
}
#[test]
fn release_operator_component_matrix() {
    release_case("release-operator-fetch", &["fetch__web"], 1);
    release_case("release-operator-component", &["session__todo_write"], 1);
    release_case(
        "release-operator-mixed",
        &["fetch__web", "session__todo_write"],
        11,
    );
}

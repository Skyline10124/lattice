// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0
//! 公开 Session 查询、树形历史与输入拒绝。
mod support;
use lattice_system_harness::{SessionSendDriver, SseScript};
use serde_json::Value;
use support::{assert_clean_send, cli, direct_world, driver, wire_fake_llm};

#[test]
fn session_queries_and_rewind_preserve_the_tree_across_restart() {
    let (world, spec) = direct_world("session-query-tree", "plain", "[]", "");
    let server = wire_fake_llm(&world, "plain", SseScript::plain("durable answer"));
    let driver = driver();
    let send = SessionSendDriver::new(&driver, &world, &spec, "plain");
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let first = send.send("first").unwrap();
        evidence.record_command("first", &first.command);
        assert_clean_send(&first).unwrap();
        let sid = send.session_id().unwrap();
        let tid = first.accepted_turn_id().unwrap();
        let mut query = |args: &[&str]| {
            let out = cli(&world, &spec, args);
            evidence.record_command(args.join(" "), &out);
            assert!(out.is_clean_exit(0), "{}", out.summary());
            serde_json::from_slice::<Value>(&out.stdout).unwrap()
        };
        let list = query(&["--json", "session", "list"]);
        assert_eq!(list.as_array().unwrap().len(), 1);
        assert_eq!(list[0]["session_id"], sid);
        assert_eq!(list[0]["current"], 2);
        assert_eq!(list[0]["node_count"], 3);
        assert!(list[0]["active_turn"].is_null());
        let log = query(&["--json", "session", "log", sid]);
        assert_eq!(log["nodes"].as_array().unwrap().len(), 3);
        assert_eq!(log["nodes"][0]["node"]["id"], 0);
        let turn = query(&["--json", "session", "turn", tid]);
        assert_eq!(turn["summary"]["outcome"], "completed");
        assert_eq!(turn["steps"].as_array().unwrap().len(), 1);
        world.stop_daemon().unwrap();
        assert_eq!(query(&["--json", "session", "log", sid]), log);
        assert_eq!(query(&["--json", "session", "turn", tid]), turn);
        let moved = query(&["--json", "session", "rewind", sid, "0"]);
        assert_eq!(moved["current"], 0);
        let rewound = query(&["--json", "session", "log", sid]);
        assert_eq!(
            rewound["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| (&r["node"], &r["turn_start"]))
                .collect::<Vec<_>>(),
            log["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| (&r["node"], &r["turn_start"]))
                .collect::<Vec<_>>()
        );
        assert_eq!(rewound["session"]["current"], 0);
        let branch = send.send("branch").unwrap();
        evidence.record_command("branch", &branch.command);
        assert_clean_send(&branch).unwrap();
        let tree = cli(&world, &spec, &["session", "log", sid]);
        evidence.record_command("text tree", &tree);
        assert!(tree.is_clean_exit(0));
        let text = String::from_utf8(tree.stdout).unwrap();
        assert!(text.contains("current=4"));
        for node in [
            "  0 root",
            "    1 user",
            "      2 step 0",
            "    3 user",
            "    * 4 step 0",
        ] {
            assert!(
                text.lines().any(|line| line == node),
                "missing tree row {node}: {text}"
            );
        }
        let out = cli(&world, &spec, &["--json", "session", "list"]);
        evidence.record_command("list after branch", &out);
        let list: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(list[0]["node_count"], 5);
        assert_eq!(server.captured_requests().len(), 2);
        support::record_session(
            &send,
            &mut evidence,
            sid,
            serde_json::json!({"prompts":["first","branch"],"rewind":0,"restart":true,"reply":"durable answer"}),
        );
    }));
    evidence.record_captures(server.captured_requests());
    support::finish_recorded(evidence, result);
}

#[test]
fn missing_store_read_does_not_create_a_database() {
    let spec = lattice_system_harness::ScenarioSpec::fast("session-missing-store");
    let world = lattice_system_harness::ScenarioWorld::new(&spec.name).unwrap();
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let out = cli(&world, &spec, &["session", "list"]);
        evidence.record_command("session list", &out);
        lattice_system_harness::assert_exit_code(&out, 1).unwrap();
        lattice_system_harness::assert_stderr_contains(&out, "store unavailable").unwrap();
        let audit = cli(&world, &spec, &["--json", "audit"]);
        evidence.record_command("--json audit", &audit);
        assert!(audit.is_clean_exit(1));
        let exists = world.session_store_dir().join("lattice.redb").exists();
        assert!(!exists);
        evidence
            .record_json(
                "state-summary",
                &serde_json::json!({
                    "database_exists":exists,"log_turn":"不适用：库尚未创建，没有 Session 或 Turn"
                }),
            )
            .unwrap();
        evidence.record_json("parameters", &serde_json::json!({"commands":["session list","--json audit"],"profile":null,"provider":null})).unwrap();
    }));
    support::finish_recorded(evidence, result);
}

#[test]
fn missing_session_and_missing_profile_fail_before_dispatch() {
    let (world, spec) = direct_world("session-lookup-refusal", "plain", "[]", "");
    let server = wire_fake_llm(&world, "plain", SseScript::plain("seed"));
    let driver = driver();
    let send = SessionSendDriver::new(&driver, &world, &spec, "plain");
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let seed = send.send("seed").unwrap();
        evidence.record_command("seed", &seed.command);
        assert_clean_send(&seed).unwrap();
        for args in [
            vec!["--json", "session", "log", "nope-sid"],
            vec![
                "--json",
                "session",
                "send",
                "hello",
                "--session",
                "nope-sid",
            ],
        ] {
            let out = cli(&world, &spec, &args);
            evidence.record_command(args.join(" "), &out);
            assert!(out.is_clean_exit(2), "{}", out.summary());
            let error: Value = serde_json::from_slice(&out.stdout).unwrap();
            assert_eq!(error["kind"], "not_found");
        }
        std::fs::remove_dir_all(world.project_dir().join(".lattice/agents/plain")).unwrap();
        let sid = send.session_id().unwrap();
        let out = cli(
            &world,
            &spec,
            &["--json", "session", "send", "hello", "--session", sid],
        );
        evidence.record_command("profile missing", &out);
        assert!(out.is_clean_exit(2));
        let errors: Vec<Value> = std::str::from_utf8(&out.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(errors.last().unwrap()["kind"], "bad_request");
        lattice_system_harness::assert_no_secret(&out, lattice_system_harness::FAKE_API_KEY)
            .unwrap();
        assert_eq!(server.captured_requests().len(), 1);
        support::record_session(
            &send,
            &mut evidence,
            sid,
            serde_json::json!({"seed":"seed","rejected_prompt":"hello","unknown_session":"nope-sid","profile_removed":true,"provider_requests":1}),
        );
    }));
    evidence.record_captures(server.captured_requests());
    support::finish_recorded(evidence, result);
}

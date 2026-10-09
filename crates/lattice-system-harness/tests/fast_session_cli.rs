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

#[test]
fn configuration_rejection_reasons_are_safe_and_precede_provider_dispatch() {
    use lattice_system_harness::{FAKE_API_KEY, ScenarioEvidence, assert_no_secret};

    const SENTINEL: &str = "sk-config-rejection-91ac75d28e";
    for (case, expected) in [
        ("missing-profile", "agent profile 'missing' not found"),
        ("toml", "line 2"),
        ("missing-key", "'OPENAI_COMPATIBLE_API_KEY' is not set"),
        ("empty-key", "'OPENAI_COMPATIBLE_API_KEY' is empty"),
        ("model", "unknown model alias: '[REDACTED]'"),
        ("model-typo", "unknown model alias: 'sonet'"),
        ("tools", "unknown enabled tools [\"[REDACTED]\"]"),
        ("agent-name", "must match selected agent 'plain'"),
    ] {
        let (world, spec) = direct_world(&format!("config-rejection-{case}"), "plain", "[]", "");
        let server = wire_fake_llm(&world, "plain", SseScript::plain("must not be requested"));
        let path = world.project_dir().join(".lattice/agents/plain/agent.toml");
        let original = std::fs::read_to_string(&path).unwrap();
        match case {
            "toml" => {
                std::fs::write(&path, format!("# 中文\nschema_version=@{SENTINEL}\n")).unwrap()
            }
            "missing-key" => world.unset_env("OPENAI_COMPATIBLE_API_KEY"),
            "empty-key" => world.set_env("OPENAI_COMPATIBLE_API_KEY", ""),
            "model" => std::fs::write(&path, original.replace("compat:plain", SENTINEL)).unwrap(),
            "model-typo" => {
                std::fs::write(&path, original.replace("compat:plain", "sonet")).unwrap()
            }
            "tools" => std::fs::write(
                &path,
                original.replace("enabled = []", &format!("enabled = ['{SENTINEL}']")),
            )
            .unwrap(),
            "agent-name" => std::fs::write(
                &path,
                original.replace(
                    "name = \"plain\"",
                    &format!("name = '{SENTINEL}:{FAKE_API_KEY}'"),
                ),
            )
            .unwrap(),
            "missing-profile" => {}
            _ => unreachable!(),
        }
        let agent = if case == "missing-profile" {
            "missing"
        } else {
            "plain"
        };
        let mut evidence = ScenarioEvidence::new(
            &world,
            &spec.name,
            [FAKE_API_KEY.to_string(), SENTINEL.to_string()],
        )
        .unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut commands = vec![
                vec!["--json", "session", "send", "hello", "--agent", agent],
                vec!["--json", "run", "hello", "--agent", agent],
            ];
            // 工具选择归后台接受阶段；启动预检只在进入终端前加载并冻结配置。
            if case != "tools" {
                commands.push(vec!["--json", "tui", "--agent", agent]);
            }
            let mut messages = Vec::new();
            for args in commands {
                let out = cli(&world, &spec, &args);
                evidence.record_command(args.join(" "), &out);
                assert!(out.is_clean_exit(2), "{}", out.summary());
                assert_no_secret(&out, SENTINEL).unwrap();
                assert_no_secret(&out, FAKE_API_KEY).unwrap();
                let records: Vec<Value> = std::str::from_utf8(&out.stdout)
                    .unwrap()
                    .lines()
                    .map(|line| serde_json::from_str(line).unwrap())
                    .collect();
                let error = records.last().expect("final error record");
                assert_eq!(error["type"], "error");
                assert_eq!(error["kind"], "bad_request");
                let message = error["message"].as_str().unwrap();
                assert!(
                    message.contains(expected),
                    "missing expected rejection reason"
                );
                if case == "agent-name" {
                    // 测试 key 不符合形状规则，必须由提交端读取已知凭据后精确替换。
                    assert!(message.contains("declares '[REDACTED]:[REDACTED]'"));
                }
                if case == "tools" {
                    assert!(message.contains("available catalog tools:"));
                    assert!(message.contains("observe.workspace"));
                }
                messages.push(message.to_owned());
                assert!(server.captured_requests().is_empty());
            }
            assert!(messages.windows(2).all(|pair| pair[0] == pair[1]));
            evidence
                .record_json(
                    "parameters",
                    &serde_json::json!({
                        "case": case, "provider_requests": 0, "accepted": false
                    }),
                )
                .unwrap();
        }));
        evidence.record_captures(server.captured_requests());
        support::finish_recorded(evidence, result);
    }
}

// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0
//! 公开 Session 查询、树形历史与输入拒绝。
mod support;
use lattice_system_harness::{PtyTuiDriver, SessionSendDriver, SseScript};
use serde_json::Value;
use support::{assert_clean_send, cli, direct_world, driver, parse_send_stdout, wire_fake_llm};

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
        assert!(list[0]["unclosed_turn"].is_null());
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
        // 配置在请求写出前加载失败：`sending` 之后以 `rejected` 回执结束。
        let sent = parse_send_stdout(&out.stdout, out.exit_code());
        assert_eq!(
            sent.rejection().map(|error| error.kind.as_str()),
            Some("bad_request"),
            "{:?}",
            sent.type_sequence()
        );
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
    const ORDINARY_KEY: &str = "ordinary.secret.value";
    const LONG_FRAGMENT: &str = "ordinary.long.profile.fragment";
    let long_key = format!("{LONG_FRAGMENT}.").repeat(100);
    for (case, expected) in [
        ("missing-profile", "agent profile 'missing' not found"),
        ("toml", "line 2"),
        ("missing-key", "credentials are missing"),
        ("invalid-auth", "auth.json:"),
        ("provider-missing-invalid-auth", "available providers:"),
        ("provider-config", "providers.toml"),
        ("provider-config-known-key", "providers.toml"),
        ("model", "model reference must be <provider>/<model>"),
        ("model-typo", "model reference must be <provider>/<model>"),
        ("tools", "unknown enabled tools [\"[REDACTED]\"]"),
        ("tools-known-key", "unknown enabled tools [\"[REDACTED]\"]"),
        ("short-key", "unknown enabled tools [\"EMPTY\"]"),
        ("agent-name", "must match selected agent 'plain'"),
        ("long-profile", "declares '[REDACTED]'"),
        ("responses", "protocol responses is not implemented"),
        ("interactions", "protocol interactions is not implemented"),
        ("oauth-chatgpt", "auth oauth:chatgpt is not implemented"),
    ] {
        let (world, spec) = direct_world(&format!("config-rejection-{case}"), "plain", "[]", "");
        let server = wire_fake_llm(&world, "plain", SseScript::plain("must not be requested"));
        let path = world.project_dir().join(".lattice/agents/plain/agent.toml");
        let original = std::fs::read_to_string(&path).unwrap();
        let provider = lattice_system_harness::ProviderFixture::chat_completions("plain");
        let model = provider.model_ref().unwrap();
        match case {
            "toml" => {
                std::fs::write(&path, format!("# 中文\nschema_version=@{SENTINEL}\n")).unwrap()
            }
            // 分别验证凭据缺失与凭据文件损坏的拒绝诊断。
            "missing-key" => world.register_key(provider, None).unwrap(),
            "invalid-auth" => {
                std::fs::write(world.home().join(".lattice/auth.json"), "invalid").unwrap()
            }
            "provider-missing-invalid-auth" => {
                std::fs::write(world.home().join(".lattice/auth.json"), "invalid").unwrap();
                std::fs::write(&path, original.replace(&model, "absent/model")).unwrap();
            }
            "provider-config" => std::fs::write(
                world.home().join(".lattice/providers.toml"),
                format!("[providers.local]\nprotocol='{SENTINEL}:{FAKE_API_KEY}'\n"),
            )
            .unwrap(),
            "provider-config-known-key" => {
                world.register_key(provider, Some(ORDINARY_KEY)).unwrap();
                std::fs::write(
                    world.home().join(".lattice/providers.toml"),
                    format!("[providers.local]\nprotocol='{ORDINARY_KEY}'\n"),
                )
                .unwrap();
            }
            "model" => std::fs::write(&path, original.replace(&model, SENTINEL)).unwrap(),
            "model-typo" => std::fs::write(&path, original.replace(&model, "sonet")).unwrap(),
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
            "long-profile" => {
                world.register_key(provider, Some(&long_key)).unwrap();
                std::fs::write(
                    &path,
                    original.replace("name = \"plain\"", &format!("name = '{long_key}'")),
                )
                .unwrap();
            }
            "tools-known-key" => {
                world.register_key(provider, Some(ORDINARY_KEY)).unwrap();
                std::fs::write(
                    &path,
                    original.replace("enabled = []", &format!("enabled = ['{ORDINARY_KEY}']")),
                )
                .unwrap();
            }
            "short-key" => {
                world.register_key(provider, Some("EMPTY")).unwrap();
                std::fs::write(
                    &path,
                    original.replace("enabled = []", "enabled = ['EMPTY']"),
                )
                .unwrap();
            }
            "responses" | "interactions" => {
                std::fs::write(
                    world.home().join(".lattice/providers.toml"),
                    format!(
                        "[providers.local]\nprotocol='{case}'\nbase_url='{}'\nauth='none'\n",
                        server.base_url()
                    ),
                )
                .unwrap();
            }
            "oauth-chatgpt" => {
                std::fs::write(world.home().join(".lattice/providers.toml"), "").unwrap();
                std::fs::write(&path, original.replace(&model, "chatgpt/plain")).unwrap();
            }
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
            [
                FAKE_API_KEY.to_string(),
                SENTINEL.to_string(),
                ORDINARY_KEY.into(),
                long_key.clone(),
                LONG_FRAGMENT.into(),
            ],
        )
        .unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let commands = [
                vec!["--json", "session", "send", "hello", "--agent", agent],
                vec!["--json", "run", "hello", "--agent", agent],
            ];
            let mut messages = Vec::new();
            for args in commands {
                let out = cli(&world, &spec, &args);
                evidence.record_command(args.join(" "), &out);
                assert!(out.is_clean_exit(2), "{}", out.summary());
                assert_no_secret(&out, SENTINEL).unwrap();
                assert_no_secret(&out, FAKE_API_KEY).unwrap();
                assert_no_secret(&out, ORDINARY_KEY).unwrap();
                assert_no_secret(&out, &long_key).unwrap();
                assert_no_secret(&out, LONG_FRAGMENT).unwrap();
                let sent = parse_send_stdout(&out.stdout, out.exit_code());
                let error = sent.rejection().unwrap_or_else(|| {
                    panic!("rejected receipt expected: {:?}", sent.type_sequence())
                });
                assert_eq!(error.kind, "bad_request");
                let message = error.message.as_str();
                assert!(
                    message.contains(expected),
                    "{case}: missing expected rejection reason"
                );
                if case == "agent-name" {
                    // 后台按已知凭据值和密钥形状脱敏，前端不读取 key。
                    assert!(message.contains("declares '[REDACTED]:[REDACTED]'"));
                }
                if matches!(case, "tools" | "tools-known-key") {
                    assert!(message.contains("available catalog tools:"));
                    assert!(message.contains("observe__workspace"));
                }
                if case == "provider-missing-invalid-auth" {
                    for provider in ["anthropic", "openai", "google", "local"] {
                        assert!(message.contains(provider));
                    }
                }
                if case == "provider-config-known-key" {
                    assert!(message.contains("[REDACTED]"));
                }
                messages.push(message.to_owned());
                assert!(sent.accepted_turn_id().is_none());
                assert!(server.captured_requests().is_empty());
            }
            assert!(messages.windows(2).all(|pair| pair[0] == pair[1]));
            if matches!(case, "toml" | "tools-known-key") {
                let budget = world.budget(&spec);
                let mut tui = PtyTuiDriver::spawn(&driver(), &world, agent).unwrap();
                let pty_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    tui.wait_for_ready(budget.cap_timeout(spec.step_timeout()))
                        .unwrap();
                    tui.send_line("hello").unwrap();
                    tui.wait_for(budget.cap_timeout(spec.step_timeout()), |text| {
                        text.contains("被拒绝")
                    })
                    .unwrap();
                    let frame = tui
                        .redraw_text(budget.cap_timeout(spec.step_timeout()))
                        .unwrap();
                    // 终端折行会把诊断拆开；比较完整文字，不依赖列宽与换行位置。
                    let compact = |text: &str| {
                        text.chars()
                            .filter(|c| !c.is_whitespace())
                            .collect::<String>()
                    };
                    assert!(
                        compact(&frame).contains(&compact(&messages[0])),
                        "{case}: {frame}"
                    );
                    for secret in [
                        SENTINEL,
                        FAKE_API_KEY,
                        ORDINARY_KEY,
                        &long_key,
                        LONG_FRAGMENT,
                    ] {
                        assert!(
                            !compact(&frame).contains(&compact(secret)),
                            "{case}: unredacted credential"
                        );
                    }
                    // 关闭错误消息后编辑归还的输入，避免把提交记录误认为输入仍在。
                    tui.send_keys(b" -preserved").unwrap();
                    tui.wait_for(budget.cap_timeout(spec.step_timeout()), |text| {
                        text.contains("preserved")
                    })
                    .unwrap();
                    let frame = tui
                        .redraw_text(budget.cap_timeout(spec.step_timeout()))
                        .unwrap();
                    assert!(frame.contains("hello-preserved"), "{case}: {frame}");
                    tui.send_keys(b"\x03").unwrap();
                    tui.send_line("/quit").unwrap();
                    tui.wait_for_exit(budget.cap_timeout(spec.step_timeout()))
                        .unwrap();
                    assert!(server.captured_requests().is_empty());
                }));
                evidence.record_pty(tui.output());
                drop(tui);
                if let Err(payload) = pty_result {
                    std::panic::resume_unwind(payload);
                }
            }
            if matches!(case, "long-profile" | "tools-known-key") {
                let out = cli(
                    &world,
                    &spec,
                    &["session", "send", "hello", "--agent", agent],
                );
                evidence.record_command("configuration rejection text", &out);
                assert!(out.is_clean_exit(2));
                assert_no_secret(&out, ORDINARY_KEY).unwrap();
                assert_no_secret(&out, &long_key).unwrap();
                assert_no_secret(&out, LONG_FRAGMENT).unwrap();
                assert!(String::from_utf8_lossy(&out.stderr).contains(expected));
            }
            if world.session_store_dir().join("lattice.redb").exists() {
                let sessions = cli(&world, &spec, &["--json", "session", "list"]);
                evidence.record_command("no Session before acceptance", &sessions);
                assert!(sessions.is_clean_exit(0));
                let sessions: serde_json::Value = serde_json::from_slice(&sessions.stdout).unwrap();
                assert!(sessions.as_array().unwrap().is_empty());
            }
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

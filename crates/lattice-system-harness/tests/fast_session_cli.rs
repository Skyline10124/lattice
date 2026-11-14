// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0
//! 公开 Session 查询、树形历史与输入拒绝。
mod support;
use lattice_system_harness::{PtyTuiDriver, SessionSendDriver, SseScript};
use serde_json::Value;
use support::{assert_clean_send, cli, direct_world, driver, parse_send_stdout, wire_fake_llm};

#[test]
fn session_queries_and_rewind_preserve_the_tree_across_restart() {
    let (world, spec) = direct_world("session-query-tree", "plain", "");
    world.start_daemon().expect("显式启动后台");
    let server = wire_fake_llm(&world, "plain", SseScript::plain("durable answer"));
    let driver = driver();
    let send = SessionSendDriver::new(&driver, &world, &spec, "plain");
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let first = send.send("first").unwrap();
        evidence.record_command("首轮会话发送", &first.command);
        assert_clean_send(&first).unwrap();
        let sid = send.session_id().unwrap();
        let tid = first.accepted_turn_id().unwrap();
        let mut query = |args: &[&str]| {
            let out = cli(&world, &spec, args);
            evidence.record_command(format!("查询会话状态（{}）", args.join(" ")), &out);
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
        world
            .restart_daemon()
            .expect("显式重启后台后再查询持久历史");
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
        evidence.record_command("回退后分叉发送", &branch.command);
        assert_clean_send(&branch).unwrap();
        let tree = cli(&world, &spec, &["session", "log", sid]);
        evidence.record_command("查询会话树文本输出", &tree);
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
                "缺少预期会话树节点行 `{node}`：{text}"
            );
        }
        let out = cli(&world, &spec, &["--json", "session", "list"]);
        evidence.record_command("分叉后查询会话列表", &out);
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
    world.budget(&spec);
    world.start_daemon().expect("显式启动后台");
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let out = cli(&world, &spec, &["session", "list"]);
        evidence.record_command("查询空会话列表", &out);
        lattice_system_harness::assert_exit_code(&out, 1).unwrap();
        lattice_system_harness::assert_stderr_contains(&out, "store unavailable").unwrap();
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
        evidence
            .record_json(
                "parameters",
                &serde_json::json!({"commands":["session list"],"profile":null,"provider":null}),
            )
            .unwrap();
    }));
    support::finish_recorded(evidence, result);
}

#[test]
fn missing_session_and_missing_profile_fail_before_dispatch() {
    let (world, spec) = direct_world("session-lookup-refusal", "plain", "");
    world.start_daemon().expect("显式启动后台");
    let server = wire_fake_llm(&world, "plain", SseScript::plain("seed"));
    let driver = driver();
    let send = SessionSendDriver::new(&driver, &world, &spec, "plain");
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let seed = send.send("seed").unwrap();
        evidence.record_command("建立初始会话种子", &seed.command);
        assert_clean_send(&seed).unwrap();
        for args in [
            vec!["--json", "session", "log", "nope-sid"],
            vec!["--json", "session", "usage", "nope-sid"],
            vec![
                "--json",
                "session",
                "send",
                "hello",
                "--session",
                "nope-sid",
            ],
        ] {
            let out = send.run_command(&args).unwrap();
            evidence.record_command(format!("校验不存在的会话（{}）", args.join(" ")), &out);
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
        evidence.record_command("删除源配置后沿用已继承副本发送", &out);
        assert!(out.is_clean_exit(0), "{}", out.summary());
        let inherited = parse_send_stdout(&out.stdout, out.exit_code());
        support::assert_accepted_and_closed(&inherited).unwrap();
        for args in [
            vec![
                "--json",
                "session",
                "send",
                "refresh missing",
                "--session",
                sid,
                "--agent",
                "plain",
            ],
            vec![
                "--json",
                "session",
                "send",
                "new missing",
                "--agent",
                "plain",
            ],
        ] {
            let rejected = cli(&world, &spec, &args);
            evidence.record_command("拒绝显式指定的不存在配置", &rejected);
            assert!(rejected.is_clean_exit(2), "{}", rejected.summary());
            let sent = parse_send_stdout(&rejected.stdout, rejected.exit_code());
            assert_eq!(
                sent.rejection().map(|error| error.kind.as_str()),
                Some("bad_request"),
                "{:?}",
                sent.type_sequence()
            );
            assert!(
                sent.rejection()
                    .unwrap()
                    .message
                    .contains("agent profile 'plain' not found")
            );
            lattice_system_harness::assert_no_secret(
                &rejected,
                lattice_system_harness::FAKE_API_KEY,
            )
            .unwrap();
        }
        let oversized_agent = "a".repeat(129);
        for subcommand in [vec!["session", "send"], vec!["run"]] {
            let mut args = subcommand;
            args.extend(["oversized agent", "--agent", &oversized_agent]);
            let rejected = cli(&world, &spec, &args);
            evidence.record_command("拒绝超长的 CLI --agent 参数", &rejected);
            assert!(rejected.is_clean_exit(2), "{}", rejected.summary());
            let stdout = String::from_utf8_lossy(&rejected.stdout);
            let stderr = String::from_utf8_lossy(&rejected.stderr);
            assert!(
                stderr.contains("agent name must be at most 128 bytes"),
                "标准错误应报告 agent 名称长度限制错误，实际输出：{stderr}"
            );
            assert!(
                !stdout.contains("new Session requires explicit agent")
                    && !stderr.contains("new Session requires explicit agent"),
                "超长 --agent 不得误报缺少显式 agent"
            );
        }
        assert_eq!(server.captured_requests().len(), 2);
        support::record_session(
            &send,
            &mut evidence,
            sid,
            serde_json::json!({"seed":"seed","inherited_prompt":"hello","rejected_prompt":"refresh missing","unknown_session":"nope-sid","profile_removed":true,"provider_requests":2}),
        );
    }));
    evidence.record_captures(server.captured_requests());
    support::finish_recorded(evidence, result);
}

#[test]
fn usage_resolves_exact_identity_and_rejects_ambiguous_prefixes() {
    use serde_json::json;
    let (world, spec) = direct_world("usage-prefix", "plain", "");
    let budget = world.budget(&spec);
    world.start_daemon().expect("显式启动后台");
    let driver = driver();
    let send = SessionSendDriver::new(&driver, &world, &spec, "plain");
    let server = wire_fake_llm(&world, "plain", SseScript::plain("seed"));
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        for (sid, tid) in [
            (
                "abcd1111111141118111111111111111",
                "10000000-0000-4000-8000-000000000001",
            ),
            (
                "abcd2222222242228222222222222222",
                "20000000-0000-4000-8000-000000000002",
            ),
        ] {
            let configured = support::ipc(
                &world,
                budget.cap_timeout(spec.step_timeout()),
                support::configure_request(&world, json!({"new":sid}), Some("plain"), None, None),
            );
            assert_eq!(configured["response"], "configure", "{configured}");
            let accepted = support::ipc(
                &world,
                budget.cap_timeout(spec.step_timeout()),
                support::submit_request(&world, tid, sid, "seed"),
            );
            evidence.record_json("acceptance", &accepted).unwrap();
            assert_eq!(accepted["value"]["acceptance"], "accepted");
            for prefix in [sid, &sid[..8]] {
                let out = send
                    .run_command(&["session", "usage", prefix, "--json"])
                    .unwrap();
                evidence.record_command("按精确标识或唯一前缀查询会话用量", &out);
                assert!(out.is_clean_exit(0), "{}", out.summary());
                let usage: Value = serde_json::from_slice(&out.stdout).unwrap();
                assert_eq!(usage["session_id"], sid);
            }
        }
        for (prefix, kind) in [
            ("abcd", "bad_request"),
            ("abc", "bad_request"),
            (" ", "bad_request"),
            ("ffff0000000000000000000000000000", "not_found"),
        ] {
            let out = send
                .run_command(&["--json", "session", "usage", prefix])
                .unwrap();
            evidence.record_command("拒绝无效或歧义的会话用量前缀", &out);
            assert!(out.is_clean_exit(2), "{}", out.summary());
            let error: Value = serde_json::from_slice(&out.stdout).unwrap();
            assert_eq!(error["kind"], kind);
        }
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
        ("missing-key", "lattice auth set local"),
        ("invalid-auth", "auth.json:"),
        ("provider-missing-invalid-auth", "available providers:"),
        ("provider-config", "providers.toml"),
        ("provider-config-known-key", "providers.toml"),
        ("model", "model reference must be <provider>/<model>"),
        ("model-typo", "model reference must be <provider>/<model>"),
        (
            "tools",
            "`[tools]` has been removed; delete this section from agent.toml",
        ),
        (
            "lancet",
            "`[lancet]` has been removed; delete this section from agent.toml",
        ),
        (
            "known-key",
            "invalid value `[REDACTED]` for `name` in [agent]",
        ),
        ("short-key", "invalid value `EMPTY` for `name` in [agent]"),
        ("agent-name", "expected `plain`"),
        (
            "long-profile",
            "invalid value `[REDACTED]` for `name` in [agent]",
        ),
        ("oauth-chatgpt", "ChatGPT is not signed in"),
        ("cli-provider", "available providers:"),
        ("cli-effort", "does not support reasoning effort off"),
    ] {
        let (world, spec) = direct_world(&format!("config-rejection-{case}"), "plain", "");
        world.start_daemon().expect("显式启动后台");
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
                format!("[metadata]\nauto_refresh=false\n[providers.local]\nprotocol='{SENTINEL}:{FAKE_API_KEY}'\n"),
            )
            .unwrap(),
            "provider-config-known-key" => {
                world.register_key(provider, Some(ORDINARY_KEY)).unwrap();
                std::fs::write(
                    world.home().join(".lattice/providers.toml"),
                    format!("[metadata]\nauto_refresh=false\n[providers.local]\nprotocol='{ORDINARY_KEY}'\n"),
                )
                .unwrap();
            }
            "model" => std::fs::write(&path, original.replace(&model, SENTINEL)).unwrap(),
            "model-typo" => std::fs::write(&path, original.replace(&model, "sonet")).unwrap(),
            "tools" => std::fs::write(
                &path,
                format!("{original}\n[tools]\nenabled = ['{SENTINEL}']\n"),
            )
            .unwrap(),
            "lancet" => std::fs::write(
                &path,
                format!("{original}\n[lancet.workspace]\ndenied_write_patterns = ['{SENTINEL}']\n"),
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
            "known-key" => {
                world.register_key(provider, Some(ORDINARY_KEY)).unwrap();
                std::fs::write(
                    &path,
                    original.replace("name = \"plain\"", &format!("name = '{ORDINARY_KEY}'")),
                )
                .unwrap();
            }
            "short-key" => {
                world.register_key(provider, Some("EMPTY")).unwrap();
                std::fs::write(&path, original.replace("name = \"plain\"", "name = 'EMPTY'"))
                    .unwrap();
            }
            "oauth-chatgpt" => {
                std::fs::write(world.home().join(".lattice/providers.toml"), "[metadata]\nauto_refresh=false\n").unwrap();
                std::fs::write(&path, original.replace(&model, "chatgpt/plain")).unwrap();
            }
            "missing-profile" | "cli-provider" | "cli-effort" => {}
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
            for mut args in commands {
                if case == "model-typo" {
                    args.extend(["--model", &model]);
                }
                if case == "cli-provider" {
                    args.extend(["--model", "absent/model"]);
                }
                if case == "cli-effort" {
                    args.extend(["--effort", "off"]);
                }
                let out = cli(&world, &spec, &args);
                evidence.record_command(format!("校验错误配置拒绝（{}）", args.join(" ")), &out);
                assert!(out.is_clean_exit(2), "{}", out.summary());
                assert_no_secret(&out, SENTINEL).unwrap();
                assert_no_secret(&out, FAKE_API_KEY).unwrap();
                assert_no_secret(&out, ORDINARY_KEY).unwrap();
                assert_no_secret(&out, &long_key).unwrap();
                assert_no_secret(&out, LONG_FRAGMENT).unwrap();
                let sent = parse_send_stdout(&out.stdout, out.exit_code());
                let error = sent.rejection().unwrap_or_else(|| {
                    panic!("预期收到拒绝回执，实际序列：{:?}", sent.type_sequence())
                });
                assert_eq!(error.kind, "bad_request");
                let message = error.message.as_str();
                assert!(
                    message.contains(expected),
                    "{case}：拒绝原因未包含预期内容：`{message}`"
                );
                if case == "model-typo" {
                    let canonical_profile = path.canonicalize().unwrap();
                    assert!(
                        message.contains(&canonical_profile.display().to_string()),
                        "{case}：拒绝信息缺少配置文件路径：{message}"
                    );
                    assert!(
                        message.contains("for `model` in [agent]"),
                        "{case}：拒绝信息缺少 `[agent].model` 位置说明：{message}"
                    );
                }
                if case == "agent-name" {
                    // 后台按已知凭据值和密钥形状脱敏，前端不读取密钥。
                    assert!(
                        message.contains(
                            "invalid value `[REDACTED]:[REDACTED]` for `name` in [agent]"
                        )
                    );
                }
                if case == "tools" {
                    assert!(message.contains("forbidden top-level section [tools]"));
                    assert!(!message.contains("available catalog tools:"));
                }
                if case == "lancet" {
                    assert!(message.contains("forbidden top-level section [lancet]"));
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
            if matches!(case, "toml" | "known-key") {
                // `lattice tui --agent` 在启动时执行 `configure(New)`，配置文件错误在进入主屏幕前即退出。
                let out = cli(&world, &spec, &["tui", "--agent", agent]);
                evidence.record_command("TUI 启动阶段拒绝错误配置", &out);
                assert!(out.is_clean_exit(2), "{}", out.summary());
                assert_no_secret(&out, SENTINEL).unwrap();
                assert_no_secret(&out, FAKE_API_KEY).unwrap();
                assert_no_secret(&out, ORDINARY_KEY).unwrap();
                assert_no_secret(&out, &long_key).unwrap();
                assert_no_secret(&out, LONG_FRAGMENT).unwrap();
                let stderr = String::from_utf8_lossy(&out.stderr);
                assert!(stderr.contains(&messages[0]), "{case}: {stderr}");
            }
            if case == "missing-key" {
                // 凭据不在启动 `configure(New)` 时核对，而在提交准入时拒绝并归还输入。
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
                            "{case}：终端帧包含未脱敏的凭据"
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
            if matches!(case, "long-profile" | "tools" | "known-key") {
                let out = cli(
                    &world,
                    &spec,
                    &["session", "send", "hello", "--agent", agent],
                );
                evidence.record_command("配置拒绝的文本模式输出", &out);
                assert!(out.is_clean_exit(2));
                assert_no_secret(&out, ORDINARY_KEY).unwrap();
                assert_no_secret(&out, &long_key).unwrap();
                assert_no_secret(&out, LONG_FRAGMENT).unwrap();
                assert!(String::from_utf8_lossy(&out.stderr).contains(expected));
            }
            if world.session_store_dir().join("lattice.redb").exists() {
                let sessions = cli(&world, &spec, &["--json", "session", "list"]);
                evidence.record_command("拒绝后检查会话列表状态", &sessions);
                assert!(sessions.is_clean_exit(0));
                let sessions: serde_json::Value = serde_json::from_slice(&sessions.stdout).unwrap();
                if matches!(case, "missing-key" | "invalid-auth" | "oauth-chatgpt") {
                    // 凭据在 `submit` 阶段核对：`configure(New)` 已创建只有根节点的草稿，但未接受任何轮次。
                    assert!(
                        sessions
                            .as_array()
                            .unwrap()
                            .iter()
                            .all(|session| session["node_count"] == 1),
                        "{case}: {sessions}"
                    );
                } else {
                    assert!(
                        sessions.as_array().unwrap().is_empty(),
                        "{case}: {sessions}"
                    );
                }
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

#[test]
fn non_utf8_project_path_round_trips_bytes_and_enforces_binding() {
    use lattice_system_harness::RunningCommand;
    use std::ffi::OsString;
    use std::os::unix::ffi::{OsStrExt, OsStringExt};

    let (world, spec) = direct_world("session-non-utf8-project-path", "plain", "");
    world.start_daemon().expect("显式启动后台");
    let server = wire_fake_llm(
        &world,
        "plain",
        support::sequence(vec![
            SseScript::plain("first non-utf8 reply"),
            SseScript::plain("second non-utf8 reply"),
            SseScript::plain("must not run"),
        ]),
    );
    let driver = driver();
    let budget = world.budget(&spec);
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let non_utf8_dir = world
            .root()
            .join(OsString::from_vec(b"project-\xff-workspace".to_vec()));
        std::fs::create_dir_all(non_utf8_dir.join(".lattice/agents/plain")).unwrap();
        std::fs::copy(
            world.project_dir().join(".lattice/agents/plain/agent.toml"),
            non_utf8_dir.join(".lattice/agents/plain/agent.toml"),
        )
        .unwrap();
        let canonical = non_utf8_dir.canonicalize().unwrap();
        let expected_bytes: Vec<Value> = canonical
            .as_os_str()
            .as_bytes()
            .iter()
            .map(|byte| Value::from(*byte))
            .collect();
        let expected_lossy = canonical.to_string_lossy().into_owned();
        assert!(expected_lossy.contains('\u{fffd}'));

        let run_in =
            |dir: &std::path::Path,
             label: &str,
             args: &[&str],
             evidence: &mut lattice_system_harness::ScenarioEvidence| {
                let mut cmd = world.command(driver.binary_path());
                cmd.current_dir(dir).args(args);
                let out = RunningCommand::spawn(cmd)
                    .unwrap()
                    .wait(Some(budget.cap_timeout(spec.step_timeout())))
                    .unwrap();
                evidence.record_command(format!("{label}: {}", args.join(" ")), &out);
                out
            };

        // 在含 `0xff` 字节的项目目录中新建会话并完成首轮发送。
        let first = run_in(
            &non_utf8_dir,
            "非 UTF-8 目录首轮发送",
            &[
                "--json",
                "session",
                "send",
                "first from non-utf8",
                "--agent",
                "plain",
            ],
            &mut evidence,
        );
        assert!(first.is_clean_exit(0), "{}", first.summary());
        let first_parsed = parse_send_stdout(&first.stdout, first.exit_code());
        assert!(first_parsed.malformed_lines.is_empty());
        support::assert_accepted_and_closed(&first_parsed).unwrap();
        let sid = first_parsed.accepted_session_id().unwrap().to_owned();

        // `session list` 文本输出以替换字符显示路径；`--json` 保留与规范路径完全一致的原始字节数组。
        let list_text_out = run_in(
            &non_utf8_dir,
            "非 UTF-8 目录文本会话列表",
            &["session", "list"],
            &mut evidence,
        );
        assert!(
            list_text_out.is_clean_exit(0),
            "{}",
            list_text_out.summary()
        );
        let list_text = String::from_utf8(list_text_out.stdout).unwrap();
        assert!(list_text.contains('\u{fffd}'), "{list_text}");
        assert!(
            list_text.contains(&format!("project={expected_lossy}")),
            "{list_text}"
        );

        let list_json_out = run_in(
            &non_utf8_dir,
            "非 UTF-8 目录 JSON 会话列表",
            &["--json", "session", "list"],
            &mut evidence,
        );
        assert!(
            list_json_out.is_clean_exit(0),
            "{}",
            list_json_out.summary()
        );
        let list_json: Value = serde_json::from_slice(&list_json_out.stdout).unwrap();
        assert_eq!(list_json.as_array().unwrap().len(), 1, "{list_json}");
        assert_eq!(list_json[0]["session_id"], sid);
        assert_eq!(
            list_json[0]["project_path"],
            Value::Array(expected_bytes.clone())
        );

        // 在同一非 UTF-8 目录继续 `--session S` 发送成功。
        let second = run_in(
            &non_utf8_dir,
            "非 UTF-8 目录续发第二轮",
            &[
                "--json",
                "session",
                "send",
                "second from non-utf8",
                "--session",
                &sid,
            ],
            &mut evidence,
        );
        assert!(second.is_clean_exit(0), "{}", second.summary());
        let second_parsed = parse_send_stdout(&second.stdout, second.exit_code());
        assert!(second_parsed.malformed_lines.is_empty());
        support::assert_accepted_and_closed(&second_parsed).unwrap();
        assert_eq!(server.captured_requests().len(), 2);

        // 切到另一个目录对同一会话发送：返回项目绑定不一致冲突，退出码 2，且不写新节点。
        let mismatch = run_in(
            world.project_dir(),
            "其他目录发送触发绑定冲突",
            &[
                "--json",
                "session",
                "send",
                "wrong directory",
                "--session",
                &sid,
            ],
            &mut evidence,
        );
        assert!(mismatch.is_clean_exit(2), "{}", mismatch.summary());
        let mismatch_parsed = parse_send_stdout(&mismatch.stdout, mismatch.exit_code());
        assert!(mismatch_parsed.malformed_lines.is_empty());
        let rejection = mismatch_parsed
            .rejection()
            .expect("跨项目目录发送应以冲突拒绝");
        assert_eq!(rejection.kind, "conflict", "{rejection:?}");
        assert_eq!(
            rejection.conflict.as_deref(),
            Some("binding_mismatch"),
            "{rejection:?}"
        );
        assert_eq!(server.captured_requests().len(), 2);

        let log_out = run_in(
            &non_utf8_dir,
            "非 UTF-8 目录 JSON 会话日志",
            &["--json", "session", "log", &sid],
            &mut evidence,
        );
        assert!(log_out.is_clean_exit(0), "{}", log_out.summary());
        let log_json: Value = serde_json::from_slice(&log_out.stdout).unwrap();
        assert_eq!(
            log_json["session"]["project_path"],
            Value::Array(expected_bytes)
        );
        assert_eq!(log_json["nodes"].as_array().unwrap().len(), 5);
    }));
    evidence.record_captures(server.captured_requests());
    support::finish_recorded(evidence, result);
}

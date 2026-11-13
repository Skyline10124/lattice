// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0
//! 公开树/详情与 provider 上下文互证；接受前拒绝不建立 Session。
mod support;
use lattice_system_harness::{
    ScenarioSpec, ScenarioWorld, SendRecord, SessionSendDriver, SseScript, Submit,
};
use serde_json::Value;
use support::*;

#[test]
fn direct_turn_completed_reopen_contract() {
    let driver = driver();
    let (world, spec) = direct_world("direct-turn-contract", "dl-contract", "");
    world.budget(&spec);
    world.start_daemon().expect("显式启动后台");
    let server = wire_fake_llm(
        &world,
        "dl-contract",
        sequence(vec![
            SseScript::plain("first answer"),
            SseScript::plain("second answer"),
        ]),
    );
    let mut evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-contract");
    let first = send.send("first prompt").unwrap();
    let sid = send.session_id().unwrap();
    let before = cli(
        &world,
        &spec,
        &[
            "--json",
            "session",
            "turn",
            first.accepted_turn_id().unwrap(),
        ],
    );
    let second = send.send("second prompt").unwrap();
    let after = cli(
        &world,
        &spec,
        &[
            "--json",
            "session",
            "turn",
            first.accepted_turn_id().unwrap(),
        ],
    );
    let log = cli(&world, &spec, &["--json", "session", "log", sid]);
    let human = cli(&world, &spec, &["session", "log", sid]);
    let list = cli(&world, &spec, &["--json", "session", "list"]);
    let missing = cli(
        &world,
        &spec,
        &[
            "--json",
            "session",
            "send",
            "hello",
            "--session",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ],
    );
    let result = (|| {
        for turn in [&first, &second] {
            assert_clean_send(turn)?;
            assert_accepted_and_closed(turn)?;
            require(
                turn.has_completed() && turn.command.is_clean_exit(0),
                "Completed and exit0",
            )?;
        }
        require(
            before.is_clean_exit(0) && after.is_clean_exit(0) && before.stdout == after.stdout,
            "closed Turn unchanged after new Turn",
        )?;
        let detail: Value = serde_json::from_slice(&before.stdout).unwrap();
        require(
            detail["steps"][0]["attempts"][0]["origin"]
                == serde_json::json!({"protocol":"chat-completions","provider_id":"local"}),
            "completed attempt records protocol and provider id",
        )?;
        let captures = server.captured_requests();
        require(
            captures.len() == 2 && captures.iter().all(|c| c.model == "dl-contract"),
            "two correctly routed requests",
        )?;
        let messages = &captures[1].raw["messages"];
        require(
            messages.as_array().unwrap().len() == 4
                && messages[0]["role"] == "system"
                && messages[1]["role"] == "user"
                && messages[1]["content"] == "first prompt"
                && messages[2]["role"] == "assistant"
                && messages[2]["content"] == "first answer"
                && messages[3]["role"] == "user"
                && messages[3]["content"] == "second prompt",
            "exact prior pair precedes new input",
        )?;
        let tree: Value = serde_json::from_slice(&log.stdout).unwrap();
        require(
            log.is_clean_exit(0)
                && tree["nodes"].as_array().unwrap().len() == 5
                && tree["session"]["current"] == 4,
            "complete tree and current",
        )?;
        for turn in [&first, &second] {
            require(
                tree["nodes"].as_array().unwrap().iter().any(|r| {
                    r["turn_start"]["turn_id"] == turn.accepted_turn_id().unwrap()
                        && r["turn_start"]["summary"]["outcome"] == "completed"
                }),
                "tree preserves both Turn summaries",
            )?;
        }
        let text = String::from_utf8_lossy(&human.stdout);
        require(
            human.is_clean_exit(0)
                && text.contains(first.accepted_turn_id().unwrap())
                && text.contains(second.accepted_turn_id().unwrap())
                && text.contains("current=4"),
            "human tree exposes identity and current",
        )?;
        let listed: Value = serde_json::from_slice(&list.stdout).unwrap();
        require(
            list.is_clean_exit(0)
                && listed.as_array().unwrap().len() == 1
                && listed[0]["node_count"] == 5
                && listed[0]["unclosed_turn"].is_null(),
            "list reflects one closed Session",
        )?;
        let error: Value = serde_json::from_slice(&missing.stdout).unwrap();
        require(
            missing.is_clean_exit(2) && error["type"] == "error" && error["kind"] == "not_found",
            "missing Session produces only typed lookup error",
        )
    })();
    for (label, out) in [
        ("first", &first.command),
        ("before", &before),
        ("second", &second.command),
        ("after", &after),
        ("tree", &log),
        ("human tree", &human),
        ("list", &list),
        ("missing", &missing),
    ] {
        evidence.record_command(label, out);
    }
    finish_scenario(evidence, &[], server.captured_requests(), result);
}

#[test]
fn direct_turn_registered_model_with_missing_credentials_rejects_before_acceptance() {
    let driver = driver();
    let spec = ScenarioSpec::direct_local("direct-turn-sonnet-route");
    let world = ScenarioWorld::new(&spec.name).unwrap();
    world
        .write_agent_profile("dl-sonnet", "anthropic/claude-sonnet-4-6")
        .unwrap();
    world.init_git_project().unwrap();
    world.budget(&spec);
    world.start_daemon().expect("显式启动后台");
    let evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-sonnet");
    let resolved = send
        .run_command(&["--json", "models", "--provider", "anthropic"])
        .unwrap();
    let outcome = send.send("route sonnet").unwrap();
    let list = send.run_command(&["--json", "session", "list"]).unwrap();
    let result = (|| {
        assert_clean_send(&outcome)?;
        let routing: Value = serde_json::from_slice(&resolved.stdout).unwrap();
        require(
            resolved.is_clean_exit(0)
                && routing["models"].as_array().unwrap().iter().any(|model| {
                    model["provider"] == "anthropic"
                        && model["model"] == "claude-sonnet-4-6"
                        && model["availability"]
                            == serde_json::json!({"status":"unconfigured","requirement":"api_key"})
                }),
            "formal sonnet routing with missing credential",
        )?;
        require(
            outcome.command.is_clean_exit(2)
                && outcome.accepted_turn_id().is_none()
                && send.session_id().is_none(),
            "reject before Session acceptance",
        )?;
        // 身份先于提交输出；`configure(New)` 不检查凭据并创建只有根节点的草稿会话，
        // 随后 `submit` 在准入时因缺少凭据被拒绝，不接受任何 Turn。
        require(
            matches!(
                outcome.records.first(),
                Some(SendRecord::Submit(Submit::Sending { .. }))
            ) && outcome.configures().count() == 1
                && outcome.rejection().is_some(),
            format!(
                "identity first, then configured draft and rejected receipt: {:?}",
                outcome.type_sequence()
            ),
        )?;
        let sessions: Value = serde_json::from_slice(&list.stdout).unwrap();
        require(
            list.is_clean_exit(0)
                && sessions
                    .as_array()
                    .is_some_and(|items| items.len() == 1 && items[0]["node_count"] == 1),
            format!("only unaccepted draft Session at root after credential rejection: {sessions}"),
        )
    })();
    finish_scenario(
        evidence,
        &[
            ("models", &resolved),
            ("rejection", &outcome.command),
            ("list", &list),
        ],
        vec![],
        result,
    );
}

#[test]
fn direct_turn_availability_requirements_gate_submission_and_status() {
    use lattice_system_harness::{FAKE_API_KEY, FakeLlmServer, LatticeCommand};
    use serde_json::json;

    let driver = driver();
    let spec = ScenarioSpec::direct_local("direct-turn-availability");
    let world = ScenarioWorld::new(&spec.name).unwrap();
    let budget = world.budget(&spec);
    world.init_git_project().unwrap();

    let ollama_default = std::net::TcpListener::bind("127.0.0.1:11434")
        .expect("127.0.0.1:11434 must be bindable for undeclared ollama check");
    ollama_default
        .set_nonblocking(true)
        .expect("set_nonblocking on 127.0.0.1:11434 listener");

    let server = FakeLlmServer::start_plain_http1(sequence(vec![
        SseScript::plain("custom key reply"),
        SseScript::plain("declared ollama reply"),
    ]))
    .unwrap();
    let providers_path = world.home().join(".lattice/providers.toml");
    let initial_providers = format!(
        "[metadata]\nauto_refresh = false\n\n\
         [providers.custom-key]\n\
         protocol = 'chat-completions'\n\
         base_url = '{}'\n\
         auth = 'bearer'\n\n\
         [providers.custom-key.models.chat]\n",
        server.base_url()
    );
    std::fs::write(&providers_path, &initial_providers).unwrap();
    world
        .write_agent_profile("dl-avail", "custom-key/chat")
        .unwrap();
    world.start_daemon().expect("显式启动后台");

    let mut evidence = evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let auth_text = cli(&world, &spec, &["auth", "status"]);
        evidence.record_command("auth status text", &auth_text);
        assert!(auth_text.is_clean_exit(0), "{}", auth_text.summary());
        let auth_stdout = String::from_utf8(auth_text.stdout).unwrap();
        assert!(
            auth_stdout.lines().any(|line| {
                line.starts_with("custom-key unconfigured")
                    && line.contains("lattice auth set custom-key")
            }),
            "custom key provider row missing unconfigured diagnostic: {auth_stdout}"
        );
        assert!(
            auth_stdout.lines().any(|line| {
                line.starts_with("chatgpt unconfigured")
                    && line.contains("lattice auth login chatgpt")
            }),
            "chatgpt row missing unconfigured diagnostic: {auth_stdout}"
        );

        let auth_json = cli(&world, &spec, &["--json", "auth", "status"]);
        evidence.record_command("auth status json", &auth_json);
        assert!(auth_json.is_clean_exit(0), "{}", auth_json.summary());
        let status_doc: Value = serde_json::from_slice(&auth_json.stdout).unwrap();
        let rows = status_doc["providers"].as_array().unwrap();
        assert_eq!(
            rows.iter()
                .find(|row| row["provider"] == "custom-key")
                .unwrap()["availability"],
            json!({"status": "unconfigured", "requirement": "api_key"})
        );
        assert_eq!(
            rows.iter()
                .find(|row| row["provider"] == "chatgpt")
                .unwrap()["availability"],
            json!({"status": "unconfigured", "requirement": "chatgpt_login"})
        );

        let models_json = cli(
            &world,
            &spec,
            &["--json", "models", "--provider", "custom-key"],
        );
        evidence.record_command("models custom-key json", &models_json);
        assert!(models_json.is_clean_exit(0), "{}", models_json.summary());
        let models_doc: Value = serde_json::from_slice(&models_json.stdout).unwrap();
        let candidate = models_doc["models"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["provider"] == "custom-key" && row["model"] == "chat")
            .expect("custom-key/chat candidate");
        assert_eq!(
            candidate["availability"],
            json!({"status": "unconfigured", "requirement": "api_key"})
        );
        let models_text = cli(&world, &spec, &["models", "--provider", "custom-key"]);
        evidence.record_command("models custom-key text", &models_text);
        assert!(models_text.is_clean_exit(0), "{}", models_text.summary());
        let models_stdout = String::from_utf8(models_text.stdout).unwrap();
        assert!(
            models_stdout.lines().any(|line| {
                line.starts_with("custom-key/chat\t") && line.ends_with("\tunconfigured")
            }),
            "models text did not mark custom-key/chat unconfigured: {models_stdout}"
        );

        let undeclared = cli(
            &world,
            &spec,
            &[
                "--json",
                "session",
                "send",
                "undeclared ollama",
                "--agent",
                "dl-avail",
                "--model",
                "ollama/llama3",
            ],
        );
        evidence.record_command("send undeclared ollama", &undeclared);
        assert!(undeclared.is_clean_exit(2), "{}", undeclared.summary());
        let undeclared_parsed = parse_send_stdout(&undeclared.stdout, undeclared.exit_code());
        let undeclared_msg = &undeclared_parsed.rejection().unwrap().message;
        assert!(
            undeclared_msg.contains(&providers_path.display().to_string()),
            "missing providers.toml path in {undeclared_msg}"
        );
        assert!(
            undeclared_msg.contains("[providers.ollama]"),
            "missing [providers.ollama] in {undeclared_msg}"
        );
        let mut accepted_on_default = 0usize;
        loop {
            match ollama_default.accept() {
                Ok(_) => accepted_on_default += 1,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => panic!("127.0.0.1:11434 listener error: {error}"),
            }
        }
        assert_eq!(
            accepted_on_default, 0,
            "undeclared ollama must not connect to default 127.0.0.1:11434 endpoint"
        );
        drop(ollama_default);

        let missing_key = cli(
            &world,
            &spec,
            &[
                "--json",
                "session",
                "send",
                "missing key",
                "--agent",
                "dl-avail",
            ],
        );
        evidence.record_command("send custom-key without key", &missing_key);
        assert!(missing_key.is_clean_exit(2), "{}", missing_key.summary());
        let missing_key_parsed = parse_send_stdout(&missing_key.stdout, missing_key.exit_code());
        assert!(
            missing_key_parsed
                .rejection()
                .unwrap()
                .message
                .contains("lattice auth set custom-key")
        );

        let auth_set = driver
            .run_with_stdin(
                &world,
                &LatticeCommand {
                    args: vec!["auth".into(), "set".into(), "custom-key".into()],
                    timeout: Some(budget.cap_timeout(spec.step_timeout())),
                },
                format!("{FAKE_API_KEY}\n").as_bytes(),
            )
            .unwrap();
        evidence.record_command("auth set custom-key", &auth_set);
        assert!(auth_set.is_clean_exit(0), "{}", auth_set.summary());

        let with_key = cli(
            &world,
            &spec,
            &[
                "--json", "session", "send", "with key", "--agent", "dl-avail",
            ],
        );
        evidence.record_command("send custom-key with key", &with_key);
        assert!(with_key.is_clean_exit(0), "{}", with_key.summary());
        let with_key_parsed = parse_send_stdout(&with_key.stdout, with_key.exit_code());
        assert_accepted_and_closed(&with_key_parsed).unwrap();
        assert!(with_key_parsed.has_completed());
        assert_eq!(server.captured_requests().len(), 1);
        assert_eq!(server.captured_requests()[0].model, "chat");

        std::fs::write(
            &providers_path,
            format!(
                "{initial_providers}\n[providers.ollama]\nbase_url = '{}'\n",
                server.base_url()
            ),
        )
        .unwrap();
        let declared_ollama = cli(
            &world,
            &spec,
            &[
                "--json",
                "session",
                "send",
                "declared ollama",
                "--agent",
                "dl-avail",
                "--model",
                "ollama/llama3",
            ],
        );
        evidence.record_command("send declared ollama", &declared_ollama);
        assert!(
            declared_ollama.is_clean_exit(0),
            "{}",
            declared_ollama.summary()
        );
        let declared_parsed =
            parse_send_stdout(&declared_ollama.stdout, declared_ollama.exit_code());
        assert_accepted_and_closed(&declared_parsed).unwrap();
        assert!(declared_parsed.has_completed());
        let captures = server.captured_requests();
        assert_eq!(captures.len(), 2);
        assert_eq!(captures[1].model, "llama3");
    }));
    evidence.record_captures(server.captured_requests());
    finish_recorded(evidence, result);
}

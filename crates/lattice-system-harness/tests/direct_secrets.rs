// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Direct-Local 秘密保护场景：凭据不进入模型回复或公开查询输出，配置拒绝原因先脱敏再返回，
//! 失败产物与报告对秘密字面量完整脱敏并保留原始主断言。

mod support;

use std::fs;

use lattice_system_harness::{
    AssertError, ScenarioEvidence, SessionSendDriver, SseScript, failure_dir,
    finalize_with_failure_artifacts,
};
use serde_json::Value;
use support::{
    assert_clean_send, cli, direct_world, driver, finish_recorded, finish_scenario, require,
    wire_fake_llm,
};

// Preserve all decoded JSONL fields, including records unknown to the typed
// parser, so JSON escapes cannot hide a sentinel from the evidence finalizer.
fn record_jsonl(evidence: &mut ScenarioEvidence, name: &str, bytes: &[u8]) {
    let events: Vec<Value> = serde_json::Deserializer::from_slice(bytes)
        .into_iter::<Value>()
        .collect::<Result<_, _>>()
        .expect("public send JSONL");
    assert!(!events.is_empty(), "send JSONL must contain events");
    assert!(
        events.iter().all(Value::is_object),
        "send event roots must be objects"
    );
    evidence.record_json(name, &Value::Array(events)).unwrap();
}

/// 验证录入的 provider 凭据发往模型服务但绝不进入回复或公开查询输出，且配置拒绝诊断对其脱敏。
#[test]
fn direct_secrets_sentinels_never_leak_into_public_sinks() {
    let driver = driver();
    let (world, spec) = direct_world("direct-secrets", "dl-secret", "");
    let unique = world.root().file_name().unwrap().to_str().unwrap();
    // 不带密钥前缀、认证语法、URL 或 40 字符随机串：只有按已录入凭据值脱敏才能隐藏它。
    let credential = format!("fixture.{unique}");
    assert!(credential.len() < 40);

    let server = wire_fake_llm(&world, "dl-secret", SseScript::plain("hello"));
    world
        .register_key(
            lattice_system_harness::ProviderFixture::chat_completions("dl-secret"),
            Some(&credential),
        )
        .unwrap();
    server.expect_api_key(&credential);
    let mut evidence = ScenarioEvidence::new(&world, &spec.name, [credential.clone()]).unwrap();
    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-secret");
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let turn = send.send("say hello").expect("turn");
        evidence.record_command("secrets send", &turn.command);
        record_jsonl(&mut evidence, "send-events", &turn.command.stdout);
        assert_clean_send(&turn).unwrap();
        assert!(turn.command.is_clean_exit(0));
        assert!(turn.has_completed(), "纯文本轮次应正常完成");
        assert!(
            !String::from_utf8_lossy(&turn.command.stdout).contains(&credential),
            "认证凭据不得进入公开回复流"
        );
        let sid = send.session_id().expect("accepted Session");
        let tid = turn.accepted_turn_id().expect("accepted Turn");
        let captures = server.captured_requests();
        assert_eq!(captures.len(), 1, "正常轮次应向 provider 发送一次请求");

        let mut query = |name: &str, args: &[&str]| -> Value {
            let output = cli(&world, &spec, args);
            evidence.record_command(args.join(" "), &output);
            assert!(
                output.is_clean_exit(0),
                "{}: {}",
                args.join(" "),
                output.summary()
            );
            let value: Value = serde_json::from_slice(&output.stdout).expect("public query JSON");
            if name == "session-list" {
                assert!(value.is_array(), "session list root array");
            } else {
                assert!(value.is_object(), "public query root object");
            }
            evidence.record_json(name, &value).unwrap();
            value
        };
        let sessions = query("session-list", &["--json", "session", "list"]);
        let sessions = sessions.as_array().expect("session list root array");
        assert_eq!(sessions.len(), 1);
        let log = query("session-log", &["--json", "session", "log", sid]);
        assert!(log.is_object(), "session log root object");
        assert!(!log["nodes"].as_array().expect("session nodes").is_empty());
        let detail = query("session-turn", &["--json", "session", "turn", tid]);
        assert!(detail.is_object(), "session turn root object");
        assert!(!detail["steps"].as_array().expect("turn steps").is_empty());
        let text = cli(&world, &spec, &["session", "turn", tid]);
        evidence.record_command("session turn text", &text);
        assert!(text.is_clean_exit(0));

        // 配置拒绝原因必须经过脱敏，且不得产生新的 provider 请求。
        let profile = world
            .project_dir()
            .join(".lattice/agents/dl-secret/agent.toml");
        let declared = fs::read_to_string(&profile)
            .expect("read profile")
            .replace("name = \"dl-secret\"", &format!("name = '{credential}'"));
        fs::write(&profile, declared).expect("write profile");
        let rejected = ["--json", "session", "send", "again", "--agent", "dl-secret"];
        let rj = cli(&world, &spec, &rejected);
        evidence.record_command("rejected send JSON", &rj);
        assert!(rj.is_clean_exit(2), "configuration rejection exits 2");
        record_jsonl(&mut evidence, "rejected-send-events", &rj.stdout);
        let rt = cli(&world, &spec, &rejected[1..]);
        evidence.record_command("rejected send text", &rt);
        for (output, reason) in [(&rj, &rj.stdout), (&rt, &rt.stderr)] {
            assert!(output.is_clean_exit(2), "configuration rejection exits 2");
            assert!(
                String::from_utf8_lossy(reason)
                    .contains("invalid value `[REDACTED]` for `name` in [agent]"),
                "rejection reason must show the declared name redacted"
            );
        }
        let rejection = lattice_system_harness::parse_send_stdout(&rj.stdout, rj.exit_code());
        assert!(
            rejection.malformed_lines.is_empty(),
            "rejection JSONL must parse"
        );
        assert_eq!(support::rejected_kind(&rejection), Some("bad_request"));
        assert_eq!(
            server.captured_requests().len(),
            captures.len(),
            "rejected configuration must not reach the provider"
        );
        // Raw commands and captures are scanned unconditionally by finish_report,
        // including when one of the business assertions above panics.
    }));
    evidence.record_captures(server.captured_requests());
    finish_recorded(evidence, result);
}

/// Prove redaction actually transforms a secret-bearing model reply, retains
/// the original business failure, and reports the raw-output leak separately.
#[test]
fn direct_secrets_failure_bundle_redacts_sentinels() {
    let driver = driver();
    let (world, spec) = direct_world("direct-secrets-bundle", "dl-sec-b", "");
    let unique = world.root().file_name().unwrap().to_str().unwrap();
    let credential = format!("bundle-auth.{unique}");
    let fake_key = format!("sk-reply-fixture-{unique}");
    let protected = format!("protected-content-{unique}");
    let injected = format!("injected-secret-{unique}");
    let reply = format!("credential={fake_key} protected={protected} injected={injected}");
    let reply_secrets = [
        ("credential", fake_key.as_str()),
        ("protected", protected.as_str()),
        ("injected", injected.as_str()),
    ];
    let server = wire_fake_llm(&world, "dl-sec-b", SseScript::plain(&reply));
    world
        .register_key(
            lattice_system_harness::ProviderFixture::chat_completions("dl-sec-b"),
            Some(&credential),
        )
        .unwrap();
    server.expect_api_key(&credential);
    let scenario_evidence =
        ScenarioEvidence::new(&world, &spec.name, [credential.clone()]).unwrap();
    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-sec-b");
    let turn = send.send("reply with the fixture text").expect("turn");
    let captures = server.captured_requests();
    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&turn)?;
        require(
            turn.command.is_clean_exit(0) && turn.has_completed(),
            "model reply completes",
        )?;
        require(captures.len() == 1, "model reply must reach provider")?;
        let raw_stdout = String::from_utf8_lossy(&turn.command.stdout);
        let mut expected_redactions = 0;
        for (label, secret) in reply_secrets {
            require(
                raw_stdout.contains(&format!("{label}={secret}")),
                format!("raw stdout must contain injected {label} value"),
            )?;
            expected_redactions += raw_stdout.matches(secret).count();
        }
        require(
            !raw_stdout.contains(&credential),
            "authentication credential must stay out of reply",
        )?;
        require(
            !raw_stdout.contains("[REDACTED]"),
            "raw reply must contain unredacted inputs",
        )?;
        // The intentional leak belongs to this finalizer, which returns its
        // report for inspection rather than failing the surrounding scenario.
        let bundle_name = format!("{}-fast", spec.name);
        let bundle_dir = failure_dir(&bundle_name).expect("valid scenario name");
        let primary = "intentional failure to trigger artifacts";
        let report = finalize_with_failure_artifacts(
            &world,
            &bundle_name,
            "lattice --json session send",
            &turn.command,
            &[&credential, &fake_key, &protected, &injected],
            Err(AssertError::StdoutMissing {
                expected: primary.to_string(),
            }),
        )
        .expect_err("intentional business assertion and raw leak must be reported");
        let stdout = fs::read_to_string(bundle_dir.join("stdout.txt")).expect("read bundle stdout");
        for (label, secret) in reply_secrets {
            require(
                !stdout.contains(secret),
                format!("bundle stdout must redact {label} value"),
            )?;
            require(
                stdout.matches(&format!("{label}=[REDACTED]")).count()
                    == raw_stdout.matches(secret).count(),
                format!("bundle stdout must replace every {label} occurrence"),
            )?;
        }
        require(
            stdout.matches("[REDACTED]").count() == expected_redactions,
            "bundle stdout must replace every occurrence of all three inputs",
        )?;
        let text = report.to_string();
        require(
            text.contains(primary),
            "report must retain original assertion text",
        )?;
        require(
            text.contains("- command stdout"),
            "report must locate original stdout leak",
        )?;
        for (label, secret) in reply_secrets {
            require(
                !text.contains(secret),
                format!("report must redact {label} value"),
            )?;
        }
        let leaks = lattice_system_harness::scan_artifacts_for_secrets(
            &bundle_dir,
            &[&credential, &fake_key, &protected, &injected],
        )
        .expect("scan artifacts");
        require(leaks.is_empty(), format!("bundle leaks: {leaks:?}"))?;
        Ok(())
    })();
    finish_scenario(
        scenario_evidence,
        &[("secrets bundle send", &turn.command)],
        captures,
        result,
    );
}

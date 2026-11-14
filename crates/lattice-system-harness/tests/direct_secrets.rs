// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 本地直连秘密保护场景：凭据不进入模型回复或公开查询输出，配置拒绝原因先脱敏再返回，
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

// 保留解码后的全部 JSONL 字段（包括强类型解析器未识别的记录），防止 JSON 转义向证据收尾扫描隐藏哨兵值。
fn record_jsonl(evidence: &mut ScenarioEvidence, name: &str, bytes: &[u8]) {
    let events: Vec<Value> = serde_json::Deserializer::from_slice(bytes)
        .into_iter::<Value>()
        .collect::<Result<_, _>>()
        .expect("解析公开发送流 JSONL");
    assert!(!events.is_empty(), "发送流 JSONL 必须包含事件记录");
    assert!(
        events.iter().all(Value::is_object),
        "发送流每条事件顶层都必须是 JSON 对象"
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
    world.budget(&spec);
    world.start_daemon().expect("显式启动后台");

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
        let turn = send.send("say hello").expect("执行轮次发送");
        evidence.record_command("携带凭据发送轮次", &turn.command);
        record_jsonl(&mut evidence, "send-events", &turn.command.stdout);
        assert_clean_send(&turn).unwrap();
        assert!(turn.command.is_clean_exit(0));
        assert!(turn.has_completed(), "纯文本轮次应正常完成");
        assert!(
            !String::from_utf8_lossy(&turn.command.stdout).contains(&credential),
            "认证凭据不得进入公开回复流"
        );
        let sid = send.session_id().expect("应已接受会话");
        let tid = turn.accepted_turn_id().expect("应已接受轮次");
        let captures = server.captured_requests();
        assert_eq!(captures.len(), 1, "正常轮次应向 provider 发送一次请求");

        let mut query = |name: &str, args: &[&str]| -> Value {
            let output = cli(&world, &spec, args);
            evidence.record_command(format!("公开查询（{}）", args.join(" ")), &output);
            assert!(
                output.is_clean_exit(0),
                "{}: {}",
                args.join(" "),
                output.summary()
            );
            let value: Value = serde_json::from_slice(&output.stdout).expect("解析公开查询 JSON");
            if name == "session-list" {
                assert!(value.is_array(), "会话列表顶层应为 JSON 数组");
            } else {
                assert!(value.is_object(), "公开查询顶层应为 JSON 对象");
            }
            evidence.record_json(name, &value).unwrap();
            value
        };
        let sessions = query("session-list", &["--json", "session", "list"]);
        let sessions = sessions.as_array().expect("会话列表顶层应为 JSON 数组");
        assert_eq!(sessions.len(), 1);
        let log = query("session-log", &["--json", "session", "log", sid]);
        assert!(log.is_object(), "会话日志顶层应为 JSON 对象");
        assert!(!log["nodes"].as_array().expect("会话节点数组").is_empty());
        let detail = query("session-turn", &["--json", "session", "turn", tid]);
        assert!(detail.is_object(), "轮次详情顶层应为 JSON 对象");
        assert!(!detail["steps"].as_array().expect("轮次步骤数组").is_empty());
        let text = cli(&world, &spec, &["session", "turn", tid]);
        evidence.record_command("查询文本格式轮次详情", &text);
        assert!(text.is_clean_exit(0));

        // 配置拒绝原因必须经过脱敏，且不得产生新的 provider 请求。
        let profile = world
            .project_dir()
            .join(".lattice/agents/dl-secret/agent.toml");
        let declared = fs::read_to_string(&profile)
            .expect("读取智能体配置文件")
            .replace("name = \"dl-secret\"", &format!("name = '{credential}'"));
        fs::write(&profile, declared).expect("写入智能体配置文件");
        let rejected = ["--json", "session", "send", "again", "--agent", "dl-secret"];
        let rj = cli(&world, &spec, &rejected);
        evidence.record_command("配置拒绝发送（JSON）", &rj);
        assert!(rj.is_clean_exit(2), "配置拒绝退出码应为 2");
        record_jsonl(&mut evidence, "rejected-send-events", &rj.stdout);
        let rt = cli(&world, &spec, &rejected[1..]);
        evidence.record_command("配置拒绝发送（文本）", &rt);
        for (output, reason) in [(&rj, &rj.stdout), (&rt, &rt.stderr)] {
            assert!(output.is_clean_exit(2), "配置拒绝退出码应为 2");
            assert!(
                String::from_utf8_lossy(reason)
                    .contains("invalid value `[REDACTED]` for `name` in [agent]"),
                "拒绝原因中的声明名称必须已被脱敏"
            );
        }
        let rejection = lattice_system_harness::parse_send_stdout(&rj.stdout, rj.exit_code());
        assert!(
            rejection.malformed_lines.is_empty(),
            "配置拒绝输出的 JSONL 必须可正常解析"
        );
        assert_eq!(support::rejected_kind(&rejection), Some("bad_request"));
        assert_eq!(
            server.captured_requests().len(),
            captures.len(),
            "被拒绝的配置不得向 provider 发起请求"
        );
        // 即使上面的业务断言发生 panic，`finish_report` 也会无条件扫描原始命令输出与捕获请求。
    }));
    evidence.record_captures(server.captured_requests());
    finish_recorded(evidence, result);
}

/// 验证脱敏逻辑会真实改写含秘密的模型回复、保留原始业务失败断言，并单独报告原始输出中的泄露位置。
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
    world.budget(&spec);
    world.start_daemon().expect("显式启动后台");
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
    let turn = send
        .send("reply with the fixture text")
        .expect("执行轮次发送");
    let captures = server.captured_requests();
    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&turn)?;
        require(
            turn.command.is_clean_exit(0) && turn.has_completed(),
            "模型回复轮次应正常完成",
        )?;
        require(captures.len() == 1, "模型回复轮次必须到达 provider")?;
        let raw_stdout = String::from_utf8_lossy(&turn.command.stdout);
        let mut expected_redactions = 0;
        for (label, secret) in reply_secrets {
            require(
                raw_stdout.contains(&format!("{label}={secret}")),
                format!("原始标准输出应包含注入的 {label} 哨兵值"),
            )?;
            expected_redactions += raw_stdout.matches(secret).count();
        }
        require(
            !raw_stdout.contains(&credential),
            "认证凭据不得出现在模型回复流中",
        )?;
        require(
            !raw_stdout.contains("[REDACTED]"),
            "原始回复流应保留尚未脱敏的注入文本",
        )?;
        // 这里的故意泄露仅交给内层收尾函数生成失败报告以供检查，不应导致外层场景失败。
        let bundle_name = format!("{}-fast", spec.name);
        let bundle_dir = failure_dir(&bundle_name).expect("场景名称应合法");
        let primary = "故意触发失败以生成证据产物包";
        let report = finalize_with_failure_artifacts(
            &world,
            &bundle_name,
            "执行会话发送（lattice --json session send）",
            &turn.command,
            &[&credential, &fake_key, &protected, &injected],
            Err(AssertError::StdoutMissing {
                expected: primary.to_string(),
            }),
        )
        .expect_err("应同时报告故意构造的业务断言与原始输出泄露");
        let stdout =
            fs::read_to_string(bundle_dir.join("stdout.txt")).expect("读取产物包标准输出副本");
        for (label, secret) in reply_secrets {
            require(
                !stdout.contains(secret),
                format!("产物包标准输出副本必须已脱敏 {label} 的值"),
            )?;
            require(
                stdout.matches(&format!("{label}=[REDACTED]")).count()
                    == raw_stdout.matches(secret).count(),
                format!("产物包标准输出副本必须替换每一处 {label} 出现位置"),
            )?;
        }
        require(
            stdout.matches("[REDACTED]").count() == expected_redactions,
            "产物包标准输出副本必须替换全部三类哨兵的每一处出现位置",
        )?;
        let text = report.to_string();
        require(text.contains(primary), "失败报告必须保留原始主断言文本")?;
        require(
            text.contains("- 命令 stdout"),
            "失败报告必须指明原始命令标准输出中的泄露位置",
        )?;
        for (label, secret) in reply_secrets {
            require(
                !text.contains(secret),
                format!("失败报告自身必须已脱敏 {label} 的值"),
            )?;
        }
        let leaks = lattice_system_harness::scan_artifacts_for_secrets(
            &bundle_dir,
            &[&credential, &fake_key, &protected, &injected],
        )
        .expect("扫描失败产物目录");
        require(
            leaks.is_empty(),
            format!("产物包仍残留未脱敏秘密：{leaks:?}"),
        )?;
        Ok(())
    })();
    finish_scenario(
        scenario_evidence,
        &[("秘密产物包验证发送", &turn.command)],
        captures,
        result,
    );
}

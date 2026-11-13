// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 凭据命令的真实二进制场景，秘密仅经标准输入录入。

use std::{fs, os::unix::fs::PermissionsExt};

use lattice_system_harness::{
    CommandOutcome, DaemonOptions, LatticeCommand, LatticeDriver, ScenarioBudget, ScenarioEvidence,
    ScenarioSpec, ScenarioWorld, assert_exit_code, assert_no_secret,
};

const KEY: &str = "auth-cli-sentinel-secret-7Y29";

fn run(
    world: &ScenarioWorld,
    driver: &LatticeDriver,
    budget: &ScenarioBudget,
    evidence: &mut ScenarioEvidence,
    args: &[&str],
    input: Option<&[u8]>,
) -> Result<CommandOutcome, String> {
    budget.check("fast-auth-cli");
    let command = LatticeCommand {
        args: args.iter().map(|arg| (*arg).into()).collect(),
        timeout: Some(budget.cap_timeout(std::time::Duration::from_secs(30))),
    };
    let output = match input {
        Some(input) => driver.run_with_stdin(world, &command, input),
        None => driver.run(world, &command),
    }
    .map_err(|error| error.to_string())?;
    evidence.record_command(
        format!(
            "执行凭据命令（{}）",
            args.join(" ").replace(KEY, "[REDACTED]")
        ),
        &output,
    );
    assert_no_secret(&output, KEY).map_err(|error| error.to_string())?;
    Ok(output)
}

fn state(output: &CommandOutcome, provider: &str) -> Result<String, String> {
    let status: serde_json::Value =
        serde_json::from_slice(&output.stdout).map_err(|_| "auth status 输出必须是合法 JSON")?;
    status["providers"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["provider"] == provider))
        .and_then(|row| row["availability"]["status"].as_str())
        .map(str::to_owned)
        .ok_or_else(|| "缺少对应 provider 的凭据状态".into())
}

#[test]
fn fast_auth_stdin_status_permissions_and_rejections() {
    let spec = ScenarioSpec::direct_local("fast-auth-cli");
    let world = ScenarioWorld::new(&spec.name).expect("创建场景世界");
    let budget = world.budget(&spec);
    let driver = LatticeDriver::from_env().expect("LATTICE_BIN 必须指向已构建的二进制");
    let mut evidence =
        ScenarioEvidence::new(&world, &spec.name, [KEY.into()]).expect("创建场景证据");
    world
        .start_daemon_with(&DaemonOptions::new().env("LATTICE_LOG", "trace"))
        .expect("显式启动带日志追踪的后台");
    let daemon_pgid = world.daemon_pgid().expect("获取自有后台进程组");
    let result = (|| -> Result<(), String> {
        if world.daemon_pids().map_err(|error| error.to_string())? != vec![daemon_pgid] {
            return Err("凭据命令必须复用显式启动的自有后台".into());
        }
        let initial = run(
            &world,
            &driver,
            &budget,
            &mut evidence,
            &["--json", "auth", "status"],
            None,
        )?;
        assert_exit_code(&initial, 0).map_err(|error| error.to_string())?;
        if state(&initial, "openai")? != "unconfigured" {
            return Err("初始密钥状态必须为 unconfigured".into());
        }
        let input = format!("{KEY}\n");
        let set = run(
            &world,
            &driver,
            &budget,
            &mut evidence,
            &["--json", "auth", "set", "openai"],
            Some(input.as_bytes()),
        )?;
        assert_exit_code(&set, 0).map_err(|error| error.to_string())?;
        if serde_json::from_slice::<serde_json::Value>(&set.stdout)
            .map_err(|_| "auth set 必须返回合法 JSON")?
            != serde_json::json!({"type":"auth","action":"set","provider":"openai"})
        {
            return Err("auth set 输出必须标明类型、动作与 provider".into());
        }
        let text_set = run(
            &world,
            &driver,
            &budget,
            &mut evidence,
            &["auth", "set", "openai"],
            Some(input.as_bytes()),
        )?;
        assert_exit_code(&text_set, 0).map_err(|error| error.to_string())?;
        if text_set.stdout != b"openai set\n" {
            return Err("文本模式的 auth set 输出必须标明 provider 与动作".into());
        }
        for input in [
            b"\n".as_slice(),
            b"bad\nkey\n".as_slice(),
            vec![b'k'; 16_385].as_slice(),
        ] {
            let rejected = run(
                &world,
                &driver,
                &budget,
                &mut evidence,
                &["--json", "auth", "set", "openai"],
                Some(input),
            )?;
            assert_exit_code(&rejected, 2).map_err(|error| error.to_string())?;
            let error: serde_json::Value = serde_json::from_slice(&rejected.stdout)
                .map_err(|_| "密钥拒绝响应必须是合法 JSON")?;
            if error["kind"] != "bad_request"
                || error["message"]
                    != "API key must be 1–16384 bytes and contain no control characters"
            {
                return Err("密钥校验规则必须由后台直接执行且不带文件错误前缀".into());
            }
        }
        let path = world.home().join(".lattice/auth.json");
        if fs::metadata(path.parent().unwrap())
            .map_err(|error| error.to_string())?
            .permissions()
            .mode()
            & 0o777
            != 0o700
        {
            return Err("新建凭据目录权限必须为 0700".into());
        }
        let metadata = fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
        if !metadata.is_file() || metadata.permissions().mode() & 0o777 != 0o600 {
            return Err("凭据文件必须是权限为 0600 的普通文件".into());
        }
        let mut document: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).map_err(|error| error.to_string())?)
                .map_err(|_| "保存的凭据文件必须是合法 JSON")?;
        document["providers"]["chatgpt"] = serde_json::json!({
            "type":"chatgpt","host_id":"test-host","client_id":"test-client",
            "account":{"subject":"test-subject","email":"user@example.com"},
            "tokens":{"access_token":KEY,"refresh_token":KEY,"id_token":KEY,
                "expires_at":"2026-01-01T00:00:00+00:00","scopes":[]}
        });
        fs::write(&path, document.to_string()).map_err(|error| error.to_string())?;
        let status = run(
            &world,
            &driver,
            &budget,
            &mut evidence,
            &["auth", "status", "--json"],
            None,
        )?;
        assert_exit_code(&status, 0).map_err(|error| error.to_string())?;
        if state(&status, "openai")? != "ready" {
            return Err("录入密钥后的状态必须为 ready".into());
        }
        let text = run(
            &world,
            &driver,
            &budget,
            &mut evidence,
            &["auth", "status"],
            None,
        )?;
        assert_exit_code(&text, 0).map_err(|error| error.to_string())?;
        if !std::str::from_utf8(&text.stdout)
            .unwrap_or_default()
            .lines()
            .any(|line| line == "openai ready")
        {
            return Err("文本模式状态必须报告 openai ready".into());
        }
        if !std::str::from_utf8(&text.stdout)
            .unwrap_or_default()
            .lines()
            .any(|line| {
                line == "chatgpt ready email=user@example.com expires_at=2026-01-01T00:00:00+00:00"
            })
        {
            return Err("凭据状态元数据必须使用 key=value 字段格式".into());
        }
        for provider in ["unknown", "ollama", "chatgpt"] {
            let rejected = run(
                &world,
                &driver,
                &budget,
                &mut evidence,
                &["--json", "auth", "set", provider],
                Some(input.as_bytes()),
            )?;
            assert_exit_code(&rejected, 2).map_err(|error| error.to_string())?;
        }
        for args in [
            vec!["auth", "set", "openai", KEY],
            vec!["--json", "auth", "set", "openai", KEY],
            vec!["help", "auth", "set", KEY],
            vec!["--json", "help", "auth", "set", KEY],
        ] {
            let rejected = run(&world, &driver, &budget, &mut evidence, &args, None)?;
            assert_exit_code(&rejected, 2).map_err(|error| error.to_string())?;
        }
        let remove = run(
            &world,
            &driver,
            &budget,
            &mut evidence,
            &["auth", "remove", "openai"],
            None,
        )?;
        assert_exit_code(&remove, 0).map_err(|error| error.to_string())?;
        if remove.stdout != b"openai removed\n" {
            return Err("文本模式删除输出必须标明被删除的 provider".into());
        }
        let missing = run(
            &world,
            &driver,
            &budget,
            &mut evidence,
            &["--json", "auth", "remove", "openai"],
            None,
        )?;
        assert_exit_code(&missing, 0).map_err(|error| error.to_string())?;
        if serde_json::from_slice::<serde_json::Value>(&missing.stdout)
            .map_err(|_| "auth remove 必须返回合法 JSON")?
            != serde_json::json!({"type":"auth","action":"remove","provider":"openai","removed":false})
        {
            return Err("auth remove 在密钥不存在时必须报告 removed=false".into());
        }
        let missing = run(
            &world,
            &driver,
            &budget,
            &mut evidence,
            &["auth", "remove", "openai"],
            None,
        )?;
        assert_exit_code(&missing, 0).map_err(|error| error.to_string())?;
        if missing.stdout != b"openai missing\n" {
            return Err("文本模式删除输出必须区分密钥原本不存在的情况".into());
        }
        let status = run(
            &world,
            &driver,
            &budget,
            &mut evidence,
            &["--json", "auth", "status"],
            None,
        )?;
        if state(&status, "openai")? != "unconfigured" {
            return Err("删除后的密钥状态必须恢复为 unconfigured".into());
        }
        fs::write(&path, format!(r#"{{"version":"{KEY}"}}"#)).map_err(|error| error.to_string())?;
        let status = run(
            &world,
            &driver,
            &budget,
            &mut evidence,
            &["--json", "auth", "status"],
            None,
        )?;
        assert_exit_code(&status, 0).map_err(|error| error.to_string())?;
        if state(&status, "openai")? != "invalid" {
            return Err("损坏的凭据文件必须报告 invalid 状态".into());
        }
        Ok(())
    })();
    let mut failures: Vec<String> = result.err().into_iter().collect();
    if let Err(errors) = world.stop_daemon() {
        failures.extend(errors);
    }
    match world.take_daemon_outcome() {
        Some(output) => {
            evidence.record_command("开启 LATTICE_LOG=trace 的后台", &output);
            for result in [assert_exit_code(&output, 0), assert_no_secret(&output, KEY)] {
                if let Err(error) = result {
                    failures.push(error.to_string());
                }
            }
        }
        None => failures.push("停止后台后未取回后台退出结果".into()),
    }
    evidence
        .finish_report((!failures.is_empty()).then(|| failures.join("\n")))
        .unwrap_or_else(|report| panic!("{report}"));
}

#[test]
fn fast_invalid_home_keeps_daemon_available_and_preserves_configuration_reason() {
    for (name, home, reason) in [
        ("missing", None, "HOME is not set"),
        (
            "relative",
            Some("relative-home"),
            "HOME must be an absolute path",
        ),
    ] {
        let spec = ScenarioSpec::direct_local(format!("fast-auth-home-{name}"));
        let world = ScenarioWorld::new(&spec.name).expect("创建场景世界");
        if let Some(home) = home {
            world.set_env("HOME", home);
        } else {
            world.unset_env("HOME");
        }
        let budget = world.budget(&spec);
        world
            .start_daemon()
            .expect("以无效 HOME 环境快照显式启动后台");
        let driver = LatticeDriver::from_env().expect("LATTICE_BIN 必须指向已构建的二进制");
        let mut evidence =
            ScenarioEvidence::new(&world, &spec.name, [KEY.into()]).expect("创建场景证据");
        let result = (|| -> Result<(), String> {
            for (args, input) in [
                (vec!["--json", "auth", "status"], None),
                (
                    vec!["--json", "auth", "set", "openai"],
                    Some(KEY.as_bytes()),
                ),
                (vec!["--json", "auth", "remove", "openai"], None),
            ] {
                let rejected = run(&world, &driver, &budget, &mut evidence, &args, input)?;
                assert_exit_code(&rejected, 2).map_err(|error| error.to_string())?;
                let error: serde_json::Value = serde_json::from_slice(&rejected.stdout)
                    .map_err(|_| "配置拒绝响应必须是合法 JSON")?;
                if error["kind"] != "bad_request" || error["message"] != reason {
                    return Err("配置拒绝响应必须保留 HOME 无效的具体原因".into());
                }
            }
            let status = run(
                &world,
                &driver,
                &budget,
                &mut evidence,
                &["--json", "daemon", "status"],
                None,
            )?;
            assert_exit_code(&status, 0).map_err(|error| error.to_string())?;
            world.set_env("HOME", world.home().display().to_string());
            let rejected = run(
                &world,
                &driver,
                &budget,
                &mut evidence,
                &["--json", "auth", "status"],
                None,
            )?;
            assert_exit_code(&rejected, 2).map_err(|error| error.to_string())?;
            let error: serde_json::Value = serde_json::from_slice(&rejected.stdout)
                .map_err(|_| "配置拒绝响应必须是合法 JSON")?;
            if error["message"] != reason {
                return Err("运行中的后台必须保持启动时的 HOME 快照".into());
            }
            Ok(())
        })();
        evidence
            .finish_report(result.err())
            .unwrap_or_else(|report| panic!("{report}"));
    }
}

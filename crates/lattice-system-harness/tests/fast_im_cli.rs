// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 内置 IM 网关组件托管、`inotify` 配置热重载、`im/` 目录恢复与 `lattice im` CLI 的真实二进制闭环场景。

use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
    time::{Duration, Instant},
};

use lattice_system_harness::{
    CommandOutcome, DaemonOptions, LatticeCommand, LatticeDriver, ScenarioBudget, ScenarioEvidence,
    ScenarioSpec, ScenarioWorld, assert_exit_code, assert_no_secret,
};

const IM_SECRET_TOKEN: &str = "123456789:AAH_HARNESS_SENTINEL_TELEGRAM_TOKEN_9Z";
const MISPLACED_PLATFORM_SECRET: &str = "998877665:AAH_MISPLACED_PLATFORM_SECRET_4X";
const EXTRA_ARG_SECRET: &str = "554433221:AAH_EXTRA_ARG_SECRET_2W";

fn all_secrets() -> [&'static str; 3] {
    [IM_SECRET_TOKEN, MISPLACED_PLATFORM_SECRET, EXTRA_ARG_SECRET]
}

fn redact_label(raw: &str) -> String {
    let mut sanitized = raw.to_owned();
    for secret in all_secrets() {
        sanitized = sanitized.replace(secret, "[REDACTED]");
    }
    sanitized
}

fn run_cli(
    world: &ScenarioWorld,
    driver: &LatticeDriver,
    budget: &ScenarioBudget,
    evidence: &mut ScenarioEvidence,
    args: &[&str],
    stdin: Option<&[u8]>,
) -> Result<CommandOutcome, String> {
    budget.check("fast-im-cli");
    let command = LatticeCommand {
        args: args.iter().map(|arg| (*arg).into()).collect(),
        timeout: Some(budget.cap_timeout(Duration::from_secs(30))),
    };
    let output = match stdin {
        Some(bytes) => driver.run_with_stdin(world, &command, bytes),
        None => driver.run(world, &command),
    }
    .map_err(|error| error.to_string())?;
    evidence.record_command(
        format!("执行 IM/守护进程命令（{}）", redact_label(&args.join(" "))),
        &output,
    );
    for secret in all_secrets() {
        assert_no_secret(&output, secret).map_err(|error| error.to_string())?;
    }
    Ok(output)
}

fn write_private_im_config(im_dir: &Path, content: &str) -> Result<(), String> {
    let tmp_path = im_dir.join(".config.toml.harness-tmp");
    let dst_path = im_dir.join("config.toml");
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp_path)
        .map_err(|e| format!("创建临时配置文件失败：{e}"))?;
    file.write_all(content.as_bytes())
        .map_err(|e| format!("写入临时配置文件失败：{e}"))?;
    file.sync_all()
        .map_err(|e| format!("同步临时配置文件失败：{e}"))?;
    drop(file);
    fs::rename(&tmp_path, &dst_path).map_err(|e| format!("原子替换 im/config.toml 失败：{e}"))
}

fn valid_telegram_config(workspace: &Path) -> String {
    let escaped = workspace.display().to_string().replace('\\', "\\\\");
    format!(
        "[telegram]\nallowed_senders = [42]\nworkspace = \"{escaped}\"\nagent = \"assistant\"\n"
    )
}

fn poll_components_until<F>(
    world: &ScenarioWorld,
    driver: &LatticeDriver,
    budget: &ScenarioBudget,
    evidence: &mut ScenarioEvidence,
    description: &str,
    predicate: F,
) -> Result<serde_json::Value, String>
where
    F: Fn(&serde_json::Value) -> bool,
{
    let deadline = Instant::now() + budget.cap_timeout(Duration::from_secs(5));
    loop {
        let out = run_cli(
            world,
            driver,
            budget,
            evidence,
            &["--json", "daemon", "status"],
            None,
        )?;
        assert_exit_code(&out, 0).map_err(|e| e.to_string())?;
        let parsed: serde_json::Value = serde_json::from_slice(&out.stdout)
            .map_err(|e| format!("daemon status JSON 解析失败：{e}"))?;
        if predicate(&parsed["components"]) {
            return Ok(parsed);
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "等待 `{description}` 超时，最后一次 daemon status 为：{parsed}"
            ));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn fast_im_component_lifecycle_inotify_reload_auth_and_secret_redaction() {
    let spec = ScenarioSpec::direct_local("fast-im-cli");
    let world = ScenarioWorld::new(&spec.name).expect("创建场景世界");
    let budget = world.budget(&spec);
    let driver = LatticeDriver::from_env().expect("LATTICE_BIN 必须指向已构建的二进制");
    let mut evidence = ScenarioEvidence::new(
        &world,
        &spec.name,
        all_secrets().into_iter().map(String::from),
    )
    .expect("创建场景证据");

    // 令守护进程在启动期不加载任何根证书：`TelegramHttpClient` 将在本地立即返回可重试无根证书诊断，
    // 完全不发起外部 DNS 或 HTTPS 请求，同时已挂载的 `PreparedImGateway` 保持正常存活与响应停止信号。
    world.set_env("SSL_CERT_FILE", "/nonexistent-cert");
    world.set_env("SSL_CERT_DIR", "/nonexistent-dir");
    world
        .start_daemon_with(
            &DaemonOptions::new()
                .env("LATTICE_LOG", "trace")
                .env("SSL_CERT_FILE", "/nonexistent-cert")
                .env("SSL_CERT_DIR", "/nonexistent-dir"),
        )
        .expect("显式启动隔离 TLS 信任根的后台");

    let daemon_pgid = world.daemon_pgid().expect("获取自有后台进程组");
    let im_dir = world.home().join(".lattice/im");
    let auth_path = im_dir.join("auth.json");
    let valid_cfg = valid_telegram_config(world.project_dir());

    let result = (|| -> Result<(), String> {
        if world.daemon_pids().map_err(|e| e.to_string())? != vec![daemon_pgid] {
            return Err("IM 命令必须复用显式启动的自有后台".into());
        }

        // 1. 默认未启用：后台启动后异步初始挂载自动创建 0700 的 ~/.lattice/im，`components` 为 `[]`，文本输出 `组件：无`。
        let im_dir_deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !im_dir.exists() {
            if std::time::Instant::now() >= im_dir_deadline {
                return Err("后台启动后未在 5 秒内创建 ~/.lattice/im".into());
            }
            std::thread::sleep(Duration::from_millis(15));
        }
        let im_dir_meta =
            fs::symlink_metadata(&im_dir).map_err(|e| format!("读取 ~/.lattice/im 失败：{e}"))?;
        if !im_dir_meta.is_dir() || im_dir_meta.permissions().mode() & 0o777 != 0o700 {
            return Err(format!(
                "启动期准备的 ~/.lattice/im 必须是权限 0700 的目录，实际 mode={:#o}",
                im_dir_meta.permissions().mode() & 0o777
            ));
        }

        let initial_json = run_cli(
            &world,
            &driver,
            &budget,
            &mut evidence,
            &["--json", "daemon", "status"],
            None,
        )?;
        assert_exit_code(&initial_json, 0).map_err(|e| e.to_string())?;
        let initial_parsed: serde_json::Value =
            serde_json::from_slice(&initial_json.stdout).map_err(|e| e.to_string())?;
        let initial_epoch = initial_parsed["epoch"]
            .as_str()
            .ok_or("daemon status 缺少 epoch")?
            .to_owned();
        if initial_parsed["components"] != serde_json::json!([]) {
            return Err(format!(
                "默认未启用 IM 时 components 必须为空数组，实际：{initial_parsed}"
            ));
        }

        let initial_text = run_cli(
            &world,
            &driver,
            &budget,
            &mut evidence,
            &["daemon", "status"],
            None,
        )?;
        assert_exit_code(&initial_text, 0).map_err(|e| e.to_string())?;
        let initial_stdout = String::from_utf8_lossy(&initial_text.stdout);
        if !initial_stdout.contains("组件：无") {
            return Err(format!(
                "默认未启用时人类可读 daemon status 应输出 `组件：无`，实际：{initial_stdout}"
            ));
        }

        let disabled_reload = run_cli(
            &world,
            &driver,
            &budget,
            &mut evidence,
            &["im", "reload"],
            None,
        )?;
        assert_exit_code(&disabled_reload, 0).map_err(|e| e.to_string())?;
        if disabled_reload.stdout != "IM：未启用\n".as_bytes() {
            return Err(format!(
                "未启用时 `lattice im reload` 应输出 `IM：未启用`，实际：{:?}",
                String::from_utf8_lossy(&disabled_reload.stdout)
            ));
        }
        let disabled_reload_json = run_cli(
            &world,
            &driver,
            &budget,
            &mut evidence,
            &["--json", "im", "reload"],
            None,
        )?;
        assert_exit_code(&disabled_reload_json, 0).map_err(|e| e.to_string())?;
        if serde_json::from_slice::<serde_json::Value>(&disabled_reload_json.stdout)
            .map_err(|e| e.to_string())?
            != serde_json::json!({"status": "disabled"})
        {
            return Err("未启用时 `--json im reload` 应返回 status=disabled".into());
        }

        // 2. 写入 `im/config.toml`：`inotify` 自动重挂载并变为缺凭据未挂载。
        write_private_im_config(&im_dir, &valid_cfg)?;
        poll_components_until(
            &world,
            &driver,
            &budget,
            &mut evidence,
            "写入 im/config.toml 后 inotify 自动切换为缺凭据未挂载",
            |components| {
                *components
                    == serde_json::json!([{
                        "name": "im",
                        "state": {
                            "status": "unmounted",
                            "reason": "缺少 Telegram 平台凭据（im/auth.json）"
                        }
                    }])
            },
        )?;

        let unmounted_daemon_text = run_cli(
            &world,
            &driver,
            &budget,
            &mut evidence,
            &["daemon", "status"],
            None,
        )?;
        assert_exit_code(&unmounted_daemon_text, 0).map_err(|e| e.to_string())?;
        let unmounted_stdout = String::from_utf8_lossy(&unmounted_daemon_text.stdout);
        if !unmounted_stdout
            .contains("组件：\n  im：未挂载；缺少 Telegram 平台凭据（im/auth.json）\n")
        {
            return Err(format!(
                "缺凭据时 daemon status 文本输出不符：{unmounted_stdout}"
            ));
        }

        let unmounted_reload = run_cli(
            &world,
            &driver,
            &budget,
            &mut evidence,
            &["im", "reload"],
            None,
        )?;
        assert_exit_code(&unmounted_reload, 1).map_err(|e| e.to_string())?;
        if unmounted_reload.stdout
            != "IM：未挂载；缺少 Telegram 平台凭据（im/auth.json）\n".as_bytes()
        {
            return Err(format!(
                "Unmounted 时 `im reload` 应退出 1 并输出原因，实际：{:?}",
                String::from_utf8_lossy(&unmounted_reload.stdout)
            ));
        }

        // 3. `im auth status` -> `im auth set telegram`：0600 落盘并立即挂载 IM 组件。
        let auth_missing = run_cli(
            &world,
            &driver,
            &budget,
            &mut evidence,
            &["im", "auth", "status"],
            None,
        )?;
        assert_exit_code(&auth_missing, 0).map_err(|e| e.to_string())?;
        if auth_missing.stdout != "telegram：未配置\n".as_bytes() {
            return Err(format!(
                "初始 `im auth status` 应为未配置，实际：{:?}",
                String::from_utf8_lossy(&auth_missing.stdout)
            ));
        }

        let stdin_token = format!("{IM_SECRET_TOKEN}\n");
        let auth_set = run_cli(
            &world,
            &driver,
            &budget,
            &mut evidence,
            &["im", "auth", "set", "telegram"],
            Some(stdin_token.as_bytes()),
        )?;
        assert_exit_code(&auth_set, 0).map_err(|e| e.to_string())?;
        if auth_set.stdout != "telegram 凭据已保存\nIM：已挂载\n".as_bytes() {
            return Err(format!(
                "`im auth set telegram` 应保存凭据并挂载 IM，实际：{:?}",
                String::from_utf8_lossy(&auth_set.stdout)
            ));
        }

        let auth_meta =
            fs::symlink_metadata(&auth_path).map_err(|e| format!("读取 im/auth.json 失败：{e}"))?;
        if !auth_meta.is_file() || auth_meta.permissions().mode() & 0o777 != 0o600 {
            return Err(format!(
                "im/auth.json 必须是权限 0600 的普通文件，实际 mode={:#o}",
                auth_meta.permissions().mode() & 0o777
            ));
        }

        let auth_present_json = run_cli(
            &world,
            &driver,
            &budget,
            &mut evidence,
            &["--json", "im", "auth", "status"],
            None,
        )?;
        assert_exit_code(&auth_present_json, 0).map_err(|e| e.to_string())?;
        if serde_json::from_slice::<serde_json::Value>(&auth_present_json.stdout)
            .map_err(|e| e.to_string())?
            != serde_json::json!({
                "platforms": [{"platform": "telegram", "credential": {"status": "present"}}]
            })
        {
            return Err("录入凭据后 `--json im auth status` 应报告 status=present".into());
        }

        let mounted_daemon_text = run_cli(
            &world,
            &driver,
            &budget,
            &mut evidence,
            &["daemon", "status"],
            None,
        )?;
        assert_exit_code(&mounted_daemon_text, 0).map_err(|e| e.to_string())?;
        if !String::from_utf8_lossy(&mounted_daemon_text.stdout).contains("组件：\n  im：已挂载\n")
        {
            return Err("挂载成功后 daemon status 应输出 `im：已挂载`".into());
        }

        // 4. 写坏 `im/config.toml`：触发 `仍按上一份配置运行` 且 `lattice im reload` 退出 1。
        let broken_cfg = "[telegram]\nallowed_senders = [42]\nworkspace = \"relative/path\"\nagent = \"assistant\"\n";
        write_private_im_config(&im_dir, broken_cfg)?;
        let expected_cfg_reason = "im/config.toml：[telegram].workspace 必须是绝对路径";
        poll_components_until(
            &world,
            &driver,
            &budget,
            &mut evidence,
            "写坏 im/config.toml 后保留上一份配置运行",
            |components| {
                *components
                    == serde_json::json!([{
                        "name": "im",
                        "state": {
                            "status": "mounted",
                            "configuration_error": expected_cfg_reason
                        }
                    }])
            },
        )?;

        let prev_cfg_status = run_cli(
            &world,
            &driver,
            &budget,
            &mut evidence,
            &["daemon", "status"],
            None,
        )?;
        assert_exit_code(&prev_cfg_status, 0).map_err(|e| e.to_string())?;
        if !String::from_utf8_lossy(&prev_cfg_status.stdout).contains(&format!(
            "  im：已挂载；仍按上一份配置运行；{expected_cfg_reason}\n"
        )) {
            return Err("配置写坏后 daemon status 文本应提示仍按上一份配置运行".into());
        }

        let broken_reload_text = run_cli(
            &world,
            &driver,
            &budget,
            &mut evidence,
            &["im", "reload"],
            None,
        )?;
        assert_exit_code(&broken_reload_text, 1).map_err(|e| e.to_string())?;
        if broken_reload_text.stdout
            != format!("IM：已挂载；仍按上一份配置运行；{expected_cfg_reason}\n").as_bytes()
        {
            return Err("UsingPreviousConfiguration 时 `im reload` 文本输出不符".into());
        }

        let broken_reload_json = run_cli(
            &world,
            &driver,
            &budget,
            &mut evidence,
            &["--json", "im", "reload"],
            None,
        )?;
        assert_exit_code(&broken_reload_json, 1).map_err(|e| e.to_string())?;
        if serde_json::from_slice::<serde_json::Value>(&broken_reload_json.stdout)
            .map_err(|e| e.to_string())?
            != serde_json::json!({
                "status": "using_previous_configuration",
                "reason": expected_cfg_reason
            })
        {
            return Err("UsingPreviousConfiguration 时 `--json im reload` 输出不符".into());
        }

        // 5. 修复 `im/config.toml`：恢复 `Mounted` 且 `im reload` 退出 0，整个过程后台 `epoch` 不变。
        write_private_im_config(&im_dir, &valid_cfg)?;
        let recovered_reload = run_cli(
            &world,
            &driver,
            &budget,
            &mut evidence,
            &["im", "reload"],
            None,
        )?;
        assert_exit_code(&recovered_reload, 0).map_err(|e| e.to_string())?;
        if recovered_reload.stdout != "IM：已挂载\n".as_bytes() {
            return Err("修复配置后 `im reload` 应输出 `IM：已挂载`".into());
        }

        let after_recover_json = run_cli(
            &world,
            &driver,
            &budget,
            &mut evidence,
            &["--json", "daemon", "status"],
            None,
        )?;
        assert_exit_code(&after_recover_json, 0).map_err(|e| e.to_string())?;
        let after_recover: serde_json::Value =
            serde_json::from_slice(&after_recover_json.stdout).map_err(|e| e.to_string())?;
        if after_recover["epoch"].as_str() != Some(initial_epoch.as_str()) {
            return Err("IM 热重载期间后台 epoch 不得变化".into());
        }
        if after_recover["components"]
            != serde_json::json!([{"name": "im", "state": {"status": "mounted"}}])
        {
            return Err(format!(
                "修复配置后 components 应清除 configuration_error，实际：{after_recover}"
            ));
        }

        // 6. `rm -rf ~/.lattice/im`：后台不擅自重建目录；执行 `im reload` 重建 0700 目录并恢复 `inotify` 监视。
        fs::remove_dir_all(&im_dir).map_err(|e| format!("删除 ~/.lattice/im 失败：{e}"))?;
        std::thread::sleep(Duration::from_millis(80));
        if im_dir.exists() {
            return Err("删除 ~/.lattice/im 后在显式 reload 前后台不得自动重建该目录".into());
        }

        let recreate_reload = run_cli(
            &world,
            &driver,
            &budget,
            &mut evidence,
            &["im", "reload"],
            None,
        )?;
        assert_exit_code(&recreate_reload, 0).map_err(|e| e.to_string())?;
        if recreate_reload.stdout != "IM：未启用\n".as_bytes() {
            return Err("删除 im/ 后 `im reload` 应返回未启用并退出 0".into());
        }

        let recreated_meta = fs::symlink_metadata(&im_dir)
            .map_err(|e| format!("读取重建后的 ~/.lattice/im 失败：{e}"))?;
        if !recreated_meta.is_dir() || recreated_meta.permissions().mode() & 0o777 != 0o700 {
            return Err("`im reload` 重建的 ~/.lattice/im 必须是权限 0700 的目录".into());
        }

        // 再次保存 `im/config.toml`，验证恢复后的 `inotify` 无需手动 reload 即可自动触发重新挂载。
        write_private_im_config(&im_dir, &valid_cfg)?;
        poll_components_until(
            &world,
            &driver,
            &budget,
            &mut evidence,
            "重建 ~/.lattice/im 恢复监视后自动触发重新挂载",
            |components| {
                *components
                    == serde_json::json!([{
                        "name": "im",
                        "state": {
                            "status": "unmounted",
                            "reason": "缺少 Telegram 平台凭据（im/auth.json）"
                        }
                    }])
            },
        )?;

        // 7. 再次录入凭据后验证 `im auth remove`（已删除 vs 未配置）与各类参数/秘密零回显拒绝。
        let set_again = run_cli(
            &world,
            &driver,
            &budget,
            &mut evidence,
            &["--json", "im", "auth", "set", "telegram"],
            Some(stdin_token.as_bytes()),
        )?;
        assert_exit_code(&set_again, 0).map_err(|e| e.to_string())?;
        if serde_json::from_slice::<serde_json::Value>(&set_again.stdout)
            .map_err(|e| e.to_string())?
            != serde_json::json!({"mount": {"status": "mounted"}})
        {
            return Err("`--json im auth set telegram` 应返回 mount.status=mounted".into());
        }

        let removed_once = run_cli(
            &world,
            &driver,
            &budget,
            &mut evidence,
            &["im", "auth", "remove", "telegram"],
            None,
        )?;
        assert_exit_code(&removed_once, 0).map_err(|e| e.to_string())?;
        if removed_once.stdout
            != "telegram 凭据已删除\nIM：未挂载；缺少 Telegram 平台凭据（im/auth.json）\n"
                .as_bytes()
        {
            return Err(format!(
                "首次 `im auth remove telegram` 输出不符：{:?}",
                String::from_utf8_lossy(&removed_once.stdout)
            ));
        }

        let removed_twice = run_cli(
            &world,
            &driver,
            &budget,
            &mut evidence,
            &["im", "auth", "remove", "telegram"],
            None,
        )?;
        assert_exit_code(&removed_twice, 0).map_err(|e| e.to_string())?;
        if removed_twice.stdout
            != "telegram 凭据未配置\nIM：未挂载；缺少 Telegram 平台凭据（im/auth.json）\n"
                .as_bytes()
        {
            return Err(format!(
                "重复 `im auth remove telegram` 应提示凭据未配置，实际：{:?}",
                String::from_utf8_lossy(&removed_twice.stdout)
            ));
        }

        let removed_json = run_cli(
            &world,
            &driver,
            &budget,
            &mut evidence,
            &["--json", "im", "auth", "remove", "telegram"],
            None,
        )?;
        assert_exit_code(&removed_json, 0).map_err(|e| e.to_string())?;
        if serde_json::from_slice::<serde_json::Value>(&removed_json.stdout)
            .map_err(|e| e.to_string())?["removed"]
            != serde_json::json!(false)
        {
            return Err("凭据不存在时 `--json im auth remove` 应返回 removed=false".into());
        }

        // 非法平台参数、多传位置参数与非法凭据内容全部退出 2 且零回显秘密。
        for args in [
            vec!["im", "auth", "set", MISPLACED_PLATFORM_SECRET],
            vec!["--json", "im", "auth", "set", MISPLACED_PLATFORM_SECRET],
            vec!["im", "auth", "remove", MISPLACED_PLATFORM_SECRET],
            vec!["--json", "im", "auth", "remove", MISPLACED_PLATFORM_SECRET],
            vec!["im", "auth", "set", "telegram", EXTRA_ARG_SECRET],
            vec!["--json", "im", "auth", "set", "telegram", EXTRA_ARG_SECRET],
            vec!["help", "im", "auth", "set", EXTRA_ARG_SECRET],
            vec!["--json", "help", "im", "auth", "set", EXTRA_ARG_SECRET],
            vec!["im", "help", "auth", EXTRA_ARG_SECRET],
        ] {
            let rejected = run_cli(&world, &driver, &budget, &mut evidence, &args, None)?;
            assert_exit_code(&rejected, 2).map_err(|e| e.to_string())?;
        }

        for bad_token in [format!("{IM_SECRET_TOKEN}\x07\n"), "\n".to_owned()] {
            for args in [
                vec!["im", "auth", "set", "telegram"],
                vec!["--json", "im", "auth", "set", "telegram"],
            ] {
                let rejected = run_cli(
                    &world,
                    &driver,
                    &budget,
                    &mut evidence,
                    &args,
                    Some(bad_token.as_bytes()),
                )?;
                assert_exit_code(&rejected, 2).map_err(|e| e.to_string())?;
            }
        }

        let stopped = run_cli(
            &world,
            &driver,
            &budget,
            &mut evidence,
            &["daemon", "stop"],
            None,
        )?;
        assert_exit_code(&stopped, 0).map_err(|e| e.to_string())?;
        world
            .daemon_departed()
            .map_err(|errors| format!("后台退场检查失败：{errors:?}"))?;
        Ok(())
    })();

    evidence
        .finish_report(result.err())
        .unwrap_or_else(|report| panic!("{report}"));
}

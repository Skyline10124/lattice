// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `lattice daemon install` 与 `lattice daemon` 人类可读输出场景验收测试。
//!
//! 所有用例均使用临时目录下的假 `systemctl` 脚本与隔离的 `XDG_CONFIG_HOME` / `HOME`，
//! 并将场景 `PATH` 仅指向假 `systemctl` 目录，绝不触碰宿主真实的 `systemctl` 或 `~/.config/systemd/user`。

use std::{
    fs,
    io::{self, Read, Write},
    os::unix::{
        fs::{MetadataExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use lattice_system_harness::{
    CommandOutcome, LatticeCommand, LatticeDaemonDriver, LatticeDriver, RunningCommand,
    ScenarioEvidence, ScenarioSpec, ScenarioWorld, assert_exit_code, assert_stderr_contains,
    assert_stdout_contains_all,
};

const FAKE_SYSTEMCTL_SCRIPT: &str = r#"#!/bin/sh
first=1
line=""
for arg in "$@"; do
    if [ "$first" -eq 1 ]; then
        line="$arg"
        first=0
    else
        line="${line}|${arg}"
    fi
done
printf '%s\n' "$line" >> "$FAKE_SYSTEMCTL_LOG"
case "$line" in
    "--user|daemon-reload")
        if [ "${FAKE_SYSTEMCTL_FAIL_RELOAD:-0}" = "1" ]; then
            printf '%s\n' "模拟 daemon-reload 失败" >&2
            exit 1
        fi
        exit 0
        ;;
    "--user|enable|--now|lattice")
        if [ "${FAKE_SYSTEMCTL_FAIL_ENABLE:-0}" = "1" ]; then
            printf '%s\n' "模拟 enable --now 失败" >&2
            exit 1
        fi
        exit 0
        ;;
    *)
        printf '未知 systemctl 参数：%s\n' "$line" >&2
        exit 99
        ;;
esac
"#;

struct InstallRig {
    lattice_bin: PathBuf,
    daemon_bin: PathBuf,
    systemctl_log: PathBuf,
    xdg_config_home: PathBuf,
}

impl InstallRig {
    fn setup(
        world: &ScenarioWorld,
        case_dir_name: &str,
        copy_daemon: bool,
    ) -> Result<Self, String> {
        let src_lattice = LatticeDriver::from_env()
            .map_err(|e| format!("读取 LATTICE_BIN 失败：{e}"))?
            .binary_path()
            .to_path_buf();
        let src_daemon = LatticeDaemonDriver::from_env()
            .map_err(|e| format!("读取 LATTICE_DAEMON_BIN 失败：{e}"))?
            .binary_path()
            .to_path_buf();

        let case_root = world.root().join(case_dir_name);
        let bin_dir = case_root.join("install-bin");
        let fake_sys_dir = case_root.join("fake-systemctl");
        let xdg_config_home = case_root.join("xdg-config");
        let systemctl_log = case_root.join("systemctl.log");

        fs::create_dir_all(&bin_dir).map_err(|e| format!("创建临时二进制目录失败：{e}"))?;
        fs::create_dir_all(&fake_sys_dir).map_err(|e| format!("创建假 systemctl 目录失败：{e}"))?;

        let lattice_bin = bin_dir.join("lattice");
        fs::copy(&src_lattice, &lattice_bin).map_err(|e| format!("复制 lattice 失败：{e}"))?;
        fs::set_permissions(&lattice_bin, fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("设置 lattice 可执行权限失败：{e}"))?;

        let daemon_bin = bin_dir.join("lattice-daemon");
        if copy_daemon {
            fs::copy(&src_daemon, &daemon_bin)
                .map_err(|e| format!("复制 lattice-daemon 失败：{e}"))?;
            fs::set_permissions(&daemon_bin, fs::Permissions::from_mode(0o755))
                .map_err(|e| format!("设置 lattice-daemon 可执行权限失败：{e}"))?;
        }

        let fake_systemctl = fake_sys_dir.join("systemctl");
        fs::write(&fake_systemctl, FAKE_SYSTEMCTL_SCRIPT)
            .map_err(|e| format!("写入假 systemctl 失败：{e}"))?;
        fs::set_permissions(&fake_systemctl, fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("设置假 systemctl 权限失败：{e}"))?;

        // 将 PATH 仅设为假 systemctl 目录，彻底阻断落入宿主真实 systemctl 的可能。
        world.set_env("PATH", fake_sys_dir.display().to_string());
        world.set_env("XDG_CONFIG_HOME", xdg_config_home.display().to_string());
        world.set_env("FAKE_SYSTEMCTL_LOG", systemctl_log.display().to_string());
        world.unset_env("FAKE_SYSTEMCTL_FAIL_RELOAD");
        world.unset_env("FAKE_SYSTEMCTL_FAIL_ENABLE");

        Ok(Self {
            lattice_bin,
            daemon_bin,
            systemctl_log,
            xdg_config_home,
        })
    }

    fn unit_path(&self) -> PathBuf {
        self.xdg_config_home
            .join("systemd")
            .join("user")
            .join("lattice.service")
    }

    fn clear_systemctl_log(&self) {
        let _ = fs::remove_file(&self.systemctl_log);
    }

    fn systemctl_calls(&self) -> Vec<String> {
        match fs::read_to_string(&self.systemctl_log) {
            Ok(raw) => raw.lines().map(str::to_owned).collect(),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(err) => panic!("读取假 systemctl 日志失败：{err}"),
        }
    }

    fn run_lattice(
        &self,
        world: &ScenarioWorld,
        args: &[&str],
        timeout: Duration,
    ) -> Result<CommandOutcome, String> {
        run_binary(world, &self.lattice_bin, args, timeout)
    }
}

fn run_binary(
    world: &ScenarioWorld,
    binary: &Path,
    args: &[&str],
    timeout: Duration,
) -> Result<CommandOutcome, String> {
    let mut cmd = world.command(binary);
    cmd.args(args);
    RunningCommand::spawn(cmd)
        .map_err(|e| format!("启动子进程 {} 失败：{e}", binary.display()))?
        .wait(Some(timeout))
        .map_err(|e| format!("等待子进程 {} 失败：{e}", binary.display()))
}

fn expected_unit_text(daemon_exec: &str, state_root_exec: &str, drain: &str, stop: &str) -> String {
    format!(
        "[Unit]\n\
StartLimitIntervalSec=120\n\
StartLimitBurst=5\n\
\n\
[Service]\n\
Type=notify\n\
ExecStart={daemon_exec} --state-root {state_root_exec} --drain-timeout {drain}\n\
TimeoutStopSec={stop}\n\
NotifyAccess=main\n\
Restart=on-failure\n\
RestartSec=5\n\
RestartPreventExitStatus=2 3 4 SIGKILL\n\
\n\
[Install]\n\
WantedBy=default.target\n"
    )
}

/// 旧协议模拟对端句柄：`accept`、读写与线程退出共用同一个 `deadline`，并在提前析构时唤醒退出。
struct IncompatiblePeerFixture {
    cli_done: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<Result<(), String>>>,
}

impl IncompatiblePeerFixture {
    fn spawn(listener: UnixListener, timeout: Duration) -> Result<Self, String> {
        listener
            .set_nonblocking(true)
            .map_err(|e| format!("设置模拟 socket 非阻塞失败：{e}"))?;
        let deadline = Instant::now() + timeout;
        let cli_done = Arc::new(AtomicBool::new(false));
        let done_in_thread = Arc::clone(&cli_done);

        let handle = std::thread::spawn(move || -> Result<(), String> {
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                        if done_in_thread.load(Ordering::Acquire) {
                            match listener.accept() {
                                Ok((stream, _)) => break stream,
                                Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                                    return Err(
                                        "CLI 已结束，但未连接状态根 api.sock 探测旧协议后台"
                                            .to_owned(),
                                    );
                                }
                                Err(err) => {
                                    return Err(format!("旧协议模拟 socket accept 失败：{err}"));
                                }
                            }
                        }
                        if Instant::now() >= deadline {
                            return Err(format!(
                                "等待 CLI 连接旧协议模拟 socket 超出期限（{timeout:?}）"
                            ));
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(err) => return Err(format!("旧协议模拟 socket accept 失败：{err}")),
                }
            };

            stream
                .set_nonblocking(false)
                .map_err(|e| format!("切换模拟连接为带超时阻塞模式失败：{e}"))?;
            apply_stream_deadline(&stream, deadline)?;

            let mut req_magic = [0_u8; 8];
            stream
                .read_exact(&mut req_magic)
                .map_err(|e| format!("期限内读取客户端探测帧魔数失败：{e}"))?;

            apply_stream_deadline(&stream, deadline)?;
            // 写入比当前协议版本（63 = 0x3f）旧一版的 LTLOCAL 帧魔数。
            stream
                .write_all(b"LTLOCAL\x3e")
                .map_err(|e| format!("期限内写入旧协议魔数失败：{e}"))?;
            stream
                .flush()
                .map_err(|e| format!("刷新旧协议魔数失败：{e}"))?;
            let _ = stream.shutdown(std::net::Shutdown::Write);

            let mut sink = [0_u8; 256];
            loop {
                apply_stream_deadline(&stream, deadline)?;
                match stream.read(&mut sink) {
                    Ok(0) => break,
                    Ok(_) => continue,
                    Err(err)
                        if matches!(
                            err.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                        ) =>
                    {
                        return Err(format!("等待客户端关闭旧协议探测连接超时：{err}"));
                    }
                    Err(_) => break,
                }
            }
            Ok(())
        });

        Ok(Self {
            cli_done,
            handle: Some(handle),
        })
    }

    fn finish_and_verify(mut self) -> Result<(), String> {
        self.cli_done.store(true, Ordering::Release);
        let handle = self.handle.take().expect("模拟线程句柄存在");
        handle
            .join()
            .map_err(|_| "旧协议模拟线程发生 panic".to_owned())?
    }
}

impl Drop for IncompatiblePeerFixture {
    fn drop(&mut self) {
        self.cli_done.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn apply_stream_deadline(stream: &UnixStream, deadline: Instant) -> Result<(), String> {
    let now = Instant::now();
    if now >= deadline {
        return Err("旧协议模拟连接读写超出截止时间".into());
    }
    let remaining = (deadline - now).max(Duration::from_millis(1));
    stream
        .set_read_timeout(Some(remaining))
        .map_err(|e| format!("设置模拟连接读超时失败：{e}"))?;
    stream
        .set_write_timeout(Some(remaining))
        .map_err(|e| format!("设置模拟连接写超时失败：{e}"))?;
    Ok(())
}

#[test]
fn fast_daemon_install_writes_unit_and_enables_service() {
    let spec = ScenarioSpec::direct_local("daemon-install-success");
    let world = ScenarioWorld::new(&spec.name).expect("创建场景世界失败");
    let budget = world.budget(&spec);
    let mut evidence = ScenarioEvidence::new(&world, &spec.name, Vec::<String>::new()).unwrap();

    let result = (|| -> Result<(), String> {
        let rig = InstallRig::setup(&world, "default-case", true)?;
        let state_root = world
            .session_store_dir()
            .parent()
            .expect("状态根目录")
            .to_path_buf();

        // 1. 默认排空上限（60s -> TimeoutStopSec=90s），且自动创建缺失的 systemd/user 目录。
        budget.check(&spec.name);
        let out = rig.run_lattice(
            &world,
            &["daemon", "install"],
            budget.cap_timeout(spec.step_timeout()),
        )?;
        evidence.record_command("执行默认 lattice daemon install", &out);
        assert_exit_code(&out, 0).map_err(|e| e.to_string())?;

        let unit_path = rig.unit_path();
        assert_stdout_contains_all(
            &out,
            &[
                &unit_path.display().to_string(),
                "已启用并启动服务 lattice.service",
            ],
        )
        .map_err(|e| e.to_string())?;
        if !out.stderr.is_empty() {
            return Err(format!(
                "成功安装时标准错误应为空，实际：{}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        if rig.systemctl_calls() != ["--user|daemon-reload", "--user|enable|--now|lattice"] {
            return Err(format!(
                "systemctl 调用顺序不符合预期：{:?}",
                rig.systemctl_calls()
            ));
        }

        let unit_body =
            fs::read_to_string(&unit_path).map_err(|e| format!("读取 unit 文件失败：{e}"))?;
        let expected = expected_unit_text(
            &rig.daemon_bin.display().to_string(),
            &state_root.display().to_string(),
            "60s",
            "90s",
        );
        if unit_body != expected {
            return Err(format!(
                "默认 unit 内容与 ADR 0011 第 3 节不一致：\n实际：\n{unit_body}\n预期：\n{expected}"
            ));
        }

        // 确认原子重命名没有遗留临时文件。
        for entry in fs::read_dir(unit_path.parent().unwrap()).map_err(|e| e.to_string())? {
            let name = entry.map_err(|e| e.to_string())?.file_name();
            if name.to_string_lossy().contains(".tmp.") {
                return Err(format!("systemd/user 目录下残留临时文件：{name:?}"));
            }
        }

        // 2. 非整秒 `--drain-timeout 300ms` -> `TimeoutStopSec=30300ms`。
        budget.check(&spec.name);
        rig.clear_systemctl_log();
        let ms_out = rig.run_lattice(
            &world,
            &["daemon", "install", "--drain-timeout", "300ms"],
            budget.cap_timeout(spec.step_timeout()),
        )?;
        evidence.record_command("执行 300ms 排空上限的 lattice daemon install", &ms_out);
        assert_exit_code(&ms_out, 0).map_err(|e| e.to_string())?;
        if rig.systemctl_calls() != ["--user|daemon-reload", "--user|enable|--now|lattice"] {
            return Err(format!(
                "300ms 安装的 systemctl 调用顺序不符合预期：{:?}",
                rig.systemctl_calls()
            ));
        }
        let ms_unit =
            fs::read_to_string(&unit_path).map_err(|e| format!("读取 300ms unit 失败：{e}"))?;
        let expected_ms = expected_unit_text(
            &rig.daemon_bin.display().to_string(),
            &state_root.display().to_string(),
            "300ms",
            "30300ms",
        );
        if ms_unit != expected_ms {
            return Err(format!(
                "300ms unit 内容不符合预期：\n实际：\n{ms_unit}\n预期：\n{expected_ms}"
            ));
        }

        // 3. 未设置 `XDG_CONFIG_HOME` 时回退到隔离 `$HOME/.config/systemd/user/lattice.service`。
        budget.check(&spec.name);
        rig.clear_systemctl_log();
        world.unset_env("XDG_CONFIG_HOME");
        let home_unit_path = world
            .home()
            .join(".config")
            .join("systemd")
            .join("user")
            .join("lattice.service");
        let home_out = rig.run_lattice(
            &world,
            &["daemon", "install"],
            budget.cap_timeout(spec.step_timeout()),
        )?;
        evidence.record_command("未设置 XDG_CONFIG_HOME 时执行 install", &home_out);
        assert_exit_code(&home_out, 0).map_err(|e| e.to_string())?;
        assert_stdout_contains_all(&home_out, &[&home_unit_path.display().to_string()])
            .map_err(|e| e.to_string())?;
        if !home_unit_path.is_file() {
            return Err(format!(
                "回退路径下未写出 unit 文件：{}",
                home_unit_path.display()
            ));
        }

        Ok(())
    })();
    evidence.finish_report(result.err()).unwrap();
}

#[test]
fn fast_daemon_install_rejects_bad_daemon_binary_or_overflow_without_side_effects() {
    let spec = ScenarioSpec::direct_local("daemon-install-prechecks");
    let world = ScenarioWorld::new(&spec.name).expect("创建场景世界失败");
    let budget = world.budget(&spec);
    let mut evidence = ScenarioEvidence::new(&world, &spec.name, Vec::<String>::new()).unwrap();

    let result = (|| -> Result<(), String> {
        // 1. 同目录缺失 `lattice-daemon`：退出 2，不写 unit，不调用 systemctl。
        budget.check(&spec.name);
        let missing_rig = InstallRig::setup(&world, "missing-daemon", false)?;
        let out_missing = missing_rig.run_lattice(
            &world,
            &["daemon", "install"],
            budget.cap_timeout(spec.step_timeout()),
        )?;
        evidence.record_command("缺失同目录 lattice-daemon 时执行 install", &out_missing);
        assert_exit_code(&out_missing, 2).map_err(|e| e.to_string())?;
        assert_stderr_contains(&out_missing, "lattice-daemon").map_err(|e| e.to_string())?;
        if missing_rig.unit_path().exists() {
            return Err("缺失 lattice-daemon 时不应写出 unit 文件".into());
        }
        if !missing_rig.systemctl_calls().is_empty() {
            return Err(format!(
                "缺失 lattice-daemon 时不应调用 systemctl：{:?}",
                missing_rig.systemctl_calls()
            ));
        }

        // 2. 同目录 `lattice-daemon` 是目录而非普通文件：退出 2，无副作用。
        budget.check(&spec.name);
        fs::create_dir_all(&missing_rig.daemon_bin).map_err(|e| e.to_string())?;
        let out_dir = missing_rig.run_lattice(
            &world,
            &["daemon", "install"],
            budget.cap_timeout(spec.step_timeout()),
        )?;
        evidence.record_command("同目录 lattice-daemon 为目录时执行 install", &out_dir);
        assert_exit_code(&out_dir, 2).map_err(|e| e.to_string())?;
        assert_stderr_contains(&out_dir, "不是普通文件").map_err(|e| e.to_string())?;
        if missing_rig.unit_path().exists() || !missing_rig.systemctl_calls().is_empty() {
            return Err("同目录 lattice-daemon 为目录时不应写 unit 或调用 systemctl".into());
        }

        // 3. 同目录 `lattice-daemon` 完全无执行位（0644）：退出 2，无副作用。
        budget.check(&spec.name);
        let nonexec_rig = InstallRig::setup(&world, "nonexec-daemon", true)?;
        fs::set_permissions(&nonexec_rig.daemon_bin, fs::Permissions::from_mode(0o644))
            .map_err(|e| e.to_string())?;
        let out_nonexec = nonexec_rig.run_lattice(
            &world,
            &["daemon", "install"],
            budget.cap_timeout(spec.step_timeout()),
        )?;
        evidence.record_command(
            "同目录 lattice-daemon 权限 0644 时执行 install",
            &out_nonexec,
        );
        assert_exit_code(&out_nonexec, 2).map_err(|e| e.to_string())?;
        assert_stderr_contains(&out_nonexec, "可执行权限").map_err(|e| e.to_string())?;
        if nonexec_rig.unit_path().exists() || !nonexec_rig.systemctl_calls().is_empty() {
            return Err("同目录 lattice-daemon 权限 0644 时不应写 unit 或调用 systemctl".into());
        }

        // 4. 权限 `0641`、属主为当前用户的 `lattice-daemon`（仅 other 具执行位，当前用户不可执行）：退出 2，无副作用。
        budget.check(&spec.name);
        let mode_0641_rig = InstallRig::setup(&world, "mode-0641-daemon", true)?;
        fs::set_permissions(&mode_0641_rig.daemon_bin, fs::Permissions::from_mode(0o641))
            .map_err(|e| e.to_string())?;
        let owner_uid = fs::metadata(&mode_0641_rig.daemon_bin)
            .map_err(|e| e.to_string())?
            .uid();
        if owner_uid == 0 {
            return Err(
                "测试不应以 root 身份运行，无法验证属主 0641 的有效身份可执行性检查".into(),
            );
        }
        let out_0641 = mode_0641_rig.run_lattice(
            &world,
            &["daemon", "install"],
            budget.cap_timeout(spec.step_timeout()),
        )?;
        evidence.record_command(
            "属主为当前用户且权限为 0641 的 lattice-daemon 执行 install",
            &out_0641,
        );
        assert_exit_code(&out_0641, 2).map_err(|e| e.to_string())?;
        assert_stderr_contains(&out_0641, "可执行权限").map_err(|e| e.to_string())?;
        if mode_0641_rig.unit_path().exists() {
            return Err("权限 0641 的 lattice-daemon 不应写出 unit 文件".into());
        }
        if !mode_0641_rig.systemctl_calls().is_empty() {
            return Err(format!(
                "权限 0641 的 lattice-daemon 不应调用 systemctl：{:?}",
                mode_0641_rig.systemctl_calls()
            ));
        }

        // 5. `--drain-timeout` 解析溢出或加 30 秒后溢出，以及 `install --json`：退出 2，不写 unit，不调用 systemctl。
        let overflow_rig = InstallRig::setup(&world, "overflow-case", true)?;
        for (label, bad_args, check_json) in [
            (
                "u64 解析溢出",
                vec![
                    "daemon",
                    "install",
                    "--drain-timeout",
                    "18446744073709551616s",
                ],
                false,
            ),
            (
                "秒数加 30 秒溢出",
                vec![
                    "daemon",
                    "install",
                    "--drain-timeout",
                    "18446744073709551600s",
                ],
                false,
            ),
            (
                "毫秒数加 30 秒溢出",
                vec![
                    "daemon",
                    "install",
                    "--drain-timeout",
                    "18446744073709551600ms",
                ],
                false,
            ),
            (
                "install --json 无副作用拒绝",
                vec!["daemon", "install", "--json"],
                true,
            ),
        ] {
            budget.check(&spec.name);
            let out = overflow_rig.run_lattice(
                &world,
                &bad_args,
                budget.cap_timeout(spec.step_timeout()),
            )?;
            evidence.record_command(format!("预检拒绝无副作用校验（{label}）"), &out);
            assert_exit_code(&out, 2).map_err(|e| format!("{label}: {e}"))?;
            if check_json {
                let parsed: serde_json::Value =
                    serde_json::from_slice(&out.stdout).map_err(|e| format!("{label}: {e}"))?;
                let msg = parsed["message"]
                    .as_str()
                    .ok_or_else(|| format!("{label}: 缺少 message：{parsed}"))?;
                if !msg.contains("不支持 `--json`") {
                    return Err(format!("{label}: 错误未说明不支持 --json：{msg}"));
                }
            } else {
                assert_stderr_contains(&out, "溢出").map_err(|e| format!("{label}: {e}"))?;
            }
            if overflow_rig.unit_path().exists() {
                return Err(format!("{label}: 拒绝时不应写出 unit 文件"));
            }
            if !overflow_rig.systemctl_calls().is_empty() {
                return Err(format!(
                    "{label}: 拒绝时不应调用 systemctl：{:?}",
                    overflow_rig.systemctl_calls()
                ));
            }
        }

        Ok(())
    })();
    evidence.finish_report(result.err()).unwrap();
}

#[test]
fn fast_daemon_install_handles_daemon_reload_and_enable_failures_and_chinese_status_output() {
    let spec = ScenarioSpec::direct_local("daemon-install-systemctl-failures");
    let world = ScenarioWorld::new(&spec.name).expect("创建场景世界失败");
    let budget = world.budget(&spec);
    let mut evidence = ScenarioEvidence::new(&world, &spec.name, Vec::<String>::new()).unwrap();

    let result = (|| -> Result<(), String> {
        let rig = InstallRig::setup(&world, "failure-case", true)?;
        let unit_str = rig.unit_path().display().to_string();

        // 1. `daemon-reload` 失败：退出 1，unit 已写出，不再执行 `enable --now`。
        budget.check(&spec.name);
        world.set_env("FAKE_SYSTEMCTL_FAIL_RELOAD", "1");
        let reload_fail = rig.run_lattice(
            &world,
            &["daemon", "install"],
            budget.cap_timeout(spec.step_timeout()),
        )?;
        evidence.record_command("daemon-reload 失败时执行 install", &reload_fail);
        assert_exit_code(&reload_fail, 1).map_err(|e| e.to_string())?;
        if !rig.unit_path().is_file() {
            return Err("daemon-reload 失败前应已写出 unit 文件".into());
        }
        assert_stderr_contains(&reload_fail, &unit_str).map_err(|e| e.to_string())?;
        assert_stderr_contains(&reload_fail, "daemon-reload").map_err(|e| e.to_string())?;
        if rig.systemctl_calls() != ["--user|daemon-reload"] {
            return Err(format!(
                "daemon-reload 失败后不应继续调用 enable：{:?}",
                rig.systemctl_calls()
            ));
        }

        // 2. `enable --now` 失败且无运行中后台：退出 1，提示 `systemctl --user status lattice`。
        budget.check(&spec.name);
        rig.clear_systemctl_log();
        world.unset_env("FAKE_SYSTEMCTL_FAIL_RELOAD");
        world.set_env("FAKE_SYSTEMCTL_FAIL_ENABLE", "1");
        let enable_no_daemon = rig.run_lattice(
            &world,
            &["daemon", "install"],
            budget.cap_timeout(spec.step_timeout()),
        )?;
        evidence.record_command("无后台时 enable --now 失败", &enable_no_daemon);
        assert_exit_code(&enable_no_daemon, 1).map_err(|e| e.to_string())?;
        assert_stderr_contains(&enable_no_daemon, &unit_str).map_err(|e| e.to_string())?;
        assert_stderr_contains(&enable_no_daemon, "systemctl --user status lattice")
            .map_err(|e| e.to_string())?;
        if rig.systemctl_calls() != ["--user|daemon-reload", "--user|enable|--now|lattice"] {
            return Err(format!(
                "enable 失败时应按顺序调用两条命令：{:?}",
                rig.systemctl_calls()
            ));
        }

        // 顺带核验无后台时人类可读的 `lattice daemon status` 输出中文错误且无英文 `error:` 前缀。
        budget.check(&spec.name);
        let status_not_running = rig.run_lattice(
            &world,
            &["daemon", "status"],
            budget.cap_timeout(spec.step_timeout()),
        )?;
        evidence.record_command("无后台时执行人类可读 daemon status", &status_not_running);
        assert_exit_code(&status_not_running, 1).map_err(|e| e.to_string())?;
        assert_stderr_contains(&status_not_running, "错误：").map_err(|e| e.to_string())?;
        if String::from_utf8_lossy(&status_not_running.stderr).contains("error:") {
            return Err(format!(
                "daemon status 错误输出不应含英文 `error:`：{}",
                String::from_utf8_lossy(&status_not_running.stderr)
            ));
        }

        // 3. `enable --now` 失败且状态根上有运行中后台：报出对端 pid，提示先 `lattice daemon stop` 再 `systemctl --user start lattice`。
        budget.check(&spec.name);
        world
            .start_daemon()
            .map_err(|e| format!("显式启动场景后台失败：{e}"))?;
        let daemon_pid = world.daemon_pgid().ok_or("未取得场景后台进程号")?;

        rig.clear_systemctl_log();
        let enable_with_daemon = rig.run_lattice(
            &world,
            &["daemon", "install"],
            budget.cap_timeout(spec.step_timeout()),
        )?;
        evidence.record_command("有后台运行时 enable --now 失败", &enable_with_daemon);
        assert_exit_code(&enable_with_daemon, 1).map_err(|e| e.to_string())?;
        let expected_pid_fragment = format!("对端进程号：{daemon_pid}");
        assert_stderr_contains(&enable_with_daemon, &expected_pid_fragment)
            .map_err(|e| e.to_string())?;
        assert_stderr_contains(&enable_with_daemon, "lattice daemon stop")
            .map_err(|e| e.to_string())?;
        assert_stderr_contains(&enable_with_daemon, "systemctl --user start lattice")
            .map_err(|e| e.to_string())?;

        // 核验有后台时 `lattice daemon status` 与 `lattice daemon stop` 的人类可读中文输出，以及 `--json` 形状不变。
        budget.check(&spec.name);
        let human_status = rig.run_lattice(
            &world,
            &["daemon", "status"],
            budget.cap_timeout(spec.step_timeout()),
        )?;
        evidence.record_command("有后台时执行人类可读 daemon status", &human_status);
        assert_exit_code(&human_status, 0).map_err(|e| e.to_string())?;
        assert_stdout_contains_all(
            &human_status,
            &[
                "后台状态：运行中",
                "代次：",
                "在途执行者：",
                "未确认收尾操作：",
            ],
        )
        .map_err(|e| e.to_string())?;

        budget.check(&spec.name);
        let json_status = LatticeDriver::from_env()
            .map_err(|e| e.to_string())?
            .run(
                &world,
                &LatticeCommand {
                    args: vec!["--json".into(), "daemon".into(), "status".into()],
                    timeout: Some(budget.cap_timeout(spec.step_timeout())),
                },
            )
            .map_err(|e| e.to_string())?;
        evidence.record_command("有后台时执行 --json daemon status", &json_status);
        assert_exit_code(&json_status, 0).map_err(|e| e.to_string())?;
        let parsed: serde_json::Value =
            serde_json::from_slice(&json_status.stdout).map_err(|e| e.to_string())?;
        for field in ["epoch", "stopping", "owners", "unconfirmed_operations"] {
            if parsed.get(field).is_none() {
                return Err(format!("--json daemon status 缺少字段 `{field}`：{parsed}"));
            }
        }

        budget.check(&spec.name);
        let human_stop = rig.run_lattice(
            &world,
            &["daemon", "stop"],
            budget.cap_timeout(spec.step_timeout()),
        )?;
        evidence.record_command("执行人类可读 daemon stop", &human_stop);
        assert_exit_code(&human_stop, 0).map_err(|e| e.to_string())?;
        assert_stdout_contains_all(
            &human_stop,
            &[
                "后台已停止",
                "代次：",
                "剩余在途执行者：",
                "未确认收尾操作：",
            ],
        )
        .map_err(|e| e.to_string())?;
        world
            .daemon_departed()
            .map_err(|errs| format!("后台退场核验失败：{errs:?}"))?;

        // 4. `enable --now` 失败且状态根上为旧协议后台：accept、读写与线程退出共用有界期限，
        // 保留版本不兼容诊断与对端 pid，不说成“没有后台”。
        budget.check(&spec.name);
        let short_state = tempfile::Builder::new()
            .prefix("ltc-incompat-")
            .tempdir_in("/tmp")
            .map_err(|e| format!("创建短路径状态根失败：{e}"))?;
        let lattice_root = short_state.path().join("lattice");
        let local_dir = lattice_root.join("local");
        fs::create_dir_all(&local_dir).map_err(|e| e.to_string())?;
        fs::set_permissions(&lattice_root, fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
        fs::set_permissions(&local_dir, fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
        let sock_path = local_dir.join("api.sock");
        let listener = UnixListener::bind(&sock_path).map_err(|e| e.to_string())?;
        fs::set_permissions(&sock_path, fs::Permissions::from_mode(0o600))
            .map_err(|e| e.to_string())?;

        let fixture_timeout = budget.cap_timeout(Duration::from_secs(10));
        let peer_fixture = IncompatiblePeerFixture::spawn(listener, fixture_timeout)?;

        world.set_env("XDG_STATE_HOME", short_state.path().display().to_string());
        rig.clear_systemctl_log();
        let enable_incompat = rig.run_lattice(
            &world,
            &["daemon", "install"],
            budget.cap_timeout(spec.step_timeout()),
        )?;
        evidence.record_command("旧协议后台占用时 enable --now 失败", &enable_incompat);
        peer_fixture.finish_and_verify()?;

        assert_exit_code(&enable_incompat, 1).map_err(|e| e.to_string())?;
        let self_pid_fragment = format!("对端进程号：{}", std::process::id());
        assert_stderr_contains(&enable_incompat, "版本较旧且不兼容").map_err(|e| e.to_string())?;
        assert_stderr_contains(&enable_incompat, &self_pid_fragment).map_err(|e| e.to_string())?;
        assert_stderr_contains(&enable_incompat, "systemctl --user start lattice")
            .map_err(|e| e.to_string())?;
        let incompat_stderr = String::from_utf8_lossy(&enable_incompat.stderr);
        if incompat_stderr.contains("status lattice") || incompat_stderr.contains("没有后台") {
            return Err(format!(
                "版本不兼容后台不应被报告为无后台：{incompat_stderr}"
            ));
        }

        Ok(())
    })();
    evidence.finish_report(result.err()).unwrap();
}

#[test]
fn fast_daemon_install_distinguishes_exec_path_and_arg_escaping_and_rejects_invalid_paths() {
    let spec = ScenarioSpec::direct_local("daemon-install-systemd-escaping");
    let world = ScenarioWorld::new(&spec.name).expect("创建场景世界失败");
    let budget = world.budget(&spec);
    let mut evidence = ScenarioEvidence::new(&world, &spec.name, Vec::<String>::new()).unwrap();

    let result = (|| -> Result<(), String> {
        let rig = InstallRig::setup(&world, "escape-base", true)?;
        let root_str = world
            .root()
            .to_str()
            .ok_or_else(|| "场景临时根目录不是有效 UTF-8".to_owned())?;
        if !root_str
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '/' | '-' | '_' | '.'))
        {
            return Err(format!(
                "场景临时根目录含需转义字符，无法作为无转义前缀使用：{root_str:?}"
            ));
        }

        let plain_state = world.root().join("plain-state");
        fs::create_dir_all(&plain_state).map_err(|e| e.to_string())?;
        world.set_env("XDG_STATE_HOME", plain_state.display().to_string());

        // 1a. 可执行路径目录含 `$` 与 `%`（无空白）：`$` 保持字面，`%` 写为 `%%`，无需外层引号。
        budget.check(&spec.name);
        let dollar_pct_bin_dir = world.root().join("bin-$HOME-100%p");
        fs::create_dir_all(&dollar_pct_bin_dir).map_err(|e| e.to_string())?;
        let dollar_pct_lattice = dollar_pct_bin_dir.join("lattice");
        let dollar_pct_daemon = dollar_pct_bin_dir.join("lattice-daemon");
        fs::copy(&rig.lattice_bin, &dollar_pct_lattice).map_err(|e| e.to_string())?;
        fs::copy(&rig.daemon_bin, &dollar_pct_daemon).map_err(|e| e.to_string())?;
        fs::set_permissions(&dollar_pct_lattice, fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
        fs::set_permissions(&dollar_pct_daemon, fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;

        let out_unquoted = run_binary(
            &world,
            &dollar_pct_lattice,
            &["daemon", "install"],
            budget.cap_timeout(spec.step_timeout()),
        )?;
        evidence.record_command(
            "可执行路径含 $ 与 %（无空格）时执行 lattice daemon install",
            &out_unquoted,
        );
        assert_exit_code(&out_unquoted, 0).map_err(|e| e.to_string())?;
        let expected_unquoted_unit = format!(
            "[Unit]\n\
StartLimitIntervalSec=120\n\
StartLimitBurst=5\n\
\n\
[Service]\n\
Type=notify\n\
ExecStart={root_str}/bin-$HOME-100%%p/lattice-daemon --state-root {root_str}/plain-state --drain-timeout 60s\n\
TimeoutStopSec=90s\n\
NotifyAccess=main\n\
Restart=on-failure\n\
RestartSec=5\n\
RestartPreventExitStatus=2 3 4 SIGKILL\n\
\n\
[Install]\n\
WantedBy=default.target\n"
        );
        let actual_unquoted_unit =
            fs::read_to_string(rig.unit_path()).map_err(|e| e.to_string())?;
        if actual_unquoted_unit != expected_unquoted_unit {
            return Err(format!(
                "可执行路径（含 $ 与 %）的 unit 不符合手写预期：\n实际：\n{actual_unquoted_unit}\n预期：\n{expected_unquoted_unit}"
            ));
        }

        // 1b. 可执行路径目录含空格、`$` 与 `%`：整体加双引号，`$` 保持字面，`%` 写为 `%%`。
        budget.check(&spec.name);
        rig.clear_systemctl_log();
        let space_bin_dir = world.root().join("bin $HOME 100%p");
        fs::create_dir_all(&space_bin_dir).map_err(|e| e.to_string())?;
        let space_lattice = space_bin_dir.join("lattice");
        let space_daemon = space_bin_dir.join("lattice-daemon");
        fs::copy(&rig.lattice_bin, &space_lattice).map_err(|e| e.to_string())?;
        fs::copy(&rig.daemon_bin, &space_daemon).map_err(|e| e.to_string())?;
        fs::set_permissions(&space_lattice, fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
        fs::set_permissions(&space_daemon, fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;

        let out_space = run_binary(
            &world,
            &space_lattice,
            &["daemon", "install"],
            budget.cap_timeout(spec.step_timeout()),
        )?;
        evidence.record_command(
            "可执行路径含空格、$ 与 % 时执行 lattice daemon install",
            &out_space,
        );
        assert_exit_code(&out_space, 0).map_err(|e| e.to_string())?;
        let expected_space_unit = format!(
            "[Unit]\n\
StartLimitIntervalSec=120\n\
StartLimitBurst=5\n\
\n\
[Service]\n\
Type=notify\n\
ExecStart=\"{root_str}/bin $HOME 100%%p/lattice-daemon\" --state-root {root_str}/plain-state --drain-timeout 60s\n\
TimeoutStopSec=90s\n\
NotifyAccess=main\n\
Restart=on-failure\n\
RestartSec=5\n\
RestartPreventExitStatus=2 3 4 SIGKILL\n\
\n\
[Install]\n\
WantedBy=default.target\n"
        );
        let actual_space_unit = fs::read_to_string(rig.unit_path()).map_err(|e| e.to_string())?;
        if actual_space_unit != expected_space_unit {
            return Err(format!(
                "可执行路径（含空格、$ 与 %）的 unit 不符合手写预期：\n实际：\n{actual_space_unit}\n预期：\n{expected_space_unit}"
            ));
        }

        // 2. 可执行路径目录含单引号、双引号、反斜杠或制表符：写 unit 前以 2 退出，无副作用。
        for (idx, (label, forbidden_dir_name)) in [
            ("单引号", "reject-single-'quote'"),
            ("双引号", "reject-double-\"quote\""),
            ("反斜杠", "reject-back-\\slash"),
            ("制表符", "reject-tab-\there"),
        ]
        .into_iter()
        .enumerate()
        {
            budget.check(&spec.name);
            let bad_bin_dir = world.root().join(forbidden_dir_name);
            fs::create_dir_all(&bad_bin_dir).map_err(|e| format!("{label}: {e}"))?;
            let bad_lattice = bad_bin_dir.join("lattice");
            let bad_daemon = bad_bin_dir.join("lattice-daemon");
            fs::copy(&rig.lattice_bin, &bad_lattice).map_err(|e| format!("{label}: {e}"))?;
            fs::copy(&rig.daemon_bin, &bad_daemon).map_err(|e| format!("{label}: {e}"))?;
            fs::set_permissions(&bad_lattice, fs::Permissions::from_mode(0o755))
                .map_err(|e| format!("{label}: {e}"))?;
            fs::set_permissions(&bad_daemon, fs::Permissions::from_mode(0o755))
                .map_err(|e| format!("{label}: {e}"))?;

            let isolated_cfg = world.root().join(format!("reject-exec-cfg-{idx}"));
            world.set_env("XDG_CONFIG_HOME", isolated_cfg.display().to_string());
            rig.clear_systemctl_log();

            let bad_exec_out = run_binary(
                &world,
                &bad_lattice,
                &["daemon", "install"],
                budget.cap_timeout(spec.step_timeout()),
            )?;
            evidence.record_command(format!("可执行路径含{label}时拒绝 install"), &bad_exec_out);
            assert_exit_code(&bad_exec_out, 2).map_err(|e| format!("{label}: {e}"))?;
            if isolated_cfg.join("systemd/user/lattice.service").exists() {
                return Err(format!("可执行路径含{label}时不应写出 unit 文件"));
            }
            if !rig.systemctl_calls().is_empty() {
                return Err(format!(
                    "可执行路径含{label}时不应调用 systemctl：{:?}",
                    rig.systemctl_calls()
                ));
            }
        }

        // 3. 状态根参数含空格、双引号、单引号、反斜杠、制表符、`%` 与 `$`（可执行路径保持普通）：按参数规则转义。
        budget.check(&spec.name);
        let state_arg_cfg = world.root().join("state-arg-xdg-config");
        let special_state_root = world
            .root()
            .join("state dir \"quote\" 'single' \\back\ttab %i $USER");
        fs::create_dir_all(&special_state_root).map_err(|e| e.to_string())?;
        world.set_env("XDG_CONFIG_HOME", state_arg_cfg.display().to_string());
        world.set_env("XDG_STATE_HOME", special_state_root.display().to_string());
        rig.clear_systemctl_log();

        let state_arg_out = rig.run_lattice(
            &world,
            &["daemon", "install"],
            budget.cap_timeout(spec.step_timeout()),
        )?;
        evidence.record_command(
            "状态根含特殊字符（普通可执行路径）时执行 lattice daemon install",
            &state_arg_out,
        );
        assert_exit_code(&state_arg_out, 0).map_err(|e| e.to_string())?;
        let expected_state_arg_unit = format!(
            "[Unit]\n\
StartLimitIntervalSec=120\n\
StartLimitBurst=5\n\
\n\
[Service]\n\
Type=notify\n\
ExecStart={root_str}/escape-base/install-bin/lattice-daemon --state-root \"{root_str}/state dir \\\"quote\\\" 'single' \\\\back\\ttab %%i $$USER\" --drain-timeout 60s\n\
TimeoutStopSec=90s\n\
NotifyAccess=main\n\
Restart=on-failure\n\
RestartSec=5\n\
RestartPreventExitStatus=2 3 4 SIGKILL\n\
\n\
[Install]\n\
WantedBy=default.target\n"
        );
        let actual_state_arg_unit =
            fs::read_to_string(state_arg_cfg.join("systemd/user/lattice.service"))
                .map_err(|e| e.to_string())?;
        if actual_state_arg_unit != expected_state_arg_unit {
            return Err(format!(
                "状态根参数转义的 unit 不符合手写预期：\n实际：\n{actual_state_arg_unit}\n预期：\n{expected_state_arg_unit}"
            ));
        }

        // 4. 含换行符的状态根路径无法写入 systemd unit：退出 2，不写 unit，不调用 systemctl。
        budget.check(&spec.name);
        let newline_config_home = world.root().join("newline-xdg-config");
        world.set_env("XDG_CONFIG_HOME", newline_config_home.display().to_string());
        world.set_env(
            "XDG_STATE_HOME",
            world.root().join("bad\nstate").display().to_string(),
        );
        rig.clear_systemctl_log();
        let bad_out = rig.run_lattice(
            &world,
            &["daemon", "install"],
            budget.cap_timeout(spec.step_timeout()),
        )?;
        evidence.record_command("状态根含换行符时拒绝写出 unit", &bad_out);
        assert_exit_code(&bad_out, 2).map_err(|e| e.to_string())?;
        if newline_config_home
            .join("systemd/user/lattice.service")
            .exists()
        {
            return Err("路径含换行符时不应写出 unit 文件".into());
        }
        if !rig.systemctl_calls().is_empty() {
            return Err(format!(
                "路径含换行符时不应调用 systemctl：{:?}",
                rig.systemctl_calls()
            ));
        }

        Ok(())
    })();
    evidence.finish_report(result.err()).unwrap();
}

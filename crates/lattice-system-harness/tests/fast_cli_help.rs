// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 快速二进制黑盒测试：CLI 帮助表面与 `HOME` 隔离。

use lattice_system_harness::{
    FAKE_API_KEY, LatticeCommand, LatticeDaemonDriver, LatticeDriver, ScenarioSpec, ScenarioWorld,
    assert_exit_code, assert_exit_success, assert_no_secret, assert_stderr_contains,
    assert_stdout_contains_all, finalize_with_failure_artifacts,
};

fn driver() -> LatticeDriver {
    LatticeDriver::from_env().expect("LATTICE_BIN 必须指向已构建的 lattice 二进制")
}

fn daemon_driver() -> LatticeDaemonDriver {
    LatticeDaemonDriver::from_env()
        .expect("LATTICE_DAEMON_BIN 必须指向已构建的 lattice-daemon 二进制")
}

/// `lattice --help` 必须列出的公开子命令。
const EXPECTED_PUBLIC_COMMANDS: &[&str] = &["run", "daemon", "auth", "models", "session", "tui"];
const REMOVED_PUBLIC_COMMANDS: &[&str] = &["audit", "lancet"];

#[test]
fn models_help_advertises_refresh_and_rejects_list_filters_on_refresh() {
    let spec = ScenarioSpec::fast("models-refresh-help");
    let world = ScenarioWorld::new(&spec.name).unwrap();
    let driver = driver();
    let mut evidence =
        lattice_system_harness::ScenarioEvidence::new(&world, &spec.name, Vec::<String>::new())
            .unwrap();
    let result = (|| {
        for args in [
            vec!["models", "--help"],
            vec!["models", "refresh", "--help"],
        ] {
            let output = driver
                .run(
                    &world,
                    &LatticeCommand {
                        args: args.iter().map(|arg| (*arg).into()).collect(),
                        timeout: Some(spec.step_timeout()),
                    },
                )
                .unwrap();
            evidence.record_command("查询 models 帮助", &output);
            assert_exit_code(&output, 0).map_err(|error| error.to_string())?;
            assert_stdout_contains_all(&output, &["refresh"]).map_err(|error| error.to_string())?;
        }
        let output = driver
            .run(
                &world,
                &LatticeCommand {
                    args: vec!["--json", "models", "--auth", "refresh"]
                        .into_iter()
                        .map(String::from)
                        .collect(),
                    timeout: Some(spec.step_timeout()),
                },
            )
            .unwrap();
        evidence.record_command("拒绝带过滤参数的 models refresh", &output);
        assert_exit_code(&output, 2).map_err(|error| error.to_string())?;
        let error: serde_json::Value =
            serde_json::from_slice(&output.stdout).map_err(|error| error.to_string())?;
        if error["kind"] != "bad_request" {
            return Err("带过滤参数的 refresh 必须返回参数用法错误".into());
        }
        Ok(())
    })();
    evidence.finish_report(result.err()).unwrap();
}

#[test]
fn fast_home_xdg_isolated_from_developer_home() {
    let world = ScenarioWorld::new("lattice-harness-home").expect("创建场景世界失败");
    world
        .real_home_leak_check()
        .expect("场景不得复用开发者的真实 HOME");
    assert!(world.home().starts_with(world.root()));
    assert!(world.session_store_dir().starts_with(world.home()));
    // 操作系统分配的临时端口（非零代表系统已预留）。
    assert!(world.port() != 0, "场景必须持有操作系统分配的端口");
}

#[test]
fn fast_cli_help_lists_public_commands() {
    let spec = ScenarioSpec::load_fixture("cli_help_surface.json").expect("加载场景夹具失败");
    let world = ScenarioWorld::new(&spec.name).expect("创建场景世界失败");
    let driver = driver();
    let output = driver.help(&world, &spec).expect("执行 --help 失败");

    // 将退出码检查与公开子命令语义检查合并进同一个 `Result`，使语义断言失败同样落盘失败产物——
    // 避免只在进程非零退出时保留产物而在子命令缺失时裸 panic。
    let result = assert_exit_success(&output)
        .and_then(|_| assert_stdout_contains_all(&output, EXPECTED_PUBLIC_COMMANDS))
        .and_then(|_| {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for removed in REMOVED_PUBLIC_COMMANDS {
                if stdout.split_whitespace().any(|token| token == *removed) {
                    return Err(lattice_system_harness::AssertError::StdoutMissing {
                        expected: format!("已移除的子命令 `{removed}` 不得再出现在 --help 中"),
                    });
                }
            }
            Ok(())
        });
    finalize_with_failure_artifacts(&world, &spec.name, "lattice --help", &output, &[], result)
        .unwrap_or_else(|report| panic!("--help 应以退出码 0 列出全部公开子命令\n{report}"));

    let daemon_help = driver
        .run(
            &world,
            &LatticeCommand {
                args: vec!["daemon".into(), "--help".into()],
                timeout: Some(spec.step_timeout()),
            },
        )
        .unwrap();
    let daemon_help_result = assert_exit_code(&daemon_help, 0)
        .and_then(|_| assert_stdout_contains_all(&daemon_help, &["status", "stop", "install"]))
        .and_then(|_| {
            let stdout = String::from_utf8_lossy(&daemon_help.stdout);
            if stdout.split_whitespace().any(|token| token == "serve") {
                return Err(lattice_system_harness::AssertError::StdoutMissing {
                    expected: "已移除的 `daemon serve` 不得再出现在 `lattice daemon --help` 中"
                        .into(),
                });
            }
            Ok(())
        });
    finalize_with_failure_artifacts(
        &world,
        &spec.name,
        "lattice daemon --help",
        &daemon_help,
        &[],
        daemon_help_result,
    )
    .unwrap_or_else(|report| panic!("lattice daemon --help 不符合预期\n{report}"));

    for (args, expected) in [
        (vec!["session", "--help"], vec!["usage"]),
        (
            vec!["run", "--help"],
            vec![
                "--model",
                "--effort",
                "<provider>/<model>",
                "inherit",
                "off, minimal, low, medium, high, xhigh, max",
            ],
        ),
        (
            vec!["session", "send", "--help"],
            vec![
                "--model",
                "--effort",
                "<provider>/<model>",
                "inherit",
                "off, minimal, low, medium, high, xhigh, max",
            ],
        ),
        (
            vec!["session", "usage", "--help"],
            vec![
                "SESSION_ID",
                "current path",
                "whole-session totals",
                "Unknown counts and costs are shown as '-'",
                "--json",
            ],
        ),
    ] {
        let output = driver
            .run(
                &world,
                &LatticeCommand {
                    args: args.iter().map(|arg| (*arg).into()).collect(),
                    timeout: Some(spec.step_timeout()),
                },
            )
            .unwrap();
        let result = assert_exit_code(&output, 0)
            .and_then(|_| assert_stdout_contains_all(&output, &expected));
        finalize_with_failure_artifacts(&world, &spec.name, &args.join(" "), &output, &[], result)
            .unwrap_or_else(|report| panic!("session usage 帮助输出不符合预期\n{report}"));
    }
    for args in [vec!["session", "usage"], vec!["session", "usage", "--json"]] {
        let output = driver
            .run(
                &world,
                &LatticeCommand {
                    args: args.iter().map(|arg| (*arg).into()).collect(),
                    timeout: Some(spec.step_timeout()),
                },
            )
            .unwrap();
        let result = assert_exit_code(&output, 2).and_then(|_| {
            if args.contains(&"--json") {
                assert_stdout_contains_all(
                    &output,
                    &["\"type\":\"error\"", "\"kind\":\"bad_request\""],
                )
            } else {
                assert_stderr_contains(&output, "SESSION_ID")
            }
        });
        finalize_with_failure_artifacts(&world, &spec.name, &args.join(" "), &output, &[], result)
            .unwrap_or_else(|report| panic!("session usage 缺少会话标识时应报错退出\n{report}"));
    }
    for command in [vec!["run"], vec!["session", "send"]] {
        for json in [false, true] {
            for (flag, value) in [("--model", "invalid"), ("--effort", "none")] {
                let mut args = Vec::new();
                if json {
                    args.push("--json");
                }
                args.extend(command.clone());
                args.extend(["hello", "--agent", "reader", flag, value]);
                let output = driver
                    .run(
                        &world,
                        &LatticeCommand {
                            args: args.iter().map(|arg| (*arg).into()).collect(),
                            timeout: Some(spec.step_timeout()),
                        },
                    )
                    .unwrap();
                let result = assert_exit_code(&output, 2).and_then(|_| {
                    if json {
                        assert_stdout_contains_all(
                            &output,
                            &["\"type\":\"error\"", "\"kind\":\"bad_request\""],
                        )
                    } else {
                        assert_stderr_contains(&output, "invalid value")
                    }
                });
                finalize_with_failure_artifacts(
                    &world,
                    &spec.name,
                    &args.join(" "),
                    &output,
                    &[],
                    result,
                )
                .unwrap();
            }
        }
    }
}

#[test]
fn fast_bare_lattice_prints_help_and_exits_zero() {
    let spec = ScenarioSpec::fast("lattice-harness-bare");
    let world = ScenarioWorld::new(&spec.name).expect("创建场景世界失败");
    let output = driver()
        .bare_invoke(&world, &spec)
        .expect("裸调用 lattice 失败");

    let result = assert_exit_success(&output)
        .and_then(|_| assert_stdout_contains_all(&output, &["session"]));
    finalize_with_failure_artifacts(&world, &spec.name, "lattice", &output, &[], result)
        .unwrap_or_else(|report| panic!("裸调用 lattice 应以退出码 0 提及 session\n{report}"));
}

#[test]
fn fast_unknown_command_rejected() {
    let spec = ScenarioSpec::fast("lattice-harness-unknown");
    let world = ScenarioWorld::new(&spec.name).expect("创建场景世界失败");
    let output = driver()
        .run(
            &world,
            &LatticeCommand {
                args: vec!["not-a-real-command".into()],
                timeout: Some(spec.step_timeout()),
            },
        )
        .expect("执行未知子命令失败");

    let result = assert_exit_code(&output, 2)
        .and_then(|_| assert_stderr_contains(&output, "unrecognized subcommand"));
    finalize_with_failure_artifacts(
        &world,
        &spec.name,
        "lattice not-a-real-command",
        &output,
        &[],
        result,
    )
    .unwrap_or_else(|report| {
        panic!("未知子命令应以退出码 2 报告 unrecognized subcommand\n{report}")
    });
}

#[test]
fn fast_lattice_daemon_chinese_help_and_argument_validation() {
    let spec = ScenarioSpec::fast("lattice-daemon-cli-args");
    let world = ScenarioWorld::new(&spec.name).expect("创建场景世界失败");
    let daemon = daemon_driver();
    let mut evidence =
        lattice_system_harness::ScenarioEvidence::new(&world, &spec.name, Vec::<String>::new())
            .unwrap();
    let abs_root = world
        .session_store_dir()
        .parent()
        .expect("场景状态根")
        .display()
        .to_string();

    let result = (|| -> Result<(), String> {
        for flag in ["--help", "-h"] {
            let output = daemon
                .run(
                    &world,
                    &LatticeCommand {
                        args: vec![flag.into()],
                        timeout: Some(spec.step_timeout()),
                    },
                )
                .map_err(|e| e.to_string())?;
            evidence.record_command(format!("查询 lattice-daemon 帮助（{flag}）"), &output);
            assert_exit_code(&output, 0).map_err(|e| e.to_string())?;
            assert_stdout_contains_all(
                &output,
                &[
                    "用法：lattice-daemon --state-root <绝对路径> [--drain-timeout <时长>]",
                    "选项：",
                    "--state-root <绝对路径>",
                    "--drain-timeout <时长>",
                    "-h, --help",
                    "显示帮助。",
                ],
            )
            .map_err(|e| e.to_string())?;
            let stdout = String::from_utf8_lossy(&output.stdout);
            for forbidden in ["Usage:", "Options:", "[default:"] {
                if stdout.contains(forbidden) {
                    return Err(format!("中文帮助输出不应包含 `{forbidden}`：{stdout}"));
                }
            }
        }

        let invalid_cases: Vec<(&str, Vec<String>, &str)> = vec![
            ("缺 --state-root（裸调用）", vec![], "--state-root"),
            (
                "缺 --state-root（仅带 --drain-timeout）",
                vec!["--drain-timeout".into(), "30s".into()],
                "--state-root",
            ),
            (
                "相对路径 --state-root",
                vec!["--state-root".into(), "relative/state".into()],
                "绝对路径",
            ),
            (
                "当前目录相对路径 --state-root",
                vec!["--state-root".into(), "./state".into()],
                "绝对路径",
            ),
            (
                "非法 --drain-timeout 单位 1h",
                vec![
                    "--state-root".into(),
                    abs_root.clone(),
                    "--drain-timeout".into(),
                    "1h".into(),
                ],
                "排空时长",
            ),
            (
                "负数 --drain-timeout -1",
                vec![
                    "--state-root".into(),
                    abs_root.clone(),
                    "--drain-timeout".into(),
                    "-1".into(),
                ],
                "排空时长",
            ),
            (
                "小数 --drain-timeout 1.5s",
                vec![
                    "--state-root".into(),
                    abs_root,
                    "--drain-timeout".into(),
                    "1.5s".into(),
                ],
                "排空时长",
            ),
        ];

        for (label, args, expected_stderr) in invalid_cases {
            let output = daemon
                .run(
                    &world,
                    &LatticeCommand {
                        args,
                        timeout: Some(spec.step_timeout()),
                    },
                )
                .map_err(|e| e.to_string())?;
            evidence.record_command(format!("校验参数错误（{label}）"), &output);
            assert_exit_code(&output, 2).map_err(|e| format!("{label}: {e}"))?;
            assert_stderr_contains(&output, expected_stderr)
                .map_err(|e| format!("{label}: {e}"))?;
        }
        Ok(())
    })();
    evidence.finish_report(result.err()).unwrap();
}

#[test]
fn fast_lattice_daemon_group_chinese_help_and_argument_validation() {
    let spec = ScenarioSpec::fast("lattice-daemon-subcommands-help");
    let world = ScenarioWorld::new(&spec.name).expect("创建场景世界失败");
    let cli = driver();
    let mut evidence =
        lattice_system_harness::ScenarioEvidence::new(&world, &spec.name, Vec::<String>::new())
            .unwrap();

    let result = (|| -> Result<(), String> {
        // 顶层 `lattice --help` 中的 `daemon` 条目保持与同列表一致。
        let top_help = cli.help(&world, &spec).map_err(|e| e.to_string())?;
        evidence.record_command("查询顶层 lattice --help", &top_help);
        assert_exit_code(&top_help, 0).map_err(|e| e.to_string())?;
        assert_stdout_contains_all(
            &top_help,
            &["Manage the local daemon for this user and state root"],
        )
        .map_err(|e| e.to_string())?;

        let forbidden_english = [
            "Usage:",
            "Commands:",
            "Options:",
            "Print help",
            "Verbose logging",
            "Emit JSONL",
            "[default:",
            "Manage the local daemon",
            "Query the running daemon",
            "Stop accepting new work",
        ];

        for flag in ["--help", "-h"] {
            let group_out = cli
                .run(
                    &world,
                    &LatticeCommand {
                        args: vec!["daemon".into(), flag.into()],
                        timeout: Some(spec.step_timeout()),
                    },
                )
                .map_err(|e| e.to_string())?;
            evidence.record_command(format!("查询 lattice daemon 帮助（{flag}）"), &group_out);
            assert_exit_code(&group_out, 0).map_err(|e| e.to_string())?;
            assert_stdout_contains_all(
                &group_out,
                &[
                    "管理当前用户与状态根的本地后台守护进程。",
                    "用法：lattice daemon <命令>",
                    "命令：",
                    "status",
                    "stop",
                    "install",
                    "选项：",
                    "-h, --help",
                    "显示帮助。",
                ],
            )
            .map_err(|e| e.to_string())?;
            let group_stdout = String::from_utf8_lossy(&group_out.stdout);
            for forbidden in forbidden_english {
                if group_stdout.contains(forbidden) {
                    return Err(format!(
                        "`lattice daemon {flag}` 不应残留英文 `{forbidden}`：{group_stdout}"
                    ));
                }
            }

            for (sub, expected_snippets) in [
                (
                    "status",
                    &[
                        "查询运行中的本地后台状态",
                        "用法：lattice daemon status [--json]",
                        "选项：",
                        "--json",
                        "-h, --help",
                        "显示帮助。",
                    ][..],
                ),
                (
                    "stop",
                    &[
                        "停止接收新请求",
                        "用法：lattice daemon stop [--json]",
                        "选项：",
                        "--json",
                        "-h, --help",
                        "显示帮助。",
                    ][..],
                ),
                (
                    "install",
                    &[
                        "写出用户级 systemd 服务单元",
                        "不支持 --json",
                        "用法：lattice daemon install [--drain-timeout <时长>]",
                        "选项：",
                        "--drain-timeout <时长>",
                        "-h, --help",
                        "显示帮助。",
                    ][..],
                ),
            ] {
                let sub_out = cli
                    .run(
                        &world,
                        &LatticeCommand {
                            args: vec!["daemon".into(), sub.into(), flag.into()],
                            timeout: Some(spec.step_timeout()),
                        },
                    )
                    .map_err(|e| e.to_string())?;
                evidence.record_command(
                    format!("查询 lattice daemon {sub} 帮助（{flag}）"),
                    &sub_out,
                );
                assert_exit_code(&sub_out, 0).map_err(|e| format!("{sub} {flag}: {e}"))?;
                assert_stdout_contains_all(&sub_out, expected_snippets)
                    .map_err(|e| format!("{sub} {flag}: {e}"))?;
                let sub_stdout = String::from_utf8_lossy(&sub_out.stdout);
                for forbidden in forbidden_english {
                    if sub_stdout.contains(forbidden) {
                        return Err(format!(
                            "`lattice daemon {sub} {flag}` 不应残留英文 `{forbidden}`：{sub_stdout}"
                        ));
                    }
                }
                if sub == "install"
                    && (sub_stdout.contains("-j, --json")
                        || sub_stdout.contains("以 JSON 输出结果"))
                {
                    return Err(format!(
                        "`lattice daemon install {flag}` 选项表不应把 `--json` 列为可用选项：{sub_stdout}"
                    ));
                }
            }
        }

        for (label, args, expected_stderr) in [
            ("缺少子命令", vec!["daemon"], "缺少子命令"),
            (
                "已移除的 serve 子命令",
                vec!["daemon", "serve"],
                "未知子命令",
            ),
            (
                "install 非法排空单位",
                vec!["daemon", "install", "--drain-timeout", "1h"],
                "排空时长",
            ),
            (
                "install 未知参数",
                vec!["daemon", "install", "--unknown"],
                "未知参数",
            ),
        ] {
            let out = cli
                .run(
                    &world,
                    &LatticeCommand {
                        args: args.into_iter().map(String::from).collect(),
                        timeout: Some(spec.step_timeout()),
                    },
                )
                .map_err(|e| e.to_string())?;
            evidence.record_command(format!("校验 daemon 参数错误（{label}）"), &out);
            assert_exit_code(&out, 2).map_err(|e| format!("{label}: {e}"))?;
            assert_stderr_contains(&out, expected_stderr).map_err(|e| format!("{label}: {e}"))?;
        }

        // JSON 模式下 `lattice daemon` 组的参数错误与 `install --json` 均保持既有 JSON 错误形状、输出中文且保持脱敏。
        for (label, args, expected_msg) in [
            ("JSON 缺少子命令", vec!["--json", "daemon"], "缺少子命令"),
            (
                "JSON 未知子命令",
                vec!["--json", "daemon", "serve"],
                "未知子命令",
            ),
            (
                "JSON status 未知参数",
                vec!["--json", "daemon", "status", "--bad"],
                "未知参数",
            ),
            (
                "JSON install 非法排空单位",
                vec!["--json", "daemon", "install", "--drain-timeout", "1h"],
                "排空时长",
            ),
            (
                "install 后置 --json",
                vec!["daemon", "install", "--json"],
                "不支持 `--json`",
            ),
            (
                "install 前置 --json",
                vec!["--json", "daemon", "install"],
                "不支持 `--json`",
            ),
            (
                "install 短标志 -j",
                vec!["-j", "daemon", "install"],
                "不支持 `--json`",
            ),
            (
                "JSON 参数含秘密字面脱敏",
                vec!["--json", "daemon", "status", FAKE_API_KEY],
                "未知参数",
            ),
            (
                "JSON 排空时长含秘密字面脱敏",
                vec![
                    "--json",
                    "daemon",
                    "install",
                    "--drain-timeout",
                    FAKE_API_KEY,
                ],
                "排空时长",
            ),
        ] {
            let out = cli
                .run(
                    &world,
                    &LatticeCommand {
                        args: args.into_iter().map(String::from).collect(),
                        timeout: Some(spec.step_timeout()),
                    },
                )
                .map_err(|e| e.to_string())?;
            evidence.record_command(format!("校验 daemon JSON 参数错误（{label}）"), &out);
            assert_exit_code(&out, 2).map_err(|e| format!("{label}: {e}"))?;
            assert_no_secret(&out, FAKE_API_KEY).map_err(|e| format!("{label}: {e}"))?;
            if !out.stderr.is_empty() {
                return Err(format!(
                    "{label}: JSON 模式参数错误不应写标准错误：{}",
                    String::from_utf8_lossy(&out.stderr)
                ));
            }
            let stdout = String::from_utf8_lossy(&out.stdout);
            let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
                .map_err(|e| format!("{label}: 标准输出不是合法 JSON（{e}）：{stdout}"))?;
            let obj = parsed
                .as_object()
                .ok_or_else(|| format!("{label}: 应输出 JSON 对象：{parsed}"))?;
            if obj.len() != 3 {
                return Err(format!(
                    "{label}: JSON 错误字段数应保持 3 项（type/kind/message），实际：{parsed}"
                ));
            }
            if parsed.get("type").and_then(serde_json::Value::as_str) != Some("error")
                || parsed.get("kind").and_then(serde_json::Value::as_str) != Some("bad_request")
            {
                return Err(format!("{label}: JSON 错误 type/kind 不符：{parsed}"));
            }
            let msg = parsed
                .get("message")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| format!("{label}: message 字段缺失或非字符串：{parsed}"))?;
            if !msg.contains(expected_msg) || msg.contains("invalid command") {
                return Err(format!(
                    "{label}: 期望中文错误包含 `{expected_msg}` 且不含英文回退，实际：{msg}"
                ));
            }
        }

        Ok(())
    })();
    evidence.finish_report(result.err()).unwrap();
}

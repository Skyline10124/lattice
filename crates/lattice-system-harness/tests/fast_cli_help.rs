// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 快速二进制黑盒测试：CLI 帮助表面与 `HOME` 隔离。

use lattice_system_harness::{
    LatticeCommand, LatticeDriver, ScenarioSpec, ScenarioWorld, assert_exit_code,
    assert_exit_success, assert_stderr_contains, assert_stdout_contains_all,
    finalize_with_failure_artifacts,
};

fn driver() -> LatticeDriver {
    LatticeDriver::from_env().expect("LATTICE_BIN 必须指向已构建的 lattice 二进制")
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
fn fast_daemon_serve_invalid_drain_timeout_exits_two() {
    let spec = ScenarioSpec::fast("lattice-harness-invalid-drain-timeout");
    let world = ScenarioWorld::new(&spec.name).expect("创建场景世界失败");
    let output = driver()
        .run(
            &world,
            &LatticeCommand {
                args: vec![
                    "daemon".into(),
                    "serve".into(),
                    "--drain-timeout".into(),
                    "1h".into(),
                ],
                timeout: Some(spec.step_timeout()),
            },
        )
        .expect("执行无效 --drain-timeout 命令失败");

    let result = assert_exit_code(&output, 2)
        .and_then(|_| assert_stderr_contains(&output, "invalid value '1h'"));
    finalize_with_failure_artifacts(
        &world,
        &spec.name,
        "lattice daemon serve --drain-timeout 1h",
        &output,
        &[],
        result,
    )
    .unwrap_or_else(|report| {
        panic!("无效的 --drain-timeout 应以退出码 2 报告参数值校验错误\n{report}")
    });
}

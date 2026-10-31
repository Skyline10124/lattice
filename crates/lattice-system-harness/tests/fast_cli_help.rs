// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Fast binary black-box: CLI help surface and HOME isolation.

use lattice_system_harness::{
    LatticeCommand, LatticeDriver, ScenarioSpec, ScenarioWorld, assert_exit_code,
    assert_exit_success, assert_stderr_contains, assert_stdout_contains_all,
    finalize_with_failure_artifacts,
};

fn driver() -> LatticeDriver {
    LatticeDriver::from_env().expect("LATTICE_BIN must point to a built lattice binary")
}

/// Public subcommands the `lattice --help` surface must advertise.
const EXPECTED_PUBLIC_COMMANDS: &[&str] = &["auth", "models", "session", "audit", "lancet", "tui"];

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
            evidence.record_command("models help", &output);
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
        evidence.record_command("models refresh filters rejected", &output);
        assert_exit_code(&output, 2).map_err(|error| error.to_string())?;
        let error: serde_json::Value =
            serde_json::from_slice(&output.stdout).map_err(|error| error.to_string())?;
        if error["kind"] != "bad_request" {
            return Err("refresh filters must be a usage error".into());
        }
        Ok(())
    })();
    evidence.finish_report(result.err()).unwrap();
}

#[test]
fn fast_home_xdg_isolated_from_developer_home() {
    let world = ScenarioWorld::new("lattice-harness-home").expect("world");
    world
        .real_home_leak_check()
        .expect("scenario must not use developer HOME");
    assert!(world.home().starts_with(world.root()));
    assert!(world.session_store_dir().starts_with(world.home()));
    // OS-assigned ephemeral port (non-zero means the OS reserved one).
    assert!(world.port() != 0, "scenario must hold an OS-assigned port");
}

#[test]
fn fast_cli_help_lists_public_commands() {
    let spec = ScenarioSpec::load_fixture("cli_help_surface.json").expect("fixture");
    let world = ScenarioWorld::new(&spec.name).expect("world");
    let driver = driver();
    let output = driver.help(&world, &spec).expect("run --help");

    // Combine the exit-success and semantic (public-commands) checks into one
    // Result so a semantic failure also triggers failure-artifact capture —
    // previously only the exit-success branch was wired to capture, and the
    // public-commands loop below panicked without writing any artifacts.
    let result = assert_exit_success(&output)
        .and_then(|_| assert_stdout_contains_all(&output, EXPECTED_PUBLIC_COMMANDS));
    finalize_with_failure_artifacts(&world, &spec.name, "lattice --help", &output, &[], result)
        .unwrap_or_else(|report| panic!("help exits 0 and lists all public commands\n{report}"));

    for (args, expected) in [
        (vec!["session", "--help"], vec!["usage"]),
        (
            vec!["run", "--help"],
            vec![
                "--model",
                "--effort",
                "<provider>/<model>",
                "inherit",
                "off",
                "minimal",
                "low",
                "medium",
                "high",
                "xhigh",
                "max",
            ],
        ),
        (
            vec!["session", "send", "--help"],
            vec![
                "--model",
                "--effort",
                "<provider>/<model>",
                "inherit",
                "off",
                "minimal",
                "low",
                "medium",
                "high",
                "xhigh",
                "max",
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
            .unwrap_or_else(|report| panic!("session usage help\n{report}"));
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
            .unwrap_or_else(|report| panic!("usage requires a session ID\n{report}"));
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
    let world = ScenarioWorld::new(&spec.name).expect("world");
    let output = driver().bare_invoke(&world, &spec).expect("bare invoke");

    let result = assert_exit_success(&output)
        .and_then(|_| assert_stdout_contains_all(&output, &["session"]));
    finalize_with_failure_artifacts(&world, &spec.name, "lattice", &output, &[], result)
        .unwrap_or_else(|report| panic!("bare lattice exits 0 and mentions session\n{report}"));
}

#[test]
fn fast_unknown_command_rejected() {
    let spec = ScenarioSpec::fast("lattice-harness-unknown");
    let world = ScenarioWorld::new(&spec.name).expect("world");
    let output = driver()
        .run(
            &world,
            &LatticeCommand {
                args: vec!["not-a-real-command".into()],
                timeout: Some(spec.step_timeout()),
            },
        )
        .expect("run unknown");

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
        panic!("unknown command exits 2 with 'unrecognized subcommand'\n{report}")
    });
}

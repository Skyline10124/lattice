// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 凭据命令的真实二进制场景，秘密仅经标准输入录入。

use std::{fs, os::unix::fs::PermissionsExt};

use lattice_system_harness::{
    CommandOutcome, LatticeCommand, LatticeDriver, ScenarioBudget, ScenarioEvidence, ScenarioSpec,
    ScenarioWorld, assert_exit_code, assert_no_secret,
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
    evidence.record_command(args.join(" ").replace(KEY, "[REDACTED]"), &output);
    assert_no_secret(&output, KEY).map_err(|error| error.to_string())?;
    Ok(output)
}

fn state(output: &CommandOutcome, provider: &str) -> Result<String, String> {
    let status: serde_json::Value =
        serde_json::from_slice(&output.stdout).map_err(|_| "status must be valid JSON")?;
    status["providers"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["provider"] == provider))
        .and_then(|row| row["status"].as_str())
        .map(str::to_owned)
        .ok_or_else(|| "provider status is missing".into())
}

#[test]
fn fast_auth_stdin_status_permissions_and_rejections() {
    let spec = ScenarioSpec::direct_local("fast-auth-cli");
    let world = ScenarioWorld::new(&spec.name).expect("world");
    let budget = world.budget(&spec);
    let driver =
        LatticeDriver::from_env().expect("LATTICE_BIN must point to a built lattice binary");
    let mut evidence = ScenarioEvidence::new(&world, &spec.name, [KEY.into()]).expect("evidence");
    world.set_env("LATTICE_LOG", "trace");
    let state_root = world
        .session_store_dir()
        .parent()
        .unwrap()
        .display()
        .to_string();
    let daemon = driver
        .start(
            &world,
            &LatticeCommand {
                args: vec![
                    "daemon".into(),
                    "serve".into(),
                    "--state-root".into(),
                    state_root,
                ],
                timeout: Some(budget.cap_timeout(spec.step_timeout())),
            },
        )
        .expect("start explicit daemon with captured output");
    let result = (|| -> Result<(), String> {
        let deadline = std::time::Instant::now() + budget.cap_timeout(spec.step_timeout());
        loop {
            let status = run(
                &world,
                &driver,
                &budget,
                &mut evidence,
                &["--json", "daemon", "status"],
                None,
            )?;
            if status.is_clean_exit(0) {
                break;
            }
            if std::time::Instant::now() >= deadline {
                return Err("explicit daemon did not start".into());
            }
            std::thread::yield_now();
        }
        if world.daemon_pids().map_err(|error| error.to_string())? != vec![daemon.pgid()] {
            return Err("auth commands must use the explicitly started daemon".into());
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
        if state(&initial, "openai")? != "missing" {
            return Err("initial key must be missing".into());
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
            .map_err(|_| "set must return JSON")?
            != serde_json::json!({"type":"auth","action":"set","provider":"openai"})
        {
            return Err("set must identify its type, action and provider".into());
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
            return Err("text set must identify its provider and action".into());
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
                .map_err(|_| "key rejection must be JSON")?;
            if error["kind"] != "bad_request"
                || error["message"]
                    != "API key must be 1–16384 bytes and contain no control characters"
            {
                return Err(
                    "key rules must be enforced by the backend without a file-error prefix".into(),
                );
            }
        }
        let path = world.home().join(".lattice/auth.json");
        let metadata = fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
        if !metadata.is_file() || metadata.permissions().mode() & 0o777 != 0o600 {
            return Err("credential file must be regular and 0600".into());
        }
        let mut document: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).map_err(|error| error.to_string())?)
                .map_err(|_| "saved credentials must be valid JSON")?;
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
            return Err("set key must be ready".into());
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
            return Err("text status must report openai ready".into());
        }
        if !std::str::from_utf8(&text.stdout)
            .unwrap_or_default()
            .lines()
            .any(|line| {
                line == "chatgpt ready email=user@example.com expires_at=2026-01-01T00:00:00+00:00"
            })
        {
            return Err("status metadata must use key=value fields".into());
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
            return Err("text remove must identify the removed provider".into());
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
            .map_err(|_| "remove must return JSON")?
            != serde_json::json!({"type":"auth","action":"remove","provider":"openai","removed":false})
        {
            return Err("remove must report when no key existed".into());
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
            return Err("text remove must distinguish a missing key".into());
        }
        let status = run(
            &world,
            &driver,
            &budget,
            &mut evidence,
            &["--json", "auth", "status"],
            None,
        )?;
        if state(&status, "openai")? != "missing" {
            return Err("removed key must be missing".into());
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
            return Err("malformed file must report invalid".into());
        }
        Ok(())
    })();
    let mut failures: Vec<String> = result.err().into_iter().collect();
    if let Err(errors) = world.stop_daemon() {
        failures.extend(errors);
    }
    match daemon.wait(None) {
        Ok(output) => {
            evidence.record_command("lattice daemon serve (LATTICE_LOG=trace)", &output);
            for result in [assert_exit_code(&output, 0), assert_no_secret(&output, KEY)] {
                if let Err(error) = result {
                    failures.push(error.to_string());
                }
            }
        }
        Err(error) => failures.push(error.to_string()),
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
        let world = ScenarioWorld::new(&spec.name).expect("world");
        if let Some(home) = home {
            world.set_env("HOME", home);
        } else {
            world.unset_env("HOME");
        }
        let budget = world.budget(&spec);
        let driver = LatticeDriver::from_env().expect("built lattice binary");
        let mut evidence =
            ScenarioEvidence::new(&world, &spec.name, [KEY.into()]).expect("evidence");
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
                    .map_err(|_| "configuration rejection must be JSON")?;
                if error["kind"] != "bad_request" || error["message"] != reason {
                    return Err(
                        "configuration rejection must preserve the HOME failure reason".into(),
                    );
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
                .map_err(|_| "configuration rejection must be JSON")?;
            if error["message"] != reason {
                return Err("a running daemon must retain its startup HOME".into());
            }
            Ok(())
        })();
        evidence
            .finish_report(result.err())
            .unwrap_or_else(|report| panic!("{report}"));
    }
}

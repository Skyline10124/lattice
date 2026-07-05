// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Minimal full-turn scenario (RFC #19 layer 3: real Microsandbox).
//!
//! This test is `#[ignore]`-gated and opted into via `LATTICE_HARNESS_MSB=1`
//! (see `layer_guard`). Ordinary `cargo test` skips it; the local full gate
//! and release gate set the env var. When the env var is set but the MSB
//! stack is not ready, `require_msb_env` panics — the suite fails rather
//! than silently passing (RFC #19 irreversible boundary 7).
//!
//! What it proves (issue #21 acceptance point: a minimal complete turn):
//!
//! - The fake LLM receives a real Chat Completions request through the
//!   formal `OPENAI_COMPATIBLE_*` configuration.
//! - `lattice --json session send` drives a complete turn over the real
//!   `stream_turn` → Microsandbox Upper path and emits the public JSONL.
//! - The required event subsequence is present
//!   (turn_started → model_started → token_delta+ → model_finished →
//!   turn_committed{status=completed}).
//! - The exit code is 0 and agrees with the stream-derived exit code.
//! - `lattice session log --json` reflects the new commit, cross-validating
//!   via a public CLI surface.
//!
//! It does **not** assert on TUI rendering, prompt shape, or full golden
//! output (RFC #19 decision five).

#![cfg(unix)]

use std::path::PathBuf;
use std::process::Output;
use std::time::Duration;

use lattice_system_harness::fake_llm::CapturedRequest;
use lattice_system_harness::session_send::{SendExitCode, SessionSendDriver};
use lattice_system_harness::{
    AssertError, LatticeDriver, ScenarioLayer, ScenarioSpec, ScenarioWorld, SseScript,
    assert_event_subsequence, derive_send_exit_code, require_msb_env,
};

/// Minimal project-local agent profile. `[workspace]` is mandatory; the
/// image is supplied via the `LATTICE_MSB_IMAGE` env var at run time so the
/// owner's local full gate can pin the image they have cached (daocloud
/// proxy, per the handoff). The profile deliberately enables no CEP tools —
/// this is a plain-text turn, so no `tool_calls` should appear.
const AGENT_TOML: &str = r#"schema_version = 1

[agent]
name = "msb-plain"
model = "compat:msb-plain"

[prompt]
system = "You are a concise test agent. Reply briefly."

[workspace]

[tools]
enabled = []
"#;

fn driver() -> LatticeDriver {
    LatticeDriver::from_env().expect("LATTICE_BIN must point to a built lattice binary")
}

fn failure_dir(scenario_name: &str) -> PathBuf {
    match std::env::var("LATTICE_HARNESS_FAILURE_DIR") {
        Ok(base) => PathBuf::from(base).join(scenario_name),
        Err(_) => PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("harness-failures")
            .join(scenario_name),
    }
}

/// Write the full failure bundle (stdout/stderr/command/world context +
/// fake LLM request log + workspace before/after diff + cross-validation
/// outputs) for a full-turn scenario. The world context is built by the
/// shared `write_world_context` helper (the same one
/// `assert_with_failure_artifacts` uses), so the two paths cannot drift.
///
/// Every byte written is redacted against `secrets` **before** writing, then
/// the whole bundle is scanned **after** writing (`scan_artifacts_for_secrets`)
/// as defense-in-depth (RFC #19 decision eight). A leak caught by the scan
/// is patched in place and recorded in `sentinel_scan.txt`. The cross-
/// validation outputs (`session log/status --json`) are also redacted before
/// writing — they come straight from the product and could echo prompt text.
#[allow(clippy::too_many_arguments)]
fn write_full_bundle(
    world: &ScenarioWorld,
    scenario_name: &str,
    send_output: &Output,
    log_output: &Output,
    status_output: &Output,
    captures: &[CapturedRequest],
    before: &lattice_system_harness::WorkspaceSnapshot,
    secrets: &[&str],
) {
    use lattice_system_harness::artifacts::{
        capture_from_output, redact, scan_artifacts_for_secrets, write_failure_artifacts,
        write_fake_llm_log, write_workspace_diff, write_world_context,
    };
    let dir = failure_dir(scenario_name);
    let send_art = capture_from_output(
        dir.clone(),
        send_output,
        "lattice --json session send <id> ping",
        secrets,
    );
    let _ = write_failure_artifacts(&send_art);
    let _ = std::fs::write(
        dir.join("session_log.stdout.txt"),
        redact(&String::from_utf8_lossy(&log_output.stdout), secrets).as_bytes(),
    );
    let _ = std::fs::write(
        dir.join("session_log.stderr.txt"),
        redact(&String::from_utf8_lossy(&log_output.stderr), secrets).as_bytes(),
    );
    let _ = std::fs::write(
        dir.join("session_status.stdout.txt"),
        redact(&String::from_utf8_lossy(&status_output.stdout), secrets).as_bytes(),
    );
    let _ = std::fs::write(
        dir.join("session_status.stderr.txt"),
        redact(&String::from_utf8_lossy(&status_output.stderr), secrets).as_bytes(),
    );
    let _ = write_world_context(
        &dir,
        world,
        scenario_name,
        &send_art.command_summary,
        secrets,
    );
    let _ = write_fake_llm_log(&dir, captures, secrets);
    let after = match lattice_system_harness::WorkspaceSnapshot::capture(world.project_dir()) {
        Ok(s) => s,
        Err(_) => before.clone(),
    };
    let _ = write_workspace_diff(&dir, before, &after, secrets);
    // RFC #19 decision eight: scan after writing. Any leaked file is patched
    // in place and recorded; the scan never masks the assertion failure (the
    // caller panics with the original error after this returns).
    let leaked = scan_artifacts_for_secrets(&dir, secrets);
    if !leaked.is_empty() {
        let _ = std::fs::write(
            dir.join("sentinel_scan.txt"),
            format!(
                "post-write sentinel scan redacted {} file(s): {leaked:?}\n",
                leaked.len()
            ),
        );
    }
}

#[test]
#[ignore = "Microsandbox scenario: run with LATTICE_HARNESS_MSB=1 (full gate)"]
fn msb_plain_text_turn_commits_and_exit_zero() {
    let driver = driver();
    require_msb_env(&driver);

    let spec = ScenarioSpec {
        name: "msb-plain-text-turn".into(),
        layer: ScenarioLayer::Microsandbox,
        step_timeout_secs: 120,
        total_timeout_secs: Some(300),
    };
    let budget = spec.start_budget();
    let mut world = ScenarioWorld::new(&spec.name).expect("world");
    world
        .write_project_file(".lattice/agents/msb-plain/agent.toml", AGENT_TOML)
        .expect("write agent.toml");
    world.init_git_project().expect("init git project");
    budget.check(&spec.name);

    let server = lattice_system_harness::FakeLlmServer::start(SseScript::plain("ok"))
        .expect("start fake llm");
    world.set_env("OPENAI_COMPATIBLE_BASE_URL", server.base_url());
    world.set_env("OPENAI_COMPATIBLE_MODEL", "msb-plain");
    world.set_env(
        "OPENAI_COMPATIBLE_API_KEY",
        lattice_system_harness::FAKE_API_KEY,
    );

    // Create the session via the public CLI.
    budget.check(&spec.name);
    let send = SessionSendDriver::new(&driver, &world, &spec, "msb-plain", "main")
        .expect("create session");

    // Capture the workspace tree before the turn so a failure can diff it
    // against the post-turn tree (PRD user story 21: "工作区差异").
    let before = lattice_system_harness::WorkspaceSnapshot::capture(world.project_dir())
        .expect("workspace snapshot before turn");

    // Run the turn via the raw `session send --json` command so we keep the
    // raw Output for the failure bundle.
    budget.check(&spec.name);
    let send_args = vec![
        "--json".into(),
        "session".into(),
        "send".into(),
        send.session_id().into(),
        "ping".into(),
    ];
    let send_output = driver
        .run(
            &world,
            &lattice_system_harness::LatticeCommand {
                args: send_args,
                timeout: Some(spec.step_timeout()),
            },
        )
        .expect("run session send");
    let outcome =
        lattice_system_harness::parse_send_stdout(&send_output.stdout, send_output.status.code());
    let secrets = [lattice_system_harness::FAKE_API_KEY];

    // Cross-validate via the public session log JSON.
    budget.check(&spec.name);
    let log_output = driver
        .run(
            &world,
            &lattice_system_harness::LatticeCommand {
                args: vec![
                    "--json".into(),
                    "session".into(),
                    "log".into(),
                    send.session_id().into(),
                ],
                timeout: Some(Duration::from_secs(30)),
            },
        )
        .expect("session log --json");
    let status_output = driver
        .run(
            &world,
            &lattice_system_harness::LatticeCommand {
                args: vec![
                    "--json".into(),
                    "session".into(),
                    "status".into(),
                    send.session_id().into(),
                ],
                timeout: Some(Duration::from_secs(30)),
            },
        )
        .expect("session status --json");

    // Required event subsequence (RFC #19 decision five: no full golden).
    let required = [
        "turn_started",
        "model_started",
        "token_delta",
        "model_finished",
        "turn_committed",
    ];
    let subseq = assert_event_subsequence(&outcome, &required);

    let binary_exit = outcome
        .exit_code
        .expect("outcome should have an exit code classification");
    let derived = derive_send_exit_code(&outcome.events);
    let exit_ok = binary_exit == SendExitCode::Committed && derived == SendExitCode::Committed;

    let log_stdout = String::from_utf8_lossy(&log_output.stdout).to_string();
    let log_ok = log_output.status.success()
        && log_stdout.contains("turn_committed")
        && log_stdout.contains("commit_id");
    let status_ok = status_output.status.success();

    let caps = server.captured_requests();
    let fake_llm_ok = caps
        .iter()
        .any(|c| c.model == "msb-plain" && c.message_roles.iter().any(|r| r == "user"));

    // Combine every semantic check into one Result.
    use AssertError::*;
    let result: Result<(), AssertError> = (|| {
        subseq.map_err(|e| StdoutMissing { expected: e })?;
        if !exit_ok {
            return Err(StdoutMissing {
                expected: format!("exit code mismatch: binary={binary_exit:?} derived={derived:?}"),
            });
        }
        if !log_ok {
            return Err(StdoutMissing {
                expected: "session log --json did not reflect the new commit".into(),
            });
        }
        if !status_ok {
            return Err(StdoutMissing {
                expected: "session status --json did not exit 0".into(),
            });
        }
        if !fake_llm_ok {
            return Err(StdoutMissing {
                expected: "fake LLM did not receive a request with model=msb-plain and a user role"
                    .into(),
            });
        }
        Ok(())
    })();

    if let Err(err) = &result {
        // Write the full bundle: stdout/stderr/command/world + fake-LLM log
        // + workspace before/after diff + cross-validation outputs, then
        // surface the assertion error. RFC #19 decision eight.
        write_full_bundle(
            &world,
            &spec.name,
            &send_output,
            &log_output,
            &status_output,
            &caps,
            &before,
            &secrets,
        );
        panic!("msb plain-text turn failed: {err}");
    }
}

// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Fast binary black-box: session command surface (issue #18 "fast binary
//! black-box" matrix; triage replacement target `planned_session_cli`).
//!
//! Drives the built `lattice` binary for the no-turn session surface:
//! `session new`, `list`, `status`, `log`, and `branch`. Creating a session
//! only writes the session graph record (it never resolves the Upper
//! workspace config), so these scenarios need no Microsandbox. The
//! turn-bearing behaviours (`session log` with a committed turn, branch
//! creation after a turn) are deferred to the MSB lane - see
//! `docs/TEST-TRIAGE.md` and `FINDINGS.md`.
//!
//! These scenarios supersede the crate-internal `session_*` and
//! `smoke_session_*` unit tests in `lattice-cli` whose behaviour is fully
//! covered here (see `docs/TEST-TRIAGE.md`).

use std::process::Output;

use lattice_system_harness::{
    AssertError, LatticeCommand, LatticeDriver, ScenarioSpec, ScenarioWorld, assert_exit_failure,
    assert_exit_success, assert_stderr_contains, assert_stdout_contains,
    assert_stdout_contains_all, assert_with_failure_artifacts,
};
use serde_json::Value;

fn driver() -> LatticeDriver {
    LatticeDriver::from_env().expect("LATTICE_BIN must point to a built lattice binary")
}

/// An isolated project that already contains a discoverable `plain` agent
/// profile and a born `main` HEAD, ready for `session new`.
fn project_with_agent(prefix: &str) -> (ScenarioWorld, ScenarioSpec) {
    let spec = ScenarioSpec::fast(prefix);
    let world = ScenarioWorld::new(&spec.name).expect("world");
    world
        .write_agent_profile("plain", "sonnet", "dummy/not-real:latest")
        .expect("agent profile");
    world.init_git_project().expect("git project");
    (world, spec)
}

fn run(world: &ScenarioWorld, spec: &ScenarioSpec, args: &[&str]) -> Output {
    driver()
        .run(
            world,
            &LatticeCommand {
                args: args.iter().map(|s| s.to_string()).collect(),
                timeout: Some(spec.step_timeout()),
            },
        )
        .expect("drive lattice")
}

fn ok_or_artifacts(
    world: &ScenarioWorld,
    name: &str,
    summary: &str,
    out: &Output,
    result: Result<(), AssertError>,
) {
    assert_with_failure_artifacts(world, name, summary, out, &[], result)
        .unwrap_or_else(|e| panic!("{e}"));
}

#[test]
fn fast_session_list_empty_reports_no_sessions() {
    let spec = ScenarioSpec::fast("fast-session-list-empty");
    let world = ScenarioWorld::new(&spec.name).expect("world");
    let out = run(&world, &spec, &["session", "list"]);

    let result =
        assert_exit_success(&out).and_then(|_| assert_stdout_contains(&out, "No sessions found"));
    ok_or_artifacts(
        &world,
        "fast-session-list-empty",
        "lattice session list",
        &out,
        result,
    );
}

#[test]
fn fast_session_list_empty_json_is_empty_sessions_array() {
    let spec = ScenarioSpec::fast("fast-session-list-empty-json");
    let world = ScenarioWorld::new(&spec.name).expect("world");
    let out = run(&world, &spec, &["session", "list", "--json"]);

    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: Value =
        serde_json::from_str(stdout.trim()).expect("session list --json is a JSON object");
    let result = assert_exit_success(&out).and_then(|_| {
        if parsed
            .get("sessions")
            .and_then(|v| v.as_array())
            .map(|a| a.is_empty())
            != Some(true)
        {
            return Err(AssertError::StdoutMissing {
                expected: "{\"sessions\":[]}".into(),
            });
        }
        Ok(())
    });
    ok_or_artifacts(
        &world,
        "fast-session-list-empty-json",
        "lattice session list --json",
        &out,
        result,
    );
}

#[test]
fn fast_session_new_creates_session_and_appears_in_list() {
    let (world, spec) = project_with_agent("fast-session-new");
    let out = run(
        &world,
        &spec,
        &[
            "--json",
            "session",
            "new",
            "--agent",
            "plain",
            "--project-branch",
            "main",
        ],
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: Value =
        serde_json::from_str(stdout.trim()).expect("session new --json is a JSON object");
    let session_id = parsed
        .get("session_id")
        .and_then(|v| v.as_str())
        .expect("session_id present");
    let result = assert_exit_success(&out).and_then(|_| {
        if parsed.get("active_agent").and_then(|v| v.as_str()) != Some("plain")
            || parsed.get("project_branch").and_then(|v| v.as_str()) != Some("main")
        {
            return Err(AssertError::StdoutMissing {
                expected: "active_agent=plain, project_branch=main".into(),
            });
        }
        Ok(())
    });
    ok_or_artifacts(
        &world,
        "fast-session-new",
        "lattice --json session new",
        &out,
        result,
    );

    // The new session must be visible through the public list surface.
    let list = run(&world, &spec, &["session", "list", "--json"]);
    let list_result =
        assert_exit_success(&list).and_then(|_| assert_stdout_contains(&list, session_id));
    ok_or_artifacts(
        &world,
        "fast-session-new",
        "lattice session list --json (after new)",
        &list,
        list_result,
    );
}

#[test]
fn fast_session_status_new_session_shows_zero_commits_and_agent() {
    let (world, spec) = project_with_agent("fast-session-status");
    let new = run(
        &world,
        &spec,
        &[
            "--json",
            "session",
            "new",
            "--agent",
            "plain",
            "--project-branch",
            "main",
        ],
    );
    let session_id = serde_json::from_str::<Value>(String::from_utf8_lossy(&new.stdout).trim())
        .expect("session new --json")
        .get("session_id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .expect("session_id");

    let out = run(&world, &spec, &["session", "status", &session_id]);
    let result = assert_exit_success(&out)
        .and_then(|_| assert_stdout_contains_all(&out, &["plain", "Commits:", "0", "main"]));
    ok_or_artifacts(
        &world,
        "fast-session-status",
        "lattice session status <id>",
        &out,
        result,
    );
}

#[test]
fn fast_session_log_new_session_reports_no_turns() {
    let (world, spec) = project_with_agent("fast-session-log-empty");
    let new = run(
        &world,
        &spec,
        &[
            "--json",
            "session",
            "new",
            "--agent",
            "plain",
            "--project-branch",
            "main",
        ],
    );
    let session_id = serde_json::from_str::<Value>(String::from_utf8_lossy(&new.stdout).trim())
        .expect("session new --json")
        .get("session_id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .expect("session_id");

    let out = run(&world, &spec, &["session", "log", &session_id]);
    let result =
        assert_exit_success(&out).and_then(|_| assert_stdout_contains(&out, "No turns found"));
    ok_or_artifacts(
        &world,
        "fast-session-log-empty",
        "lattice session log <id>",
        &out,
        result,
    );
}

#[test]
fn fast_session_branch_new_session_reports_no_branches() {
    let (world, spec) = project_with_agent("fast-session-branch-empty");
    let new = run(
        &world,
        &spec,
        &[
            "--json",
            "session",
            "new",
            "--agent",
            "plain",
            "--project-branch",
            "main",
        ],
    );
    let session_id = serde_json::from_str::<Value>(String::from_utf8_lossy(&new.stdout).trim())
        .expect("session new --json")
        .get("session_id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .expect("session_id");

    let out = run(&world, &spec, &["session", "branch", &session_id]);
    let result =
        assert_exit_success(&out).and_then(|_| assert_stdout_contains(&out, "No branches found"));
    ok_or_artifacts(
        &world,
        "fast-session-branch-empty",
        "lattice session branch <id>",
        &out,
        result,
    );
}

#[test]
fn fast_session_status_nonexistent_session_errors() {
    let spec = ScenarioSpec::fast("fast-session-status-missing");
    let world = ScenarioWorld::new(&spec.name).expect("world");
    let out = run(&world, &spec, &["session", "status", "nope-sid"]);

    let result = assert_exit_failure(&out)
        .and_then(|_| assert_stderr_contains(&out, "not found"))
        .and_then(|_| assert_stderr_contains(&out, "nope-sid"));
    ok_or_artifacts(
        &world,
        "fast-session-status-missing",
        "lattice session status nope-sid",
        &out,
        result,
    );
}

#[test]
fn fast_session_log_nonexistent_session_errors() {
    let spec = ScenarioSpec::fast("fast-session-log-missing");
    let world = ScenarioWorld::new(&spec.name).expect("world");
    let out = run(&world, &spec, &["session", "log", "nope-sid"]);

    let result = assert_exit_failure(&out)
        .and_then(|_| assert_stderr_contains(&out, "not found"))
        .and_then(|_| assert_stderr_contains(&out, "nope-sid"));
    ok_or_artifacts(
        &world,
        "fast-session-log-missing",
        "lattice session log nope-sid",
        &out,
        result,
    );
}

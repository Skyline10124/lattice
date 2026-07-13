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
    AssertError, LatticeCommand, LatticeDriver, ScenarioSpec, ScenarioWorld, assert_exit_code,
    assert_exit_failure, assert_exit_success, assert_no_secret, assert_stderr_contains,
    assert_stdout_contains, assert_stdout_contains_all, assert_with_failure_artifacts,
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

/// Create an empty session through the public `--json session new` surface and
/// return its id. Used by every no-turn scenario that needs a real session id
/// without seeding any turn state. Never touches the product crate internals.
fn create_session(world: &ScenarioWorld, spec: &ScenarioSpec) -> String {
    let out = run(
        world,
        spec,
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
    assert_session_new_contract(world, &spec.name, &out).unwrap_or_else(|e| panic!("{e}"))
}

/// Assert the exit-0 + stable `session new --json` fields shared by every
/// no-turn scenario (active_agent=plain, project_branch=main) and return the
/// `session_id` from the single JSON parse. Centralising it here avoids
/// re-inlining the parse in each caller. JSON parse failures are routed
/// through the failure-artifact path (not a bare `expect`) so the redacted
/// stdout/stderr bundle survives for diagnosis. The `scenario_name` (typically
/// `spec.name`) is used as the failure-artifact directory name so parallel
/// callers do not overwrite each other's artifacts on a session-new regression.
fn assert_session_new_contract(
    world: &ScenarioWorld,
    scenario_name: &str,
    out: &Output,
) -> Result<String, AssertError> {
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: Value = match serde_json::from_str(stdout.trim()) {
        Ok(v) => v,
        Err(e) => {
            assert_with_failure_artifacts(
                world,
                scenario_name,
                "lattice --json session new",
                out,
                &[],
                Err(AssertError::StdoutMissing {
                    expected: format!("valid JSON object (parse error: {e})"),
                }),
            )?;
            unreachable!()
        }
    };
    let session_id = parsed
        .get("session_id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .filter(|s| !s.is_empty());
    let result = assert_exit_success(out).and_then(|_| {
        if parsed.get("active_agent").and_then(|v| v.as_str()) != Some("plain")
            || parsed.get("project_branch").and_then(|v| v.as_str()) != Some("main")
        {
            return Err(AssertError::StdoutMissing {
                expected: "active_agent=plain, project_branch=main".into(),
            });
        }
        if session_id.is_none() {
            return Err(AssertError::StdoutMissing {
                expected: "session_id non-empty string".into(),
            });
        }
        Ok(())
    });
    assert_with_failure_artifacts(
        world,
        scenario_name,
        "lattice --json session new",
        out,
        &[],
        result,
    )?;
    Ok(session_id.expect("checked above"))
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

/// Parse `--json` stdout as a JSON value. On parse failure, record the
/// failure artifact (redacted stdout/stderr + world context) under
/// `scenario_name` and return an `AssertError` instead of panicking, so
/// tests that assert JSON shape catch regressions through the artifact path
/// rather than a bare `expect` that destroys diagnostics.
fn parse_json_or_artifacts(
    world: &ScenarioWorld,
    scenario_name: &str,
    summary: &str,
    out: &Output,
) -> Result<Value, AssertError> {
    let stdout = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str(stdout.trim()).or_else(|e| {
        assert_with_failure_artifacts(
            world,
            scenario_name,
            summary,
            out,
            &[],
            Err(AssertError::StdoutMissing {
                expected: format!("valid JSON object (parse error: {e})"),
            }),
        )?;
        unreachable!()
    })
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

    let parsed: Value = parse_json_or_artifacts(
        &world,
        "fast-session-list-empty-json",
        "lattice session list --json",
        &out,
    )
    .unwrap_or_else(|e| panic!("{e}"));
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
    let session_id = create_session(&world, &spec);

    // The new session must be visible through the public list surface.
    let list = run(&world, &spec, &["session", "list", "--json"]);
    let list_result =
        assert_exit_success(&list).and_then(|_| assert_stdout_contains(&list, &session_id));
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
    let session_id = create_session(&world, &spec);

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
fn fast_session_status_json_stable_contract() {
    // Public JSON contract for a fresh session: object shape plus the stable
    // no-turn fields. Deleting or renaming any asserted field here must fail
    // this black-box scenario (issue #18). Mirrors the renderer at
    // `crates/lattice-cli/src/commands/session.rs:435-448`.
    let (world, spec) = project_with_agent("fast-session-status-json");
    let session_id = create_session(&world, &spec);

    let out = run(&world, &spec, &["--json", "session", "status", &session_id]);
    let parsed: Value = parse_json_or_artifacts(
        &world,
        "fast-session-status-json",
        "lattice --json session status <id>",
        &out,
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let result = assert_exit_success(&out).and_then(|_| {
        if parsed.get("session_id").and_then(|v| v.as_str()) != Some(&session_id) {
            return Err(AssertError::StdoutMissing {
                expected: format!("session_id == {session_id}"),
            });
        }
        if parsed.get("active_agent").and_then(|v| v.as_str()) != Some("plain") {
            return Err(AssertError::StdoutMissing {
                expected: "active_agent == plain".into(),
            });
        }
        if parsed.get("commit_count").and_then(|v| v.as_i64()) != Some(0) {
            return Err(AssertError::StdoutMissing {
                expected: "commit_count == 0".into(),
            });
        }
        if parsed.get("head").is_none() {
            return Err(AssertError::StdoutMissing {
                expected: "head present".into(),
            });
        }
        if parsed.get("branches").and_then(|v| v.as_array()).is_none() {
            return Err(AssertError::StdoutMissing {
                expected: "branches is an array".into(),
            });
        }
        let target = parsed.get("project_target_branch").and_then(|v| v.as_str());
        if target != Some("main") {
            return Err(AssertError::StdoutMissing {
                expected: format!("project_target_branch == main, got {target:?}"),
            });
        }
        Ok(())
    });
    ok_or_artifacts(
        &world,
        "fast-session-status-json",
        "lattice --json session status <id>",
        &out,
        result,
    );
}

#[test]
fn fast_session_log_new_session_reports_no_turns() {
    let (world, spec) = project_with_agent("fast-session-log-empty");
    let session_id = create_session(&world, &spec);

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
fn fast_session_log_json_empty_turns_stable_contract() {
    // Public JSON contract for `session log` on a turn-less session: object
    // with `session_id` and an empty `turns` array (renderer at
    // `crates/lattice-cli/src/commands/session.rs:537-545`).
    let (world, spec) = project_with_agent("fast-session-log-json");
    let session_id = create_session(&world, &spec);

    let out = run(&world, &spec, &["--json", "session", "log", &session_id]);
    let parsed: Value = parse_json_or_artifacts(
        &world,
        "fast-session-log-json",
        "lattice --json session log <id>",
        &out,
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let result = assert_exit_success(&out).and_then(|_| {
        if parsed.get("session_id").and_then(|v| v.as_str()) != Some(&session_id) {
            return Err(AssertError::StdoutMissing {
                expected: format!("session_id == {session_id}"),
            });
        }
        let turns = parsed.get("turns").and_then(|v| v.as_array());
        match turns {
            Some(a) if a.is_empty() => Ok(()),
            other => Err(AssertError::StdoutMissing {
                expected: format!("turns == [], got {other:?}"),
            }),
        }
    });
    ok_or_artifacts(
        &world,
        "fast-session-log-json",
        "lattice --json session log <id>",
        &out,
        result,
    );
}

#[test]
fn fast_session_branch_new_session_reports_no_branches() {
    let (world, spec) = project_with_agent("fast-session-branch-empty");
    let session_id = create_session(&world, &spec);

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
fn fast_session_branch_json_empty_branches_stable_contract() {
    // Public JSON contract for `session branch` (list mode) on a turn-less
    // session: object with `session_id` and an empty `branches` array
    // (renderer at `crates/lattice-cli/src/commands/session.rs:650-659`).
    // `session branch` with no name lists only `SessionStatus.branches`, which
    // a fresh session has none of.
    let (world, spec) = project_with_agent("fast-session-branch-json");
    let session_id = create_session(&world, &spec);

    let out = run(&world, &spec, &["--json", "session", "branch", &session_id]);
    let parsed: Value = parse_json_or_artifacts(
        &world,
        "fast-session-branch-json",
        "lattice --json session branch <id>",
        &out,
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let result = assert_exit_success(&out).and_then(|_| {
        if parsed.get("session_id").and_then(|v| v.as_str()) != Some(&session_id) {
            return Err(AssertError::StdoutMissing {
                expected: format!("session_id == {session_id}"),
            });
        }
        let branches = parsed.get("branches").and_then(|v| v.as_array());
        match branches {
            Some(a) if a.is_empty() => Ok(()),
            other => Err(AssertError::StdoutMissing {
                expected: format!("branches == [], got {other:?}"),
            }),
        }
    });
    ok_or_artifacts(
        &world,
        "fast-session-branch-json",
        "lattice --json session branch <id>",
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

#[test]
fn fast_session_send_nonexistent_session_errors() {
    // `session send <missing-id> <prompt>` must fail at session lookup (exit 2,
    // stderr naming the missing id) before any workspace/Upper setup. This is
    // the fast-binary guard for the runtime-nesting path: `cmd_send_api`
    // calls `api.open_session` first and returns `ExitCode::from(2)` on
    // `NotFound` before any profile loading or workspace setup
    // (see `crates/lattice-cli/src/commands/session.rs:406-419`). Supersedes
    // the crate-internal `run_integration_missing_session_returns_not_found`.
    // The exit-2 contract is pinned here: `assert_exit_code` distinguishes
    // this lookup-error path from a runtime error (exit 1) that would
    // silently pass `assert_exit_failure`.
    let (world, spec) = project_with_agent("fast-session-send-missing");
    let out = run(&world, &spec, &["session", "send", "nope-sid", "hello"]);

    let result = assert_exit_code(&out, 2)
        .and_then(|_| assert_stderr_contains(&out, "not found"))
        .and_then(|_| assert_stderr_contains(&out, "nope-sid"));
    ok_or_artifacts(
        &world,
        "fast-session-send-missing",
        "lattice session send nope-sid hello",
        &out,
        result,
    );
}

#[test]
fn fast_session_resume_without_session_id_fails() {
    // `session resume` with no session_id argument is rejected at clap parse
    // time (the `session_id` field is a required positional `String`). Clap
    // exits with code 2 on argument errors. Supersedes the crate-internal
    // `smoke_session_resume_without_session_id_fails`.
    let (world, spec) = project_with_agent("fast-session-resume-no-id");
    let out = run(&world, &spec, &["session", "resume"]);

    let result = assert_exit_code(&out, 2).and_then(|_| assert_stderr_contains(&out, "SESSION_ID"));
    ok_or_artifacts(
        &world,
        "fast-session-resume-no-id",
        "lattice session resume",
        &out,
        result,
    );
}

#[test]
fn fast_session_resume_nonexistent_session_errors() {
    // `session resume <missing-id>` fails at session lookup before any
    // workspace/Upper setup. Missing sessions are invocation errors and use
    // the public exit-code-2 contract. Supersedes the crate-internal
    // `smoke_session_resume_nonexistent_session_returns_error`.
    let (world, spec) = project_with_agent("fast-session-resume-missing");
    let out = run(&world, &spec, &["session", "resume", "nope-sid"]);

    let result = assert_exit_code(&out, 2)
        .and_then(|_| assert_stderr_contains(&out, "not found"))
        .and_then(|_| assert_stderr_contains(&out, "nope-sid"));
    ok_or_artifacts(
        &world,
        "fast-session-resume-missing",
        "lattice session resume nope-sid",
        &out,
        result,
    );
}

#[test]
fn fast_session_send_no_agent_profile_errors() {
    // `session send <id> hello` when the session's agent profile has been
    // removed from the project fails at `load_agent_checked` (exit 2, stderr
    // mentioning agent.toml/profile/not found) before any workspace/Upper
    // setup. The failure path is in `cmd_send_api`
    // (`crates/lattice-cli/src/commands/session.rs:423-428`). Supersedes the
    // crate-internal `smoke_session_send_no_profile_fails_with_clear_message`.
    // The agent profile is removed from the working tree after `session new`
    // so the persisted session still references `plain` but the lookup at
    // send time can no longer resolve it.
    let (mut world, spec) = project_with_agent("fast-session-send-no-profile");
    let session_id = create_session(&world, &spec);

    std::fs::remove_dir_all(world.project_dir().join(".lattice/agents/plain"))
        .expect("remove agent profile dir");

    world.set_env("ANTHROPIC_API_KEY", "sk-fast-send-no-profile-sentinel");
    let out = run(&world, &spec, &["session", "send", &session_id, "hello"]);

    let result = assert_exit_code(&out, 2)
        .and_then(|_| {
            assert_stderr_contains(&out, "agent.toml")
                .or_else(|_| assert_stderr_contains(&out, "profile"))
                .or_else(|_| assert_stderr_contains(&out, "not found"))
        })
        .and_then(|_| assert_no_secret(&out, "sk-fast-send-no-profile-sentinel"));
    assert_with_failure_artifacts(
        &world,
        "fast-session-send-no-profile",
        "lattice session send <id> hello (no profile)",
        &out,
        &["sk-fast-send-no-profile-sentinel"],
        result,
    )
    .unwrap_or_else(|e| panic!("{e}"));
}

#[test]
fn fast_session_send_missing_credential_is_redacted_input_error() {
    // Credential resolution happens before `prepare_upper`, so this is a
    // fast-binary input/config failure and must not wait for KVM. The public
    // contract is exit 2, the missing env-var name is actionable, and neither
    // output stream may expose an unrelated secret present in the environment.
    const MISSING_CREDENTIAL: &str = "LATTICE_FAST_MISSING_CREDENTIAL";
    const SECRET_SENTINEL: &str = "sk-fast-missing-credential-sentinel";

    let (mut world, spec) = project_with_agent("fast-session-send-missing-credential");
    world
        .write_project_file(
            ".lattice/agents/plain/agent.toml",
            &format!(
                "schema_version = 1\n\n[agent]\nname = \"plain\"\nmodel = \"sonnet\"\n\n[prompt]\nsystem = \"harness agent\"\n\n[workspace]\nimage = \"dummy/not-real:latest\"\n\n[[workspace.credentials]]\nenv_var_name = \"{MISSING_CREDENTIAL}\"\n\n[tools]\nenabled = []\n"
            ),
        )
        .expect("credential-requiring agent profile");
    let session_id = create_session(&world, &spec);

    world.set_env("UNRELATED_SECRET", SECRET_SENTINEL);
    let out = run(&world, &spec, &["session", "send", &session_id, "hello"]);

    let result = assert_exit_code(&out, 2)
        .and_then(|_| assert_stderr_contains(&out, MISSING_CREDENTIAL))
        .and_then(|_| assert_no_secret(&out, SECRET_SENTINEL));
    assert_with_failure_artifacts(
        &world,
        "fast-session-send-missing-credential",
        "lattice session send <id> hello (missing credential)",
        &out,
        &[SECRET_SENTINEL],
        result,
    )
    .unwrap_or_else(|e| panic!("{e}"));
}

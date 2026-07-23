// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Direct-Local public session graph isolation (issue #24 slice 7).
//!
//! Two sessions created through `session new`, each seeded with two real
//! completed Turns, then branch/checkout/reset/diff operations scoped to the
//! addressed session only. No `sessions.git` or sidecar inspection — every
//! assertion goes through public `--json` commands.
//!
//! ponytail: helpers duplicate other Direct-Local tests; later-slice split
//! owns the dedupe.

#![cfg(unix)]

use std::process::Output;

use lattice_system_harness::{
    AssertError, CommandOutcome, FAKE_API_KEY, LatticeDriver, ScenarioEvidence, ScenarioSpec,
    ScenarioWorld, SessionSendDriver, SseScript, failure_dir,
};
use serde_json::Value;

fn driver() -> LatticeDriver {
    LatticeDriver::from_env().expect("LATTICE_BIN must point to a built lattice binary")
}

fn require(condition: bool, expected: impl Into<String>) -> Result<(), AssertError> {
    if condition {
        Ok(())
    } else {
        Err(AssertError::StdoutMissing {
            expected: expected.into(),
        })
    }
}

fn evidence(world: &ScenarioWorld, spec: &ScenarioSpec) -> ScenarioEvidence {
    ScenarioEvidence::new(world, failure_dir(&spec.name), [FAKE_API_KEY.to_string()])
        .expect("scenario evidence")
}

fn finish_scenario(
    mut evidence: ScenarioEvidence,
    commands: &[(&str, &CommandOutcome)],
    result: Result<(), AssertError>,
) {
    for (summary, outcome) in commands {
        evidence.record_command(*summary, outcome);
    }
    evidence
        .finish(result.is_err())
        .unwrap_or_else(|error| panic!("scenario evidence failed: {error}"));
    result.unwrap_or_else(|error| panic!("{error}"));
}

fn wire_fake_llm(
    world: &ScenarioWorld,
    agent: &str,
    script: SseScript,
) -> lattice_system_harness::FakeLlmServer {
    let server = lattice_system_harness::FakeLlmServer::start(script).expect("start fake llm");
    world.set_env("OPENAI_COMPATIBLE_BASE_URL", server.base_url());
    world.set_env("OPENAI_COMPATIBLE_MODEL", agent);
    world.set_env(
        "OPENAI_COMPATIBLE_API_KEY",
        lattice_system_harness::FAKE_API_KEY,
    );
    server
}

fn isolation_world(prefix: &str, agent: &str) -> (ScenarioWorld, ScenarioSpec) {
    let spec = ScenarioSpec::direct_local(prefix);
    let world = ScenarioWorld::new(&spec.name).expect("world");
    let toml = format!(
        "schema_version = 1\n\n\
         [agent]\nname = \"{agent}\"\nmodel = \"compat:{agent}\"\n\n\
         [prompt]\nsystem = \"You are a concise test agent.\"\n\n\
         [tools]\nenabled = []\n"
    );
    world
        .write_project_file(&format!(".lattice/agents/{agent}/agent.toml"), &toml)
        .expect("write agent.toml");
    world.init_git_project().expect("init git");
    (world, spec)
}

fn cli(world: &ScenarioWorld, spec: &ScenarioSpec, args: &[&str]) -> Output {
    driver()
        .run(
            world,
            &lattice_system_harness::LatticeCommand {
                args: args.iter().map(|s| s.to_string()).collect(),
                timeout: Some(spec.step_timeout()),
            },
        )
        .expect("drive lattice")
        .output
}

fn cli_json(world: &ScenarioWorld, spec: &ScenarioSpec, args: &[&str]) -> Value {
    let out = cli(world, spec, args);
    let s = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str(s.trim()).unwrap_or_else(|e| {
        panic!(
            "expected JSON from {args:?}, got {:?}: {e}",
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

/// Extract commit_id values from a session log JSON array.
fn log_commit_ids(log: &Value) -> Vec<String> {
    log.get("turns")
        .and_then(Value::as_array)
        .map(|turns| {
            turns
                .iter()
                .filter_map(|t| {
                    t.get("commit_id")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Create two sessions A and B, seed each with two real completed turns, and
/// return their session ids and per-session commit ids.
fn setup_two_sessions<'a>(
    driver: &'a LatticeDriver,
    world: &'a ScenarioWorld,
    spec: &'a ScenarioSpec,
) -> (
    SessionSendDriver<'a>,
    SessionSendDriver<'a>,
    Vec<String>,
    Vec<String>,
) {
    let send_a = SessionSendDriver::new(driver, world, spec, "dl-iso", "main").expect("session A");
    let _t1 = send_a.send("turn a1").expect("a turn 1");
    let _t2 = send_a.send("turn a2").expect("a turn 2");
    let log_a = cli_json(
        world,
        spec,
        &["--json", "session", "log", send_a.session_id()],
    );
    let commits_a = log_commit_ids(&log_a);

    let send_b = SessionSendDriver::new(driver, world, spec, "dl-iso", "main").expect("session B");
    let _t1 = send_b.send("turn b1").expect("b turn 1");
    let _t2 = send_b.send("turn b2").expect("b turn 2");
    let log_b = cli_json(
        world,
        spec,
        &["--json", "session", "log", send_b.session_id()],
    );
    let commits_b = log_commit_ids(&log_b);

    (send_a, send_b, commits_a, commits_b)
}

/// Branch listing is scoped: create `feature` in both sessions at different
/// commits; listing A reports only A's pointer and changes neither.
#[test]
fn direct_session_branch_listing_isolation() {
    let driver = driver();
    let (world, spec) = isolation_world("direct-iso-branch-list", "dl-iso");
    let _server = wire_fake_llm(&world, "dl-iso", SseScript::plain("seed"));
    let scenario_evidence = evidence(&world, &spec);
    let (send_a, send_b, commits_a, commits_b) = setup_two_sessions(&driver, &world, &spec);
    let sid_a = send_a.session_id().to_string();
    let sid_b = send_b.session_id().to_string();

    let b_before = cli_json(&world, &spec, &["--json", "session", "status", &sid_b]);

    // Create feature in A from commits_a[0]; create feature in B from commits_b[1].
    let _ = cli(
        &world,
        &spec,
        &[
            "--json",
            "session",
            "branch",
            &sid_a,
            "feature",
            "--from",
            &commits_a[0],
        ],
    );
    let _ = cli(
        &world,
        &spec,
        &[
            "--json",
            "session",
            "branch",
            &sid_b,
            "feature",
            "--from",
            &commits_b[1],
        ],
    );

    // List A's branches: feature must point to commits_a[0].
    let list_a = cli_json(&world, &spec, &["--json", "session", "branch", &sid_a]);
    let feature_a = list_a
        .get("branches")
        .and_then(Value::as_array)
        .and_then(|bs| {
            bs.iter()
                .find(|b| b.get("name").and_then(Value::as_str) == Some("feature"))
        })
        .and_then(|b| {
            b.get("commit_id")
                .and_then(Value::as_str)
                .map(str::to_string)
        });
    require(
        feature_a.as_deref() == Some(commits_a[0].as_str()),
        format!("A feature must point to A's commit: {feature_a:?}"),
    )
    .expect("A feature pointer");

    // List B's branches: feature must still point to commits_b[1] (unchanged).
    let list_b = cli_json(&world, &spec, &["--json", "session", "branch", &sid_b]);
    let feature_b = list_b
        .get("branches")
        .and_then(Value::as_array)
        .and_then(|bs| {
            bs.iter()
                .find(|b| b.get("name").and_then(Value::as_str) == Some("feature"))
        })
        .and_then(|b| {
            b.get("commit_id")
                .and_then(Value::as_str)
                .map(str::to_string)
        });
    require(
        feature_b.as_deref() == Some(commits_b[1].as_str()),
        format!("B feature must be unchanged: {feature_b:?}"),
    )
    .expect("B feature pointer");

    // B's status is unchanged overall.
    let b_after = cli_json(&world, &spec, &["--json", "session", "status", &sid_b]);
    require(
        b_after.get("session_id") == b_before.get("session_id")
            && b_after.get("current_commit_id") == b_before.get("current_commit_id"),
        "B session must be unchanged after A's branch operations",
    )
    .expect("B unchanged");

    finish_scenario(
        scenario_evidence,
        &[(
            "branch listing isolation",
            &send_a.send("seed").unwrap().command,
        )],
        Ok(()),
    );
}

/// Checkout is scoped: checkout A `feature`; A HEAD changes, B stays on `main`
/// with the same commit.
#[test]
fn direct_session_checkout_isolation() {
    let driver = driver();
    let (world, spec) = isolation_world("direct-iso-checkout", "dl-iso");
    let _server = wire_fake_llm(&world, "dl-iso", SseScript::plain("seed"));
    let scenario_evidence = evidence(&world, &spec);
    let (send_a, send_b, commits_a, _commits_b) = setup_two_sessions(&driver, &world, &spec);
    let sid_a = send_a.session_id().to_string();
    let sid_b = send_b.session_id().to_string();

    let b_before = cli_json(&world, &spec, &["--json", "session", "status", &sid_b]);

    // Create feature in A from commits_a[0], then checkout feature.
    let _ = cli(
        &world,
        &spec,
        &[
            "--json",
            "session",
            "branch",
            &sid_a,
            "feature",
            "--from",
            &commits_a[0],
        ],
    );
    let _ = cli(
        &world,
        &spec,
        &["--json", "session", "checkout", &sid_a, "feature"],
    );

    // A HEAD is now feature.
    let a_after = cli_json(&world, &spec, &["--json", "session", "status", &sid_a]);
    let head_name = a_after
        .get("head")
        .and_then(|h| h.get("branch"))
        .and_then(|b| b.get("name"))
        .and_then(Value::as_str);
    require(
        head_name == Some("feature"),
        format!("A HEAD must be feature after checkout: {a_after}"),
    )
    .expect("A HEAD feature");

    // B stays on main with the same commit.
    let b_after = cli_json(&world, &spec, &["--json", "session", "status", &sid_b]);
    require(
        b_after.get("session_id") == b_before.get("session_id")
            && b_after.get("current_commit_id") == b_before.get("current_commit_id"),
        "B session must be unchanged after A's checkout",
    )
    .expect("B unchanged");

    finish_scenario(
        scenario_evidence,
        &[("checkout isolation", &send_a.send("seed").unwrap().command)],
        Ok(()),
    );
}

/// Branch create is scoped: create A `work --from <A0>`; A has the exact
/// pointer, B has no `work`.
#[test]
fn direct_session_branch_create_isolation() {
    let driver = driver();
    let (world, spec) = isolation_world("direct-iso-branch-create", "dl-iso");
    let _server = wire_fake_llm(&world, "dl-iso", SseScript::plain("seed"));
    let scenario_evidence = evidence(&world, &spec);
    let (send_a, send_b, commits_a, _commits_b) = setup_two_sessions(&driver, &world, &spec);
    let sid_a = send_a.session_id().to_string();
    let sid_b = send_b.session_id().to_string();

    // Create work in A from commits_a[0].
    let _ = cli(
        &world,
        &spec,
        &[
            "--json",
            "session",
            "branch",
            &sid_a,
            "work",
            "--from",
            &commits_a[0],
        ],
    );

    // A has work pointing to commits_a[0].
    let list_a = cli_json(&world, &spec, &["--json", "session", "branch", &sid_a]);
    let work_a = list_a
        .get("branches")
        .and_then(Value::as_array)
        .and_then(|bs| {
            bs.iter()
                .find(|b| b.get("name").and_then(Value::as_str) == Some("work"))
        })
        .and_then(|b| {
            b.get("commit_id")
                .and_then(Value::as_str)
                .map(str::to_string)
        });
    require(
        work_a.as_deref() == Some(commits_a[0].as_str()),
        format!("A work must point to A's commit: {work_a:?}"),
    )
    .expect("A work pointer");

    // B has no work branch.
    let list_b = cli_json(&world, &spec, &["--json", "session", "branch", &sid_b]);
    let work_b = list_b
        .get("branches")
        .and_then(Value::as_array)
        .and_then(|bs| {
            bs.iter()
                .find(|b| b.get("name").and_then(Value::as_str) == Some("work"))
        });
    require(work_b.is_none(), "B must NOT have a work branch").expect("B no work");

    finish_scenario(
        scenario_evidence,
        &[(
            "branch create isolation",
            &send_a.send("seed").unwrap().command,
        )],
        Ok(()),
    );
}

/// Reset is scoped: reset A's current branch to A0; B's branch and current
/// commit are unchanged.
#[test]
fn direct_session_reset_isolation() {
    let driver = driver();
    let (world, spec) = isolation_world("direct-iso-reset", "dl-iso");
    let _server = wire_fake_llm(&world, "dl-iso", SseScript::plain("seed"));
    let scenario_evidence = evidence(&world, &spec);
    let (send_a, send_b, commits_a, _commits_b) = setup_two_sessions(&driver, &world, &spec);
    let sid_a = send_a.session_id().to_string();
    let sid_b = send_b.session_id().to_string();

    let b_before = cli_json(&world, &spec, &["--json", "session", "status", &sid_b]);

    // Reset A to commits_a[0].
    let _ = cli(
        &world,
        &spec,
        &["--json", "session", "reset", &sid_a, &commits_a[0]],
    );

    // A's current commit is now commits_a[0].
    let a_after = cli_json(&world, &spec, &["--json", "session", "status", &sid_a]);
    let a_commit = a_after.get("current_commit_id").and_then(Value::as_str);
    require(
        a_commit == Some(commits_a[0].as_str()),
        format!("A current commit must be reset target: {a_after}"),
    )
    .expect("A reset");

    // B is unchanged.
    let b_after = cli_json(&world, &spec, &["--json", "session", "status", &sid_b]);
    require(
        b_after.get("session_id") == b_before.get("session_id")
            && b_after.get("current_commit_id") == b_before.get("current_commit_id"),
        "B session must be unchanged after A's reset",
    )
    .expect("B unchanged");

    finish_scenario(
        scenario_evidence,
        &[("reset isolation", &send_a.send("seed").unwrap().command)],
        Ok(()),
    );
}

/// Diff resolves within the addressed session: identical `main`/`feature`
/// names resolve to different commits in A and B; A's diff reports only A's
/// transcript changes.
#[test]
fn direct_session_diff_isolation() {
    let driver = driver();
    let (world, spec) = isolation_world("direct-iso-diff", "dl-iso");
    let _server = wire_fake_llm(&world, "dl-iso", SseScript::plain("seed"));
    let scenario_evidence = evidence(&world, &spec);
    let (send_a, send_b, commits_a, commits_b) = setup_two_sessions(&driver, &world, &spec);
    let sid_a = send_a.session_id().to_string();
    let sid_b = send_b.session_id().to_string();

    // Create feature in both sessions at different commits.
    let _ = cli(
        &world,
        &spec,
        &[
            "--json",
            "session",
            "branch",
            &sid_a,
            "feature",
            "--from",
            &commits_a[0],
        ],
    );
    let _ = cli(
        &world,
        &spec,
        &[
            "--json",
            "session",
            "branch",
            &sid_b,
            "feature",
            "--from",
            &commits_b[1],
        ],
    );

    // Diff A: main vs feature resolves within A.
    let diff_a = cli_json(
        &world,
        &spec,
        &["--json", "session", "diff", &sid_a, "main", "feature"],
    );
    let from_commit = diff_a.get("from_commit_id").and_then(Value::as_str);
    let to_commit = diff_a.get("to_commit_id").and_then(Value::as_str);
    require(
        from_commit == Some(commits_a[commits_a.len() - 1].as_str()),
        format!("A diff from must be A main HEAD: {diff_a}"),
    )
    .expect("A diff from");
    require(
        to_commit == Some(commits_a[0].as_str()),
        format!("A diff to must be A feature commit: {diff_a}"),
    )
    .expect("A diff to");

    // Diff B: main vs feature resolves within B; to_commit differs from A's.
    let diff_b = cli_json(
        &world,
        &spec,
        &["--json", "session", "diff", &sid_b, "main", "feature"],
    );
    let b_to = diff_b.get("to_commit_id").and_then(Value::as_str);
    require(
        b_to == Some(commits_b[1].as_str()),
        format!("B diff to must be B feature commit: {diff_b}"),
    )
    .expect("B diff to");
    require(
        b_to != to_commit,
        "same branch name must resolve to different commits in different sessions",
    )
    .expect("diff isolation");

    finish_scenario(
        scenario_evidence,
        &[("diff isolation", &send_a.send("seed").unwrap().command)],
        Ok(()),
    );
}

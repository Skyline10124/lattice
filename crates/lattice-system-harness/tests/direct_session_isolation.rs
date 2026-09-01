// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Direct-Local public session graph isolation
//! (issue #46 (Codeberg #24) slice 7).
//!
//! Two sessions created through `session new`, each seeded with two real
//! completed Turns, then branch/checkout/reset/diff operations scoped to the
//! addressed session only. No `sessions.git` or sidecar inspection — every
//! assertion goes through public `--json` commands.

mod support;

use lattice_system_harness::{
    LatticeDriver, ScenarioSpec, ScenarioWorld, SessionSendDriver, SseScript,
};
use serde_json::Value;
use support::{cli, direct_world, driver, evidence, finish_scenario, require, wire_fake_llm};

fn cli_json(world: &ScenarioWorld, spec: &ScenarioSpec, args: &[&str]) -> Value {
    let out = cli(world, spec, args);
    let s = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str(s.trim()).unwrap_or_else(|e| {
        panic!(
            "expected JSON from {args:?}, outcome={}, stdout={:?}, stderr={:?}: {e}",
            out.summary(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
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
    let (world, spec) = direct_world("direct-iso-branch-list", "dl-iso", "[]", "");
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
        Vec::new(),
        Ok(()),
    );
}

/// Checkout is scoped: checkout A `feature`; A HEAD changes, B stays on `main`
/// with the same commit.
#[test]
fn direct_session_checkout_isolation() {
    let driver = driver();
    let (world, spec) = direct_world("direct-iso-checkout", "dl-iso", "[]", "");
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
        Vec::new(),
        Ok(()),
    );
}

/// Branch create is scoped: create A `work --from <A0>`; A has the exact
/// pointer, B has no `work`.
#[test]
fn direct_session_branch_create_isolation() {
    let driver = driver();
    let (world, spec) = direct_world("direct-iso-branch-create", "dl-iso", "[]", "");
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
        Vec::new(),
        Ok(()),
    );
}

/// Reset is scoped: reset A's current branch to A0; B's branch and current
/// commit are unchanged.
#[test]
fn direct_session_reset_isolation() {
    let driver = driver();
    let (world, spec) = direct_world("direct-iso-reset", "dl-iso", "[]", "");
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
        Vec::new(),
        Ok(()),
    );
}

/// Diff resolves within the addressed session: identical `main`/`feature`
/// names resolve to different commits in A and B; A's diff reports only A's
/// transcript changes.
#[test]
fn direct_session_diff_isolation() {
    let driver = driver();
    let (world, spec) = direct_world("direct-iso-diff", "dl-iso", "[]", "");
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
        Vec::new(),
        Ok(()),
    );
}

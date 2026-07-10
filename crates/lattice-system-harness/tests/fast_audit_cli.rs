// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Fast binary black-box: audit command error surface (issue #18 "fast
//! binary black-box" matrix; triage replacement target `planned_audit_cli`).
//!
//! Drives the built `lattice` binary for the no-turn audit surface. With no
//! agent turn having run in the project, the audit store does not exist, so
//! `audit query` / `replay` / `--tail` must degrade to a clean
//! configuration error (exit 1, message naming the missing store) rather than
//! panic. This is exactly the regression guard for the runtime-nesting bug
//! fixed in `lattice-cli/src/commands/audit.rs` (`audit::run` is now async and
//! reuses the top-level `#[tokio::main]` runtime instead of nesting one).
//!
//! The turn-bearing audit states (seeded store, replay of a real turn,
//! non-empty query results) are deferred to the MSB lane: seeding the audit
//! store requires a committed turn, which routes through Upper/Microsandbox.
//! See `docs/TEST-TRIAGE.md` and `FINDINGS.md`.
//!
//! These scenarios supersede the crate-internal
//! `audit_query_errors_when_store_missing` unit test in `lattice-cli`.

use std::process::Output;

use lattice_system_harness::{
    AssertError, LatticeCommand, LatticeDriver, ScenarioSpec, ScenarioWorld, assert_exit_failure,
    assert_stderr_contains, assert_with_failure_artifacts,
};

fn driver() -> LatticeDriver {
    LatticeDriver::from_env().expect("LATTICE_BIN must point to a built lattice binary")
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

/// Assert an audit subcommand degrades to the documented missing-store
/// configuration error (exit 1) instead of panicking. The audit store is
/// created lazily on the first agent turn; with no turn run it is absent.
fn assert_missing_store(world: &ScenarioWorld, name: &str, summary: &str, out: &Output) {
    let result = assert_exit_failure(out)
        .and_then(|_| assert_stderr_contains(out, "audit database not found"));
    ok_or_artifacts(world, name, summary, out, result);
}

#[test]
fn fast_audit_query_missing_store_errors_cleanly_json() {
    let spec = ScenarioSpec::fast("fast-audit-query-missing-json");
    let world = ScenarioWorld::new(&spec.name).expect("world");
    let out = run(
        &world,
        &spec,
        &["audit", "query", "--session", "nobody", "--json"],
    );
    assert_missing_store(
        &world,
        "fast-audit-query-missing-json",
        "lattice audit query --json",
        &out,
    );
}

#[test]
fn fast_audit_query_missing_store_errors_cleanly_text() {
    let spec = ScenarioSpec::fast("fast-audit-query-missing-text");
    let world = ScenarioWorld::new(&spec.name).expect("world");
    let out = run(&world, &spec, &["audit", "query", "--session", "nobody"]);
    assert_missing_store(
        &world,
        "fast-audit-query-missing-text",
        "lattice audit query",
        &out,
    );
}

#[test]
fn fast_audit_tail_missing_store_errors_cleanly() {
    let spec = ScenarioSpec::fast("fast-audit-tail-missing");
    let world = ScenarioWorld::new(&spec.name).expect("world");
    let out = run(
        &world,
        &spec,
        &["audit", "query", "--session", "nobody", "--tail", "--json"],
    );
    assert_missing_store(
        &world,
        "fast-audit-tail-missing",
        "lattice audit query --tail --json",
        &out,
    );
}

#[test]
fn fast_audit_replay_missing_store_errors_cleanly() {
    let spec = ScenarioSpec::fast("fast-audit-replay-missing");
    let world = ScenarioWorld::new(&spec.name).expect("world");
    let out = run(
        &world,
        &spec,
        &[
            "audit",
            "replay",
            "--session",
            "nobody",
            "--turn",
            "nope",
            "--json",
        ],
    );
    assert_missing_store(
        &world,
        "fast-audit-replay-missing",
        "lattice audit replay --json",
        &out,
    );
}

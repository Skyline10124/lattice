// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Fast binary black-box: `lattice lancet status` empty-state surface
//! (triage replacement target `planned_lancet_cli`).
//!
//! With no agent turn having run, the project-keyed audit store does not
//! exist; `lancet status` must report an empty state with exit 0 (not an
//! error, unlike `audit query` which is an explicit query against a missing
//! store). Supersedes the crate-internal
//! `cep_status_missing_db_returns_empty_state` unit test in `lattice-cli`.

use lattice_system_harness::cli::{ok_or_artifacts, run};
use lattice_system_harness::{
    AssertError, ScenarioSpec, ScenarioWorld, assert_exit_success, assert_stdout_contains,
};
use serde_json::Value;

#[test]
fn fast_lancet_status_missing_audit_store_reports_empty_state_json() {
    let spec = ScenarioSpec::fast("fast-lancet-status-empty-json");
    let world = ScenarioWorld::new(&spec.name).expect("world");
    let out = run(&world, &spec, &["lancet", "status", "--json"]);

    let stdout = String::from_utf8_lossy(&out.stdout);
    let result = assert_exit_success(&out).and_then(|_| {
        let parsed: Value =
            serde_json::from_str(stdout.trim()).map_err(|e| AssertError::StdoutMissing {
                expected: format!("valid JSON (parse error: {e})"),
            })?;
        let empty_calls = parsed
            .get("recent_calls")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty);
        let null_ledger = parsed.get("ledger_oid").is_some_and(Value::is_null);
        let null_verdict = parsed.get("last_verdict").is_some_and(Value::is_null);
        if empty_calls && null_ledger && null_verdict {
            Ok(())
        } else {
            Err(AssertError::StdoutMissing {
                expected: "ledger_oid=null, recent_calls=[], and last_verdict=null".into(),
            })
        }
    });
    ok_or_artifacts(
        &world,
        &spec.name,
        "lattice lancet status --json (no audit store)",
        &out,
        result,
    );
}

#[test]
fn fast_lancet_status_missing_audit_store_text_mode_smoke() {
    let spec = ScenarioSpec::fast("fast-lancet-status-empty-text");
    let world = ScenarioWorld::new(&spec.name).expect("world");
    let out = run(&world, &spec, &["lancet", "status"]);
    let result = assert_exit_success(&out)
        .and_then(|_| assert_stdout_contains(&out, "Recent Invocations:"))
        .and_then(|_| assert_stdout_contains(&out, "none"))
        .and_then(|_| assert_stdout_contains(&out, "Last Verdict"));
    ok_or_artifacts(
        &world,
        &spec.name,
        "lattice lancet status (no audit store, text)",
        &out,
        result,
    );
}

// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! The qualification summarizer lives next to `run-qualification.sh` so a
//! historical bundle can be inspected without a Btrfs campaign. `cargo test`
//! still has to prove the contract v2 gate semantics, so this test runs that
//! Python suite: manifest authority, version gating, the typical-cost /
//! scaling / safety / attribution estimators, evidence immutability, and the
//! generated gate table in the approval record.

use std::path::PathBuf;
use std::process::Command;

#[test]
fn qualification_contract_v2_estimators_and_fail_closed_rules_hold() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../benchmarks/invocation-store")
        .canonicalize()
        .expect("benchmarks/invocation-store");
    let status = Command::new("python3")
        .current_dir(&dir)
        .args([
            "-m",
            "unittest",
            "test_summarize_qualification.py",
            "test_qualification_isolation.py",
        ])
        .status()
        .expect("spawn python3 unittest");
    assert!(
        status.success(),
        "qualification summarizer contract failed (cwd {})",
        dir.display()
    );
}

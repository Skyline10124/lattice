// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Release-gate script wiring checks.
//!
//! Threat model: tampering with the checked-in release gate.

use std::process::Command;

// ─── release gate wiring ────────────────────────────────────────────────────

#[test]
fn release_smoke_release_gate_sh_is_retired() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let gate_path = manifest_dir.join("../../scripts/release-gate.sh");
    let output = Command::new(&gate_path)
        .output()
        .unwrap_or_else(|err| panic!("run {}: {err}", gate_path.display()));

    assert!(
        !output.status.success(),
        "retired release gate should exit nonzero"
    );
    assert!(
        output.stdout.is_empty(),
        "retired release gate should not write stdout, got:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("release-gate.sh is retired. Run scripts/release-gate-v1.4.sh (current)"),
        "retired release gate should point to the current v1.4 gate, got:\n{stderr}"
    );
}

#[test]
fn release_smoke_release_gate_v1_4_includes_lancet_boundary_check() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let gate_path = manifest_dir.join("../../scripts/release-gate-v1.4.sh");
    let gate = std::fs::read_to_string(&gate_path)
        .unwrap_or_else(|err| panic!("read {}: {err}", gate_path.display()));
    assert!(
        gate.contains("cargo xtask lancet-boundary"),
        "v1.4 release gate must invoke LANCET boundary check"
    );
    assert!(
        gate.contains("lancet-authority"),
        "v1.4 release gate must cover lancet-authority"
    );
    assert!(
        !gate.contains("v1_4_command_artifact_dogfood"),
        "v1.4 release gate must not reference the retired command artifact dogfood lane"
    );
    assert!(
        gate.contains("docs/v1.4-alpha-release-record.md")
            && gate.contains("does not write or enforce that record requirement"),
        "v1.4 release gate must name but neither write nor enforce the release record requirement"
    );
}

// #37: the `session resume` and `session prune-snapshots` subcommands were
// removed with the Upper/retained-snapshot lifecycle. The smoke test that
// asserted `prune-snapshots` rejected a missing session_id was deleted at the
// same time — the subcommand no longer exists, so the assertion no longer
// reflects an intentional contract.

// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

pub mod check;

use crate::output::{finish_banner, print_group, repo_root};

pub fn run() -> anyhow::Result<()> {
    let root = repo_root()?;
    let manifest = root.join("crates/lattice-system-harness/Cargo.toml");
    let mut failures = 0usize;

    let manifest_forbidden = check::manifest_forbidden_deps(&manifest, &root)?;
    if !print_group("manifest forbidden deps (toml)", &manifest_forbidden) {
        failures += 1;
    }

    let workspace_true = check::manifest_workspace_inheritance(&manifest)?;
    if !print_group("manifest workspace inheritance", &workspace_true) {
        failures += 1;
    }

    let meta_hits = check::metadata_transitive_workspace_members(&root)?;
    if !print_group(
        "cargo metadata product dependencies (transitive)",
        &meta_hits,
    ) {
        failures += 1;
    }

    finish_banner(
        "system harness boundary check passed",
        "system harness boundary check failed",
        failures,
    );
    Ok(())
}

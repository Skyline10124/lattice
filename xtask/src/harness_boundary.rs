// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

pub mod check;

use crate::boundary_report;
use crate::repo_root;

pub fn run() -> anyhow::Result<()> {
    let root = repo_root::repo_root()?;
    let manifest = root.join("crates/lattice-system-harness/Cargo.toml");

    boundary_report::run_groups(
        "system harness boundary check passed",
        "system harness boundary check failed",
        &[
            (
                "manifest forbidden deps (toml)",
                check::manifest_forbidden_deps(&manifest, &root)?,
            ),
            (
                "manifest workspace inheritance",
                check::manifest_workspace_inheritance(&manifest)?,
            ),
            (
                "cargo metadata product dependencies (transitive)",
                check::metadata_transitive_workspace_members(&root)?,
            ),
        ],
    )
}

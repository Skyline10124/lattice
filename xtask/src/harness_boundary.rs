// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

pub mod check;

use crate::boundary_report;
use crate::repo_root;

pub fn run() -> anyhow::Result<()> {
    let root = repo_root::repo_root()?;
    let manifest = root.join("crates/lattice-system-harness/Cargo.toml");

    boundary_report::run_groups(
        "harness 边界检查通过",
        "harness 边界检查失败",
        &[
            (
                "清单中的禁止依赖（toml）",
                check::manifest_forbidden_deps(&manifest, &root)?,
            ),
            (
                "清单中的 workspace 继承",
                check::manifest_workspace_inheritance(&manifest)?,
            ),
            (
                "cargo metadata 中的产品依赖（含传递依赖）",
                check::metadata_transitive_workspace_members(&root)?,
            ),
            (
                "生产 feature 或普通依赖可达的测试专用 feature",
                check::production_test_feature_hits(&root)?,
            ),
        ],
    )
}

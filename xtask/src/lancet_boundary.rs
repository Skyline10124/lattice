// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

pub mod check;

use crate::boundary_report;
use crate::repo_root;

pub fn run() -> anyhow::Result<()> {
    let root = repo_root::repo_root()?;

    boundary_report::run_groups(
        "LANCET boundary check passed",
        "LANCET boundary check failed",
        &[
            (
                "lancet-audit forbidden dependency graph (operator read stays runtime-free)",
                check::lancet_audit_forbidden_dependency_graph(&root)?,
            ),
            (
                "#139-B forbidden composition edges",
                check::issue139b_forbidden_dependency_graph(&root)?,
            ),
            (
                "test-only features reachable from production features/dependencies",
                check::production_test_feature_hits(&root)?,
            ),
        ],
    )
}

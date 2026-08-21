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
                "lattice-agent forbidden dependency graph",
                check::lattice_agent_forbidden_dependency_graph(&root)?,
            ),
            (
                "lancet-audit forbidden dependency graph (operator read stays CEP-server-free)",
                check::lancet_audit_forbidden_dependency_graph(&root)?,
            ),
            (
                "DefaultToolExecutor outside allowlisted internals",
                check::scan_with_allowlist(
                    &root,
                    "DefaultToolExecutor",
                    check::DEFAULT_TOOL_EXECUTOR_ALLOWLIST,
                )?,
            ),
            (
                "default_tool_definitions outside allowlisted fixtures",
                check::scan_with_allowlist(
                    &root,
                    "default_tool_definitions",
                    check::DEFAULT_TOOL_DEFINITIONS_ALLOWLIST,
                )?,
            ),
            (
                "lattice-agent legacy ToolExecutor re-export",
                check::tool_executor_reexport_hits(&root)?,
            ),
            (
                "CompositeExecutor outside registry/server internals",
                check::scan_with_allowlist(
                    &root,
                    "CompositeExecutor",
                    check::COMPOSITE_EXECUTOR_ALLOWLIST,
                )?,
            ),
            (
                "lattice-cli production legacy executor bypass",
                check::cli_production_bypass_hits(&root)?,
            ),
            (
                "test-only features enabled from production [dependencies]",
                check::production_test_feature_hits(&root)?,
            ),
        ],
    )
}

// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

pub mod check;

use crate::output::{finish_banner, print_group, repo_root};

pub fn run() -> anyhow::Result<()> {
    let root = repo_root()?;
    let mut failures = 0usize;

    let graph = check::lattice_agent_forbidden_dependency_graph(&root)?;
    if !print_group("lattice-agent forbidden dependency graph", &graph) {
        failures += 1;
    }

    let default_tool_executor = check::scan_with_allowlist(
        &root,
        "DefaultToolExecutor",
        check::DEFAULT_TOOL_EXECUTOR_ALLOWLIST,
    )?;
    if !print_group(
        "DefaultToolExecutor outside allowlisted internals",
        &default_tool_executor,
    ) {
        failures += 1;
    }

    let default_tool_definitions = check::scan_with_allowlist(
        &root,
        "default_tool_definitions",
        check::DEFAULT_TOOL_DEFINITIONS_ALLOWLIST,
    )?;
    if !print_group(
        "default_tool_definitions outside allowlisted fixtures",
        &default_tool_definitions,
    ) {
        failures += 1;
    }

    let tool_executor_reexport = check::tool_executor_reexport_hits(&root)?;
    if !print_group(
        "lattice-agent legacy ToolExecutor re-export",
        &tool_executor_reexport,
    ) {
        failures += 1;
    }

    let composite = check::scan_with_allowlist(
        &root,
        "CompositeExecutor",
        check::COMPOSITE_EXECUTOR_ALLOWLIST,
    )?;
    if !print_group(
        "CompositeExecutor outside registry/server internals",
        &composite,
    ) {
        failures += 1;
    }

    let cli_bypass = check::cli_production_bypass_hits(&root)?;
    if !print_group("lattice-cli production legacy executor bypass", &cli_bypass) {
        failures += 1;
    }

    finish_banner(
        "LANCET boundary check passed",
        "LANCET boundary check failed",
        failures,
    );
    Ok(())
}

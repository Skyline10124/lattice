// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::collections::BTreeSet;

use lattice_llm::ToolDefinition;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EnabledToolError {
    pub unknown: Vec<String>,
    pub available: Vec<String>,
}

pub(crate) fn resolve_enabled_tools(
    enabled: &[String],
    inventory: &[ToolDefinition],
) -> Result<Vec<ToolDefinition>, EnabledToolError> {
    let available: BTreeSet<String> = inventory.iter().map(|d| d.name.clone()).collect();
    let requested: BTreeSet<String> = enabled.iter().cloned().collect();

    let unknown: Vec<String> = requested.difference(&available).cloned().collect();
    if !unknown.is_empty() {
        let mut unknown = unknown;
        unknown.sort();
        let mut available: Vec<String> = available.into_iter().collect();
        available.sort();
        return Err(EnabledToolError { unknown, available });
    }

    Ok(inventory
        .iter()
        .filter(|d| requested.contains(&d.name))
        .map(|d| ToolDefinition::new(&d.name, &d.description, d.parameters.clone()))
        .collect())
}

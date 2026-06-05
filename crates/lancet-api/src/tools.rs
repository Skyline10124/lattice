// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Catalog-derived LLM-facing tool projection and enabled-tool resolution.

use std::collections::BTreeSet;

use lancet_catalog::catalog;
use lancet_native::to_tool_definitions;
use lattice_llm::ToolDefinition;

use crate::error::LancetError;

/// Build catalog-derived tool definitions filtered by an enabled list.
///
/// Returns [`LancetError::UnknownTools`] if any requested name is absent from the
/// production catalog.
pub fn resolve_catalog_tool_definitions(
    enabled: &[String],
) -> Result<Vec<ToolDefinition>, LancetError> {
    let inventory = to_tool_definitions(catalog());
    resolve_enabled_tools(enabled, &inventory)
}

/// Filter a tool inventory down to the requested enabled names.
///
/// Generic over the inventory so callers can project from any source; the
/// production path feeds it [`to_tool_definitions`] over the catalog.
pub fn resolve_enabled_tools(
    enabled: &[String],
    inventory: &[ToolDefinition],
) -> Result<Vec<ToolDefinition>, LancetError> {
    let available: BTreeSet<String> = inventory.iter().map(|d| d.name.clone()).collect();
    let requested: BTreeSet<String> = enabled.iter().cloned().collect();

    let unknown: Vec<String> = requested.difference(&available).cloned().collect();
    if !unknown.is_empty() {
        let mut unknown = unknown;
        unknown.sort();
        let mut available: Vec<String> = available.into_iter().collect();
        available.sort();
        return Err(LancetError::UnknownTools { unknown, available });
    }

    Ok(inventory
        .iter()
        .filter(|d| requested.contains(&d.name))
        .map(|d| ToolDefinition::new(&d.name, &d.description, d.parameters.clone()))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_catalog_tool_definitions_rejects_unknown() {
        let err = resolve_catalog_tool_definitions(&["does.not.exist".to_string()])
            .expect_err("unknown tool must error");
        match err {
            LancetError::UnknownTools { unknown, available } => {
                assert_eq!(unknown, vec!["does.not.exist".to_string()]);
                assert!(!available.is_empty());
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn resolve_catalog_tool_definitions_filters_to_enabled() {
        let defs = resolve_catalog_tool_definitions(&["observe.workspace".to_string()])
            .expect("known tool resolves");
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "observe.workspace");
    }
}

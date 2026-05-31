//! Explicit enabled-tool resolver and LLM schema adapter.
//!
//! Resolves a list of tool names against a provided inventory of definitions.
//! The protocol layer does not own tool definitions — it receives them from
//! the implementor (e.g., cep-executor).

use std::collections::BTreeSet;

use lattice_llm::ToolDefinition;

/// The resolved set of explicitly enabled tools and their LLM-compatible
/// definitions, derived from a profile `tools.enabled` list.
#[derive(Debug, Clone, PartialEq)]
pub struct EnabledTools {
    pub tool_names: BTreeSet<String>,
    pub definitions: Vec<ToolDefinition>,
}

/// Error returned when one or more enabled tool names are not found in the
/// provided inventory.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown enabled tools: {unknown:?}; available built-in tools: {available:?}")]
pub struct EnabledToolError {
    pub unknown: Vec<String>,
    pub available: Vec<String>,
}

/// Resolve an enabled-tool list against a provided inventory.
///
/// Only exact name matches are included. Unknown names cause a fast failure
/// with both the unknown names and all available tool names, so the caller
/// can produce a clear diagnostic before any model or executor call.
pub fn resolve_enabled_tools(
    enabled: &[String],
    inventory: &[ToolDefinition],
) -> Result<EnabledTools, EnabledToolError> {
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

    let definitions: Vec<ToolDefinition> = inventory
        .iter()
        .filter(|d| requested.contains(&d.name))
        .map(|d| ToolDefinition::new(&d.name, &d.description, d.parameters.clone()))
        .collect();

    Ok(EnabledTools {
        tool_names: requested,
        definitions,
    })
}

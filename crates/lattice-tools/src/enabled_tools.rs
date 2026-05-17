//! Explicit enabled-tool resolver and LLM schema adapter for v1.1 main path.
//!
//! This module replaces bundle-based capability resolution for the v1.1 main
//! path. Only tool names explicitly listed in `profile.tools.enabled` are
//! exposed to the model and allowed for execution. Bundle expansion
//! (`resolve_capabilities`) is not used here.
//!
//! # Design decisions
//!
//! - D-13: Only explicit `tools.enabled` names are used.
//! - D-14: No `ToolBundle` expansion enters the v1.1 main path.
//! - D-15: Definitions come from the built-in inventory, filtered by exact names.
//! - D-16: Unknown names fail before any model or executor call.
//! - D-17: `bash` and `web_search` are ordinary tools -- no special authorization.
//! - D-18: No second authorization field or approval/policy flow for shell/web.
//! - D-03/D-04: Module is named `enabled_tools`, not `v11` or `simple`.

use std::collections::BTreeSet;

use crate::tool_definitions::default_tool_definitions;

/// The resolved set of explicitly enabled tools and their LLM-compatible
/// definitions, derived from the profile `tools.enabled` list.
#[derive(Debug, Clone, PartialEq)]
pub struct EnabledTools {
    /// Deterministic set of enabled tool names.
    pub tool_names: BTreeSet<String>,
    /// Ordered LLM tool definitions matching the enabled names, suitable for
    /// inclusion in `ChatRequest.tools`.
    pub definitions: Vec<lattice_llm::ToolDefinition>,
}

/// Error returned when one or more enabled tool names are not found in the
/// built-in inventory.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown enabled tools: {unknown:?}; available built-in tools: {available:?}")]
pub struct EnabledToolError {
    /// Names from the `enabled` list that were not found in the inventory.
    pub unknown: Vec<String>,
    /// All tool names available in the built-in inventory.
    pub available: Vec<String>,
}

/// Resolve the profile `tools.enabled` list into [`EnabledTools`].
///
/// Only exact name matches against `default_tool_definitions()` are included.
/// Unknown names cause a fast failure with both the unknown names and all
/// available built-in tool names, so the caller can produce a clear diagnostic
/// before any model or executor call.
///
/// Bundle expansion is intentionally not performed (D-14). If you need
/// bundle-based resolution, use [`crate::capabilities::resolve_capabilities`].
pub fn resolve_enabled_tools(enabled: &[String]) -> Result<EnabledTools, EnabledToolError> {
    let inventory = default_tool_definitions();
    let available: BTreeSet<String> = inventory.iter().map(|d| d.name.clone()).collect();
    let requested: BTreeSet<String> = enabled.iter().cloned().collect();

    // Validate that all requested names exist in the inventory.
    let unknown: Vec<String> = requested.difference(&available).cloned().collect();
    if !unknown.is_empty() {
        let mut unknown = unknown;
        unknown.sort();
        let mut available: Vec<String> = available.into_iter().collect();
        available.sort();
        return Err(EnabledToolError { unknown, available });
    }

    // Filter inventory to only the requested names, preserving inventory order.
    let definitions: Vec<lattice_llm::ToolDefinition> = inventory
        .into_iter()
        .filter(|d| requested.contains(&d.name))
        .map(|d| lattice_llm::ToolDefinition::new(d.name, d.description, d.parameters))
        .collect();

    Ok(EnabledTools {
        tool_names: requested,
        definitions,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_enabled_exposes_only_named_tools() {
        let enabled: Vec<String> = vec!["read_file".into(), "grep".into()];
        let result = resolve_enabled_tools(&enabled).expect("should resolve known tools");

        // Only the two explicitly named tools are present
        assert_eq!(result.tool_names.len(), 2);
        assert!(result.tool_names.contains("read_file"));
        assert!(result.tool_names.contains("grep"));

        // LLM definitions match the enabled names
        assert_eq!(result.definitions.len(), 2);
        let names: Vec<&str> = result.definitions.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"grep"));

        // Tools NOT in the enabled list are absent
        assert!(!result.tool_names.contains("write_file"));
        assert!(!result.tool_names.contains("bash"));
        assert!(!result.tool_names.contains("web_search"));
        assert!(!result.tool_names.contains("todo_write"));
    }

    #[test]
    fn bash_and_web_search_require_explicit_names() {
        // Empty list: no tools at all
        let empty: Vec<String> = vec![];
        let result = resolve_enabled_tools(&empty).expect("empty is valid");
        assert!(result.tool_names.is_empty());
        assert!(!result.tool_names.contains("bash"));
        assert!(!result.tool_names.contains("web_search"));

        // Non-shell/non-web list: bash and web_search still absent
        let non_special: Vec<String> = vec!["read_file".into(), "grep".into()];
        let result = resolve_enabled_tools(&non_special).expect("should resolve");
        assert!(!result.tool_names.contains("bash"));
        assert!(!result.tool_names.contains("web_search"));

        // Explicit inclusion: bash and web_search are available when named
        let with_special: Vec<String> =
            vec!["read_file".into(), "bash".into(), "web_search".into()];
        let result = resolve_enabled_tools(&with_special).expect("should resolve with bash+web");
        assert!(result.tool_names.contains("bash"));
        assert!(result.tool_names.contains("web_search"));
        assert_eq!(result.tool_names.len(), 3);
        assert_eq!(result.definitions.len(), 3);
    }

    #[test]
    fn unknown_enabled_tools_fail_fast() {
        let enabled: Vec<String> = vec!["read_file".into(), "nonexistent_tool".into()];
        let err = resolve_enabled_tools(&enabled).expect_err("should fail on unknown");

        assert!(
            err.unknown.contains(&"nonexistent_tool".to_string()),
            "error should list the unknown tool, got: {:?}",
            err.unknown
        );
        // Available list should contain known built-in tools
        assert!(
            err.available.contains(&"read_file".to_string()),
            "available should include read_file, got: {:?}",
            err.available
        );
        assert!(
            err.available.contains(&"bash".to_string()),
            "available should include bash, got: {:?}",
            err.available
        );
    }

    #[test]
    fn bundle_resolver_is_not_used_by_enabled_tools() {
        // Selecting no explicit names exposes no tools, even though legacy
        // bundles still exist elsewhere in the codebase.
        let empty: Vec<String> = vec![];
        let result = resolve_enabled_tools(&empty).expect("empty is valid");
        assert!(
            result.tool_names.is_empty(),
            "no enabled names means no tools, regardless of any bundle state"
        );
        assert!(
            result.definitions.is_empty(),
            "no enabled names means no LLM definitions"
        );
    }

    #[test]
    fn all_required_builtins_can_be_resolved_by_exact_name() {
        let required = [
            "read_file",
            "read_files",
            "write_file",
            "patch",
            "grep",
            "find_files",
            "list_directory",
            "todo_write",
        ];
        let enabled: Vec<String> = required.iter().map(|s| s.to_string()).collect();
        let result = resolve_enabled_tools(&enabled).expect("all required tools should resolve");

        for name in &required {
            assert!(
                result.tool_names.contains(*name),
                "required tool '{name}' should be in tool_names"
            );
        }
        assert_eq!(result.definitions.len(), required.len());
    }

    #[test]
    fn llm_definitions_match_core_tool_definition_shape() {
        let enabled: Vec<String> = vec!["read_file".into()];
        let result = resolve_enabled_tools(&enabled).expect("should resolve");
        let def = &result.definitions[0];

        assert_eq!(def.name, "read_file");
        assert!(
            !def.description.is_empty(),
            "definition should have a description"
        );
        assert!(
            def.parameters.is_object(),
            "parameters should be a JSON object"
        );
    }
}

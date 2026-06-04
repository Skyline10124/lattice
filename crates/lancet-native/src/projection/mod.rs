//! Project CEP catalog descriptors into LLM-facing tool definitions.
//!
//! Tool definitions are projected from `lancet_catalog::CATALOG` via
//! [`to_tool_definitions`]. Names, descriptions, and JSON schemas all come from
//! the catalog, with no namespace stripping, rename maps, or hand-written
//! schemas.

use lancet_catalog::MethodDescriptor;
use lattice_llm::ToolDefinition;

/// Project feature-gated scaffold descriptors into LLM-facing tool definitions.
///
/// Not part of the default tool surface; call only in tests/experiments with
/// `inspect-scaffold` enabled.
#[cfg(feature = "inspect-scaffold")]
pub fn scaffold_tool_definitions() -> Vec<ToolDefinition> {
    to_tool_definitions(lancet_catalog::scaffold_catalog())
}

/// Project CEP catalog descriptors into LLM-facing tool definitions.
///
/// Names and parameter schemas come directly from the catalog. The full dotted
/// method name (`observe.workspace`, `exec.command_run`, etc.) is the single
/// identity used for the wire, registry, audit, and LLM-facing tool name.
pub fn to_tool_definitions(catalog: &[MethodDescriptor]) -> Vec<ToolDefinition> {
    catalog
        .iter()
        .map(|descriptor| {
            ToolDefinition::new(
                descriptor.name,
                descriptor.description,
                (descriptor.params_schema)(),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lancet_catalog::catalog;

    #[test]
    fn projection_length_equals_catalog_len() {
        let projected = to_tool_definitions(catalog());
        assert_eq!(projected.len(), catalog().len());
    }

    #[test]
    fn projected_names_match_catalog_exactly() {
        let projected = to_tool_definitions(catalog());
        for (tool, descriptor) in projected.iter().zip(catalog().iter()) {
            assert_eq!(tool.name, descriptor.name);
        }
    }

    #[test]
    fn projected_names_are_full_dotted_names() {
        let projected = to_tool_definitions(catalog());
        for tool in &projected {
            assert!(
                tool.name.contains('.'),
                "expected dotted name: {}",
                tool.name
            );
            assert!(
                !tool.name.contains('/'),
                "slash names forbidden: {}",
                tool.name
            );
        }
    }

    #[test]
    fn observe_workspace_projection_uses_catalog_name_and_typed_params() {
        let projected = to_tool_definitions(catalog());
        let observe_workspace = projected
            .iter()
            .find(|tool| tool.name == "observe.workspace")
            .expect("observe.workspace in projection");

        assert_eq!(observe_workspace.name, "observe.workspace");
        assert_eq!(observe_workspace.parameters["type"], "object");
        assert!(observe_workspace.parameters["oneOf"].is_array());
    }

    #[test]
    fn no_projected_name_is_namespace_stripped() {
        let projected = to_tool_definitions(catalog());
        for tool in &projected {
            let prefix = tool.name.split('.').next().unwrap_or("");
            assert!(
                tool.name.starts_with(&format!("{prefix}.")),
                "name appears namespace-stripped: {}",
                tool.name
            );
        }
    }

    #[test]
    fn default_projection_has_no_inspect_or_navigate() {
        let projected = to_tool_definitions(catalog());
        for tool in &projected {
            assert_ne!(tool.name, "workspace.inspect");
            assert_ne!(tool.name, "workspace.navigate");
        }
    }

    #[test]
    fn projection_keeps_exec_command_run_as_only_command_tool_without_boundary_params() {
        let projected = to_tool_definitions(catalog());
        let command_tools: Vec<_> = projected
            .iter()
            .filter(|tool| tool.name.starts_with("exec."))
            .collect();

        assert_eq!(command_tools.len(), 1);
        assert_eq!(command_tools[0].name, "exec.command_run");
        let rendered_schema = command_tools[0].parameters.to_string();
        for forbidden in [
            "supervisor",
            "effective_boundary",
            "host_probe_summary",
            "boundary_setup_failed_closed",
            "backend_invoked",
        ] {
            assert!(
                !rendered_schema.contains(forbidden),
                "agent-visible command schema leaked native metadata field {forbidden}"
            );
        }
    }
}

#[cfg(all(test, feature = "inspect-scaffold"))]
mod scaffold_tool_definitions {
    use super::*;
    use lancet_catalog::scaffold_catalog;

    #[test]
    fn scaffold_tool_definitions_match_scaffold_catalog() {
        let projected = scaffold_tool_definitions();
        assert_eq!(projected.len(), scaffold_catalog().len());
        for (tool, descriptor) in projected.iter().zip(scaffold_catalog().iter()) {
            assert_eq!(tool.name, descriptor.name);
            assert!(tool.name.contains('.'));
            assert_eq!(
                tool.parameters.get("type").and_then(|v| v.as_str()),
                Some("object")
            );
        }
    }

    #[test]
    fn scaffold_tool_definitions_include_inspect_and_navigate() {
        let projected = scaffold_tool_definitions();
        let names: Vec<_> = projected.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"workspace.inspect"));
        assert!(names.contains(&"workspace.navigate"));
    }
}

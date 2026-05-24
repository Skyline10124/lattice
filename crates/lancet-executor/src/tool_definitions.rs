//! Project CEP catalog descriptors into LLM-facing tool definitions.
//!
//! Per CEP-13, the **only** way to obtain `ToolDefinition` values for the LLM
//! surface is to project from `lancet_protocol::CATALOG` via
//! [`to_tool_definitions`]. Names, descriptions, and JSON schemas all come
//! from the catalog — no namespace stripping, no rename maps, no hand-written
//! schemas. The legacy `default_tool_definitions` helper that mirrored the
//! pre-CEP tool surface was removed at the end of Phase 19; consumers must
//! call [`to_tool_definitions`] against the catalog.

use lancet_protocol::MethodDescriptor;
use lattice_llm::ToolDefinition;

/// Project CEP catalog descriptors into LLM-facing tool definitions.
///
/// Names and parameter schemas come directly from the catalog — no namespace
/// stripping, rename maps, or hand-written JSON schemas. The full dotted method
/// name (`workspace.read_file`, `command.run`, …) is the single identity used
/// for the wire, the registry, the audit, and the LLM-facing tool name.
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
    use lancet_protocol::catalog;

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
            assert!(tool.name.contains('.'), "expected dotted name: {}", tool.name);
            assert!(!tool.name.contains('/'), "slash names forbidden: {}", tool.name);
        }
    }

    #[test]
    fn read_file_projection_uses_catalog_name_and_typed_params() {
        let projected = to_tool_definitions(catalog());
        let read_file = projected
            .iter()
            .find(|tool| tool.name == "workspace.read_file")
            .expect("workspace.read_file in projection");

        assert_ne!(read_file.name, "read_file");
        assert!(read_file.parameters["properties"]["path"].is_object());
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
}

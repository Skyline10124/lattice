//! Project CEP catalog descriptors into LLM-facing tool definitions.
//!
//! Per CEP-13, the **only** way to obtain `ToolDefinition` values for the LLM
//! surface is to project from `cep_protocol::CATALOG` via
//! [`to_tool_definitions`]. Names, descriptions, and JSON schemas all come
//! from the catalog — no namespace stripping, no rename maps, no hand-written
//! schemas. The legacy `default_tool_definitions` helper that mirrored the
//! pre-CEP tool surface was removed at the end of CEP; consumers must
//! call [`to_tool_definitions`] against the catalog.

use cep_protocol::MethodDescriptor;
use lattice_llm::ToolDefinition;

/// Project CEP catalog descriptors into LLM-facing tool definitions.
///
/// Names and parameter schemas come directly from the catalog — no namespace
/// stripping, rename maps, or hand-written JSON schemas. The full dotted method
/// name (`workspace.read_file`, `command.run`, …) is the single identity used
/// for the wire, the registry, the audit, and the LLM-facing tool name.
/// Project feature-gated scaffold descriptors into LLM-facing tool definitions.
///
/// Not part of the default alpha tool surface; call only in tests/experiments with
/// `inspect-scaffold` enabled.
#[cfg(feature = "inspect-scaffold")]
pub fn scaffold_tool_definitions() -> Vec<ToolDefinition> {
    to_tool_definitions(cep_protocol::scaffold_catalog())
}

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
    use cep_protocol::catalog;

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

    #[test]
    fn default_projection_has_no_inspect_or_navigate() {
        let projected = to_tool_definitions(catalog());
        for tool in &projected {
            assert_ne!(tool.name, "workspace.inspect");
            assert_ne!(tool.name, "workspace.navigate");
        }
    }
}

#[cfg(all(test, feature = "inspect-scaffold"))]
mod scaffold_tool_definitions {
    use super::*;
    use cep_protocol::scaffold_catalog;

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

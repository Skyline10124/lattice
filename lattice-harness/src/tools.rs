use std::collections::HashMap;
use std::sync::Arc;

use lattice_core::types::ToolDefinition;

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("execution failed: {0}")]
    Execution(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid arguments: {0}")]
    InvalidArgs(String),
    #[error("MCP server '{server}': {message}")]
    McpUnreachable { server: String, message: String },
    #[error("timeout after {0}ms")]
    Timeout(u64),
}

pub struct RegisteredTool {
    pub definition: ToolDefinition,
    pub handler: ToolHandler,
}

pub enum ToolHandler {
    Native(Arc<dyn Fn(serde_json::Value) -> Result<String, ToolError> + Send + Sync>),
    McpBacked { server: String, tool_name: String },
}

#[derive(Default)]
pub struct ToolRegistry {
    tools: HashMap<String, RegisteredTool>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, name: &str, handler: ToolHandler, definition: ToolDefinition) {
        self.tools.insert(
            name.to_string(),
            RegisteredTool {
                definition,
                handler,
            },
        );
    }

    pub fn get(&self, name: &str) -> Option<&RegisteredTool> {
        self.tools.get(name)
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

/// Merge tool definitions from three layers. Priority: plugin > slot > shared.
/// Tool names not found in the registry are warned and skipped.
pub fn merge_tool_definitions(
    registry: &ToolRegistry,
    shared_tool_names: &[String],
    slot_tool_names: &[String],
    plugin_tools: &[ToolDefinition],
) -> Vec<ToolDefinition> {
    use indexmap::IndexMap;
    let mut merged: IndexMap<String, ToolDefinition> = IndexMap::new();

    for names in [shared_tool_names, slot_tool_names] {
        for name in names {
            match registry.get(name) {
                Some(tool) => {
                    merged.insert(name.clone(), tool.definition.clone());
                }
                None => {
                    tracing::warn!("tool '{}' not in ToolRegistry — skipping", name);
                }
            }
        }
    }

    for td in plugin_tools {
        merged.insert(td.name.clone(), td.clone());
    }

    merged.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tool(name: &str) -> ToolDefinition {
        ToolDefinition {
            name: name.into(),
            description: "test".into(),
            parameters: serde_json::json!({}),
        }
    }

    #[test]
    fn test_merge_tool_definitions() {
        let mut registry = ToolRegistry::new();
        registry.register(
            "shared",
            ToolHandler::Native(Arc::new(|_| Ok("ok".into()))),
            make_tool("shared"),
        );
        registry.register(
            "slot_only",
            ToolHandler::Native(Arc::new(|_| Ok("ok".into()))),
            make_tool("slot_only"),
        );

        let shared = vec!["shared".to_string()];
        let slot = vec!["slot_only".to_string(), "missing".to_string()];
        let plugin = vec![make_tool("plugin_tool")];

        let result = merge_tool_definitions(&registry, &shared, &slot, &plugin);
        assert_eq!(result.len(), 3); // shared, slot_only, plugin_tool (missing skipped)
    }

    #[test]
    fn test_merge_plugin_overrides() {
        let mut registry = ToolRegistry::new();
        registry.register(
            "dupe",
            ToolHandler::Native(Arc::new(|_| Ok("slot".into()))),
            make_tool("dupe"),
        );

        let plugin = vec![ToolDefinition {
            name: "dupe".into(),
            description: "plugin wins".into(),
            parameters: serde_json::json!({}),
        }];

        let result = merge_tool_definitions(&registry, &[], &["dupe".to_string()], &plugin);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].description, "plugin wins");
    }
}

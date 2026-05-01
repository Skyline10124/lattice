//! Default tool definitions for the lattice-agent.
//!
//! Separated from [`crate::tools::DefaultToolExecutor`] to allow consumers
//! to reference tool definitions without importing the full executor.

use lattice_core::types::ToolDefinition;

/// Returns the default set of tool definitions: read_file, grep, write_file,
/// list_directory, bash, patch, web_search.
pub fn default_tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition::new(
            "read_file".into(),
            "Read the contents of a file at the given absolute path.".into(),
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Absolute path to the file"
                    }
                },
                "required": ["path"]
            }),
        ),
        ToolDefinition::new(
            "grep".into(),
            "Search for a pattern in files in a directory.".into(),
            serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Regex pattern to search for"
                    },
                    "path": {
                        "type": "string",
                        "description": "Directory to search in (default: current dir)"
                    }
                },
                "required": ["pattern"]
            }),
        ),
        ToolDefinition::new(
            "write_file".into(),
            "Write content to a file. Only allowed under the project source directories.".into(),
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Relative path under project root"
                    },
                    "content": {
                        "type": "string",
                        "description": "File content to write"
                    }
                },
                "required": ["path", "content"]
            }),
        ),
        ToolDefinition::new(
            "list_directory".into(),
            "List files and directories in a given path.".into(),
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Directory path to list"
                    }
                },
                "required": ["path"]
            }),
        ),
        ToolDefinition::new(
            "bash".into(),
            "Run a command and return its output. Prefer other tools when possible.".into(),
            serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "Shell command to run"
                    }
                },
                "required": ["command"]
            }),
        ),
        ToolDefinition::new(
            "patch".into(),
            "Apply a find/replace edit to a file. Safer than write_file for targeted changes."
                .into(),
            serde_json::json!({
                "type": "object",
                "properties": {
                    "file_path": {
                        "type": "string",
                        "description": "Relative path under project root"
                    },
                    "search": {
                        "type": "string",
                        "description": "Exact text to find (must appear exactly once)"
                    },
                    "insert": {
                        "type": "string",
                        "description": "Replacement text"
                    }
                },
                "required": ["file_path", "search", "insert"]
            }),
        ),
        ToolDefinition::new(
            "web_search".into(),
            "Fetch a URL and return its text content.".into(),
            serde_json::json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "URL to fetch"
                    }
                },
                "required": ["url"]
            }),
        ),
    ]
}

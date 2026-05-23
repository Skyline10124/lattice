//! Default tool definitions for the lattice engine.
//!
//! Separated from [`crate::executor::DefaultToolExecutor`] to allow consumers
//! to reference tool definitions without importing the full executor.

use lattice_llm::ToolDefinition;

/// Returns the built-in tool schema inventory.
pub fn default_tool_definitions() -> Vec<ToolDefinition> {
    #[allow(unused_mut)]
    let mut defs = vec![
        ToolDefinition::new(
            "workspace_context",
            "Summarize the current workspace: top-level entries, git state, project instruction files, and likely verification commands.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Directory to summarize, relative to the project root (default: project root)"
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 100,
                        "description": "Maximum top-level entries to return (default: 40, hard cap: 100)"
                    }
                }
            }),
        ),
        ToolDefinition::new(
            "todo_write",
            "Record the current task plan as a concise todo list. Use for multi-step coding work and keep at most one item in_progress.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "todos": {
                        "type": "array",
                        "maxItems": 100,
                        "items": {
                            "type": "object",
                            "properties": {
                                "content": {
                                    "type": "string",
                                    "description": "Concrete task or checkpoint"
                                },
                                "status": {
                                    "type": "string",
                                    "enum": ["pending", "in_progress", "completed"],
                                    "description": "Current status for this todo"
                                }
                            },
                            "required": ["content", "status"]
                        }
                    }
                },
                "required": ["todos"]
            }),
        ),
        ToolDefinition::new(
            "read_file",
            "Read a text file. Supports optional 1-based line offset/limit and line-number output for targeted inspection. To read multiple files, issue parallel read_file tool calls.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Absolute path, or a path relative to the project root"
                    },
                    "offset": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Optional 1-based starting line"
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Optional maximum number of lines to return"
                    },
                    "line_numbers": {
                        "type": "boolean",
                        "description": "Whether to prefix returned lines with line numbers (default: true)"
                    }
                },
                "required": ["path"]
            }),
        ),
        ToolDefinition::new(
            "grep",
            "Search for a regex pattern in files under a path. Results are capped; narrow the path or pattern if truncated.",
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
                    },
                    "max_results": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Maximum matching lines to return (default: 200, hard cap: 1000)"
                    }
                },
                "required": ["pattern"]
            }),
        ),
        ToolDefinition::new(
            "write_file",
            "Write content to a file. Only allowed under the project source directories.",
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
            "list_directory",
            "List files and directories in a path with stable sorting and output limits.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Directory path to list"
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Maximum number of entries to return (default: 200, hard cap: 1000)"
                    }
                },
                "required": ["path"]
            }),
        ),
        ToolDefinition::new(
            "find_files",
            "Find files or directories by a glob-like pattern matched against names and relative paths. Results are stable and capped.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Glob-like pattern, e.g. \"*.rs\", \"**/Cargo.toml\", or \"coding_*\""
                    },
                    "path": {
                        "type": "string",
                        "description": "Directory to search in (default: project root)"
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Maximum number of paths to return (default: 200, hard cap: 1000)"
                    }
                },
                "required": ["pattern"]
            }),
        ),
        ToolDefinition::new(
            "bash",
            "Run a command and return its output. Prefer other tools when possible.",
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
            "patch",
            "Apply structured patch text with multi-file add, update, and delete operations. Prefer this for edits after reading current file hashes.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "patch": {
                        "type": "string",
                        "description": "Codex-style structured patch text bounded by *** Begin Patch and *** End Patch"
                    }
                },
                "required": ["patch"]
            }),
        ),
        ToolDefinition::new(
            "web_fetch",
            "Fetch a URL and return its text content.",
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
    ];

    defs
}

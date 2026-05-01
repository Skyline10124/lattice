//! Default tool definitions for the lattice-agent.
//!
//! Separated from [`crate::tools::DefaultToolExecutor`] to allow consumers
//! to reference tool definitions without importing the full executor.
//! This module will grow as tool definitions are added/refined.

use lattice_core::types::ToolDefinition;

/// Returns the default set of tool definitions: read_file, grep, write_file,
/// list_directory, run_test, run_clippy, bash, patch, run_command,
/// list_processes, web_search, web_fetch, browser_navigate, browser_screenshot,
/// browser_console, execute_code, agent_call.
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
            "run_test".into(),
            "Run cargo test and return the output (last 50 lines).".into(),
            serde_json::json!({
                "type": "object",
                "properties": {
                    "test_name": {
                        "type": "string",
                        "description": "Optional test name filter"
                    }
                },
            }),
        ),
        ToolDefinition::new(
            "run_clippy".into(),
            "Run cargo clippy and return the warnings (last 30 lines).".into(),
            serde_json::json!({
                "type": "object",
                "properties": {},
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
            "run_command".into(),
            "Run a command in the project directory with a timeout.".into(),
            serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "Command to run"
                    },
                    "timeout_secs": {
                        "type": "integer",
                        "description": "Timeout in seconds (default 30)"
                    }
                },
                "required": ["command"]
            }),
        ),
        ToolDefinition::new(
            "list_processes".into(),
            "List running processes (ps aux head 30).".into(),
            serde_json::json!({
                "type": "object",
                "properties": {},
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
        ToolDefinition::new(
            "web_fetch".into(),
            "Fetch a URL and return the first 5000 characters.".into(),
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
        ToolDefinition::new(
            "browser_navigate".into(),
            "Open a URL in the system browser.".into(),
            serde_json::json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "URL to open"
                    }
                },
                "required": ["url"]
            }),
        ),
        ToolDefinition::new(
            "browser_screenshot".into(),
            "Take a screenshot using import (ImageMagick) or scrot.".into(),
            serde_json::json!({
                "type": "object",
                "properties": {
                    "filename": {
                        "type": "string",
                        "description": "Output filename (default: screenshot.png)"
                    }
                },
            }),
        ),
        ToolDefinition::new(
            "browser_console".into(),
            "Run JavaScript in the browser console. Not available in CLI mode.".into(),
            serde_json::json!({
                "type": "object",
                "properties": {
                    "code": {
                        "type": "string",
                        "description": "JavaScript code to execute"
                    }
                },
                "required": ["code"]
            }),
        ),
        ToolDefinition::new(
            "execute_code".into(),
            "Write code to a temp file and execute it. Allowed languages: py, js, rs, sh, ts."
                .into(),
            serde_json::json!({
                "type": "object",
                "properties": {
                    "code": {
                        "type": "string",
                        "description": "Source code to execute"
                    },
                    "language": {
                        "type": "string",
                        "description": "Language: py, js, rs, sh, ts"
                    }
                },
                "required": ["code", "language"]
            }),
        ),
        ToolDefinition::new(
            "agent_call".into(),
            "Call another agent by name. Use 'agent_call:security-audit' to run the security audit agent, or call 'agent_call' with 'name' and 'input' arguments. Available agents are listed in the registry.".into(),
            serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Agent name to call (e.g., 'security-audit', 'refactor')"
                    },
                    "input": {
                        "type": "string",
                        "description": "Input to pass to the sub-agent"
                    }
                },
                "required": ["name", "input"]
            }),
        ),
    ]
}

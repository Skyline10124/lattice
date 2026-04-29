use artemis_core::types::ToolCall;
use artemis_core::types::ToolDefinition;

use crate::ToolExecutor;

/// Returns the default set of tool definitions: read_file, grep, write_file,
/// list_directory, run_test, run_clippy, bash.
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
    ]
}

/// Executes tools using the local filesystem and shell.
///
/// Supports: read_file, grep, write_file, list_directory, run_test,
/// run_clippy, bash. The `project_root` is used by `write_file` to
/// restrict writes to project source directories.
pub struct DefaultToolExecutor {
    pub project_root: String,
}

impl DefaultToolExecutor {
    pub fn new(project_root: &str) -> Self {
        Self {
            project_root: project_root.to_string(),
        }
    }
}

impl ToolExecutor for DefaultToolExecutor {
    fn execute(&self, call: &ToolCall) -> String {
        let args: serde_json::Value = serde_json::from_str(&call.function.arguments)
            .unwrap_or(serde_json::Value::Null);

        match call.function.name.as_str() {
            "read_file" => {
                let path = args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                std::fs::read_to_string(path)
                    .unwrap_or_else(|e| format!("Error: {}", e))
            }
            "grep" => {
                let pattern = args
                    .get("pattern")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let path = args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or(".");
                let output = std::process::Command::new("grep")
                    .args(["-rn", "--include=*.rs", pattern, path])
                    .output();
                match output {
                    Ok(o) => {
                        let mut result =
                            String::from_utf8_lossy(&o.stdout).to_string();
                        if !o.stderr.is_empty() {
                            result.push_str(&format!(
                                "\nERR:{}",
                                String::from_utf8_lossy(&o.stderr)
                            ));
                        }
                        result
                    }
                    Err(e) => format!("Error: {}", e),
                }
            }
            "write_file" => {
                let path = args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let content = args
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let abs = format!(
                    "{}/{}",
                    self.project_root,
                    path.trim_start_matches('/')
                );
                // Safety: only allow writing to project source dirs
                let allowed = [
                    "artemis-core/",
                    "artemis-agent/",
                    "artemis-memory/",
                    "artemis-token-pool/",
                    "artemis-python/",
                    "artemis-plugin/",
                    "artemis-cli/",
                    "artemis-tui/",
                ];
                let safe = allowed.iter().any(|p| abs.contains(p));
                if !safe {
                    return format!(
                        "Write denied: path '{}' must be under {}",
                        path,
                        allowed.join(", ")
                    );
                }
                if abs.contains("..") {
                    return "Write denied: path contains '..'".into();
                }
                match std::fs::write(&abs, content) {
                    Ok(_) => {
                        format!("Wrote {} bytes to {}", content.len(), path)
                    }
                    Err(e) => format!("Error writing {}: {}", path, e),
                }
            }
            "list_directory" => {
                let path = args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or(".");
                match std::fs::read_dir(path) {
                    Ok(entries) => {
                        let mut files: Vec<_> =
                            entries.filter_map(|e| e.ok()).collect();
                        files.sort_by_key(|e| e.file_name());
                        files
                            .iter()
                            .map(|e| {
                                let ty = if e
                                    .file_type()
                                    .map(|t| t.is_dir())
                                    .unwrap_or(false)
                                {
                                    "DIR"
                                } else {
                                    "FILE"
                                };
                                format!(
                                    "{}  {}",
                                    ty,
                                    e.file_name().to_string_lossy()
                                )
                            })
                            .collect::<Vec<_>>()
                            .join("\n")
                    }
                    Err(e) => format!("Error: {}", e),
                }
            }
            "run_test" => {
                let test_name = args
                    .get("test_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let mut cmd = std::process::Command::new("cargo");
                cmd.arg("test").args(["--color", "never"]);
                if !test_name.is_empty() {
                    cmd.arg("--").arg(test_name);
                }
                match cmd.output() {
                    Ok(o) => {
                        let out = String::from_utf8_lossy(&o.stdout);
                        let lines: Vec<&str> = out.lines().collect();
                        let last = lines.len().saturating_sub(50);
                        lines[last..].join("\n")
                    }
                    Err(e) => format!("Error running test: {}", e),
                }
            }
            "run_clippy" => {
                let output = std::process::Command::new("cargo")
                    .args(["clippy", "--color", "never"])
                    .output();
                match output {
                    Ok(o) => {
                        let out = String::from_utf8_lossy(&o.stdout);
                        let lines: Vec<&str> = out.lines().collect();
                        let last = lines.len().saturating_sub(30);
                        lines[last..].join("\n")
                    }
                    Err(e) => format!("Error running clippy: {}", e),
                }
            }
            "bash" => {
                let cmd = args
                    .get("command")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let output = std::process::Command::new("sh")
                    .args(["-c", cmd])
                    .output();
                match output {
                    Ok(o) => {
                        let mut result =
                            String::from_utf8_lossy(&o.stdout).to_string();
                        if !o.stderr.is_empty() {
                            result.push_str(&format!(
                                "\nERR:{}",
                                String::from_utf8_lossy(&o.stderr)
                            ));
                        }
                        result
                    }
                    Err(e) => format!("Error: {}", e),
                }
            }
            _ => format!("Unknown tool: {}", call.function.name),
        }
    }
}

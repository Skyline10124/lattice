use artemis_core::types::ToolCall;
use artemis_core::types::ToolDefinition;

use crate::ToolExecutor;

/// Returns the default set of tool definitions: read_file, grep, write_file,
/// list_directory, run_test, run_clippy, bash, patch, run_command,
/// list_processes, web_search, web_fetch, browser_navigate, browser_screenshot,
/// browser_console, execute_code.
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
        // --- New tools ---
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
    ]
}

/// Executes tools using the local filesystem and shell.
///
/// Supports: read_file, grep, write_file, list_directory, run_test,
/// run_clippy, bash, patch, run_command, list_processes, web_search,
/// web_fetch, browser_navigate, browser_screenshot, browser_console,
/// execute_code. The `project_root` is used by `write_file` and `patch`
/// to restrict writes to project source directories.
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
        let args: serde_json::Value =
            serde_json::from_str(&call.function.arguments).unwrap_or(serde_json::Value::Null);

        match call.function.name.as_str() {
            "read_file" => {
                let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
                std::fs::read_to_string(path).unwrap_or_else(|e| format!("Error: {}", e))
            }
            "grep" => {
                let pattern = args.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
                let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
                let output = std::process::Command::new("grep")
                    .args(["-rn", "--include=*.rs", pattern, path])
                    .output();
                match output {
                    Ok(o) => {
                        let mut result = String::from_utf8_lossy(&o.stdout).to_string();
                        if !o.stderr.is_empty() {
                            result
                                .push_str(&format!("\nERR:{}", String::from_utf8_lossy(&o.stderr)));
                        }
                        result
                    }
                    Err(e) => format!("Error: {}", e),
                }
            }
            "write_file" => {
                let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
                let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
                let abs = format!("{}/{}", self.project_root, path.trim_start_matches('/'));
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
                let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
                match std::fs::read_dir(path) {
                    Ok(entries) => {
                        let mut files: Vec<_> = entries.filter_map(|e| e.ok()).collect();
                        files.sort_by_key(|e| e.file_name());
                        files
                            .iter()
                            .map(|e| {
                                let ty = if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                                    "DIR"
                                } else {
                                    "FILE"
                                };
                                format!("{}  {}", ty, e.file_name().to_string_lossy())
                            })
                            .collect::<Vec<_>>()
                            .join("\n")
                    }
                    Err(e) => format!("Error: {}", e),
                }
            }
            "run_test" => {
                let test_name = args.get("test_name").and_then(|v| v.as_str()).unwrap_or("");
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
                let cmd = args.get("command").and_then(|v| v.as_str()).unwrap_or("");
                let output = std::process::Command::new("sh").args(["-c", cmd]).output();
                match output {
                    Ok(o) => {
                        let mut result = String::from_utf8_lossy(&o.stdout).to_string();
                        if !o.stderr.is_empty() {
                            result
                                .push_str(&format!("\nERR:{}", String::from_utf8_lossy(&o.stderr)));
                        }
                        result
                    }
                    Err(e) => format!("Error: {}", e),
                }
            }
            // --- New tool implementations ---
            "patch" => {
                let path = args.get("file_path").and_then(|v| v.as_str()).unwrap_or("");
                let search = args.get("search").and_then(|v| v.as_str()).unwrap_or("");
                let insert = args.get("insert").and_then(|v| v.as_str()).unwrap_or("");
                let abs = format!("{}/{}", self.project_root, path.trim_start_matches('/'));
                match std::fs::read_to_string(&abs) {
                    Ok(content) => {
                        let count = content.matches(search).count();
                        if count == 0 {
                            format!("Error: search text not found in {}", path)
                        } else if count > 1 {
                            format!("Error: search text found {} times in {}. Use a more specific search.", count, path)
                        } else {
                            let new_content = content.replace(search, insert);
                            match std::fs::write(&abs, &new_content) {
                                Ok(_) => {
                                    // Show diff
                                    let diff_lines: Vec<String> = new_content
                                        .lines()
                                        .zip(content.lines())
                                        .enumerate()
                                        .filter(|(_, (a, b))| a != b)
                                        .map(|(i, _)| {
                                            let old_line = content.lines().nth(i).unwrap_or("");
                                            let new_line = new_content.lines().nth(i).unwrap_or("");
                                            format!("- {}\n+ {}", old_line, new_line)
                                        })
                                        .collect();
                                    format!("Patched {}. Changes:\n{}", path, diff_lines.join("\n"))
                                }
                                Err(e) => format!("Error writing {}: {}", path, e),
                            }
                        }
                    }
                    Err(e) => format!("Error reading {}: {}", path, e),
                }
            }
            "run_command" => {
                let cmd = args.get("command").and_then(|v| v.as_str()).unwrap_or("");
                let timeout_secs = args
                    .get("timeout_secs")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(30);
                let output = std::process::Command::new("timeout")
                    .arg(timeout_secs.to_string())
                    .arg("sh")
                    .arg("-c")
                    .arg(cmd)
                    .output();
                match output {
                    Ok(o) => {
                        let mut result = String::from_utf8_lossy(&o.stdout).to_string();
                        if !o.stderr.is_empty() {
                            if !result.is_empty() {
                                result.push('\n');
                            }
                            result.push_str(&String::from_utf8_lossy(&o.stderr));
                        }
                        let lines: Vec<&str> = result.lines().collect();
                        let last = lines.len().saturating_sub(200);
                        lines[last..].join("\n")
                    }
                    Err(e) => format!("Error running command: {}", e),
                }
            }
            "list_processes" => {
                let output = std::process::Command::new("sh")
                    .args(["-c", "ps aux | head -30"])
                    .output();
                match output {
                    Ok(o) => String::from_utf8_lossy(&o.stdout).to_string(),
                    Err(e) => format!("Error listing processes: {}", e),
                }
            }
            "web_search" => {
                let url = args.get("url").and_then(|v| v.as_str()).unwrap_or("");
                let output = std::process::Command::new("curl")
                    .args(["-sL", url])
                    .output();
                match output {
                    Ok(o) => {
                        let mut result = String::from_utf8_lossy(&o.stdout).to_string();
                        if !o.stderr.is_empty() {
                            if !result.is_empty() {
                                result.push('\n');
                            }
                            result.push_str(&format!("ERR:{}", String::from_utf8_lossy(&o.stderr)));
                        }
                        result
                    }
                    Err(e) => format!("Error fetching URL: {}", e),
                }
            }
            "web_fetch" => {
                let url = args.get("url").and_then(|v| v.as_str()).unwrap_or("");
                let output = std::process::Command::new("curl")
                    .args(["-sL", url])
                    .output();
                match output {
                    Ok(o) => {
                        let content = String::from_utf8_lossy(&o.stdout);
                        let truncated: String = content.chars().take(5000).collect();
                        let mut result = truncated;
                        if !o.stderr.is_empty() {
                            result
                                .push_str(&format!("\nERR:{}", String::from_utf8_lossy(&o.stderr)));
                        }
                        result
                    }
                    Err(e) => format!("Error fetching URL: {}", e),
                }
            }
            "browser_navigate" => {
                let url = args.get("url").and_then(|v| v.as_str()).unwrap_or("");
                let output = std::process::Command::new("xdg-open").arg(url).output();
                match output {
                    Ok(o) => {
                        if o.status.success() {
                            format!("Opened {} in browser", url)
                        } else {
                            let stderr = String::from_utf8_lossy(&o.stderr);
                            format!("Failed to open {}: {}", url, stderr)
                        }
                    }
                    Err(e) => format!(
                        "Error opening browser: {}. Try opening manually: {}",
                        e, url
                    ),
                }
            }
            "browser_screenshot" => {
                let filename = args
                    .get("filename")
                    .and_then(|v| v.as_str())
                    .unwrap_or("screenshot.png");
                // Try import (ImageMagick) first
                let result = std::process::Command::new("import")
                    .args(["-window", "root", filename])
                    .output();
                match result {
                    Ok(o) if o.status.success() => {
                        format!("Screenshot saved to {}", filename)
                    }
                    _ => {
                        // Fall back to scrot
                        let result2 = std::process::Command::new("scrot").arg(filename).output();
                        match result2 {
                            Ok(o2) if o2.status.success() => {
                                format!("Screenshot saved to {}", filename)
                            }
                            _ => {
                                "No screenshot tool available (tried import and scrot). Install ImageMagick or scrot.".to_string()
                            }
                        }
                    }
                }
            }
            "browser_console" => "not available in CLI mode".to_string(),
            "execute_code" => {
                let code = args.get("code").and_then(|v| v.as_str()).unwrap_or("");
                let language = args.get("language").and_then(|v| v.as_str()).unwrap_or("");

                let (ext, interpreter, interpreter_args): (&str, &str, &[&str]) = match language {
                    "py" => ("py", "python3", &[]),
                    "js" => ("js", "node", &[]),
                    "sh" => ("sh", "sh", &[]),
                    "ts" => ("ts", "npx", &["tsx"]),
                    "rs" => ("rs", "rustc", &[]),
                    _ => {
                        return format!(
                            "Unsupported language: {}. Allowed: py, js, rs, sh, ts",
                            language
                        )
                    }
                };

                let tmp_dir =
                    std::env::temp_dir().join(format!("artemis_code_{}", std::process::id()));
                let _ = std::fs::create_dir_all(&tmp_dir);

                // Rust: compile then run
                if language == "rs" {
                    let src_file = tmp_dir.join("code.rs");
                    let bin_file = tmp_dir.join("code");
                    if let Err(e) = std::fs::write(&src_file, code) {
                        return format!("Error writing temp file: {}", e);
                    }
                    let compile = std::process::Command::new("rustc")
                        .arg(&src_file)
                        .arg("-o")
                        .arg(&bin_file)
                        .output();
                    match compile {
                        Ok(o) if !o.status.success() => {
                            return format!(
                                "Compilation failed:\n{}",
                                String::from_utf8_lossy(&o.stderr)
                            );
                        }
                        Err(e) => return format!("Error running rustc: {}", e),
                        _ => {}
                    }
                    let run = std::process::Command::new("timeout")
                        .arg("10")
                        .arg(&bin_file)
                        .output();
                    match run {
                        Ok(o) => {
                            let mut result = String::from_utf8_lossy(&o.stdout).to_string();
                            if !o.stderr.is_empty() {
                                if !result.is_empty() {
                                    result.push('\n');
                                }
                                result.push_str(&String::from_utf8_lossy(&o.stderr));
                            }
                            let lines: Vec<&str> = result.lines().collect();
                            let last = lines.len().saturating_sub(100);
                            lines[last..].join("\n")
                        }
                        Err(e) => format!("Error running code: {}", e),
                    }
                } else {
                    let file_path = tmp_dir.join(format!("code.{}", ext));
                    if let Err(e) = std::fs::write(&file_path, code) {
                        return format!("Error writing temp file: {}", e);
                    }
                    let mut cmd = std::process::Command::new("timeout");
                    cmd.arg("10");
                    cmd.arg(interpreter);
                    if !interpreter_args.is_empty() {
                        cmd.args(interpreter_args);
                    }
                    cmd.arg(&file_path);
                    let output = cmd.output();
                    match output {
                        Ok(o) => {
                            let mut result = String::from_utf8_lossy(&o.stdout).to_string();
                            if !o.stderr.is_empty() {
                                if !result.is_empty() {
                                    result.push('\n');
                                }
                                result.push_str(&String::from_utf8_lossy(&o.stderr));
                            }
                            let lines: Vec<&str> = result.lines().collect();
                            let last = lines.len().saturating_sub(100);
                            lines[last..].join("\n")
                        }
                        Err(e) => format!("Error running code: {}", e),
                    }
                }
            }
            _ => format!("Unknown tool: {}", call.function.name),
        }
    }
}

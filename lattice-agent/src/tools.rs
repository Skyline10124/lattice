//! Default tool executor — executes tool calls against the local filesystem.
//!
//! Tool definitions (the schemas that tell the LLM what tools are available)
//! live in [`crate::tool_definitions`]. This module provides the execution
//! layer that runs those tools when the model requests them.

use async_trait::async_trait;

use crate::sandbox::SandboxConfig;
use crate::tool_error::ToolError;
use crate::ToolExecutor;

/// Executes tools using the local filesystem and shell.
///
/// Supports: read_file, grep, write_file, list_directory, bash, patch,
/// web_search. The `project_root` is used by `write_file` and `patch`
/// to restrict writes to project source directories.
///
/// All tool operations are gated by the `sandbox` configuration
/// (path validation, sensitive-file blocking, command allowlisting,
/// URL scheme restrictions, and size/timeout limits).
pub struct DefaultToolExecutor {
    pub project_root: String,
    pub sandbox: SandboxConfig,
    pub http_client: reqwest::Client,
}

impl DefaultToolExecutor {
    pub fn new(project_root: &str) -> Self {
        Self {
            project_root: project_root.to_string(),
            sandbox: SandboxConfig::default(),
            http_client: reqwest::Client::new(),
        }
    }

    /// Override the sandbox config (replaces the default).
    pub fn with_sandbox(mut self, config: SandboxConfig) -> Self {
        self.sandbox = config;
        self
    }
}

#[async_trait]
impl ToolExecutor for DefaultToolExecutor {
    async fn execute(&self, call: &lattice_core::types::ToolCall) -> String {
        let args: serde_json::Value =
            serde_json::from_str(&call.function.arguments).unwrap_or(serde_json::Value::Null);

        match call.function.name.as_str() {
            "read_file" => {
                let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
                if let Err(e) = self.sandbox.check_read(path) {
                    return e;
                }
                match tokio::fs::metadata(path).await {
                    Ok(meta) if meta.len() > self.sandbox.max_read_size as u64 => {
                        return format!(
                            "Sandbox: file size {} exceeds max_read_size {}",
                            meta.len(),
                            self.sandbox.max_read_size
                        );
                    }
                    Err(e) => {
                        return ToolError::IoError {
                            path: path.to_string(),
                            error: e,
                        }
                        .to_string()
                    }
                    _ => {}
                }
                tokio::fs::read_to_string(path)
                    .await
                    .unwrap_or_else(|e| {
                        ToolError::IoError {
                            path: path.to_string(),
                            error: e,
                        }
                        .to_string()
                    })
            }
            "grep" => {
                let pattern = args.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
                let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
                if let Err(e) = self.sandbox.check_read(path) {
                    return e;
                }
                let output = std::process::Command::new("grep")
                    .args(["-rn", pattern, path])
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
                if let Err(e) = self.sandbox.check_write(path) {
                    return e;
                }
                if content.len() > self.sandbox.max_write_size {
                    return ToolError::SizeLimit {
                        limit: self.sandbox.max_write_size,
                        actual: content.len(),
                    }
                    .to_string();
                }
                match tokio::fs::write(&abs, content).await {
                    Ok(_) => format!("Wrote {} bytes to {}", content.len(), path),
                    Err(e) => ToolError::IoError {
                        path: abs,
                        error: e,
                    }
                    .to_string(),
                }
            }
            "list_directory" => {
                let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
                if let Err(e) = self.sandbox.check_read(path) {
                    return e;
                }
                let mut entries = match tokio::fs::read_dir(path).await {
                    Ok(dir) => dir,
                    Err(e) => {
                        return ToolError::IoError {
                            path: path.to_string(),
                            error: e,
                        }
                        .to_string()
                    }
                };
                let mut files = Vec::new();
                loop {
                    match entries.next_entry().await {
                        Ok(Some(entry)) => {
                            let ty =
                                if entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false) {
                                    "DIR"
                                } else {
                                    "FILE"
                                };
                            files.push(format!(
                                "{}  {}",
                                ty,
                                entry.file_name().to_string_lossy()
                            ));
                        }
                        Ok(None) => break,
                        Err(e) => {
                            // Log but continue on individual entry errors
                            files.push(format!("Error reading entry: {}", e));
                        }
                    }
                }
                files.sort();
                files.join("\n")
            }
            "bash" => {
                let cmd = args.get("command").and_then(|v| v.as_str()).unwrap_or("");
                if let Err(e) = self.sandbox.check_command(cmd) {
                    return e;
                }
                let output = tokio::process::Command::new("sh")
                    .args(["-c", cmd])
                    .output()
                    .await;
                match output {
                    Ok(o) => {
                        let mut result = String::from_utf8_lossy(&o.stdout).to_string();
                        if !o.stderr.is_empty() {
                            result.push_str(&format!("\nERR:{}", String::from_utf8_lossy(&o.stderr)));
                        }
                        result
                    }
                    Err(e) => ToolError::CommandError(e.to_string()).to_string(),
                }
            }
            "patch" => {
                let path = args.get("file_path").and_then(|v| v.as_str()).unwrap_or("");
                let search = args.get("search").and_then(|v| v.as_str()).unwrap_or("");
                let insert = args.get("insert").and_then(|v| v.as_str()).unwrap_or("");
                if let Err(e) = self.sandbox.check_write(path) {
                    return e;
                }
                let abs = format!("{}/{}", self.project_root, path.trim_start_matches('/'));
                match tokio::fs::read_to_string(&abs).await {
                    Ok(content) => {
                        let count = content.matches(search).count();
                        if count == 0 {
                            format!("Error: search text not found in {}", path)
                        } else if count > 1 {
                            format!(
                                "Error: search text found {} times in {}. Use a more specific search.",
                                count, path
                            )
                        } else {
                            let new_content = content.replace(search, insert);
                            match tokio::fs::write(&abs, &new_content).await {
                                Ok(_) => {
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
                                Err(e) => ToolError::IoError { path: abs, error: e }.to_string(),
                            }
                        }
                    }
                    Err(e) => ToolError::IoError { path: abs, error: e }.to_string(),
                }
            }
            "web_search" => {
                let url = args.get("url").and_then(|v| v.as_str()).unwrap_or("");
                if let Err(e) = self.sandbox.check_url(url) {
                    return e;
                }
                match self.http_client.get(url).send().await {
                    Ok(response) => response
                        .text()
                        .await
                        .unwrap_or_else(|e| ToolError::HttpError(e.to_string()).to_string()),
                    Err(e) => ToolError::HttpError(e.to_string()).to_string(),
                }
            }
            _ => format!("Unknown tool: {}", call.function.name),
        }
    }
}

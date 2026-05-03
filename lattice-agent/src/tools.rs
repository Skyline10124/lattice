//! Default tool executor — executes tool calls against the local filesystem.
//!
//! Tool definitions (the schemas that tell the LLM what tools are available)
//! live in [`crate::tool_definitions`]. This module provides the execution
//! layer that runs those tools when the model requests them.

use std::collections::HashSet;
use std::path::Path;

use async_trait::async_trait;

use crate::sandbox::SandboxConfig;
use crate::tool_error::ToolError;
use crate::ToolExecutor;

/// Trait abstracting tool registry access. lattice-harness::tools::ToolRegistry implements this.
/// This abstraction lets DefaultToolExecutor (in lattice-agent) use ToolRegistry (in lattice-harness)
/// without a direct crate dependency.
pub trait RegistryToolAccess: Send + Sync {
    fn get_handler(
        &self,
        tool_name: &str,
    ) -> Option<std::sync::Arc<dyn Fn(serde_json::Value) -> String + Send + Sync>>;
    fn get_definitions(&self) -> Vec<lattice_core::types::ToolDefinition>;
}

/// Executes tools using the local filesystem and shell.
///
/// Supports: read_file, grep, write_file, list_directory, bash, patch,
/// web_search. The `project_root` is used by `write_file` and `patch`
/// to restrict writes to project source directories.
///
/// All tool operations are gated by the `sandbox` configuration
/// (path validation, sensitive-file blocking, command allowlisting,
/// URL scheme restrictions, and size/timeout limits).
///
/// When a `registry` is set, custom tool handlers from the registry are
/// checked first before falling back to the default 7 tools.
pub struct DefaultToolExecutor {
    pub project_root: String,
    pub sandbox: SandboxConfig,
    pub http_client: reqwest::Client,
    pub registry: Option<std::sync::Arc<dyn RegistryToolAccess>>,
}

impl DefaultToolExecutor {
    pub fn new(project_root: &str) -> Self {
        Self {
            project_root: project_root.to_string(),
            sandbox: SandboxConfig::default(),
            http_client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("reqwest client"),
            registry: None,
        }
    }

    pub fn new_with_registry(
        project_root: &str,
        registry: Option<std::sync::Arc<dyn RegistryToolAccess>>,
    ) -> Self {
        Self {
            project_root: project_root.to_string(),
            sandbox: SandboxConfig::default(),
            http_client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("reqwest client"),
            registry,
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

        // Check registry for custom tools first
        if let Some(reg) = &self.registry {
            if let Some(handler) = reg.get_handler(&call.function.name) {
                return handler(args);
            }
        }

        match call.function.name.as_str() {
            "read_file" => {
                let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
                // Canonicalize path to resolve symlinks (TOCTOU prevention)
                let canonical = match tokio::fs::canonicalize(path).await {
                    Ok(p) => p,
                    Err(e) => {
                        return ToolError::IoError {
                            path: path.to_string(),
                            error: e,
                        }
                        .to_string()
                    }
                };
                let path_str = canonical.to_string_lossy();
                if let Err(e) = self.sandbox.check_read(&path_str) {
                    return e;
                }
                match tokio::fs::metadata(&canonical).await {
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
                tokio::fs::read_to_string(&canonical)
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

                let re = match regex::Regex::new(pattern) {
                    Ok(r) => r,
                    Err(e) => return ToolError::RegexError(e.to_string()).to_string(),
                };

                let mut results = Vec::new();
                let mut visited = HashSet::new();
                grep_recursive(
                    &re,
                    Path::new(path),
                    &mut results,
                    &self.sandbox,
                    0,
                    &mut visited,
                )
                .await;

                if results.is_empty() {
                    "(no matches)".to_string()
                } else {
                    results.join("\n")
                }
            }
            "write_file" => {
                let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
                let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
                let abs = std::path::PathBuf::from(format!(
                    "{}/{}",
                    self.project_root,
                    path.trim_start_matches('/')
                ));
                // Canonicalize parent directory (TOCTOU prevention)
                let write_path = if let Some(parent) = abs.parent() {
                    match tokio::fs::canonicalize(parent).await {
                        Ok(canonical_parent) => {
                            let canonical = canonical_parent.join(abs.file_name().unwrap());
                            let path_str = canonical.to_string_lossy();
                            if let Err(e) = self.sandbox.check_write(&path_str) {
                                return e;
                            }
                            canonical
                        }
                        Err(_) => {
                            // Parent doesn't exist — check original path
                            if let Err(e) = self.sandbox.check_write(path) {
                                return e;
                            }
                            abs.clone()
                        }
                    }
                } else {
                    if let Err(e) = self.sandbox.check_write(path) {
                        return e;
                    }
                    abs.clone()
                };
                if content.len() > self.sandbox.max_write_size {
                    return ToolError::SizeLimit {
                        limit: self.sandbox.max_write_size,
                        actual: content.len(),
                    }
                    .to_string();
                }
                let path_str = write_path.to_string_lossy().to_string();
                match tokio::fs::write(&write_path, content).await {
                    Ok(_) => format!("Wrote {} bytes to {}", content.len(), path),
                    Err(e) => ToolError::IoError {
                        path: path_str,
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
                            let ty = if entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false)
                            {
                                "DIR"
                            } else {
                                "FILE"
                            };
                            files.push(format!("{}  {}", ty, entry.file_name().to_string_lossy()));
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
                let result = tokio::time::timeout(
                    tokio::time::Duration::from_secs(std::cmp::max(
                        self.sandbox.max_command_timeout,
                        1,
                    ) as u64),
                    tokio::process::Command::new("sh")
                        .args(["-c", cmd])
                        .output(),
                )
                .await;
                match result {
                    Ok(Ok(o)) => {
                        let mut cmd_result = String::from_utf8_lossy(&o.stdout).to_string();
                        if !o.stderr.is_empty() {
                            cmd_result
                                .push_str(&format!("\nERR:{}", String::from_utf8_lossy(&o.stderr)));
                        }
                        cmd_result
                    }
                    Ok(Err(e)) => ToolError::CommandError(e.to_string()).to_string(),
                    Err(_) => ToolError::CommandError(format!(
                        "bash command timed out after {}s",
                        self.sandbox.max_command_timeout
                    ))
                    .to_string(),
                }
            }
            "patch" => {
                let path = args.get("file_path").and_then(|v| v.as_str()).unwrap_or("");
                let search = args.get("search").and_then(|v| v.as_str()).unwrap_or("");
                let insert = args.get("insert").and_then(|v| v.as_str()).unwrap_or("");
                let abs = std::path::PathBuf::from(format!(
                    "{}/{}",
                    self.project_root,
                    path.trim_start_matches('/')
                ));
                // Canonicalize path for TOCTOU prevention
                let abs = match tokio::fs::canonicalize(&abs).await {
                    Ok(canonical) => {
                        let path_str = canonical.to_string_lossy();
                        if let Err(e) = self.sandbox.check_write(&path_str) {
                            return e;
                        }
                        canonical
                    }
                    Err(_) => {
                        if let Err(e) = self.sandbox.check_write(path) {
                            return e;
                        }
                        abs
                    }
                };
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
                                Err(e) => ToolError::IoError {
                                    path: abs.to_string_lossy().to_string(),
                                    error: e,
                                }
                                .to_string(),
                            }
                        }
                    }
                    Err(e) => ToolError::IoError {
                        path: abs.to_string_lossy().to_string(),
                        error: e,
                    }
                    .to_string(),
                }
            }
            "web_search" => {
                let url = args.get("url").and_then(|v| v.as_str()).unwrap_or("");
                if let Err(e) = self.sandbox.check_url(url) {
                    return e;
                }
                match self.http_client.get(url).send().await {
                    Ok(response) => {
                        let limit = self.sandbox.max_http_response_size;
                        if let Some(content_length) = response.content_length() {
                            if content_length > limit as u64 {
                                return ToolError::HttpError(format!(
                                    "Response too large: {} bytes (max {})",
                                    content_length, limit
                                ))
                                .to_string();
                            }
                        }
                        let bytes = response.bytes().await.unwrap_or_default();
                        if bytes.len() > limit {
                            let truncated = String::from_utf8_lossy(&bytes[..limit]);
                            format!(
                                "{}[TRUNCATED: response exceeded {} bytes]",
                                truncated.trim_end(),
                                limit
                            )
                        } else {
                            String::from_utf8_lossy(&bytes).to_string()
                        }
                    }
                    Err(e) => ToolError::HttpError(e.to_string()).to_string(),
                }
            }
            _ => format!("Unknown tool: {}", call.function.name),
        }
    }
}

const GREP_MAX_DEPTH: u32 = 32;

/// Recursively search files under `path` for lines matching `pattern`.
/// Respects sandbox limits: max_depth, max_read_size, check_read.
/// Skips hidden dirs, binary files, and follows symlinks with cycle detection.
async fn grep_recursive(
    pattern: &regex::Regex,
    path: &Path,
    results: &mut Vec<String>,
    sandbox: &crate::sandbox::SandboxConfig,
    depth: u32,
    visited: &mut HashSet<std::path::PathBuf>,
) {
    if depth > GREP_MAX_DEPTH {
        return;
    }

    // Resolve symlinks to detect cycles AND use canonical path for sandbox checks
    let resolved = match tokio::fs::canonicalize(path).await {
        Ok(p) => p,
        Err(_) => return,
    };
    if !visited.insert(resolved.clone()) {
        return; // symlink cycle
    }

    let path_str = resolved.to_string_lossy();
    if sandbox.check_read(&path_str).is_err() {
        return;
    }

    if resolved.is_file() {
        // Skip files too large
        if let Ok(meta) = tokio::fs::metadata(&resolved).await {
            if meta.len() > sandbox.max_read_size as u64 {
                return;
            }
        }

        if let Ok(content) = tokio::fs::read_to_string(&resolved).await {
            // Skip binary files
            if content.contains('\0') {
                return;
            }
            for (line_num, line) in content.lines().enumerate() {
                if pattern.is_match(line) {
                    results.push(format!("{}:{}:{}", path_str, line_num + 1, line));
                }
            }
        }
    } else if resolved.is_dir() {
        let mut entries = match tokio::fs::read_dir(&resolved).await {
            Ok(d) => d,
            Err(_) => return,
        };
        let mut children = Vec::new();
        while let Ok(Some(entry)) = entries.next_entry().await {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            // Skip hidden directories (but not . and ..)
            if name_str.starts_with('.') && name_str != "." && name_str != ".." {
                continue;
            }
            children.push(entry.path());
        }
        for child_path in children {
            Box::pin(grep_recursive(
                pattern,
                &child_path,
                results,
                sandbox,
                depth + 1,
                visited,
            ))
            .await;
        }
    }
}

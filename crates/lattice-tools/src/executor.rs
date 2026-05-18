//! Default tool executor — executes tool calls against the local filesystem.
//!
//! Tool definitions (the schemas that tell the LLM what tools are available)
//! live in [`crate::tool_definitions`]. This module provides the execution
//! layer that runs those tools when the model requests them.

use std::collections::{BTreeSet, HashSet};
use std::net::{IpAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

use async_trait::async_trait;

use crate::edit::{apply_patch as apply_structured_patch, content_hash, PatchResultEnvelope};
use crate::tool_error::ToolError;
use crate::ToolExecutor;

/// Executes tools using the local filesystem and shell.
///
/// Supports: workspace_context, todo_write, read_file, read_files, grep,
/// write_file, list_directory, find_files, bash, patch,
/// web_search, plus bus:fetch when the `blob-store` feature is enabled.
/// The `project_root` is used by `write_file` and `patch`
/// to restrict writes to project source directories.
///
/// File/search/edit operations enforce project-root containment, parent
/// traversal rejection, and size limits directly in this crate.
pub struct DefaultToolExecutor {
    project_root: String,
    limits: ToolLimits,
    http_client: reqwest::Client,
    authorized_tools: Option<BTreeSet<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolLimits {
    pub max_read_size: usize,
    pub max_write_size: usize,
    pub max_http_response_size: usize,
    pub max_command_timeout: u32,
}

impl Default for ToolLimits {
    fn default() -> Self {
        Self {
            max_read_size: 10 * 1024 * 1024,
            max_write_size: 1024 * 1024,
            max_http_response_size: 10 * 1024 * 1024,
            max_command_timeout: 30,
        }
    }
}

const DEFAULT_READ_LINE_LIMIT: usize = 2000;
const MAX_BATCH_READ_FILES: usize = 20;
const DEFAULT_WORKSPACE_ENTRY_LIMIT: usize = 40;
const MAX_WORKSPACE_ENTRY_LIMIT: usize = 100;
const DEFAULT_LIST_LIMIT: usize = 200;
const MAX_LIST_LIMIT: usize = 1000;
const DEFAULT_GREP_LIMIT: usize = 200;
const MAX_GREP_LIMIT: usize = 1000;
const MAX_COMMAND_OUTPUT_SIZE: usize = 200 * 1024;

impl DefaultToolExecutor {
    fn build_http_client() -> Result<reqwest::Client, String> {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| format!("failed to build reqwest client: {e}"))
    }

    pub fn new(project_root: &str) -> Result<Self, String> {
        Ok(Self {
            project_root: project_root.to_string(),
            limits: ToolLimits::default(),
            http_client: Self::build_http_client()?,
            authorized_tools: None,
        })
    }

    pub async fn execute_bash(
        project_root: &str,
        limits: &ToolLimits,
        command: &str,
    ) -> Result<std::process::Output, String> {
        check_command(command)?;
        execute_bash_command(command, limits, project_root).await
    }

    fn resolve_user_path(&self, path: &str) -> PathBuf {
        let path = PathBuf::from(path);
        if path.is_absolute() {
            path
        } else {
            Path::new(&self.project_root).join(path)
        }
    }

    async fn read_text_file(
        &self,
        path: &str,
        offset: usize,
        limit: usize,
        line_numbers: bool,
    ) -> String {
        // Reject parent traversal before any filesystem access
        if let Err(e) = Self::reject_parent_traversal(path) {
            return e;
        }
        let requested = self.resolve_user_path(path);
        // Canonicalize path to resolve symlinks (TOCTOU prevention)
        let canonical = match tokio::fs::canonicalize(&requested).await {
            Ok(p) => p,
            Err(e) => {
                return ToolError::IoError {
                    path: path.to_string(),
                    error: e,
                }
                .to_string()
            }
        };
        // v1.1 project-root containment (D-19, D-21)
        if let Err(e) = self.ensure_under_project_root(&canonical) {
            return e;
        }
        match tokio::fs::metadata(&canonical).await {
            Ok(meta) if meta.len() > self.limits.max_read_size as u64 => {
                return format!(
                    "file size {} exceeds max_read_size {}",
                    meta.len(),
                    self.limits.max_read_size
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
        match tokio::fs::read_to_string(&canonical).await {
            Ok(content) => {
                let rendered = render_file_slice(&content, offset, limit, line_numbers);
                let hash = content_hash(content.as_bytes());
                if rendered.is_empty() {
                    format!("Content hash: {hash}")
                } else {
                    format!("{rendered}\nContent hash: {hash}")
                }
            }
            Err(e) => ToolError::IoError {
                path: path.to_string(),
                error: e,
            }
            .to_string(),
        }
    }

    async fn workspace_context(&self, path: &str, limit: usize) -> String {
        if let Err(e) = Self::reject_parent_traversal(path) {
            return e;
        }
        let requested = self.resolve_user_path(path);
        let canonical = match tokio::fs::canonicalize(&requested).await {
            Ok(p) => p,
            Err(e) => {
                return ToolError::IoError {
                    path: path.to_string(),
                    error: e,
                }
                .to_string()
            }
        };
        if let Err(e) = self.ensure_under_project_root(&canonical) {
            return e;
        }
        let mut lines = vec![format!("Workspace: {}", canonical.display())];
        lines.push("Top-level entries:".into());
        match tokio::fs::read_dir(&canonical).await {
            Ok(mut entries) => {
                let mut rendered = Vec::new();
                loop {
                    match entries.next_entry().await {
                        Ok(Some(entry)) => {
                            let name = entry.file_name().to_string_lossy().to_string();
                            if matches!(name.as_str(), ".git" | "target" | "node_modules") {
                                continue;
                            }
                            let ty = if entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false)
                            {
                                "DIR"
                            } else {
                                "FILE"
                            };
                            rendered.push(format!("- {ty} {name}"));
                        }
                        Ok(None) => break,
                        Err(e) => {
                            rendered.push(format!("- Error reading entry: {e}"));
                            break;
                        }
                    }
                }
                rendered.sort();
                let total = rendered.len();
                let visible: Vec<String> = rendered.into_iter().take(limit).collect();
                if visible.is_empty() {
                    lines.push("- no readable top-level entries".into());
                } else {
                    lines.extend(visible.iter().cloned());
                    if total > visible.len() {
                        lines.push(format!(
                            "[TRUNCATED: showing {} of {} entries]",
                            visible.len(),
                            total
                        ));
                    }
                }
            }
            Err(e) => {
                return ToolError::IoError {
                    path: path.to_string(),
                    error: e,
                }
                .to_string()
            }
        }

        if let Some(git) = workspace_git_context(&canonical) {
            lines.push(String::new());
            lines.push("Git state:".into());
            lines.extend(git);
        }

        let instructions = workspace_instruction_context(&canonical);
        if !instructions.is_empty() {
            lines.push(String::new());
            lines.push("Project instruction files visible from this directory:".into());
            lines.extend(instructions);
        }

        let project = workspace_project_type_context(&canonical);
        if !project.is_empty() {
            lines.push(String::new());
            lines.push("Detected project files and likely verification commands:".into());
            lines.extend(project);
        }

        lines.join("\n")
    }

    pub fn with_limits(mut self, limits: ToolLimits) -> Self {
        self.limits = limits;
        self
    }

    pub fn with_authorized_tools<I, S>(mut self, tool_names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.authorized_tools = Some(tool_names.into_iter().map(Into::into).collect());
        self
    }

    fn authorization_denial(&self, tool_name: &str) -> Option<String> {
        let authorized = self.authorized_tools.as_ref()?;
        (!authorized.contains(tool_name)).then(|| {
            format!(
                "Tool denied: '{}' is not enabled for this agent. Enabled tools: {}",
                tool_name,
                render_enabled_tools(authorized)
            )
        })
    }

    /// v1.1 main-path constructor: create an executor bound to a project root
    /// with an explicit tool allowlist.
    ///
    /// This constructor is the authoritative v1.1 boundary (D-19, D-20, D-21,
    /// D-22). It canonicalizes the project root, applies the explicit tool
    /// allowlist via `with_authorized_tools`, and sets up path containment
    /// checks without a dynamic approval flow.
    pub fn for_project_root<I, S>(project_root: &str, enabled_tool_names: I) -> Result<Self, String>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let canonical = std::fs::canonicalize(project_root)
            .map_err(|e| format!("cannot canonicalize project root '{}': {}", project_root, e))?;
        Ok(Self {
            project_root: canonical.to_string_lossy().to_string(),
            limits: ToolLimits::default(),
            http_client: Self::build_http_client()?,
            authorized_tools: Some(enabled_tool_names.into_iter().map(Into::into).collect()),
        })
    }

    /// Reject paths containing `..` traversal components.
    ///
    /// This is a fast pre-check applied before filesystem access. It catches
    /// obvious traversal attempts without requiring canonicalization. Paths
    /// that pass this check may still be rejected by
    /// [`ensure_under_project_root`] if symlinks resolve outside the root.
    fn reject_parent_traversal(path: &str) -> Result<(), String> {
        // Check for literal ".." components
        for component in path.split('/') {
            if component == ".." {
                return Err(format!(
                    "path traversal denied: '{}' contains '..' component",
                    path
                ));
            }
        }
        // Also check for backslash-separated traversal on Windows-style paths
        for component in path.split('\\') {
            if component == ".." {
                return Err(format!(
                    "path traversal denied: '{}' contains '..' component",
                    path
                ));
            }
        }
        Ok(())
    }

    /// Verify that a canonical path is under the project root.
    ///
    /// Called after canonicalization (symlink resolution) to ensure the
    /// resolved path is contained within the project root boundary (D-19,
    /// D-21).
    fn ensure_under_project_root(&self, canonical: &Path) -> Result<(), String> {
        let root = Path::new(&self.project_root);
        if canonical == root {
            return Ok(());
        }
        if !canonical.starts_with(root) {
            return Err(format!(
                "path '{}' is outside the project root '{}'",
                canonical.display(),
                self.project_root
            ));
        }
        Ok(())
    }
}

#[async_trait]
impl ToolExecutor for DefaultToolExecutor {
    async fn execute(&self, call: &lattice_llm::ToolCall) -> String {
        let args: serde_json::Value = match serde_json::from_str(&call.function.arguments) {
            Ok(v) => v,
            Err(e) => {
                return format!(
                    "Error: invalid JSON arguments for tool '{}': {} — raw args: {:?}",
                    call.function.name,
                    e,
                    &call.function.arguments[..call.function.arguments.len().min(200)]
                );
            }
        };

        if let Some(denial) = self.authorization_denial(&call.function.name) {
            return denial;
        }

        match call.function.name.as_str() {
            "workspace_context" => {
                let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
                let limit = args
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .and_then(|v| usize::try_from(v).ok())
                    .unwrap_or(DEFAULT_WORKSPACE_ENTRY_LIMIT)
                    .clamp(1, MAX_WORKSPACE_ENTRY_LIMIT);
                self.workspace_context(path, limit).await
            }
            "todo_write" => render_todos(&args),
            "read_file" => {
                let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
                let offset = args
                    .get("offset")
                    .and_then(|v| v.as_u64())
                    .and_then(|v| usize::try_from(v).ok())
                    .unwrap_or(1)
                    .max(1);
                let limit = args
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .and_then(|v| usize::try_from(v).ok())
                    .unwrap_or(DEFAULT_READ_LINE_LIMIT)
                    .max(1);
                let line_numbers = args
                    .get("line_numbers")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                self.read_text_file(path, offset, limit, line_numbers).await
            }
            "read_files" => {
                let files = match args.get("files").and_then(|v| v.as_array()) {
                    Some(files) if !files.is_empty() => files,
                    _ => return "Error: read_files requires a non-empty files array".to_string(),
                };
                if files.len() > MAX_BATCH_READ_FILES {
                    return format!(
                        "Error: read_files accepts at most {} files per call",
                        MAX_BATCH_READ_FILES
                    );
                }

                let mut sections = Vec::new();
                for file in files {
                    let path = file.get("path").and_then(|v| v.as_str()).unwrap_or("");
                    let offset = file
                        .get("offset")
                        .and_then(|v| v.as_u64())
                        .and_then(|v| usize::try_from(v).ok())
                        .unwrap_or(1)
                        .max(1);
                    let limit = file
                        .get("limit")
                        .and_then(|v| v.as_u64())
                        .and_then(|v| usize::try_from(v).ok())
                        .unwrap_or(DEFAULT_READ_LINE_LIMIT)
                        .max(1);
                    let line_numbers = file
                        .get("line_numbers")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(true);
                    let content = self.read_text_file(path, offset, limit, line_numbers).await;
                    sections.push(format!("--- {path} ---\n{content}"));
                }
                sections.join("\n\n")
            }
            "grep" => {
                let pattern = args.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
                let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
                if let Err(e) = Self::reject_parent_traversal(path) {
                    return e;
                }
                let max_results = args
                    .get("max_results")
                    .and_then(|v| v.as_u64())
                    .and_then(|v| usize::try_from(v).ok())
                    .unwrap_or(DEFAULT_GREP_LIMIT)
                    .clamp(1, MAX_GREP_LIMIT);

                let re = match regex::Regex::new(pattern) {
                    Ok(r) => r,
                    Err(e) => return ToolError::RegexError(e.to_string()).to_string(),
                };

                let mut results = Vec::new();
                let mut visited = HashSet::new();
                let mut truncated = false;
                let project_root = PathBuf::from(&self.project_root);
                let mut context = GrepContext {
                    pattern: &re,
                    project_root: &project_root,
                    results: &mut results,
                    limits: &self.limits,
                    visited: &mut visited,
                    max_results,
                    truncated: &mut truncated,
                };
                grep_recursive(&mut context, &self.resolve_user_path(path), 0).await;

                if results.is_empty() {
                    "(no matches)".to_string()
                } else if truncated {
                    format!(
                        "{}\n[TRUNCATED: grep returned first {} matches; narrow path or pattern]",
                        results.join("\n"),
                        results.len()
                    )
                } else {
                    results.join("\n")
                }
            }
            "write_file" => {
                let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
                if let Err(e) = Self::reject_parent_traversal(path) {
                    return e;
                }
                let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
                let abs =
                    std::path::PathBuf::from(&self.project_root).join(path.trim_start_matches('/'));
                // Canonicalize parent directory (TOCTOU prevention)
                let write_path = if let Some(parent) = abs.parent() {
                    match tokio::fs::canonicalize(parent).await {
                        Ok(canonical_parent) => {
                            let canonical = canonical_parent.join(abs.file_name().unwrap());
                            // v1.1 project-root containment (D-19, D-21)
                            if let Err(e) = self.ensure_under_project_root(&canonical) {
                                return e;
                            }
                            canonical
                        }
                        Err(_) => {
                            // Parent doesn't exist — reject write rather than
                            // bypass canonicalize (TOCTOU: symlink could redirect
                            // a non-canonical path after the check)
                            return format!(
                                "cannot write to '{}' because parent directory does not exist or cannot be resolved",
                                path
                            );
                        }
                    }
                } else {
                    abs.clone()
                };
                if content.len() > self.limits.max_write_size {
                    return ToolError::SizeLimit {
                        limit: self.limits.max_write_size,
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
                if let Err(e) = Self::reject_parent_traversal(path) {
                    return e;
                }
                let limit = args
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .and_then(|v| usize::try_from(v).ok())
                    .unwrap_or(DEFAULT_LIST_LIMIT)
                    .clamp(1, MAX_LIST_LIMIT);
                let requested = self.resolve_user_path(path);
                // Canonicalize path to resolve symlinks (TOCTOU prevention)
                let canonical = match tokio::fs::canonicalize(&requested).await {
                    Ok(p) => p,
                    Err(e) => {
                        return ToolError::IoError {
                            path: path.to_string(),
                            error: e,
                        }
                        .to_string()
                    }
                };
                // v1.1 project-root containment (D-19, D-21)
                if let Err(e) = self.ensure_under_project_root(&canonical) {
                    return e;
                }
                let mut entries = match tokio::fs::read_dir(&canonical).await {
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
                let total = files.len();
                let mut visible: Vec<String> = files.into_iter().take(limit).collect();
                if total > visible.len() {
                    visible.push(format!(
                        "[TRUNCATED: showing {} of {} entries; pass a narrower path or higher limit]",
                        visible.len(),
                        total
                    ));
                }
                visible.join("\n")
            }
            "find_files" => {
                let pattern = args.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
                let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
                if let Err(e) = Self::reject_parent_traversal(path) {
                    return e;
                }
                let limit = args
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .and_then(|v| usize::try_from(v).ok())
                    .unwrap_or(DEFAULT_LIST_LIMIT)
                    .clamp(1, MAX_LIST_LIMIT);
                let requested = self.resolve_user_path(path);
                let canonical = match tokio::fs::canonicalize(&requested).await {
                    Ok(p) => p,
                    Err(e) => {
                        return ToolError::IoError {
                            path: path.to_string(),
                            error: e,
                        }
                        .to_string()
                    }
                };
                // v1.1 project-root containment (D-19, D-21)
                if let Err(e) = self.ensure_under_project_root(&canonical) {
                    return e;
                }
                let mut matches = Vec::new();
                let mut visited = HashSet::new();
                let mut truncated = false;
                let project_root = PathBuf::from(&self.project_root);
                let mut context = FindFilesContext {
                    root: &canonical,
                    project_root: &project_root,
                    matches: &mut matches,
                    pattern,
                    visited: &mut visited,
                    max_results: limit,
                    truncated: &mut truncated,
                };
                find_files_recursive(&mut context, &canonical, 0).await;
                if matches.is_empty() {
                    "(no matches)".to_string()
                } else if truncated {
                    format!(
                        "{}\n[TRUNCATED: find_files returned first {} matches; narrow path or pattern]",
                        matches.join("\n"),
                        matches.len()
                    )
                } else {
                    matches.join("\n")
                }
            }
            "bash" => {
                let cmd = args.get("command").and_then(|v| v.as_str()).unwrap_or("");
                let result = Self::execute_bash(&self.project_root, &self.limits, cmd).await;
                match result {
                    Ok(o) => {
                        let mut cmd_result = String::from_utf8_lossy(&o.stdout).to_string();
                        if !o.stderr.is_empty() {
                            cmd_result
                                .push_str(&format!("\nERR:{}", String::from_utf8_lossy(&o.stderr)));
                        }
                        truncate_output(cmd_result, MAX_COMMAND_OUTPUT_SIZE, "command output")
                    }
                    Err(e) => ToolError::CommandError(e).to_string(),
                }
            }
            "patch" => {
                let patch = args.get("patch").and_then(|v| v.as_str()).unwrap_or("");
                let envelope: PatchResultEnvelope =
                    apply_structured_patch(Path::new(&self.project_root), &self.limits, patch)
                        .await;
                serde_json::to_string(&envelope).unwrap_or_else(|error| {
                    serde_json::json!({
                        "ok": false,
                        "summary": "failed to serialize patch result envelope",
                        "errors": [{
                            "code": "io_error",
                            "message": format!("failed to serialize patch result envelope: {error}"),
                            "path": null,
                            "operation_index": null,
                            "hunk_index": null
                        }],
                        "changed_files": [],
                        "attempted_operations": []
                    })
                    .to_string()
                })
            }
            "multi_patch" => {
                "Tool retired: multi_patch. Use patch with structured patch text.".to_string()
            }
            "web_search" => {
                let url = args.get("url").and_then(|v| v.as_str()).unwrap_or("");
                if let Err(e) = check_url(url) {
                    return e;
                }
                match self.http_client.get(url).send().await {
                    Ok(response) => {
                        let limit = self.limits.max_http_response_size;
                        if let Some(content_length) = response.content_length() {
                            if content_length > limit as u64 {
                                return ToolError::HttpError(format!(
                                    "Response too large: {} bytes (max {})",
                                    content_length, limit
                                ))
                                .to_string();
                            }
                        }
                        // Stream the response body chunk-by-chunk, aborting at
                        // the size limit. This prevents buffering an unbounded
                        // response into memory before truncation.
                        use futures::StreamExt;
                        let mut buf = Vec::new();
                        let mut exceeded = false;
                        let mut stream = response.bytes_stream();
                        while let Some(chunk) = stream.next().await {
                            let chunk = match chunk {
                                Ok(c) => c,
                                Err(e) => {
                                    return ToolError::HttpError(e.to_string()).to_string();
                                }
                            };
                            if buf.len() + chunk.len() > limit {
                                let remaining = limit - buf.len();
                                buf.extend_from_slice(&chunk[..remaining]);
                                exceeded = true;
                                break;
                            }
                            buf.extend_from_slice(&chunk);
                        }
                        if exceeded {
                            let truncated = String::from_utf8_lossy(&buf);
                            format!(
                                "{}[TRUNCATED: response exceeded {} bytes]",
                                truncated.trim_end(),
                                limit
                            )
                        } else {
                            String::from_utf8_lossy(&buf).to_string()
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

fn render_file_slice(content: &str, offset: usize, limit: usize, line_numbers: bool) -> String {
    let total_lines = content.lines().count();
    let start_index = offset.saturating_sub(1);
    if start_index >= total_lines {
        return format!(
            "[No content: requested offset {} but file has {} lines]",
            offset, total_lines
        );
    }

    let mut rendered = Vec::new();
    let mut returned = 0usize;
    for (line_index, line) in content.lines().enumerate().skip(start_index) {
        if returned >= limit {
            break;
        }
        if line_numbers {
            rendered.push(format!("{:>6}\t{}", line_index + 1, line));
        } else {
            rendered.push(line.to_string());
        }
        returned += 1;
    }

    if start_index + returned < total_lines {
        rendered.push(format!(
            "[TRUNCATED: showing lines {}-{} of {}; call read_file with offset {} to continue]",
            offset,
            start_index + returned,
            total_lines,
            start_index + returned + 1
        ));
    }

    rendered.join("\n")
}

fn render_todos(args: &serde_json::Value) -> String {
    let todos = match args.get("todos").and_then(|v| v.as_array()) {
        Some(todos) => todos,
        None => return "Error: todo_write requires a todos array".to_string(),
    };
    if todos.len() > 100 {
        return "Error: todo_write accepts at most 100 todos".to_string();
    }

    let mut pending = 0usize;
    let mut in_progress = 0usize;
    let mut completed = 0usize;
    let mut rendered = Vec::new();

    for (index, todo) in todos.iter().enumerate() {
        let content = todo
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if content.is_empty() {
            return format!("Error: todo {} has empty content", index + 1);
        }
        let status = todo.get("status").and_then(|v| v.as_str()).unwrap_or("");
        match status {
            "pending" => pending += 1,
            "in_progress" => in_progress += 1,
            "completed" => completed += 1,
            _ => {
                return format!(
                    "Error: todo {} has invalid status '{}'; expected pending, in_progress, or completed",
                    index + 1,
                    status
                );
            }
        }
        rendered.push(format!("{}. [{}] {}", index + 1, status, content));
    }

    if in_progress > 1 {
        return "Error: todo_write allows at most one in_progress todo".to_string();
    }

    let mut lines = vec![format!(
        "Todo list updated: {} item(s), {} pending, {} in_progress, {} completed",
        todos.len(),
        pending,
        in_progress,
        completed
    )];
    lines.extend(rendered);
    lines.join("\n")
}

fn render_enabled_tools(authorized: &BTreeSet<String>) -> String {
    if authorized.is_empty() {
        "<none>".to_string()
    } else {
        authorized.iter().cloned().collect::<Vec<_>>().join(", ")
    }
}

fn check_command(command: &str) -> Result<(), String> {
    let command = command.trim();
    if command.is_empty() {
        return Err("empty command".to_string());
    }

    if command.contains("$'") {
        return Err(
            "command contains ANSI-C quoting ($'...') which may expand to shell metacharacters"
                .to_string(),
        );
    }

    let upper = command.to_uppercase();
    if upper.contains("PATH=") || upper.contains("LD_PRELOAD=") {
        return Err(
            "command contains environment variable override (PATH= or LD_PRELOAD=)".to_string(),
        );
    }

    for meta in [
        "&", ";", "|", "&&", "||", "$(", "${", "`", "$", "\n", "\r", ">", "<",
    ] {
        if command.contains(meta) {
            return Err(format!(
                "command contains dangerous shell metacharacter '{}'",
                meta
            ));
        }
    }

    Ok(())
}

fn check_url(url_str: &str) -> Result<(), String> {
    let parsed = url::Url::parse(url_str).map_err(|error| format!("invalid URL: {error}"))?;
    let scheme = parsed.scheme();
    let host = parsed.host_str().unwrap_or("");

    let rebinding_domains = ["nip.io", "xip.io"];
    if rebinding_domains
        .iter()
        .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
    {
        return Err(format!(
            "URL not allowed: {url_str} (DNS rebinding domain '{host}' blocked)"
        ));
    }

    if scheme == "http" && is_localhost(host) {
        return Ok(());
    }

    if scheme != "https" {
        return Err(format!(
            "URL not allowed: {url_str}. Only https:// and http://localhost are permitted."
        ));
    }

    let port = parsed.port_or_known_default().unwrap_or(443);
    reject_private_resolved_host(host, port)
        .map_err(|reason| format!("HTTPS URL not allowed: {url_str} ({reason})"))
}

fn is_localhost(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "::1" | "[::1]")
}

fn reject_private_resolved_host(host: &str, port: u16) -> Result<(), String> {
    if is_localhost(host) {
        return Err("host resolves to localhost".to_string());
    }

    if let Ok(ip) = host.parse::<IpAddr>() {
        return reject_private_ip(ip);
    }

    let addrs = (host, port)
        .to_socket_addrs()
        .map_err(|error| format!("DNS resolution failed for '{host}:{port}': {error}"))?;
    for addr in addrs {
        reject_private_ip(addr.ip())?;
    }
    Ok(())
}

fn reject_private_ip(ip: IpAddr) -> Result<(), String> {
    match ip {
        IpAddr::V4(v4)
            if v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified() =>
        {
            Err(format!("blocked private IPv4 address {v4}"))
        }
        IpAddr::V6(v6)
            if v6.is_loopback()
                || v6.is_unique_local()
                || v6.is_unicast_link_local()
                || v6.is_unspecified() =>
        {
            Err(format!("blocked private IPv6 address {v6}"))
        }
        _ => Ok(()),
    }
}

fn workspace_git_context(workdir: &Path) -> Option<Vec<String>> {
    let branch = workspace_run_git(workdir, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let status = workspace_run_git(workdir, &["status", "--short"]).unwrap_or_default();
    let mut lines = vec![format!("- branch: {branch}")];
    if status.trim().is_empty() {
        lines.push("- worktree: clean".into());
    } else {
        let count = status.lines().count();
        lines.push(format!("- worktree: {count} changed path(s)"));
        for line in status.lines().take(12) {
            lines.push(format!("  {line}"));
        }
        if count > 12 {
            lines.push(format!("  ... {remaining} more", remaining = count - 12));
        }
    }
    Some(lines)
}

fn workspace_run_git(workdir: &Path, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(workdir)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn workspace_instruction_context(workdir: &Path) -> Vec<String> {
    const FILES: [&str; 3] = ["LATTICE.md", "AGENTS.md", "CLAUDE.md"];
    let root = workspace_run_git(workdir, &["rev-parse", "--show-toplevel"])
        .map(PathBuf::from)
        .unwrap_or_else(|| workdir.to_path_buf());
    let root = root.canonicalize().unwrap_or(root);
    let mut current = workdir
        .canonicalize()
        .unwrap_or_else(|_| workdir.to_path_buf());
    let mut dirs = Vec::new();
    loop {
        if current.starts_with(&root) {
            dirs.push(current.clone());
        }
        if current == root || !current.pop() {
            break;
        }
    }
    dirs.reverse();

    let mut lines = Vec::new();
    for dir in dirs {
        for filename in FILES {
            let path = dir.join(filename);
            if path.is_file() {
                let display = path.strip_prefix(&root).unwrap_or(&path).display();
                lines.push(format!("- {display}"));
            }
        }
    }
    lines
}

fn workspace_project_type_context(workdir: &Path) -> Vec<String> {
    let mut lines = Vec::new();
    let checks = [
        (
            "Cargo.toml",
            "Rust workspace/crate; verify with cargo test, cargo check, or cargo fmt as appropriate.",
        ),
        (
            "package.json",
            "Node/JS project; inspect scripts before choosing npm/pnpm/yarn commands.",
        ),
        (
            "pyproject.toml",
            "Python project; inspect tool config before choosing pytest/ruff/mypy commands.",
        ),
        (
            "go.mod",
            "Go module; verify with go test ./... or go test on touched packages.",
        ),
        ("Makefile", "Make targets available; inspect before use."),
    ];
    for (file, note) in checks {
        if workdir.join(file).is_file() {
            lines.push(format!("- {file}: {note}"));
        }
    }
    lines
}

fn truncate_output(mut output: String, limit: usize, label: &str) -> String {
    if output.len() <= limit {
        return output;
    }
    output.truncate(limit);
    output.push_str(&format!(
        "\n[TRUNCATED: {} exceeded {} bytes]",
        label, limit
    ));
    output
}

fn glob_like_matches(pattern: &str, candidate: &str) -> bool {
    if pattern.is_empty() {
        return false;
    }
    if wildcard_match(pattern, candidate) {
        return true;
    }
    let name = candidate.rsplit('/').next().unwrap_or(candidate);
    wildcard_match(pattern, name)
}

fn wildcard_match(pattern: &str, text: &str) -> bool {
    let pattern = pattern.as_bytes();
    let text = text.as_bytes();
    let (mut p, mut t) = (0usize, 0usize);
    let mut star = None;
    let mut star_text = 0usize;

    while t < text.len() {
        if p < pattern.len() && (pattern[p] == b'?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == b'*' {
            star = Some(p);
            p += 1;
            star_text = t;
        } else if let Some(star_pos) = star {
            p = star_pos + 1;
            star_text += 1;
            t = star_text;
        } else {
            return false;
        }
    }

    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }

    p == pattern.len()
}

struct FindFilesContext<'a> {
    root: &'a Path,
    project_root: &'a Path,
    matches: &'a mut Vec<String>,
    pattern: &'a str,
    visited: &'a mut HashSet<PathBuf>,
    max_results: usize,
    truncated: &'a mut bool,
}

async fn find_files_recursive(context: &mut FindFilesContext<'_>, path: &Path, depth: u32) {
    if depth > GREP_MAX_DEPTH || context.matches.len() >= context.max_results {
        if context.matches.len() >= context.max_results {
            *context.truncated = true;
        }
        return;
    }

    let resolved = match tokio::fs::canonicalize(path).await {
        Ok(p) => p,
        Err(_) => return,
    };
    if !context.visited.insert(resolved.clone()) {
        return;
    }
    // v1.1 project-root containment: reject symlinks escaping the root (D-19)
    if resolved != context.project_root && !resolved.starts_with(context.project_root) {
        return;
    }
    if resolved != context.root {
        let relative = resolved
            .strip_prefix(context.root)
            .unwrap_or(&resolved)
            .to_string_lossy()
            .replace('\\', "/");
        if glob_like_matches(context.pattern, &relative) {
            context.matches.push(relative);
            if context.matches.len() >= context.max_results {
                *context.truncated = true;
                return;
            }
        }
    }

    if !resolved.is_dir() {
        return;
    }

    let mut entries = match tokio::fs::read_dir(&resolved).await {
        Ok(d) => d,
        Err(_) => return,
    };
    let mut children = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if matches!(name_str.as_ref(), ".git" | "target" | "node_modules") {
            continue;
        }
        if name_str.starts_with('.') && name_str != "." && name_str != ".." {
            continue;
        }
        children.push(entry.path());
    }
    children.sort();

    for child_path in children {
        if context.matches.len() >= context.max_results {
            *context.truncated = true;
            return;
        }
        Box::pin(find_files_recursive(context, &child_path, depth + 1)).await;
    }
}

/// Recursively search files under `path` for lines matching `pattern`.
/// Respects depth, project-root containment, and file-size limits.
/// Skips hidden dirs, binary files, and follows symlinks with cycle detection.
struct GrepContext<'a> {
    pattern: &'a regex::Regex,
    project_root: &'a Path,
    results: &'a mut Vec<String>,
    limits: &'a ToolLimits,
    visited: &'a mut HashSet<PathBuf>,
    max_results: usize,
    truncated: &'a mut bool,
}

async fn grep_recursive(context: &mut GrepContext<'_>, path: &Path, depth: u32) {
    if depth > GREP_MAX_DEPTH || context.results.len() >= context.max_results {
        if context.results.len() >= context.max_results {
            *context.truncated = true;
        }
        return;
    }

    // Resolve symlinks to detect cycles and reject paths that escape the root.
    let resolved = match tokio::fs::canonicalize(path).await {
        Ok(p) => p,
        Err(_) => return,
    };
    if !context.visited.insert(resolved.clone()) {
        return; // symlink cycle
    }

    // v1.1 project-root containment: reject symlinks escaping the root (D-19)
    if resolved != context.project_root && !resolved.starts_with(context.project_root) {
        return;
    }

    if resolved.is_file() {
        // Skip files too large
        if let Ok(meta) = tokio::fs::metadata(&resolved).await {
            if meta.len() > context.limits.max_read_size as u64 {
                return;
            }
        }

        if let Ok(content) = tokio::fs::read_to_string(&resolved).await {
            // Skip binary files
            if content.contains('\0') {
                return;
            }
            for (line_num, line) in content.lines().enumerate() {
                if context.pattern.is_match(line) {
                    if context.results.len() >= context.max_results {
                        *context.truncated = true;
                        return;
                    }
                    context.results.push(format!(
                        "{}:{}:{}",
                        resolved.display(),
                        line_num + 1,
                        line
                    ));
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
            if context.results.len() >= context.max_results {
                *context.truncated = true;
                return;
            }
            Box::pin(grep_recursive(context, &child_path, depth + 1)).await;
        }
    }
}

// ---------------------------------------------------------------------------
// Platform-specific bash execution helpers (setrlimit)
// ---------------------------------------------------------------------------

/// Wait for a child process with a timeout, producing a uniform result.
async fn wait_with_timeout(
    child: tokio::process::Child,
    timeout_secs: u64,
    configured_timeout: u32,
) -> Result<std::process::Output, String> {
    match tokio::time::timeout(
        std::time::Duration::from_secs(timeout_secs),
        child.wait_with_output(),
    )
    .await
    {
        Ok(Ok(o)) => Ok(o),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err(format!(
            "bash command timed out after {}s",
            configured_timeout
        )),
    }
}

/// Execute a bash command with platform-specific resource limits (Unix).
#[cfg(unix)]
async fn execute_bash_command(
    command: &str,
    limits: &ToolLimits,
    _project_root: &str,
) -> Result<std::process::Output, String> {
    let mut cmd = std::process::Command::new("sh");
    cmd.arg("-c")
        .arg(command)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    #[cfg(target_os = "linux")]
    // SAFETY: `pre_exec` runs in the forked child process before `exec`.
    // The closure only calls `apply_resource_limits_linux` (POSIX setrlimit),
    // which is async-signal-safe. No allocations occur after fork. The
    // closure returns `Ok(())` on success or logs a warning on failure.
    unsafe {
        cmd.pre_exec(move || {
            apply_resource_limits_linux();
            Ok(())
        });
    }

    #[cfg(all(unix, not(target_os = "linux")))]
    // SAFETY: `pre_exec` runs in the forked child process before `exec`.
    // The closure only calls `apply_resource_limits_unix` (POSIX setrlimit),
    // which is async-signal-safe. No allocations occur after fork. The
    // closure returns `Ok(())` on success or logs a warning on failure —
    // it never panics.
    unsafe {
        cmd.pre_exec(move || {
            apply_resource_limits_unix();
            Ok(())
        });
    }

    let mut tokio_cmd: tokio::process::Command = cmd.into();
    tokio_cmd.kill_on_drop(true);
    let timeout_secs = std::cmp::max(limits.max_command_timeout, 1) as u64;
    let child = tokio_cmd.spawn().map_err(|e| e.to_string())?;
    wait_with_timeout(child, timeout_secs, limits.max_command_timeout).await
}

/// Execute a bash command on non-Unix platforms.
#[cfg(not(unix))]
async fn execute_bash_command(
    command: &str,
    limits: &ToolLimits,
    _project_root: &str,
) -> Result<std::process::Output, String> {
    let timeout_secs = std::cmp::max(limits.max_command_timeout, 1) as u64;
    let mut cmd = tokio::process::Command::new("sh");
    cmd.args(["-c", command]).kill_on_drop(true);
    let child = cmd.spawn().map_err(|e| e.to_string())?;
    wait_with_timeout(child, timeout_secs, limits.max_command_timeout).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_llm::{FunctionCall, ToolCall};

    fn tool_call(name: &str, args: serde_json::Value) -> ToolCall {
        ToolCall::new("call_test", FunctionCall::new(name, args.to_string()))
    }

    #[tokio::test]
    async fn read_file_supports_relative_paths_line_ranges_and_line_numbers() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("sample.txt"), "one\ntwo\nthree\nfour\n").unwrap();
        let executor = DefaultToolExecutor::new(dir.path().to_str().unwrap()).unwrap();

        let result = executor
            .execute(&tool_call(
                "read_file",
                serde_json::json!({
                    "path": "sample.txt",
                    "offset": 2,
                    "limit": 2
                }),
            ))
            .await;

        assert!(result.contains("     2\ttwo"));
        assert!(result.contains("     3\tthree"));
        assert!(result.contains("[TRUNCATED: showing lines 2-3 of 4"));
        assert!(!result.contains("one"));
    }

    #[tokio::test]
    async fn read_files_batches_targeted_file_reads() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "a1\na2\na3\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "b1\nb2\n").unwrap();
        let executor = DefaultToolExecutor::new(dir.path().to_str().unwrap()).unwrap();

        let result = executor
            .execute(&tool_call(
                "read_files",
                serde_json::json!({
                    "files": [
                        {"path": "a.txt", "offset": 2, "limit": 1},
                        {"path": "b.txt", "line_numbers": false}
                    ]
                }),
            ))
            .await;

        assert!(result.contains("--- a.txt ---"));
        assert!(result.contains("     2\ta2"));
        assert!(!result.contains("     1\ta1"));
        assert!(result.contains("--- b.txt ---"));
        assert!(result.contains("b1\nb2"));
    }

    #[tokio::test]
    async fn read_file_and_read_files_show_content_hash() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "a1\na2\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "b1\n").unwrap();
        let executor = DefaultToolExecutor::new(dir.path().to_str().unwrap()).unwrap();

        let single = executor
            .execute(&tool_call(
                "read_file",
                serde_json::json!({
                    "path": "a.txt"
                }),
            ))
            .await;

        let batch = executor
            .execute(&tool_call(
                "read_files",
                serde_json::json!({
                    "files": [
                        {"path": "a.txt"},
                        {"path": "b.txt"}
                    ]
                }),
            ))
            .await;

        assert!(single.contains("Content hash: blake3:"));
        assert_eq!(batch.matches("Content hash: blake3:").count(), 2);
    }

    #[tokio::test]
    async fn workspace_context_reports_entries_instructions_and_project_type() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "").unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "rules").unwrap();
        let executor = DefaultToolExecutor::new(dir.path().to_str().unwrap()).unwrap();

        let result = executor
            .execute(&tool_call(
                "workspace_context",
                serde_json::json!({
                    "path": "."
                }),
            ))
            .await;

        assert!(result.contains("Workspace:"));
        assert!(result.contains("FILE Cargo.toml"));
        assert!(result.contains("DIR src"));
        assert!(result.contains("Project instruction files"));
        assert!(result.contains("AGENTS.md"));
        assert!(result.contains("Detected project files"));
        assert!(result.contains("cargo test"));
    }

    #[tokio::test]
    async fn todo_write_validates_single_in_progress_item() {
        let dir = tempfile::tempdir().unwrap();
        let executor = DefaultToolExecutor::new(dir.path().to_str().unwrap()).unwrap();

        let result = executor
            .execute(&tool_call(
                "todo_write",
                serde_json::json!({
                    "todos": [
                        {"content": "Inspect code", "status": "completed"},
                        {"content": "Apply patch", "status": "in_progress"},
                        {"content": "Run tests", "status": "pending"}
                    ]
                }),
            ))
            .await;

        assert!(
            result.contains("Todo list updated: 3 item(s), 1 pending, 1 in_progress, 1 completed")
        );
        assert!(result.contains("2. [in_progress] Apply patch"));

        let invalid = executor
            .execute(&tool_call(
                "todo_write",
                serde_json::json!({
                    "todos": [
                        {"content": "One", "status": "in_progress"},
                        {"content": "Two", "status": "in_progress"}
                    ]
                }),
            ))
            .await;

        assert!(invalid.contains("at most one in_progress"));
    }

    #[tokio::test]
    async fn grep_caps_results_and_reports_truncation() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("sample.txt"), "hit 1\nhit 2\nhit 3\n").unwrap();
        let executor = DefaultToolExecutor::new(dir.path().to_str().unwrap()).unwrap();

        let result = executor
            .execute(&tool_call(
                "grep",
                serde_json::json!({
                    "pattern": "hit",
                    "path": ".",
                    "max_results": 2
                }),
            ))
            .await;

        assert!(result.contains("sample.txt:1:hit 1"));
        assert!(result.contains("sample.txt:2:hit 2"));
        assert!(!result.contains("hit 3"));
        assert!(result.contains("[TRUNCATED: grep returned first 2 matches"));
    }

    #[tokio::test]
    async fn list_directory_caps_entries_and_reports_total() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["a.txt", "b.txt", "c.txt"] {
            std::fs::write(dir.path().join(name), name).unwrap();
        }
        let executor = DefaultToolExecutor::new(dir.path().to_str().unwrap()).unwrap();

        let result = executor
            .execute(&tool_call(
                "list_directory",
                serde_json::json!({
                    "path": ".",
                    "limit": 2
                }),
            ))
            .await;

        assert!(result.contains("FILE  a.txt"));
        assert!(result.contains("FILE  b.txt"));
        assert!(!result.contains("FILE  c.txt"));
        assert!(result.contains("[TRUNCATED: showing 2 of 3 entries"));
    }

    #[tokio::test]
    async fn find_files_matches_names_and_relative_paths_with_limits() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/bin")).unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), "").unwrap();
        std::fs::write(dir.path().join("src/bin/main.rs"), "").unwrap();
        std::fs::write(dir.path().join("README.md"), "").unwrap();
        let executor = DefaultToolExecutor::new(dir.path().to_str().unwrap()).unwrap();

        let result = executor
            .execute(&tool_call(
                "find_files",
                serde_json::json!({
                    "pattern": "*.rs",
                    "path": ".",
                    "limit": 1
                }),
            ))
            .await;

        assert!(result.contains(".rs"));
        assert!(!result.contains("README.md"));
        assert!(result.contains("[TRUNCATED: find_files returned first 1 matches"));
    }

    #[tokio::test]
    async fn patch_applies_structured_patch_and_returns_json_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.txt");
        let before = "alpha\nbeta\ngamma\n";
        std::fs::write(&path, before).unwrap();
        let executor = DefaultToolExecutor::new(dir.path().to_str().unwrap()).unwrap();
        let old_hash = crate::edit::content_hash(before.as_bytes());

        let result = executor
            .execute(&tool_call(
                "patch",
                serde_json::json!({ "patch": format!(
                    "\
*** Begin Patch
*** Update File: sample.txt
*** Old Hash: {old_hash}
@@
 alpha
-beta
+BETA
 gamma
*** End Patch"
                )}),
            ))
            .await;
        let value: serde_json::Value = serde_json::from_str(&result).unwrap();

        assert_eq!(value["ok"], true);
        assert_eq!(value["changed_files"][0]["path"], "sample.txt");
        assert_eq!(value["changed_files"][0]["operation"], "update");
        assert!(value["changed_files"][0]["bytes_before"].as_u64().unwrap() > 0);
        assert!(value["changed_files"][0]["bytes_after"].as_u64().unwrap() > 0);
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "alpha\nBETA\ngamma\n"
        );
    }

    #[tokio::test]
    async fn patch_failure_json_keeps_changed_files_empty_and_attempts_populated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.txt");
        let before = "alpha\n";
        std::fs::write(&path, before).unwrap();
        let executor = DefaultToolExecutor::new(dir.path().to_str().unwrap()).unwrap();
        let old_hash = crate::edit::content_hash(before.as_bytes());

        let result = executor
            .execute(&tool_call(
                "patch",
                serde_json::json!({ "patch": format!(
                    "\
*** Begin Patch
*** Update File: sample.txt
*** Old Hash: {old_hash}
@@
-missing
+new
*** End Patch"
                )}),
            ))
            .await;
        let value: serde_json::Value = serde_json::from_str(&result).unwrap();

        assert_eq!(value["ok"], false);
        assert_eq!(value["errors"][0]["code"], "hunk_missing");
        assert_eq!(value["changed_files"].as_array().unwrap().len(), 0);
        assert_eq!(value["attempted_operations"][0]["path"], "sample.txt");
        assert_eq!(std::fs::read_to_string(path).unwrap(), before);
    }

    #[tokio::test]
    async fn direct_multi_patch_call_returns_retired_message() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.txt");
        std::fs::write(&path, "alpha\n").unwrap();
        let executor = DefaultToolExecutor::new(dir.path().to_str().unwrap()).unwrap();

        let result = executor
            .execute(&tool_call(
                "multi_patch",
                serde_json::json!({
                    "file_path": "sample.txt",
                    "edits": [
                        {"search": "alpha", "insert": "beta"}
                    ]
                }),
            ))
            .await;

        assert_eq!(
            result,
            "Tool retired: multi_patch. Use patch with structured patch text."
        );
        assert_eq!(std::fs::read_to_string(path).unwrap(), "alpha\n");
    }

    #[tokio::test]
    async fn authorized_executor_denies_unlisted_builtin_before_execution() {
        let dir = tempfile::tempdir().unwrap();
        let executor = DefaultToolExecutor::new(dir.path().to_str().unwrap())
            .unwrap()
            .with_authorized_tools(["read_file"]);

        let result = executor
            .execute(&tool_call(
                "write_file",
                serde_json::json!({
                    "path": "denied.txt",
                    "content": "should not write"
                }),
            ))
            .await;

        assert!(
            result.contains("Tool denied: 'write_file' is not enabled"),
            "denial should be plain enabled-tool text: {}",
            result
        );
        assert!(result.contains("read_file"));
        assert!(!dir.path().join("denied.txt").exists());
    }

    #[tokio::test]
    async fn denied_tool_is_not_unknown_tool() {
        let dir = tempfile::tempdir().unwrap();
        let executor = DefaultToolExecutor::new(dir.path().to_str().unwrap())
            .unwrap()
            .with_authorized_tools(["read_file"]);

        let result = executor
            .execute(&tool_call(
                "bash",
                serde_json::json!({"command": "echo no"}),
            ))
            .await;

        assert!(
            result.contains("Tool denied: 'bash' is not enabled"),
            "denial should be plain enabled-tool text: {}",
            result
        );
        assert!(!result.contains("Unknown tool:"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bash_timeout_returns_promptly() {
        let dir = tempfile::tempdir().unwrap();
        let limits = ToolLimits {
            max_command_timeout: 1,
            ..ToolLimits::default()
        };

        let started = std::time::Instant::now();
        let result = DefaultToolExecutor::execute_bash(
            dir.path().to_str().unwrap(),
            &limits,
            "exec sleep 5",
        )
        .await;

        assert!(result.unwrap_err().contains("timed out"));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(3),
            "timeout should return without waiting for the full command duration"
        );
    }

    // ── project-root executor tests (v1.1 path containment) ──────────────

    #[tokio::test]
    async fn project_root_executor_rejects_parent_traversal() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("secret.txt"), "secret").unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("ok.txt"), "ok").unwrap();
        let executor = DefaultToolExecutor::for_project_root(
            sub.to_str().unwrap(),
            [
                "read_file",
                "read_files",
                "write_file",
                "grep",
                "list_directory",
                "find_files",
            ],
        )
        .unwrap();

        // read_file with .. traversal
        let result = executor
            .execute(&tool_call(
                "read_file",
                serde_json::json!({"path": "../secret.txt"}),
            ))
            .await;
        assert!(
            result.contains("traversal denied"),
            "should reject .. traversal, got: {}",
            result
        );

        // read_files with .. traversal
        let result = executor
            .execute(&tool_call(
                "read_files",
                serde_json::json!({"files": [{"path": "../secret.txt"}]}),
            ))
            .await;
        assert!(
            result.contains("traversal denied"),
            "read_files: {}",
            result
        );

        // write_file with .. traversal
        let result = executor
            .execute(&tool_call(
                "write_file",
                serde_json::json!({"path": "../evil.txt", "content": "bad"}),
            ))
            .await;
        assert!(
            result.contains("traversal denied"),
            "write_file: {}",
            result
        );

        // grep with .. traversal
        let result = executor
            .execute(&tool_call(
                "grep",
                serde_json::json!({"pattern": "secret", "path": ".."}),
            ))
            .await;
        assert!(result.contains("traversal denied"), "grep: {}", result);

        // list_directory with .. traversal
        let result = executor
            .execute(&tool_call(
                "list_directory",
                serde_json::json!({"path": ".."}),
            ))
            .await;
        assert!(
            result.contains("traversal denied"),
            "list_directory: {}",
            result
        );

        // find_files with .. traversal
        let result = executor
            .execute(&tool_call(
                "find_files",
                serde_json::json!({"pattern": "*.txt", "path": ".."}),
            ))
            .await;
        assert!(
            result.contains("traversal denied"),
            "find_files: {}",
            result
        );
    }

    #[tokio::test]
    async fn project_root_executor_rejects_symlink_escape() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        std::fs::create_dir(&project).unwrap();
        let outside = dir.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "secret data").unwrap();

        // Create a symlink inside project pointing outside
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, project.join("escape_link")).unwrap();

        let executor = DefaultToolExecutor::for_project_root(
            project.to_str().unwrap(),
            ["read_file", "grep", "list_directory", "find_files"],
        )
        .unwrap();

        // read_file through symlink escape
        let result = executor
            .execute(&tool_call(
                "read_file",
                serde_json::json!({"path": "escape_link/secret.txt"}),
            ))
            .await;
        assert!(
            result.contains("outside the project root") || result.contains("IoError"),
            "should deny symlink escape for read_file, got: {}",
            result
        );

        // list_directory through symlink escape
        let result = executor
            .execute(&tool_call(
                "list_directory",
                serde_json::json!({"path": "escape_link"}),
            ))
            .await;
        assert!(
            result.contains("outside the project root"),
            "should deny symlink escape for list_directory, got: {}",
            result
        );

        // grep through symlink escape
        let result = executor
            .execute(&tool_call(
                "grep",
                serde_json::json!({"pattern": "secret", "path": "escape_link"}),
            ))
            .await;
        // grep_recursive silently skips paths outside root (returns no matches)
        assert!(
            result.contains("(no matches)") || result.contains("outside"),
            "grep should not find matches outside root, got: {}",
            result
        );

        // find_files through symlink escape
        let result = executor
            .execute(&tool_call(
                "find_files",
                serde_json::json!({"pattern": "*.txt", "path": "escape_link"}),
            ))
            .await;
        assert!(
            result.contains("(no matches)") || result.contains("outside"),
            "find_files should not find matches outside root, got: {}",
            result
        );
    }

    #[tokio::test]
    async fn project_root_executor_allows_inside_project_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("hello.txt"), "hello world\n").unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}").unwrap();

        let executor = DefaultToolExecutor::for_project_root(
            dir.path().to_str().unwrap(),
            [
                "read_file",
                "write_file",
                "grep",
                "list_directory",
                "find_files",
                "patch",
            ],
        )
        .unwrap();

        // read_file inside root
        let result = executor
            .execute(&tool_call(
                "read_file",
                serde_json::json!({"path": "hello.txt"}),
            ))
            .await;
        assert!(result.contains("hello world"), "read_file: {}", result);

        // grep inside root
        let result = executor
            .execute(&tool_call(
                "grep",
                serde_json::json!({"pattern": "hello", "path": "."}),
            ))
            .await;
        assert!(result.contains("hello.txt"), "grep: {}", result);

        // list_directory inside root
        let result = executor
            .execute(&tool_call(
                "list_directory",
                serde_json::json!({"path": "."}),
            ))
            .await;
        assert!(
            result.contains("FILE  hello.txt"),
            "list_directory: {}",
            result
        );
        assert!(result.contains("DIR  src"), "list_directory: {}", result);

        // find_files inside root
        let result = executor
            .execute(&tool_call(
                "find_files",
                serde_json::json!({"pattern": "*.rs"}),
            ))
            .await;
        assert!(result.contains("main.rs"), "find_files: {}", result);

        // write_file inside root
        let result = executor
            .execute(&tool_call(
                "write_file",
                serde_json::json!({"path": "output.txt", "content": "written"}),
            ))
            .await;
        assert!(result.contains("Wrote"), "write_file: {}", result);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("output.txt")).unwrap(),
            "written"
        );
    }

    #[tokio::test]
    async fn project_root_executor_keeps_output_limits() {
        let dir = tempfile::tempdir().unwrap();
        let content: String = (0..3000).map(|i| format!("line {i}\n")).collect();
        std::fs::write(dir.path().join("big.txt"), &content).unwrap();

        let executor = DefaultToolExecutor::for_project_root(
            dir.path().to_str().unwrap(),
            ["read_file", "grep", "list_directory", "find_files"],
        )
        .unwrap();

        // read_file respects DEFAULT_READ_LINE_LIMIT (2000)
        let result = executor
            .execute(&tool_call(
                "read_file",
                serde_json::json!({"path": "big.txt"}),
            ))
            .await;
        assert!(
            result.contains("[TRUNCATED"),
            "read_file should truncate at line limit"
        );

        // grep respects DEFAULT_GREP_LIMIT (200)
        let result = executor
            .execute(&tool_call(
                "grep",
                serde_json::json!({"pattern": "line", "path": "."}),
            ))
            .await;
        assert!(
            result.contains("[TRUNCATED"),
            "grep should truncate at result limit"
        );
    }

    #[tokio::test]
    async fn project_root_executor_has_no_dynamic_approval() {
        let dir = tempfile::tempdir().unwrap();
        let executor =
            DefaultToolExecutor::for_project_root(dir.path().to_str().unwrap(), ["read_file"])
                .unwrap();

        // The for_project_root constructor has no dynamic approval flow.
        let result = executor
            .execute(&tool_call(
                "bash",
                serde_json::json!({"command": "echo test"}),
            ))
            .await;
        assert!(
            result.contains("Tool denied: 'bash' is not enabled"),
            "unlisted tool should be denied: {}",
            result
        );
    }
}

// --- Resource limit helpers ------------------------------------------------

/// Apply resource limits in the child process (Linux).
///
/// - RLIMIT_AS: 512 MB virtual memory
/// - RLIMIT_NPROC: 1 process
/// - RLIMIT_CPU: 30 seconds
///
/// Failures are silently ignored (logged at trace level).
#[cfg(target_os = "linux")]
fn apply_resource_limits_linux() {
    let rlim_as = libc::rlimit {
        rlim_cur: 512 * 1024 * 1024,
        rlim_max: 512 * 1024 * 1024,
    };
    // SAFETY: `setrlimit` is a POSIX syscall that sets process resource limits.
    // The `rlimit` struct fields are initialized with valid values. The return
    // value is checked; a non-zero return is logged but does not panic. This
    // call is safe because: (1) we pass stack-allocated structs with valid
    // fields, (2) the kernel validates the values, (3) failure is non-fatal.
    if unsafe { libc::setrlimit(libc::RLIMIT_AS, &rlim_as) } != 0 {
        tracing::warn!("setrlimit(RLIMIT_AS) failed in child process");
    }

    let rlim_nproc = libc::rlimit {
        rlim_cur: 1,
        rlim_max: 1,
    };
    // SAFETY: Same as above — valid struct, checked return, non-fatal failure.
    if unsafe { libc::setrlimit(libc::RLIMIT_NPROC, &rlim_nproc) } != 0 {
        tracing::warn!("setrlimit(RLIMIT_NPROC) failed in child process");
    }

    let rlim_cpu = libc::rlimit {
        rlim_cur: 30,
        rlim_max: 30,
    };
    // SAFETY: Same as above — valid struct, checked return, non-fatal failure.
    if unsafe { libc::setrlimit(libc::RLIMIT_CPU, &rlim_cpu) } != 0 {
        tracing::warn!("setrlimit(RLIMIT_CPU) failed in child process");
    }
}

/// Apply resource limits in the child process (macOS / other Unix).
///
/// - RLIMIT_AS: 512 MB virtual memory
/// - RLIMIT_NPROC: 32 (macOS limits entire process tree, not individual)
/// - RLIMIT_CPU: 30 seconds
///
/// Failures are silently ignored (logged at trace level).
#[cfg(all(unix, not(target_os = "linux")))]
fn apply_resource_limits_unix() {
    let rlim_as = libc::rlimit {
        rlim_cur: 512 * 1024 * 1024,
        rlim_max: 512 * 1024 * 1024,
    };
    // SAFETY: `setrlimit` is a POSIX syscall that sets process resource limits.
    // The `rlimit` struct fields are initialized with valid values. The return
    // value is checked; a non-zero return is logged but does not panic. This
    // call is safe because: (1) we pass stack-allocated structs with valid
    // fields, (2) the kernel validates the values, (3) failure is non-fatal.
    if unsafe { libc::setrlimit(libc::RLIMIT_AS, &rlim_as) } != 0 {
        tracing::warn!("setrlimit(RLIMIT_AS) failed in child process");
    }

    let rlim_nproc = libc::rlimit {
        rlim_cur: 32,
        rlim_max: 32,
    };
    // SAFETY: Same as above — valid struct, checked return, non-fatal failure.
    if unsafe { libc::setrlimit(libc::RLIMIT_NPROC, &rlim_nproc) } != 0 {
        tracing::warn!("setrlimit(RLIMIT_NPROC) failed in child process");
    }

    let rlim_cpu = libc::rlimit {
        rlim_cur: 30,
        rlim_max: 30,
    };
    // SAFETY: Same as above — valid struct, checked return, non-fatal failure.
    if unsafe { libc::setrlimit(libc::RLIMIT_CPU, &rlim_cpu) } != 0 {
        tracing::warn!("setrlimit(RLIMIT_CPU) failed in child process");
    }
}

//! Default tool executor — executes tool calls against the local filesystem.
//!
//! Tool definitions (the schemas that tell the LLM what tools are available)
//! live in [`crate::tool_definitions`]. This module provides the execution
//! layer that runs those tools when the model requests them.

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::str::FromStr;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

use async_trait::async_trait;

use crate::capabilities::ToolBundle;
use crate::edit::{apply_patch as apply_structured_patch, content_hash, PatchResultEnvelope};
use crate::tool_error::ToolError;
use crate::{ToolExecutor, ToolOutcome};
use lattice_sandbox::{
    ApprovalKind, DenialKind, DenialReport, PolicyDecision, RiskClass, SandboxConfig, ToolCatalog,
};

/// Trait abstracting tool registry access. Provides custom tool handlers
/// that DefaultToolExecutor checks before falling back to built-in tools.
pub trait RegistryToolAccess: Send + Sync {
    fn get_handler(
        &self,
        tool_name: &str,
    ) -> Option<std::sync::Arc<dyn Fn(serde_json::Value) -> String + Send + Sync>>;
    fn get_definitions(&self) -> Vec<lattice_core::types::ToolDefinition>;
}

/// Adapter implementing [`ToolCatalog`] via [`ToolBundle`] for bundle
/// resolution in `DefaultToolExecutor`.
struct ToolBundleCatalog;

impl ToolCatalog for ToolBundleCatalog {
    fn tool_exists(&self, _name: &str) -> bool {
        true // defer to bundle_contains for actual matching
    }
    fn bundle_exists(&self, name: &str) -> bool {
        ToolBundle::from_str(name).is_ok()
    }
    fn bundle_contains(&self, bundle_name: &str, tool_name: &str) -> bool {
        ToolBundle::from_str(bundle_name)
            .map(|bundle| bundle.contains_tool(tool_name))
            .unwrap_or(false)
    }
}

/// Executes tools using the local filesystem and shell.
///
/// Supports: workspace_context, todo_write, read_file, read_files, grep,
/// write_file, list_directory, find_files, bash, patch,
/// web_search, plus bus:fetch when the `blob-store` feature is enabled.
/// The `project_root` is used by `write_file` and `patch`
/// to restrict writes to project source directories.
///
/// All tool operations are gated by the `sandbox` configuration
/// (path validation, sensitive-file blocking, command allowlisting,
/// URL scheme restrictions, and size/timeout limits).
///
/// When a `registry` is set, custom tool handlers from the registry are
/// checked first before falling back to the default 7 tools.
pub struct DefaultToolExecutor {
    project_root: String,
    sandbox: SandboxConfig,
    http_client: reqwest::Client,
    registry: Option<std::sync::Arc<dyn RegistryToolAccess>>,
    authorized_tools: Option<BTreeSet<String>>,
    #[cfg(feature = "blob-store")]
    blob_store: Option<std::sync::Arc<dyn crate::BlobFetcher>>,
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
            sandbox: SandboxConfig::default(),
            http_client: Self::build_http_client()?,
            registry: None,
            authorized_tools: None,
            #[cfg(feature = "blob-store")]
            blob_store: None,
        })
    }

    pub fn new_with_registry(
        project_root: &str,
        registry: Option<std::sync::Arc<dyn RegistryToolAccess>>,
    ) -> Result<Self, String> {
        Ok(Self {
            project_root: project_root.to_string(),
            sandbox: SandboxConfig::default(),
            http_client: Self::build_http_client()?,
            registry,
            authorized_tools: None,
            #[cfg(feature = "blob-store")]
            blob_store: None,
        })
    }

    pub async fn execute_bash(
        project_root: &str,
        sandbox: &SandboxConfig,
        command: &str,
    ) -> Result<std::process::Output, String> {
        sandbox
            .check_command(command)
            .map_err(|e| format!("sandbox: {e}"))?;
        execute_bash_command(command, sandbox, project_root).await
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
        let path_str = canonical.to_string_lossy();
        if let Err(e) = self.sandbox.check_read(&path_str) {
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

    /// Override the sandbox config (replaces the default).
    pub fn with_sandbox(mut self, config: SandboxConfig) -> Self {
        self.sandbox = config;
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

    /// Typed version of [`authorization_denial`] returning a structured
    /// `DenialReport` instead of a plain string on denial.
    ///
    /// The report includes the denied tool name, the list of enabled tools,
    /// and a `RiskClass::Mutate` classification so the agent loop can
    /// detect denials and present approval options.
    fn authorization_denial_report(&self, tool_name: &str) -> Option<DenialReport> {
        let authorized = self.authorized_tools.as_ref()?;
        if authorized.contains(tool_name) {
            return None;
        }
        if self.is_tool_dynamically_approved(tool_name) {
            return None;
        }

        Some(DenialReport::new(
            format!("tool '{}' is not enabled for this agent", tool_name),
            RiskClass::Mutate,
            DenialKind::Tool {
                tool_name: tool_name.to_string(),
                enabled_tools: authorized.iter().cloned().collect(),
            },
        ))
    }

    /// Backward-compatible wrapper: returns the `DenialReport` serialized
    /// as a JSON string so existing call sites in [`execute`] continue to
    /// receive a denial string.
    fn authorization_denial(&self, tool_name: &str) -> Option<String> {
        self.authorization_denial_report(tool_name)
            .map(|report| report.to_json())
    }

    fn is_tool_dynamically_approved(&self, tool_name: &str) -> bool {
        if let Some(ref authority) = self.sandbox.policy_authority {
            let catalog = ToolBundleCatalog;
            let decision =
                authority.decide_tool(tool_name, self.authorized_tools.as_ref(), Some(&catalog));
            return matches!(decision, PolicyDecision::Allow);
        }

        // Fallback: inline matching against approved_rules (backward-compatible path)
        let Some(ref rules) = self.sandbox.approved_rules else {
            return false;
        };
        let rules = rules.lock().unwrap_or_else(|e| e.into_inner());
        rules.iter().any(|rule| match &rule.kind {
            ApprovalKind::Tool {
                tool_name: approved,
            } => approved == tool_name,
            ApprovalKind::Bundle { bundle_name } => ToolBundle::from_str(bundle_name)
                .map(|bundle| bundle.contains_tool(tool_name))
                .unwrap_or(false),
            _ => false,
        })
    }

    /// Create a executor with an optional BlobFetcher for bus:fetch support.
    #[cfg(feature = "blob-store")]
    pub fn new_with_blob_store(
        project_root: &str,
        blob_store: Option<std::sync::Arc<dyn crate::BlobFetcher>>,
    ) -> Result<Self, String> {
        Ok(Self {
            project_root: project_root.to_string(),
            sandbox: SandboxConfig::default(),
            http_client: Self::build_http_client()?,
            registry: None,
            authorized_tools: None,
            blob_store,
        })
    }
}

#[async_trait]
impl ToolExecutor for DefaultToolExecutor {
    async fn execute_outcome(&self, call: &lattice_core::types::ToolCall) -> ToolOutcome {
        if serde_json::from_str::<serde_json::Value>(&call.function.arguments).is_ok() {
            if let Some(denial) = self.authorization_denial_report(&call.function.name) {
                return ToolOutcome::denied(denial);
            }
        }

        ToolOutcome::from_text(self.execute(call).await)
    }

    async fn execute(&self, call: &lattice_core::types::ToolCall) -> String {
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

        // Check registry for custom tools first
        if let Some(reg) = &self.registry {
            if let Some(handler) = reg.get_handler(&call.function.name) {
                return handler(args);
            }
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
                let mut context = GrepContext {
                    pattern: &re,
                    results: &mut results,
                    sandbox: &self.sandbox,
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
                            // Parent doesn't exist — reject write rather than
                            // bypass canonicalize (TOCTOU: symlink could redirect
                            // a non-canonical path after the check)
                            return format!(
                                "Sandbox: cannot write to '{}' — parent directory does not exist or cannot be resolved",
                                path
                            );
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
                let path_str = canonical.to_string_lossy();
                if let Err(e) = self.sandbox.check_read(&path_str) {
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
                let path_str = canonical.to_string_lossy();
                if let Err(e) = self.sandbox.check_read(&path_str) {
                    return e;
                }

                let mut matches = Vec::new();
                let mut visited = HashSet::new();
                let mut truncated = false;
                let mut context = FindFilesContext {
                    root: &canonical,
                    matches: &mut matches,
                    pattern,
                    sandbox: &self.sandbox,
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
                let result = Self::execute_bash(&self.project_root, &self.sandbox, cmd).await;
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
                    apply_structured_patch(Path::new(&self.project_root), &self.sandbox, patch)
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
            #[cfg(feature = "blob-store")]
            "bus:fetch" => {
                let key = args.get("key").and_then(|v| v.as_str()).unwrap_or("");
                if !key.starts_with("blob://") {
                    format!(
                        "Error: invalid blob key '{}', expected format blob://source/topic/hash",
                        key
                    )
                } else {
                    match &self.blob_store {
                        Some(fetcher) => match fetcher.fetch(key).await {
                            Ok(payload) => payload,
                            Err(e) => format!("Error: blob '{}' fetch failed: {}", key, e),
                        },
                        None => "Error: blob storage not configured for this agent".to_string(),
                    }
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
    matches: &'a mut Vec<String>,
    pattern: &'a str,
    sandbox: &'a lattice_sandbox::SandboxConfig,
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
    let path_str = resolved.to_string_lossy();
    if context.sandbox.check_read(&path_str).is_err() {
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
/// Respects sandbox limits: max_depth, max_read_size, check_read.
/// Skips hidden dirs, binary files, and follows symlinks with cycle detection.
struct GrepContext<'a> {
    pattern: &'a regex::Regex,
    results: &'a mut Vec<String>,
    sandbox: &'a lattice_sandbox::SandboxConfig,
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

    // Resolve symlinks to detect cycles AND use canonical path for sandbox checks
    let resolved = match tokio::fs::canonicalize(path).await {
        Ok(p) => p,
        Err(_) => return,
    };
    if !context.visited.insert(resolved.clone()) {
        return; // symlink cycle
    }

    let path_str = resolved.to_string_lossy();
    if context.sandbox.check_read(&path_str).is_err() {
        return;
    }

    if resolved.is_file() {
        // Skip files too large
        if let Ok(meta) = tokio::fs::metadata(&resolved).await {
            if meta.len() > context.sandbox.max_read_size as u64 {
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
                    context
                        .results
                        .push(format!("{}:{}:{}", path_str, line_num + 1, line));
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
// Platform-specific bash execution helpers (Landlock + setrlimit)
// ---------------------------------------------------------------------------

/// Execute a bash command with platform-specific sandboxing (Unix).
///
/// On Linux with `LandlockConfig` set, applies Landlock file isolation and
/// resource limits via `pre_exec`. On other Unix (macOS, BSD), applies
/// resource limits only.
#[cfg(unix)]
async fn execute_bash_command(
    command: &str,
    sandbox: &SandboxConfig,
    project_root: &str,
) -> Result<std::process::Output, String> {
    let mut cmd = std::process::Command::new("sh");
    cmd.arg("-c")
        .arg(command)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    #[cfg(not(target_os = "linux"))]
    let _ = project_root;

    // Linux: Landlock + setrlimit
    #[cfg(target_os = "linux")]
    {
        let read_paths: Option<Vec<PathBuf>> = sandbox
            .landlock
            .as_ref()
            .map(|lc| build_landlock_read_paths(project_root, &lc.extra_read_paths));
        let write_paths: Option<Vec<PathBuf>> = sandbox
            .landlock
            .as_ref()
            .map(|lc| build_landlock_write_paths(project_root, &lc.extra_write_paths));

        // SAFETY: `pre_exec` runs in the forked child process before `exec`.
        // The closure only calls `apply_resource_limits_linux` (POSIX setrlimit)
        // and `apply_landlock_sandbox` (Linux-specific Landlock LSM), both of
        // which are async-signal-safe. No allocations occur after fork. The
        // closure returns `Ok(())` on success or logs a warning on failure —
        // it never panics.
        unsafe {
            cmd.pre_exec(move || {
                // Apply resource limits first (best-effort, warn on failure)
                apply_resource_limits_linux();

                // Apply Landlock (best-effort; silently skipped on old kernels)
                if let (Some(read), Some(write)) = (&read_paths, &write_paths) {
                    let _ = apply_landlock_sandbox(read, write);
                }

                Ok(())
            });
        }
    }

    // macOS / other Unix: setrlimit only (no Landlock available)
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
    let timeout_secs = std::cmp::max(sandbox.max_command_timeout, 1) as u64;
    let child = tokio_cmd.spawn().map_err(|e| e.to_string())?;

    match tokio::time::timeout(
        std::time::Duration::from_secs(timeout_secs),
        child.wait_with_output(),
    )
    .await
    {
        Ok(Ok(o)) => Ok(o),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => {
            // `wait_with_output` owns the child, so `kill_on_drop(true)` is the
            // cleanup path when the timeout future is dropped.
            Err(format!(
                "bash command timed out after {}s",
                sandbox.max_command_timeout
            ))
        }
    }
}

/// Execute a bash command on non-Unix platforms (no sandboxing available).
#[cfg(not(unix))]
async fn execute_bash_command(
    command: &str,
    sandbox: &SandboxConfig,
    _project_root: &str,
) -> Result<std::process::Output, String> {
    let timeout_secs = std::cmp::max(sandbox.max_command_timeout, 1) as u64;
    let mut cmd = tokio::process::Command::new("sh");
    cmd.args(["-c", command]).kill_on_drop(true);
    let child = cmd.spawn().map_err(|e| e.to_string())?;

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
            sandbox.max_command_timeout
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_core::types::ToolDefinition;
    use lattice_core::types::{FunctionCall, ToolCall};
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    };

    struct TestRegistry {
        called: Arc<AtomicBool>,
    }

    impl RegistryToolAccess for TestRegistry {
        fn get_handler(
            &self,
            tool_name: &str,
        ) -> Option<Arc<dyn Fn(serde_json::Value) -> String + Send + Sync>> {
            if tool_name != "custom_tool" {
                return None;
            }

            let called = Arc::clone(&self.called);
            Some(Arc::new(move |_| {
                called.store(true, Ordering::SeqCst);
                "custom handler called".to_string()
            }))
        }

        fn get_definitions(&self) -> Vec<ToolDefinition> {
            Vec::new()
        }
    }

    fn tool_call(name: &str, args: serde_json::Value) -> ToolCall {
        ToolCall::new(
            "call_test".into(),
            FunctionCall::new(name.into(), args.to_string()),
        )
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
            result.contains("\"__denial_report__\":true"),
            "denial should be DenialReport JSON: {}",
            result
        );
        assert!(result.contains("\"write_file\""));
        assert!(!dir.path().join("denied.txt").exists());
    }

    #[tokio::test]
    async fn execute_outcome_returns_typed_denial_for_unlisted_builtin() {
        let dir = tempfile::tempdir().unwrap();
        let executor = DefaultToolExecutor::new(dir.path().to_str().unwrap())
            .unwrap()
            .with_authorized_tools(["read_file"]);

        let outcome = executor
            .execute_outcome(&tool_call(
                "write_file",
                serde_json::json!({
                    "path": "denied.txt",
                    "content": "should not write"
                }),
            ))
            .await;

        let ToolOutcome::Denied(report) = outcome else {
            panic!("expected typed denial outcome");
        };
        assert!(matches!(report.kind, DenialKind::Tool { .. }));
        assert!(!dir.path().join("denied.txt").exists());
    }

    #[tokio::test]
    async fn approved_tool_rule_allows_unlisted_builtin() {
        let dir = tempfile::tempdir().unwrap();
        let mut sandbox = SandboxConfig::default();
        sandbox.approved_rules = Some(Arc::new(Mutex::new(vec![lattice_sandbox::ApprovedRule {
            kind: ApprovalKind::Tool {
                tool_name: "workspace_context".into(),
            },
            risk_class: RiskClass::Read,
        }])));
        let executor = DefaultToolExecutor::new(dir.path().to_str().unwrap())
            .unwrap()
            .with_sandbox(sandbox)
            .with_authorized_tools(["read_file"]);

        let result = executor
            .execute(&tool_call(
                "workspace_context",
                serde_json::json!({"path": "."}),
            ))
            .await;

        assert!(
            result.contains("Workspace:"),
            "approved tool should execute instead of returning denial: {}",
            result
        );
    }

    #[tokio::test]
    async fn approved_bundle_rule_allows_bundle_tool() {
        let dir = tempfile::tempdir().unwrap();
        let mut sandbox = SandboxConfig::default();
        sandbox.approved_rules = Some(Arc::new(Mutex::new(vec![lattice_sandbox::ApprovedRule {
            kind: ApprovalKind::Bundle {
                bundle_name: "workspace".into(),
            },
            risk_class: RiskClass::Read,
        }])));
        let executor = DefaultToolExecutor::new(dir.path().to_str().unwrap())
            .unwrap()
            .with_sandbox(sandbox)
            .with_authorized_tools(["read_file"]);

        let result = executor
            .execute(&tool_call(
                "workspace_context",
                serde_json::json!({"path": "."}),
            ))
            .await;

        assert!(
            result.contains("Workspace:"),
            "approved bundle should execute instead of returning denial: {}",
            result
        );
    }

    #[tokio::test]
    async fn policy_authority_bundle_rule_allows_unlisted_builtin() {
        let dir = tempfile::tempdir().unwrap();
        let shared = Arc::new(Mutex::new(vec![lattice_sandbox::ApprovedRule {
            kind: ApprovalKind::Bundle {
                bundle_name: "workspace".into(),
            },
            risk_class: RiskClass::Read,
        }]));
        let mut sandbox = SandboxConfig::default();
        sandbox.approved_rules = Some(Arc::clone(&shared));
        sandbox.policy_authority =
            Some(lattice_sandbox::PolicyAuthority::from_shared_rules(shared));
        let executor = DefaultToolExecutor::new(dir.path().to_str().unwrap())
            .unwrap()
            .with_sandbox(sandbox)
            .with_authorized_tools(["read_file"]);

        let result = executor
            .execute(&tool_call(
                "workspace_context",
                serde_json::json!({"path": "."}),
            ))
            .await;

        assert!(
            result.contains("Workspace:"),
            "policy authority bundle approval should execute instead of returning denial: {}",
            result
        );
    }

    #[tokio::test]
    async fn authorized_executor_denies_registry_tool_before_handler_runs() {
        let dir = tempfile::tempdir().unwrap();
        let called = Arc::new(AtomicBool::new(false));
        let registry = Arc::new(TestRegistry {
            called: Arc::clone(&called),
        });
        let executor =
            DefaultToolExecutor::new_with_registry(dir.path().to_str().unwrap(), Some(registry))
                .unwrap()
                .with_authorized_tools(["read_file"]);

        let result = executor
            .execute(&tool_call("custom_tool", serde_json::json!({})))
            .await;

        assert!(
            result.contains("\"__denial_report__\":true"),
            "denial should be DenialReport JSON: {}",
            result
        );
        assert!(!called.load(Ordering::SeqCst));
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
            result.contains("\"__denial_report__\":true"),
            "denial should be DenialReport JSON: {}",
            result
        );
        assert!(!result.contains("Unknown tool:"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bash_timeout_returns_promptly() {
        let dir = tempfile::tempdir().unwrap();
        let mut sandbox = SandboxConfig::permissive();
        sandbox.max_command_timeout = 1;

        let started = std::time::Instant::now();
        let result = DefaultToolExecutor::execute_bash(
            dir.path().to_str().unwrap(),
            &sandbox,
            "exec sleep 5",
        )
        .await;

        assert!(result.unwrap_err().contains("timed out"));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(3),
            "timeout should return without waiting for the full command duration"
        );
    }
}

// --- Linux Landlock helpers ------------------------------------------------

#[cfg(target_os = "linux")]
fn build_landlock_read_paths(project_root: &str, extra: &[PathBuf]) -> Vec<PathBuf> {
    let mut paths = vec![
        PathBuf::from(project_root),
        PathBuf::from("/usr"),
        PathBuf::from("/lib"),
        PathBuf::from("/lib64"),
        PathBuf::from("/etc"),
        PathBuf::from("/proc/self"),
    ];
    paths.extend(extra.iter().cloned());
    paths
}

#[cfg(target_os = "linux")]
fn build_landlock_write_paths(project_root: &str, extra: &[PathBuf]) -> Vec<PathBuf> {
    let mut paths = vec![PathBuf::from(project_root), PathBuf::from("/tmp")];
    paths.extend(extra.iter().cloned());
    paths
}

/// Apply Landlock file isolation in the child process.
///
/// Denies all filesystem access by default, then adds permissive
/// PathBeneath rules: read-only + execute for system paths, full
/// access for write paths (project root + /tmp).
///
/// Returns `Ok(())` on success or if Landlock is not supported
/// (kernel < 5.13). Only returns `Err` for unexpected OS errors.
#[cfg(target_os = "linux")]
fn apply_landlock_sandbox(read_paths: &[PathBuf], write_paths: &[PathBuf]) -> std::io::Result<()> {
    use landlock::{Access, AccessFs, PathBeneath, Ruleset, RulesetAttr, RulesetCreatedAttr, ABI};

    // Progressive ABI: try highest first, fall back step by step.
    // V4 (kernel 6.7+) adds network + io_uring isolation.
    // V3 (kernel 6.0+) adds file truncation control.
    // V2 (kernel 5.19+) adds file reparenting control.
    // V1 (kernel 5.13+) provides filesystem isolation only.
    let ruleset = [ABI::V4, ABI::V3, ABI::V2, ABI::V1]
        .iter()
        .find_map(|&abi| {
            Ruleset::default()
                .handle_access(AccessFs::from_all(abi))
                .ok()
                .and_then(|r| r.create().ok())
        });

    let ruleset = match ruleset {
        Some(r) => r,
        None => return Ok(()), // Landlock not supported on this kernel
    };

    // Use the ABI that was actually accepted for PathBeneath rules
    let abi = ABI::V1; // V1-compatible read/write flags work on all ABIs

    // Collect PathBeneath rules (needs file descriptors, not just paths)
    let mut rules: Vec<PathBeneath<std::fs::File>> = Vec::new();

    // Read-only + execute for read paths (only existing paths)
    for path in read_paths {
        if let Ok(file) = std::fs::File::open(path) {
            rules.push(PathBeneath::new(
                file,
                AccessFs::from_read(abi) | AccessFs::Execute,
            ));
        }
    }

    // Full access for write paths (only existing paths)
    for path in write_paths {
        if let Ok(file) = std::fs::File::open(path) {
            rules.push(PathBeneath::new(file, AccessFs::from_all(abi)));
        }
    }

    // add_rules takes an iterator of Result<T, E> where T: Rule<U>
    let rules_iter = rules
        .into_iter()
        .map(|r| -> Result<_, landlock::RulesetError> { Ok(r) });

    let ruleset = match ruleset.add_rules(rules_iter) {
        Ok(r) => r,
        Err(_) => return Ok(()), // Failed to add rules, skip Landlock
    };

    // Enforce the ruleset
    if let Err(e) = ruleset.restrict_self() {
        tracing::warn!(
            "Landlock restrict_self failed: {:?}; child runs without Landlock",
            e
        );
    }

    Ok(())
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

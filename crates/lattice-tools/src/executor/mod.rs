//! Default tool executor — executes tool calls against the local filesystem.
//!
//! Tool definitions (the schemas that tell the LLM what tools are available)
//! live in [`crate::tool_definitions`]. This module provides the execution
//! layer that runs those tools when the model requests them.

mod auth;
mod fs;
mod path_guard;
mod render;
mod search;
mod shell;
mod web;
mod workspace;

use std::collections::BTreeSet;
use std::path::Path;

use async_trait::async_trait;

use crate::edit::{apply_patch as apply_structured_patch, PatchResultEnvelope};
use crate::tool_error::ToolError;
use crate::ToolExecutor;

const MAX_COMMAND_OUTPUT_SIZE: usize = 200 * 1024;

pub struct DefaultToolExecutor {
    pub(crate) project_root: String,
    pub(crate) limits: ToolLimits,
    http_client: reqwest::Client,
    authorized_tools: BTreeSet<String>,
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

impl DefaultToolExecutor {
    fn build_http_client() -> Result<reqwest::Client, String> {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| format!("failed to build reqwest client: {e}"))
    }

    pub async fn execute_bash(
        project_root: &str,
        limits: &ToolLimits,
        command: &str,
    ) -> Result<std::process::Output, String> {
        shell::execute_bash(project_root, limits, command).await
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
        self.authorized_tools = tool_names.into_iter().map(Into::into).collect();
        self
    }

    fn authorization_denial(&self, tool_name: &str) -> Option<String> {
        if self.authorized_tools.is_empty() {
            return Some(format!(
                "Tool denied: '{}' — no tools are enabled for this agent.",
                tool_name
            ));
        }
        (!self.authorized_tools.contains(tool_name)).then(|| {
            format!(
                "Tool denied: '{}' is not enabled for this agent. Enabled tools: {}",
                tool_name,
                auth::render_enabled_tools(&self.authorized_tools)
            )
        })
    }

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
            authorized_tools: enabled_tool_names.into_iter().map(Into::into).collect(),
        })
    }
}

fn preview_utf8(value: &str, max: usize) -> &str {
    let mut end = value.len().min(max);
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

#[async_trait]
impl ToolExecutor for DefaultToolExecutor {
    async fn execute(&self, call: &lattice_llm::ToolCall) -> Result<String, String> {
        let args: serde_json::Value =
            serde_json::from_str(&call.function.arguments).map_err(|e| {
                format!(
                    "invalid JSON arguments for tool '{}': {} — raw args: {:?}",
                    call.function.name,
                    e,
                    preview_utf8(&call.function.arguments, 200)
                )
            })?;

        if let Some(denial) = self.authorization_denial(&call.function.name) {
            return Err(denial);
        }

        match call.function.name.as_str() {
            "workspace_context" => workspace::workspace_context(self, &args).await,
            "todo_write" => render::render_todos(&args),
            "read_file" => fs::read_file(self, &args).await,
            "grep" => search::grep(self, &args).await,
            "write_file" => fs::write_file(self, &args).await,
            "list_directory" => fs::list_directory(self, &args).await,
            "find_files" => search::find_files(self, &args).await,
            "bash" => {
                let cmd = args.get("command").and_then(|v| v.as_str()).unwrap_or("");
                let output = Self::execute_bash(&self.project_root, &self.limits, cmd)
                    .await
                    .map_err(|e| ToolError::CommandError(e).to_string())?;
                let mut cmd_result = String::from_utf8_lossy(&output.stdout).to_string();
                if !output.stderr.is_empty() {
                    cmd_result.push_str(&format!(
                        "\nERR:{}",
                        String::from_utf8_lossy(&output.stderr)
                    ));
                }
                Ok(render::truncate_output(
                    cmd_result,
                    MAX_COMMAND_OUTPUT_SIZE,
                    "command output",
                ))
            }
            "patch" => {
                let patch = args.get("patch").and_then(|v| v.as_str()).unwrap_or("");
                let envelope: PatchResultEnvelope =
                    apply_structured_patch(Path::new(&self.project_root), &self.limits, patch)
                        .await;
                let body = serde_json::to_string(&envelope).unwrap_or_else(|error| {
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
                });
                if envelope.ok {
                    Ok(body)
                } else {
                    Err(body)
                }
            }
            "web_fetch" => web::web_fetch(&self.http_client, &self.limits, &args).await,
            other => Err(format!("Unknown tool: {other}")),
        }
    }
}

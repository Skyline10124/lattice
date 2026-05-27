//! Default tool executor — executes tool calls against the local filesystem.
//!
//! Tool definitions (the schemas that tell the LLM what tools are available)
//! live in [`crate::tool_definitions`]. This module provides the execution
//! layer that runs those tools when the model requests them.

mod fs;
mod path_guard;
pub(crate) mod render;
mod search;
pub(crate) mod shell;
pub(crate) mod web;

use std::path::Path;

use crate::edit::{apply_patch as apply_structured_patch, PatchResultEnvelope};
use lattice_tooling::ToolLimits;

pub struct DefaultToolExecutor {
    pub(crate) project_root: String,
    pub(crate) limits: ToolLimits,
    pub(crate) http_client: reqwest::Client,
}

impl DefaultToolExecutor {
    pub(crate) fn for_bridge(
        project_root: &str,
        http_client: reqwest::Client,
    ) -> Result<Self, String> {
        let canonical = std::fs::canonicalize(project_root)
            .map_err(|e| format!("cannot canonicalize project root '{}': {}", project_root, e))?;
        Ok(Self {
            project_root: canonical.to_string_lossy().to_string(),
            limits: ToolLimits::default(),
            http_client,
        })
    }
}

pub(crate) async fn apply_patch_body(
    executor: &DefaultToolExecutor,
    patch: &str,
) -> Result<String, String> {
    let envelope: PatchResultEnvelope =
        apply_structured_patch(Path::new(&executor.project_root), &executor.limits, patch).await;
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

impl DefaultToolExecutor {
    pub(crate) async fn bridge_read_file(
        &self,
        args: &serde_json::Value,
    ) -> Result<String, String> {
        fs::read_file(self, args).await
    }

    pub(crate) async fn bridge_grep(&self, args: &serde_json::Value) -> Result<String, String> {
        search::grep(self, args).await
    }

    pub(crate) async fn bridge_list_directory(
        &self,
        args: &serde_json::Value,
    ) -> Result<String, String> {
        fs::list_directory(self, args).await
    }

    pub(crate) async fn bridge_find_files(
        &self,
        args: &serde_json::Value,
    ) -> Result<String, String> {
        search::find_files(self, args).await
    }

    pub(crate) async fn bridge_web_fetch(
        &self,
        args: &serde_json::Value,
    ) -> Result<String, String> {
        web::web_fetch(&self.http_client, &self.limits, args).await
    }

    pub(crate) fn bridge_render_todos(args: &serde_json::Value) -> Result<String, String> {
        render::render_todos(args)
    }

    pub(crate) async fn bridge_execute_argv(
        &self,
        argv: &[String],
        cwd: Option<&str>,
        env: &std::collections::BTreeMap<String, String>,
        timeout_ms: Option<u64>,
    ) -> Result<std::process::Output, String> {
        shell::bridge_execute_argv_compat(
            &self.project_root,
            &self.limits,
            argv,
            cwd,
            env,
            timeout_ms,
        )
        .await
    }

    pub(crate) async fn bridge_write_file(
        &self,
        args: &serde_json::Value,
    ) -> Result<String, String> {
        fs::write_file(self, args).await
    }

    pub(crate) async fn bridge_patch(&self, patch: &str) -> Result<String, String> {
        apply_patch_body(self, patch).await
    }
}

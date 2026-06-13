// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! CubeSandbox backend: RustVMM/KVM microVMs via the E2B-compatible HTTP API.
//!
//! Lifecycle (create / destroy) goes through the local CubeAPI service. Command
//! execution uses `cubecli exec`, which runs the command and propagates its
//! exit code but does **not** forward guest stdout (that needs the envd
//! protocol, out of scope for this MVP) and requires host root for the cubelet
//! control socket. [`CubeWorkspace::capabilities`] states both limits.

use std::process::Output;

use async_trait::async_trait;
use serde_json::json;
use tokio::process::Command;

use crate::{
    Backend, Capabilities, ExecOutput, Result, SandboxHandle, SandboxSpec, Workspace,
    WorkspaceError,
};

/// Drives CubeSandbox through CubeAPI plus the `cubecli` control tool.
pub struct CubeWorkspace {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    cubecli_bin: String,
}

impl CubeWorkspace {
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: base_url.into(),
            api_key: api_key.into(),
            cubecli_bin: "cubecli".to_string(),
        }
    }

    /// Defaults matching the local single-node deployment.
    pub fn from_env() -> Self {
        let base = std::env::var("CUBE_API_URL")
            .or_else(|_| std::env::var("E2B_API_URL"))
            .unwrap_or_else(|_| "http://127.0.0.1:13000".to_string());
        let key = std::env::var("E2B_API_KEY").unwrap_or_else(|_| "e2b_000000".to_string());
        let mut w = Self::new(base, key);
        if let Ok(p) = std::env::var("CUBECLI_BIN") {
            w.cubecli_bin = p;
        }
        w
    }

    fn http_err(msg: impl Into<String>) -> WorkspaceError {
        WorkspaceError::Http {
            backend: Backend::Cube,
            msg: msg.into(),
        }
    }

    fn proc_err(msg: impl Into<String>) -> WorkspaceError {
        WorkspaceError::Process {
            backend: Backend::Cube,
            msg: msg.into(),
        }
    }

    async fn cubecli(&self, args: &[&str]) -> Result<Output> {
        Command::new(&self.cubecli_bin)
            .args(args)
            .output()
            .await
            .map_err(WorkspaceError::Io)
    }
}

/// `cubecli` prints `exec failed with exit code N` rather than propagating N as
/// its own status; recover the guest exit code from that line.
fn parse_guest_exit(stderr: &str) -> Option<i32> {
    let marker = "exit code ";
    let idx = stderr.find(marker)?;
    let digits: String = stderr[idx + marker.len()..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

#[async_trait]
impl Workspace for CubeWorkspace {
    fn backend(&self) -> Backend {
        Backend::Cube
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            exec: true,
            // cubecli does not forward guest stdout; envd would.
            stdout_capture: false,
            // CubeCoW supports both natively; not wired through this MVP adapter.
            disk_snapshot: true,
            memory_snapshot: true,
            needs_root_exec: true,
        }
    }

    async fn create(&self, spec: &SandboxSpec) -> Result<SandboxHandle> {
        let url = format!("{}/sandboxes", self.base_url);
        let resp = self
            .http
            .post(&url)
            .header("X-API-Key", &self.api_key)
            .json(&json!({ "templateID": spec.image, "timeout": spec.timeout_secs }))
            .send()
            .await
            .map_err(|e| Self::http_err(e.to_string()))?;
        if !resp.status().is_success() {
            let code = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(Self::http_err(format!("create {code}: {body}")));
        }
        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| Self::http_err(e.to_string()))?;
        let id = body
            .get("sandboxID")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Self::http_err(format!("no sandboxID in response: {body}")))?
            .to_string();
        Ok(SandboxHandle {
            id,
            backend: Backend::Cube,
        })
    }

    async fn exec(&self, handle: &SandboxHandle, argv: &[&str]) -> Result<ExecOutput> {
        let mut args = vec!["exec", "-i", handle.id.as_str()];
        args.extend_from_slice(argv);
        let out = self.cubecli(&args).await?;
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        let exit_code = if out.status.success() {
            0
        } else if let Some(code) = parse_guest_exit(&stderr) {
            code
        } else {
            return Err(Self::proc_err(format!("cubecli exec failed: {}", stderr.trim())));
        };
        Ok(ExecOutput {
            exit_code,
            // Guest stdout is not forwarded by cubecli; see capabilities().
            stdout: String::new(),
            stderr,
        })
    }

    async fn snapshot(&self, _handle: &SandboxHandle, _name: &str) -> Result<String> {
        Err(WorkspaceError::Unsupported(
            Backend::Cube,
            "CubeCoW snapshot exists natively but is not wired through this adapter yet".into(),
        ))
    }

    async fn destroy(&self, handle: &SandboxHandle) -> Result<()> {
        let url = format!("{}/sandboxes/{}", self.base_url, handle.id);
        let resp = self
            .http
            .delete(&url)
            .header("X-API-Key", &self.api_key)
            .send()
            .await
            .map_err(|e| Self::http_err(e.to_string()))?;
        // Treat already-gone (404) as success: destroy is idempotent.
        if resp.status().is_success() || resp.status().as_u16() == 404 {
            Ok(())
        } else {
            let code = resp.status();
            let body = resp.text().await.unwrap_or_default();
            Err(Self::http_err(format!("destroy {code}: {body}")))
        }
    }
}

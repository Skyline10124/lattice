// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! microsandbox backend: libkrun microVMs driven by the `msb` CLI.
//!
//! libkrun ships its own minimal guest kernel, so this backend boots cleanly on
//! IBT-enforcing hosts (e.g. 12th-gen Intel) with no kernel patching. Full
//! lifecycle is available: `msb run -d` to boot a detached sandbox, `msb exec`
//! to run commands with output capture, `msb snapshot` for disk snapshots.

use std::path::PathBuf;
use std::process::Output;

use async_trait::async_trait;
use tokio::process::Command;

use crate::{
    Backend, Capabilities, ExecOutput, Result, SandboxHandle, SandboxSpec, Workspace,
    WorkspaceError,
};

/// Drives microsandbox through the `msb` binary.
pub struct MicrosandboxWorkspace {
    msb_bin: PathBuf,
}

impl MicrosandboxWorkspace {
    /// Use an explicit `msb` binary path.
    pub fn new(msb_bin: impl Into<PathBuf>) -> Self {
        Self {
            msb_bin: msb_bin.into(),
        }
    }

    /// Resolve `msb` from `$MSB_BIN`, else `$HOME/.microsandbox/bin/msb`.
    pub fn from_env() -> Self {
        if let Ok(p) = std::env::var("MSB_BIN") {
            return Self::new(p);
        }
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
        Self::new(format!("{home}/.microsandbox/bin/msb"))
    }

    async fn run(&self, args: &[&str]) -> Result<Output> {
        Command::new(&self.msb_bin)
            .args(args)
            .output()
            .await
            .map_err(WorkspaceError::Io)
    }

    fn proc_err(msg: impl Into<String>) -> WorkspaceError {
        WorkspaceError::Process {
            backend: Backend::Microsandbox,
            msg: msg.into(),
        }
    }
}

#[async_trait]
impl Workspace for MicrosandboxWorkspace {
    fn backend(&self) -> Backend {
        Backend::Microsandbox
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            exec: true,
            stdout_capture: true,
            disk_snapshot: true,
            // libkrun snapshots are disk-only in this CLI; no live-memory
            // snapshot of a running VM.
            memory_snapshot: false,
            needs_root_exec: false,
        }
    }

    async fn create(&self, spec: &SandboxSpec) -> Result<SandboxHandle> {
        // `msb run -d <image>` boots a detached sandbox and prints its name.
        let out = self.run(&["run", "-d", &spec.image]).await?;
        if !out.status.success() {
            return Err(Self::proc_err(format!(
                "msb run -d failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        let name = String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::trim)
            .rfind(|l| !l.is_empty())
            .unwrap_or_default()
            .to_string();
        if name.is_empty() {
            return Err(Self::proc_err("msb run -d returned no sandbox name"));
        }
        Ok(SandboxHandle {
            id: name,
            backend: Backend::Microsandbox,
        })
    }

    async fn exec(&self, handle: &SandboxHandle, argv: &[&str]) -> Result<ExecOutput> {
        // `msb exec <name> -- <argv...>`
        let mut args = vec!["exec", handle.id.as_str(), "--"];
        args.extend_from_slice(argv);
        let out = self.run(&args).await?;
        Ok(ExecOutput {
            exit_code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }

    async fn snapshot(&self, handle: &SandboxHandle, name: &str) -> Result<String> {
        // Snapshot requires a stopped sandbox; stop it first, then capture.
        let _ = self.run(&["stop", &handle.id]).await;
        let out = self
            .run(&["snapshot", "create", "--from", &handle.id, name, "-f"])
            .await?;
        if !out.status.success() {
            return Err(Self::proc_err(format!(
                "msb snapshot create failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(name.to_string())
    }

    async fn destroy(&self, handle: &SandboxHandle) -> Result<()> {
        // Stop then remove; both are best-effort so destroy stays idempotent.
        let _ = self.run(&["stop", &handle.id]).await;
        let _ = self.run(&["rm", &handle.id]).await;
        Ok(())
    }
}

// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Feature-gated test doubles for the workspace adapter seam.
//!
//! Available only with `#[cfg(any(test, feature = "test-utils"))]`.
//!
//! # DeterministicWorkspace
//!
//! A CI-safe adapter that uses `git2` + `tempfile::TempDir`; no KVM or
//! microsandbox SDK required.  It is suitable for unit and integration tests
//! that want to exercise the [`Workspace`] trait contract without a
//! hypervisor.
//!
//! ## Egress policy
//!
//! Records the requested [`EgressPolicy`] (inspectable via
//! [`DeterministicWorkspace::requested_policy`]) but does **not** enforce
//! network blocking — commands run on the host network.  Contract tests
//! assert the policy is *recorded*, not that the host network is blocked.
//! Live enforcement is verified on the KVM integration lane (V-08).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;

use crate::{
    Backend, DirectoryEntry, EgressPolicy, ExecOutput, ExecRequest, FindRequest, GitStatus,
    GrepMatch, GrepRequest, PathFacts, PathIntent, Result, UpperHandle, UpperSpec, Workspace,
    WorkspaceError, WorkspacePath,
};

pub mod contract_tests;

/// In-process test adapter: `git2` + tempdir; no KVM required.
///
/// Thread-safe via `Arc<RwLock<...>>` internal maps.
pub struct DeterministicWorkspace {
    base_dir: Arc<tempfile::TempDir>,
    handles: Arc<RwLock<HashMap<String, PathBuf>>>,
    policies: Arc<RwLock<HashMap<String, EgressPolicy>>>,
}

impl DeterministicWorkspace {
    /// Create a new workspace backed by a fresh OS tempdir.
    pub fn new() -> std::io::Result<Self> {
        Ok(Self {
            base_dir: Arc::new(tempfile::TempDir::new()?),
            handles: Arc::new(RwLock::new(HashMap::new())),
            policies: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// Inspect the egress policy recorded for a handle.
    ///
    /// Returns `None` if the handle has already been destroyed.
    pub fn requested_policy(&self, handle: &UpperHandle) -> Option<EgressPolicy> {
        self.policies
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&handle.id)
            .cloned()
    }

    fn git_err(e: impl std::fmt::Display) -> WorkspaceError {
        WorkspaceError::Git(e.to_string())
    }

    fn handle_dir(&self, handle: &UpperHandle) -> Result<PathBuf> {
        self.handles
            .read()
            .unwrap()
            .get(&handle.id)
            .cloned()
            .ok_or_else(|| WorkspaceError::Sdk(format!("det handle not found: {}", handle.id)))
    }
}

#[async_trait]
impl Workspace for DeterministicWorkspace {
    // ── Upper methods ─────────────────────────────────────────────────────────

    /// Provision an Upper: checkout `spec.baseline_oid` into a fresh tempdir
    /// subdir using `git2`, then `git init` + stage + commit the tree so the
    /// subdir is a standalone git repository.
    ///
    /// The `EgressPolicy` is recorded (inspectable via `requested_policy`) but
    /// NOT enforced — commands run on the host network in this adapter.
    async fn create_upper(&self, spec: &UpperSpec) -> Result<UpperHandle> {
        crate::validate_baseline_oid(&spec.baseline_oid)?;

        // Allocate a unique subdir under base_dir.
        let uid = uuid::Uuid::new_v4().simple().to_string();
        let dir = self.base_dir.path().join(&uid);
        std::fs::create_dir_all(&dir).map_err(WorkspaceError::Io)?;

        // Checkout the baseline tree into the new dir (no .git metadata).
        crate::git::checkout_tree_to_dir(&spec.host_repo_path, &spec.baseline_oid, &dir)?;

        // Init a new git repo in the dir and commit the checked-out tree.
        let repo = git2::Repository::init(&dir).map_err(Self::git_err)?;
        let sig = git2::Signature::now("lattice-det", "det@localhost").map_err(Self::git_err)?;
        let mut index = repo.index().map_err(Self::git_err)?;
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .map_err(Self::git_err)?;
        index.write().map_err(Self::git_err)?;
        let tree_id = index.write_tree().map_err(Self::git_err)?;
        let tree = repo.find_tree(tree_id).map_err(Self::git_err)?;
        repo.commit(
            Some("HEAD"),
            &sig,
            &sig,
            &format!("baseline:{}", spec.baseline_oid),
            &tree,
            &[],
        )
        .map_err(Self::git_err)?;

        // Register handle.
        let id = format!("det-{}", &uid[..8]);
        self.handles
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.clone(), dir);
        self.policies
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.clone(), spec.egress.clone());

        Ok(UpperHandle {
            id,
            backend: Backend::Deterministic,
        })
    }

    /// Run `prog args` inside the handle's directory on the host.
    ///
    /// Egress is NOT enforced; this is CI-safe (no network blocking needed).
    async fn exec_upper(
        &self,
        handle: &UpperHandle,
        prog: &str,
        args: &[&str],
    ) -> Result<ExecOutput> {
        let dir = self
            .handles
            .read()
            .unwrap()
            .get(&handle.id)
            .cloned()
            .ok_or_else(|| WorkspaceError::Sdk(format!("det handle not found: {}", handle.id)))?;
        let out = std::process::Command::new(prog)
            .args(args)
            .current_dir(&dir)
            .output()
            .map_err(WorkspaceError::Io)?;
        Ok(ExecOutput {
            exit_code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }

    /// Run `git status --porcelain=v1` via `exec_upper` and parse the result.
    async fn git_status(&self, handle: &UpperHandle) -> Result<GitStatus> {
        let out = self
            .exec_upper(handle, "git", &["status", "--porcelain=v1"])
            .await?;
        Ok(GitStatus {
            porcelain: out.stdout.clone(),
            is_clean: out.stdout.trim().is_empty(),
        })
    }

    /// Copy the handle's directory to a named sibling under `base_dir`.
    ///
    /// The template name is returned (mirrors `Workspace::template_create`
    /// semantics).  Credentials are **not** captured in the copy (T-33-04).
    async fn template_create(&self, source: &UpperHandle, name: &str) -> Result<String> {
        let src_dir = self
            .handles
            .read()
            .unwrap()
            .get(&source.id)
            .cloned()
            .ok_or_else(|| WorkspaceError::Sdk(format!("det handle not found: {}", source.id)))?;
        let dest = self.base_dir.path().join(name);
        copy_dir_all(&src_dir, &dest).map_err(WorkspaceError::Io)?;
        Ok(name.to_string())
    }

    /// Boot a new Upper from an existing template (directory copy).
    async fn restore_upper(
        &self,
        template_name: &str,
        _new_name: &str,
        spec: &UpperSpec,
    ) -> Result<UpperHandle> {
        crate::validate_baseline_oid(&spec.baseline_oid)?;
        let template_dir = self.base_dir.path().join(template_name);
        if !template_dir.exists() {
            return Err(WorkspaceError::Sdk(format!(
                "template '{template_name}' not found"
            )));
        }
        let uid = uuid::Uuid::new_v4().simple().to_string();
        let dest = self.base_dir.path().join(&uid);
        copy_dir_all(&template_dir, &dest).map_err(WorkspaceError::Io)?;

        let id = format!("det-{}", &uid[..8]);
        self.handles
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.clone(), dest);
        self.policies
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.clone(), spec.egress.clone());

        Ok(UpperHandle {
            id,
            backend: Backend::Deterministic,
        })
    }

    /// Remove a handle from the maps (idempotent; actual tempdir cleanup
    /// happens when `DeterministicWorkspace` drops).
    async fn destroy_upper(&self, handle: &UpperHandle) -> Result<()> {
        self.handles
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&handle.id);
        self.policies
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&handle.id);
        Ok(())
    }

    async fn resolve_path(
        &self,
        handle: &UpperHandle,
        path: &WorkspacePath,
        intent: PathIntent,
    ) -> Result<PathFacts> {
        let dir = self.handle_dir(handle)?;
        crate::primitives::resolve_path_facts(&dir, path, intent)
    }

    async fn read_file(&self, handle: &UpperHandle, path: &WorkspacePath) -> Result<Vec<u8>> {
        let dir = self.handle_dir(handle)?;
        crate::primitives::read_file_bytes(&dir, path)
    }

    async fn write_file(
        &self,
        handle: &UpperHandle,
        path: &WorkspacePath,
        content: &[u8],
    ) -> Result<()> {
        let dir = self.handle_dir(handle)?;
        crate::primitives::write_file_bytes(&dir, path, content)
    }

    async fn remove_file(&self, handle: &UpperHandle, path: &WorkspacePath) -> Result<()> {
        let dir = self.handle_dir(handle)?;
        crate::primitives::remove_file_path(&dir, path)
    }

    async fn list_dir(
        &self,
        handle: &UpperHandle,
        path: &WorkspacePath,
    ) -> Result<Vec<DirectoryEntry>> {
        let dir = self.handle_dir(handle)?;
        crate::primitives::list_dir_entries(&dir, path)
    }

    async fn grep(&self, handle: &UpperHandle, request: &GrepRequest) -> Result<Vec<GrepMatch>> {
        let dir = self.handle_dir(handle)?;
        crate::primitives::grep_tree(&dir, request)
    }

    async fn find_files(
        &self,
        handle: &UpperHandle,
        request: &FindRequest,
    ) -> Result<Vec<WorkspacePath>> {
        let dir = self.handle_dir(handle)?;
        crate::primitives::find_files_tree(&dir, request)
    }

    async fn exec(&self, handle: &UpperHandle, request: &ExecRequest) -> Result<ExecOutput> {
        let dir = self.handle_dir(handle)?;
        crate::primitives::exec_on_host(&dir, request)
    }
}

/// Recursively copy a directory tree.
fn copy_dir_all(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_all(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

// ── DeterministicWorkspace unit tests ──────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Create a minimal git repo in a tempdir with one committed file.
    /// Returns (TempDir, baseline_oid).
    fn make_fixture_repo() -> (tempfile::TempDir, String) {
        let dir = tempfile::TempDir::new().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        let sig = git2::Signature::now("tester", "t@t.com").unwrap();
        // Write a file and commit it
        let file_path = dir.path().join("hello.txt");
        std::fs::write(&file_path, b"hello from fixture\n").unwrap();
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let oid = repo
            .commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();
        (dir, oid.to_string())
    }

    fn make_spec(repo_path: PathBuf, oid: String) -> UpperSpec {
        UpperSpec {
            image_or_template: crate::ImageOrTemplate::Image(
                "docker.m.daocloud.io/library/alpine:3.19".into(),
            ),
            baseline_oid: oid,
            host_repo_path: repo_path,
            egress: EgressPolicy::Blocked,
            credentials: vec![],
            timeout_secs: 30,
            cpus: 1,
            memory_mb: 256,
        }
    }

    #[tokio::test]
    async fn test_deterministic_create_upper_returns_handle() {
        let (repo_dir, oid) = make_fixture_repo();
        let ws = DeterministicWorkspace::new().unwrap();
        let spec = make_spec(repo_dir.path().to_owned(), oid);
        let handle = ws
            .create_upper(&spec)
            .await
            .expect("create_upper must succeed");
        assert!(!handle.id.is_empty());
        assert_eq!(handle.backend, Backend::Deterministic);
    }

    #[tokio::test]
    async fn test_deterministic_exec_upper_echo() {
        let (repo_dir, oid) = make_fixture_repo();
        let ws = DeterministicWorkspace::new().unwrap();
        let spec = make_spec(repo_dir.path().to_owned(), oid);
        let handle = ws.create_upper(&spec).await.unwrap();
        let out = ws.exec_upper(&handle, "echo", &["hello"]).await.unwrap();
        assert_eq!(out.exit_code, 0);
        assert!(out.stdout.contains("hello"));
    }

    #[tokio::test]
    async fn test_deterministic_destroy_is_idempotent() {
        let (repo_dir, oid) = make_fixture_repo();
        let ws = DeterministicWorkspace::new().unwrap();
        let spec = make_spec(repo_dir.path().to_owned(), oid);
        let handle = ws.create_upper(&spec).await.unwrap();
        ws.destroy_upper(&handle).await.unwrap();
        // Second call must also succeed (idempotent).
        ws.destroy_upper(&handle).await.unwrap();
    }

    #[tokio::test]
    async fn test_deterministic_requested_policy() {
        let (repo_dir, oid) = make_fixture_repo();
        let ws = DeterministicWorkspace::new().unwrap();
        let spec = make_spec(repo_dir.path().to_owned(), oid);
        let handle = ws.create_upper(&spec).await.unwrap();
        let recorded = ws.requested_policy(&handle);
        assert!(
            matches!(recorded, Some(EgressPolicy::Blocked)),
            "expected Blocked, got {:?}",
            recorded
        );
    }

    #[tokio::test]
    async fn test_deterministic_git_status_clean() {
        let (repo_dir, oid) = make_fixture_repo();
        let ws = DeterministicWorkspace::new().unwrap();
        let spec = make_spec(repo_dir.path().to_owned(), oid);
        let handle = ws.create_upper(&spec).await.unwrap();
        let status = ws.git_status(&handle).await.unwrap();
        assert!(
            status.is_clean,
            "fresh upper should be clean; porcelain: {:?}",
            status.porcelain
        );
    }
}

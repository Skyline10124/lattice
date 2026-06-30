// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Shared helpers for lattice-api integration tests that exercise Upper routing.

use std::path::Path;
use std::sync::Arc;

use lattice_api::RunTurnRequest;
use lattice_workspace::Workspace;
use lattice_workspace::testing::DeterministicWorkspace;

pub fn init_git_repo(root: &Path) {
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(root)
        .status()
        .expect("git init");
    std::process::Command::new("git")
        .args(["config", "user.email", "test@lattice.local"])
        .current_dir(root)
        .status()
        .expect("git user.email");
    std::process::Command::new("git")
        .args(["config", "user.name", "lattice-test"])
        .current_dir(root)
        .status()
        .expect("git user.name");
    std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(root)
        .status()
        .expect("git add");
    std::process::Command::new("git")
        .args(["commit", "-q", "-m", "init"])
        .current_dir(root)
        .status()
        .expect("git commit");
}

pub fn deterministic_workspace() -> Arc<dyn Workspace> {
    Arc::new(DeterministicWorkspace::new().expect("deterministic workspace for integration tests"))
}

pub fn attach_upper_fixture(
    mut request: RunTurnRequest,
    _enabled_tools: &[&str],
) -> RunTurnRequest {
    if request.workspace_override.is_none() {
        request.workspace_override = Some(deterministic_workspace());
    }
    request
}

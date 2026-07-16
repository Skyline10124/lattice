// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Shared helpers for lattice-api integration tests.
//!
//! #37 removed the Upper workspace override surface; these helpers now only
//! provide the git-repo bootstrap used by Direct-Local test fixtures.

use std::path::Path;

use lattice_api::RunTurnRequest;

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

/// No-op request normalizer retained for callers that previously attached an
/// Upper fixture. Direct-Local builds its own execution routing internally,
/// so there is nothing to inject.
pub fn attach_upper_fixture(
    request: RunTurnRequest,
    _enabled_tools: &[&str],
) -> RunTurnRequest {
    request
}

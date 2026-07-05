// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::path::PathBuf;

/// Workspace root (parent of the `xtask` crate directory).
pub fn repo_root() -> anyhow::Result<PathBuf> {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    Ok(manifest_dir
        .parent()
        .ok_or_else(|| anyhow::anyhow!("xtask manifest has no parent directory"))?
        .to_path_buf())
}

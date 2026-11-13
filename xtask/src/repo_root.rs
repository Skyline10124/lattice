// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::path::PathBuf;

/// workspace 根目录，即 `xtask` crate 目录的上一级。
pub fn repo_root() -> anyhow::Result<PathBuf> {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    Ok(manifest_dir
        .parent()
        .ok_or_else(|| anyhow::anyhow!("`xtask` crate 目录没有上级目录"))?
        .to_path_buf())
}

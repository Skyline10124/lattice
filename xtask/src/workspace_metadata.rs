// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::path::Path;

use cargo_metadata::{CargoOpt, Metadata, MetadataCommand};

/// 读取包含完整解析图（`resolve`）的 workspace metadata。
pub fn load_full(repo_root: &Path) -> anyhow::Result<Metadata> {
    MetadataCommand::new()
        .current_dir(repo_root)
        .exec()
        .map_err(|e| anyhow::anyhow!("failed to read workspace metadata: {e}"))
}

/// 读取启用全部 workspace feature、包含完整解析图（`resolve`）的 workspace metadata。
pub fn load_full_all_features(repo_root: &Path) -> anyhow::Result<Metadata> {
    MetadataCommand::new()
        .current_dir(repo_root)
        .features(CargoOpt::AllFeatures)
        .exec()
        .map_err(|e| anyhow::anyhow!("failed to read workspace metadata (--all-features): {e}"))
}

/// 读取仅包含 workspace 成员声明（`--no-deps`）的 metadata。
pub fn load_no_deps(repo_root: &Path) -> anyhow::Result<Metadata> {
    MetadataCommand::new()
        .current_dir(repo_root)
        .no_deps()
        .exec()
        .map_err(|e| anyhow::anyhow!("cargo metadata --no-deps --format-version 1 failed: {e}"))
}

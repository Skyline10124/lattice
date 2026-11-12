// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::path::Path;

use cargo_metadata::{Metadata, MetadataCommand};

/// 读取包含完整解析图（`resolve`）的 workspace metadata。
pub fn load_full(repo_root: &Path) -> anyhow::Result<Metadata> {
    MetadataCommand::new()
        .current_dir(repo_root)
        .exec()
        .map_err(|e| anyhow::anyhow!("读取 workspace metadata 失败：{e}"))
}

/// 读取仅包含 workspace 成员声明（`--no-deps`）的 metadata。
pub fn load_no_deps(repo_root: &Path) -> anyhow::Result<Metadata> {
    MetadataCommand::new()
        .current_dir(repo_root)
        .no_deps()
        .exec()
        .map_err(|e| anyhow::anyhow!("读取 workspace metadata（--no-deps）失败：{e}"))
}

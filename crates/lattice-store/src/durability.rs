// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 文件与目录 `fsync` 持久性屏障。
//!
//! 每次屏障失败都返回类型化的 [`StoreError::Durability`]，写入路径上的持久性不降级为尽力而为。

use std::fs::File;
use std::path::Path;

use crate::StoreError;

/// fsync one file's data and metadata.
pub fn fsync_file(path: &Path) -> Result<(), StoreError> {
    let file = File::open(path).map_err(|source| StoreError::Io {
        operation: "open for fsync",
        path: path.to_path_buf(),
        source,
    })?;
    file.sync_all().map_err(|source| StoreError::Durability {
        operation: "fsync file",
        path: path.to_path_buf(),
        source,
    })
}

/// fsync one directory entry point (Linux allows opening directories
/// read-only for exactly this purpose).
pub fn fsync_dir(path: &Path) -> Result<(), StoreError> {
    let dir = File::open(path).map_err(|source| StoreError::Io {
        operation: "open directory for fsync",
        path: path.to_path_buf(),
        source,
    })?;
    dir.sync_all().map_err(|source| StoreError::Durability {
        operation: "fsync directory",
        path: path.to_path_buf(),
        source,
    })
}

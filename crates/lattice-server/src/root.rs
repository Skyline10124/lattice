// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 本地工作区根目录的后台解析与规范化入口（ADR 0005 §3、ADR 0012 §8）。
//!
//! CLI 与 TUI 发送原始进程工作目录（或显式传入的路径），由后台统一通过本解析器做规范化与合法性校验，
//! 不存在第二套绑定启发式或惰性首轮绑定路径；会话绑定的项目路径直接取规范化工作区根目录的原始字节。

use std::path::{Path, PathBuf};

use lattice_session::ProjectPath;

use crate::session::ApiError;

/// 已显式解析并规范化的本地工作区根目录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalWorkspaceRoot {
    canonical: PathBuf,
}

impl LocalWorkspaceRoot {
    /// 解析并规范化显式传入的工作区根路径。
    pub fn from_path(path: &Path) -> std::result::Result<Self, ApiError> {
        let canonical = std::fs::canonicalize(path).map_err(|source| {
            ApiError::BadRequest(format!(
                "cannot canonicalize workspace root '{}': {source}",
                path.display()
            ))
        })?;
        ProjectPath::from_path(&canonical)
            .map_err(|err| ApiError::Internal(format!("规范化工作区路径非法: {err}")))?;
        Ok(Self { canonical })
    }

    /// 已规范化的工作区根路径。
    pub fn canonical_path(&self) -> &Path {
        &self.canonical
    }

    /// 会话绑定使用的规范项目路径原始字节。
    pub fn project_path(&self) -> ProjectPath {
        ProjectPath::from_path(&self.canonical).expect("已校验的规范项目路径")
    }
}

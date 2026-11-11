// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 本地工作区根目录的唯一解析入口（ADR 0005 §3、ADR 0012 §8）。
//!
//! CLI 与 TUI 统一通过本解析器获取当前本地工作区规范根目录，不存在第二套绑定启发式或惰性首轮绑定路径；
//! 会话绑定的项目路径直接取规范化工作区根目录的原始字节。

use std::path::{Path, PathBuf};

use lattice_session::ProjectPath;

use crate::session::{ApiError, Result};

/// 已显式解析并规范化的本地工作区根目录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalWorkspaceRoot {
    canonical: PathBuf,
}

impl LocalWorkspaceRoot {
    /// Resolve the current Local Workspace root from the process working
    /// directory (the production resolution).
    pub fn resolve() -> Result<Self> {
        let cwd = std::env::current_dir().map_err(|source| {
            ApiError::Internal(format!("cannot read process working directory: {source}"))
        })?;
        Self::from_path(&cwd)
    }

    /// Resolve from an explicit path (tests, embedders).
    pub fn from_path(path: &Path) -> Result<Self> {
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

    /// The canonical root path.
    pub fn canonical_path(&self) -> &Path {
        &self.canonical
    }

    /// 会话绑定使用的规范项目路径原始字节。
    pub fn project_path(&self) -> ProjectPath {
        ProjectPath::from_path(&self.canonical).expect("已校验的规范项目路径")
    }
}

/// Resolve the current Local Workspace root (see [`LocalWorkspaceRoot`]).
pub fn resolve_local_workspace_root() -> Result<LocalWorkspaceRoot> {
    LocalWorkspaceRoot::resolve()
}

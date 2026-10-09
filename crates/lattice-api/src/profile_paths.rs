// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::path::{Path, PathBuf};

use crate::ApiError;
use crate::config_rejection::{AgentsDirectoryRule, ConfigRejection, Reason};

/// 提交端检查 Agent 目录；纯找回和观察不得以新目录配置挡住既有状态。
pub fn validate_agents_dir(path: &Path) -> Result<(), ApiError> {
    if path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(directory_rejection(path, AgentsDirectoryRule::Traversal));
    }
    let canonical = canonicalize_agents_dir_prefix(path).map_err(ApiError::BadRequest)?;
    for prefix in ["/etc", "/usr", "/var", "/sys", "/proc", "/dev", "/boot"] {
        if canonical.starts_with(prefix) {
            return Err(directory_rejection(
                path,
                AgentsDirectoryRule::System(prefix),
            ));
        }
    }
    let tmp_root = Path::new("/tmp")
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from("/tmp"));
    if canonical.starts_with(tmp_root) {
        return Err(directory_rejection(path, AgentsDirectoryRule::Temporary));
    }
    Ok(())
}

fn directory_rejection(path: &Path, rule: AgentsDirectoryRule) -> ApiError {
    ApiError::Configuration(ConfigRejection(Reason::AgentsDirectory {
        path: path.to_owned(),
        rule,
    }))
}

/// 不存在的尾段可保留，但先解析既有祖先的符号链接，不能绕过目录安全边界。
pub fn canonicalize_agents_dir_prefix(path: &Path) -> Result<PathBuf, String> {
    if path.as_os_str().is_empty() {
        return Ok(PathBuf::new());
    }
    if path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(format!(
            "Agents directory '{}' contains path traversal.",
            path.display()
        ));
    }
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()
            .map_err(|error| format!("cannot resolve current directory: {error}"))?
            .join(path)
    };
    let mut cursor = absolute.as_path();
    let mut missing = Vec::new();
    loop {
        match cursor.canonicalize() {
            Ok(mut canonical) => {
                for component in missing.iter().rev() {
                    canonical.push(component);
                }
                return Ok(canonical);
            }
            Err(error) => {
                let file_name = cursor.file_name().ok_or_else(|| {
                    format!(
                        "cannot resolve agents directory '{}': {error}",
                        path.display()
                    )
                })?;
                missing.push(file_name.to_os_string());
                cursor = cursor.parent().ok_or_else(|| {
                    format!(
                        "cannot resolve agents directory '{}': {error}",
                        path.display()
                    )
                })?;
            }
        }
    }
}

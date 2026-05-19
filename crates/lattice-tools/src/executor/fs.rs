use std::path::PathBuf;

use crate::edit::content_hash;
use crate::tool_error::ToolError;
use crate::DefaultToolExecutor;

use super::render;

const MAX_BATCH_READ_FILES: usize = 20;
const DEFAULT_LIST_LIMIT: usize = 200;
const MAX_LIST_LIMIT: usize = 1000;

impl DefaultToolExecutor {
    pub(super) async fn read_text_file(
        &self,
        path: &str,
        offset: usize,
        limit: usize,
        line_numbers: bool,
    ) -> Result<String, String> {
        Self::reject_parent_traversal(path)?;
        let requested = self.resolve_user_path(path);
        let canonical = tokio::fs::canonicalize(&requested).await.map_err(|e| {
            ToolError::IoError {
                path: path.to_string(),
                error: e,
            }
            .to_string()
        })?;
        self.ensure_under_project_root(&canonical)?;
        match tokio::fs::metadata(&canonical).await {
            Ok(meta) if meta.len() > self.limits.max_read_size as u64 => {
                return Err(format!(
                    "file size {} exceeds max_read_size {}",
                    meta.len(),
                    self.limits.max_read_size
                ));
            }
            Err(e) => {
                return Err(ToolError::IoError {
                    path: path.to_string(),
                    error: e,
                }
                .to_string());
            }
            _ => {}
        }
        let content = tokio::fs::read_to_string(&canonical).await.map_err(|e| {
            ToolError::IoError {
                path: path.to_string(),
                error: e,
            }
            .to_string()
        })?;
        let rendered = render::render_file_slice(&content, offset, limit, line_numbers);
        let hash = content_hash(content.as_bytes());
        if rendered.is_empty() {
            Ok(format!("Content hash: {hash}"))
        } else {
            Ok(format!("{rendered}\nContent hash: {hash}"))
        }
    }
}

pub(super) async fn read_file(
    executor: &DefaultToolExecutor,
    args: &serde_json::Value,
) -> Result<String, String> {
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
    let offset = args
        .get("offset")
        .and_then(|v| v.as_u64())
        .and_then(|v| usize::try_from(v).ok())
        .unwrap_or(1)
        .max(1);
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .and_then(|v| usize::try_from(v).ok())
        .unwrap_or(2000)
        .max(1);
    let line_numbers = args
        .get("line_numbers")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    executor
        .read_text_file(path, offset, limit, line_numbers)
        .await
}

pub(super) async fn read_files(
    executor: &DefaultToolExecutor,
    args: &serde_json::Value,
) -> Result<String, String> {
    let files = match args.get("files").and_then(|v| v.as_array()) {
        Some(files) if !files.is_empty() => files,
        _ => return Err("read_files requires a non-empty files array".to_string()),
    };
    if files.len() > MAX_BATCH_READ_FILES {
        return Err(format!(
            "read_files accepts at most {} files per call",
            MAX_BATCH_READ_FILES
        ));
    }

    let mut sections = Vec::new();
    for file in files {
        let path = file.get("path").and_then(|v| v.as_str()).unwrap_or("");
        let offset = file
            .get("offset")
            .and_then(|v| v.as_u64())
            .and_then(|v| usize::try_from(v).ok())
            .unwrap_or(1)
            .max(1);
        let limit = file
            .get("limit")
            .and_then(|v| v.as_u64())
            .and_then(|v| usize::try_from(v).ok())
            .unwrap_or(2000)
            .max(1);
        let line_numbers = file
            .get("line_numbers")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let content = match executor
            .read_text_file(path, offset, limit, line_numbers)
            .await
        {
            Ok(c) => c,
            Err(e) => format!("Error: {e}"),
        };
        sections.push(format!("--- {path} ---\n{content}"));
    }
    Ok(sections.join("\n\n"))
}

pub(super) async fn write_file(
    executor: &DefaultToolExecutor,
    args: &serde_json::Value,
) -> Result<String, String> {
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
    DefaultToolExecutor::reject_parent_traversal(path)?;
    let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
    let abs = PathBuf::from(&executor.project_root).join(path.trim_start_matches('/'));
    let write_path = if let Some(parent) = abs.parent() {
        let canonical_parent = tokio::fs::canonicalize(parent).await.map_err(|_| {
            format!(
                "cannot write to '{}' because parent directory does not exist or cannot be resolved",
                path
            )
        })?;
        let canonical = canonical_parent.join(abs.file_name().unwrap());
        executor.ensure_under_project_root(&canonical)?;
        canonical
    } else {
        abs.clone()
    };
    if content.len() > executor.limits.max_write_size {
        return Err(ToolError::SizeLimit {
            limit: executor.limits.max_write_size,
            actual: content.len(),
        }
        .to_string());
    }
    let path_str = write_path.to_string_lossy().to_string();
    tokio::fs::write(&write_path, content).await.map_err(|e| {
        ToolError::IoError {
            path: path_str,
            error: e,
        }
        .to_string()
    })?;
    Ok(format!("Wrote {} bytes to {}", content.len(), path))
}

pub(super) async fn list_directory(
    executor: &DefaultToolExecutor,
    args: &serde_json::Value,
) -> Result<String, String> {
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
    DefaultToolExecutor::reject_parent_traversal(path)?;
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .and_then(|v| usize::try_from(v).ok())
        .unwrap_or(DEFAULT_LIST_LIMIT)
        .clamp(1, MAX_LIST_LIMIT);
    let requested = executor.resolve_user_path(path);
    let canonical = tokio::fs::canonicalize(&requested).await.map_err(|e| {
        ToolError::IoError {
            path: path.to_string(),
            error: e,
        }
        .to_string()
    })?;
    executor.ensure_under_project_root(&canonical)?;
    let mut entries = tokio::fs::read_dir(&canonical).await.map_err(|e| {
        ToolError::IoError {
            path: path.to_string(),
            error: e,
        }
        .to_string()
    })?;
    let mut files = Vec::new();
    loop {
        match entries.next_entry().await {
            Ok(Some(entry)) => {
                let ty = if entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false) {
                    "DIR"
                } else {
                    "FILE"
                };
                files.push(format!("{}  {}", ty, entry.file_name().to_string_lossy()));
            }
            Ok(None) => break,
            Err(e) => {
                files.push(format!("Error reading entry: {}", e));
            }
        }
    }
    files.sort();
    let total = files.len();
    let mut visible: Vec<String> = files.into_iter().take(limit).collect();
    if total > visible.len() {
        visible.push(format!(
            "[TRUNCATED: showing {} of {} entries; pass a narrower path or higher limit]",
            visible.len(),
            total
        ));
    }
    Ok(visible.join("\n"))
}

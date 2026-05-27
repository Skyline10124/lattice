use std::path::{Component, Path, PathBuf};

use super::DefaultToolExecutor;
use crate::edit::content_hash;
use lattice_tooling::ToolError;

use super::render;

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
        // M-07: re-check actual content length to defeat metadata→read TOCTOU
        if content.len() > self.limits.max_read_size {
            return Err(format!(
                "file content {} exceeds max_read_size {} (file may have grown during read)",
                content.len(),
                self.limits.max_read_size
            ));
        }
        let rendered = render::render_file_slice(&content, offset, limit, line_numbers);
        let hash = content_hash(content.as_bytes());
        if rendered.is_empty() {
            Ok(format!("Content hash: {hash}"))
        } else {
            Ok(format!("{rendered}\nContent hash: {hash}"))
        }
    }
}

pub(crate) async fn read_file(
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

pub(crate) async fn write_file(
    executor: &DefaultToolExecutor,
    args: &serde_json::Value,
) -> Result<String, String> {
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
    DefaultToolExecutor::reject_parent_traversal(path)?;
    let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
    if content.len() > executor.limits.max_write_size {
        return Err(ToolError::SizeLimit {
            limit: executor.limits.max_write_size,
            actual: content.len(),
        }
        .to_string());
    }
    let relative_path = sanitize_write_path(path)?;
    write_file_no_symlink(
        PathBuf::from(&executor.project_root),
        relative_path,
        content.to_string(),
        path.to_string(),
    )
    .await?;
    Ok(format!("Wrote {} bytes to {}", content.len(), path))
}

fn sanitize_write_path(path: &str) -> Result<PathBuf, String> {
    let trimmed = path.trim_start_matches('/');
    let mut relative = PathBuf::new();
    for component in Path::new(trimmed).components() {
        match component {
            Component::Normal(part) => relative.push(part),
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(format!(
                    "path traversal denied: '{}' contains '..' component",
                    path
                ));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(format!("cannot write to '{}': invalid path", path));
            }
        }
    }

    if relative.as_os_str().is_empty() {
        return Err(format!("cannot write to '{}': invalid path", path));
    }

    Ok(relative)
}

#[cfg(unix)]
async fn write_file_no_symlink(
    project_root: PathBuf,
    relative_path: PathBuf,
    content: String,
    display_path: String,
) -> Result<PathBuf, String> {
    let write_path = project_root.join(&relative_path);
    tokio::task::spawn_blocking(move || {
        secure_write_file_unix(
            &project_root,
            &relative_path,
            content.as_bytes(),
            &display_path,
        )?;
        Ok(write_path)
    })
    .await
    .map_err(|error| format!("write task failed: {error}"))?
}

#[cfg(unix)]
fn secure_write_file_unix(
    project_root: &Path,
    relative_path: &Path,
    content: &[u8],
    display_path: &str,
) -> Result<(), String> {
    use std::ffi::{CString, OsStr};
    use std::io::Write as _;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;

    fn component_cstring(component: &OsStr, display_path: &str) -> Result<CString, String> {
        CString::new(component.as_bytes()).map_err(|_| {
            format!(
                "cannot write to '{}': path component contains NUL byte",
                display_path
            )
        })
    }

    let components: Vec<_> = relative_path.components().collect();
    let (file_name, parent_components) = components
        .split_last()
        .ok_or_else(|| format!("cannot write to '{}': invalid path", display_path))?;

    let mut dir = std::fs::File::open(project_root).map_err(|error| {
        ToolError::IoError {
            path: project_root.display().to_string(),
            error,
        }
        .to_string()
    })?;

    for component in parent_components {
        let Component::Normal(name) = component else {
            return Err(format!("cannot write to '{}': invalid path", display_path));
        };
        let name = component_cstring(name, display_path)?;
        let fd = unsafe {
            libc::openat(
                dir.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            let error = std::io::Error::last_os_error();
            if matches!(
                error.raw_os_error(),
                Some(code) if code == libc::ELOOP || code == libc::ENOTDIR
            ) {
                return Err(format!(
                    "write denied: '{}' contains a symlink parent directory or non-directory component",
                    display_path
                ));
            }
            return Err(format!(
                "cannot write to '{}' because parent directory does not exist or cannot be resolved: {}",
                display_path, error
            ));
        }
        dir = unsafe { std::fs::File::from_raw_fd(fd) };
    }

    let Component::Normal(name) = file_name else {
        return Err(format!("cannot write to '{}': invalid path", display_path));
    };
    let name = component_cstring(name, display_path)?;
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o666 as libc::mode_t,
        )
    };
    if fd < 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ELOOP) {
            return Err(format!(
                "write denied: '{}' is a symlink (potential path escape)",
                display_path
            ));
        }
        return Err(ToolError::IoError {
            path: display_path.to_string(),
            error,
        }
        .to_string());
    }

    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
    file.write_all(content).map_err(|error| {
        ToolError::IoError {
            path: display_path.to_string(),
            error,
        }
        .to_string()
    })
}

#[cfg(not(unix))]
async fn write_file_no_symlink(
    project_root: PathBuf,
    relative_path: PathBuf,
    content: String,
    display_path: String,
) -> Result<PathBuf, String> {
    let write_path = project_root.join(&relative_path);
    let Some(parent) = write_path.parent() else {
        return Err(format!("cannot write to '{}': invalid path", display_path));
    };
    let canonical_parent = tokio::fs::canonicalize(parent).await.map_err(|_| {
        format!(
            "cannot write to '{}' because parent directory does not exist or cannot be resolved",
            display_path
        )
    })?;
    if canonical_parent != project_root && !canonical_parent.starts_with(&project_root) {
        return Err(format!(
            "path '{}' is outside the project root '{}'",
            canonical_parent.display(),
            project_root.display()
        ));
    }
    if tokio::fs::symlink_metadata(&write_path)
        .await
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
    {
        return Err(format!(
            "write denied: '{}' is a symlink (potential path escape)",
            display_path
        ));
    }
    tokio::fs::write(&write_path, content)
        .await
        .map_err(|error| {
            ToolError::IoError {
                path: display_path,
                error,
            }
            .to_string()
        })?;
    Ok(write_path)
}

pub(crate) async fn list_directory(
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

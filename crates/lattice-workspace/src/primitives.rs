// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Shared filesystem primitive helpers for workspace adapters.

use std::fs;
use std::path::{Component, Path, PathBuf};

use regex::Regex;

use crate::paths::{
    DirectoryEntry, ExecRequest, FindRequest, GrepMatch, GrepRequest, MAX_WORKSPACE_WRITE_BYTES,
    PathFacts, PathIntent, PathKind, WorkspacePath, WorkspacePathError,
};
use crate::{ExecOutput, Result, WorkspaceError};

/// Map a workspace-local path error into [`WorkspaceError`].
pub fn path_err(err: WorkspacePathError) -> WorkspaceError {
    WorkspaceError::InvalidPath(err.to_string())
}

/// Resolve a normalized relative path under `root`, rejecting escapes.
pub fn resolve_host_path(root: &Path, path: &WorkspacePath) -> Result<PathBuf> {
    if path.is_project_root() {
        return Ok(root.to_path_buf());
    }
    let joined = root.join(path.as_str());
    let canonical_root = root
        .canonicalize()
        .map_err(WorkspaceError::Io)?
        .to_string_lossy()
        .into_owned();
    let parent = joined
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| root.to_path_buf());
    fs::create_dir_all(&parent).map_err(WorkspaceError::Io)?;
    let canonical = joined
        .canonicalize()
        .map_err(|_| WorkspaceError::NotFound {
            path: path.as_str().to_string(),
        })?;
    if !canonical
        .to_string_lossy()
        .starts_with(canonical_root.as_str())
    {
        return Err(WorkspaceError::PathEscape {
            path: path.as_str().to_string(),
        });
    }
    Ok(canonical)
}

fn metadata_kind(meta: &fs::Metadata) -> PathKind {
    let ft = meta.file_type();
    if ft.is_symlink() {
        PathKind::Symlink
    } else if ft.is_dir() {
        PathKind::Directory
    } else {
        PathKind::File
    }
}

fn parent_facts(root: &Path, path: &WorkspacePath) -> (bool, Option<PathKind>) {
    if path.is_project_root() {
        let meta = fs::symlink_metadata(root).ok();
        return (meta.is_some(), meta.map(|m| metadata_kind(&m)));
    }
    let parent_rel = crate::paths::parent_relative(path.as_str());
    let parent_path = match WorkspacePath::parse(&parent_rel) {
        Ok(p) => p,
        Err(_) => return (false, None),
    };
    match resolve_host_path(root, &parent_path) {
        Ok(p) => {
            let meta = fs::symlink_metadata(&p).ok();
            (meta.is_some(), meta.map(|m| metadata_kind(&m)))
        }
        Err(_) => (false, None),
    }
}

/// No-follow path facts for a path under `root`.
pub fn resolve_path_facts(
    root: &Path,
    path: &WorkspacePath,
    _intent: PathIntent,
) -> Result<PathFacts> {
    let (exists, kind, byte_size, canonical_under_workspace) = if path.is_project_root() {
        let meta = fs::symlink_metadata(root).map_err(WorkspaceError::Io)?;
        (true, Some(metadata_kind(&meta)), None, true)
    } else {
        let host = root.join(path.as_str());
        match fs::symlink_metadata(&host) {
            Ok(meta) => {
                let under = resolve_host_path(root, path).is_ok();
                (
                    true,
                    Some(metadata_kind(&meta)),
                    if meta.is_file() {
                        Some(meta.len())
                    } else {
                        None
                    },
                    under,
                )
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let (parent_exists, _) = parent_facts(root, path);
                (false, None, None, parent_exists)
            }
            Err(e) => return Err(WorkspaceError::Io(e)),
        }
    };
    let (parent_exists, parent_kind) = parent_facts(root, path);
    Ok(PathFacts {
        normalized_path: path.as_str().to_string(),
        exists,
        kind,
        parent_exists,
        parent_kind,
        byte_size,
        canonical_under_workspace,
    })
}

pub fn read_file_bytes(root: &Path, path: &WorkspacePath) -> Result<Vec<u8>> {
    path.require_file_path().map_err(path_err)?;
    let host = resolve_host_path(root, path)?;
    let meta = fs::symlink_metadata(&host).map_err(WorkspaceError::Io)?;
    if !meta.is_file() {
        return Err(WorkspaceError::WrongKind {
            path: path.as_str().to_string(),
            expected: "file".into(),
        });
    }
    fs::read(&host).map_err(WorkspaceError::Io)
}

pub fn write_file_bytes(root: &Path, path: &WorkspacePath, content: &[u8]) -> Result<()> {
    path.require_file_path().map_err(path_err)?;
    if content.len() > MAX_WORKSPACE_WRITE_BYTES {
        return Err(WorkspaceError::ContentTooLarge {
            actual: content.len(),
            max: MAX_WORKSPACE_WRITE_BYTES,
        });
    }
    let joined = root.join(path.as_str());
    if let Some(parent) = joined.parent() {
        fs::create_dir_all(parent).map_err(WorkspaceError::Io)?;
        let canonical_root = root.canonicalize().map_err(WorkspaceError::Io)?;
        let parent_canon = parent.canonicalize().map_err(WorkspaceError::Io)?;
        if !parent_canon.starts_with(&canonical_root) {
            return Err(WorkspaceError::PathEscape {
                path: path.as_str().to_string(),
            });
        }
    }
    fs::write(&joined, content).map_err(WorkspaceError::Io)
}

pub fn remove_file_path(root: &Path, path: &WorkspacePath) -> Result<()> {
    path.require_file_path().map_err(path_err)?;
    let host = resolve_host_path(root, path)?;
    let meta = fs::symlink_metadata(&host).map_err(WorkspaceError::Io)?;
    if meta.is_dir() {
        return Err(WorkspaceError::WrongKind {
            path: path.as_str().to_string(),
            expected: "file".into(),
        });
    }
    fs::remove_file(&host).map_err(WorkspaceError::Io)
}

pub fn list_dir_entries(root: &Path, path: &WorkspacePath) -> Result<Vec<DirectoryEntry>> {
    let host = if path.is_project_root() {
        root.to_path_buf()
    } else {
        resolve_host_path(root, path)?
    };
    let meta = fs::symlink_metadata(&host).map_err(WorkspaceError::Io)?;
    if !meta.is_dir() {
        return Err(WorkspaceError::WrongKind {
            path: path.as_str().to_string(),
            expected: "directory".into(),
        });
    }
    let mut entries = Vec::new();
    for entry in fs::read_dir(&host).map_err(WorkspaceError::Io)? {
        let entry = entry.map_err(WorkspaceError::Io)?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let em = entry.metadata().map_err(WorkspaceError::Io)?;
        entries.push(DirectoryEntry {
            name,
            kind: metadata_kind(&em),
        });
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(entries)
}

pub fn grep_tree(root: &Path, request: &GrepRequest) -> Result<Vec<GrepMatch>> {
    let base = if request.path.is_project_root() {
        root.to_path_buf()
    } else {
        resolve_host_path(root, &request.path)?
    };
    let re = Regex::new(&request.pattern).map_err(|e| WorkspaceError::Sdk(e.to_string()))?;
    let mut matches = Vec::new();
    grep_walk(
        root,
        &base,
        &re,
        request.glob.as_deref(),
        request.max_results,
        &mut matches,
    )?;
    Ok(matches)
}

fn grep_walk(
    workspace_root: &Path,
    current: &Path,
    re: &Regex,
    glob: Option<&str>,
    max_results: usize,
    out: &mut Vec<GrepMatch>,
) -> Result<()> {
    if out.len() >= max_results {
        return Ok(());
    }
    for entry in fs::read_dir(current).map_err(WorkspaceError::Io)? {
        if out.len() >= max_results {
            break;
        }
        let entry = entry.map_err(WorkspaceError::Io)?;
        let path = entry.path();
        let meta = fs::symlink_metadata(&path).map_err(WorkspaceError::Io)?;
        if meta.is_dir() {
            grep_walk(workspace_root, &path, re, glob, max_results, out)?;
        } else if meta.is_file() {
            if let Some(g) = glob {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if !glob_match(g, name) {
                    continue;
                }
            }
            let rel = relative_path(workspace_root, &path)?;
            let content = match fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            for (idx, line) in content.lines().enumerate() {
                if out.len() >= max_results {
                    break;
                }
                if re.is_match(line) {
                    let wp = WorkspacePath::parse(&rel).map_err(path_err)?;
                    out.push(GrepMatch {
                        path: wp,
                        line_number: (idx as u64) + 1,
                        line: line.to_string(),
                    });
                }
            }
        }
    }
    Ok(())
}

pub fn find_files_tree(root: &Path, request: &FindRequest) -> Result<Vec<WorkspacePath>> {
    let base = if request.path.is_project_root() {
        root.to_path_buf()
    } else {
        resolve_host_path(root, &request.path)?
    };
    let mut found = Vec::new();
    find_walk(
        root,
        &base,
        &request.pattern,
        request.max_results,
        &mut found,
    )?;
    Ok(found)
}

fn find_walk(
    workspace_root: &Path,
    current: &Path,
    pattern: &str,
    max_results: usize,
    out: &mut Vec<WorkspacePath>,
) -> Result<()> {
    if out.len() >= max_results {
        return Ok(());
    }
    for entry in fs::read_dir(current).map_err(WorkspaceError::Io)? {
        if out.len() >= max_results {
            break;
        }
        let entry = entry.map_err(WorkspaceError::Io)?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if glob_match(pattern, &name) {
            let rel = relative_path(workspace_root, &path)?;
            out.push(WorkspacePath::parse(&rel).map_err(path_err)?);
        }
        let meta = fs::symlink_metadata(&path).map_err(WorkspaceError::Io)?;
        if meta.is_dir() {
            find_walk(workspace_root, &path, pattern, max_results, out)?;
        }
    }
    Ok(())
}

fn relative_path(root: &Path, path: &Path) -> Result<String> {
    let root_canon = root.canonicalize().map_err(WorkspaceError::Io)?;
    let path_canon = path.canonicalize().map_err(WorkspaceError::Io)?;
    let rel = path_canon
        .strip_prefix(&root_canon)
        .map_err(|_| WorkspaceError::PathEscape {
            path: path.to_string_lossy().into_owned(),
        })?;
    let mut parts = Vec::new();
    for component in rel.components() {
        match component {
            Component::Normal(p) => parts.push(p.to_string_lossy().into_owned()),
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(WorkspaceError::PathEscape {
                    path: path.to_string_lossy().into_owned(),
                });
            }
            Component::RootDir | Component::Prefix(_) => {}
        }
    }
    if parts.is_empty() {
        Ok(".".to_string())
    } else {
        Ok(parts.join("/"))
    }
}

fn glob_match(pattern: &str, name: &str) -> bool {
    if pattern.contains('*') {
        let parts: Vec<&str> = pattern.split('*').collect();
        if parts.len() == 2 {
            let (prefix, suffix) = (parts[0], parts[1]);
            return name.starts_with(prefix) && name.ends_with(suffix);
        }
    }
    pattern == name
}

pub fn exec_on_host(root: &Path, request: &ExecRequest) -> Result<ExecOutput> {
    let cwd = if request.cwd.is_project_root() {
        root.to_path_buf()
    } else {
        resolve_host_path(root, &request.cwd)?
    };
    let prog = &request.argv[0];
    let args: Vec<&str> = request.argv[1..].iter().map(String::as_str).collect();
    let mut cmd = std::process::Command::new(prog);
    cmd.args(&args).current_dir(&cwd);
    for (k, v) in &request.env {
        cmd.env(k, v);
    }
    let out = cmd.output().map_err(WorkspaceError::Io)?;
    Ok(ExecOutput {
        exit_code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

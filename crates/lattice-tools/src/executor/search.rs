use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::{DefaultToolExecutor, ToolLimits};

const GREP_MAX_DEPTH: u32 = 32;
const DEFAULT_GREP_LIMIT: usize = 200;
const MAX_GREP_LIMIT: usize = 1000;
const DEFAULT_LIST_LIMIT: usize = 200;
const MAX_LIST_LIMIT: usize = 1000;

pub(super) async fn grep(executor: &DefaultToolExecutor, args: &serde_json::Value) -> String {
    use crate::tool_error::ToolError;

    let pattern = args.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
    if let Err(e) = DefaultToolExecutor::reject_parent_traversal(path) {
        return e;
    }
    let max_results = args
        .get("max_results")
        .and_then(|v| v.as_u64())
        .and_then(|v| usize::try_from(v).ok())
        .unwrap_or(DEFAULT_GREP_LIMIT)
        .clamp(1, MAX_GREP_LIMIT);

    let re = match regex::Regex::new(pattern) {
        Ok(r) => r,
        Err(e) => return ToolError::RegexError(e.to_string()).to_string(),
    };

    let mut results = Vec::new();
    let mut visited = HashSet::new();
    let mut truncated = false;
    let project_root = PathBuf::from(&executor.project_root);
    let mut context = GrepContext {
        pattern: &re,
        project_root: &project_root,
        results: &mut results,
        limits: &executor.limits,
        visited: &mut visited,
        max_results,
        truncated: &mut truncated,
    };
    grep_recursive(&mut context, &executor.resolve_user_path(path), 0).await;

    if results.is_empty() {
        "(no matches)".to_string()
    } else if truncated {
        format!(
            "{}\n[TRUNCATED: grep returned first {} matches; narrow path or pattern]",
            results.join("\n"),
            results.len()
        )
    } else {
        results.join("\n")
    }
}

pub(super) async fn find_files(executor: &DefaultToolExecutor, args: &serde_json::Value) -> String {
    use crate::tool_error::ToolError;

    let pattern = args.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
    if let Err(e) = DefaultToolExecutor::reject_parent_traversal(path) {
        return e;
    }
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .and_then(|v| usize::try_from(v).ok())
        .unwrap_or(DEFAULT_LIST_LIMIT)
        .clamp(1, MAX_LIST_LIMIT);
    let requested = executor.resolve_user_path(path);
    let canonical = match tokio::fs::canonicalize(&requested).await {
        Ok(p) => p,
        Err(e) => {
            return ToolError::IoError {
                path: path.to_string(),
                error: e,
            }
            .to_string()
        }
    };
    if let Err(e) = executor.ensure_under_project_root(&canonical) {
        return e;
    }
    let mut matches = Vec::new();
    let mut visited = HashSet::new();
    let mut truncated = false;
    let project_root = PathBuf::from(&executor.project_root);
    let mut context = FindFilesContext {
        root: &canonical,
        project_root: &project_root,
        matches: &mut matches,
        pattern,
        visited: &mut visited,
        max_results: limit,
        truncated: &mut truncated,
    };
    find_files_recursive(&mut context, &canonical, 0).await;
    if matches.is_empty() {
        "(no matches)".to_string()
    } else if truncated {
        format!(
            "{}\n[TRUNCATED: find_files returned first {} matches; narrow path or pattern]",
            matches.join("\n"),
            matches.len()
        )
    } else {
        matches.join("\n")
    }
}

struct FindFilesContext<'a> {
    root: &'a Path,
    project_root: &'a Path,
    matches: &'a mut Vec<String>,
    pattern: &'a str,
    visited: &'a mut HashSet<PathBuf>,
    max_results: usize,
    truncated: &'a mut bool,
}

async fn find_files_recursive(context: &mut FindFilesContext<'_>, path: &Path, depth: u32) {
    if depth > GREP_MAX_DEPTH || context.matches.len() >= context.max_results {
        if context.matches.len() >= context.max_results {
            *context.truncated = true;
        }
        return;
    }

    let resolved = match tokio::fs::canonicalize(path).await {
        Ok(p) => p,
        Err(_) => return,
    };
    if !context.visited.insert(resolved.clone()) {
        return;
    }
    if resolved != context.project_root && !resolved.starts_with(context.project_root) {
        return;
    }
    if resolved != context.root {
        let relative = resolved
            .strip_prefix(context.root)
            .unwrap_or(&resolved)
            .to_string_lossy()
            .replace('\\', "/");
        if glob_like_matches(context.pattern, &relative) {
            context.matches.push(relative);
            if context.matches.len() >= context.max_results {
                *context.truncated = true;
                return;
            }
        }
    }

    if !resolved.is_dir() {
        return;
    }

    let mut entries = match tokio::fs::read_dir(&resolved).await {
        Ok(d) => d,
        Err(_) => return,
    };
    let mut children = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if matches!(name_str.as_ref(), ".git" | "target" | "node_modules") {
            continue;
        }
        if name_str.starts_with('.') && name_str != "." && name_str != ".." {
            continue;
        }
        children.push(entry.path());
    }
    children.sort();

    for child_path in children {
        if context.matches.len() >= context.max_results {
            *context.truncated = true;
            return;
        }
        Box::pin(find_files_recursive(context, &child_path, depth + 1)).await;
    }
}

struct GrepContext<'a> {
    pattern: &'a regex::Regex,
    project_root: &'a Path,
    results: &'a mut Vec<String>,
    limits: &'a ToolLimits,
    visited: &'a mut HashSet<PathBuf>,
    max_results: usize,
    truncated: &'a mut bool,
}

async fn grep_recursive(context: &mut GrepContext<'_>, path: &Path, depth: u32) {
    if depth > GREP_MAX_DEPTH || context.results.len() >= context.max_results {
        if context.results.len() >= context.max_results {
            *context.truncated = true;
        }
        return;
    }

    let resolved = match tokio::fs::canonicalize(path).await {
        Ok(p) => p,
        Err(_) => return,
    };
    if !context.visited.insert(resolved.clone()) {
        return;
    }

    if resolved != context.project_root && !resolved.starts_with(context.project_root) {
        return;
    }

    if resolved.is_file() {
        if let Ok(meta) = tokio::fs::metadata(&resolved).await {
            if meta.len() > context.limits.max_read_size as u64 {
                return;
            }
        }

        if let Ok(content) = tokio::fs::read_to_string(&resolved).await {
            if content.contains('\0') {
                return;
            }
            for (line_num, line) in content.lines().enumerate() {
                if context.pattern.is_match(line) {
                    if context.results.len() >= context.max_results {
                        *context.truncated = true;
                        return;
                    }
                    context.results.push(format!(
                        "{}:{}:{}",
                        resolved.display(),
                        line_num + 1,
                        line
                    ));
                }
            }
        }
    } else if resolved.is_dir() {
        let mut entries = match tokio::fs::read_dir(&resolved).await {
            Ok(d) => d,
            Err(_) => return,
        };
        let mut children = Vec::new();
        while let Ok(Some(entry)) = entries.next_entry().await {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.starts_with('.') && name_str != "." && name_str != ".." {
                continue;
            }
            children.push(entry.path());
        }
        for child_path in children {
            if context.results.len() >= context.max_results {
                *context.truncated = true;
                return;
            }
            Box::pin(grep_recursive(context, &child_path, depth + 1)).await;
        }
    }
}

fn glob_like_matches(pattern: &str, candidate: &str) -> bool {
    if pattern.is_empty() {
        return false;
    }
    if wildcard_match(pattern, candidate) {
        return true;
    }
    let name = candidate.rsplit('/').next().unwrap_or(candidate);
    wildcard_match(pattern, name)
}

fn wildcard_match(pattern: &str, text: &str) -> bool {
    let pattern = pattern.as_bytes();
    let text = text.as_bytes();
    let (mut p, mut t) = (0usize, 0usize);
    let mut star = None;
    let mut star_text = 0usize;

    while t < text.len() {
        if p < pattern.len() && (pattern[p] == b'?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == b'*' {
            star = Some(p);
            p += 1;
            star_text = t;
        } else if let Some(star_pos) = star {
            p = star_pos + 1;
            star_text += 1;
            t = star_text;
        } else {
            return false;
        }
    }

    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }

    p == pattern.len()
}

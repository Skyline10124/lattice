use std::path::{Path, PathBuf};

use lattice_tooling::ToolError;
use super::DefaultToolExecutor;

const DEFAULT_WORKSPACE_ENTRY_LIMIT: usize = 40;
const MAX_WORKSPACE_ENTRY_LIMIT: usize = 100;

fn workspace_run_git(workdir: &Path, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(workdir)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn workspace_git_context(workdir: &Path) -> Option<Vec<String>> {
    let branch = workspace_run_git(workdir, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let status = workspace_run_git(workdir, &["status", "--short"]).unwrap_or_default();
    let mut lines = vec![format!("- branch: {branch}")];
    if status.trim().is_empty() {
        lines.push("- worktree: clean".into());
    } else {
        let count = status.lines().count();
        lines.push(format!("- worktree: {count} changed path(s)"));
        for line in status.lines().take(12) {
            lines.push(format!("  {line}"));
        }
        if count > 12 {
            lines.push(format!("  ... {remaining} more", remaining = count - 12));
        }
    }
    Some(lines)
}

fn workspace_instruction_context(workdir: &Path) -> Vec<String> {
    const FILES: [&str; 3] = ["LATTICE.md", "AGENTS.md", "CLAUDE.md"];
    let root = workspace_run_git(workdir, &["rev-parse", "--show-toplevel"])
        .map(PathBuf::from)
        .unwrap_or_else(|| workdir.to_path_buf());
    let root = root.canonicalize().unwrap_or(root);
    let mut current = workdir
        .canonicalize()
        .unwrap_or_else(|_| workdir.to_path_buf());
    let mut dirs = Vec::new();
    loop {
        if current.starts_with(&root) {
            dirs.push(current.clone());
        }
        if current == root || !current.pop() {
            break;
        }
    }
    dirs.reverse();

    let mut lines = Vec::new();
    for dir in dirs {
        for filename in FILES {
            let path = dir.join(filename);
            if path.is_file() {
                let display = path.strip_prefix(&root).unwrap_or(&path).display();
                lines.push(format!("- {display}"));
            }
        }
    }
    lines
}

fn workspace_project_type_context(workdir: &Path) -> Vec<String> {
    let mut lines = Vec::new();
    let checks = [
        (
            "Cargo.toml",
            "Rust workspace/crate; verify with cargo test, cargo check, or cargo fmt as appropriate.",
        ),
        (
            "package.json",
            "Node/JS project; inspect scripts before choosing npm/pnpm/yarn commands.",
        ),
        (
            "pyproject.toml",
            "Python project; inspect tool config before choosing pytest/ruff/mypy commands.",
        ),
        (
            "go.mod",
            "Go module; verify with go test ./... or go test on touched packages.",
        ),
        ("Makefile", "Make targets available; inspect before use."),
    ];
    for (file, note) in checks {
        if workdir.join(file).is_file() {
            lines.push(format!("- {file}: {note}"));
        }
    }
    lines
}

impl DefaultToolExecutor {
    pub(super) async fn workspace_context_inner(
        &self,
        path: &str,
        limit: usize,
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
        let mut lines = vec![format!("Workspace: {}", canonical.display())];
        lines.push("Top-level entries:".into());
        let mut entries = tokio::fs::read_dir(&canonical).await.map_err(|e| {
            ToolError::IoError {
                path: path.to_string(),
                error: e,
            }
            .to_string()
        })?;
        let mut rendered = Vec::new();
        loop {
            match entries.next_entry().await {
                Ok(Some(entry)) => {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if matches!(name.as_str(), ".git" | "target" | "node_modules") {
                        continue;
                    }
                    let ty = if entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false) {
                        "DIR"
                    } else {
                        "FILE"
                    };
                    rendered.push(format!("- {ty} {name}"));
                }
                Ok(None) => break,
                Err(e) => {
                    rendered.push(format!("- Error reading entry: {e}"));
                    break;
                }
            }
        }
        rendered.sort();
        let total = rendered.len();
        let visible: Vec<String> = rendered.into_iter().take(limit).collect();
        if visible.is_empty() {
            lines.push("- no readable top-level entries".into());
        } else {
            lines.extend(visible.iter().cloned());
            if total > visible.len() {
                lines.push(format!(
                    "[TRUNCATED: showing {} of {} entries]",
                    visible.len(),
                    total
                ));
            }
        }

        if let Some(git) = workspace_git_context(&canonical) {
            lines.push(String::new());
            lines.push("Git state:".into());
            lines.extend(git);
        }

        let instructions = workspace_instruction_context(&canonical);
        if !instructions.is_empty() {
            lines.push(String::new());
            lines.push("Project instruction files visible from this directory:".into());
            lines.extend(instructions);
        }

        let project = workspace_project_type_context(&canonical);
        if !project.is_empty() {
            lines.push(String::new());
            lines.push("Detected project files and likely verification commands:".into());
            lines.extend(project);
        }

        Ok(lines.join("\n"))
    }
}

pub(super) async fn workspace_context(
    executor: &DefaultToolExecutor,
    args: &serde_json::Value,
) -> Result<String, String> {
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .and_then(|v| usize::try_from(v).ok())
        .unwrap_or(DEFAULT_WORKSPACE_ENTRY_LIMIT)
        .clamp(1, MAX_WORKSPACE_ENTRY_LIMIT);

    executor.workspace_context_inner(path, limit).await
}

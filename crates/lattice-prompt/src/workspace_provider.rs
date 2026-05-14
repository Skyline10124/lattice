use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use async_trait::async_trait;

use crate::provider::ContextProvider;
use crate::types::{AssemblyContext, Layer, PromptSection, TokenBudget};

const MAX_TOP_LEVEL_ENTRIES: usize = 40;
const MAX_GIT_STATUS_LINES: usize = 12;
const SKIPPED_TOP_LEVEL_DIRS: [&str; 3] = [".git", "target", "node_modules"];
const INSTRUCTION_FILES: [&str; 3] = ["LATTICE.md", "AGENTS.md", "CLAUDE.md"];
const PROJECT_FILE_HINTS: [(&str, &str); 5] = [
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

#[derive(Debug, Clone)]
pub struct WorkspaceContextProvider {
    root: PathBuf,
    cwd: PathBuf,
    priority: u8,
    budget: TokenBudget,
    provenance_id: String,
}

impl WorkspaceContextProvider {
    pub fn new(root: impl Into<PathBuf>, cwd: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            cwd: cwd.into(),
            priority: 5,
            budget: TokenBudget::Ratio(0.08),
            provenance_id: "workspace-context".to_string(),
        }
    }

    fn render_context(&self) -> Option<String> {
        let mut lines = Vec::new();

        if let Some(top_level) = self.top_level_entries() {
            lines.push("Top-level entries:".to_string());
            lines.extend(top_level);
        }

        if let Some(git) = self.git_state() {
            push_section(&mut lines, "Git state:");
            lines.extend(git);
        }

        let instructions = self.instruction_files();
        if !instructions.is_empty() {
            push_section(
                &mut lines,
                "Project instruction files visible from this directory:",
            );
            lines.extend(instructions);
        }

        let project_files = self.project_file_hints();
        if !project_files.is_empty() {
            push_section(
                &mut lines,
                "Detected project files and likely verification commands:",
            );
            lines.extend(project_files);
        }

        if lines.is_empty() {
            None
        } else {
            Some(lines.join("\n"))
        }
    }

    fn top_level_entries(&self) -> Option<Vec<String>> {
        let entries = std::fs::read_dir(&self.cwd).ok()?;
        let mut names: Vec<_> = entries
            .flatten()
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| !SKIPPED_TOP_LEVEL_DIRS.contains(&name.as_str()))
            .collect();
        names.sort();

        let mut lines: Vec<_> = names
            .into_iter()
            .take(MAX_TOP_LEVEL_ENTRIES)
            .map(|name| format!("- {name}"))
            .collect();
        if lines.is_empty() {
            lines.push("- no readable top-level entries".to_string());
        }
        Some(lines)
    }

    fn git_state(&self) -> Option<Vec<String>> {
        let branch = run_git(&self.cwd, &["rev-parse", "--abbrev-ref", "HEAD"])?;
        let status = run_git(&self.cwd, &["status", "--short"]).unwrap_or_default();
        let mut lines = vec![format!("- branch: {branch}")];

        if status.trim().is_empty() {
            lines.push("- worktree: clean".to_string());
            return Some(lines);
        }

        let count = status.lines().count();
        lines.push(format!("- worktree: {count} changed path(s)"));
        for line in status.lines().take(MAX_GIT_STATUS_LINES) {
            lines.push(format!("  {line}"));
        }
        if count > MAX_GIT_STATUS_LINES {
            lines.push(format!(
                "  ... {remaining} more",
                remaining = count - MAX_GIT_STATUS_LINES
            ));
        }
        Some(lines)
    }

    fn instruction_files(&self) -> Vec<String> {
        let root = canonical_or_original(&self.root);
        self.root_to_cwd_dirs()
            .into_iter()
            .flat_map(|dir| {
                let root = root.clone();
                INSTRUCTION_FILES.into_iter().filter_map(move |filename| {
                    let path = dir.join(filename);
                    path.is_file()
                        .then(|| format!("- {}", display_path(&root, &path)))
                })
            })
            .collect()
    }

    fn project_file_hints(&self) -> Vec<String> {
        let root = canonical_or_original(&self.root);
        let mut seen = BTreeSet::new();
        let mut lines = Vec::new();

        for dir in self.root_to_cwd_dirs() {
            for (file, note) in PROJECT_FILE_HINTS {
                let path = dir.join(file);
                if path.is_file() && seen.insert(path.clone()) {
                    lines.push(format!("- {}: {note}", display_path(&root, &path)));
                }
            }
        }

        lines
    }

    fn root_to_cwd_dirs(&self) -> Vec<PathBuf> {
        let root = canonical_or_original(&self.root);
        let mut current = canonical_or_original(&self.cwd);
        let mut dirs = Vec::new();

        loop {
            if current.starts_with(&root) {
                dirs.push(current.clone());
            }
            if current == root {
                break;
            }
            if !current.pop() {
                break;
            }
        }

        dirs.reverse();
        dirs
    }
}

#[async_trait]
impl ContextProvider for WorkspaceContextProvider {
    fn configure_from_spec(&mut self, spec: &crate::spec::PromptProviderSpec) {
        self.priority = spec.priority;
        self.budget = spec.budget.to_token_budget(512);
        self.provenance_id = spec.id.clone();
    }

    fn name(&self) -> &str {
        "workspace-context"
    }

    fn provenance_provider(&self) -> &str {
        &self.provenance_id
    }

    fn layer(&self) -> Layer {
        Layer::Memory
    }

    fn priority(&self) -> u8 {
        self.priority
    }

    fn budget(&self) -> TokenBudget {
        self.budget
    }

    async fn produce(&self, _ctx: &AssemblyContext<'_>) -> Option<PromptSection> {
        let content = self.render_context()?;
        let tokens = lattice_core::tokens::TokenEstimator::estimate_text(&content);
        Some(PromptSection::new(
            content,
            Layer::Memory,
            self.priority(),
            tokens,
        ))
    }
}

fn push_section(lines: &mut Vec<String>, heading: &str) {
    if !lines.is_empty() {
        lines.push(String::new());
    }
    lines.push(heading.to_string());
}

fn run_git(workdir: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(workdir)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn canonical_or_original(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn display_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_core::catalog::ApiProtocol;
    use lattice_core::ResolvedModel;
    use std::collections::HashMap;

    fn make_resolved() -> ResolvedModel {
        ResolvedModel {
            canonical_id: "test".into(),
            provider: "test".into(),
            api_key: None,
            base_url: "".into(),
            api_protocol: ApiProtocol::OpenAiChat,
            api_model_id: "test".into(),
            context_length: 8192,
            provider_specific: HashMap::new(),
            credential_status: lattice_core::CredentialStatus::Missing,
        }
    }

    fn make_ctx<'a>(resolved: &'a ResolvedModel) -> AssemblyContext<'a> {
        AssemblyContext {
            request_id: "test",
            model: resolved,
            user_input: "hello",
        }
    }

    #[tokio::test]
    async fn workspace_context_provider_renders_workspace_sections() {
        let root =
            std::env::temp_dir().join(format!("lattice-workspace-context-{}", std::process::id()));
        let cwd = root.join("crates").join("demo");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(cwd.join("src")).unwrap();
        std::fs::create_dir_all(cwd.join(".git")).unwrap();
        std::fs::create_dir_all(cwd.join("target")).unwrap();
        std::fs::create_dir_all(cwd.join("node_modules")).unwrap();
        std::fs::write(root.join("Cargo.toml"), "").unwrap();
        std::fs::write(root.join("AGENTS.md"), "root rules").unwrap();
        std::fs::write(cwd.join("CLAUDE.md"), "child rules").unwrap();
        std::fs::write(cwd.join("package.json"), "{}").unwrap();
        std::fs::write(cwd.join("src").join("lib.rs"), "").unwrap();

        Command::new("git")
            .args(["init", "-q"])
            .current_dir(&root)
            .status()
            .unwrap();
        Command::new("git")
            .args(["add", "Cargo.toml"])
            .current_dir(&root)
            .status()
            .unwrap();
        Command::new("git")
            .args([
                "-c",
                "user.name=Lattice Test",
                "-c",
                "user.email=lattice@example.com",
                "commit",
                "-q",
                "-m",
                "init",
            ])
            .current_dir(&root)
            .status()
            .unwrap();

        let provider = WorkspaceContextProvider::new(&root, &cwd);
        let resolved = make_resolved();
        let section = provider.produce(&make_ctx(&resolved)).await.unwrap();

        assert_eq!(section.layer, Layer::Memory);
        assert!(section.content.contains("Top-level entries:"));
        assert!(section.content.contains("- package.json"));
        assert!(section.content.contains("- src"));
        assert!(!section.content.contains("- .git"));
        assert!(!section.content.contains("- target"));
        assert!(!section.content.contains("- node_modules"));
        assert!(section.content.contains("Git state:"));
        assert!(section.content.contains("- branch:"));
        assert!(section.content.contains("changed path(s)"));
        assert!(section
            .content
            .contains("Project instruction files visible from this directory:"));
        assert!(section.content.contains("- AGENTS.md"));
        assert!(section.content.contains("- crates/demo/CLAUDE.md"));
        assert!(section
            .content
            .contains("Detected project files and likely verification commands:"));
        assert!(section
            .content
            .contains("- Cargo.toml: Rust workspace/crate"));
        assert!(section
            .content
            .contains("- crates/demo/package.json: Node/JS project"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn workspace_context_provider_returns_none_when_nothing_can_be_read() {
        let root = PathBuf::from("/definitely/missing/lattice/root");
        let provider = WorkspaceContextProvider::new(&root, root.join("child"));
        let resolved = make_resolved();

        assert!(provider.produce(&make_ctx(&resolved)).await.is_none());
    }
}

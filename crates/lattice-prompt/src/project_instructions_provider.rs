use std::path::{Path, PathBuf};

use async_trait::async_trait;

use crate::provider::ContextProvider;
use crate::types::{AssemblyContext, Layer, PromptSection, TokenBudget};

const MAX_INSTRUCTION_BYTES: usize = 64 * 1024;
const INSTRUCTION_PRECEDENCE: &str = "Instruction precedence: deeper directories override parent directories; within one directory LATTICE.md > AGENTS.md > CLAUDE.md.";
const INSTRUCTION_FILES_RENDER_ORDER: [(&str, &str); 3] = [
    ("CLAUDE.md", "compat"),
    ("AGENTS.md", "general"),
    ("LATTICE.md", "native"),
];

#[derive(Debug, Clone)]
pub struct ProjectInstructionsProvider {
    root: PathBuf,
    cwd: PathBuf,
    priority: u8,
    budget: TokenBudget,
    provenance_id: String,
}

impl ProjectInstructionsProvider {
    pub fn new(root: impl Into<PathBuf>, cwd: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            cwd: cwd.into(),
            priority: 0,
            budget: TokenBudget::Ratio(0.12),
            provenance_id: "project-instructions".to_string(),
        }
    }

    pub fn from_root(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            cwd: root.clone(),
            root,
            priority: 0,
            budget: TokenBudget::Ratio(0.12),
            provenance_id: "project-instructions".to_string(),
        }
    }

    fn instruction_sources(&self) -> Vec<InstructionSource> {
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
        let dir_count = dirs.len();
        let dirs = dirs
            .into_iter()
            .enumerate()
            .map(|(dir_index, dir)| (dir, directory_priority_label(dir_index, dir_count)))
            .collect::<Vec<_>>();
        let mut sources = Vec::new();
        for (filename, file_priority) in INSTRUCTION_FILES_RENDER_ORDER {
            for (dir, directory_priority) in &dirs {
                let path = dir.join(filename);
                let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                    continue;
                };
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    continue;
                }
                let Ok(canonical_path) = path.canonicalize() else {
                    continue;
                };
                if !canonical_path.starts_with(&root) {
                    continue;
                }
                sources.push(InstructionSource {
                    path,
                    priority_label: format!("priority={directory_priority}/{file_priority}"),
                });
            }
        }
        sources
    }
}

#[derive(Debug, Clone)]
struct InstructionSource {
    path: PathBuf,
    priority_label: String,
}

#[async_trait]
impl ContextProvider for ProjectInstructionsProvider {
    fn configure_from_spec(&mut self, spec: &crate::spec::PromptProviderSpec) {
        self.priority = spec.priority;
        self.budget = spec.budget.to_token_budget(512);
        self.provenance_id = spec.id.clone();
    }

    fn name(&self) -> &str {
        "project-instructions"
    }

    fn provenance_provider(&self) -> &str {
        &self.provenance_id
    }

    fn layer(&self) -> Layer {
        Layer::Rules
    }

    fn priority(&self) -> u8 {
        self.priority
    }

    fn budget(&self) -> TokenBudget {
        self.budget
    }

    async fn produce(&self, _ctx: &AssemblyContext<'_>) -> Option<PromptSection> {
        let mut remaining = MAX_INSTRUCTION_BYTES;
        let mut parts = Vec::new();

        for source in self.instruction_sources() {
            if remaining == 0 {
                break;
            }
            let Ok(bytes) = std::fs::read(&source.path) else {
                continue;
            };
            let take = bytes.len().min(remaining);
            let content = String::from_utf8_lossy(&bytes[..take]).to_string();
            remaining = remaining.saturating_sub(take);
            parts.push(format!(
                "Project instructions from {} ({})\n{}",
                display_path(&self.root, &source.path),
                source.priority_label,
                content.trim()
            ));
        }

        if parts.is_empty() {
            return None;
        }

        let content = format!(
            "Hierarchical project instructions. Treat these as Rules-layer instructions scoped to this project. \
             LATTICE.md, AGENTS.md, and CLAUDE.md files apply to the directory tree rooted at the directory that contains them.\n\
             {INSTRUCTION_PRECEDENCE}\n\
             Render order: CLAUDE.md blocks, then AGENTS.md blocks, then LATTICE.md blocks; within each file tier, parent directories before child directories so later local blocks carry higher precedence.\n{}",
            parts.join("\n\n")
        );
        let tokens = lattice_core::tokens::TokenEstimator::estimate_text(&content);
        Some(PromptSection::new(
            content,
            Layer::Rules,
            self.priority(),
            tokens,
        ))
    }
}

fn canonical_or_original(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn directory_priority_label(index: usize, len: usize) -> &'static str {
    if len <= 1 || index + 1 == len {
        "local"
    } else if index == 0 {
        "parent"
    } else {
        "nested"
    }
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_TEST_ID: AtomicUsize = AtomicUsize::new(0);

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

    fn temp_project(name: &str) -> PathBuf {
        let id = NEXT_TEST_ID.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "lattice-project-instructions-{name}-{}-{id}",
            std::process::id(),
        ));
        std::fs::remove_dir_all(&root).ok();
        root
    }

    fn assert_before(content: &str, first: &str, second: &str) {
        let first_pos = content
            .find(first)
            .unwrap_or_else(|| panic!("missing first marker: {first}"));
        let second_pos = content
            .find(second)
            .unwrap_or_else(|| panic!("missing second marker: {second}"));
        assert!(
            first_pos < second_pos,
            "expected `{first}` before `{second}` in:\n{content}"
        );
    }

    async fn render_provider(root: &Path, cwd: &Path) -> String {
        let provider = ProjectInstructionsProvider::new(root, cwd);
        let resolved = make_resolved();
        let ctx = AssemblyContext {
            request_id: "test",
            model: &resolved,
            user_input: "hello",
        };

        provider.produce(&ctx).await.unwrap().content
    }

    mod project_instruction_precedence {
        use super::*;

        #[tokio::test]
        async fn renders_header_and_child_directory_after_root_directory() {
            let root = temp_project("nested-order");
            let child = root.join("crates").join("demo");
            std::fs::create_dir_all(&child).unwrap();
            std::fs::write(root.join("AGENTS.md"), "root agents").unwrap();
            std::fs::write(child.join("AGENTS.md"), "child agents").unwrap();

            let content = render_provider(&root, &child).await;
            assert!(content.contains(INSTRUCTION_PRECEDENCE));
            assert!(
                content.contains("Project instructions from AGENTS.md (priority=parent/general)")
            );
            assert!(content.contains(
                "Project instructions from crates/demo/AGENTS.md (priority=local/general)"
            ));
            assert_before(&content, "root agents", "child agents");

            std::fs::remove_dir_all(root).ok();
        }

        #[tokio::test]
        async fn renders_same_directory_files_from_lowest_to_highest_precedence() {
            let root = temp_project("same-directory-order");
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(root.join("LATTICE.md"), "lattice native").unwrap();
            std::fs::write(root.join("AGENTS.md"), "agents general").unwrap();
            std::fs::write(root.join("CLAUDE.md"), "claude compat").unwrap();

            let content = render_provider(&root, &root).await;
            assert!(content.contains("Project instructions from CLAUDE.md (priority=local/compat)"));
            assert!(
                content.contains("Project instructions from AGENTS.md (priority=local/general)")
            );
            assert!(
                content.contains("Project instructions from LATTICE.md (priority=local/native)")
            );
            assert_before(&content, "claude compat", "agents general");
            assert_before(&content, "agents general", "lattice native");

            std::fs::remove_dir_all(root).ok();
        }

        #[tokio::test]
        async fn renders_mixed_root_child_files_by_file_tier_then_directory_depth() {
            let root = temp_project("mixed-matrix-order");
            let child = root.join("crates").join("demo");
            std::fs::create_dir_all(&child).unwrap();
            std::fs::write(root.join("CLAUDE.md"), "root claude").unwrap();
            std::fs::write(root.join("AGENTS.md"), "root agents").unwrap();
            std::fs::write(root.join("LATTICE.md"), "root lattice").unwrap();
            std::fs::write(child.join("CLAUDE.md"), "child claude").unwrap();
            std::fs::write(child.join("AGENTS.md"), "child agents").unwrap();
            std::fs::write(child.join("LATTICE.md"), "child lattice").unwrap();

            let content = render_provider(&root, &child).await;
            assert!(
                content.contains("Project instructions from CLAUDE.md (priority=parent/compat)")
            );
            assert!(content.contains(
                "Project instructions from crates/demo/CLAUDE.md (priority=local/compat)"
            ));
            assert!(
                content.contains("Project instructions from AGENTS.md (priority=parent/general)")
            );
            assert!(content.contains(
                "Project instructions from crates/demo/AGENTS.md (priority=local/general)"
            ));
            assert!(
                content.contains("Project instructions from LATTICE.md (priority=parent/native)")
            );
            assert!(content.contains(
                "Project instructions from crates/demo/LATTICE.md (priority=local/native)"
            ));
            assert_before(&content, "root claude", "child claude");
            assert_before(&content, "child claude", "root agents");
            assert_before(&content, "root agents", "child agents");
            assert_before(&content, "child agents", "root lattice");
            assert_before(&content, "root lattice", "child lattice");

            std::fs::remove_dir_all(root).ok();
        }

        #[tokio::test]
        async fn treats_at_file_references_as_literal_instruction_text() {
            let root = temp_project("literal-include");
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(root.join("AGENTS.md"), "literal include: @nested.md").unwrap();
            std::fs::write(root.join("nested.md"), "referenced file body").unwrap();

            let content = render_provider(&root, &root).await;
            assert!(content.contains("@nested.md"));
            assert!(!content.contains("referenced file body"));

            std::fs::remove_dir_all(root).ok();
        }

        #[cfg(unix)]
        #[tokio::test]
        async fn ignores_symlinked_instruction_files() {
            let root = temp_project("symlink-instructions");
            std::fs::create_dir_all(&root).unwrap();
            let outside = std::env::temp_dir().join(format!(
                "lattice-project-instructions-outside-{}",
                std::process::id()
            ));
            std::fs::write(&outside, "outside secret").unwrap();
            std::os::unix::fs::symlink(&outside, root.join("AGENTS.md")).unwrap();
            std::fs::write(root.join("LATTICE.md"), "local rules").unwrap();

            let content = render_provider(&root, &root).await;
            assert!(content.contains("local rules"));
            assert!(!content.contains("outside secret"));
            assert!(!content.contains("Project instructions from AGENTS.md"));

            std::fs::remove_dir_all(root).ok();
            std::fs::remove_file(outside).ok();
        }

        #[tokio::test]
        async fn loads_lattice_agents_and_claude_files_from_root_to_cwd() {
            let root = temp_project("all-files");
            let child = root.join("crates").join("demo");
            std::fs::create_dir_all(&child).unwrap();
            std::fs::write(root.join("LATTICE.md"), "root rules").unwrap();
            std::fs::write(child.join("AGENTS.md"), "child rules").unwrap();
            std::fs::write(child.join("CLAUDE.md"), "claude rules").unwrap();

            let provider = ProjectInstructionsProvider::new(&root, &child);
            let resolved = make_resolved();
            let ctx = AssemblyContext {
                request_id: "test",
                model: &resolved,
                user_input: "hello",
            };

            let section = provider.produce(&ctx).await.unwrap();
            assert_eq!(section.layer, Layer::Rules);
            assert!(section.content.contains("root rules"));
            assert!(section.content.contains("child rules"));
            assert!(section.content.contains("claude rules"));

            std::fs::remove_dir_all(root).ok();
        }
    }
}

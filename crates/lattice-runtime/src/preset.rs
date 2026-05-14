use std::path::{Path, PathBuf};

use lattice_agent::Agent;
use lattice_core::ResolvedModel;
use lattice_tools::{resolve_capabilities, CapabilitySelection, DefaultToolExecutor, ToolBundle};

use crate::RuntimeError;

// AgentPreset has been removed. Coding-capable behavior is now expressed
// through prompt content plus tool bundles, as per D-26/D-27/D-30.

pub fn coding_system_prompt(project_root: impl AsRef<Path>, cwd: impl AsRef<Path>) -> String {
    format!(
        "You are LATTICE Swarm, a repo-aware coding agent running inside the user's checkout.\n\n\
         Operating discipline:\n\
         - Read the code before making non-trivial changes; prefer grep and targeted read_file ranges over broad file dumps.\n\
         - Obey project instructions from LATTICE.md, AGENTS.md, and CLAUDE.md. More deeply nested files apply to the tree beneath them.\n\
         - Treat the worktree as shared with the user. Never overwrite unrelated local edits or deleted files.\n\
         - Use the single visible patch tool with structured patch text for precise edits.\n\
         - For update and delete patches, read files first to obtain the current content hashes and include those hashes in the patch text.\n\
         - Use write_file only when creating a brand-new file or replacing generated content.\n\
         - Treat multi_patch as retired and unavailable.\n\
         - Keep changes minimal, idiomatic for this repo, and covered by the narrowest useful verification.\n\
         - Before risky or hard-to-reverse operations, stop and surface the blocker instead of guessing.\n\
         - Never claim a change is verified unless you actually ran the relevant command and saw it succeed.\n\
         - When asked for a review, lead with bugs, regressions, security risks, and missing tests, with file/line references.\n\
         - Final answers should be concise: changed files, verification, and any residual risk.\n\n\
         Tool guidance:\n\
         - workspace_context gives a fresh repo snapshot; use it at the start of substantial work or after context loss.\n\
         - todo_write records multi-step plans and progress; keep exactly one active item when work is underway.\n\
         - read_file supports offset, limit, and line_numbers. Use it to inspect only the lines you need.\n\
         - read_files batches several targeted file reads when you already know the paths.\n\
         - grep and list_directory have result limits. If output is truncated, narrow the path or pattern.\n\
         - find_files is for locating paths by glob-like patterns before reading them.\n\
         - bash is sandboxed and may reject commands; use purpose-specific tools when possible.\n\n\
         Project root: {}\n\
         Working directory: {}",
        project_root.as_ref().display(),
        cwd.as_ref().display()
    )
}

pub(crate) fn apply_coding_preset(
    agent: &mut Agent,
    spec: &crate::AgentSpec,
    resolved: &ResolvedModel,
) -> Result<(), RuntimeError> {
    let project_root = spec.project_root.clone().unwrap_or_else(|| ".".to_string());
    let cwd = spec.cwd.clone().unwrap_or_else(|| project_root.clone());

    let project_root = validate_directory("project_root", PathBuf::from(&project_root))?;
    let cwd = validate_directory("cwd", PathBuf::from(&cwd))?;
    if !cwd.starts_with(&project_root) {
        return Err(RuntimeError::Config(format!(
            "cwd '{}' must be inside project_root '{}'",
            cwd.display(),
            project_root.display()
        )));
    }

    let capabilities = resolve_capabilities(&CapabilitySelection {
        bundles: vec![
            ToolBundle::Workspace,
            ToolBundle::Filesystem,
            ToolBundle::Editing,
            ToolBundle::Shell,
            ToolBundle::Planning,
            ToolBundle::Web,
        ],
        enabled: Vec::new(),
    });

    let security = spec.security.as_ref().ok_or_else(|| {
        RuntimeError::Config("coding-capable bundles require RuntimeSecurity".into())
    })?;
    let project_root_string = project_root.to_string_lossy().to_string();
    let executor = DefaultToolExecutor::new(&project_root_string)
        .map_err(RuntimeError::Config)?
        .with_sandbox(security.sandbox.clone())
        .with_authorized_tools(capabilities.tool_names.clone());

    *agent = std::mem::replace(agent, Agent::new(resolved.clone()))
        .with_tools(capabilities.definitions.clone())
        .with_tool_executor(Box::new(executor))
        .with_provider(lattice_prompt::ProjectInstructionsProvider::new(
            project_root.clone(),
            cwd.clone(),
        ))
        .with_provider(lattice_prompt::WorkspaceContextProvider::new(
            project_root.clone(),
            cwd.clone(),
        ))
        .with_task_state_provider();

    agent.set_system_prompt(&coding_system_prompt(&project_root, &cwd));

    if let Some(seed) = &spec.task_state_seed {
        *agent.task_state_mut() = seed.clone();
    }

    Ok(())
}

fn validate_directory(label: &str, path: PathBuf) -> Result<PathBuf, RuntimeError> {
    if path.as_os_str().is_empty() {
        return Err(RuntimeError::Config(format!("{label} must not be empty")));
    }

    let canonical = path.canonicalize().map_err(|err| {
        RuntimeError::Config(format!(
            "{label} '{}' does not exist or cannot be read: {err}",
            path.display()
        ))
    })?;
    if !canonical.is_dir() {
        return Err(RuntimeError::Config(format!(
            "{label} '{}' is not a directory",
            canonical.display()
        )));
    }
    Ok(canonical)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    use lattice_core::catalog::{ApiProtocol, CatalogProviderEntry, ModelCatalogEntry};
    use lattice_prompt::ContextProvider;

    use crate::{Runtime, RuntimeSecurity};

    fn register_test_model(runtime: &mut Runtime, name: &str) {
        runtime
            .register_model(ModelCatalogEntry {
                canonical_id: name.to_string(),
                context_length: 8192,
                providers: vec![CatalogProviderEntry {
                    provider_id: "ollama".to_string(),
                    api_model_id: name.to_string(),
                    priority: 1,
                    credential_keys: HashMap::new(),
                    base_url: Some("http://localhost:11434".to_string()),
                    api_protocol: ApiProtocol::OpenAiChat,
                    provider_specific: HashMap::new(),
                }],
                aliases: vec![],
            })
            .unwrap();
    }

    fn make_temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lattice-coding-preset-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn make_security(root: &Path) -> RuntimeSecurity {
        crate::build_runtime_security(&crate::RuntimeSecurityConfig::default(), root).unwrap()
    }

    fn make_coding_spec(root: &Path, cwd: &Path, _runtime: &Runtime) -> crate::AgentSpec {
        crate::AgentSpec::new("code-agent-factory-test-model".to_string())
            .project_root(root.to_string_lossy().to_string())
            .cwd(cwd.to_string_lossy().to_string())
            .security(Some(make_security(root)))
            .capability_preset("coding")
    }

    #[test]
    fn coding_preset_rejects_invalid_workspace_paths() {
        let mut runtime = Runtime::builder().build();
        register_test_model(&mut runtime, "code-agent-factory-test-model");

        let root = make_temp_dir("invalid-root");
        let missing = root.join("missing");
        let result = runtime.build_agent(
            crate::AgentSpec::new("code-agent-factory-test-model".to_string())
                .project_root(missing.to_string_lossy().to_string())
                .cwd(root.to_string_lossy().to_string())
                .security(Some(make_security(&root)))
                .capability_preset("coding"),
        );
        assert!(
            matches!(result, Err(RuntimeError::Config(message)) if message.contains("project_root"))
        );

        let outside = make_temp_dir("outside-cwd");
        let result = runtime.build_agent(make_coding_spec(&root, &outside, &runtime));
        assert!(
            matches!(result, Err(RuntimeError::Config(message)) if message.contains("must be inside project_root"))
        );

        let result = runtime.build_agent(
            crate::AgentSpec::new("code-agent-factory-test-model".to_string())
                .project_root(PathBuf::new().to_string_lossy().to_string())
                .cwd(root.to_string_lossy().to_string())
                .security(Some(make_security(&root)))
                .capability_preset("coding"),
        );
        assert!(
            matches!(result, Err(RuntimeError::Config(message)) if message.contains("project_root must not be empty"))
        );

        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(outside);
    }

    #[test]
    fn coding_preset_exposes_expected_authorized_tools() {
        let mut runtime = Runtime::builder().build();
        register_test_model(&mut runtime, "code-agent-factory-test-model");
        let root = make_temp_dir("tools");

        let built = runtime
            .build_agent(make_coding_spec(&root, &root, &runtime))
            .unwrap();
        let names = built
            .tool_definitions
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>();

        for expected in [
            "workspace_context",
            "read_file",
            "grep",
            "patch",
            "bash",
            "todo_write",
            "web_search",
        ] {
            assert!(names.contains(&expected), "{expected} missing");
        }
        assert!(!names.contains(&"multi_patch"), "multi_patch leaked");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn coding_preset_seeds_task_state() {
        let mut runtime = Runtime::builder().build();
        register_test_model(&mut runtime, "code-agent-factory-test-model");
        let root = make_temp_dir("task-state");
        let mut seed = lattice_agent::TaskState::new();
        seed.mark_unverified("cargo test not run".to_string());

        let built = runtime
            .build_agent(
                make_coding_spec(&root, &root, &runtime).task_state_seed(Some(seed.clone())),
            )
            .unwrap();

        assert_eq!(built.agent.task_state().snapshot(), seed);

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn coding_preset_uses_root_and_cwd_project_instructions() {
        let root = make_temp_dir("nested-instructions");
        let cwd = root.join("crates").join("demo");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::write(root.join("AGENTS.md"), "style = root").unwrap();
        std::fs::write(cwd.join("LATTICE.md"), "style = nested-lattice").unwrap();

        let provider = lattice_prompt::ProjectInstructionsProvider::new(root.clone(), cwd.clone());
        let resolved = lattice_core::ResolvedModel {
            canonical_id: "test".into(),
            provider: "test".into(),
            api_key: None,
            base_url: "".into(),
            api_protocol: ApiProtocol::OpenAiChat,
            api_model_id: "test".into(),
            context_length: 8192,
            provider_specific: HashMap::new(),
            credential_status: lattice_core::CredentialStatus::Missing,
        };
        let ctx = lattice_prompt::AssemblyContext {
            request_id: "test",
            model: &resolved,
            user_input: "hello",
        };

        let section = provider.produce(&ctx).await.unwrap();
        assert!(section.content.contains("style = root"));
        assert!(section.content.contains("style = nested-lattice"));
        assert!(section.content.contains(
            "Instruction precedence: deeper directories override parent directories; within one directory LATTICE.md > AGENTS.md > CLAUDE.md."
        ));
        assert!(
            section.content.find("style = root").unwrap()
                < section.content.find("style = nested-lattice").unwrap()
        );

        let mut runtime = Runtime::builder().build();
        register_test_model(&mut runtime, "code-agent-factory-test-model");
        runtime
            .build_agent(make_coding_spec(&root, &cwd, &runtime))
            .unwrap();

        let _ = std::fs::remove_dir_all(root);
    }
}

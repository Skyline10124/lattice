// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Fail-closed Upper configuration resolution and one-shot lease lifecycle.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lattice_profile::{
    AgentProfile, CredentialRef, EgressPolicyConfig, LoadedAgentProfile, WorkspaceSection,
};
use lattice_workspace::{
    AllowlistEntry, Backend, CredentialSpec, EgressPolicy, ImageOrTemplate, UpperHandle, UpperSpec,
    Workspace, WorkspaceError,
};

use super::workspace_backend::WorkspaceExecutionBackend;
use lancet_native::UpperContext;

/// Default Upper wall-clock lifetime when the profile omits an explicit budget.
pub const DEFAULT_UPPER_LIFETIME_SECS: u64 = 3600;

const UPPER_BACKED_TOOLS: &[&str] = &[
    "observe.workspace",
    "mutate.create",
    "mutate.edit",
    "mutate.batch",
    "exec.command_run",
];

/// Resolved immutable Upper configuration before adapter construction.
#[derive(Clone, Debug)]
pub struct ResolvedUpperConfig {
    pub spec: UpperSpec,
    pub requires_upper: bool,
}

/// One prepared Upper with its execution bridge.
pub struct PreparedUpper {
    pub lease: UpperLease,
    pub backend: WorkspaceExecutionBackend,
}

/// Owns one workspace adapter and at most one live Upper handle.
pub struct UpperLease {
    workspace: Arc<dyn Workspace>,
    handle: Option<UpperHandle>,
}

impl UpperLease {
    pub fn workspace(&self) -> &Arc<dyn Workspace> {
        &self.workspace
    }

    pub fn handle(&self) -> Option<&UpperHandle> {
        self.handle.as_ref()
    }

    pub async fn destroy(&mut self) -> Result<(), UpperPrepareError> {
        if let Some(handle) = self.handle.take() {
            self.workspace
                .destroy_upper(&handle)
                .await
                .map_err(|e| UpperPrepareError::Workspace(e.to_string()))?;
        }
        Ok(())
    }
}

impl Drop for UpperLease {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            let workspace = Arc::clone(&self.workspace);
            if let Ok(handle_rt) = tokio::runtime::Handle::try_current() {
                handle_rt.spawn(async move {
                    let _ = workspace.destroy_upper(&handle).await;
                });
            } else {
                eprintln!(
                    "UpperLease dropped without destroy() and no tokio runtime; \
                     Upper '{}' leaked",
                    handle.id
                );
            }
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum UpperPrepareError {
    #[error("workspace section required for Upper-backed tools")]
    MissingWorkspaceSection,
    #[error("workspace.image required for Upper-backed tools")]
    MissingWorkspaceSource,
    #[error("invalid workspace source: {0}")]
    InvalidWorkspaceSource(String),
    #[error("missing credential environment variable: {0}")]
    MissingCredential(String),
    #[error("git baseline resolution failed: {0}")]
    Baseline(String),
    #[error("workspace adapter error: {0}")]
    Workspace(String),
}

impl From<WorkspaceError> for UpperPrepareError {
    fn from(value: WorkspaceError) -> Self {
        Self::Workspace(value.to_string())
    }
}

pub fn upper_backed_tools_enabled(enabled_tools: &[String]) -> bool {
    enabled_tools
        .iter()
        .any(|tool| UPPER_BACKED_TOOLS.contains(&tool.as_str()))
}

pub fn resolve_explicit_image_or_template(
    image: &str,
) -> Result<ImageOrTemplate, UpperPrepareError> {
    let trimmed = image.trim();
    if trimmed.is_empty() {
        return Err(UpperPrepareError::MissingWorkspaceSource);
    }
    if trimmed.contains('/') {
        if trimmed.contains(':') || trimmed.contains('@') {
            return Ok(ImageOrTemplate::Image(trimmed.to_string()));
        }
        return Err(UpperPrepareError::InvalidWorkspaceSource(
            "ambiguous OCI reference".into(),
        ));
    }
    if is_valid_template_name(trimmed) {
        return Ok(ImageOrTemplate::Template(trimmed.to_string()));
    }
    Err(UpperPrepareError::InvalidWorkspaceSource(
        "invalid template name".into(),
    ))
}

fn is_valid_template_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

pub fn resolve_baseline_head(repo_path: &Path) -> Result<String, UpperPrepareError> {
    let repo = git2::Repository::open(repo_path)
        .map_err(|e| UpperPrepareError::Baseline(e.to_string()))?;
    let head = repo
        .head()
        .map_err(|e| UpperPrepareError::Baseline(e.to_string()))?;
    let oid = head
        .peel_to_commit()
        .map_err(|e| UpperPrepareError::Baseline(e.to_string()))?;
    Ok(oid.id().to_string())
}

pub fn resolve_credentials(
    refs: &[CredentialRef],
) -> Result<Vec<CredentialSpec>, UpperPrepareError> {
    let mut out = Vec::with_capacity(refs.len());
    for cred in refs {
        let value = std::env::var(&cred.env_var_name)
            .map_err(|_| UpperPrepareError::MissingCredential(cred.env_var_name.clone()))?;
        out.push(CredentialSpec::EnvVar {
            name: cred.env_var_name.clone(),
            value,
        });
    }
    Ok(out)
}

fn map_egress(section: &WorkspaceSection) -> EgressPolicy {
    match section.egress.clone().unwrap_or_default() {
        EgressPolicyConfig::Open => EgressPolicy::Open,
        EgressPolicyConfig::Blocked => EgressPolicy::Blocked,
        EgressPolicyConfig::Allowlist => EgressPolicy::Allowlist(
            section
                .allowed_egress_targets
                .iter()
                .map(|target| AllowlistEntry {
                    target: target.clone(),
                })
                .collect(),
        ),
    }
}

pub fn resolve_upper_config(
    profile: &LoadedAgentProfile,
    project_root: &Path,
    enabled_tools: &[String],
) -> Result<ResolvedUpperConfig, UpperPrepareError> {
    let requires_upper = upper_backed_tools_enabled(enabled_tools);
    let section = match profile.profile.workspace.as_ref() {
        Some(section) => section,
        None if requires_upper => return Err(UpperPrepareError::MissingWorkspaceSection),
        None => {
            return Ok(ResolvedUpperConfig {
                requires_upper: false,
                spec: UpperSpec {
                    image_or_template: ImageOrTemplate::Image(String::new()),
                    baseline_oid: String::new(),
                    host_repo_path: project_root.to_path_buf(),
                    egress: EgressPolicy::Blocked,
                    credentials: vec![],
                    timeout_secs: DEFAULT_UPPER_LIFETIME_SECS,
                    cpus: 1,
                    memory_mb: 512,
                },
            });
        }
    };

    if requires_upper
        && section
            .image
            .as_deref()
            .map(str::trim)
            .is_none_or(str::is_empty)
    {
        return Err(UpperPrepareError::MissingWorkspaceSource);
    }

    let image = section.image.as_deref().unwrap_or("");
    let image_or_template = if image.trim().is_empty() {
        ImageOrTemplate::Image(String::new())
    } else {
        resolve_explicit_image_or_template(image)?
    };
    let baseline_oid = resolve_baseline_head(project_root)?;
    let credentials = resolve_credentials(&section.credentials)?;

    Ok(ResolvedUpperConfig {
        requires_upper,
        spec: UpperSpec {
            image_or_template,
            baseline_oid,
            host_repo_path: project_root.to_path_buf(),
            egress: map_egress(section),
            credentials,
            timeout_secs: DEFAULT_UPPER_LIFETIME_SECS,
            cpus: section.cpus.unwrap_or(1),
            memory_mb: section.memory_mb.unwrap_or(512),
        },
    })
}

pub async fn prepare_upper(
    config: &ResolvedUpperConfig,
    workspace_override: Option<Arc<dyn Workspace>>,
) -> Result<PreparedUpper, UpperPrepareError> {
    if !config.requires_upper {
        return Err(UpperPrepareError::MissingWorkspaceSource);
    }

    let workspace = workspace_override.unwrap_or_else(|| {
        Arc::new(lattice_workspace::microsandbox::MicrosandboxWorkspace::from_env())
            as Arc<dyn Workspace>
    });

    let handle = workspace
        .create_upper(&config.spec)
        .await
        .map_err(UpperPrepareError::from)?;

    let upper_context = UpperContext {
        execution_backend: handle.backend.as_str().to_string(),
        upper_workspace_id: handle.id.clone(),
    };
    let backend = WorkspaceExecutionBackend::new(workspace.clone(), handle.clone(), upper_context);
    let lease = UpperLease {
        workspace,
        handle: Some(handle),
    };
    Ok(PreparedUpper { lease, backend })
}

#[cfg(test)]
mod upper_workspace_source_tests {
    use super::*;
    use lattice_profile::{
        AgentProfile, AgentSection, LancetSection, LlmSection, PromptSection, ToolsSection,
    };

    fn profile_with_workspace(image: Option<&str>) -> LoadedAgentProfile {
        LoadedAgentProfile {
            path: PathBuf::from("/tmp/agent.toml"),
            package_dir: PathBuf::from("/tmp"),
            profile: AgentProfile {
                schema_version: 1,
                agent: AgentSection {
                    name: "t".into(),
                    model: "m".into(),
                    description: None,
                },
                prompt: PromptSection {
                    system: Some("sys".into()),
                    system_path: None,
                },
                tools: ToolsSection {
                    enabled: vec!["observe.workspace".into()],
                },
                llm: LlmSection::default(),
                lancet: LancetSection::default(),
                workspace: Some(WorkspaceSection {
                    egress: None,
                    credentials: vec![],
                    image: image.map(str::to_string),
                    cpus: None,
                    memory_mb: None,
                    allowed_egress_targets: vec![],
                }),
            },
            system_prompt: "sys".into(),
        }
    }

    #[test]
    fn upper_workspace_source_missing_section_fails() {
        let profile = LoadedAgentProfile {
            path: PathBuf::from("/tmp/agent.toml"),
            package_dir: PathBuf::from("/tmp"),
            profile: AgentProfile {
                schema_version: 1,
                agent: AgentSection {
                    name: "t".into(),
                    model: "m".into(),
                    description: None,
                },
                prompt: PromptSection {
                    system: Some("sys".into()),
                    system_path: None,
                },
                tools: ToolsSection {
                    enabled: vec!["observe.workspace".into()],
                },
                llm: LlmSection::default(),
                lancet: LancetSection::default(),
                workspace: None,
            },
            system_prompt: "sys".into(),
        };
        let err = resolve_upper_config(&profile, Path::new("."), &["observe.workspace".into()])
            .unwrap_err();
        assert!(matches!(err, UpperPrepareError::MissingWorkspaceSection));
    }

    #[test]
    fn upper_workspace_source_missing_image_fails() {
        let profile = profile_with_workspace(None);
        let err = resolve_upper_config(&profile, Path::new("."), &["observe.workspace".into()])
            .unwrap_err();
        assert!(matches!(err, UpperPrepareError::MissingWorkspaceSource));
    }

    #[test]
    fn upper_workspace_source_explicit_oci_selection() {
        let image = "docker.io/library/python:3.12-slim";
        let resolved = resolve_explicit_image_or_template(image).unwrap();
        assert!(matches!(resolved, ImageOrTemplate::Image(v) if v == image));
    }

    #[test]
    fn upper_workspace_source_explicit_template_selection() {
        let resolved =
            resolve_explicit_image_or_template("proj-deadbeef-git-ready-template").unwrap();
        assert!(matches!(resolved, ImageOrTemplate::Template(_)));
    }

    #[test]
    fn upper_workspace_source_invalid_ambiguous_source_rejected() {
        let err = resolve_explicit_image_or_template("python:3.12-slim").unwrap_err();
        assert!(matches!(err, UpperPrepareError::InvalidWorkspaceSource(_)));
    }
}

#[cfg(test)]
mod upper_config_tests {
    use super::*;
    use lattice_profile::{
        AgentProfile, AgentSection, LancetSection, LlmSection, PromptSection, ToolsSection,
    };

    fn make_repo_with_commit() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        let sig = git2::Signature::now("t", "t@t").unwrap();
        std::fs::write(dir.path().join("f"), b"x").unwrap();
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let oid = repo
            .commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();
        (dir, oid.to_string())
    }

    #[test]
    fn upper_config_baseline_oid_matches_head() {
        let (dir, oid) = make_repo_with_commit();
        let head = resolve_baseline_head(dir.path()).unwrap();
        assert_eq!(head, oid);
    }

    #[test]
    fn upper_config_default_lifetime_constant() {
        assert_eq!(DEFAULT_UPPER_LIFETIME_SECS, 3600);
    }

    #[test]
    fn upper_config_resolves_with_explicit_image() {
        let (dir, _oid) = make_repo_with_commit();
        let profile = LoadedAgentProfile {
            path: PathBuf::from("/tmp/agent.toml"),
            package_dir: PathBuf::from("/tmp"),
            profile: AgentProfile {
                schema_version: 1,
                agent: AgentSection {
                    name: "t".into(),
                    model: "m".into(),
                    description: None,
                },
                prompt: PromptSection {
                    system: Some("sys".into()),
                    system_path: None,
                },
                tools: ToolsSection {
                    enabled: vec!["observe.workspace".into()],
                },
                llm: LlmSection::default(),
                lancet: LancetSection::default(),
                workspace: Some(WorkspaceSection {
                    egress: None,
                    credentials: vec![],
                    image: Some("docker.io/library/python:3.12-slim".into()),
                    cpus: None,
                    memory_mb: None,
                    allowed_egress_targets: vec![],
                }),
            },
            system_prompt: "sys".into(),
        };
        let cfg =
            resolve_upper_config(&profile, dir.path(), &["observe.workspace".into()]).unwrap();
        assert!(cfg.requires_upper);
        assert!(matches!(
            cfg.spec.image_or_template,
            ImageOrTemplate::Image(_)
        ));
    }
}

#[cfg(test)]
mod upper_lease_tests {
    use super::*;
    use async_trait::async_trait;
    use lattice_workspace::{ExecOutput, GitStatus, Workspace};

    struct NoopWorkspace {
        destroyed: std::sync::Mutex<Option<String>>,
    }

    #[async_trait]
    impl Workspace for NoopWorkspace {
        async fn create_upper(&self, _spec: &UpperSpec) -> lattice_workspace::Result<UpperHandle> {
            Ok(UpperHandle {
                id: "noop-1".into(),
                backend: Backend::Deterministic,
            })
        }

        async fn exec_upper(
            &self,
            _handle: &UpperHandle,
            _prog: &str,
            _args: &[&str],
        ) -> lattice_workspace::Result<ExecOutput> {
            Ok(ExecOutput {
                exit_code: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        }

        async fn git_status(&self, _handle: &UpperHandle) -> lattice_workspace::Result<GitStatus> {
            Ok(GitStatus::default())
        }

        async fn template_create(
            &self,
            _source: &UpperHandle,
            _name: &str,
        ) -> lattice_workspace::Result<String> {
            Ok("tpl".into())
        }

        async fn restore_upper(
            &self,
            _template_name: &str,
            _new_name: &str,
            _spec: &UpperSpec,
        ) -> lattice_workspace::Result<UpperHandle> {
            Ok(UpperHandle {
                id: "noop-2".into(),
                backend: Backend::Deterministic,
            })
        }

        async fn destroy_upper(&self, handle: &UpperHandle) -> lattice_workspace::Result<()> {
            *self.destroyed.lock().unwrap() = Some(handle.id.clone());
            Ok(())
        }

        async fn export_candidate(
            &self,
            _handle: &UpperHandle,
            _baseline_oid: &str,
            _max_artifact_bytes: u64,
        ) -> lattice_workspace::Result<lattice_workspace::CandidateExport> {
            Err(lattice_workspace::WorkspaceError::Unavailable("noop".into()))
        }
    }

    #[tokio::test]
    async fn upper_lease_destroy_consumes_handle_once() {
        let workspace = Arc::new(NoopWorkspace {
            destroyed: std::sync::Mutex::new(None),
        }) as Arc<dyn Workspace>;
        let mut lease = UpperLease {
            workspace,
            handle: Some(UpperHandle {
                id: "noop-1".into(),
                backend: Backend::Deterministic,
            }),
        };
        lease.destroy().await.expect("destroy once");
    }
}

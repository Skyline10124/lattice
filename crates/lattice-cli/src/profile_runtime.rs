use std::path::{Path, PathBuf};

use anyhow::Result;
use lattice_profile::{
    lower_profile_to_spec, resolve_agent_profile, DiscoveryPaths, ProfileSource,
};
use lattice_runtime::{
    AgentSessionRequest, Runtime, RuntimeAgentSession, RuntimeInvocationContext,
};
use sha2::{Digest, Sha256};

use crate::config::SecurityConfig;
use crate::security::RuntimeSecurity;

pub(crate) fn default_global_agents_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".lattice")
        .join("agents")
}

#[derive(Clone)]
pub(crate) struct ProfileRuntimeSessionRequest {
    pub(crate) source: ProfileSource,
    pub(crate) project_root: PathBuf,
    pub(crate) global_agents_dir: PathBuf,
    pub(crate) security_config: SecurityConfig,
    pub(crate) model_override: Option<String>,
    pub(crate) provider_override: Option<String>,
    pub(crate) session_id: Option<String>,
    pub(crate) task_state_seed: Option<lattice_agent::TaskState>,
}

pub(crate) fn prepare_profile_runtime_session(
    runtime: &Runtime,
    request: ProfileRuntimeSessionRequest,
) -> Result<RuntimeAgentSession> {
    let security =
        crate::security::build_runtime_security(&request.security_config, &request.project_root)?;
    prepare_profile_runtime_session_with_security(
        runtime,
        request.source,
        request.project_root,
        request.global_agents_dir,
        security,
        request.model_override,
        request.provider_override,
        request.session_id,
        request.task_state_seed,
    )
}

pub(crate) fn prepare_profile_runtime_session_with_security(
    runtime: &Runtime,
    source: ProfileSource,
    project_root: PathBuf,
    global_agents_dir: PathBuf,
    security: RuntimeSecurity,
    model_override: Option<String>,
    provider_override: Option<String>,
    session_id: Option<String>,
    task_state_seed: Option<lattice_agent::TaskState>,
) -> Result<RuntimeAgentSession> {
    let paths = DiscoveryPaths {
        project_root: project_root.clone(),
        global_agents_dir,
    };
    let loaded = resolve_agent_profile(source, &paths)?;
    let source_fingerprint = source_fingerprint(&loaded.path)?;
    let mut spec = lower_profile_to_spec(&loaded)?;
    if let Some(model) = model_override {
        spec.model = model;
    }
    let source_label = loaded.path.display().to_string();

    runtime
        .prepare_agent_session(AgentSessionRequest {
            spec,
            invocation: RuntimeInvocationContext {
                cwd: project_root.clone(),
                project_root,
                provider_override,
                security,
                task_state_seed,
            },
            session_id,
            source_label,
            source_fingerprint: Some(source_fingerprint),
        })
        .map_err(Into::into)
}

pub(crate) fn profile_source_from_agent_arg(agent: Option<&str>) -> ProfileSource {
    let Some(agent) = agent.map(str::trim).filter(|value| !value.is_empty()) else {
        return ProfileSource::Default;
    };
    let has_path_separator = agent.contains('/') || (cfg!(windows) && agent.contains('\\'));
    let is_path = Path::new(agent).is_absolute()
        || agent.starts_with("./")
        || agent.starts_with("../")
        || has_path_separator
        || agent.ends_with(".toml");
    if is_path {
        ProfileSource::Path(PathBuf::from(agent))
    } else {
        ProfileSource::Name(agent.to_string())
    }
}

pub(crate) fn create_default_profile(dir: &Path, template: &str) -> Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join("agent.toml");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    let mut file = options.open(&path)?;
    use std::io::Write as _;
    file.write_all(template.as_bytes())?;
    Ok(path)
}

pub(crate) fn missing_default_message(default_agent_dir: &Path) -> String {
    let profile_path = default_agent_dir.join("agent.toml");
    format!(
        "No default agent profile found at {}. Create one before running non-interactively.",
        profile_path.display()
    )
}

pub(crate) fn source_fingerprint(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path)?;
    Ok(format!("sha256:{:x}", Sha256::digest(&bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_fingerprint_changes_with_profile_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.toml");
        std::fs::write(&path, "schema_version = 1\n").unwrap();
        let first = source_fingerprint(&path).unwrap();
        std::fs::write(&path, "schema_version = 1\n# changed\n").unwrap();
        let second = source_fingerprint(&path).unwrap();
        assert_ne!(first, second);
        assert!(first.starts_with("sha256:"));
    }

    #[test]
    fn profile_runtime_source_audit_excludes_credentials() {
        let project = tempfile::tempdir().unwrap();
        let agent_dir = project
            .path()
            .join(".lattice")
            .join("agents")
            .join("default");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(
            agent_dir.join("agent.toml"),
            r#"
schema_version = 1

[agent]
name = "default"
model = "sonnet"

[prompt]
system = "Use no secrets."

[tools]
bundles = ["workspace", "filesystem"]
enabled = []
"#,
        )
        .unwrap();

        let runtime = Runtime::builder()
            .credentials(
                [
                    ("OPENAI_API_KEY".to_string(), "sk-test".to_string()),
                    (
                        "ANTHROPIC_API_KEY".to_string(),
                        "anthropic-test".to_string(),
                    ),
                ]
                .into_iter()
                .collect(),
            )
            .build();
        let session = prepare_profile_runtime_session(
            &runtime,
            ProfileRuntimeSessionRequest {
                source: ProfileSource::Default,
                project_root: project.path().to_path_buf(),
                global_agents_dir: tempfile::tempdir().unwrap().path().to_path_buf(),
                security_config: SecurityConfig::default(),
                model_override: None,
                provider_override: None,
                session_id: Some("audit-session".to_string()),
                task_state_seed: None,
            },
        )
        .unwrap();
        let audit = serde_json::to_string(&session.profile_snapshot.source).unwrap();
        let metadata = session.profile_snapshot.manifest_metadata();
        let metadata_text = serde_json::to_string(&metadata).unwrap();

        for secret in [
            "sk-test",
            "anthropic-test",
            "OPENAI_API_KEY",
            "ANTHROPIC_API_KEY",
            "api_key",
            "credentials",
        ] {
            assert!(!audit.contains(secret), "audit leaked {secret}");
            assert!(!metadata_text.contains(secret), "metadata leaked {secret}");
        }
        assert!(session
            .profile_snapshot
            .source
            .source_label
            .ends_with(".lattice/agents/default/agent.toml"));
        assert!(session
            .profile_snapshot
            .source
            .source_fingerprint
            .as_deref()
            .is_some_and(|value| value.starts_with("sha256:")));
    }

    #[test]
    fn agent_arg_none_uses_default_profile_source() {
        assert!(matches!(
            profile_source_from_agent_arg(None),
            ProfileSource::Default
        ));
        assert!(matches!(
            profile_source_from_agent_arg(Some("  ")),
            ProfileSource::Default
        ));
    }

    #[test]
    fn agent_arg_name_uses_named_profile_source() {
        match profile_source_from_agent_arg(Some("reviewer")) {
            ProfileSource::Name(name) => assert_eq!(name, "reviewer"),
            other => panic!("expected named source, got {other:?}"),
        }
    }

    #[test]
    fn agent_arg_path_uses_explicit_profile_source() {
        for value in [
            "./agent.toml",
            "../profiles/agent.toml",
            "/tmp/agent.toml",
            "profiles/reviewer",
            "reviewer.toml",
        ] {
            assert!(
                matches!(
                    profile_source_from_agent_arg(Some(value)),
                    ProfileSource::Path(_)
                ),
                "{value} should parse as path"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn agent_arg_windows_separator_uses_explicit_profile_source() {
        assert!(matches!(
            profile_source_from_agent_arg(Some("profiles\\reviewer")),
            ProfileSource::Path(_)
        ));
    }

    #[test]
    fn missing_default_creation_writes_project_local_template() {
        let dir = tempfile::tempdir().unwrap();
        let agent_dir = dir.path().join(".lattice/agents/default");
        let path = create_default_profile(&agent_dir, "schema_version = 1\n").unwrap();

        assert_eq!(path, agent_dir.join("agent.toml"));
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "schema_version = 1\n"
        );
    }

    #[test]
    fn missing_default_creation_refuses_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let agent_dir = dir.path().join(".lattice/agents/default");
        let path = create_default_profile(&agent_dir, "first").unwrap();

        let err = create_default_profile(&agent_dir, "second").unwrap_err();

        assert_eq!(std::fs::read_to_string(path).unwrap(), "first");
        assert_eq!(
            err.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::AlreadyExists
        );
    }

    #[test]
    fn profile_runtime_model_override_keeps_profile_semantics() {
        let project = tempfile::tempdir().unwrap();
        let agent_dir = project.path().join(".lattice/agents/default");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(
            agent_dir.join("agent.toml"),
            r#"
schema_version = 1

[agent]
name = "default"
model = "profile-model"

[prompt]
system = "Keep this prompt."

[tools]
bundles = ["workspace"]
enabled = []
"#,
        )
        .unwrap();

        let runtime = Runtime::builder().build();
        let session = prepare_profile_runtime_session(
            &runtime,
            ProfileRuntimeSessionRequest {
                source: ProfileSource::Default,
                project_root: project.path().to_path_buf(),
                global_agents_dir: tempfile::tempdir().unwrap().path().to_path_buf(),
                security_config: SecurityConfig::default(),
                model_override: Some("override-model".into()),
                provider_override: None,
                session_id: None,
                task_state_seed: None,
            },
        )
        .unwrap();

        assert_eq!(session.spec.model, "override-model");
        assert_eq!(session.spec.system_prompt, "Keep this prompt.");
        assert_eq!(session.spec.tools.bundles, vec!["workspace"]);
    }
}

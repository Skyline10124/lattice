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

#[allow(dead_code)] // Profile-source helper is the Phase 2 contract; CLI UX wires it in Phase 3.
pub(crate) struct ProfileRuntimeSessionRequest {
    pub(crate) source: ProfileSource,
    pub(crate) project_root: PathBuf,
    pub(crate) global_agents_dir: PathBuf,
    pub(crate) security_config: SecurityConfig,
    pub(crate) provider_override: Option<String>,
    pub(crate) session_id: Option<String>,
    pub(crate) task_state_seed: Option<lattice_agent::TaskState>,
}

pub(crate) struct GeneratedProfileRuntimeSessionRequest {
    pub(crate) profile_name: String,
    pub(crate) model: String,
    pub(crate) system_prompt: String,
    pub(crate) tool_bundles: Vec<String>,
    pub(crate) thinking_effort: Option<String>,
    pub(crate) project_root: PathBuf,
    pub(crate) security: RuntimeSecurity,
    pub(crate) provider_override: Option<String>,
    pub(crate) session_id: Option<String>,
    pub(crate) task_state_seed: Option<lattice_agent::TaskState>,
}

#[allow(dead_code)] // Profile-source helper is the Phase 2 contract; CLI UX wires it in Phase 3.
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
        request.provider_override,
        request.session_id,
        request.task_state_seed,
    )
}

pub(crate) fn prepare_generated_profile_runtime_session(
    runtime: &Runtime,
    request: GeneratedProfileRuntimeSessionRequest,
) -> Result<RuntimeAgentSession> {
    let dir = std::env::temp_dir().join(format!(
        "lattice-runtime-profile-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir)?;
    let profile_path = dir.join("agent.toml");
    std::fs::write(
        &profile_path,
        generated_agent_toml(
            &request.profile_name,
            &request.model,
            &request.system_prompt,
            &request.tool_bundles,
            request.thinking_effort.as_deref(),
        ),
    )?;
    let result = prepare_profile_runtime_session_with_security(
        runtime,
        ProfileSource::Path(profile_path),
        request.project_root,
        default_global_agents_dir(),
        request.security,
        request.provider_override,
        request.session_id,
        request.task_state_seed,
    );
    let _ = std::fs::remove_dir_all(&dir);
    result
}

fn prepare_profile_runtime_session_with_security(
    runtime: &Runtime,
    source: ProfileSource,
    project_root: PathBuf,
    global_agents_dir: PathBuf,
    security: RuntimeSecurity,
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
    let spec = lower_profile_to_spec(&loaded)?;
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

pub(crate) fn source_fingerprint(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path)?;
    Ok(format!("sha256:{:x}", Sha256::digest(&bytes)))
}

fn generated_agent_toml(
    profile_name: &str,
    model: &str,
    system_prompt: &str,
    tool_bundles: &[String],
    thinking_effort: Option<&str>,
) -> String {
    let bundles = tool_bundles
        .iter()
        .map(|bundle| toml_string(bundle))
        .collect::<Vec<_>>()
        .join(", ");
    let reasoning_effort = thinking_effort
        .map(|effort| format!("reasoning_effort = {}\n", toml_string(effort)))
        .unwrap_or_default();
    format!(
        r#"schema_version = 1

[agent]
name = {}
model = {}

[prompt]
system = {}

[tools]
bundles = [{bundles}]
enabled = []

[security]
sandbox_mode = "project"

[context]
project_instructions = true
workspace_context = true

[behavior]
max_turns = 10
{reasoning_effort}
"#,
        toml_string(profile_name),
        toml_string(model),
        toml_string(system_prompt)
    )
}

fn toml_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('"');
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            '\u{08}' => escaped.push_str("\\b"),
            '\u{0c}' => escaped.push_str("\\f"),
            ch if ch.is_control() => escaped.push_str(&format!("\\u{:04X}", ch as u32)),
            ch => escaped.push(ch),
        }
    }
    escaped.push('"');
    escaped
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
}

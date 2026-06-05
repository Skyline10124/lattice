// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::path::{Path, PathBuf};

use crate::error::ProfileError;
use crate::schema::AgentProfile;

/// A loaded and validated agent profile with resolved system prompt.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedAgentProfile {
    pub path: PathBuf,
    pub package_dir: PathBuf,
    pub profile: AgentProfile,
    pub system_prompt: String,
}

/// Forbidden top-level sections that must be rejected before deserialization.
const FORBIDDEN_SECTIONS: &[&str] = &[
    "capabilities",
    "handoff",
    "orchestrator",
    "plugins",
    "pipeline",
    "subagents",
    "micro_agents",
    "memory",
    "security",
    "context",
    "behavior",
];

/// Load and validate an `agent.toml` profile at `path`.
///
/// Performs a pre-scan for forbidden top-level sections before strict serde
/// deserialization so legacy errors can name the exact forbidden concept.
/// Validates `schema_version = 1`, mutual exclusivity of `system`/`system_path`,
/// and package-local path containment.
pub fn load_agent_profile(path: impl AsRef<Path>) -> Result<LoadedAgentProfile, ProfileError> {
    let path = path.as_ref().to_path_buf();

    let content = std::fs::read_to_string(&path).map_err(|source| ProfileError::Io {
        path: path.clone(),
        source,
    })?;

    // Pre-scan for forbidden top-level sections before strict deserialization.
    check_forbidden_sections(&path, &content)?;

    let profile: AgentProfile = toml::from_str(&content).map_err(|source| ProfileError::Toml {
        path: path.clone(),
        source,
    })?;

    // Validate schema_version.
    if profile.schema_version != 1 {
        return Err(ProfileError::UnsupportedSchemaVersion {
            path: path.clone(),
            found: profile.schema_version,
        });
    }

    // Validate prompt mutual exclusivity.
    let system_prompt = resolve_system_prompt(&path, &profile)?;
    validate_llm_retry(&path, &profile)?;

    let package_dir = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();

    Ok(LoadedAgentProfile {
        path,
        package_dir,
        profile,
        system_prompt,
    })
}

fn validate_llm_retry(path: &Path, profile: &AgentProfile) -> Result<(), ProfileError> {
    let retry = &profile.llm.retry;
    if retry.max_retries > 10 {
        return Err(ProfileError::InvalidField {
            path: path.to_path_buf(),
            section: "llm.retry".into(),
            field: "max_retries".into(),
            value: retry.max_retries.to_string().into_boxed_str(),
            reason: "too many LLM retry attempts".into(),
            expected: "max_retries must be <= 10".into(),
        });
    }
    if retry.initial_delay_ms > 60_000 {
        return Err(ProfileError::InvalidField {
            path: path.to_path_buf(),
            section: "llm.retry".into(),
            field: "initial_delay_ms".into(),
            value: retry.initial_delay_ms.to_string().into_boxed_str(),
            reason: "initial delay is too large".into(),
            expected: "initial_delay_ms must be <= 60000".into(),
        });
    }
    if retry.max_delay_ms > 60_000 {
        return Err(ProfileError::InvalidField {
            path: path.to_path_buf(),
            section: "llm.retry".into(),
            field: "max_delay_ms".into(),
            value: retry.max_delay_ms.to_string().into_boxed_str(),
            reason: "max delay is too large".into(),
            expected: "max_delay_ms must be <= 60000".into(),
        });
    }
    if retry.initial_delay_ms > retry.max_delay_ms {
        return Err(ProfileError::InvalidField {
            path: path.to_path_buf(),
            section: "llm.retry".into(),
            field: "initial_delay_ms".into(),
            value: retry.initial_delay_ms.to_string().into_boxed_str(),
            reason: "initial delay exceeds max delay".into(),
            expected: "initial_delay_ms must be <= max_delay_ms".into(),
        });
    }
    Ok(())
}

/// Check raw TOML content for forbidden top-level sections.
fn check_forbidden_sections(path: &Path, content: &str) -> Result<(), ProfileError> {
    let value: toml::Value = toml::from_str(content).map_err(|source| ProfileError::Toml {
        path: path.to_path_buf(),
        source,
    })?;

    let Some(table) = value.as_table() else {
        // Non-table input — this is effectively a schema error.
        return Err(ProfileError::ForbiddenSection {
            path: path.to_path_buf(),
            section: "(root)".into(),
            reason: "profile must be a TOML table".into(),
        });
    };

    for (key, _) in table {
        let lower = key.to_lowercase();
        if FORBIDDEN_SECTIONS.contains(&lower.as_str()) {
            return Err(ProfileError::ForbiddenSection {
                path: path.to_path_buf(),
                section: key.clone(),
                reason: format!("`[{key}]` is not part of the v1 agent profile schema"),
            });
        }
    }

    Ok(())
}

/// Resolve the system prompt from either inline `system` or `system_path`.
fn resolve_system_prompt(path: &Path, profile: &AgentProfile) -> Result<String, ProfileError> {
    let system = profile.prompt.system.as_deref().filter(|s| !s.is_empty());
    let system_path = profile
        .prompt
        .system_path
        .as_deref()
        .filter(|s| !s.is_empty());

    match (system, system_path) {
        (Some(_), Some(_)) => Err(ProfileError::PromptMutuallyExclusive {
            path: path.to_path_buf(),
        }),
        (None, None) => Err(ProfileError::MissingPrompt {
            path: path.to_path_buf(),
        }),
        (Some(inline), None) => Ok(inline.to_string()),
        (None, Some(rel_path)) => load_prompt_file(path, rel_path),
    }
}

/// Load and validate a prompt file relative to the profile's package directory.
fn load_prompt_file(profile_path: &Path, rel_path: &str) -> Result<String, ProfileError> {
    let rel = Path::new(rel_path);

    if rel.is_absolute() {
        return Err(ProfileError::AbsoluteSystemPath {
            profile_path: profile_path.to_path_buf(),
            given: rel_path.to_string(),
        });
    }

    if rel_path.contains("..") {
        return Err(ProfileError::SystemPathTraversal {
            profile_path: profile_path.to_path_buf(),
            given: rel_path.to_string(),
        });
    }

    let package_dir = profile_path.parent().unwrap_or_else(|| Path::new("."));
    let resolved = package_dir.join(rel);

    let canonical_package =
        package_dir
            .canonicalize()
            .map_err(|source| ProfileError::SystemPathNotFound {
                profile_path: profile_path.to_path_buf(),
                given: package_dir.to_string_lossy().to_string(),
                source,
            })?;
    let canonical_resolved =
        resolved
            .canonicalize()
            .map_err(|source| ProfileError::SystemPathNotFound {
                profile_path: profile_path.to_path_buf(),
                given: rel_path.to_string(),
                source,
            })?;

    if !canonical_resolved.starts_with(&canonical_package) {
        return Err(ProfileError::PathEscape {
            profile_path: profile_path.to_path_buf(),
            package_dir: canonical_package,
            given: rel_path.to_string(),
            resolved: canonical_resolved,
        });
    }

    if !canonical_resolved.is_file() {
        return Err(ProfileError::SystemPathNotAFile {
            profile_path: profile_path.to_path_buf(),
            given: rel_path.to_string(),
            resolved: canonical_resolved,
        });
    }

    std::fs::read_to_string(&canonical_resolved).map_err(|source| ProfileError::Io {
        path: canonical_resolved,
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_profile(content: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let toml_path = dir.path().join("agent.toml");
        std::fs::write(&toml_path, content).unwrap();
        (dir, toml_path)
    }

    #[test]
    fn load_inline_system_prompt() {
        let (_dir, path) = temp_profile(
            r#"
schema_version = 1

[agent]
name = "test"
model = "sonnet"

[prompt]
system = "You are a helpful assistant."
"#,
        );
        let loaded = load_agent_profile(&path).unwrap();
        assert_eq!(loaded.system_prompt, "You are a helpful assistant.");
        assert_eq!(loaded.profile.agent.name, "test");
    }

    #[test]
    fn load_system_path_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let prompt_path = dir.path().join("prompt.md");
        std::fs::write(&prompt_path, "Be concise and helpful.").unwrap();

        let toml_path = dir.path().join("agent.toml");
        let toml = format!(
            r#"
schema_version = 1

[agent]
name = "test"
model = "sonnet"

[prompt]
system_path = "{}"
"#,
            prompt_path.file_name().unwrap().to_str().unwrap()
        );
        std::fs::write(&toml_path, toml).unwrap();

        let loaded = load_agent_profile(&toml_path).unwrap();
        assert_eq!(loaded.system_prompt, "Be concise and helpful.");
    }

    #[test]
    fn reject_both_system_and_system_path() {
        let (_dir, path) = temp_profile(
            r#"
schema_version = 1

[agent]
name = "test"
model = "sonnet"

[prompt]
system = "inline"
system_path = "prompt.md"
"#,
        );
        let err = load_agent_profile(&path).unwrap_err();
        assert!(err.to_string().contains("both"), "got: {err}");
    }

    #[test]
    fn reject_neither_system_nor_system_path() {
        let (_dir, path) = temp_profile(
            r#"
schema_version = 1

[agent]
name = "test"
model = "sonnet"

[prompt]
"#,
        );
        let err = load_agent_profile(&path).unwrap_err();
        assert!(err.to_string().contains("neither"), "got: {err}");
    }

    #[test]
    fn reject_missing_prompt_file() {
        let dir = tempfile::tempdir().unwrap();
        let toml_path = dir.path().join("agent.toml");
        std::fs::write(
            &toml_path,
            r#"
schema_version = 1

[agent]
name = "test"
model = "sonnet"

[prompt]
system_path = "nonexistent.md"
"#,
        )
        .unwrap();
        let err = load_agent_profile(&toml_path).unwrap_err();
        assert!(err.to_string().contains("nonexistent"), "got: {err}");
    }

    #[test]
    fn reject_absolute_system_path() {
        let dir = tempfile::tempdir().unwrap();
        let toml_path = dir.path().join("agent.toml");
        std::fs::write(
            &toml_path,
            r#"
schema_version = 1

[agent]
name = "test"
model = "sonnet"

[prompt]
system_path = "/etc/passwd"
"#,
        )
        .unwrap();
        let err = load_agent_profile(&toml_path).unwrap_err();
        assert!(err.to_string().contains("absolute"), "got: {err}");
    }

    #[test]
    fn reject_dot_dot_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let toml_path = dir.path().join("agent.toml");
        std::fs::write(
            &toml_path,
            r#"
schema_version = 1

[agent]
name = "test"
model = "sonnet"

[prompt]
system_path = "../secret/prompt.md"
"#,
        )
        .unwrap();
        let err = load_agent_profile(&toml_path).unwrap_err();
        assert!(err.to_string().contains(".."), "got: {err}");
    }

    #[test]
    fn reject_path_escape_via_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let secret = outside.path().join("secret.md");
        std::fs::write(&secret, "secret").unwrap();

        let toml_path = dir.path().join("agent.toml");
        let link = dir.path().join("escape");
        std::os::unix::fs::symlink(&outside, &link).unwrap();

        std::fs::write(
            &toml_path,
            r#"
schema_version = 1

[agent]
name = "test"
model = "sonnet"

[prompt]
system_path = "escape/secret.md"
"#,
        )
        .unwrap();
        let err = load_agent_profile(&toml_path).unwrap_err();
        assert!(err.to_string().contains("escapes"), "got: {err}");
    }

    #[test]
    fn reject_prompt_path_that_is_directory() {
        let dir = tempfile::tempdir().unwrap();
        let subdir = dir.path().join("subdir");
        std::fs::create_dir(&subdir).unwrap();

        let toml_path = dir.path().join("agent.toml");
        std::fs::write(
            &toml_path,
            r#"
schema_version = 1

[agent]
name = "test"
model = "sonnet"

[prompt]
system_path = "subdir"
"#,
        )
        .unwrap();
        let err = load_agent_profile(&toml_path).unwrap_err();
        assert!(err.to_string().contains("not a file"), "got: {err}");
    }

    #[test]
    fn loading_is_read_only_no_side_files() {
        let dir = tempfile::tempdir().unwrap();
        let toml_path = dir.path().join("agent.toml");
        std::fs::write(
            &toml_path,
            r#"
schema_version = 1

[agent]
name = "test"
model = "sonnet"

[prompt]
system = "hello"
"#,
        )
        .unwrap();

        let before: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        load_agent_profile(&toml_path).unwrap();
        let after: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();

        assert_eq!(
            before.len(),
            after.len(),
            "loader created side-effect files"
        );
    }

    #[test]
    fn reject_schema_version_0() {
        let (_dir, path) = temp_profile(
            r#"
schema_version = 0

[agent]
name = "test"
model = "sonnet"

[prompt]
system = "hi"
"#,
        );
        let err = load_agent_profile(&path).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("schema_version") && msg.contains("0"),
            "got: {err}"
        );
    }

    #[test]
    fn reject_schema_version_2() {
        let (_dir, path) = temp_profile(
            r#"
schema_version = 2

[agent]
name = "test"
model = "sonnet"

[prompt]
system = "hi"
"#,
        );
        let err = load_agent_profile(&path).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("schema_version") && msg.contains("2"),
            "got: {err}"
        );
    }

    #[test]
    fn reject_forbidden_sections_in_loader() {
        let forbidden = [
            "capabilities",
            "handoff",
            "orchestrator",
            "pipeline",
            "memory",
        ];
        for section in forbidden {
            let dir = tempfile::tempdir().unwrap();
            let toml_path = dir.path().join("agent.toml");
            let toml = format!(
                r#"
schema_version = 1

[agent]
name = "t"
model = "sonnet"

[prompt]
system = "hi"

[{section}]
x = 1
"#
            );
            std::fs::write(&toml_path, toml).unwrap();
            let err = load_agent_profile(&toml_path).unwrap_err();
            assert!(
                err.to_string().contains(section),
                "expected '{section}' in error, got: {err}"
            );
        }
    }

    #[test]
    fn llm_retry_max_retries_zero_is_valid() {
        let (_dir, path) = temp_profile(
            r#"
schema_version = 1

[agent]
name = "test"
model = "sonnet"

[prompt]
system = "hi"

[llm.retry]
max_retries = 0
initial_delay_ms = 1
max_delay_ms = 1
"#,
        );
        let loaded = load_agent_profile(&path).unwrap();
        assert_eq!(loaded.profile.llm.retry.max_retries, 0);
    }

    #[test]
    fn reject_invalid_llm_retry_bounds() {
        let cases = [
            (
                "max_retries = 11\ninitial_delay_ms = 1\nmax_delay_ms = 1",
                "max_retries",
            ),
            (
                "max_retries = 1\ninitial_delay_ms = 60001\nmax_delay_ms = 60001",
                "initial_delay_ms",
            ),
            (
                "max_retries = 1\ninitial_delay_ms = 10\nmax_delay_ms = 5",
                "initial_delay_ms",
            ),
        ];
        for (retry, field) in cases {
            let (_dir, path) = temp_profile(&format!(
                r#"
schema_version = 1

[agent]
name = "test"
model = "sonnet"

[prompt]
system = "hi"

[llm.retry]
{retry}
"#
            ));
            let err = load_agent_profile(&path).unwrap_err();
            assert!(
                err.to_string().contains(field),
                "expected {field} in error, got: {err}"
            );
        }
    }

    #[test]
    fn schema_version_must_be_1() {
        for version in [0, 2, 3, 99] {
            let (_dir, path) = temp_profile(&format!(
                r#"
schema_version = {}

[agent]
name = "test"
model = "sonnet"

[prompt]
system = "hi"
"#,
                version
            ));
            let err = load_agent_profile(&path).unwrap_err();
            let msg = err.to_string();
            assert!(
                msg.contains("schema_version") && msg.contains(&version.to_string()),
                "version {}: got: {err}",
                version
            );
        }
    }

    #[test]
    fn all_forbidden_sections_rejected() {
        // Covers: capabilities, handoff, orchestrator, plugins, pipeline,
        // subagents, micro_agents, memory
        let forbidden = [
            "capabilities",
            "handoff",
            "orchestrator",
            "plugins",
            "pipeline",
            "subagents",
            "micro_agents",
            "memory",
        ];
        for section in forbidden {
            let dir = tempfile::tempdir().unwrap();
            let toml_path = dir.path().join("agent.toml");
            let toml = format!(
                r#"
schema_version = 1

[agent]
name = "t"
model = "sonnet"

[prompt]
system = "hi"

[{section}]
x = 1
"#
            );
            std::fs::write(&toml_path, toml).unwrap();
            let err = load_agent_profile(&toml_path).unwrap_err();
            assert!(
                err.to_string().contains(section),
                "section '{section}': got: {err}"
            );
        }
    }
}

// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::{
    io::{self, Read},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

use lattice_conversation::{ModelRef, ModelRefError};
use lattice_session::{
    AgentName, AgentNameError, MAX_AGENT_NAME_BYTES, MAX_MODEL_ATTEMPTS, MAX_RETRY_DELAY_MS,
    MAX_TURN_TIMEOUT_SECS, RetryPolicy, RetryPolicyError, TurnTimeout, TurnTimeoutError,
};

use crate::discovery::{DiscoveryPaths, discover_profile_path};
use crate::error::ProfileError;
use crate::schema::{
    AgentProfile, AgentSection, LlmSection, RawAgentProfile, RawPromptSection, RawRetryPolicy,
};

const MAX_SYSTEM_PROMPT_BYTES: usize = 2 * 1024 * 1024;
// 2 MiB 提示全部写成 \uXXXX 时最多占 12 MiB；16 MiB 为 TOML 转义与其余字段留出空间，并给读取设界。
const MAX_PROFILE_FILE_BYTES: usize = 16 * 1024 * 1024;

/// group 与 other 的写位。读与执行位不拒绝。
const FOREIGN_WRITE_BITS: u32 = 0o022;

/// A loaded and validated agent profile with resolved system prompt.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedAgentProfile {
    pub path: PathBuf,
    pub package_dir: PathBuf,
    pub profile: AgentProfile,
    pub system_prompt: String,
}

/// 严格反序列化前拒绝的顶层配置节。
///
/// `lancet` 与 `tools` 随旧 LANCET 移除（ADR 0009 决策 6）；`workspace` 为早期
/// Upper microVM 退役节。预扫描在此给出明确诊断，而非落入泛化的 `deny_unknown_fields`。
const FORBIDDEN_SECTIONS: &[&str] = &[
    "lancet",
    "tools",
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
    "workspace",
];

/// 生产唯一的 Agent profile 加载入口。
///
/// 按顺序执行：路径发现 → 全局目录/包/文件属主与写权限检查 → 严格 TOML 解析 →
/// `[agent].name` 解析与一致性校验 → 领域字段构造 → 系统提示解析（含外部提示文件保护检查）。
pub fn load_agent_profile(
    name: &AgentName,
    paths: &DiscoveryPaths,
    expected_uid: u32,
) -> Result<LoadedAgentProfile, ProfileError> {
    let discovered = discover_profile_path(name, paths)?;
    let path = discovered.path;
    let package_dir = discovered.package_dir;

    if discovered.is_global {
        check_protected(&paths.global_agents_dir, true, expected_uid)?;
        check_protected(&package_dir, true, expected_uid)?;
        check_protected(&path, false, expected_uid)?;
    }

    let content =
        read_regular_file(&path, MAX_PROFILE_FILE_BYTES).map_err(|source| ProfileError::Io {
            path: path.clone(),
            source,
        })?;

    // Pre-scan for forbidden top-level sections before strict deserialization.
    check_forbidden_sections(&path, &content)?;

    let raw: RawAgentProfile = toml::from_str(&content).map_err(|source| ProfileError::Toml {
        path: path.clone(),
        line: toml_error_line(&source, &content),
        source,
    })?;

    if raw.schema_version != 1 {
        return Err(ProfileError::UnsupportedSchemaVersion {
            path: path.clone(),
            found: raw.schema_version,
        });
    }

    let declared_name = parse_declared_agent_name(&path, name, raw.agent.name)?;
    let model = raw
        .agent
        .model
        .map(|raw_model| parse_agent_model(&path, raw_model))
        .transpose()?;
    let timeout_secs = parse_llm_timeout(&path, raw.llm.timeout_secs)?;
    let retry = parse_llm_retry(&path, &raw.llm.retry)?;

    let system_prompt = resolve_system_prompt(
        &path,
        &package_dir,
        &raw.prompt,
        discovered
            .is_global
            .then_some((paths.global_agents_dir.as_path(), expected_uid)),
    )?;

    let profile = AgentProfile {
        agent: AgentSection {
            name: declared_name,
            model,
            effort: raw.agent.effort,
            description: raw.agent.description,
        },
        llm: LlmSection {
            timeout_secs,
            retry,
        },
    };

    Ok(LoadedAgentProfile {
        path,
        package_dir,
        profile,
        system_prompt,
    })
}

/// 全局 agents 目录与其中的 profile 文件只能由当前用户改写。
/// profile 不是秘密，group/other 可读（默认 umask 022 的 0755/0644）照常接受；
/// 风险是被他人改写，所以只拒绝 group/other 写位。auth.json 的 0600/0700 规则另行维护。
fn check_protected(path: &Path, directory: bool, owner: u32) -> Result<(), ProfileError> {
    let label = if directory {
        "global agents directory"
    } else {
        "profile file"
    };
    let reject = |reason: String| ProfileError::Protection {
        path: path.to_path_buf(),
        reason,
    };
    // symlink_metadata 不跟随最后一级符号链接，避免把链接目标当成配置本身。
    let meta = std::fs::symlink_metadata(path)
        .map_err(|error| reject(format!("cannot inspect {label} ({})", error.kind())))?;
    if meta.file_type().is_symlink() {
        return Err(reject(format!(
            "{label} must not be a symbolic link; permissions were not changed"
        )));
    }
    if meta.uid() != owner {
        return Err(reject(format!("{label} must belong to the current user")));
    }
    if directory && !meta.is_dir() {
        return Err(reject(format!("{label} must be a directory")));
    }
    if !directory && !meta.is_file() {
        return Err(reject(format!("{label} must be a regular file")));
    }
    if meta.mode() & FOREIGN_WRITE_BITS != 0 {
        return Err(reject(format!(
            "{label} must not be writable by group or other; permissions were not changed"
        )));
    }
    Ok(())
}

/// 读取普通 UTF-8 文件，最多读入 limit 加一字节，超限时返回 InvalidData。
pub fn read_regular_file(path: &Path, limit: usize) -> io::Result<String> {
    if !std::fs::metadata(path)?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("file exceeds {limit} bytes"),
        ));
    }
    String::from_utf8(bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn toml_error_line(error: &toml::de::Error, content: &str) -> Option<std::num::NonZeroU32> {
    let start = error.span()?.start;
    let newlines = content
        .as_bytes()
        .get(..start)?
        .iter()
        .filter(|byte| **byte == b'\n')
        .count();
    std::num::NonZeroU32::new(u32::try_from(newlines).ok()?.checked_add(1)?)
}

fn parse_declared_agent_name(
    path: &Path,
    expected: &AgentName,
    raw_name: String,
) -> Result<AgentName, ProfileError> {
    let parsed = AgentName::parse(&raw_name).map_err(|error| {
        let (reason, expected_rule) = match error {
            AgentNameError::Empty => ("agent name is empty", error.to_string()),
            AgentNameError::TooLong => (
                "agent name is too long",
                format!("agent name must be at most {MAX_AGENT_NAME_BYTES} bytes"),
            ),
            AgentNameError::ControlCharacter => {
                ("agent name contains control characters", error.to_string())
            }
            AgentNameError::Slash => ("agent name contains '/'", error.to_string()),
            AgentNameError::DotComponent => {
                ("agent name is a dot path component", error.to_string())
            }
        };
        ProfileError::InvalidField {
            path: path.to_path_buf(),
            section: "agent".into(),
            field: "name".into(),
            value: raw_name.clone().into_boxed_str(),
            reason: reason.into(),
            expected: expected_rule.into(),
        }
    })?;

    if &parsed != expected {
        return Err(ProfileError::InvalidField {
            path: path.to_path_buf(),
            section: "agent".into(),
            field: "name".into(),
            value: raw_name.into_boxed_str(),
            reason: "declared agent name does not match selected agent".into(),
            expected: format!("expected `{expected}`").into(),
        });
    }

    Ok(parsed)
}

fn parse_agent_model(path: &Path, raw_model: String) -> Result<ModelRef, ProfileError> {
    ModelRef::parse(&raw_model).map_err(|error| {
        let reason = match error {
            ModelRefError::MissingSeparator => "missing provider/model separator '/'",
            ModelRefError::InvalidProvider(_) => "invalid provider id in model reference",
            ModelRefError::InvalidModel(_) => "invalid model id in model reference",
        };
        ProfileError::InvalidField {
            path: path.to_path_buf(),
            section: "agent".into(),
            field: "model".into(),
            value: raw_model.into_boxed_str(),
            reason: reason.into(),
            expected: error.to_string().into(),
        }
    })
}

fn parse_llm_retry(path: &Path, retry: &RawRetryPolicy) -> Result<RetryPolicy, ProfileError> {
    RetryPolicy::new(
        retry.max_retries,
        retry.initial_delay_ms,
        retry.max_delay_ms,
    )
    .map_err(|error| match error {
        RetryPolicyError::TooManyRetries => ProfileError::InvalidField {
            path: path.to_path_buf(),
            section: "llm.retry".into(),
            field: "max_retries".into(),
            value: retry.max_retries.to_string().into_boxed_str(),
            reason: "too many LLM retry attempts".into(),
            expected: format!("max_retries must be <= {}", MAX_MODEL_ATTEMPTS - 1).into(),
        },
        RetryPolicyError::InitialDelayTooLarge => ProfileError::InvalidField {
            path: path.to_path_buf(),
            section: "llm.retry".into(),
            field: "initial_delay_ms".into(),
            value: retry.initial_delay_ms.to_string().into_boxed_str(),
            reason: "initial delay is too large".into(),
            expected: format!("initial_delay_ms must be <= {MAX_RETRY_DELAY_MS}").into(),
        },
        RetryPolicyError::MaxDelayTooLarge => ProfileError::InvalidField {
            path: path.to_path_buf(),
            section: "llm.retry".into(),
            field: "max_delay_ms".into(),
            value: retry.max_delay_ms.to_string().into_boxed_str(),
            reason: "max delay is too large".into(),
            expected: format!("max_delay_ms must be <= {MAX_RETRY_DELAY_MS}").into(),
        },
        RetryPolicyError::InitialDelayExceedsMax => ProfileError::InvalidField {
            path: path.to_path_buf(),
            section: "llm.retry".into(),
            field: "initial_delay_ms".into(),
            value: retry.initial_delay_ms.to_string().into_boxed_str(),
            reason: "initial delay exceeds max delay".into(),
            expected: "initial_delay_ms must be <= max_delay_ms".into(),
        },
    })
}

fn parse_llm_timeout(path: &Path, timeout_secs: Option<u64>) -> Result<TurnTimeout, ProfileError> {
    let Some(secs) = timeout_secs else {
        return Ok(TurnTimeout::default());
    };
    TurnTimeout::new(secs).map_err(|error| {
        let (reason, expected) = match error {
            TurnTimeoutError::Zero => (
                "timeout must be nonzero",
                "timeout_secs must be > 0".to_owned(),
            ),
            TurnTimeoutError::TooLarge => (
                "timeout is too large",
                format!("timeout_secs must be <= {MAX_TURN_TIMEOUT_SECS}"),
            ),
        };
        ProfileError::InvalidField {
            path: path.to_path_buf(),
            section: "llm".into(),
            field: "timeout_secs".into(),
            value: secs.to_string().into_boxed_str(),
            reason: reason.into(),
            expected: expected.into(),
        }
    })
}

/// Check raw TOML content for forbidden top-level sections.
fn check_forbidden_sections(path: &Path, content: &str) -> Result<(), ProfileError> {
    let value: toml::Value = toml::from_str(content).map_err(|source| ProfileError::Toml {
        path: path.to_path_buf(),
        line: toml_error_line(&source, content),
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
            let reason = match lower.as_str() {
                "lancet" | "tools" => {
                    format!("`[{lower}]` has been removed; delete this section from agent.toml")
                }
                _ => format!("`[{key}]` is not part of the v1 agent profile schema"),
            };
            return Err(ProfileError::ForbiddenSection {
                path: path.to_path_buf(),
                section: key.clone(),
                reason,
            });
        }
    }

    Ok(())
}

/// Resolve the system prompt from either inline `system` or `system_path`.
fn resolve_system_prompt(
    path: &Path,
    package_dir: &Path,
    prompt: &RawPromptSection,
    global_protection: Option<(&Path, u32)>,
) -> Result<String, ProfileError> {
    let system = prompt.system.as_deref().filter(|s| !s.is_empty());
    let system_path = prompt.system_path.as_deref().filter(|s| !s.is_empty());

    match (system, system_path) {
        (Some(_), Some(_)) => Err(ProfileError::PromptMutuallyExclusive {
            path: path.to_path_buf(),
        }),
        (None, None) => Err(ProfileError::MissingPrompt {
            path: path.to_path_buf(),
        }),
        (Some(inline), None) => {
            if inline.len() > MAX_SYSTEM_PROMPT_BYTES {
                return Err(ProfileError::InvalidField {
                    path: path.to_path_buf(),
                    section: "prompt".into(),
                    field: "system".into(),
                    value: inline.len().to_string().into_boxed_str(),
                    reason: "system prompt is too large".into(),
                    expected: format!("system prompt must be <= {MAX_SYSTEM_PROMPT_BYTES} bytes")
                        .into(),
                });
            }
            Ok(inline.to_string())
        }
        (None, Some(rel_path)) => load_prompt_file(path, package_dir, rel_path, global_protection),
    }
}

/// Load and validate a prompt file relative to the profile's package directory.
fn load_prompt_file(
    profile_path: &Path,
    package_dir: &Path,
    rel_path: &str,
    global_protection: Option<(&Path, u32)>,
) -> Result<String, ProfileError> {
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

    let resolved = package_dir.join(rel);
    // 外部提示路径在读取内容前完成全局属主与写位检查，不先打开未受保护的文件。
    if let Some((global_agents_dir, expected_uid)) = global_protection
        && resolved.starts_with(global_agents_dir)
    {
        match std::fs::symlink_metadata(&resolved) {
            Ok(_) => check_protected(&resolved, false, expected_uid)?,
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                return Err(ProfileError::SystemPathNotFound {
                    profile_path: profile_path.to_path_buf(),
                    given: rel_path.to_string(),
                    source,
                });
            }
            Err(error) => {
                return Err(ProfileError::Protection {
                    path: resolved,
                    reason: format!("cannot inspect profile file ({})", error.kind()),
                });
            }
        }
    }

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

    read_regular_file(&canonical_resolved, MAX_SYSTEM_PROMPT_BYTES).map_err(|source| {
        ProfileError::Io {
            path: profile_path.to_path_buf(),
            source: io::Error::new(
                source.kind(),
                format!("prompt file '{}': {source}", canonical_resolved.display()),
            ),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    struct ProjectProfileFixture {
        _dir: tempfile::TempDir,
        paths: DiscoveryPaths,
        uid: u32,
        name: AgentName,
        toml_path: PathBuf,
        package_dir: PathBuf,
    }

    impl ProjectProfileFixture {
        fn new(agent: &str, content: &str) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let uid = std::fs::symlink_metadata(dir.path()).unwrap().uid();
            let paths = DiscoveryPaths {
                project_root: dir.path().join("project"),
                global_agents_dir: dir.path().join("global-agents"),
            };
            let name = AgentName::parse(agent).unwrap();
            let package_dir = paths.project_agents_dir().join(name.as_str());
            std::fs::create_dir_all(&package_dir).unwrap();
            let toml_path = package_dir.join("agent.toml");
            std::fs::write(&toml_path, content).unwrap();
            Self {
                _dir: dir,
                paths,
                uid,
                name,
                toml_path,
                package_dir,
            }
        }

        fn load(&self) -> Result<LoadedAgentProfile, ProfileError> {
            load_agent_profile(&self.name, &self.paths, self.uid)
        }
    }

    #[test]
    fn regular_file_read_accepts_limit_and_rejects_one_extra_byte() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bounded.txt");
        std::fs::write(&path, "12345678").unwrap();
        assert_eq!(read_regular_file(&path, 8).unwrap(), "12345678");
        std::fs::write(&path, "123456789").unwrap();
        let error = read_regular_file(&path, 8).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(error.to_string(), "file exceeds 8 bytes");
    }

    #[test]
    fn fifo_is_rejected_before_opening() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fifo");
        rustix::fs::mkfifoat(rustix::fs::CWD, &path, rustix::fs::Mode::RWXU).unwrap();
        let (send, recv) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let _ = send.send(read_regular_file(&path, 8));
        });
        // 超时即失败；不 join 阻塞的线程，测试进程不会等待它退出。
        let result = recv
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("FIFO 必须在 open 前被拒绝");
        worker.join().unwrap();
        let error = result.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(error.to_string(), "not a regular file");
    }

    #[test]
    fn inline_and_file_prompts_are_limited_by_utf8_bytes() {
        for external in [false, true] {
            for (prompt, accepted) in [
                (format!("{}ab", "中".repeat(699_050)), true),
                ("中".repeat(699_051), false),
            ] {
                let field = if external {
                    "system_path='prompt.md'".into()
                } else {
                    format!("system='{prompt}'")
                };
                let fixture = ProjectProfileFixture::new(
                    "test",
                    &format!(
                        "schema_version=1\n[agent]\nname='test'\nmodel='local/test'\n[prompt]\n{field}\n"
                    ),
                );
                if external {
                    std::fs::write(fixture.package_dir.join("prompt.md"), &prompt).unwrap();
                }
                let result = fixture.load();
                if accepted {
                    assert!(
                        result.unwrap().system_prompt == prompt,
                        "external={external}"
                    );
                } else {
                    let error = result.unwrap_err().to_string();
                    assert!(error.contains("2097152"), "external={external}: {error}");
                    assert!(
                        error.contains(fixture.toml_path.to_str().unwrap()),
                        "{error}"
                    );
                }
            }
        }
    }

    #[test]
    fn load_system_path_prompt() {
        let fixture = ProjectProfileFixture::new(
            "test",
            r#"
schema_version = 1

[agent]
name = "test"
model = "anthropic/claude-sonnet-4-6"

[prompt]
system_path = "prompt.md"
"#,
        );
        std::fs::write(
            fixture.package_dir.join("prompt.md"),
            "Be concise and helpful.",
        )
        .unwrap();

        let loaded = fixture.load().unwrap();
        assert_eq!(loaded.system_prompt, "Be concise and helpful.");
    }

    #[test]
    fn rejects_invalid_prompt_configuration() {
        for (name, prompt_fields, expected_error_contains) in [
            (
                "reject_both_system_and_system_path",
                "system = \"inline\"\nsystem_path = \"prompt.md\"",
                "both",
            ),
            ("reject_neither_system_nor_system_path", "", "neither"),
            (
                "reject_missing_prompt_file",
                "system_path = \"nonexistent.md\"",
                "nonexistent",
            ),
            (
                "reject_absolute_system_path",
                "system_path = \"/etc/passwd\"",
                "absolute",
            ),
            (
                "reject_dot_dot_traversal",
                "system_path = \"../secret/prompt.md\"",
                "..",
            ),
        ] {
            let fixture = ProjectProfileFixture::new(
                "test",
                &format!(
                    r#"
schema_version = 1

[agent]
name = "test"
model = "anthropic/claude-sonnet-4-6"

[prompt]
{prompt_fields}
"#
                ),
            );
            let err = fixture.load().expect_err(name);
            assert!(
                err.to_string().contains(expected_error_contains),
                "{name}: got: {err}"
            );
        }
    }

    #[test]
    fn reject_path_escape_via_symlink() {
        let outside = tempfile::tempdir().unwrap();
        let secret = outside.path().join("secret.md");
        std::fs::write(&secret, "secret").unwrap();

        let fixture = ProjectProfileFixture::new(
            "test",
            r#"
schema_version = 1

[agent]
name = "test"
model = "anthropic/claude-sonnet-4-6"

[prompt]
system_path = "escape/secret.md"
"#,
        );
        std::os::unix::fs::symlink(&outside, fixture.package_dir.join("escape")).unwrap();

        let err = fixture.load().unwrap_err();
        assert!(err.to_string().contains("escapes"), "got: {err}");
    }

    #[test]
    fn reject_prompt_path_that_is_directory() {
        let fixture = ProjectProfileFixture::new(
            "test",
            r#"
schema_version = 1

[agent]
name = "test"
model = "anthropic/claude-sonnet-4-6"

[prompt]
system_path = "subdir"
"#,
        );
        std::fs::create_dir(fixture.package_dir.join("subdir")).unwrap();

        let err = fixture.load().unwrap_err();
        assert!(err.to_string().contains("not a regular file"), "got: {err}");
    }

    /// ADR 0009 决策 6：`[lancet]` 与 `[tools]`（含只写子表、点号键、内联表与大小写变体）
    /// 在加载预扫描中统一以 `ForbiddenSection` 拒绝，并指明该节已移除、需从 `agent.toml` 删除。
    #[test]
    fn removed_lancet_and_tools_sections_are_rejected_with_removal_diagnostic() {
        let lancet_reason = "`[lancet]` has been removed; delete this section from agent.toml";
        let tools_reason = "`[tools]` has been removed; delete this section from agent.toml";

        let cases = [
            ("top_level_lancet", "[lancet]", "lancet", lancet_reason),
            (
                "top_level_tools",
                "[tools]\nenabled = [\"observe__workspace\"]",
                "tools",
                tools_reason,
            ),
            (
                "sub_table_only_lancet_workspace",
                "[lancet.workspace]\ndenied_write_patterns = [\".git/**\"]",
                "lancet",
                lancet_reason,
            ),
            (
                "dotted_lancet_key",
                "lancet.workspace.denied_write_patterns = [\".git/**\"]",
                "lancet",
                lancet_reason,
            ),
            (
                "inline_lancet_table",
                "lancet = { workspace = { denied_write_patterns = [] } }",
                "lancet",
                lancet_reason,
            ),
            (
                "dotted_tools_key",
                "tools.enabled = [\"observe__workspace\"]",
                "tools",
                tools_reason,
            ),
            (
                "inline_tools_table",
                "tools = { enabled = [\"observe__workspace\"] }",
                "tools",
                tools_reason,
            ),
            (
                "case_variant_tools",
                "[Tools]\nenabled = [\"observe__workspace\"]",
                "Tools",
                tools_reason,
            ),
            ("case_variant_lancet", "[Lancet]", "Lancet", lancet_reason),
        ];

        for (case_name, extra_toml, expected_section, expected_reason) in cases {
            let content = format!(
                "schema_version = 1\n{extra_toml}\n\n[agent]\nname = \"t\"\nmodel = \"anthropic/claude-sonnet-4-6\"\n\n[prompt]\nsystem = \"hi\"\n"
            );
            let fixture = ProjectProfileFixture::new("t", &content);
            let err = fixture.load().expect_err(case_name);
            let display = err.to_string();
            let ProfileError::ForbiddenSection {
                path,
                section,
                reason,
            } = err
            else {
                panic!("{case_name}: 期望 ForbiddenSection，实际得到 {err:?}");
            };
            assert_eq!(path, fixture.toml_path, "{case_name}");
            assert_eq!(section, expected_section, "{case_name}");
            assert_eq!(reason, expected_reason, "{case_name}");
            assert!(
                display.contains("Allowed sections: schema_version, [agent], [prompt], [llm]"),
                "{case_name}: 允许节列表不应再含 [tools]，实际: {display}"
            );
        }
    }

    #[test]
    fn schema_version_must_be_1() {
        for version in [0, 2, 3, 99] {
            let fixture = ProjectProfileFixture::new(
                "test",
                &format!(
                    r#"
schema_version = {}

[agent]
name = "test"
model = "anthropic/claude-sonnet-4-6"

[prompt]
system = "hi"
"#,
                    version
                ),
            );
            let err = fixture.load().unwrap_err();
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
        // subagents, micro_agents, memory, workspace (retired Upper section)
        let forbidden = [
            "capabilities",
            "handoff",
            "orchestrator",
            "plugins",
            "pipeline",
            "subagents",
            "micro_agents",
            "memory",
            "workspace",
        ];
        for section in forbidden {
            let fixture = ProjectProfileFixture::new(
                "t",
                &format!(
                    r#"
schema_version = 1

[agent]
name = "t"
model = "anthropic/claude-sonnet-4-6"

[prompt]
system = "hi"

[{section}]
x = 1
"#
                ),
            );
            let err = fixture.load().unwrap_err();
            assert!(
                err.to_string().contains(section),
                "section '{section}': got: {err}"
            );
        }
    }

    #[test]
    fn invalid_fields_report_path_section_field_and_raw_value() {
        let overlong_name = "a".repeat(129);
        let cases = [
            (
                "schema_version = 1\n[agent]\nname = 'test'\nmodel = 'deepseek-flash'\n[prompt]\nsystem = 'hi'\n"
                    .to_owned(),
                "agent",
                "model",
                "deepseek-flash".to_owned(),
            ),
            (
                format!(
                    "schema_version = 1\n[agent]\nname = '{overlong_name}'\nmodel = 'local/test'\n[prompt]\nsystem = 'hi'\n"
                ),
                "agent",
                "name",
                overlong_name,
            ),
            (
                "schema_version = 1\n[agent]\nname = 'test'\nmodel = 'local/test'\n[prompt]\nsystem = 'hi'\n[llm]\ntimeout_secs = 0\n"
                    .to_owned(),
                "llm",
                "timeout_secs",
                "0".to_owned(),
            ),
            (
                "schema_version = 1\n[agent]\nname = 'test'\nmodel = 'local/test'\n[prompt]\nsystem = 'hi'\n[llm.retry]\nmax_retries = 11\n"
                    .to_owned(),
                "llm.retry",
                "max_retries",
                "11".to_owned(),
            ),
        ];

        for (toml_source, expected_section, expected_field, expected_value) in cases {
            let fixture = ProjectProfileFixture::new("test", &toml_source);
            let err = fixture.load().unwrap_err();
            let ProfileError::InvalidField {
                path,
                section,
                field,
                value,
                ..
            } = err
            else {
                panic!(
                    "expected InvalidField for {expected_section}.{expected_field}, got {err:?}"
                );
            };
            assert_eq!(path, fixture.toml_path);
            assert_eq!(section, expected_section);
            assert_eq!(field, expected_field);
            assert_eq!(value.as_ref(), expected_value);
        }
    }

    fn private_paths(root: &Path) -> (PathBuf, PathBuf, u32) {
        let uid = std::fs::symlink_metadata(root).unwrap().uid();
        let configuration_dir = root.join("home/.lattice");
        std::fs::create_dir_all(&configuration_dir).unwrap();
        std::fs::set_permissions(&configuration_dir, std::fs::Permissions::from_mode(0o700))
            .unwrap();
        let project = root.join("project");
        std::fs::create_dir_all(&project).unwrap();
        (configuration_dir, project, uid)
    }

    /// 写一个从 prompt.md 读取系统提示的全局 agent。包目录用 `dir_mode`，
    /// agent.toml 与 prompt.md 分别用各自的 mode。
    fn write_global_agent(agents: &Path, dir_mode: u32, file_mode: u32, prompt_mode: u32) {
        let package = agents.join("test");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::set_permissions(&package, std::fs::Permissions::from_mode(dir_mode)).unwrap();
        let prompt = package.join("prompt.md");
        std::fs::write(&prompt, "hi").unwrap();
        std::fs::set_permissions(&prompt, std::fs::Permissions::from_mode(prompt_mode)).unwrap();
        let profile = package.join("agent.toml");
        std::fs::write(
            &profile,
            "schema_version=1\n[agent]\nname='test'\nmodel='local/test'\n[prompt]\nsystem_path='prompt.md'\n",
        )
        .unwrap();
        std::fs::set_permissions(&profile, std::fs::Permissions::from_mode(file_mode)).unwrap();
    }

    fn global_agents_dir(configuration_dir: &Path, mode: u32) -> PathBuf {
        let agents = configuration_dir.join("agents");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::set_permissions(&agents, std::fs::Permissions::from_mode(mode)).unwrap();
        agents
    }

    // 这些测试只比较 symlink_metadata 的 uid 与 mode 位，不依赖 access(2)，
    // 因此以 root 运行时结论相同：文件属于 euid 0，mode 就是 set_permissions 设的值。
    // 不同属主由 global_profile_rejects_a_different_owner 以 euid+1 直接构造。

    #[test]
    fn global_profile_with_default_umask_permissions_loads() {
        let root = tempfile::tempdir().unwrap();
        let (configuration_dir, project, uid) = private_paths(root.path());
        let agents = global_agents_dir(&configuration_dir, 0o755);
        write_global_agent(&agents, 0o755, 0o644, 0o644);
        let paths = DiscoveryPaths {
            project_root: project,
            global_agents_dir: agents,
        };
        let test_agent = AgentName::parse("test").unwrap();
        let result = load_agent_profile(&test_agent, &paths, uid);
        assert!(
            result.is_ok(),
            "umask 022 layout (dirs 0755, files 0644) must load, got {:?}",
            result.err().map(|error| error.to_string())
        );
    }

    #[test]
    fn global_agents_directories_reject_group_or_other_write() {
        let test_agent = AgentName::parse("test").unwrap();
        for (agents_mode, package_mode, rejected) in [
            (0o775, 0o755, "/agents: "),
            (0o757, 0o755, "/agents: "),
            (0o755, 0o775, "/agents/test: "),
            (0o755, 0o757, "/agents/test: "),
        ] {
            let root = tempfile::tempdir().unwrap();
            let (configuration_dir, project, uid) = private_paths(root.path());
            let agents = global_agents_dir(&configuration_dir, agents_mode);
            write_global_agent(&agents, package_mode, 0o644, 0o644);
            let paths = DiscoveryPaths {
                project_root: project,
                global_agents_dir: agents,
            };
            let err = load_agent_profile(&test_agent, &paths, uid).unwrap_err();
            assert!(matches!(err, ProfileError::Protection { .. }), "{err:?}");
            let message = err.to_string();
            assert!(
                message.contains("global agents directory must not be writable by group or other")
                    && message.contains(rejected),
                "agents {agents_mode:o} / package {package_mode:o} must reject '{rejected}', got {message}"
            );
        }
    }

    #[test]
    fn global_profile_files_reject_group_or_other_write() {
        let test_agent = AgentName::parse("test").unwrap();
        for (file_mode, prompt_mode, rejected) in [
            (0o664, 0o644, "/agent.toml: "),
            (0o646, 0o644, "/agent.toml: "),
            (0o644, 0o664, "/prompt.md: "),
            (0o644, 0o646, "/prompt.md: "),
        ] {
            let root = tempfile::tempdir().unwrap();
            let (configuration_dir, project, uid) = private_paths(root.path());
            let agents = global_agents_dir(&configuration_dir, 0o755);
            write_global_agent(&agents, 0o755, file_mode, prompt_mode);
            let paths = DiscoveryPaths {
                project_root: project,
                global_agents_dir: agents,
            };
            let err = load_agent_profile(&test_agent, &paths, uid).unwrap_err();
            assert!(matches!(err, ProfileError::Protection { .. }), "{err:?}");
            let message = err.to_string();
            assert!(
                message.contains("profile file must not be writable by group or other")
                    && message.contains(rejected),
                "agent.toml {file_mode:o} / prompt.md {prompt_mode:o} must reject '{rejected}', got {message}"
            );
        }
    }

    #[test]
    fn global_agents_directory_rejects_a_symbolic_link() {
        let root = tempfile::tempdir().unwrap();
        let (configuration_dir, project, uid) = private_paths(root.path());
        let real = root.path().join("real-agents");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o700)).unwrap();
        write_global_agent(&real, 0o755, 0o644, 0o644);
        let agents = configuration_dir.join("agents");
        std::os::unix::fs::symlink(&real, &agents).unwrap();
        let paths = DiscoveryPaths {
            project_root: project,
            global_agents_dir: agents,
        };
        let test_agent = AgentName::parse("test").unwrap();
        let err = load_agent_profile(&test_agent, &paths, uid).unwrap_err();
        assert!(matches!(err, ProfileError::Protection { .. }), "{err:?}");
        let message = err.to_string();
        assert!(
            message.contains("symbolic link"),
            "a symlinked agents directory must be rejected, got {message}"
        );
    }

    #[test]
    fn global_profile_file_rejects_a_symbolic_link() {
        let root = tempfile::tempdir().unwrap();
        let (configuration_dir, project, uid) = private_paths(root.path());
        let agents = global_agents_dir(&configuration_dir, 0o755);
        write_global_agent(&agents, 0o755, 0o644, 0o644);
        let profile = agents.join("test/agent.toml");
        let real = agents.join("test/real.toml");
        std::fs::rename(&profile, &real).unwrap();
        std::os::unix::fs::symlink(&real, &profile).unwrap();
        let paths = DiscoveryPaths {
            project_root: project,
            global_agents_dir: agents,
        };
        let test_agent = AgentName::parse("test").unwrap();
        let err = load_agent_profile(&test_agent, &paths, uid).unwrap_err();
        assert!(matches!(err, ProfileError::Protection { .. }), "{err:?}");
        let message = err.to_string();
        assert!(
            message.contains("/agent.toml: profile file must not be a symbolic link"),
            "a symlinked agent.toml must be rejected, got {message}"
        );
    }

    #[test]
    fn project_agents_are_exempt_from_global_permission_rules() {
        // 项目 profile 由仓库权限管理：配置目录在项目外、或配置目录就是项目 .lattice
        // （两条路径重合）时，group/other 可写的项目 agent 都照常装载。
        let root = tempfile::tempdir().unwrap();
        let (outside_configuration, project, uid) = private_paths(root.path());
        let project_agents = project.join(".lattice/agents");
        std::fs::create_dir_all(&project_agents).unwrap();
        std::fs::set_permissions(&project_agents, std::fs::Permissions::from_mode(0o777)).unwrap();
        write_global_agent(&project_agents, 0o777, 0o666, 0o666);
        let test_agent = AgentName::parse("test").unwrap();
        for (case, configuration_dir) in [
            ("configuration outside project", outside_configuration),
            (
                "configuration is project .lattice",
                project.join(".lattice"),
            ),
        ] {
            let paths = DiscoveryPaths {
                project_root: project.clone(),
                global_agents_dir: configuration_dir.join("agents"),
            };
            let result = load_agent_profile(&test_agent, &paths, uid);
            assert!(
                result.is_ok(),
                "{case}: writable project agent must load, got {:?}",
                result.err().map(|error| error.to_string())
            );
        }
    }

    // 非 root 无法创建属于他人的文件；期望属主作为参数传入同一加载入口，
    // 以不等于当前 euid 的值调用，root 与非 root 结论一致。
    #[test]
    fn global_profile_rejects_a_different_owner() {
        let root = tempfile::tempdir().unwrap();
        let (configuration_dir, project, euid) = private_paths(root.path());
        let agents = global_agents_dir(&configuration_dir, 0o755);
        write_global_agent(&agents, 0o755, 0o644, 0o644);
        let paths = DiscoveryPaths {
            project_root: project,
            global_agents_dir: agents,
        };
        let test_agent = AgentName::parse("test").unwrap();
        assert!(
            load_agent_profile(&test_agent, &paths, euid).is_ok(),
            "属主等于当前 euid 时应通过"
        );
        let other = euid.wrapping_add(1);
        let err = load_agent_profile(&test_agent, &paths, other).unwrap_err();
        assert!(matches!(err, ProfileError::Protection { .. }), "{err:?}");
        let message = err.to_string();
        assert!(
            message.contains("/agents: global agents directory must belong to the current user"),
            "期望属主 {other} ≠ 当前 euid {euid} 时必须拒绝，实际 {message}"
        );
    }
}

// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::local::{LocalError, LocalErrorKind};

const SENTINEL: &str = "sk-profile-secret-a81e9b743f";

fn local(reason: ConfigRejection) -> LocalError {
    crate::ApiError::Configuration(reason).into()
}

// 此处保护类别放行边界，特别是任意原文伪装为安全字段的情况；
// 真实命令入口由 E2E 覆盖。
#[test]
fn profile_rejections_expose_only_locations_and_typed_parameters() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agent.toml");
    let prefix = format!("schema_version=1\n[agent]\nname='test'\nmodel='{SENTINEL}'\n");
    let cases = [
        (
            format!("{prefix}[prompt]\nsystem='hi'\n[llm.retry]\nmax_retries=11"),
            vec!["[llm.retry].max_retries", "11", "0..=10"],
        ),
        (
            format!("{prefix}[prompt]\nsystem='hi'\n[llm.retry]\ninitial_delay_ms=60001"),
            vec!["initial_delay_ms", "60001", "0..=60000"],
        ),
        (
            format!("{prefix}[prompt]\nsystem='hi'\n[llm.retry]\nmax_delay_ms=60001"),
            vec!["max_delay_ms", "60001", "0..=60000"],
        ),
        (
            format!(
                "{prefix}[prompt]\nsystem='hi'\n[llm.retry]\ninitial_delay_ms=9\nmax_delay_ms=8"
            ),
            vec!["[llm.retry].initial_delay_ms (9) must be <= [llm.retry].max_delay_ms (8)"],
        ),
        (
            format!("{prefix}[prompt]\nsystem='hi'\n[llm]\ntimeout_secs=0"),
            vec!["timeout_secs", "0", "1..=3600"],
        ),
        (
            format!("{prefix}[prompt]\nsystem='hi'\n[llm]\ntimeout_secs=3601"),
            vec!["timeout_secs", "3601", "1..=3600"],
        ),
        (
            format!(
                "{}[prompt]\nsystem='hi'",
                prefix.replace("schema_version=1", "schema_version=2")
            ),
            vec!["schema_version 2", "only schema_version = 1"],
        ),
        (
            format!("{prefix}[prompt]\nsystem='hi'\nsystem_path='{SENTINEL}'"),
            vec!["both", "[prompt]"],
        ),
        (format!("{prefix}[prompt]\n"), vec!["neither", "[prompt]"]),
        (
            format!("{prefix}[prompt]\nsystem_path='/{SENTINEL}'"),
            vec!["system_path", "must be relative"],
        ),
        (
            format!("{prefix}[prompt]\nsystem_path='../{SENTINEL}'"),
            vec!["system_path", "traversal"],
        ),
        (
            format!("{prefix}[prompt]\nsystem_path='{SENTINEL}'"),
            vec!["system_path", "could not be resolved"],
        ),
        (
            format!("{prefix}[prompt]\nsystem='hi'\n[workspace]\nsecret='{SENTINEL}'"),
            vec!["forbidden", "section"],
        ),
        (
            format!(
                "{prefix}[prompt]\nsystem='hi'\n[lancet.observation]\nallowed_roots=['{SENTINEL}']"
            ),
            vec![
                "retired field",
                "[lancet.observation].allowed_roots",
                "[lancet.project_root].root",
            ],
        ),
    ];
    for (source, expected) in cases {
        std::fs::write(&path, source).unwrap();
        let error = lattice_profile::load_agent_profile(&path).unwrap_err();
        let result = LocalError::from(ConfigRejection::profile(error, "test"));
        assert_eq!(result.kind, LocalErrorKind::BadRequest);
        assert!(result.message.contains(path.to_str().unwrap()));
        assert!(!result.message.contains(SENTINEL));
        for fragment in expected {
            assert!(
                result.message.contains(fragment),
                "{fragment}: {}",
                result.message
            );
        }
    }
}

#[test]
fn toml_locations_use_original_bytes_without_parser_text() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agent.toml");
    for source in [
        format!("# 中文\r\nschema_version = @{SENTINEL}\r\n"),
        format!("# 中文\nschema_version = '{SENTINEL}'\n"),
    ] {
        std::fs::write(&path, source).unwrap();
        let error = lattice_profile::load_agent_profile(&path).unwrap_err();
        let result = LocalError::from(ConfigRejection::profile(error, "test"));
        assert!(result.message.contains("invalid profile TOML"));
        assert!(result.message.contains("line 2"));
        assert!(result.message.contains(path.to_str().unwrap()));
        assert!(!result.message.contains(SENTINEL));
        assert!(!result.message.contains("schema_version ="));
    }
    let result = local(ConfigRejection(Reason::Toml { path, line: None }));
    assert!(result.message.contains("agent.toml"));
    assert!(!result.message.contains(": line "));
}

#[test]
fn profile_payloads_and_legacy_bad_requests_remain_private() {
    let path = PathBuf::from("/profiles/agent.toml");
    for error in [
        ProfileError::InvalidField {
            path: path.clone(),
            section: "llm.retry".into(),
            field: "max_retries".into(),
            value: SENTINEL.into(),
            reason: SENTINEL.into(),
            expected: SENTINEL.into(),
        },
        ProfileError::Io {
            path: PathBuf::from(SENTINEL),
            source: std::io::Error::other(SENTINEL),
        },
    ] {
        let result = LocalError::from(ConfigRejection::profile(error, "test"));
        assert_eq!(result.message, "request or frozen configuration is invalid");
    }
    let result = LocalError::from(crate::ApiError::BadRequest(SENTINEL.into()));
    assert_eq!(result.message, "request or frozen configuration is invalid");
    for (error, expected) in [
        (
            ProfileError::PathEscape {
                profile_path: path.clone(),
                package_dir: SENTINEL.into(),
                given: SENTINEL.into(),
                resolved: SENTINEL.into(),
            },
            "escapes package directory",
        ),
        (
            ProfileError::SystemPathNotAFile {
                profile_path: path.clone(),
                given: SENTINEL.into(),
                resolved: SENTINEL.into(),
            },
            "not a file",
        ),
        (
            ProfileError::ForbiddenSection {
                path: path.clone(),
                section: SENTINEL.into(),
                reason: SENTINEL.into(),
            },
            "forbidden",
        ),
        (
            ProfileError::ForbiddenField {
                path: path.clone(),
                section: SENTINEL.into(),
                field: SENTINEL.into(),
                reason: SENTINEL.into(),
            },
            "retired field",
        ),
        (
            ProfileError::MissingDefault {
                dir: "/profiles/default".into(),
                template: SENTINEL.into(),
            },
            "create /profiles/default/agent.toml",
        ),
        (
            ProfileError::ProfileNotFound {
                name: SENTINEL.into(),
                project_path: path.clone(),
                global_path: "/global/agent.toml".into(),
            },
            "agent profile 'selected' not found",
        ),
    ] {
        let result = LocalError::from(ConfigRejection::profile(error, "selected"));
        assert!(result.message.contains(expected));
        assert!(!result.message.contains(SENTINEL));
    }
}

#[test]
fn named_profile_validation_is_shared_and_does_not_echo_declared_name() {
    let dir = tempfile::tempdir().unwrap();
    let profile = dir.path().join(".lattice/agents/selected/agent.toml");
    std::fs::create_dir_all(profile.parent().unwrap()).unwrap();
    std::fs::write(
        &profile,
        format!(
            "schema_version=1\n[agent]\nname='{SENTINEL}'\nmodel='sonnet'\n[prompt]\nsystem='hi'\n"
        ),
    )
    .unwrap();
    // 空全局根是已有支持的输入；临时项目不作为受限的全局 agents 目录。
    let error =
        crate::resolve_named_agent_profile("selected", dir.path(), std::path::Path::new(""))
            .unwrap_err();
    let result = LocalError::from(error);
    assert!(result.message.contains("[agent].name"));
    assert!(result.message.contains("selected"));
    assert!(result.message.contains(profile.to_str().unwrap()));
    assert!(!result.message.contains(SENTINEL));

    let result = LocalError::from(
        crate::resolve_named_agent_profile("missing", dir.path(), std::path::Path::new(""))
            .unwrap_err(),
    );
    assert!(result.message.contains("agent profile 'missing' not found"));
    assert!(result.message.contains(dir.path().to_str().unwrap()));
    assert!(result.message.contains("global path missing/agent.toml"));
}

#[test]
fn agents_directory_rules_and_display_bounds() {
    for (path, expected) in [
        ("/etc/lattice", "system path (/etc)"),
        ("/tmp/lattice", "inside /tmp"),
        ("/safe/../agents", "path traversal"),
    ] {
        let result =
            LocalError::from(crate::validate_agents_dir(std::path::Path::new(path)).unwrap_err());
        assert_eq!(result.kind, LocalErrorKind::BadRequest);
        assert!(result.message.contains(path));
        assert!(result.message.contains(expected));
        assert!(result.message.contains("LATTICE_AGENTS_DIR"));
    }
    let result = local(ConfigRejection(Reason::AgentName {
        path: "/profiles/\u{1b}\n\t agent.toml".into(),
        selected: "chosen\r\n  agent".into(),
    }));
    assert!(result.message.contains("/profiles/ agent.toml"));
    assert!(result.message.contains("chosen agent"));
    assert!(!result.message.chars().any(char::is_control));
    assert!(!result.message.contains("  "));
    let result = local(ConfigRejection(Reason::AgentName {
        path: PathBuf::from("界".repeat(1500)),
        selected: "selected".into(),
    }));
    assert_eq!(result.message.chars().count(), MAX_REASON_CHARS);
    assert!(result.message.ends_with('…'));
}

#[test]
fn llm_allowlist_discards_untrusted_payloads() {
    use lattice_llm::{BaseUrlRule, LlmError};
    for (error, expected) in [
        (
            LlmError::UnknownModel(SENTINEL.into()),
            "[agent].model: unknown model alias; use `lattice models`",
        ),
        (
            LlmError::MissingApiKey {
                env_var: "TEST_API_KEY".into(),
            },
            "'TEST_API_KEY' is not set",
        ),
        (
            LlmError::EmptyApiKey {
                env_var: "TEST_API_KEY".into(),
            },
            "'TEST_API_KEY' is empty",
        ),
        (
            LlmError::BaseUrl(BaseUrlRule::Empty),
            "base_url must not be empty",
        ),
        (
            LlmError::BaseUrl(BaseUrlRule::RemoteHttp),
            "use HTTPS for remote hosts",
        ),
        (LlmError::BaseUrl(BaseUrlRule::Userinfo), "userinfo"),
        (
            LlmError::BaseUrl(BaseUrlRule::QueryOrFragment),
            "query or fragment",
        ),
    ] {
        let result = local(ConfigRejection::llm(error).unwrap());
        assert_eq!(result.kind, LocalErrorKind::BadRequest);
        assert!(result.message.contains(expected));
        assert!(!result.message.contains(SENTINEL));
    }
    for error in [
        LlmError::Config {
            message: SENTINEL.into(),
        },
        LlmError::Credential {
            message: SENTINEL.into(),
        },
        LlmError::Dns {
            message: SENTINEL.into(),
        },
        LlmError::Transport {
            message: SENTINEL.into(),
            retryable: false,
        },
        LlmError::Timeout {
            message: SENTINEL.into(),
        },
        LlmError::ProviderHttp {
            status_code: 400,
            message: SENTINEL.into(),
            retryable: false,
            retry_after: None,
        },
        LlmError::StreamRead {
            message: SENTINEL.into(),
        },
        LlmError::ProtocolLimit {
            message: SENTINEL.into(),
        },
        LlmError::Projection(
            lattice_llm::provider_projection::ProjectionError::InvalidStep { detail: SENTINEL },
        ),
        LlmError::Other(SENTINEL.into()),
    ] {
        assert!(ConfigRejection::llm(error).is_none());
    }
}

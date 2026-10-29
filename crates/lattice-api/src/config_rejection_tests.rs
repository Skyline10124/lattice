// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::local::{LocalError, LocalErrorKind};
use lattice_profile::ProfileError;
use std::path::PathBuf;

const SENTINEL: &str = "sk-profile-secret-a81e9b743f";

fn local(reason: ConfigRejection) -> LocalError {
    crate::ApiError::Configuration(reason).into()
}

fn profile_local(error: ProfileError) -> LocalError {
    local(ConfigRejection::new(
        &crate::profile::profile_error_message(&error),
        None,
    ))
}

// 保护原始说明经脱敏后仍可读；真实命令入口由 E2E 覆盖。
#[test]
fn profile_rejections_preserve_diagnostics_and_redact_secrets() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agent.toml");
    let prefix = format!("schema_version=1\n[agent]\nname='test'\nmodel='{SENTINEL}'\n");
    let cases = [
        (
            format!("{prefix}[prompt]\nsystem='hi'\n[llm.retry]\nmax_retries=11"),
            vec!["max_retries", "11", "max_retries must be <= 10"],
        ),
        (
            format!("{prefix}[prompt]\nsystem='hi'\n[llm.retry]\ninitial_delay_ms=60001"),
            vec!["initial_delay_ms", "60001", "must be <= 60000"],
        ),
        (
            format!("{prefix}[prompt]\nsystem='hi'\n[llm.retry]\nmax_delay_ms=60001"),
            vec!["max_delay_ms", "60001", "must be <= 60000"],
        ),
        (
            format!(
                "{prefix}[prompt]\nsystem='hi'\n[llm.retry]\ninitial_delay_ms=9\nmax_delay_ms=8"
            ),
            vec![
                "initial_delay_ms",
                "9",
                "initial delay exceeds max delay",
                "initial_delay_ms must be <= max_delay_ms",
            ],
        ),
        (
            format!("{prefix}[prompt]\nsystem='hi'\n[llm]\ntimeout_secs=0"),
            vec!["timeout_secs", "0", "timeout_secs must be > 0"],
        ),
        (
            format!("{prefix}[prompt]\nsystem='hi'\n[llm]\ntimeout_secs=3601"),
            vec!["timeout_secs", "3601", "timeout_secs must be <= 3600"],
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
            vec!["system_path", "file not found", "[REDACTED]"],
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
                "forbidden field `allowed_roots` in [lancet.observation]",
                "retired:",
                "[lancet.project_root].root",
            ],
        ),
    ];
    for (source, expected) in cases {
        std::fs::write(&path, source).unwrap();
        let error = lattice_profile::load_agent_profile(&path).unwrap_err();
        let result = profile_local(error);
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
fn toml_locations_keep_parser_message_without_source_excerpt() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agent.toml");
    for source in [
        format!("# 中文\r\nschema_version = @{SENTINEL}\r\n"),
        format!("# 中文\nschema_version = '{SENTINEL}'\n"),
    ] {
        std::fs::write(&path, source).unwrap();
        let error = lattice_profile::load_agent_profile(&path).unwrap_err();
        let ProfileError::Toml { source: parser, .. } = &error else {
            panic!("expected TOML error");
        };
        let diagnostic = crate::redact::redact(parser.message(), &[]);
        let result = profile_local(error);
        assert!(result.message.contains("TOML parse error"));
        assert!(result.message.ends_with(&diagnostic));
        assert!(result.message.contains("line 2"));
        assert!(result.message.contains(path.to_str().unwrap()));
        assert!(!result.message.contains(SENTINEL));
        assert!(!result.message.contains("schema_version ="));
    }
    let source = toml::from_str::<toml::Value>("invalid=@").unwrap_err();
    let result = profile_local(ProfileError::Toml {
        path,
        source,
        line: None,
    });
    assert!(result.message.contains("agent.toml"));
    assert!(!result.message.contains(": line "));
}

#[test]
fn profile_payloads_are_redacted_and_legacy_bad_requests_stay_fixed() {
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
        let result = profile_local(error);
        assert!(result.message.contains("[REDACTED]"));
        assert!(!result.message.contains(SENTINEL));
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
            "forbidden field",
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
            "agent profile '[REDACTED]' not found",
        ),
    ] {
        let result = profile_local(error);
        assert!(result.message.contains(expected));
        assert!(!result.message.contains(SENTINEL));
    }
}

#[test]
fn named_profile_validation_shows_redacted_declared_name() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("project");
    let profile = project.join(".lattice/agents/selected/agent.toml");
    std::fs::create_dir_all(profile.parent().unwrap()).unwrap();
    std::fs::write(
        &profile,
        format!(
            "schema_version=1\n[agent]\nname='{SENTINEL}'\nmodel='anthropic/claude-sonnet-4-6'\n[prompt]\nsystem='hi'\n"
        ),
    )
    .unwrap();
    let root = crate::LocalWorkspaceRoot::from_path(&project).unwrap();
    let configuration_dir = dir.path().join("home/.lattice");
    std::fs::create_dir_all(&configuration_dir).unwrap();
    std::fs::set_permissions(
        &configuration_dir,
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
    )
    .unwrap();
    let result = LocalError::from(
        crate::profile::load_for_turn("selected", &root, &configuration_dir).unwrap_err(),
    );
    assert!(result.message.contains("[agent].name"));
    assert!(result.message.contains("selected"));
    assert!(result.message.contains("declares '[REDACTED]'"));
    assert!(result.message.contains(profile.to_str().unwrap()));
    assert!(!result.message.contains(SENTINEL));

    let result = LocalError::from(
        crate::profile::load_for_turn("missing", &root, &configuration_dir).unwrap_err(),
    );
    assert!(result.message.contains("agent profile 'missing' not found"));
    assert!(
        result.message.contains(
            project
                .join(".lattice/agents/missing/agent.toml")
                .to_str()
                .unwrap()
        )
    );
    assert!(
        result.message.contains(
            configuration_dir
                .join("agents/missing/agent.toml")
                .to_str()
                .unwrap()
        )
    );
    let key = "short.fixture-key";
    lattice_llm::auth::AuthStore::new(configuration_dir.clone())
        .set("openai", key)
        .unwrap();
    std::fs::write(
        &profile,
        format!(
            "schema_version=1\n[agent]\nname='{key}'\nmodel='local/test'\n[prompt]\nsystem='hi'\n"
        ),
    )
    .unwrap();
    let error = crate::profile::load_for_turn("selected", &root, &configuration_dir).unwrap_err();
    assert!(!format!("{error:?}").contains(key));
    let result = LocalError::from(error);
    assert!(result.message.contains("declares '[REDACTED]'"));
    assert!(!result.message.contains(key));
}

#[test]
fn configuration_wrapper_preserves_processed_message() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::set_permissions(
        directory.path(),
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
    )
    .unwrap();
    let auth = lattice_llm::auth::AuthStore::new(directory.path().into());
    auth.set("openai", "known-secret-value").unwrap();
    let reason = ConfigRejection::new("ordinary sonet key=known-secret-value\nnext", Some(&auth));
    let expected = reason.to_string();
    assert_eq!(expected, "ordinary sonet key=[REDACTED] next");
    let error = local(reason);
    assert_eq!(error.kind, LocalErrorKind::BadRequest);
    assert_eq!(error.message, expected);
    auth.set("openai", "EMPTY").unwrap();
    assert_eq!(
        ConfigRejection::new("unknown enabled tools [EMPTY]", Some(&auth)).to_string(),
        "unknown enabled tools [EMPTY]"
    );
    for invalid_directory in [false, true] {
        if invalid_directory {
            std::fs::set_permissions(
                directory.path(),
                <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
            )
            .unwrap();
        } else {
            std::fs::write(directory.path().join("auth.json"), "invalid auth").unwrap();
        }
        let reason =
            ConfigRejection::new(&format!("agent profile missing: {SENTINEL}"), Some(&auth))
                .to_string();
        assert_eq!(reason, "agent profile missing: [REDACTED]");
    }
}

#[test]
fn llm_display_is_preserved_after_redaction() {
    use lattice_llm::LlmError;
    for (error, expected) in [
        (
            LlmError::Config {
                message: "provider sonet is not registered".into(),
            },
            "configuration error: provider sonet is not registered",
        ),
        (
            LlmError::Config {
                message: format!("provider {SENTINEL} is not registered"),
            },
            "configuration error: provider [REDACTED] is not registered",
        ),
        (
            LlmError::Credential {
                message: "API key is empty".into(),
            },
            "credential error: API key is empty",
        ),
        (
            LlmError::Config {
                message: "invalid URL https://user:pass@example.test".into(),
            },
            "configuration error: invalid URL https://[REDACTED]@example.test",
        ),
        (
            LlmError::Dns {
                message: "cannot resolve typo.invalid".into(),
            },
            "DNS error: cannot resolve typo.invalid",
        ),
    ] {
        let result = local(ConfigRejection::new(&error.to_string(), None));
        assert_eq!(result.kind, LocalErrorKind::BadRequest);
        assert_eq!(result.message, expected);
        assert!(!result.message.contains(SENTINEL));
    }
}

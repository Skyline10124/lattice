// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Deterministic child environment construction for supervised commands.

use std::collections::BTreeMap;
use std::path::Path;

use lancet_authority::policy::command::{build_allowed_params_env, CommandPolicy};
use lancet_protocol::CepError;

use super::SupervisorPlan;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct IsolatedEnv {
    pub(crate) values: BTreeMap<String, String>,
    pub(crate) env_policy: &'static str,
}

/// Build the command child environment for isolated command modes.
///
/// `host_env` is accepted only so tests can prove this path ignores host
/// authority. Production passes an empty map and no process-global env reader
/// is used here.
pub(crate) fn build_isolated_env(
    params_env: &BTreeMap<String, String>,
    policy: &CommandPolicy,
    plan: &SupervisorPlan,
    _host_env: &BTreeMap<String, String>,
) -> Result<IsolatedEnv, CepError> {
    let mut values = build_allowed_params_env(params_env, policy)?;

    if let Some(path) = deterministic_path(plan) {
        values.insert("PATH".to_string(), path);
    }
    if plan
        .tmpfs_paths
        .iter()
        .any(|path| path == Path::new("/tmp"))
    {
        values.insert("TMPDIR".to_string(), "/tmp".to_string());
    }

    Ok(IsolatedEnv {
        values,
        env_policy: "allowlist",
    })
}

fn deterministic_path(plan: &SupervisorPlan) -> Option<String> {
    let entries: Vec<_> = plan
        .executable_view_paths
        .iter()
        .filter_map(|path| {
            let path = path.to_string_lossy();
            (!path.is_empty()).then(|| path.into_owned())
        })
        .collect();
    (!entries.is_empty()).then(|| entries.join(":"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::supervisor::SupervisorPlan;
    use std::path::PathBuf;

    fn plan_with_tmp() -> SupervisorPlan {
        SupervisorPlan::env_fd_hygiene(
            PathBuf::from("/project"),
            PathBuf::from("/project"),
            vec![
                PathBuf::from("/project/.tools/bin"),
                PathBuf::from("/usr/bin"),
            ],
            vec![PathBuf::from("/tmp")],
        )
    }

    fn fake_host_env() -> BTreeMap<String, String> {
        [
            ("PATH", "/host/bin:/secret/bin"),
            ("HOME", "/home/user"),
            ("SSH_AUTH_SOCK", "/run/user/1000/ssh-agent.socket"),
            ("GIT_ASKPASS", "/tmp/askpass"),
            ("GIT_CONFIG_GLOBAL", "/home/user/.gitconfig"),
            ("AWS_ACCESS_KEY_ID", "AKIA..."),
            ("GITHUB_TOKEN", "ghp_secret"),
            ("OPENAI_API_KEY", "sk-secret"),
            ("ANTHROPIC_API_KEY", "sk-ant-secret"),
            ("LD_PRELOAD", "/tmp/inject.so"),
            ("LD_LIBRARY_PATH", "/tmp/lib"),
            ("PYTHONPATH", "/tmp/python"),
            ("NODE_OPTIONS", "--require /tmp/hook.js"),
            ("RUSTC_WRAPPER", "/tmp/wrapper"),
            ("CARGO_HOME", "/home/user/.cargo"),
            ("RUSTUP_HOME", "/home/user/.rustup"),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
    }

    #[test]
    fn isolated_env_ignores_host_authority_and_generates_path() {
        let policy = CommandPolicy::defaults();
        let env = build_isolated_env(
            &BTreeMap::new(),
            &policy,
            &plan_with_tmp(),
            &fake_host_env(),
        )
        .unwrap();

        assert_eq!(env.env_policy, "allowlist");
        assert_eq!(
            env.values.get("PATH").map(String::as_str),
            Some("/project/.tools/bin:/usr/bin")
        );
        assert_eq!(env.values.get("TMPDIR").map(String::as_str), Some("/tmp"));
        for key in [
            "HOME",
            "SSH_AUTH_SOCK",
            "GIT_ASKPASS",
            "GIT_CONFIG_GLOBAL",
            "AWS_ACCESS_KEY_ID",
            "GITHUB_TOKEN",
            "OPENAI_API_KEY",
            "ANTHROPIC_API_KEY",
            "LD_PRELOAD",
            "LD_LIBRARY_PATH",
            "PYTHONPATH",
            "NODE_OPTIONS",
            "RUSTC_WRAPPER",
            "CARGO_HOME",
            "RUSTUP_HOME",
        ] {
            assert!(
                !env.values.contains_key(key),
                "{key} leaked into isolated env"
            );
        }
    }

    #[test]
    fn isolated_env_keeps_only_validated_caller_entries() {
        let policy = CommandPolicy::defaults();
        let mut params = BTreeMap::new();
        params.insert("RUST_LOG".to_string(), "debug".to_string());

        let env = build_isolated_env(&params, &policy, &plan_with_tmp(), &fake_host_env()).unwrap();

        assert_eq!(
            env.values.get("RUST_LOG").map(String::as_str),
            Some("debug")
        );
    }

    #[test]
    fn isolated_env_rejects_unallowed_caller_entries_with_policy_error() {
        let policy = CommandPolicy::defaults();
        let mut params = BTreeMap::new();
        params.insert("GITHUB_TOKEN".to_string(), "secret".to_string());

        let err =
            build_isolated_env(&params, &policy, &plan_with_tmp(), &fake_host_env()).unwrap_err();

        assert_eq!(err.code, lancet_protocol::CepErrorCode::GuardDenied);
        assert!(err.message.contains("GITHUB_TOKEN"));
    }

    #[test]
    fn isolated_env_sets_tmpdir_only_when_tmp_is_supervisor_tmpfs() {
        let policy = CommandPolicy::defaults();
        let plan = SupervisorPlan::env_fd_hygiene(
            PathBuf::from("/project"),
            PathBuf::from("/project"),
            vec![PathBuf::from("/usr/bin")],
            vec![PathBuf::from("/scratch")],
        );

        let env = build_isolated_env(&BTreeMap::new(), &policy, &plan, &fake_host_env()).unwrap();

        assert!(!env.values.contains_key("TMPDIR"));
    }
}

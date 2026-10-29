// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 后台按项目与配置目录读取 profile，并冻结提示与有效策略。

use crate::session::{ApiError, Result};
use lattice_profile::{
    LancetNetworkSection, LancetObservationSection, LancetProjectRootSection,
    LancetWorkspacePolicySection,
};
use std::path::Path;

pub use lattice_profile::LoadedAgentProfile;

/// 配置目录来自后台启动环境；项目根已经与会话绑定核对。
pub(crate) fn load_for_turn(
    agent_name: &str,
    workspace: &crate::LocalWorkspaceRoot,
    configuration_dir: &Path,
) -> Result<LoadedAgentProfile> {
    let auth = lattice_llm::auth::AuthStore::new(configuration_dir.to_owned());
    let reject =
        |message: &str| ApiError::Configuration(crate::ConfigRejection::new(message, Some(&auth)));
    let paths = lattice_profile::DiscoveryPaths {
        project_root: workspace.canonical_path().to_owned(),
        global_agents_dir: configuration_dir.join("agents"),
    };
    let mut loaded = lattice_profile::resolve_agent_profile(
        lattice_profile::ProfileSource::Name(agent_name.to_owned()),
        &paths,
    )
    .map_err(|error| reject(&profile_error_message(&error)))?;
    if loaded.profile.agent.name != agent_name {
        return Err(reject(&format!(
            "[agent].name at {} declares '{}'; must match selected agent '{agent_name}'",
            loaded.path.display(),
            loaded.profile.agent.name,
        )));
    }
    if loaded.profile.agent.model.len()
        > lattice_conversation::MAX_PROVIDER_ID_BYTES + 1 + lattice_llm::MAX_MODEL_ID_BYTES
    {
        return Err(reject(&format!(
            "invalid or oversized profile at {}",
            loaded.path.display()
        )));
    }
    let policy =
        crate::turn::cep::load_policy_config(&loaded.profile.lancet, workspace.canonical_path())?;
    let lancet = &mut loaded.profile.lancet;
    // 将准入时的有效策略填回具体字段，执行时不再读取策略文件，保持这一轮的值不变。
    lancet.policy = None;
    lancet.project_root = Some(LancetProjectRootSection {
        root: Some(
            policy
                .project_root
                .to_str()
                .ok_or_else(invalid_profile)?
                .into(),
        ),
    });
    lancet.network = Some(LancetNetworkSection {
        allow_public_https: Some(policy.network.allow_public_https),
        allow_localhost: Some(policy.network.allow_localhost),
        allowed_hosts: policy.network.allowed_hosts,
        max_web_response_bytes: Some(policy.network.max_web_response_bytes),
    });
    lancet.workspace = Some(LancetWorkspacePolicySection {
        denied_write_patterns: policy.workspace.denied_write_patterns,
        included_ignored_read_patterns: policy.workspace.included_ignored_read_patterns,
        denied_read_patterns: policy.workspace.denied_read_patterns,
    });
    lancet.observation = Some(LancetObservationSection {
        max_single_read_bytes: Some(policy.observation.max_single_read_bytes),
        max_turn_observation_bytes: Some(policy.observation.max_turn_observation_bytes),
        max_grep_matches: Some(policy.observation.max_grep_matches),
        max_list_entries: Some(policy.observation.max_list_entries),
        max_find_paths: Some(policy.observation.max_find_paths),
    });
    Ok(loaded)
}

fn invalid_profile() -> ApiError {
    ApiError::BadRequest("invalid or oversized frozen configuration".into())
}

// TOML 的 Display 会附上多行源码；保留解析器说明及位置，省去源码摘录。
pub(crate) fn profile_error_message(error: &lattice_profile::ProfileError) -> String {
    match error {
        lattice_profile::ProfileError::Toml { path, source, line } => {
            let location = line
                .map(|line| format!("line {line}: "))
                .unwrap_or_default();
            format!(
                "TOML parse error at {}: {location}{}",
                path.display(),
                source.message()
            )
        }
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn loaded_prompt_and_effective_policy_survive_source_removal() {
        let root = tempfile::tempdir().unwrap();
        let configuration_dir = root.path().join("home/.lattice");
        std::fs::create_dir_all(&configuration_dir).unwrap();
        std::fs::set_permissions(&configuration_dir, std::fs::Permissions::from_mode(0o700))
            .unwrap();
        let project = root.path().join("project");
        let package = project.join(".lattice/agents/test");
        std::fs::create_dir_all(&package).unwrap();
        let profile = package.join("agent.toml");
        let prompt = "冻结的提示".to_owned();
        std::fs::write(package.join("prompt.md"), &prompt).unwrap();
        std::fs::write(&profile, "schema_version=1\n[agent]\nname='test'\nmodel='local/test'\n[prompt]\nsystem_path='prompt.md'\n[lancet.policy]\npath='policy.toml'\n").unwrap();
        let policy_path = project.join("policy.toml");
        std::fs::write(
            &policy_path,
            "[network]\nallow_public_https=true\nallow_localhost=true\nallowed_hosts=['localhost']\nmax_web_response_bytes=234\n\
             [workspace]\ndenied_write_patterns=['locked/**']\n\
             included_ignored_read_patterns=['generated/**']\ndenied_read_patterns=['private/**']\n\
             [observation]\nmax_single_read_bytes=123\nmax_turn_observation_bytes=456\n\
             max_grep_matches=7\nmax_list_entries=8\nmax_find_paths=9\n",
        )
        .unwrap();
        let workspace = crate::LocalWorkspaceRoot::from_path(&project).unwrap();
        let loaded = load_for_turn("test", &workspace, &configuration_dir).unwrap();
        std::fs::remove_file(policy_path).unwrap();
        std::fs::remove_dir_all(package).unwrap();
        let lancet = &loaded.profile.lancet;
        assert_eq!(loaded.system_prompt, prompt);
        assert_eq!(
            lancet.project_root,
            Some(LancetProjectRootSection {
                root: Some(workspace.canonical_path().to_str().unwrap().into()),
            })
        );
        assert_eq!(
            lancet.network,
            Some(LancetNetworkSection {
                allow_public_https: Some(true),
                allow_localhost: Some(true),
                allowed_hosts: vec!["localhost".into()],
                max_web_response_bytes: Some(234),
            })
        );
        assert_eq!(
            lancet.workspace,
            Some(LancetWorkspacePolicySection {
                denied_write_patterns: vec!["locked/**".into()],
                included_ignored_read_patterns: vec!["generated/**".into()],
                denied_read_patterns: vec!["private/**".into()],
            })
        );
        assert_eq!(
            lancet.observation,
            Some(LancetObservationSection {
                max_single_read_bytes: Some(123),
                max_turn_observation_bytes: Some(456),
                max_grep_matches: Some(7),
                max_list_entries: Some(8),
                max_find_paths: Some(9),
            })
        );
        assert!(lancet.policy.is_none());
    }
}

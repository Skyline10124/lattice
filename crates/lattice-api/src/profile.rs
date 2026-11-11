// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 后台按项目与配置目录读取 profile，并冻结提示与模型预算。

use crate::session::{ApiError, Result};
use std::path::Path;

pub use lattice_profile::LoadedAgentProfile;

/// 已解析的执行输入，只持有有效值和提示正文，不借用源文件路径。
pub(crate) struct TurnProfile {
    pub effective: lattice_session::store::EffectiveProfile,
    pub system_prompt: Option<String>,
}

impl TurnProfile {
    pub(crate) fn from_loaded(loaded: &LoadedAgentProfile) -> Self {
        let source = &loaded.profile;
        let effective = lattice_session::store::EffectiveProfile {
            agent_name: source.agent.name.clone(),
            description: source.agent.description.clone(),
            default_model: source.agent.model.clone(),
            default_effort: source.agent.effort,
            timeout_secs: source.llm.timeout_secs,
            retry: source.llm.retry,
        };
        Self {
            effective,
            system_prompt: Some(loaded.system_prompt.clone()),
        }
    }
}

/// 配置目录来自后台启动环境；项目根已经与会话绑定核对。
pub(crate) fn load_for_turn(
    agent_name: &crate::AgentName,
    workspace: &crate::LocalWorkspaceRoot,
    configuration_dir: &Path,
) -> Result<LoadedAgentProfile> {
    let auth = lattice_llm::auth::AuthStore::new(configuration_dir.to_owned());
    let paths = lattice_profile::DiscoveryPaths {
        project_root: workspace.canonical_path().to_owned(),
        global_agents_dir: configuration_dir.join("agents"),
    };
    lattice_profile::load_agent_profile(
        agent_name,
        &paths,
        // 与本地 IPC 的属主检查共用同一个有效 UID 来源。
        crate::local::ipc::uid(),
    )
    .map_err(|error| {
        ApiError::Configuration(crate::ConfigRejection::new(
            &profile_error_message(&error),
            Some(&auth),
        ))
    })
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
    fn loaded_prompt_and_effective_profile_survive_source_removal() {
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
        std::fs::write(
            &profile,
            "schema_version=1\n[agent]\nname='test'\nmodel='local/test'\n[prompt]\nsystem_path='prompt.md'\n",
        )
        .unwrap();
        let workspace = crate::LocalWorkspaceRoot::from_path(&project).unwrap();
        let test_agent = crate::AgentName::parse("test").unwrap();
        let loaded = load_for_turn(&test_agent, &workspace, &configuration_dir).unwrap();
        std::fs::remove_dir_all(package).unwrap();
        assert_eq!(loaded.system_prompt, prompt);

        let normalized = TurnProfile::from_loaded(&loaded);
        assert_eq!(normalized.effective.timeout_secs.as_secs(), 300);
        let settings = lattice_session::store::SessionSettings {
            profile: normalized.effective.clone(),
            system_prompt: normalized.system_prompt.clone(),
            model: normalized.effective.default_model.clone(),
            effort: normalized.effective.default_effort,
        };
        let bytes = serde_json::to_vec(&settings).unwrap();
        let persisted: lattice_session::store::SessionSettings =
            serde_json::from_slice(&bytes).unwrap();
        assert_eq!(persisted.profile, normalized.effective);
        assert_eq!(persisted.system_prompt.as_deref(), Some(prompt.as_str()));

        // 防止 TurnProfile::from_loaded 截断或忽略超过一小时的显式 timeout_secs；
        // EffectiveProfile 为私有持久结构，E2E 无法在不跑满长流的情况下直接读取该字段。
        for timeout_secs in [3601, 86_400] {
            let mut explicit = loaded.clone();
            explicit.profile.llm.timeout_secs = crate::TurnTimeout::new(timeout_secs).unwrap();
            let prepared = TurnProfile::from_loaded(&explicit);
            assert_eq!(prepared.effective.timeout_secs.as_secs(), timeout_secs);
        }
    }
}

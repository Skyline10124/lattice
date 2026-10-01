// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::types::{LocalError, LocalErrorKind, Result};
use lattice_profile::{
    LancetNetworkSection, LancetObservationSection, LancetProjectRootSection,
    LancetWorkspacePolicySection,
};
use std::{path::PathBuf, sync::Arc};

/// 仅供私有 IPC 的冻结输入；路径已解引用，秘密不进入 profile 或公开 DTO。
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrozenConfiguration {
    profile_toml: String,
    system_prompt: String,
    connection: lattice_llm::ConnectionMaterial,
}

impl FrozenConfiguration {
    /// 供仅依赖 lattice-api 的提交端冻结当前有效连接环境；后台不得调用。
    pub fn from_env(
        loaded: crate::LoadedAgentProfile,
        workspace: &crate::LocalWorkspaceRoot,
    ) -> Result<Self> {
        let target = lattice_llm::ModelTarget::new(&loaded.profile.agent.model);
        let connection = lattice_llm::LlmConnection::from_env(&target).map_err(|_| invalid())?;
        Self::capture(loaded, workspace, connection)
    }

    /// 提交端先执行已有 dotenv/profile 优先级，再调用此方法；后台不运行环境安装器。
    /// 文件读取与 TLS 根读取是阻塞操作，应在提交端 spawn_blocking 内调用。
    pub fn capture(
        mut loaded: crate::LoadedAgentProfile,
        workspace: &crate::LocalWorkspaceRoot,
        connection: lattice_llm::LlmConnection,
    ) -> Result<Self> {
        let policy = crate::turn::cep::load_policy_config(
            &loaded.profile.lancet,
            workspace.canonical_path(),
        )?;
        let lancet = &mut loaded.profile.lancet;
        // 将有效策略填回已有具体字段，避免另立一份会被后台文件覆盖的策略权威。
        lancet.policy = None;
        lancet.project_root = Some(LancetProjectRootSection {
            root: Some(policy.project_root.to_str().ok_or_else(invalid)?.into()),
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
        let profile_toml = toml::to_string(&loaded.profile).map_err(|_| invalid())?;
        let frozen = Self {
            profile_toml,
            system_prompt: loaded.system_prompt,
            connection: connection.transport_material(),
        };
        frozen.validate_profile()?;
        Ok(frozen)
    }

    fn validate_profile(&self) -> Result<lattice_profile::AgentProfile> {
        if self.system_prompt.len() > 2 * 1024 * 1024 {
            return Err(invalid());
        }
        let profile: lattice_profile::AgentProfile =
            toml::from_str(&self.profile_toml).map_err(|_| invalid())?;
        let retry = &profile.llm.retry;
        if profile.schema_version != 1
            || profile.lancet.policy.is_some()
            || profile.agent.name.len() > 128
            || profile.agent.model.len() > 4096
            || retry.max_retries > 10
            || retry.initial_delay_ms > 60_000
            || retry.max_delay_ms > 60_000
            || retry.max_delay_ms < retry.initial_delay_ms
            || profile
                .llm
                .timeout_secs
                .is_some_and(|secs| !(1..=3600).contains(&secs))
        {
            return Err(invalid());
        }
        Ok(profile)
    }

    pub(super) fn into_execution(
        self,
    ) -> Result<(crate::LoadedAgentProfile, Arc<lattice_llm::LlmConnection>)> {
        let profile = self.validate_profile()?;
        let connection =
            lattice_llm::LlmConnection::from_transport(self.connection).map_err(|_| invalid())?;
        if profile.agent.model != connection.model().alias {
            return Err(invalid());
        }
        Ok((
            crate::LoadedAgentProfile {
                path: PathBuf::new(),
                package_dir: PathBuf::new(),
                profile,
                system_prompt: self.system_prompt,
            },
            Arc::new(connection),
        ))
    }
}

fn invalid() -> LocalError {
    LocalError::new(
        LocalErrorKind::BadRequest,
        "invalid or oversized frozen configuration",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_keeps_effective_policy_after_source_changes() {
        let project = tempfile::tempdir().unwrap();
        let profile_path = project.path().join("agent.toml");
        let policy_path = project.path().join("policy.toml");
        let prompt = "中".repeat(100 * 1024);
        std::fs::write(&profile_path, format!("schema_version=1\n[agent]\nname='test'\nmodel='compat:test'\n[prompt]\nsystem='{prompt}'\n[lancet.policy]\npath='policy.toml'\n")).unwrap();
        std::fs::write(
            &policy_path,
            "[network]\nallow_localhost=true\nallowed_hosts=['localhost']\n",
        )
        .unwrap();
        let loaded = lattice_profile::load_agent_profile(&profile_path).unwrap();
        let root = crate::LocalWorkspaceRoot::from_path(project.path()).unwrap();
        let connection = lattice_llm::LlmConnection::new(
            lattice_llm::ResolvedModel {
                alias: "compat:test".into(),
                provider: lattice_llm::ProviderKind::OpenAiCompatible,
                provider_model: "test".into(),
                base_url: "http://127.0.0.1:12345/v1".into(),
                api_key_env: String::new(),
                credential_status: lattice_llm::CredentialStatus::Present,
            },
            "test-private-key".into(),
            vec![],
        )
        .unwrap();
        let frozen = FrozenConfiguration::capture(loaded, &root, connection).unwrap();
        let transport = serde_json::to_vec(&frozen).unwrap();
        std::fs::remove_file(policy_path).unwrap();
        let (loaded, connection) = serde_json::from_slice::<FrozenConfiguration>(&transport)
            .unwrap()
            .into_execution()
            .unwrap();
        let policy =
            crate::turn::cep::load_policy_config(&loaded.profile.lancet, root.canonical_path())
                .unwrap();
        assert_eq!(loaded.system_prompt, prompt);
        assert!(policy.network.allow_localhost);
        assert_eq!(policy.network.allowed_hosts, vec!["localhost"]);
        assert_eq!(connection.model().base_url, "http://127.0.0.1:12345/v1");
        assert!(loaded.profile.lancet.policy.is_none());
    }
}

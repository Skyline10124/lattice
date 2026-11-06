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

/// 已解析的执行输入，只持有有效值和提示正文，不借用源文件路径。
pub(crate) struct TurnProfile {
    pub effective: lattice_session::store::EffectiveProfile,
    pub system_prompt: Option<String>,
}

pub(crate) struct PreparedProfile {
    pub configuration: TurnProfile,
    pub previous: Option<lattice_session::store::SessionProfileCopy>,
    pub inherited: bool,
    pub source_path: Option<std::path::PathBuf>,
}

impl TurnProfile {
    pub(crate) fn from_loaded(
        loaded: &LoadedAgentProfile,
        workspace: &crate::LocalWorkspaceRoot,
    ) -> Result<Self> {
        use lattice_session::store::*;
        let source = &loaded.profile;
        let policy =
            crate::turn::cep::load_policy_config(&source.lancet, workspace.canonical_path())?;
        let capabilities = source.capability_admission_policy();
        let effective = EffectiveProfile {
            agent_name: source.agent.name.clone(),
            description: source.agent.description.clone(),
            default_model: source.agent.model.clone(),
            default_effort: source.agent.effort,
            enabled_tools: source.tools.enabled.clone(),
            timeout_secs: source.llm.timeout_secs,
            retry: source.llm.retry,
            capabilities: ProfileCapabilities {
                deny: capabilities.denied_capabilities.into_iter().collect(),
                max_boundary: capabilities.max_boundary,
            },
            policy: ProfilePolicy {
                project_root: policy
                    .project_root
                    .to_str()
                    .ok_or_else(invalid_profile)?
                    .into(),
                workspace: ProfileWorkspace {
                    denied_write_patterns: policy.workspace.denied_write_patterns,
                    included_ignored_read_patterns: policy.workspace.included_ignored_read_patterns,
                    denied_read_patterns: policy.workspace.denied_read_patterns,
                },
                network: ProfileNetwork {
                    allow_public_https: policy.network.allow_public_https,
                    allow_localhost: policy.network.allow_localhost,
                    allowed_hosts: policy.network.allowed_hosts,
                    max_web_response_bytes: policy.network.max_web_response_bytes,
                },
                observation: ProfileObservation {
                    max_single_read_bytes: policy.observation.max_single_read_bytes,
                    max_turn_observation_bytes: policy.observation.max_turn_observation_bytes,
                    max_grep_matches: policy.observation.max_grep_matches,
                    max_list_entries: policy.observation.max_list_entries,
                    max_find_paths: policy.observation.max_find_paths,
                },
            },
            extensions: source
                .lancet
                .extensions
                .iter()
                .map(|(name, value)| {
                    serde_json::to_value(value)
                        .map(|value| (name.clone(), value))
                        .map_err(|_| invalid_profile())
                })
                .collect::<std::result::Result<_, ApiError>>()?,
        };
        Ok(Self {
            effective,
            system_prompt: Some(loaded.system_prompt.clone()),
        })
    }

    pub(crate) fn from_copy(
        copy: &lattice_session::store::SessionProfileCopy,
        prompt: Option<String>,
    ) -> Self {
        Self {
            effective: copy.profile.clone(),
            system_prompt: prompt,
        }
    }

    pub(crate) fn retry(&self) -> crate::RetryPolicy {
        self.effective.retry
    }

    pub(crate) fn capability_admission(
        &self,
    ) -> lancet_protocol::evidence::CapabilityAdmissionPolicy {
        lancet_protocol::evidence::CapabilityAdmissionPolicy {
            enabled_tools: self.effective.enabled_tools.iter().cloned().collect(),
            denied_capabilities: self.effective.capabilities.deny.iter().cloned().collect(),
            max_boundary: self.effective.capabilities.max_boundary,
        }
    }

    pub(crate) fn lancet(&self) -> lattice_profile::LancetSection {
        let policy = &self.effective.policy;
        lattice_profile::LancetSection {
            project_root: Some(LancetProjectRootSection {
                root: Some(policy.project_root.clone()),
            }),
            workspace: Some(LancetWorkspacePolicySection {
                denied_write_patterns: policy.workspace.denied_write_patterns.clone(),
                included_ignored_read_patterns: policy
                    .workspace
                    .included_ignored_read_patterns
                    .clone(),
                denied_read_patterns: policy.workspace.denied_read_patterns.clone(),
            }),
            network: Some(LancetNetworkSection {
                allow_public_https: Some(policy.network.allow_public_https),
                allow_localhost: Some(policy.network.allow_localhost),
                allowed_hosts: policy.network.allowed_hosts.clone(),
                max_web_response_bytes: Some(policy.network.max_web_response_bytes),
            }),
            observation: Some(LancetObservationSection {
                max_single_read_bytes: Some(policy.observation.max_single_read_bytes),
                max_turn_observation_bytes: Some(policy.observation.max_turn_observation_bytes),
                max_grep_matches: Some(policy.observation.max_grep_matches),
                max_list_entries: Some(policy.observation.max_list_entries),
                max_find_paths: Some(policy.observation.max_find_paths),
            }),
            ..Default::default()
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
    let mut loaded = lattice_profile::load_agent_profile(
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
    })?;
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
        let test_agent = crate::AgentName::parse("test").unwrap();
        let loaded = load_for_turn(&test_agent, &workspace, &configuration_dir).unwrap();
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
        let normalized = TurnProfile::from_loaded(&loaded, &workspace).unwrap();
        assert_eq!(normalized.effective.timeout_secs.as_secs(), 300);
        let config = lattice_session::store::FrozenTurnConfig {
            agent_name: test_agent,
            model: crate::ModelId::parse("test").unwrap(),
            provider: crate::ProviderId::parse("local").unwrap(),
            effort: None,
            system_prompt: normalized.system_prompt.clone(),
            catalog: lattice_session::store::ToolCatalogSnapshot::new("test".into(), vec![])
                .unwrap(),
            limits: lattice_session::store::TurnExecutionLimits {
                timeout_secs: crate::TurnTimeout::default(),
                max_model_steps: 32,
                max_model_attempts: normalized.retry().max_attempts(),
            },
        };
        let mut copy = lattice_session::store::SessionProfileCopy::for_test(&config);
        copy.profile = normalized.effective;
        let bytes = serde_json::to_vec(&copy).unwrap();
        let persisted = serde_json::from_slice(&bytes).unwrap();
        let restored = TurnProfile::from_copy(&persisted, config.system_prompt);
        assert_eq!(
            serde_json::to_value(restored.lancet()).unwrap(),
            serde_json::to_value(&loaded.profile.lancet).unwrap()
        );
        assert_eq!(restored.system_prompt.as_deref(), Some(prompt.as_str()));
        assert_eq!(
            serde_json::to_value(restored.capability_admission()).unwrap(),
            serde_json::to_value(loaded.profile.capability_admission_policy()).unwrap()
        );

        // 防止 TurnProfile::from_loaded 截断或忽略超过一小时的显式 timeout_secs；
        // EffectiveProfile 为私有持久结构，E2E 无法在不跑满长流的情况下直接读取该字段。
        for timeout_secs in [3601, 86_400] {
            let mut explicit = loaded.clone();
            explicit.profile.llm.timeout_secs = crate::TurnTimeout::new(timeout_secs).unwrap();
            let prepared = TurnProfile::from_loaded(&explicit, &workspace).unwrap();
            assert_eq!(prepared.effective.timeout_secs.as_secs(), timeout_secs);
        }
    }
}

// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 会话持有的有效 profile；源文件与请求网关的全局状态不进入这份记录。

use super::{FrozenTurnConfig, Result, hex_digest, invalid};
use crate::model::{AgentName, RetryPolicy, TurnTimeout};
use lancet_protocol::cell::Trust;
use lattice_conversation::{Effort, ModelId, ModelRef, ProviderId};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionModelChoice {
    pub provider: ProviderId,
    pub model: ModelId,
    pub effort: Option<Effort>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionProfileCopy {
    pub profile: EffectiveProfile,
    #[serde(with = "hex_digest")]
    pub prompt_config_digest: [u8; 32],
    pub current: SessionModelChoice,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectiveProfile {
    pub agent_name: AgentName,
    pub description: Option<String>,
    pub default_model: Option<ModelRef>,
    pub default_effort: Option<Effort>,
    pub enabled_tools: Vec<String>,
    pub timeout_secs: TurnTimeout,
    pub retry: RetryPolicy,
    pub capabilities: ProfileCapabilities,
    pub policy: ProfilePolicy,
    pub extensions: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileCapabilities {
    pub deny: Vec<String>,
    pub max_boundary: Trust,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfilePolicy {
    pub project_root: String,
    pub workspace: ProfileWorkspace,
    pub network: ProfileNetwork,
    pub observation: ProfileObservation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileWorkspace {
    pub denied_write_patterns: Vec<String>,
    pub included_ignored_read_patterns: Vec<String>,
    pub denied_read_patterns: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileNetwork {
    pub allow_public_https: bool,
    pub allow_localhost: bool,
    pub allowed_hosts: Vec<String>,
    pub max_web_response_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileObservation {
    pub max_single_read_bytes: usize,
    pub max_turn_observation_bytes: usize,
    pub max_grep_matches: usize,
    pub max_list_entries: usize,
    pub max_find_paths: usize,
}

impl EffectiveProfile {
    fn validate(&self) -> Result<()> {
        if !Path::new(&self.policy.project_root).is_absolute()
            || self.policy.project_root.contains('\0')
        {
            return Err(invalid("profile", "invalid policy project root"));
        }
        if self.enabled_tools.iter().any(|name| {
            name.is_empty()
                || name.len() > 64
                || !name
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
        }) || self
            .capabilities
            .deny
            .iter()
            .any(|name| name.is_empty() || name.chars().any(char::is_control))
            || self.extensions.values().any(|value| !value.is_object())
        {
            return Err(invalid(
                "profile",
                "invalid tools, capabilities or extensions",
            ));
        }
        Ok(())
    }
}

impl SessionProfileCopy {
    pub(super) fn validate(&self) -> Result<()> {
        self.profile.validate()
    }

    /// 只作为准备与接受之间的比较依据，不增加第二份持久版本号。
    pub fn digest(&self) -> Result<[u8; 32]> {
        use sha2::{Digest, Sha256};
        let bytes = super::codec::canonical(self)?;
        let mut digest = Sha256::new();
        digest.update(b"lattice-session-profile-copy-v1\0");
        digest.update((bytes.len() as u64).to_be_bytes());
        digest.update(bytes);
        Ok(digest.finalize().into())
    }

    pub(super) fn matches_config(&self, config: &FrozenTurnConfig) -> bool {
        self.profile.agent_name == config.agent_name
            && self.current.provider == config.provider
            && self.current.model == config.model
            && self.current.effort == config.effort
            && self.profile.timeout_secs == config.limits.timeout_secs
            && self.profile.retry.max_attempts() == config.limits.max_model_attempts
    }

    /// 隔离测试的有效副本夹具；生产只能从已校验的源配置或持久副本准备输入。
    #[cfg(any(test, feature = "test-utils"))]
    pub fn for_test(config: &FrozenTurnConfig) -> Self {
        Self {
            profile: EffectiveProfile {
                agent_name: config.agent_name.clone(),
                description: None,
                default_model: Some(ModelRef::new(config.provider.clone(), config.model.clone())),
                default_effort: config.effort,
                enabled_tools: config
                    .catalog
                    .entries
                    .iter()
                    .map(|entry| entry.name.clone())
                    .collect(),
                timeout_secs: config.limits.timeout_secs,
                retry: RetryPolicy::new(
                    config
                        .limits
                        .max_model_attempts
                        .saturating_sub(1)
                        .min(crate::MAX_MODEL_ATTEMPTS - 1),
                    500,
                    3000,
                )
                .expect("valid test retry policy"),
                capabilities: ProfileCapabilities {
                    deny: vec![],
                    max_boundary: Trust::In,
                },
                policy: ProfilePolicy {
                    project_root: "/".into(),
                    workspace: ProfileWorkspace {
                        denied_write_patterns: vec![],
                        included_ignored_read_patterns: vec![],
                        denied_read_patterns: vec![],
                    },
                    network: ProfileNetwork {
                        allow_public_https: true,
                        allow_localhost: false,
                        allowed_hosts: vec![],
                        max_web_response_bytes: 10 * 1024 * 1024,
                    },
                    observation: ProfileObservation {
                        max_single_read_bytes: 10 * 1024 * 1024,
                        max_turn_observation_bytes: 10 * 1024 * 1024,
                        max_grep_matches: 1000,
                        max_list_entries: 1000,
                        max_find_paths: 1000,
                    },
                },
                extensions: BTreeMap::new(),
            },
            prompt_config_digest: config.digest().unwrap(),
            current: SessionModelChoice {
                provider: config.provider.clone(),
                model: config.model.clone(),
                effort: config.effort,
            },
        }
    }
}

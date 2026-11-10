// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 会话持有的有效 profile；源文件与请求网关的全局状态不进入这份记录。

use super::{EffectiveProfile, FrozenTurnConfig, Result, hex_digest};
use lattice_conversation::{Effort, ModelId, ProviderId};
use serde::{Deserialize, Serialize};

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

impl SessionProfileCopy {
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
        use crate::model::RetryPolicy;
        use lattice_conversation::ModelRef;

        Self {
            profile: EffectiveProfile {
                agent_name: config.agent_name.clone(),
                description: None,
                default_model: Some(ModelRef::new(config.provider.clone(), config.model.clone())),
                default_effort: config.effort,
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

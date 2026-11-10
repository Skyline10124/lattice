// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 会话持有的下一轮配置与内容摘要版本；源文件与请求网关的全局状态不进入这份记录。

use super::{Result, hex_digest};
use crate::model::{AgentName, RetryPolicy, TurnTimeout};
use lattice_conversation::{Effort, ModelRef};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectiveProfile {
    pub agent_name: AgentName,
    pub description: Option<String>,
    pub default_model: Option<ModelRef>,
    pub default_effort: Option<Effort>,
    pub timeout_secs: TurnTimeout,
    pub retry: RetryPolicy,
}

/// 会话设置：下一轮将使用的配置（ADR 0012 第 1 节）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionSettings {
    pub profile: EffectiveProfile,
    pub system_prompt: Option<String>,
    pub model: Option<ModelRef>,
    pub effort: Option<Effort>,
}

/// 会话设置的内容摘要版本，供提交时核对前端所见配置未发生变化。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsVersion(#[serde(with = "hex_digest")] [u8; 32]);

impl SessionSettings {
    /// 版本只由规范编码内容决定，设置改回原值时自动恢复原版本。
    pub fn version(&self) -> Result<SettingsVersion> {
        use sha2::{Digest, Sha256};
        let bytes = super::codec::canonical(self)?;
        let mut digest = Sha256::new();
        digest.update(b"lattice-session-settings-v1\0");
        digest.update((bytes.len() as u64).to_be_bytes());
        digest.update(bytes);
        Ok(SettingsVersion(digest.finalize().into()))
    }

    /// 与单轮持久配置共用 2 MiB 系统提示上限，防止草稿或设置写入接受后续无法冻结的提示。
    pub fn validate(&self) -> Result<()> {
        super::config::validate_system_prompt("settings", self.system_prompt.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::super::StoreError;
    use super::*;
    use lattice_conversation::{ModelId, ProviderId};

    fn model_ref(provider: &str, model: &str) -> ModelRef {
        ModelRef::new(
            ProviderId::parse(provider).expect("解析测试 ProviderId"),
            ModelId::parse(model).expect("解析测试 ModelId"),
        )
    }

    fn base_settings() -> SessionSettings {
        SessionSettings {
            profile: EffectiveProfile {
                agent_name: AgentName::parse("coder").expect("解析测试 AgentName"),
                description: Some("编码助手".to_string()),
                default_model: Some(model_ref("openai", "gpt-5")),
                default_effort: Some(Effort::Medium),
                timeout_secs: TurnTimeout::new(120).expect("构造测试 TurnTimeout"),
                retry: RetryPolicy::new(2, 500, 3_000).expect("构造测试 RetryPolicy"),
            },
            system_prompt: Some("初始系统提示".to_string()),
            model: Some(model_ref("openai", "gpt-5")),
            effort: Some(Effort::Medium),
        }
    }

    #[test]
    fn version_depends_only_on_content() {
        let base = base_settings();
        let same = base_settings();
        let base_version = base.version().expect("计算基准设置版本");
        assert_eq!(
            same.version().expect("计算相同内容设置版本"),
            base_version,
            "内容相同的两份设置版本必须相同"
        );

        type Case = (
            &'static str,
            fn(&mut SessionSettings),
            fn(&mut SessionSettings, &SessionSettings),
        );
        let cases: &[Case] = &[
            (
                "system_prompt",
                |s| s.system_prompt = Some("修改后的系统提示".to_string()),
                |s, orig| s.system_prompt.clone_from(&orig.system_prompt),
            ),
            (
                "model",
                |s| s.model = Some(model_ref("anthropic", "claude-sonnet-4")),
                |s, orig| s.model.clone_from(&orig.model),
            ),
            (
                "effort",
                |s| s.effort = Some(Effort::High),
                |s, orig| s.effort = orig.effort,
            ),
            (
                "profile.default_effort",
                |s| s.profile.default_effort = Some(Effort::Low),
                |s, orig| s.profile.default_effort = orig.profile.default_effort,
            ),
            (
                "profile.retry",
                |s| {
                    s.profile.retry =
                        RetryPolicy::new(4, 1_000, 5_000).expect("构造修改后的 RetryPolicy");
                },
                |s, orig| s.profile.retry = orig.profile.retry,
            ),
        ];

        let mut current = base_settings();
        for (field, change, revert) in cases {
            change(&mut current);
            let changed_version = current.version().expect("计算修改后的设置版本");
            assert_ne!(
                changed_version, base_version,
                "修改 {field} 后设置版本必须变化"
            );

            revert(&mut current, &base);
            let reverted_version = current.version().expect("计算改回原值后的设置版本");
            assert_eq!(
                reverted_version, base_version,
                "将 {field} 改回原值后设置版本必须恢复"
            );
        }
    }

    #[test]
    fn system_prompt_respects_two_mib_bound() {
        let mut settings = base_settings();
        settings.system_prompt = Some("a".repeat(2 * 1024 * 1024));
        assert!(
            settings.validate().is_ok(),
            "恰好 2 MiB 的系统提示应当通过校验"
        );

        settings.system_prompt = Some("a".repeat(2 * 1024 * 1024 + 1));
        let err = settings
            .validate()
            .expect_err("超过 2 MiB 1 个字节的系统提示应当被拒绝");
        assert!(
            matches!(
                &err,
                StoreError::InvalidInput { detail, .. } if detail == "system prompt exceeds 2 MiB bound"
            ),
            "超出 2 MiB 上限应返回预期 invalid 错误，实际为 {err:?}"
        );
    }
}

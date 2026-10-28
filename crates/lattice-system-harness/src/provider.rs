// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 场景 provider 接线：协议、配置目标、模型与凭据由同一个入口转换为产品配置。

use serde_json::{Value, json};

use crate::{FakeLlmServer, ScenarioWorld, TlsIdentity, WorldError};

/// 场景使用的模型流式协议。
#[derive(Clone, Copy, Debug)]
pub enum ProviderProtocol {
    ChatCompletions,
    AnthropicMessages,
    GenerateContent,
}

/// 区分所测 provider 的内置 id 与自定义 id。
#[derive(Clone, Copy, Debug)]
pub enum ProviderTarget<'a> {
    Builtin(&'a str),
    Custom(&'a str),
}

#[derive(Clone, Copy, Debug)]
pub struct ProviderFixture<'a> {
    pub protocol: ProviderProtocol,
    pub target: ProviderTarget<'a>,
    pub model: &'a str,
}

impl<'a> ProviderFixture<'a> {
    /// 共用本地 Chat Completions provider，模型由各场景命名。
    pub const fn chat_completions(model: &'a str) -> Self {
        Self {
            protocol: ProviderProtocol::ChatCompletions,
            target: ProviderTarget::Custom("local"),
            model,
        }
    }

    /// profile 与接线使用同一模型引用；profile 可以先于 fake server 创建。
    pub fn model_ref(self) -> Result<String, WorldError> {
        Ok(self.legacy()?.model_ref)
    }

    /// 公开 IPC 提交中的 provider 连接材料，与 CLI 接线使用相同的模型、provider 映射和信任根。
    pub fn configuration(self, base_url: &str, key: &str) -> Result<Value, WorldError> {
        let legacy = self.legacy()?;
        require_non_empty_key(key)?;
        Ok(json!({
            "alias": legacy.model_ref,
            "provider": legacy.provider,
            "provider_model": self.model,
            "base_url": base_url,
            "api_key": key,
            "roots_der": [TlsIdentity::shared()?.ca_der()],
        }))
    }

    fn legacy(self) -> Result<LegacyProvider, WorldError> {
        use ProviderProtocol::*;
        use ProviderTarget::*;
        let (provider, endpoint_env, key_env, model_ref) = match (self.protocol, self.target) {
            (ChatCompletions, Custom(_)) => (
                "openai_compatible",
                "OPENAI_COMPATIBLE_BASE_URL",
                "OPENAI_COMPATIBLE_API_KEY",
                format!("compat:{}", self.model),
            ),
            (AnthropicMessages, Builtin("anthropic")) => (
                "anthropic",
                "ANTHROPIC_BASE_URL",
                "ANTHROPIC_API_KEY",
                match self.model {
                    "claude-sonnet-4-6" => "sonnet",
                    _ => return Err(WorldError::Provider("unsupported Anthropic model alias")),
                }
                .into(),
            ),
            (GenerateContent, Custom(_)) => (
                "gemini",
                "GEMINI_BASE_URL",
                "GEMINI_API_KEY",
                match self.model {
                    "gemini-2.5-flash" => "gemini",
                    _ => {
                        return Err(WorldError::Provider(
                            "unsupported generateContent model alias",
                        ));
                    }
                }
                .into(),
            ),
            _ => return Err(WorldError::Provider("unsupported provider wiring")),
        };
        Ok(LegacyProvider {
            provider,
            endpoint_env,
            key_env,
            model_ref,
        })
    }
}

struct LegacyProvider {
    provider: &'static str,
    endpoint_env: &'static str,
    key_env: &'static str,
    model_ref: String,
}

fn require_non_empty_key(key: &str) -> Result<(), WorldError> {
    if key.is_empty() {
        return Err(WorldError::Provider("provider key must not be empty"));
    }
    Ok(())
}

impl ScenarioWorld {
    /// 把 fake server 与非空凭据接到所测协议，返回可写入 profile 的模型引用。
    pub fn wire_provider(
        &self,
        provider: ProviderFixture<'_>,
        server: &FakeLlmServer,
        key: &str,
    ) -> Result<String, WorldError> {
        let legacy = provider.legacy()?;
        self.register_key(provider, Some(key))?;
        self.set_env(legacy.endpoint_env, server.base_url());
        if matches!(provider.protocol, ProviderProtocol::ChatCompletions) {
            self.set_env("OPENAI_COMPATIBLE_MODEL", provider.model);
        }
        Ok(legacy.model_ref)
    }

    /// 设置非空 provider 凭据；`None` 移除已有凭据。
    pub fn register_key(
        &self,
        provider: ProviderFixture<'_>,
        key: Option<&str>,
    ) -> Result<(), WorldError> {
        let legacy = provider.legacy()?;
        match key {
            Some(key) => {
                require_non_empty_key(key)?;
                self.set_env(legacy.key_env, key);
            }
            None => self.unset_env(legacy.key_env),
        }
        Ok(())
    }
}

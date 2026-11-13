// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 场景 provider 通过用户级配置与公开凭据命令接入后台。

use crate::{FakeLlmServer, LatticeCommand, LatticeDriver, ScenarioWorld, WorldError};

/// 场景使用的模型流式协议。
#[derive(Clone, Copy, Debug)]
pub enum ProviderProtocol {
    ChatCompletions,
    Responses,
    Interactions,
    AnthropicMessages,
    GenerateContent,
}

/// 区分所测 provider 的内置标识与自定义标识。
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

    /// 智能体配置与 provider 接线使用同一模型引用；智能体配置可以先于假模型服务创建。
    pub fn model_ref(self) -> Result<String, WorldError> {
        Ok(format!("{}/{}", self.id(), self.model))
    }

    fn id(self) -> &'a str {
        match self.target {
            ProviderTarget::Builtin(id) | ProviderTarget::Custom(id) => id,
        }
    }

    /// 内置 provider 只覆盖地址；自定义 provider 显式声明协议与认证方式。
    pub fn configure(self, world: &ScenarioWorld, base_url: &str) -> Result<(), WorldError> {
        let directory = world.home().join(".lattice");
        std::fs::create_dir_all(&directory)?;
        let path = directory.join("providers.toml");
        let mut config: toml::Table = match std::fs::read_to_string(&path) {
            Ok(source) => toml::from_str(&source)
                .map_err(|_| WorldError::Provider("夹具 providers.toml 格式无效"))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => toml::Table::new(),
            Err(error) => return Err(error.into()),
        };
        let providers = config
            .entry("providers")
            .or_insert_with(|| toml::Value::Table(toml::Table::new()))
            .as_table_mut()
            .ok_or(WorldError::Provider("夹具 providers 表结构无效"))?;
        let mut entry = providers
            .get(self.id())
            .and_then(toml::Value::as_table)
            .cloned()
            .unwrap_or_default();
        entry.insert("base_url".into(), base_url.into());
        if matches!(self.target, ProviderTarget::Custom(_)) {
            let protocol = match self.protocol {
                ProviderProtocol::ChatCompletions => "chat-completions",
                ProviderProtocol::Responses => "responses",
                ProviderProtocol::Interactions => "interactions",
                ProviderProtocol::AnthropicMessages => "anthropic-messages",
                ProviderProtocol::GenerateContent => "generate-content",
            };
            entry.insert("protocol".into(), protocol.into());
            let auth = match self.protocol {
                ProviderProtocol::ChatCompletions | ProviderProtocol::Responses => "bearer",
                ProviderProtocol::AnthropicMessages => "x-api-key",
                ProviderProtocol::GenerateContent | ProviderProtocol::Interactions => {
                    "x-goog-api-key"
                }
            };
            entry.insert("auth".into(), auth.into());
        }
        providers.insert(self.id().into(), toml::Value::Table(entry));
        let source = toml::to_string(&config)
            .map_err(|_| WorldError::Provider("无法序列化夹具 provider 配置"))?;
        std::fs::write(path, source)?;
        Ok(())
    }
}

impl ScenarioWorld {
    /// 把假模型服务与非空凭据接入所测协议，返回可写入智能体配置的模型引用。
    pub fn wire_provider(
        &self,
        provider: ProviderFixture<'_>,
        server: &FakeLlmServer,
        key: &str,
    ) -> Result<String, WorldError> {
        provider.configure(self, &server.base_url())?;
        self.register_key(provider, Some(key))?;
        provider.model_ref()
    }

    /// 设置非空 provider 凭据；传入 `None` 时移除已有凭据。
    /// 密钥只通过标准输入录入，不进入命令行参数或失败产物。
    pub fn register_key(
        &self,
        provider: ProviderFixture<'_>,
        key: Option<&str>,
    ) -> Result<(), WorldError> {
        if key.is_some_and(str::is_empty) {
            return Err(WorldError::Provider("provider 密钥不能为空"));
        }
        let driver = LatticeDriver::from_env()
            .map_err(|_| WorldError::Provider("LATTICE_BIN 必须指向已构建的 lattice 二进制"))?;
        let command = LatticeCommand {
            args: vec![
                "auth".into(),
                if key.is_some() { "set" } else { "remove" }.into(),
                provider.id().into(),
            ],
            timeout: Some(
                self.budget_slot()
                    .normal_stop_deadline()
                    .saturating_duration_since(std::time::Instant::now()),
            ),
        };
        let outcome = driver
            .run_with_stdin(self, &command, key.unwrap_or_default().as_bytes())
            .map_err(|_| WorldError::Provider("执行凭据命令失败"))?;
        if !outcome.is_clean_exit(0) {
            return Err(WorldError::Provider("凭据命令被拒绝"));
        }
        Ok(())
    }
}

// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 场景 provider 通过用户级配置与公开凭据命令接入后台。

use crate::{FakeLlmServer, LatticeCommand, LatticeDriver, ScenarioWorld, WorldError};

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
        Ok(format!("{}/{}", self.id(), self.model))
    }

    fn id(self) -> &'a str {
        match self.target {
            ProviderTarget::Builtin(id) | ProviderTarget::Custom(id) => id,
        }
    }

    /// 内置 provider 只覆盖地址；自定义 provider 显式声明协议与认证。
    pub fn configure(self, world: &ScenarioWorld, base_url: &str) -> Result<(), WorldError> {
        let directory = world.home().join(".lattice");
        std::fs::create_dir_all(&directory)?;
        let path = directory.join("providers.toml");
        let mut config: toml::Table = match std::fs::read_to_string(&path) {
            Ok(source) => toml::from_str(&source)
                .map_err(|_| WorldError::Provider("invalid fixture providers.toml"))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => toml::Table::new(),
            Err(error) => return Err(error.into()),
        };
        let providers = config
            .entry("providers")
            .or_insert_with(|| toml::Value::Table(toml::Table::new()))
            .as_table_mut()
            .ok_or(WorldError::Provider("invalid fixture providers table"))?;
        let mut entry = providers
            .get(self.id())
            .and_then(toml::Value::as_table)
            .cloned()
            .unwrap_or_default();
        entry.insert("base_url".into(), base_url.into());
        if matches!(self.target, ProviderTarget::Custom(_)) {
            let protocol = match self.protocol {
                ProviderProtocol::ChatCompletions => "chat-completions",
                ProviderProtocol::AnthropicMessages => "anthropic-messages",
                ProviderProtocol::GenerateContent => "generate-content",
            };
            entry.insert("protocol".into(), protocol.into());
            let auth = match self.protocol {
                ProviderProtocol::ChatCompletions => "bearer",
                ProviderProtocol::AnthropicMessages => "x-api-key",
                ProviderProtocol::GenerateContent => "x-goog-api-key",
            };
            entry.insert("auth".into(), auth.into());
        }
        providers.insert(self.id().into(), toml::Value::Table(entry));
        let source = toml::to_string(&config)
            .map_err(|_| WorldError::Provider("cannot serialize fixture provider"))?;
        std::fs::write(path, source)?;
        Ok(())
    }
}

impl ScenarioWorld {
    /// 把 fake server 与非空凭据接到所测协议，返回可写入 profile 的模型引用。
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

    /// 设置非空 provider 凭据；`None` 移除已有凭据。
    /// key 只通过标准输入录入，不进入参数或证据。
    pub fn register_key(
        &self,
        provider: ProviderFixture<'_>,
        key: Option<&str>,
    ) -> Result<(), WorldError> {
        if key.is_some_and(str::is_empty) {
            return Err(WorldError::Provider("provider key must not be empty"));
        }
        let driver = LatticeDriver::from_env().map_err(|_| {
            WorldError::Provider("LATTICE_BIN must point to the built lattice binary")
        })?;
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
            .map_err(|_| WorldError::Provider("credential command failed"))?;
        if !outcome.is_clean_exit(0) {
            return Err(WorldError::Provider("credential command was rejected"));
        }
        Ok(())
    }
}

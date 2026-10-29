// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 每次请求重读 provider 与凭据，Turn 句柄封装地址固定和连接复用。

use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use crate::auth::{AuthStore, Credentials};
use crate::client::{self, RequestEndpoint, TurnClient};
use crate::provider::{Auth, Provider, config::ProviderRegistry, validate_model_id};
use crate::{ChatRequest, LlmError, LlmStream, StreamControl};

struct Configuration {
    directory: PathBuf,
    roots: Vec<reqwest::Certificate>,
}

/// 持有配置目录与给定的 TLS 根，按 Turn 创建连接句柄。
pub struct RequestGateway {
    configuration: Arc<Configuration>,
}

impl RequestGateway {
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "#315 第 8 个 PR 接入后台请求网关")
    )]
    pub fn new(config_dir: PathBuf, roots_der: Vec<Vec<u8>>) -> Result<Self, LlmError> {
        Ok(Self {
            configuration: Arc::new(Configuration {
                directory: config_dir,
                roots: client::tls_certificates(&roots_der)?,
            }),
        })
    }

    /// 新句柄独占一轮的连接缓存；网关本身不跨 Turn 保存 client。
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "#315 第 8 个 PR 接入后台请求网关")
    )]
    pub fn open_turn(&self) -> GatewayTurn {
        GatewayTurn {
            configuration: self.configuration.clone(),
            clients: Mutex::default(),
        }
    }
}

/// provider 与 base_url 共同区分连接，凭据始终放在单次请求中。
pub struct GatewayTurn {
    configuration: Arc<Configuration>,
    clients: Mutex<BTreeMap<(String, String), Arc<TurnClient>>>,
}

impl GatewayTurn {
    /// target 必须分别给出 provider id 与模型 id，client_request_id 承载 SessionId。
    /// 只发送对话与工具；采样及可选输出上限沿用服务端默认值。
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "#315 第 8 个 PR 注入 lattice-agent 的 LlmClient")
    )]
    pub async fn stream_chat(
        &self,
        mut request: ChatRequest,
        control: StreamControl,
    ) -> Result<LlmStream, LlmError> {
        let provider_id = request
            .target
            .provider
            .as_deref()
            .ok_or_else(|| configuration_error("gateway requests require a provider id"))?;
        lattice_conversation::validate_provider_id(provider_id).map_err(configuration_error)?;
        validate_model_id(&request.target.model).map_err(configuration_error)?;
        if request.client_request_id.is_none() {
            return Err(configuration_error(
                "gateway requests require a SessionId in client_request_id",
            ));
        }
        let directory = self.configuration.directory.clone();
        let provider_id = provider_id.to_owned();
        let (provider, credentials) =
            tokio::task::spawn_blocking(move || load_provider(directory, &provider_id))
                .await
                .map_err(|_| configuration_error("provider configuration task failed"))??;
        let client = self
            .clients
            .lock()
            .unwrap()
            .entry((provider.id.clone(), provider.base_url.clone()))
            .or_default()
            .clone();
        request.temperature = None;
        request.max_tokens = None;
        request.max_completion_tokens = None;
        let model = request.target.model.clone();
        client::dispatch(
            request,
            RequestEndpoint {
                provider: &provider.id,
                model: &model,
                base_url: &provider.base_url,
                protocol: provider.protocol,
                auth: provider.auth,
                credentials,
                roots: &self.configuration.roots,
                client,
            },
            control,
        )
        .await
    }
}

fn configuration_error(message: impl std::fmt::Display) -> LlmError {
    LlmError::Config {
        message: message.to_string(),
    }
}

fn load_provider(directory: PathBuf, id: &str) -> Result<(Provider, Credentials), LlmError> {
    let credentials = AuthStore::new(directory.clone()).load();
    let registry = ProviderRegistry::load(directory.join("providers.toml")).map_err(|error| {
        let message = match &credentials {
            Ok(credentials) => credentials.redact(&error.to_string()),
            Err(auth_error) => format!(
                "{}:{}: invalid provider configuration; {auth_error}",
                error.path.display(),
                error.line,
            ),
        };
        configuration_error(message)
    })?;
    let provider = registry.get(id).cloned().ok_or_else(|| {
        let message = credentials.as_ref().map_or_else(
            |_| "requested provider is not registered".into(),
            |credentials| credentials.redact(&format!("provider {id:?} is not registered")),
        );
        configuration_error(message)
    })?;
    let credentials = match credentials {
        Ok(credentials) => credentials,
        Err(_) if provider.auth == Auth::None => Credentials::default(),
        Err(error) => return Err(configuration_error(error)),
    };
    Ok((provider, credentials))
}

#[cfg(test)]
mod tests;

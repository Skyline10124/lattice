// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 每次请求重读 provider 与凭据，Turn 句柄封装地址固定和连接复用。

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use crate::auth::{AuthError, AuthStore, Credentials};
use crate::client::{self, RequestEndpoint, TurnClient};
use crate::provider::{Auth, Provider, config::ProviderRegistry, validate_model_id};
use crate::{ChatRequest, LlmError, LlmStream, ModelTarget, StreamControl};

struct Configuration {
    directory: PathBuf,
    roots: Vec<reqwest::Certificate>,
    roots_der: Vec<Vec<u8>>,
    #[cfg(test)]
    metadata_url: Option<String>,
}

/// 持有配置目录与给定的 TLS 根，按 Turn 创建连接句柄。
pub struct RequestGateway {
    configuration: Arc<Configuration>,
}

impl RequestGateway {
    pub fn new(config_dir: PathBuf, roots_der: Vec<Vec<u8>>) -> Result<Self, LlmError> {
        Ok(Self {
            configuration: Arc::new(Configuration {
                directory: config_dir,
                roots: client::tls_certificates(&roots_der)?,
                roots_der,
                #[cfg(test)]
                metadata_url: None,
            }),
        })
    }

    /// Tool Host 使用同一份启动时信任根，每个 Turn 独立构造信任配置。
    pub fn tls_roots_der(&self) -> &[Vec<u8>] {
        &self.configuration.roots_der
    }

    pub fn auth_store(&self) -> AuthStore {
        AuthStore::new(self.configuration.directory.clone())
    }

    /// 刷新只读取当前注册表，固定使用正式元数据源与启动时的信任根。
    pub async fn refresh_metadata(
        &self,
        cancel: Arc<crate::MetadataRefreshControl>,
    ) -> Result<(), crate::MetadataError> {
        if self.configuration.roots_der.is_empty() {
            return Err(crate::MetadataError::Configuration(
                "no usable TLS root certificates".into(),
            ));
        }
        let directory = self.configuration.directory.clone();
        let registry = tokio::task::spawn_blocking(move || {
            ProviderRegistry::load(directory.join("providers.toml"))
                .map_err(|error| crate::MetadataError::Configuration(error.to_string()))
        })
        .await??;
        let metadata_url = crate::MODELS_URL;
        #[cfg(test)]
        let metadata_url = self
            .configuration
            .metadata_url
            .as_deref()
            .unwrap_or(metadata_url);
        crate::metadata::fetch::refresh_cancellable(
            &self.configuration.directory,
            &registry,
            metadata_url,
            &self.configuration.roots_der,
            cancel,
        )
        .await
    }

    #[cfg(test)]
    fn with_metadata_url(mut self, url: String) -> Self {
        Arc::get_mut(&mut self.configuration)
            .expect("unshared test gateway")
            .metadata_url = Some(url);
        self
    }

    /// 新句柄独占一轮的连接缓存；网关本身不跨 Turn 保存 client。
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
    pub fn auth_store(&self) -> AuthStore {
        AuthStore::new(self.configuration.directory.clone())
    }

    /// 每次解析读取当前配置与凭据；发送句柄独占这一次解析结果。
    pub async fn resolve(&self, target: &ModelTarget) -> Result<ResolvedRequest, LlmError> {
        let provider_id = target.provider.as_str();
        lattice_conversation::validate_provider_id(provider_id).map_err(configuration_error)?;
        validate_model_id(&target.model).map_err(configuration_error)?;
        let directory = self.configuration.directory.clone();
        let provider_id = provider_id.to_owned();
        let model_id = target.model.clone();
        let (provider, credentials, output_limit) =
            tokio::task::spawn_blocking(move || load_provider(directory, &provider_id, &model_id))
                .await
                .map_err(|_| configuration_error("provider configuration task failed"))??;
        let client = self
            .clients
            .lock()
            .unwrap()
            .entry((provider.id.clone(), provider.base_url.clone()))
            .or_default()
            .clone();
        client::validate_configuration(
            provider.protocol,
            provider.auth,
            credentials.api_key(&provider.id),
        )?;
        Ok(ResolvedRequest {
            configuration: self.configuration.clone(),
            provider,
            credentials: Arc::new(credentials),
            client,
            target: target.clone(),
            output_limit,
        })
    }

    #[cfg(test)]
    async fn stream_chat(
        &self,
        request: ChatRequest,
        control: StreamControl,
    ) -> Result<LlmStream, LlmError> {
        self.resolve(&request.target)
            .await?
            .stream_chat(request, control)
            .await
    }
}

/// 一次请求的解析结果；连接材料与秘密始终封装在网关中。
pub struct ResolvedRequest {
    configuration: Arc<Configuration>,
    provider: Provider,
    credentials: Arc<Credentials>,
    client: Arc<TurnClient>,
    target: ModelTarget,
    output_limit: Option<u64>,
}

impl ResolvedRequest {
    pub fn protocol(&self) -> crate::protocol::Protocol {
        self.provider.protocol
    }

    pub fn provider_id(&self) -> &str {
        &self.provider.id
    }

    /// 发送沿用解析时取得的配置；调用方不能替换模型或省去 SessionId。
    pub async fn stream_chat(
        &self,
        request: ChatRequest,
        control: StreamControl,
    ) -> Result<LlmStream, LlmError> {
        if request.target != self.target || request.client_request_id.is_none() {
            return Err(configuration_error(
                "resolved request requires its original target and a SessionId",
            ));
        }
        client::dispatch(
            request,
            RequestEndpoint {
                provider: &self.provider.id,
                model: &self.target.model,
                base_url: &self.provider.base_url,
                protocol: self.provider.protocol,
                auth: self.provider.auth,
                credentials: self.credentials.clone(),
                roots: &self.configuration.roots,
                client: self.client.clone(),
                output_limit: self.output_limit,
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

pub(crate) fn load_configuration(
    directory: &Path,
) -> Result<(ProviderRegistry, Result<Credentials, AuthError>), LlmError> {
    let credentials = AuthStore::new(directory.to_owned()).load();
    let registry =
        ProviderRegistry::load(directory.join("providers.toml")).map_err(configuration_error)?;
    Ok((registry, credentials))
}

fn load_provider(
    directory: PathBuf,
    id: &str,
    model_id: &str,
) -> Result<(Provider, Credentials, Option<u64>), LlmError> {
    let (registry, credentials) = load_configuration(&directory)?;
    let provider = registry
        .get(id)
        .cloned()
        .ok_or_else(|| missing_provider_error(&registry, id))?;
    let credentials = match credentials {
        Ok(credentials) => credentials,
        Err(_) if provider.auth == Auth::None => Credentials::default(),
        Err(error) => return Err(configuration_error(error)),
    };
    let output_limit = crate::metadata::Metadata::load(&directory)
        .merge(&provider)
        .get(model_id)
        .and_then(|model| model.limit.as_ref().and_then(|limit| limit.output));
    Ok((provider, credentials, output_limit))
}

pub(crate) fn missing_provider_error(registry: &ProviderRegistry, id: &str) -> LlmError {
    configuration_error(format!(
        "provider {id:?} is not registered; available providers: {}",
        registry
            .iter()
            .map(|provider| provider.id.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

#[cfg(test)]
mod tests;

// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 每次请求重读 provider 与凭据，Turn 句柄封装地址固定和连接复用。

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use crate::auth::{AuthError, AuthStore, ChatgptAuth, Credentials};
use crate::client::{self, RequestEndpoint, TurnClient};
use crate::provider::{Auth, Provider, config::ProviderRegistry};
use crate::reasoning::{ReasoningCapabilities, ReasoningPlan};
use crate::{ChatRequest, LlmError, LlmStream, ModelTarget, ProviderId, StreamControl};

mod cost;

struct Configuration {
    directory: PathBuf,
    roots: Vec<reqwest::Certificate>,
    roots_der: Vec<Vec<u8>>,
    chatgpt_auth: ChatgptAuth,
    #[cfg(test)]
    metadata_url: Option<String>,
}

impl Configuration {
    /// 选根之后是否还有可用的根证书；网关各处的“没有可用的根证书”都由这里判定。
    fn has_usable_roots(&self) -> bool {
        !self.roots_der.is_empty()
    }
}

/// 持有配置目录与给定的 TLS 根，按 Turn 创建连接句柄。
pub struct RequestGateway {
    configuration: Arc<Configuration>,
}

/// 没有可用根证书时对 https 端点的拒绝说明；http 端点不受影响。
const NO_USABLE_TLS_ROOTS: &str = "no usable TLS root certificates: https provider endpoints are rejected, http endpoints are allowed";

impl RequestGateway {
    /// 选根之后集合为空也照常构造：此时记为“没有可用的根证书”，只拒绝依赖 TLS 的请求。
    pub fn new(config_dir: PathBuf, roots_der: Vec<Vec<u8>>) -> Result<Self, LlmError> {
        let (roots_der, roots) = client::load_bounded_roots(roots_der);
        let chatgpt_auth = ChatgptAuth::new(AuthStore::new(config_dir.clone()), roots.clone());
        Ok(Self {
            configuration: Arc::new(Configuration {
                directory: config_dir,
                roots,
                roots_der,
                chatgpt_auth,
                #[cfg(test)]
                metadata_url: None,
            }),
        })
    }

    /// 选根之后是否还有可用的根证书（元数据刷新据此拒绝；`GatewayTurn::resolve` 用同一判定）。
    pub fn has_usable_tls_roots(&self) -> bool {
        self.configuration.has_usable_roots()
    }

    /// Tool Host 使用同一份启动时信任根，每个 Turn 独立构造信任配置。
    pub fn tls_roots_der(&self) -> &[Vec<u8>] {
        &self.configuration.roots_der
    }

    pub fn auth_store(&self) -> AuthStore {
        AuthStore::new(self.configuration.directory.clone())
    }

    /// 后台凭据命令与所有 Turn 共用同一个 SIWC 生命周期。
    pub fn chatgpt_auth(&self) -> ChatgptAuth {
        self.configuration.chatgpt_auth.clone()
    }

    /// 仅从本地读取登录与缓存状态，不使用或刷新 token。
    pub fn chatgpt_catalog_state(
        &self,
    ) -> Result<crate::ChatgptCatalogState, crate::MetadataError> {
        crate::metadata::chatgpt::state(&self.configuration.directory)
    }

    pub async fn refresh_chatgpt_models(
        &self,
        cancel: Arc<crate::MetadataRefreshControl>,
    ) -> Result<usize, crate::MetadataError> {
        if !self.has_usable_tls_roots() {
            return Err(crate::MetadataError::Configuration(
                "no usable TLS root certificates".into(),
            ));
        }
        crate::metadata::chatgpt::refresh(
            &self.configuration.directory,
            &self.configuration.chatgpt_auth,
            &self.configuration.roots_der,
            cancel,
        )
        .await
    }

    /// 刷新只读取当前注册表，固定使用正式元数据源与启动时的信任根。
    pub async fn refresh_metadata(
        &self,
        cancel: Arc<crate::MetadataRefreshControl>,
    ) -> Result<(), crate::MetadataError> {
        if !self.has_usable_tls_roots() {
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
    clients: Mutex<BTreeMap<(ProviderId, String), Arc<TurnClient>>>,
}

impl GatewayTurn {
    pub fn auth_store(&self) -> AuthStore {
        AuthStore::new(self.configuration.directory.clone())
    }

    /// 只读当前 provider 与模型元数据，返回按强度排序的可表达档位；不要求凭据。
    /// target 的 effort 不参与能力查询，发送前仍须 resolve 最终选择。
    pub async fn supported_efforts(
        &self,
        target: &ModelTarget,
    ) -> Result<Vec<lattice_conversation::Effort>, LlmError> {
        let directory = self.configuration.directory.clone();
        let target = target.clone();
        tokio::task::spawn_blocking(move || {
            let registry = ProviderRegistry::load(directory.join("providers.toml"))
                .map_err(configuration_error)?;
            let provider_id = target.model.provider().as_str();
            let model_id = target.model.model().as_str();
            let provider = registry
                .get(provider_id)
                .ok_or_else(|| missing_provider_error(&registry, provider_id))?;
            let models = crate::metadata::Metadata::load(&directory).merge(provider);
            crate::reasoning::supported_efforts(provider, model_id, models.get(model_id))
        })
        .await
        .map_err(|_| configuration_error("model capabilities task failed"))?
    }

    /// 每次解析读取当前配置与凭据；发送句柄独占这一次解析结果。
    pub async fn resolve(&self, target: &ModelTarget) -> Result<ResolvedRequest, LlmError> {
        let directory = self.configuration.directory.clone();
        let model_ref = target.model.clone();
        let effort = target.effort;
        let LoadedProvider {
            provider,
            credentials,
            output_limit,
            model_reasoning,
            reasoning,
        } = tokio::task::spawn_blocking(move || {
            load_provider(
                directory,
                model_ref.provider().as_str(),
                model_ref.model().as_str(),
                effort,
            )
        })
        .await
        .map_err(|_| configuration_error("provider configuration task failed"))??;
        // 没有可用根证书时 https 端点按配置错误拒绝，不回退平台根；http 端点照常。
        if !self.configuration.has_usable_roots() {
            require_endpoint_without_tls_roots(&provider.base_url)?;
        }
        let client = self
            .clients
            .lock()
            .unwrap()
            .entry((provider.id.clone(), provider.base_url.clone()))
            .or_default()
            .clone();
        client::validate_configuration(provider.auth, provider.id.as_str(), &credentials)?;
        Ok(ResolvedRequest {
            configuration: self.configuration.clone(),
            provider,
            credentials: Arc::new(credentials),
            client,
            target: target.clone(),
            output_limit,
            model_reasoning,
            reasoning,
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
    model_reasoning: Option<bool>,
    reasoning: Option<ReasoningPlan>,
}

impl ResolvedRequest {
    pub fn origin(&self) -> lattice_conversation::ReplayOrigin {
        lattice_conversation::ReplayOrigin {
            protocol: self.provider.protocol,
            provider_id: self.provider.id.clone(),
        }
    }

    pub fn protocol(&self) -> crate::protocol::Protocol {
        self.provider.protocol
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
        let input = client::dispatch(
            request,
            RequestEndpoint {
                provider: &self.provider.id,
                model: self.target.model.model(),
                base_url: &self.provider.base_url,
                protocol: self.provider.protocol,
                auth: self.provider.auth,
                credentials: self.credentials.clone(),
                chatgpt_auth: &self.configuration.chatgpt_auth,
                roots: &self.configuration.roots,
                client: self.client.clone(),
                output_limit: self.output_limit,
                model_reasoning: self.model_reasoning,
                reasoning: self.reasoning,
                features: self
                    .provider
                    .models
                    .get(self.target.model.model())
                    .map_or(self.provider.features, |model| model.features),
            },
            control,
        )
        .await?;
        Ok(cost::with_estimated_cost(
            input,
            self.configuration.directory.clone(),
            self.target.clone(),
        ))
    }
}

/// 没有可用根证书时只放行能解析且 scheme 为 http 的端点，其余一律按配置错误拒绝（失败即关闭）。
///
/// provider 载入时已用 `net_guard::validate_base_url_static` 校验 `base_url`（能解析、只允许
/// http/https），按现有代码路径这里不会遇到无法解析的地址；仍按失败即关闭处理，避免日后载入
/// 规则变化时把无法解析的地址当成 http 放行。
fn require_endpoint_without_tls_roots(base_url: &str) -> Result<(), LlmError> {
    match url::Url::parse(base_url) {
        Ok(url) if url.scheme() == "http" => Ok(()),
        Ok(_) => Err(configuration_error(NO_USABLE_TLS_ROOTS)),
        Err(error) => Err(configuration_error(format!(
            "invalid provider endpoint URL ({error}); {NO_USABLE_TLS_ROOTS}"
        ))),
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

struct LoadedProvider {
    provider: Provider,
    credentials: Credentials,
    output_limit: Option<u64>,
    model_reasoning: Option<bool>,
    reasoning: Option<ReasoningPlan>,
}

fn load_provider(
    directory: PathBuf,
    id: &str,
    model_id: &str,
    effort: Option<lattice_conversation::Effort>,
) -> Result<LoadedProvider, LlmError> {
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
    let models = crate::metadata::Metadata::load(&directory).merge(&provider);
    let model = models.get(model_id);
    let limits = model.and_then(|model| model.limit.as_ref());
    let output_limit = limits.and_then(|limit| limit.output);
    let features = provider
        .models
        .get(model_id)
        .map_or(provider.features, |model| model.features);
    let capabilities = ReasoningCapabilities::new(id, model_id, model, features)?;
    let reasoning = effort.map(|effort| {
        capabilities.select(effort).ok_or_else(|| configuration_error(format!(
            "model {id}/{model_id} does not support reasoning effort {effort} with its current metadata and features"
        )))
    }).transpose()?;
    Ok(LoadedProvider {
        provider,
        credentials,
        output_limit,
        model_reasoning: model.and_then(|model| model.reasoning),
        reasoning,
    })
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

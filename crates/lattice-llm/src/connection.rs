// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use crate::{
    ChatRequest, CredentialStatus, LlmError, LlmStream, ModelRouter, ModelTarget, ResolvedModel,
};
use zeroize::Zeroizing;

/// 单次提交冻结的连接材料。刻意不实现 Debug/serde，凭据和信任根不进入持久配置。
#[derive(Clone)]
pub struct LlmConnection {
    pub(crate) resolved: ResolvedModel,
    pub(crate) api_key: Zeroizing<String>,
    pub(crate) roots: Vec<reqwest::Certificate>,
    roots_der: Vec<Vec<u8>>,
}

impl LlmConnection {
    /// 显式材料入口；调用者传入证书内容，后台不再读取提交端路径或共享环境。
    pub fn new(
        mut resolved: ResolvedModel,
        api_key: String,
        roots_der: Vec<Vec<u8>>,
    ) -> Result<Self, LlmError> {
        let api_key = Zeroizing::new(api_key);
        if api_key.len() > 16_384
            || resolved.alias.len() > 4096
            || resolved.provider_model.len() > 4096
            || resolved.base_url.len() > 8192
            || roots_der.len() > 1024
            || roots_der.iter().any(|root| root.len() > 65_536)
            || roots_der.iter().map(Vec::len).sum::<usize>() > 4 * 1024 * 1024
        {
            return Err(LlmError::Config {
                message: "connection material exceeds limit".into(),
            });
        }
        if api_key.is_empty() || api_key.bytes().any(|b| b < 32 || b == 127) {
            return Err(LlmError::Credential {
                message: "API key is empty or contains control bytes".into(),
            });
        }
        let url = url::Url::parse(&resolved.base_url).map_err(|_| LlmError::Config {
            message: "invalid provider base URL".into(),
        })?;
        if !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(LlmError::Config {
                message: "provider base URL must not contain credentials, query or fragment".into(),
            });
        }
        net_guard::validate_base_url(&resolved.base_url).map_err(|_| LlmError::Config {
            message: "provider base URL rejected by network policy".into(),
        })?;
        let roots = roots_der
            .iter()
            .map(|der| {
                reqwest::Certificate::from_der(der).map_err(|_| LlmError::Config {
                    message: "invalid TLS root certificate".into(),
                })
            })
            .collect::<Result<_, _>>()?;
        resolved.credential_status = CredentialStatus::Present;
        Ok(Self {
            resolved,
            api_key,
            roots,
            roots_der,
        })
    }

    /// 仅提交端调用：保留 router 的环境优先级，并一次读取实际启用的 Linux TLS 信任根。
    pub fn from_env(target: &ModelTarget) -> Result<Self, LlmError> {
        Self::from_resolved_env(ModelRouter::new().resolve(target)?)
    }

    pub(crate) fn from_resolved_env(resolved: ResolvedModel) -> Result<Self, LlmError> {
        let key = crate::client::validate_and_read_key(&resolved)?;
        let roots = rustls_native_certs::load_native_certs();
        // 与平台 verifier 一样，部分可用根可继续；完全无法建立 HTTPS 信任时 fail closed。
        if resolved.base_url.starts_with("https:") && roots.certs.is_empty() {
            return Err(LlmError::Config {
                message: "no usable TLS root certificates".into(),
            });
        }
        Self::new(
            resolved,
            key,
            roots
                .certs
                .into_iter()
                .map(|cert| cert.as_ref().to_vec())
                .collect(),
        )
    }

    /// 同一次提交的 Tool Host 复用这些根；空集合拒绝全部 TLS 证书，不回退宿主信任库。
    pub fn tls_roots_der(&self) -> &[Vec<u8>] {
        &self.roots_der
    }

    pub fn model(&self) -> &ResolvedModel {
        &self.resolved
    }

    pub async fn stream(&self, request: ChatRequest) -> Result<LlmStream, LlmError> {
        if request.target.model != self.resolved.alias {
            return Err(LlmError::Config {
                message: "request model differs from frozen connection".into(),
            });
        }
        crate::client::dispatch_frozen(request, self).await
    }
}

/// 仅供已验证本地连接传输的秘密材料；不实现 Debug，不写入持久快照。
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionMaterial {
    alias: String,
    provider: String,
    provider_model: String,
    base_url: String,
    api_key: String,
    roots_der: Vec<Vec<u8>>,
}

impl Drop for ConnectionMaterial {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.api_key.zeroize();
    }
}

impl LlmConnection {
    pub fn transport_material(&self) -> ConnectionMaterial {
        ConnectionMaterial {
            alias: self.resolved.alias.clone(),
            provider: self.resolved.provider.as_str().into(),
            provider_model: self.resolved.provider_model.clone(),
            base_url: self.resolved.base_url.clone(),
            api_key: self.api_key.to_string(),
            roots_der: self.roots_der.clone(),
        }
    }

    pub fn from_transport(mut material: ConnectionMaterial) -> Result<Self, LlmError> {
        let provider = match material.provider.as_str() {
            "openai" => crate::ProviderKind::OpenAi,
            "openai_compatible" => crate::ProviderKind::OpenAiCompatible,
            "anthropic" => crate::ProviderKind::Anthropic,
            "gemini" => crate::ProviderKind::Gemini,
            _ => {
                return Err(LlmError::Config {
                    message: "unknown connection provider".into(),
                });
            }
        };
        Self::new(
            ResolvedModel {
                alias: std::mem::take(&mut material.alias),
                provider,
                provider_model: std::mem::take(&mut material.provider_model),
                base_url: std::mem::take(&mut material.base_url),
                api_key_env: String::new(),
                credential_status: CredentialStatus::Present,
            },
            std::mem::take(&mut material.api_key),
            std::mem::take(&mut material.roots_der),
        )
    }
}

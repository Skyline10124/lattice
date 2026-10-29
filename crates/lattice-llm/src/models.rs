// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{LlmError, auth::AuthState, metadata::Metadata};

/// 候选只包含模型元数据与凭据状态，连接配置和秘密不跨过后台边界。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ModelCandidate {
    pub provider: String,
    pub model: String,
    pub name: Option<String>,
    pub context: Option<u64>,
    pub output: Option<u64>,
    pub cost: Option<ModelPrice>,
    pub credential_status: AuthState,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ModelPrice {
    pub input: Option<f64>,
    pub output: Option<f64>,
    pub cache_read: Option<f64>,
    pub cache_write: Option<f64>,
    pub reasoning: Option<f64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelWarning {
    pub provider: Option<String>,
    pub message: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ModelCandidates {
    pub models: Vec<ModelCandidate>,
    pub warnings: Vec<ModelWarning>,
}

/// 查询只读取当前注册表、元数据与凭据状态，不发起网络请求。
pub fn model_candidates(
    directory: &Path,
    selected: Option<&str>,
) -> Result<ModelCandidates, LlmError> {
    let (registry, credentials) = crate::gateway::load_configuration(directory)?;
    if let Some(id) = selected
        && registry.get(id).is_none()
    {
        return Err(crate::gateway::missing_provider_error(&registry, id));
    }
    let metadata = Metadata::load(directory);
    let mut result = ModelCandidates::default();
    if let Err(error) = &credentials {
        result.warnings.push(ModelWarning {
            provider: None,
            message: format!("provider credentials are not ready: {error}"),
        });
    }
    for provider in registry
        .iter()
        .filter(|provider| selected.is_none_or(|id| id == provider.id))
    {
        let mut warn = |message: String| {
            result.warnings.push(ModelWarning {
                provider: Some(provider.id.clone()),
                message,
            })
        };
        if provider.id == "chatgpt" {
            warn("ChatGPT account model listing is not implemented".into());
            continue;
        }
        if let Some(catalog) = &provider.catalog
            && !metadata.has_catalog(catalog)
        {
            warn(format!(
                "catalog {catalog:?} has no metadata; only handwritten models are available"
            ));
        }
        let status = crate::auth::provider_status(provider, credentials.as_ref()).status;
        for (id, model) in metadata.merge(provider) {
            if model.status.as_deref() == Some("deprecated") {
                continue;
            }
            result.models.push(ModelCandidate {
                provider: provider.id.clone(),
                model: id,
                name: model.name,
                context: model.limit.as_ref().and_then(|limit| limit.context),
                output: model.limit.as_ref().and_then(|limit| limit.output),
                cost: model.cost.map(|cost| ModelPrice {
                    input: cost.input,
                    output: cost.output,
                    cache_read: cost.cache_read,
                    cache_write: cost.cache_write,
                    reasoning: cost.reasoning,
                }),
                credential_status: status,
            });
        }
    }
    Ok(result)
}

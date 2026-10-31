// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::path::Path;

use lattice_conversation::Effort;
use serde::{Deserialize, Serialize};

use crate::{
    LlmError, ModelRef,
    auth::AuthState,
    metadata::{Metadata, Model},
    provider::Provider,
    reasoning::ReasoningCapabilities,
};

/// 候选只包含模型元数据与凭据状态，连接配置和秘密不跨过后台边界。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ModelCandidate {
    pub provider: String,
    pub model: String,
    pub name: Option<String>,
    pub context: Option<u64>,
    pub output: Option<u64>,
    pub supported_efforts: Vec<Effort>,
    pub cost: Option<ModelPrice>,
    pub credential_status: AuthState,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ModelPrice {
    pub input: Option<f64>,
    pub output: Option<f64>,
    pub cache_read: Option<f64>,
    pub cache_write: Option<f64>,
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
    target: Option<&str>,
) -> Result<ModelCandidates, LlmError> {
    if selected.is_some() && target.is_some() {
        return Err(LlmError::Config {
            message: "provider and target cannot be used together".into(),
        });
    }
    let target = target
        .map(ModelRef::parse)
        .transpose()
        .map_err(|error| LlmError::Config {
            message: error.to_string(),
        })?;
    let selected = target.as_ref().map(ModelRef::provider).or(selected);
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
        .iter_for_candidates()
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
            if target.is_none() {
                continue;
            }
        }
        if let Some(catalog) = &provider.catalog
            && !metadata.has_catalog(catalog)
        {
            warn(format!(
                "catalog {catalog:?} has no metadata; only handwritten models are available"
            ));
        }
        let status = crate::auth::provider_status(provider, credentials.as_ref()).status;
        let models = metadata.merge(provider);
        if let Some(target) = &target {
            result.models.push(candidate(
                provider,
                target.model(),
                models.get(target.model()),
                status,
            ));
        } else {
            for (id, model) in &models {
                if model.status.as_deref() != Some("deprecated") {
                    result
                        .models
                        .push(candidate(provider, id, Some(model), status));
                }
            }
        }
    }
    Ok(result)
}

fn candidate(
    provider: &Provider,
    id: &str,
    model: Option<&Model>,
    status: AuthState,
) -> ModelCandidate {
    let features = provider
        .models
        .get(id)
        .map_or(provider.features, |model| model.features);
    let limits = model.and_then(|model| model.limit.as_ref());
    ModelCandidate {
        provider: provider.id.clone(),
        model: id.into(),
        name: model.and_then(|model| model.name.clone()),
        context: limits.and_then(|limit| limit.context),
        output: limits.and_then(|limit| limit.output),
        supported_efforts: ReasoningCapabilities::new(model, features).supported_efforts(),
        cost: model
            .and_then(|model| model.cost.as_ref())
            .map(|cost| ModelPrice {
                input: cost.input,
                output: cost.output,
                cache_read: cost.cache_read,
                cache_write: cost.cache_write,
            }),
        credential_status: status,
    }
}

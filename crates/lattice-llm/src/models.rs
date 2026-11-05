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
    reasoning::supported_efforts,
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
    /// 手写配置对该模型无效时的诊断；此时没有可选档位，其他模型照常列出。
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub config_error: Option<String>,
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
    let selected = target
        .as_ref()
        .map(|target| target.provider().as_str())
        .or(selected);
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
        .filter(|provider| selected.is_none_or(|id| provider.id == id))
    {
        let mut warn = |message: String| {
            result.warnings.push(ModelWarning {
                provider: Some(provider.id.as_str().to_owned()),
                message,
            })
        };
        if provider.id == "chatgpt" {
            let catalog = crate::metadata::chatgpt::load(directory);
            // 未登录时不请求账号目录，也不提示；已经登录却缺目录时才说明原因。
            if let Err(error) = &catalog
                && credentials
                    .as_ref()
                    .is_ok_and(|credentials| credentials.chatgpt_logged_in())
            {
                warn(format!(
                    "ChatGPT account model directory is unavailable: {error}"
                ));
            }
            let status = crate::auth::provider_status(provider, credentials.as_ref()).status;
            let models = metadata.merge(provider);
            if let Some(target) = &target {
                let model_id = target.model().as_str();
                let mut model = candidate(provider, model_id, models.get(model_id), status);
                if let Some(name) = catalog
                    .as_ref()
                    .ok()
                    .and_then(|catalog| catalog.models.get(model_id))
                {
                    model.name = Some(name.clone());
                }
                result.models.push(model);
            } else if let Ok(catalog) = catalog {
                for (id, name) in catalog.models {
                    let model = models.get(&id);
                    if model.is_some_and(|model| model.status.as_deref() == Some("deprecated")) {
                        continue;
                    }
                    let mut model = candidate(provider, &id, model, status);
                    model.name = Some(name);
                    result.models.push(model);
                }
            }
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
        let models = metadata.merge(provider);
        if let Some(target) = &target {
            let model_id = target.model().as_str();
            result
                .models
                .push(candidate(provider, model_id, models.get(model_id), status));
        } else {
            // 先滤掉已弃用模型再校验，隐藏的模型不因配置问题出错。
            for (id, model) in models
                .iter()
                .filter(|(_, model)| model.status.as_deref() != Some("deprecated"))
            {
                result
                    .models
                    .push(candidate(provider, id, Some(model), status));
            }
        }
    }
    Ok(result)
}

/// 档位校验只拒绝出错的那个模型：保留候选并带上诊断，请求路径仍按所选模型拒绝。
fn candidate(
    provider: &Provider,
    id: &str,
    model: Option<&Model>,
    status: AuthState,
) -> ModelCandidate {
    let limits = model.and_then(|model| model.limit.as_ref());
    let (supported_efforts, config_error) = match supported_efforts(provider, id, model) {
        Ok(efforts) => (efforts, None),
        Err(error) => (Vec::new(), Some(error.to_string())),
    };
    ModelCandidate {
        provider: provider.id.as_str().to_owned(),
        model: id.into(),
        name: model.and_then(|model| model.name.clone()),
        context: limits.and_then(|limit| limit.context),
        output: limits.and_then(|limit| limit.output),
        supported_efforts,
        cost: model
            .and_then(|model| model.cost.as_ref())
            .map(|cost| ModelPrice {
                input: cost.input,
                output: cost.output,
                cache_read: cost.cache_read,
                cache_write: cost.cache_write,
            }),
        credential_status: status,
        config_error,
    }
}

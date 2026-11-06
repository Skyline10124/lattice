// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::path::Path;

use lattice_conversation::Effort;
use serde::{Deserialize, Serialize};

use crate::{
    Availability, LlmError, ModelId, ModelRef, ProviderId,
    auth::{AuthError, Credentials},
    metadata::{Metadata, Model},
    provider::Provider,
    reasoning::supported_efforts,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ModelCandidateQuery {
    List {
        provider: Option<ProviderId>,
        ready_only: bool,
    },
    Target(ModelRef),
}

/// 候选只包含模型元数据与可用性状态，连接配置和秘密不跨过后台边界。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ModelCandidate {
    pub provider: ProviderId,
    pub model: ModelId,
    pub name: Option<String>,
    pub context: Option<u64>,
    pub output: Option<u64>,
    pub supported_efforts: Vec<Effort>,
    pub cost: Option<ModelPrice>,
    pub availability: Availability,
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
    query: &ModelCandidateQuery,
) -> Result<ModelCandidates, LlmError> {
    let providers_path = directory.join("providers.toml");
    let (registry, credentials) = crate::gateway::load_configuration(directory)?;
    let selected = match query {
        ModelCandidateQuery::List { provider, .. } => provider.as_ref(),
        ModelCandidateQuery::Target(target) => Some(target.provider()),
    };
    if let Some(id) = selected
        && registry.get(id.as_str()).is_none()
    {
        return Err(crate::gateway::missing_provider_error(
            &registry,
            id.as_str(),
            &providers_path,
        ));
    }
    let metadata = Metadata::load(directory);
    let mut result = ModelCandidates::default();
    for provider in registry
        .iter_for_candidates()
        .filter(|provider| selected.is_none_or(|id| &provider.id == id))
    {
        if matches!(
            query,
            ModelCandidateQuery::List {
                ready_only: true,
                ..
            }
        ) && !crate::availability::provider_availability(provider, credentials.as_ref())
            .is_ready()
        {
            continue;
        }
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
            let models = metadata.merge(provider);
            match query {
                ModelCandidateQuery::Target(target) => {
                    let model_id = target.model();
                    let mut model = candidate(
                        provider,
                        model_id.clone(),
                        models.get(model_id.as_str()),
                        credentials.as_ref(),
                    );
                    if let Some(name) = catalog
                        .as_ref()
                        .ok()
                        .and_then(|catalog| catalog.models.get(model_id.as_str()))
                    {
                        model.name = Some(name.clone());
                    }
                    result.models.push(model);
                }
                ModelCandidateQuery::List { ready_only, .. } => {
                    if let Ok(catalog) = catalog {
                        for (id, name) in catalog.models {
                            let Ok(model_id) = ModelId::parse(&id) else {
                                continue;
                            };
                            let model = models.get(&id);
                            if model
                                .is_some_and(|model| model.status.as_deref() == Some("deprecated"))
                            {
                                continue;
                            }
                            let mut model =
                                candidate(provider, model_id, model, credentials.as_ref());
                            if *ready_only && !model.availability.is_ready() {
                                continue;
                            }
                            model.name = Some(name);
                            result.models.push(model);
                        }
                    }
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
        let models = metadata.merge(provider);
        match query {
            ModelCandidateQuery::Target(target) => {
                let model_id = target.model();
                result.models.push(candidate(
                    provider,
                    model_id.clone(),
                    models.get(model_id.as_str()),
                    credentials.as_ref(),
                ));
            }
            ModelCandidateQuery::List { ready_only, .. } => {
                // 先滤掉已弃用模型再校验，隐藏的模型不因配置问题出错。
                for (id, model) in models
                    .iter()
                    .filter(|(_, model)| model.status.as_deref() != Some("deprecated"))
                {
                    let Ok(model_id) = ModelId::parse(id) else {
                        continue;
                    };
                    let model = candidate(provider, model_id, Some(model), credentials.as_ref());
                    if *ready_only && !model.availability.is_ready() {
                        continue;
                    }
                    result.models.push(model);
                }
            }
        }
    }
    Ok(result)
}

/// 档位校验只拒绝出错的那个模型：保留候选并带上可用性状态，请求路径仍按所选模型拒绝。
fn candidate(
    provider: &Provider,
    id: ModelId,
    model: Option<&Model>,
    credentials: Result<&Credentials, &AuthError>,
) -> ModelCandidate {
    let limits = model.and_then(|model| model.limit.as_ref());
    let efforts_result = supported_efforts(provider, id.as_str(), model);
    let supported_efforts = efforts_result.as_ref().cloned().unwrap_or_default();
    let (availability, _) =
        crate::availability::model_availability(provider, credentials, || efforts_result);
    ModelCandidate {
        provider: provider.id.clone(),
        model: id,
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
        availability,
    }
}

// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::path::Path;

use lattice_conversation::Effort;
use serde::{Deserialize, Serialize};

use crate::{
    Availability, LlmError, ModelId, ModelRef, ProviderId,
    auth::{AuthError, Credentials},
    metadata::{Metadata, Model},
    provider::{Provider, config::ProviderRegistry},
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

/// 只读取本地 `providers.toml` 与模型元数据，不读取凭据、不联网；
/// 配置错误如实返回 `Err`。供后台的 `configure` 判断模型支持的推理档位。
pub fn model_supported_efforts(
    directory: &Path,
    model: &ModelRef,
) -> Result<Vec<Effort>, LlmError> {
    let providers_path = directory.join("providers.toml");
    let registry =
        ProviderRegistry::load(&providers_path).map_err(crate::gateway::configuration_error)?;
    let provider_id = model.provider().as_str();
    let Some(provider) = registry.get(provider_id) else {
        return Err(crate::gateway::missing_provider_error(
            &registry,
            provider_id,
            &providers_path,
        ));
    };
    let models = Metadata::load(directory).merge(provider);
    let model_id = model.model().as_str();
    supported_efforts(provider, model_id, models.get(model_id))
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

#[cfg(test)]
pub(crate) mod test_support {
    pub(crate) fn list_query(
        provider: Option<&str>,
        ready_only: bool,
    ) -> super::ModelCandidateQuery {
        super::ModelCandidateQuery::List {
            provider: provider.map(|id| crate::ProviderId::parse(id).unwrap()),
            ready_only,
        }
    }

    pub(crate) fn target_query(target: &str) -> super::ModelCandidateQuery {
        super::ModelCandidateQuery::Target(crate::ModelRef::parse(target).unwrap())
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt, path::Path};

    use lattice_conversation::Effort;
    use serde_json::json;

    fn private_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        dir
    }

    fn write_providers(dir: &Path, content: &str) {
        fs::write(dir.join("providers.toml"), content).unwrap();
    }

    fn write_cache(dir: &Path, source: &serde_json::Value) {
        fs::create_dir_all(dir.join("cache")).unwrap();
        fs::write(
            dir.join("cache/models.json"),
            serde_json::to_vec(source).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn supported_efforts_ignores_credentials_and_reloads_providers_config() {
        let dir = private_dir();
        let base =
            "[providers.test]\nprotocol = 'chat-completions'\nbase_url = 'http://localhost:1/v1'\n";
        write_providers(
            dir.path(),
            &format!("{base}[providers.test.models.m]\neffort = ['high', 'off', 'low', 'high']\n"),
        );
        let model = crate::ModelRef::parse("test/m").unwrap();

        let efforts = crate::model_supported_efforts(dir.path(), &model)
            .expect("未配置凭据时也应按手写元数据返回档位");
        assert_eq!(
            efforts,
            vec![Effort::Off, Effort::Low, Effort::High],
            "返回档位应按 Effort 顺序排列并去重"
        );

        let auth_path = dir.path().join("auth.json");
        fs::write(&auth_path, b"{corrupted").unwrap();
        fs::set_permissions(&auth_path, fs::Permissions::from_mode(0o600)).unwrap();
        let efforts_with_corrupted_auth = crate::model_supported_efforts(dir.path(), &model)
            .expect("损坏的 auth.json 不应影响档位查询");
        assert_eq!(
            efforts_with_corrupted_auth,
            vec![Effort::Off, Effort::Low, Effort::High],
            "写入损坏的 auth.json 后档位结果应保持不变"
        );

        write_providers(
            dir.path(),
            &format!("{base}[providers.test.models.m]\neffort = ['medium']\n"),
        );
        let updated_efforts = crate::model_supported_efforts(dir.path(), &model)
            .expect("修改 providers.toml 后应读取最新配置");
        assert_eq!(
            updated_efforts,
            vec![Effort::Medium],
            "修改手写 effort 后应返回更新后的档位"
        );
    }

    #[test]
    fn supported_efforts_rejects_unregistered_provider_with_missing_provider_message() {
        let dir = private_dir();
        let providers_path = dir.path().join("providers.toml");
        let model = crate::ModelRef::parse("missing/m").unwrap();

        let error = crate::model_supported_efforts(dir.path(), &model)
            .expect_err("未注册的 provider 应返回配置错误");
        let registry = super::ProviderRegistry::load(&providers_path).unwrap();
        let expected =
            crate::gateway::missing_provider_error(&registry, "missing", &providers_path);
        assert_eq!(
            error, expected,
            "未注册 provider 的错误消息应与 missing_provider_error 完全相等"
        );
    }

    #[test]
    fn supported_efforts_resolves_undeclared_builtin_provider() {
        let dir = private_dir();
        write_cache(
            dir.path(),
            &json!({
                "lmstudio": {
                    "models": {
                        "m": {
                            "id": "m",
                            "reasoning_options": [
                                {"type": "effort", "values": ["low", "high"]}
                            ]
                        }
                    }
                }
            }),
        );
        let model = crate::ModelRef::parse("lmstudio/m").unwrap();

        let efforts = crate::model_supported_efforts(dir.path(), &model)
            .expect("未在 providers.toml 声明的内置 provider 也应照常计算档位");
        assert_eq!(
            efforts,
            vec![Effort::Low, Effort::High],
            "未声明的内置 lmstudio 模型应返回缓存中的档位"
        );
    }

    #[test]
    fn supported_efforts_surfaces_feature_mismatch_without_credentials() {
        let dir = private_dir();
        write_providers(
            dir.path(),
            "[providers.test]\nprotocol = 'chat-completions'\nbase_url = 'http://localhost:1/v1'\n[providers.test.features]\nthinking_toggle = 'enable-thinking'\n[providers.test.models.m]\neffort = ['low']\n",
        );
        let model = crate::ModelRef::parse("test/m").unwrap();

        let error = crate::model_supported_efforts(dir.path(), &model)
            .expect_err("无法用当前 features 表达的手写档位应在无凭据时直接报错");
        let crate::LlmError::Config { message } = error else {
            panic!("预期返回 LlmError::Config，实际为 {error:?}");
        };
        for needle in [
            "provider \"test\"",
            "model \"m\"",
            "effort low",
            "thinking_toggle",
        ] {
            assert!(
                message.contains(needle),
                "错误消息应包含 {needle:?}，实际消息为：{message}"
            );
        }

        let candidates =
            crate::model_candidates(dir.path(), &crate::ModelCandidateQuery::Target(model))
                .expect("model_candidates 在无凭据时不应因手写档位错误而整体失败");
        assert_eq!(candidates.models.len(), 1, "Target 查询应返回单个模型候选");
        assert_eq!(
            candidates.models[0].availability,
            crate::Availability::Unconfigured {
                requirement: crate::ConfigurationRequirement::ApiKey,
            },
            "对照候选在未配置 API key 时应标为 Unconfigured"
        );
        assert!(
            candidates.models[0].supported_efforts.is_empty(),
            "对照候选在档位推导失败时应将 supported_efforts 置为空列表"
        );
    }

    #[test]
    fn supported_efforts_keeps_deprecated_models_and_returns_empty_for_unknown_model() {
        let dir = private_dir();
        write_providers(
            dir.path(),
            "[providers.test]\nprotocol = 'chat-completions'\nbase_url = 'http://localhost:1/v1'\ncatalog = 'test-catalog'\n",
        );
        write_cache(
            dir.path(),
            &json!({
                "test-catalog": {
                    "models": {
                        "m": {
                            "id": "m",
                            "status": "deprecated",
                            "reasoning_options": [
                                {"type": "effort", "values": ["low", "high"]}
                            ]
                        }
                    }
                }
            }),
        );

        let deprecated = crate::ModelRef::parse("test/m").unwrap();
        let efforts = crate::model_supported_efforts(dir.path(), &deprecated)
            .expect("deprecated 模型不应被过滤或报错");
        assert_eq!(
            efforts,
            vec![Effort::Low, Effort::High],
            "deprecated 模型应照常返回元数据中的推理档位"
        );

        let unknown = crate::ModelRef::parse("test/org/unknown-model").unwrap();
        let unknown_efforts = crate::model_supported_efforts(dir.path(), &unknown)
            .expect("无元数据的未知模型应返回空档位列表而非错误");
        assert_eq!(
            unknown_efforts,
            Vec::<Effort>::new(),
            "无元数据的目标模型应返回空档位列表"
        );
    }

    #[test]
    fn supported_efforts_resolves_chatgpt_from_openai_catalog_and_handwritten_override() {
        let dir = private_dir();
        write_cache(
            dir.path(),
            &json!({
                "openai": {
                    "models": {
                        "m": {
                            "id": "m",
                            "reasoning_options": [
                                {"type": "effort", "values": ["low", "high"]}
                            ]
                        }
                    }
                }
            }),
        );
        let model = crate::ModelRef::parse("chatgpt/m").unwrap();

        let efforts = crate::model_supported_efforts(dir.path(), &model)
            .expect("chatgpt 未登录且无账号目录时应按 openai 元数据计算档位");
        assert_eq!(
            efforts,
            vec![Effort::Low, Effort::High],
            "chatgpt 模型应读取 openai catalog 中的推理档位"
        );

        write_providers(
            dir.path(),
            "[providers.chatgpt.models.m]\neffort = ['low']\n",
        );
        let overridden_efforts = crate::model_supported_efforts(dir.path(), &model)
            .expect("chatgpt 叠加手写模型字段后应返回新的档位列表");
        assert_eq!(
            overridden_efforts,
            vec![Effort::Low],
            "providers.toml 中对 chatgpt 模型的手写 effort 应覆盖候选档位"
        );
    }

    #[test]
    fn supported_efforts_rejects_invalid_providers_toml_with_file_path() {
        let dir = private_dir();
        let providers_path = dir.path().join("providers.toml");
        write_providers(dir.path(), "[providers.test]\n");
        let model = crate::ModelRef::parse("test/m").unwrap();

        let error = crate::model_supported_efforts(dir.path(), &model)
            .expect_err("缺少必填字段的 providers.toml 应返回配置错误");
        let crate::LlmError::Config { message } = error else {
            panic!("预期返回 LlmError::Config，实际为 {error:?}");
        };
        let path_display = providers_path.display().to_string();
        assert!(
            message.contains(&path_display),
            "错误消息应包含 providers.toml 路径 {path_display:?}，实际消息为：{message}"
        );
    }
}

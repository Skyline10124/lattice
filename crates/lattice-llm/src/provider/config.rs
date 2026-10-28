// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::BTreeMap,
    fs::File,
    io::{self, Read},
    path::{Path, PathBuf},
};

use serde::Deserialize;
use toml::Spanned;

use super::{Auth, ModelCost, ModelMetadata, Provider, builtins, model_ref};
use crate::protocol::Features;

pub(crate) const MAX_PROVIDERS_FILE_BYTES: usize = 1_048_576;
/// 按 providers.toml 中声明的表计数，对内置 provider 的覆盖也计入。
pub(crate) const MAX_PROVIDERS: usize = 256;
pub(crate) const MAX_MODELS_PER_PROVIDER: usize = 1_024;

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
#[error("{path}:{line}: {reason}", path = .path.display())]
pub(crate) struct ProviderConfigError {
    pub(crate) path: PathBuf,
    /// 源码行号从 1 开始；文件读取错误使用第 1 行。
    pub(crate) line: usize,
    pub(crate) reason: String,
}

impl ProviderConfigError {
    fn at(path: &Path, source: &[u8], offset: usize, reason: impl ToString) -> Self {
        Self {
            path: path.to_owned(),
            line: 1 + source[..offset.min(source.len())]
                .iter()
                .filter(|byte| **byte == b'\n')
                .count(),
            reason: reason.to_string(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ProviderRegistry {
    providers: BTreeMap<String, Provider>,
}

impl ProviderRegistry {
    pub(crate) fn builtins() -> Self {
        Self {
            providers: builtins::providers()
                .into_iter()
                .map(|provider| (provider.id.clone(), provider))
                .collect(),
        }
    }

    /// 从指定的用户配置路径读取，文件不存在时只使用内置表。
    pub(crate) fn load(path: impl AsRef<Path>) -> Result<Self, ProviderConfigError> {
        let path = path.as_ref();
        let file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::builtins()),
            Err(error) => return Err(ProviderConfigError::at(path, &[], 0, error)),
        };
        let mut bytes = Vec::new();
        file.take((MAX_PROVIDERS_FILE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|error| ProviderConfigError::at(path, &[], 0, error))?;
        check_file_size(path, &bytes)?;
        let source = std::str::from_utf8(&bytes).map_err(|error| {
            ProviderConfigError::at(
                path,
                &bytes,
                error.valid_up_to(),
                "providers.toml must be UTF-8",
            )
        })?;
        Self::parse(path, source)
    }

    /// 整份配置通过校验后才返回合并结果，避免部分配置生效。
    pub(crate) fn parse(path: impl AsRef<Path>, source: &str) -> Result<Self, ProviderConfigError> {
        let path = path.as_ref();
        let raw: RawConfig = toml::from_str(source).map_err(|error: toml::de::Error| {
            ProviderConfigError::at(
                path,
                source.as_bytes(),
                error.span().map_or(0, |span| span.start),
                error.message(),
            )
        })?;
        let error_at = |offset, reason: String| {
            ProviderConfigError::at(path, source.as_bytes(), offset, reason)
        };
        if raw.providers.len() > MAX_PROVIDERS {
            let offset = raw
                .providers
                .keys()
                .map(|id| id.span().start)
                .max()
                .unwrap_or(0);
            return Err(error_at(
                offset,
                format!("providers.toml declares more than {MAX_PROVIDERS} providers"),
            ));
        }

        let mut registry = Self::builtins();
        for (id, raw) in raw.providers {
            lattice_conversation::validate_provider_id(id.get_ref())
                .map_err(|error| error_at(id.span().start, error.to_string()))?;
            let offset = id.span().start;
            let raw = raw.into_inner().0;
            let builtin = registry.providers.remove(id.get_ref());
            // 内置 provider 的协议不可覆盖，即使取值相同；换协议须新建 id。
            if let Some(protocol) = raw.protocol.as_ref().filter(|_| builtin.is_some()) {
                return Err(error_at(
                    protocol.span().start,
                    "built-in provider protocol cannot be overridden; to use another protocol, create a new provider id".to_owned(),
                ));
            }
            if id.get_ref() == "chatgpt" {
                let forbidden = raw
                    .base_url
                    .as_ref()
                    .map(Spanned::span)
                    .or_else(|| raw.auth.as_ref().map(Spanned::span))
                    .or_else(|| raw.features.as_ref().map(Spanned::span))
                    .or_else(|| raw.catalog.as_ref().map(Spanned::span));
                if let Some(span) = forbidden {
                    return Err(error_at(
                        span.start,
                        "chatgpt only permits handwritten model metadata".to_owned(),
                    ));
                }
            }
            let protocol = raw
                .protocol
                .as_ref()
                .map(|value| parse_enum(value, &error_at))
                .transpose()?
                .or_else(|| builtin.as_ref().map(|provider| provider.protocol))
                .ok_or_else(|| error_at(offset, "new provider requires protocol".to_owned()))?;
            let auth = raw
                .auth
                .as_ref()
                .map(|value| parse_enum(value, &error_at))
                .transpose()?
                .or_else(|| builtin.as_ref().map(|provider| provider.auth))
                .unwrap_or_else(|| Auth::default_for(protocol));
            if auth == Auth::OauthChatgpt && id.get_ref() != "chatgpt" {
                return Err(error_at(
                    raw.auth.as_ref().map_or(offset, |value| value.span().start),
                    "oauth:chatgpt is reserved for chatgpt".to_owned(),
                ));
            }
            let base_url = if let Some(url) = raw.base_url {
                net_guard::validate_base_url_static(url.get_ref())
                    .map_err(|reason| error_at(url.span().start, reason.to_string()))?;
                url.into_inner()
            } else {
                builtin
                    .as_ref()
                    .map(|provider| provider.base_url.clone())
                    .ok_or_else(|| error_at(offset, "new provider requires base_url".to_owned()))?
            };
            let mut features = builtin.as_ref().map_or_else(
                || Features::defaults(protocol),
                |provider| provider.features,
            );
            apply_features(&mut features, raw.features, &error_at)?;
            let catalog = raw.catalog.map(Spanned::into_inner).or_else(|| {
                builtin
                    .as_ref()
                    .and_then(|provider| provider.catalog.clone())
            });
            if raw.models.len() > MAX_MODELS_PER_PROVIDER {
                let offset = raw
                    .models
                    .keys()
                    .map(|model| model.span().start)
                    .max()
                    .unwrap_or(offset);
                return Err(error_at(
                    offset,
                    format!(
                        "provider `{id}` has more than {MAX_MODELS_PER_PROVIDER} handwritten models"
                    ),
                ));
            }
            let mut models = BTreeMap::new();
            for (model, raw) in raw.models {
                model_ref::validate_model_id(model.get_ref())
                    .map_err(|error| error_at(model.span().start, error.to_string()))?;
                let raw = raw.into_inner().0;
                let mut model_features = features;
                apply_features(&mut model_features, raw.features, &error_at)?;
                models.insert(
                    model.into_inner(),
                    ModelMetadata {
                        context: raw.context,
                        output: raw.output,
                        effort: raw
                            .effort
                            .map(|values| {
                                values
                                    .iter()
                                    .map(|value| parse_enum(value, &error_at))
                                    .collect()
                            })
                            .transpose()?,
                        tools: raw.tools,
                        cost: raw.cost.map(|table| table.0),
                        features: model_features,
                    },
                );
            }
            let id = id.into_inner();
            registry.providers.insert(
                id.clone(),
                Provider {
                    id,
                    protocol,
                    base_url,
                    auth,
                    catalog,
                    features,
                    models,
                },
            );
        }
        Ok(registry)
    }

    pub(crate) fn get(&self, id: &str) -> Option<&Provider> {
        self.providers.get(id)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &Provider> {
        self.providers.values()
    }
}

fn check_file_size(path: &Path, bytes: &[u8]) -> Result<(), ProviderConfigError> {
    if bytes.len() > MAX_PROVIDERS_FILE_BYTES {
        return Err(ProviderConfigError::at(
            path,
            bytes,
            MAX_PROVIDERS_FILE_BYTES,
            format!("providers.toml exceeds {MAX_PROVIDERS_FILE_BYTES} bytes (1 MiB)"),
        ));
    }
    Ok(())
}

type RawFeatures = BTreeMap<Spanned<String>, Spanned<toml::Value>>;

/// Serde 派生的结构体也接受序列；配置记录必须是表。
struct Table<T>(T);

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Table<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct TableVisitor<T>(std::marker::PhantomData<T>);

        impl<'de, T: Deserialize<'de>> serde::de::Visitor<'de> for TableVisitor<T> {
            type Value = Table<T>;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a table")
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                map: A,
            ) -> Result<Self::Value, A::Error> {
                T::deserialize(serde::de::value::MapAccessDeserializer::new(map)).map(Table)
            }
        }

        deserializer.deserialize_map(TableVisitor(std::marker::PhantomData))
    }
}

fn parse_enum<T: serde::de::DeserializeOwned>(
    value: &Spanned<String>,
    error_at: &impl Fn(usize, String) -> ProviderConfigError,
) -> Result<T, ProviderConfigError> {
    let deserializer =
        serde::de::value::StrDeserializer::<serde::de::value::Error>::new(value.get_ref());
    T::deserialize(deserializer).map_err(|error| error_at(value.span().start, error.to_string()))
}

fn apply_features(
    features: &mut Features,
    overrides: Option<Spanned<RawFeatures>>,
    error_at: &impl Fn(usize, String) -> ProviderConfigError,
) -> Result<(), ProviderConfigError> {
    if let Some(overrides) = overrides {
        for (name, value) in overrides.into_inner() {
            let offset = value.span().start;
            features
                .apply(name.get_ref(), value.into_inner())
                .map_err(|reason| error_at(offset, format!("feature `{name}`: {reason}")))?;
        }
    }
    Ok(())
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawConfig {
    providers: BTreeMap<Spanned<String>, Spanned<Table<RawProvider>>>,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawProvider {
    protocol: Option<Spanned<String>>,
    base_url: Option<Spanned<String>>,
    auth: Option<Spanned<String>>,
    catalog: Option<Spanned<String>>,
    features: Option<Spanned<RawFeatures>>,
    models: BTreeMap<Spanned<String>, Spanned<Table<RawModel>>>,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawModel {
    context: Option<u64>,
    output: Option<u64>,
    effort: Option<Vec<Spanned<String>>>,
    tools: Option<bool>,
    cost: Option<Table<ModelCost>>,
    features: Option<Spanned<RawFeatures>>,
}

// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 按 catalog 选择元数据来源，再叠加 providers.toml 中的手写值。

use std::{collections::BTreeMap, path::Path, sync::LazyLock};

use serde::{Deserialize, Serialize};

use crate::provider::{Effort, ModelMetadata, Provider, config::ProviderRegistry};

pub(crate) mod fetch;
pub use fetch::{MetadataError, MetadataRefreshControl, fetch_snapshot};

/// 正式元数据源。
pub const MODELS_URL: &str = "https://models.dev/api.json";

/// 每次自动触发都按当前完整配置决定是否刷新；无效配置不发起网络请求。
pub fn metadata_auto_refresh_enabled(config_dir: &Path) -> Result<bool, MetadataError> {
    ProviderRegistry::load(config_dir.join("providers.toml"))
        .map(|registry| registry.auto_refresh_enabled())
        .map_err(|error| MetadataError::Configuration(error.to_string()))
}

const SNAPSHOT: &[u8] = include_bytes!("snapshot.json");
static BUILTIN_SNAPSHOT: LazyLock<Catalog> = LazyLock::new(|| {
    Catalog::parse(SNAPSHOT).expect("built-in model metadata snapshot must be valid")
});

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct Limits {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) context: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) output: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub(crate) struct Modalities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) input: Option<Vec<String>>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct Cost {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) input: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) output: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) cache_read: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) cache_write: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reasoning: Option<f64>,
}

/// 远端字段仅按白名单反序列化；effort 只由手写元数据提供。
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct Model {
    pub(crate) id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) name: Option<String>,
    #[serde(
        default,
        deserialize_with = "optional_object",
        skip_serializing_if = "Option::is_none"
    )]
    pub(crate) limit: Option<Limits>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reasoning: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reasoning_options: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tool_call: Option<bool>,
    #[serde(
        default,
        deserialize_with = "optional_object",
        skip_serializing_if = "Option::is_none"
    )]
    pub(crate) modalities: Option<Modalities>,
    #[serde(
        default,
        deserialize_with = "optional_object",
        skip_serializing_if = "Option::is_none"
    )]
    pub(crate) cost: Option<Cost>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) status: Option<String>,
    #[serde(skip)]
    pub(crate) effort: Option<Vec<Effort>>,
}

impl Model {
    fn overlay(&mut self, handwritten: &ModelMetadata) {
        if let Some(context) = handwritten.context {
            self.limit.get_or_insert_default().context = Some(context);
        }
        if let Some(output) = handwritten.output {
            self.limit.get_or_insert_default().output = Some(output);
        }
        self.effort.clone_from(&handwritten.effort);
        if let Some(tools) = handwritten.tools {
            self.tool_call = Some(tools);
        }
        if let Some(cost) = &handwritten.cost {
            let merged = self.cost.get_or_insert_default();
            merged.input = cost.input.or(merged.input);
            merged.output = cost.output.or(merged.output);
            merged.cache_read = cost.cache_read.or(merged.cache_read);
            merged.cache_write = cost.cache_write.or(merged.cache_write);
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct CatalogEntry {
    #[serde(deserialize_with = "objects_by_id")]
    models: BTreeMap<String, Model>,
}

/// 命名记录只接受对象，避免 Serde 将位置数组解释为结构体字段。
struct Object<T>(T);

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Object<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ObjectVisitor<T>(std::marker::PhantomData<T>);

        impl<'de, T: Deserialize<'de>> serde::de::Visitor<'de> for ObjectVisitor<T> {
            type Value = Object<T>;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a JSON object")
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                map: A,
            ) -> Result<Self::Value, A::Error> {
                T::deserialize(serde::de::value::MapAccessDeserializer::new(map)).map(Object)
            }
        }

        deserializer.deserialize_map(ObjectVisitor(std::marker::PhantomData))
    }
}

fn optional_object<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    Option::<Object<T>>::deserialize(deserializer).map(|value| value.map(|object| object.0))
}

fn objects_by_id<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<BTreeMap<String, T>, D::Error> {
    BTreeMap::<String, Object<T>>::deserialize(deserializer).map(|objects| {
        objects
            .into_iter()
            .map(|(id, object)| (id, object.0))
            .collect()
    })
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct Catalog(#[serde(deserialize_with = "objects_by_id")] BTreeMap<String, CatalogEntry>);

impl Catalog {
    fn parse(source: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(source)
    }

    fn filtered(source: &[u8], registry: &ProviderRegistry) -> Result<Self, serde_json::Error> {
        let mut raw: BTreeMap<String, serde_json::Value> = serde_json::from_slice(source)?;
        let mut entries = BTreeMap::new();
        let builtins = ProviderRegistry::builtins();
        for provider in builtins.iter().chain(registry.iter()) {
            if let Some(id) = &provider.catalog
                && let Some(value) = raw.remove(id)
            {
                let Object(entry) = serde_json::from_value(value)?;
                entries.insert(id.clone(), entry);
            }
        }
        Ok(Self(entries))
    }

    fn to_json(&self) -> Result<Vec<u8>, serde_json::Error> {
        let mut value = serde_json::to_value(self)?;
        value.sort_all_objects();
        let mut bytes = serde_json::to_vec_pretty(&value)?;
        bytes.push(b'\n');
        Ok(bytes)
    }
}

/// 将 models.dev 原始 JSON 过滤为可审查的快照；对象键排序，末尾保留换行。
pub(crate) fn snapshot_from_json(source: &[u8]) -> Result<Vec<u8>, MetadataError> {
    let catalog = Catalog::filtered(source, &ProviderRegistry::builtins())?;
    Ok(catalog.to_json()?)
}

pub(crate) struct Metadata {
    catalog: Catalog,
}

impl Metadata {
    pub(crate) fn has_catalog(&self, id: &str) -> bool {
        self.catalog.0.contains_key(id)
    }
    /// 缓存保留全部 catalog；缺失的内置条目回落快照，不补回缓存删除的模型或字段。
    pub(crate) fn load(config_dir: &Path) -> Self {
        let mut catalog = std::fs::read(config_dir.join("cache/models.json"))
            .ok()
            .and_then(|bytes| Catalog::parse(&bytes).ok())
            .unwrap_or_default();
        for (id, entry) in &BUILTIN_SNAPSHOT.0 {
            catalog.0.entry(id.clone()).or_insert_with(|| entry.clone());
        }
        Self { catalog }
    }

    pub(crate) fn merge(&self, provider: &Provider) -> BTreeMap<String, Model> {
        let mut models = provider
            .catalog
            .as_ref()
            .and_then(|id| self.catalog.0.get(id))
            .map(|entry| entry.models.clone())
            .unwrap_or_default();
        for (id, handwritten) in &provider.models {
            models
                .entry(id.clone())
                .or_insert_with(|| Model {
                    id: id.clone(),
                    ..Model::default()
                })
                .overlay(handwritten);
        }
        for model in models.values_mut() {
            model.tool_call.get_or_insert(true);
        }
        models
    }
}

#[cfg(test)]
mod tests;

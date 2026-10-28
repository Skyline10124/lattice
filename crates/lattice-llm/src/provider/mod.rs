// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 维护内置 provider、用户配置的严格解析与合并。

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::protocol::{Features, Protocol};

mod builtins;
pub(crate) mod config;
mod model_ref;

pub(crate) use model_ref::validate_provider_id;
pub use model_ref::{ModelRef, ModelRefError};

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Auth {
    Bearer,
    XApiKey,
    XGoogApiKey,
    #[serde(rename = "oauth:chatgpt")]
    OauthChatgpt,
    None,
}

impl Auth {
    pub(crate) fn default_for(protocol: Protocol) -> Self {
        match protocol {
            Protocol::ChatCompletions | Protocol::Responses => Self::Bearer,
            Protocol::Messages => Self::XApiKey,
            Protocol::GenerateContent | Protocol::Interactions => Self::XGoogApiKey,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Effort {
    Off,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

/// 手写价格以美元/百万 token 计；缺失字段保留，供元数据层按字段合并。
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ModelCost {
    pub(crate) input: Option<f64>,
    pub(crate) output: Option<f64>,
    pub(crate) cache_read: Option<f64>,
    pub(crate) cache_write: Option<f64>,
}

/// 这里只保存手写元数据，缺失值由元数据层补齐。
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ModelMetadata {
    pub(crate) context: Option<u64>,
    pub(crate) output: Option<u64>,
    pub(crate) effort: Option<Vec<Effort>>,
    /// 未指定时继承元数据；所有来源都未指定时，默认支持工具。
    pub(crate) tools: Option<bool>,
    pub(crate) cost: Option<ModelCost>,
    /// 模型级覆盖与 provider 配置合并后的有效 features。
    pub(crate) features: Features,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Provider {
    pub(crate) id: String,
    pub(crate) protocol: Protocol,
    pub(crate) base_url: String,
    pub(crate) auth: Auth,
    pub(crate) catalog: Option<String>,
    pub(crate) features: Features,
    pub(crate) models: BTreeMap<String, ModelMetadata>,
}

#[cfg(test)]
mod tests;

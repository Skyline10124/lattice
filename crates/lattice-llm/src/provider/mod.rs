// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Built-in providers and strictly validated user overrides. No request dispatch lives here.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::protocol::{Features, Protocol};

mod builtins;
pub(crate) mod config;
mod model_ref;

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

/// Handwritten prices in USD per million tokens, retaining unspecified fields for metadata merging.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ModelCost {
    pub(crate) input: Option<f64>,
    pub(crate) output: Option<f64>,
    pub(crate) cache_read: Option<f64>,
    pub(crate) cache_write: Option<f64>,
}

/// Only handwritten metadata is stored here; missing values are left for the metadata layer.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ModelMetadata {
    pub(crate) context: Option<u64>,
    pub(crate) output: Option<u64>,
    pub(crate) effort: Option<Vec<Effort>>,
    /// Inherit metadata when absent; tool support defaults to true if no source specifies it.
    pub(crate) tools: Option<bool>,
    pub(crate) cost: Option<ModelCost>,
    /// Effective features after applying this model's overrides to its provider's features.
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

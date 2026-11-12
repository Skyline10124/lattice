// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 本地 IPC 的模型查询请求与模型候选展示类型。

use std::{fmt, path::Path};

use lattice_conversation::{Effort, ModelId, ModelRef, ProviderId};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigurationRequirement {
    Declaration,
    ApiKey,
    ChatgptLogin,
}

impl ConfigurationRequirement {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Declaration => "declaration",
            Self::ApiKey => "api_key",
            Self::ChatgptLogin => "chatgpt_login",
        }
    }
}

impl fmt::Display for ConfigurationRequirement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Availability {
    Ready,
    Unconfigured {
        requirement: ConfigurationRequirement,
    },
    Invalid {
        reason: String,
    },
}

impl Availability {
    pub fn status_str(&self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Unconfigured { .. } => "unconfigured",
            Self::Invalid { .. } => "invalid",
        }
    }

    /// 由 provider id、可用性结果与实际的 `providers.toml` 路径生成面向用户的说明。
    pub fn diagnostic(&self, provider_id: &str, providers_path: &Path) -> Option<String> {
        match self {
            Self::Ready => None,
            Self::Unconfigured {
                requirement: ConfigurationRequirement::ApiKey,
            } => Some(format!(
                "provider {provider_id:?} has no API key configured; run lattice auth set {provider_id}"
            )),
            Self::Unconfigured {
                requirement: ConfigurationRequirement::ChatgptLogin,
            } => Some("ChatGPT is not signed in; run lattice auth login chatgpt".to_owned()),
            Self::Unconfigured {
                requirement: ConfigurationRequirement::Declaration,
            } => Some(format!(
                "provider {provider_id:?} is not declared; add [providers.{provider_id}] in {}",
                providers_path.display()
            )),
            Self::Invalid { reason } => Some(reason.clone()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ModelCandidateQuery {
    List {
        provider: Option<ProviderId>,
        ready_only: bool,
    },
    Target(ModelRef),
}

/// 候选只包含模型元数据与可用性展示状态，连接配置和秘密不跨过后台边界。
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

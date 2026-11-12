// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 本地 IPC 的模型查询请求与模型候选展示类型。

use std::fmt;

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
    fn as_str(self) -> &'static str {
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

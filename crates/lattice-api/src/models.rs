// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Public model diagnostics API.

use crate::session::{ApiError, Result};

/// Safe model metadata for frontend rendering.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct ModelInfo {
    pub alias: String,
    pub provider: String,
    pub provider_model: String,
    pub api_key_env: String,
    pub credential_status: String,
    pub base_url: String,
}

/// List known model aliases. `auth_only` filters to aliases with present credentials.
pub fn list_models(auth_only: bool) -> Vec<ModelInfo> {
    let router = lattice_llm::ModelRouter::new();
    router
        .aliases()
        .into_iter()
        .filter(|model| {
            !auth_only || model.credential_status == lattice_llm::CredentialStatus::Present
        })
        .map(model_info)
        .collect()
}

/// Resolve one model alias to safe frontend metadata.
pub fn resolve_model(model: &str) -> Result<ModelInfo> {
    let router = lattice_llm::ModelRouter::new();
    let target = lattice_llm::ModelTarget::new(model);
    router
        .resolve(&target)
        .map(model_info)
        .map_err(|err| ApiError::BadRequest(model_resolution_error(&err)))
}

/// 未知别名等使用错误的公开诊断：别名原文不进入 `ApiError`。
///
/// 未知别名和空的 OpenAI 兼容模型名诊断会拼入命令行输入。此处按错误类型
/// 提取稳定原因；缺少模型名的固定配置提示可原样展示，其余错误仅保留类别。
fn model_resolution_error(error: &lattice_llm::LlmError) -> String {
    match error {
        lattice_llm::LlmError::UnknownModel(_) => "unknown model alias".to_string(),
        lattice_llm::LlmError::Other(message)
            if message == lattice_llm::MISSING_OPENAI_COMPATIBLE_MODEL_HINT =>
        {
            message.clone()
        }
        lattice_llm::LlmError::Other(_) => "openai-compatible model name is invalid".to_string(),
        other => format!("model resolution failed: {}", other.kind()),
    }
}

/// Initialize LATTICE logging through the public API facade.
pub fn init_logging(verbose: bool) -> std::io::Result<()> {
    lattice_llm::init_logging(verbose)
}

fn model_info(model: lattice_llm::ResolvedModel) -> ModelInfo {
    ModelInfo {
        alias: model.alias,
        provider: format!("{:?}", model.provider),
        provider_model: model.provider_model,
        api_key_env: model.api_key_env,
        credential_status: format!("{:?}", model.credential_status),
        base_url: model.base_url,
    }
}

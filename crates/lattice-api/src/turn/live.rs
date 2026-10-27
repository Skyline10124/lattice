// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 接受事务使用的非秘密冻结事实。运行时目录与提交端配置只有这一个转换入口。

use super::types::RunTurnRequest;
use crate::session::{ApiError, Result};
use lattice_conversation::ProviderOrigin;
use lattice_llm::ProviderKind;
use lattice_session::store::{
    AttemptOrigin, AttemptProtocol, FrozenTurnConfig, MAX_MODEL_STEPS, ToolCatalogEntry,
    ToolCatalogSnapshot, TurnExecutionLimits,
};

/// ProviderKind 接入使用的持久来源与消息协议标记。
pub(super) fn provider_origin(provider: &ProviderKind) -> (AttemptOrigin, ProviderOrigin) {
    let (provider_id, protocol, message_origin) = match provider {
        ProviderKind::OpenAi => (
            "openai",
            AttemptProtocol::ChatCompletions,
            ProviderOrigin::OpenAiCompatible,
        ),
        ProviderKind::OpenAiCompatible => (
            "openai-compatible",
            AttemptProtocol::ChatCompletions,
            ProviderOrigin::OpenAiCompatible,
        ),
        ProviderKind::Anthropic => (
            "anthropic",
            AttemptProtocol::AnthropicMessages,
            ProviderOrigin::Anthropic,
        ),
        ProviderKind::Gemini => (
            "google",
            AttemptProtocol::GenerateContent,
            ProviderOrigin::Gemini,
        ),
    };
    (
        AttemptOrigin {
            protocol,
            provider_id: provider_id.into(),
        },
        message_origin,
    )
}

pub(crate) fn freeze_config(
    request: &RunTurnRequest,
    provider: &ProviderKind,
    catalog: &[lancet_api::ToolDefinition],
) -> Result<FrozenTurnConfig> {
    let loaded = request
        .loaded_profile
        .as_ref()
        .ok_or_else(|| ApiError::BadRequest("loaded agent profile required".into()))?;
    let config = FrozenTurnConfig {
        agent_name: loaded.profile.agent.name.clone(),
        model: loaded.profile.agent.model.clone(),
        provider: provider_origin(provider).0.provider_id,
        system_prompt: Some(loaded.system_prompt.clone()),
        catalog: ToolCatalogSnapshot::new(
            env!("CARGO_PKG_VERSION").into(),
            catalog
                .iter()
                .map(|tool| ToolCatalogEntry {
                    name: tool.name.clone(),
                    descriptor_identity: tool.descriptor_identity.clone(),
                    description: tool.description.clone(),
                    input_schema: tool.parameters.clone(),
                })
                .collect(),
        )
        .map_err(|_| ApiError::BadRequest("invalid frozen catalog".into()))?,
        limits: TurnExecutionLimits {
            timeout_secs: loaded.profile.llm.timeout_secs.unwrap_or(300),
            max_model_steps: MAX_MODEL_STEPS,
            max_model_attempts: loaded.profile.llm.retry.max_retries.saturating_add(1),
        },
    };
    Ok(config)
}

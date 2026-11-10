// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 接受事务使用的非秘密冻结事实。执行器工具规格与后台配置只有这一个转换入口。

use super::tools::ToolSpec;
use crate::profile::TurnProfile;
use crate::session::{ApiError, Result};
use lattice_session::store::{
    FrozenTurnConfig, MAX_MODEL_STEPS, ToolCatalogEntry, ToolCatalogSnapshot, TurnExecutionLimits,
};

pub(crate) fn freeze_config(
    profile: &TurnProfile,
    target: &lattice_llm::ModelTarget,
    catalog: &[ToolSpec],
) -> Result<FrozenTurnConfig> {
    let config = FrozenTurnConfig {
        agent_name: profile.effective.agent_name.clone(),
        model: target.model.model().clone(),
        provider: target.model.provider().clone(),
        effort: target.effort,
        system_prompt: profile.system_prompt.clone(),
        catalog: ToolCatalogSnapshot::new(
            catalog
                .iter()
                .map(|tool| ToolCatalogEntry {
                    name: tool.name.clone(),
                    tool_identity: tool.tool_identity.clone(),
                    description: tool.description.clone(),
                    input_schema: tool.input_schema.clone(),
                })
                .collect(),
        )
        .map_err(|_| ApiError::BadRequest("invalid frozen catalog".into()))?,
        limits: TurnExecutionLimits {
            timeout_secs: profile.effective.timeout_secs,
            max_model_steps: MAX_MODEL_STEPS,
            max_model_attempts: profile.effective.retry.max_attempts(),
        },
    };
    Ok(config)
}

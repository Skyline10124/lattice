// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

#[cfg(any(test, feature = "test-utils"))]
use std::path::PathBuf;

use super::types::RunTurnRequest;

/// 模型与提示来自已加载配置，provider 必须由冻结连接或测试注入显式提供。
pub(crate) fn resolve_model_and_prompt(
    request: &RunTurnRequest,
    frozen_provider: &str,
) -> crate::session::Result<(String, Option<String>, String)> {
    let lp = request.loaded_profile.as_ref().ok_or_else(|| {
        crate::session::ApiError::BadRequest("loaded active agent profile is required".into())
    })?;
    let model = lp.profile.agent.model.clone();
    let system_prompt = if lp.system_prompt.is_empty() {
        None
    } else {
        Some(lp.system_prompt.clone())
    };

    Ok((model, system_prompt, frozen_provider.to_owned()))
}

#[cfg(any(test, feature = "test-utils"))]
pub(crate) fn resolve_project_root(request: &RunTurnRequest) -> crate::session::Result<PathBuf> {
    if let Some(root) = &request.test_project_root {
        return Ok(PathBuf::from(root));
    }
    std::env::current_dir().map_err(|err| {
        crate::session::ApiError::Internal(format!(
            "cannot resolve current Local Workspace root: {err}"
        ))
    })
}

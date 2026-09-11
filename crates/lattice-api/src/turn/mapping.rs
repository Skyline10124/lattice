// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use lattice_llm::{ModelRouter, ModelTarget};
use std::path::PathBuf;

use super::types::RunTurnRequest;

/// 显式连接的 provider 优先，避免后台用自己的环境重新解释提交端模型。
pub(crate) fn resolve_model_and_prompt(
    request: &RunTurnRequest,
    frozen_provider: Option<&str>,
) -> (String, Option<String>, String) {
    let (model, system_prompt) = if let Some(ref lp) = request.loaded_profile {
        let model = lp.profile.agent.model.clone();
        let system_prompt = if lp.system_prompt.is_empty() {
            None
        } else {
            Some(lp.system_prompt.clone())
        };
        (model, system_prompt)
    } else {
        ("sonnet".to_string(), None)
    };

    let provider = frozen_provider.map(str::to_owned).unwrap_or_else(|| {
        ModelRouter::new()
            .resolve(&ModelTarget::new(&model))
            .map(|rm| rm.provider.as_str().to_string())
            .unwrap_or_else(|_| "unknown".to_string())
    });

    (model, system_prompt, provider)
}

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

// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use lattice_api::ModelInfo;

/// Render a resolved model to a JSON value suitable for CLI output.
pub(crate) fn resolved_model_json(model: &ModelInfo) -> serde_json::Value {
    serde_json::json!({
        "alias": model.alias,
        "provider": model.provider,
        "provider_model": model.provider_model,
        "api_key_env": model.api_key_env,
        "credential_status": model.credential_status,
        "base_url": model.base_url,
    })
}

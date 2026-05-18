use lattice_llm::ResolvedModel;

/// Render a resolved model to a JSON value suitable for CLI output.
pub(crate) fn resolved_model_json(model: &ResolvedModel) -> serde_json::Value {
    serde_json::json!({
        "alias": model.alias,
        "provider": format!("{:?}", model.provider),
        "provider_model": model.provider_model,
        "api_key_env": model.api_key_env,
        "credential_status": format!("{:?}", model.credential_status),
        "base_url": model.base_url,
    })
}

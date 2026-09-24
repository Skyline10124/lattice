// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use colored::Colorize;

/// 保留未知别名等使用错误的可归因诊断。
///
/// 原因在 `ApiError::BadRequest` 中；本地命令不跨 IPC，借道会脱敏的
/// `From<ApiError> for LocalError` 会丢掉原因，因此按既有 `LocalError` 词汇
/// 保留原因；其它意外故障仍走 `report_error` 的安全泛化。
fn resolve_error(error: lattice_api::ApiError) -> anyhow::Error {
    match error {
        lattice_api::ApiError::BadRequest(reason) => {
            anyhow::Error::new(lattice_api::local::LocalError {
                kind: lattice_api::local::LocalErrorKind::BadRequest,
                message: reason,
                recovery: None,
            })
        }
        error => anyhow::Error::new(error),
    }
}

/// Resolve a model alias to provider details.
///
/// Default text: short provider/provider_model only.
/// `--verbose`: includes alias, provider, provider_model, api_key_env,
///   credential_status, and base_url.
/// `--json`: structured object with all fields.
///
/// Never prints credential values — only env var names and present/missing status.
pub fn run(model: &str, verbose: bool, json: bool) -> Result<()> {
    let resolved = lattice_api::resolve_model(model).map_err(resolve_error)?;

    if json {
        let out = super::model_render::resolved_model_json(&resolved);
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    if verbose {
        println!(
            "{}",
            format!(
                "resolve: {} -> {}@{}",
                model, resolved.provider_model, resolved.provider
            )
            .cyan()
        );
        println!("  {}: {}", "Alias".bold(), resolved.alias);
        println!("  {}: {}", "Provider".bold(), resolved.provider);
        println!("  {}: {}", "Provider Model".bold(), resolved.provider_model);
        println!("  {}: {}", "Auth Env".bold(), resolved.api_key_env);
        println!(
            "  {}: {}",
            "Credential Status".bold(),
            resolved.credential_status
        );
        println!("  {}: {}", "Base URL".bold(), resolved.base_url);
    } else {
        println!("{}: {}", "Provider".bold(), resolved.provider);
        println!("{}: {}", "Model".bold(), resolved.provider_model);
    }

    Ok(())
}

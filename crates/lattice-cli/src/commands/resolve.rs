// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use colored::Colorize;

/// Resolve a model alias to provider details.
///
/// Default text: short provider/provider_model only.
/// `--verbose`: includes alias, provider, provider_model, api_key_env,
///   credential_status, and base_url.
/// `--json`: structured object with all fields.
///
/// Never prints credential values — only env var names and present/missing status.
pub fn run(model: &str, verbose: bool, json: bool) -> Result<()> {
    let resolved = lattice_api::resolve_model(model).map_err(|e| anyhow::anyhow!("{e}"))?;

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

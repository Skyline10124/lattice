use anyhow::Result;
use colored::Colorize;
use lattice_core::router::ModelRouter;

use crate::display::{credential_label, status_icon};

pub fn run(model: &str, provider_override: Option<&str>, trace: bool, json: bool) -> Result<()> {
    let router = ModelRouter::new();
    let resolved = router.resolve(model, provider_override)?;

    if json {
        let out = serde_json::json!({
            "canonical_id": resolved.canonical_id,
            "provider": resolved.provider,
            "api_model_id": resolved.api_model_id,
            "api_protocol": format!("{:?}", resolved.api_protocol),
            "base_url": resolved.base_url,
            "context_length": resolved.context_length,
            "api_key_present": resolved.api_key.is_some(),
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    if trace {
        println!(
            "{}",
            format!(
                "resolve: {} \u{2192} {}@{}",
                model, resolved.canonical_id, resolved.provider
            )
            .cyan()
        );
        println!("  {}: {}", "Provider".bold(), resolved.provider);
        println!("  {}: {}", "Model".bold(), resolved.api_model_id);
        println!("  {}: {:?}", "Protocol".bold(), resolved.api_protocol);
        println!("  {}: {}", "Base URL".bold(), resolved.base_url);
        println!("  {}: {}", "Context".bold(), resolved.context_length);
        println!(
            "  {}: {} {}",
            "Auth".bold(),
            status_icon(resolved.api_key.is_some()),
            credential_label(resolved.api_key.is_some())
        );
    } else {
        println!("{}: {}", "Provider".bold(), resolved.provider);
        println!("{}: {}", "Model".bold(), resolved.api_model_id);
        println!("{}: {:?}", "Protocol".bold(), resolved.api_protocol);
        println!("{}: {}", "Base URL".bold(), resolved.base_url);
    }

    Ok(())
}

use anyhow::Result;
use artemis_core::router::ModelRouter;
use colored::Colorize;

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
        println!("{}", format!("resolve: {} \u{2192} {}@{}", model, resolved.canonical_id, resolved.provider).cyan());
        println!("  {}: {}", "Provider".bold(), resolved.provider);
        println!("  {}: {}", "Model".bold(), resolved.api_model_id);
        println!("  {}: {:?}", "Protocol".bold(), resolved.api_protocol);
        println!("  {}: {}", "Base URL".bold(), resolved.base_url);
        println!("  {}: {}", "Context".bold(), resolved.context_length);
        println!("  {}: {}", "Auth".bold(), if resolved.api_key.is_some() { "\u{2713} set".green() } else { "\u{2717} missing".red() });
    } else {
        println!("{}: {}", "Provider".bold(), resolved.provider);
        println!("{}: {}", "Model".bold(), resolved.api_model_id);
        println!("{}: {:?}", "Protocol".bold(), resolved.api_protocol);
        println!("{}: {}", "Base URL".bold(), resolved.base_url);
    }

    Ok(())
}

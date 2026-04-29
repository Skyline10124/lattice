use anyhow::Result;
use artemis_core::router::ModelRouter;
use artemis_core::{
    chat_complete,
    types::{Message, Role},
};
use colored::Colorize;
use std::time::Instant;

pub async fn run(
    model: &str,
    prompt: &str,
    provider_override: Option<&str>,
    verbose: bool,
    json: bool,
    creds: &crate::credentials::CredentialStore,
) -> Result<()> {
    let start = Instant::now();

    if verbose {
        eprintln!("{}", format!("resolve: {} ...", model).dimmed());
    }

    let router = ModelRouter::with_credentials(creds.to_hashmap());
    let resolved = router.resolve(model, provider_override)?;

    if verbose {
        eprintln!(
            "{}",
            format!("resolved: {}@{}", resolved.canonical_id, resolved.provider).cyan()
        );
    }

    let messages = vec![Message::new(Role::User, prompt.into(), None, None, None)];

    if verbose {
        eprintln!("{}", "streaming...".dimmed());
    }

    let response = chat_complete(&resolved, &messages, &[]).await?;
    let elapsed = start.elapsed().as_millis();

    if json {
        let out = serde_json::json!({
            "content": response.content,
            "model": resolved.canonical_id,
            "provider": resolved.provider,
            "usage": response.usage,
            "finish_reason": response.finish_reason,
            "elapsed_ms": elapsed,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        if let Some(content) = response.content {
            println!("{}", content);
        }
        if verbose {
            if let Some(usage) = response.usage {
                eprintln!(
                    "\n{}: {} tok (prompt: {}, completion: {}) | {} ms",
                    "usage".dimmed(),
                    usage.total_tokens,
                    usage.prompt_tokens,
                    usage.completion_tokens,
                    elapsed
                );
            }
        }
    }

    Ok(())
}

use anyhow::Result;
use colored::Colorize;
use lattice_agent::{default_tool_definitions, Agent, DefaultToolExecutor, LoopEvent};
use lattice_core::router::ModelRouter;
use std::time::Instant;

pub async fn run(
    prompt: Option<String>,
    model: String,
    provider_override: Option<String>,
    resolve_only: bool,
    creds: &crate::credentials::CredentialStore,
) -> Result<()> {
    // --- Resolve chain (always shown) ---
    println!("{}", "╔══ RESOLVE ══╗".bright_cyan());

    tracing::info!(model = %model, "debug: resolve start");

    let router = ModelRouter::with_credentials(creds.to_hashmap());
    let resolved = router.resolve(&model, provider_override.as_deref())?;

    // Print resolve details with colored highlights
    println!(
        "  {} {} → {}",
        "normalize:".dimmed(),
        model,
        resolved.canonical_id.bold()
    );
    println!("  {} {}", "provider:".dimmed(), resolved.provider.cyan());
    println!(
        "  {} {}",
        "protocol:".dimmed(),
        format!("{:?}", resolved.api_protocol).yellow()
    );
    println!("  {} {}", "base_url:".dimmed(), resolved.base_url.green());
    println!(
        "  {} {}",
        "api_model_id:".dimmed(),
        resolved.api_model_id.bold()
    );

    let key_preview = resolved
        .api_key
        .as_ref()
        .map(|k| {
            if k.len() > 4 {
                format!("{}{}", &k[..4], "...".dimmed())
            } else {
                k.clone()
            }
        })
        .unwrap_or_else(|| "None".to_string());
    println!("  {} {}", "credential:".dimmed(), key_preview.red());

    println!(
        "  {} {}",
        "context_length:".dimmed(),
        resolved.context_length
    );
    println!("{}", "╚═════════════╝".bright_cyan());

    tracing::info!(
        canonical = %resolved.canonical_id,
        provider = %resolved.provider,
        protocol = ?resolved.api_protocol,
        "debug: resolved"
    );

    if resolve_only {
        println!("\n{}", "--resolve-only: stopping after resolve".dimmed());
        return Ok(());
    }

    // --- Chat phase ---
    let prompt_text = prompt.unwrap_or_else(|| "Hello, respond with one word.".to_string());
    println!("\n{}", "╔══ CHAT ══╗".bright_magenta());
    println!("  {} {}", "prompt:".dimmed(), prompt_text.bold());

    let start = Instant::now();

    // Clone resolved for summary before moving into Agent
    let resolved_summary = resolved.clone();

    let tools = default_tool_definitions();
    let mut agent = Agent::new(resolved)
        .with_tools(tools)
        .with_tool_executor(Box::new(DefaultToolExecutor::new(".")));

    tracing::info!(prompt_len = prompt_text.len(), "debug: agent.run start");

    let events = agent.run(&prompt_text, 10).await;
    let elapsed = start.elapsed();

    // Print execution summary
    println!(
        "  {} {}",
        "elapsed:".dimmed(),
        format!("{}ms", elapsed.as_millis()).bold()
    );

    let mut tool_call_names: Vec<String> = Vec::new();
    let mut total_tokens: u64 = 0;
    let mut content_buf = String::new();

    for event in &events {
        match event {
            LoopEvent::Token { text } => {
                content_buf.push_str(&text);
            }
            LoopEvent::Reasoning { .. } => {}
            LoopEvent::ToolCallRequired { calls } => {
                for c in calls {
                    tool_call_names.push(c.function.name.clone());
                }
            }
            LoopEvent::Done { ref usage } => {
                if let Some(u) = usage {
                    total_tokens = u.total_tokens as u64;
                }
            }
            LoopEvent::Error { message } => {
                tracing::error!(error = %message, "debug: stream error");
                println!("  {} {}", "error:".red(), message.red());
            }
        }
    }

    println!(
        "  {} {}",
        "tokens:".dimmed(),
        format!("{}", total_tokens).bold()
    );

    if !tool_call_names.is_empty() {
        println!(
            "  {} {}",
            "tool_calls:".dimmed(),
            tool_call_names.join(", ").yellow()
        );
    }

    println!("  {} {}", "response_len:".dimmed(), content_buf.len());

    println!("{}", "╚═════════════╝".bright_magenta());

    // --- Agent summary ---
    println!("\n{}", "╔══ AGENT SUMMARY ══╗".bright_green());
    println!(
        "  {} {}",
        "model:".dimmed(),
        resolved_summary.api_model_id.bold()
    );
    println!(
        "  {} {}",
        "provider:".dimmed(),
        resolved_summary.provider.cyan()
    );
    println!("  {} {}", "turns:".dimmed(), events.len());
    println!(
        "  {} {}",
        "tool_executions:".dimmed(),
        tool_call_names.len()
    );
    println!("  {} {}ms", "wall_time:".dimmed(), elapsed.as_millis());
    println!("  {} {}", "total_tokens:".dimmed(), total_tokens);
    println!("{}", "╚═══════════════════╝".bright_green());

    // Print content to stdout
    println!("\n{}", content_buf);

    Ok(())
}

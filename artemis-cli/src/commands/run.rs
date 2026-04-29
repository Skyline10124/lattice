use anyhow::Result;
use artemis_agent::{default_tool_definitions, Agent, DefaultToolExecutor, LoopEvent};
use artemis_core::router::ModelRouter;
use colored::Colorize;
use std::io::Write;

pub fn run(
    prompt: String,
    model: String,
    provider_override: Option<&str>,
    verbose: bool,
    json: bool,
    creds: &crate::credentials::CredentialStore,
) -> Result<()> {
    if verbose {
        eprintln!("{}", format!("resolve: {} ...", model).dimmed());
    }

    let router = ModelRouter::with_credentials(creds.to_hashmap());
    let resolved = router.resolve(&model, provider_override)?;

    if verbose {
        eprintln!(
            "{}",
            format!("resolved: {}@{}", resolved.canonical_id, resolved.provider).cyan()
        );
    }

    let tools = default_tool_definitions();
    let mut agent = Agent::new(resolved)
        .with_tools(tools)
        .with_tool_executor(Box::new(DefaultToolExecutor::new(".")));

    if verbose {
        eprintln!("{}", "streaming...".dimmed());
    }

    let events = agent.run(&prompt, 10);
    display_events(&agent, events, verbose, json)?;

    Ok(())
}

fn display_events(agent: &Agent, events: Vec<LoopEvent>, verbose: bool, json: bool) -> Result<()> {
    let mut content_buf = String::new();

    for event in events {
        match event {
            LoopEvent::Token { text } => {
                if !json {
                    print!("{}", text);
                    std::io::stdout().flush()?;
                }
                content_buf.push_str(&text);
            }
            LoopEvent::Reasoning { text } => {
                if verbose {
                    eprintln!("{} {}", "reasoning:".dimmed(), text);
                }
            }
            LoopEvent::ToolCallRequired { calls } => {
                if verbose && !json {
                    eprintln!("\n{} {} tool call(s)...", "executing".dimmed(), calls.len());
                }
            }
            LoopEvent::Done { usage } => {
                if verbose && !json {
                    if let Some(u) = usage {
                        eprintln!(
                            "\n{}: {} tok (prompt: {}, completion: {})",
                            "usage".dimmed(),
                            u.total_tokens,
                            u.prompt_tokens,
                            u.completion_tokens,
                        );
                    }
                }
            }
            LoopEvent::Error { message } => {
                eprintln!("{} {}", "error:".red(), message);
            }
        }
    }

    if !json {
        println!();
    }

    if json {
        let usage = agent.token_usage();
        let out = serde_json::json!({
            "content": content_buf,
            "total_tokens": usage,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
    }

    Ok(())
}

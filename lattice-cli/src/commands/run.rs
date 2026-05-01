use anyhow::Result;
use colored::Colorize;
use lattice_agent::{default_tool_definitions, Agent, DefaultToolExecutor, LoopEvent};
use lattice_core::router::ModelRouter;
use lattice_harness::{AgentRegistry, Pipeline};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

pub async fn run(
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

    let events = agent.run_async(&prompt, 10).await;
    display_events(&agent, events, verbose, json)?;

    Ok(())
}

/// Run a pipeline: load agent registry, create Pipeline, and execute.
pub fn run_pipeline(
    prompt: &str,
    start_agent: &str,
    agents_dir: Option<&str>,
    verbose: bool,
    json: bool,
) -> Result<()> {
    let dir = agents_dir_path(agents_dir);

    if verbose {
        eprintln!(
            "{}",
            format!("loading agents from {} ...", dir.display()).dimmed()
        );
    }

    let registry = Arc::new(
        AgentRegistry::load_dir(&dir)
            .map_err(|e| anyhow::anyhow!("Failed to load agents: {}", e))?,
    );

    if registry.list().is_empty() {
        anyhow::bail!("No agent profiles found in '{}'", dir.display());
    }

    if verbose {
        eprintln!(
            "{}",
            format!("loaded {} agents", registry.list().len()).cyan()
        );
        for profile in registry.list() {
            eprintln!("  - {} ({})", profile.agent.name, profile.agent.model);
        }
    }

    // Validate pipeline chain before running
    let pipeline_check = Pipeline::new("pre-check", registry.clone(), None, None);
    let report = pipeline_check.dry_run(start_agent);
    if !report.valid {
        eprintln!("{}", "Pipeline validation failed:".red());
        for issue in &report.issues {
            eprintln!("  - {}", issue.red());
        }
        anyhow::bail!(
            "Pipeline '{}' is invalid — fix agent profiles before running",
            start_agent
        );
    }

    if verbose {
        eprintln!(
            "{}",
            format!(
                "pipeline chain: {} → end",
                report.agents_in_chain.join(" → ")
            )
            .cyan()
        );
    }

    // Run the pipeline
    let mut pipeline = Pipeline::new(start_agent, registry, None, None);
    let result = pipeline.run(start_agent, prompt);

    if json {
        let out = serde_json::json!({
            "completed": result.completed,
            "duration_ms": result.duration_ms,
            "agents": result.results.iter().map(|r| serde_json::json!({
                "agent": r.agent_name,
                "output_preview": r.output.to_string().chars().take(200).collect::<String>(),
                "next": r.next,
                "duration_ms": r.duration_ms,
            })).collect::<Vec<_>>(),
            "errors": result.errors.iter().map(|e| serde_json::json!({
                "agent": e.agent_name,
                "message": e.message,
                "skippable": e.skippable,
            })).collect::<Vec<_>>(),
            "skipped": result.skipped,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        println!(
            "{}",
            format!("Pipeline completed in {}ms", result.duration_ms).green()
        );
        if result.completed {
            println!("{}", "Status: completed".green());
        } else {
            println!("{}", "Status: incomplete".yellow());
        }

        for r in &result.results {
            println!();
            println!(
                "{}",
                format!("── {} ({}ms) ──", r.agent_name, r.duration_ms).cyan()
            );
            let preview: String = r.output.to_string().chars().take(500).collect();
            println!("{}", preview);
            if let Some(ref next) = r.next {
                eprintln!("{}", format!("  → next: {}", next).dimmed());
            }
        }

        for e in &result.errors {
            println!();
            println!("{}", format!("── {} (ERROR) ──", e.agent_name).red());
            println!("  {}", e.message.red());
            if e.skippable {
                println!("  {}", "(skippable)".dimmed());
            }
        }

        if !result.skipped.is_empty() {
            println!();
            println!(
                "{}",
                format!("Skipped: {}", result.skipped.join(", ")).yellow()
            );
        }
    }

    Ok(())
}

fn agents_dir_path(override_path: Option<&str>) -> PathBuf {
    if let Some(p) = override_path {
        PathBuf::from(p)
    } else if let Ok(dir) = std::env::var("LATTICE_AGENTS_DIR") {
        PathBuf::from(dir)
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".lattice").join("agents")
    } else {
        PathBuf::from(".lattice/agents")
    }
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

use anyhow::{anyhow, Result};
use lattice_agent::{Agent, LoopEvent};
use lattice_core::router::ModelRouter;
use lattice_core::types::ToolDefinition;
use colored::Colorize;
use serde::Deserialize;
use std::io::Write;
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

    let tools = tool_definitions();
    let mut agent = Agent::new(resolved.clone()).with_tools(tools);

    if verbose {
        eprintln!("{}", "streaming...".dimmed());
    }

    let events = agent.send_message_async(prompt).await;
    run_conversation(&mut agent, events, verbose, json)?;

    let elapsed = start.elapsed().as_millis();
    if verbose && !json {
        eprintln!("{}: {} ms", "elapsed".dimmed(), elapsed);
    }

    Ok(())
}

fn run_conversation(
    agent: &mut Agent,
    mut events: Vec<LoopEvent>,
    verbose: bool,
    json: bool,
) -> Result<()> {
    let mut content_buf = String::new();

    loop {
        let mut tool_calls = Vec::new();

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
                    tool_calls = calls;
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

        if tool_calls.is_empty() {
            break;
        }

        let results: Vec<(String, String)> = tool_calls
            .iter()
            .map(|call| {
                let result = execute_tool(&call.function.name, &call.function.arguments)
                    .unwrap_or_else(|e| format!("Error: {}", e));
                (call.id.clone(), result)
            })
            .collect();

        events = agent.submit_tools(results, None);
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

fn tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition::new(
            "read_file".into(),
            "Read the contents of a file at the given path".into(),
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Absolute path to the file to read"
                    }
                },
                "required": ["path"]
            }),
        ),
        ToolDefinition::new(
            "grep".into(),
            "Search for a pattern in files within a directory".into(),
            serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Search pattern (regex)"
                    },
                    "path": {
                        "type": "string",
                        "description": "Directory path to search in (default: current directory)"
                    }
                },
                "required": ["pattern"]
            }),
        ),
    ]
}

fn execute_tool(name: &str, args_json: &str) -> Result<String> {
    match name {
        "read_file" => {
            #[derive(Deserialize)]
            struct Args {
                path: String,
            }
            let args: Args =
                serde_json::from_str(args_json).map_err(|e| anyhow!("Invalid args: {}", e))?;
            std::fs::read_to_string(&args.path)
                .map_err(|e| anyhow!("Failed to read {}: {}", args.path, e))
        }
        "grep" => {
            #[derive(Deserialize)]
            struct Args {
                pattern: String,
                path: Option<String>,
            }
            let args: Args =
                serde_json::from_str(args_json).map_err(|e| anyhow!("Invalid args: {}", e))?;
            let dir = args.path.unwrap_or_else(|| ".".to_string());
            let output = std::process::Command::new("grep")
                .args(["-rn", "--color=never", &args.pattern, &dir])
                .output()
                .map_err(|e| anyhow!("Failed to run grep: {}", e))?;
            if output.status.success() {
                Ok(String::from_utf8_lossy(&output.stdout).to_string())
            } else if output.status.code() == Some(1) {
                Ok(String::new())
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                Err(anyhow!("grep failed: {}", stderr))
            }
        }
        _ => Err(anyhow!("Unknown tool: {}", name)),
    }
}

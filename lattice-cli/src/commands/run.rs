use anyhow::Result;
use colored::Colorize;
use lattice_agent::{default_tool_definitions, Agent, DefaultToolExecutor, LoopEvent};
use lattice_core::router::ModelRouter;
use lattice_harness::tools::{ToolHandler, ToolRegistry};
use lattice_harness::{AgentRegistry, Pipeline};
use lattice_plugin::{
    builtin::{
        code_review::CodeReviewPlugin, deep_research::DeepResearchPlugin, doc_gen::DocGenPlugin,
        image_gen::ImageGenPlugin, knowledge_base::KnowledgeBasePlugin, pptx_gen::PptxGenPlugin,
        refactor::RefactorPlugin, security_audit::SecurityAuditPlugin, test_gen::TestGenPlugin,
    },
    bundle::{BehaviorMode, PluginBundle, PluginMeta},
    registry::PluginRegistry,
};
use std::collections::HashMap;
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
    save: bool,
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

    let resolved_model = resolved.canonical_id.clone();
    let resolved_provider = resolved.provider.clone();

    let tools = default_tool_definitions();
    let mut agent = Agent::new(resolved)
        .with_tools(tools)
        .with_tool_executor(Box::new(DefaultToolExecutor::new(".")));

    if verbose {
        eprintln!("{}", "streaming...".dimmed());
    }

    let events = agent.run(&prompt, 10).await;
    let content = extract_content(&events);
    display_events(&agent, events, verbose, json)?;

    if save {
        let mut session = crate::session::Session::new(resolved_model, resolved_provider);
        session.messages.push(crate::session::SessionMessage {
            role: "user".to_string(),
            content: prompt.clone(),
        });
        session.messages.push(crate::session::SessionMessage {
            role: "assistant".to_string(),
            content,
        });
        let mgr = crate::session::SessionManager::new();
        mgr.save(&session)?;
        if verbose {
            eprintln!("{}", format!("session saved: {}", session.id).dimmed());
        }
    }

    Ok(())
}

/// Run a pipeline: load agent registry, create registries, and execute.
#[allow(clippy::too_many_arguments)]
pub async fn run_pipeline(
    prompt: &str,
    start_agent: &str,
    agents_dir: Option<&str>,
    _plugins_dir: Option<&str>,
    _tools_dir: Option<&str>,
    verbose: bool,
    json: bool,
    creds: HashMap<String, String>,
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

    // Build ToolRegistry with default tool definitions
    let mut tool_registry = ToolRegistry::new();
    for td in default_tool_definitions() {
        let name = td.name.clone();
        tool_registry.register(
            &name,
            ToolHandler::Native(Arc::new(move |_| {
                Ok("execution delegated to Agent tool executor".into())
            })),
            td,
        );
    }
    let tool_registry = Arc::new(tool_registry);

    // Build PluginRegistry with all 9 builtin plugins
    let mut plugin_registry = PluginRegistry::new();
    let builtins: Vec<(&str, Box<dyn lattice_plugin::erased::ErasedPlugin>, &str)> = vec![
        (
            "CodeReview",
            Box::new(CodeReviewPlugin::new()),
            "Code review plugin",
        ),
        (
            "Refactor",
            Box::new(RefactorPlugin::new()),
            "Code refactoring plugin",
        ),
        (
            "TestGen",
            Box::new(TestGenPlugin::new()),
            "Test generation plugin",
        ),
        (
            "SecurityAudit",
            Box::new(SecurityAuditPlugin::new()),
            "Security audit plugin",
        ),
        (
            "DocGen",
            Box::new(DocGenPlugin::new()),
            "Documentation generation plugin",
        ),
        (
            "PptxGen",
            Box::new(PptxGenPlugin::new()),
            "PPTX generation plugin",
        ),
        (
            "DeepResearch",
            Box::new(DeepResearchPlugin::new()),
            "Deep research plugin",
        ),
        (
            "ImageGen",
            Box::new(ImageGenPlugin::new()),
            "Image generation plugin",
        ),
        (
            "KnowledgeBase",
            Box::new(KnowledgeBasePlugin::new()),
            "Knowledge base plugin",
        ),
    ];
    for (name, plugin, desc) in builtins {
        let bundle = PluginBundle {
            meta: PluginMeta {
                name: name.to_string(),
                version: "0.1".into(),
                description: desc.to_string(),
                author: "lattice".into(),
            },
            plugin,
            default_behavior: BehaviorMode::Yolo,
            default_tools: vec![],
        };
        plugin_registry
            .register(bundle)
            .map_err(|e| anyhow::anyhow!("Failed to register plugin '{}': {}", name, e))?;
        if verbose {
            eprintln!("  plugin: {} — {}", name, desc);
        }
    }
    let plugin_registry = Arc::new(plugin_registry);

    // Validate pipeline chain before running
    let pipeline_check =
        Pipeline::new("pre-check", registry.clone(), None, None).with_credentials(creds.clone());
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

    // Run the pipeline with plugin & tool registries
    let mut pipeline = Pipeline::new(start_agent, registry, None, None)
        .with_plugin_registry(plugin_registry)
        .with_tool_registry(tool_registry)
        .with_credentials(creds);
    let result = pipeline.run(start_agent, prompt).await;

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

fn flush_reasoning(buf: &str, verbose: bool, json: bool) {
    if verbose && !json && !buf.is_empty() {
        eprintln!("{} {}", "reasoning:".dimmed(), buf.trim());
    }
}

fn display_events(agent: &Agent, events: Vec<LoopEvent>, verbose: bool, json: bool) -> Result<()> {
    let mut content_buf = String::new();
    let mut reasoning_buf = String::new();

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
                reasoning_buf.push_str(&text);
            }
            LoopEvent::ToolCallRequired { calls } => {
                flush_reasoning(&reasoning_buf, verbose, json);
                reasoning_buf.clear();
                if verbose && !json {
                    eprintln!("\n{} {} tool call(s)...", "executing".dimmed(), calls.len());
                }
            }
            LoopEvent::Done { usage } => {
                flush_reasoning(&reasoning_buf, verbose, json);
                reasoning_buf.clear();
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
                flush_reasoning(&reasoning_buf, verbose, json);
                reasoning_buf.clear();
                eprintln!("{} {}", "error:".red(), message);
            }
        }
    }
    flush_reasoning(&reasoning_buf, verbose, json);

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

fn extract_content(events: &[LoopEvent]) -> String {
    let mut buf = String::new();
    for event in events {
        if let LoopEvent::Token { text } = event {
            buf.push_str(text);
        }
    }
    buf
}

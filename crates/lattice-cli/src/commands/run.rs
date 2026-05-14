use anyhow::Result;
use colored::Colorize;
use futures::StreamExt;
use lattice_core::types::Role;
use lattice_orchestrator::AgentTurnSpec;
use lattice_prompt as prompt;
use lattice_runtime::{AgentHandleId, Runtime, RuntimeTurnEvent, TurnCompletion, TurnRequest};
use std::collections::HashMap;
use std::io::Write;

#[allow(clippy::too_many_arguments)]
pub async fn run(
    prompt: String,
    model: String,
    provider_override: Option<&str>,
    verbose: bool,
    json: bool,
    creds: &crate::credentials::CredentialStore,
    save: bool,
    previous_session: Option<crate::session::Session>,
    dry_run: bool,
    system_prompt: Option<&str>,
    max_turns: u32,
    stream_output: bool,
    security: crate::security::RuntimeSecurity,
) -> Result<()> {
    // Dry-run: compile prompt through engine and print, no LLM call, no credential needed
    if dry_run {
        return dry_run_prompt(&prompt, &model, system_prompt, previous_session.as_ref()).await;
    }

    if verbose {
        eprintln!("{}", format!("resolve: {} ...", model).dimmed());
    }

    let runtime = crate::runtime::model_runtime(creds.to_hashmap());
    if prompt.trim() == "/compact" {
        let Some(session) = previous_session else {
            anyhow::bail!("No session selected for /compact. Use --continue or --session.");
        };
        let compacted = runtime.compact_session(session, &model, provider_override, 8)?;
        if save {
            if let Some(store) = runtime.session_store() {
                store.save(&compacted)?;
            }
        }
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "session_id": compacted.id,
                    "parent_id": compacted.parent_id,
                    "message_count": compacted.messages.len(),
                    "compact": compacted.messages.first().and_then(|message| message.compact.clone()),
                }))?
            );
        } else {
            println!("compacted session: {}", display_id(&compacted.id));
        }
        return Ok(());
    }
    let session_id = previous_session.as_ref().map(|session| session.id.clone());
    let turn_spec = run_turn_spec(
        model.clone(),
        provider_override.map(str::to_string),
        system_prompt.map(str::to_string),
        ".".into(),
    );
    let seed_messages = previous_session
        .as_ref()
        .map(crate::session::messages_for_agent)
        .unwrap_or_default();
    let task_state_seed =
        crate::coding_agent::load_latest_manifest_metadata(&runtime, previous_session.as_ref());
    let handle_id = runtime.get_or_create_agent_handle_for_session_seeded(
        turn_spec,
        security.clone(),
        session_id.clone(),
        seed_messages,
        task_state_seed,
    )?;
    let snapshot = runtime.agent_handle_snapshot(&handle_id)?;

    if verbose {
        eprintln!(
            "{}",
            format!("resolved: {}@{}", snapshot.model, snapshot.provider).cyan()
        );
    }

    if verbose {
        eprintln!("{}", "streaming...".dimmed());
    }

    let request = TurnRequest {
        input: prompt.clone(),
        max_turns,
        persist_session: save,
        session_id,
        cancellation_id: None,
    };
    let completion = if json || !stream_output {
        submit_run_runtime_turn(&runtime, &handle_id, request, None).await?
    } else {
        let mut renderer = RuntimeEventRenderer::new(verbose, false);
        let completion =
            submit_run_runtime_turn(&runtime, &handle_id, request, Some(&mut renderer)).await?;
        renderer.finish(
            completion
                .token_usage
                .as_ref()
                .map(|u| u64::from(u.total_tokens)),
        )?;
        completion
    };
    crate::security::reap_audit(&security).await;

    if json || !stream_output {
        render_run_completion(&completion, verbose, json)?;
    }

    if save && verbose {
        let saved_id = completion
            .saved_session_id
            .as_deref()
            .map(display_id)
            .unwrap_or_else(|| "not saved".into());
        eprintln!("{}", format!("session saved: {}", saved_id).dimmed());
    }

    Ok(())
}

fn display_id(id: &str) -> String {
    let mut chars = id.chars();
    let mut prefix = chars.by_ref().take(8).collect::<String>();
    if chars.next().is_some() {
        prefix.push_str("...");
    }
    prefix
}

/// Compile prompt through the engine and print the result without calling LLM.
async fn dry_run_prompt(
    prompt: &str,
    model_name: &str,
    system_prompt: Option<&str>,
    previous_session: Option<&crate::session::Session>,
) -> Result<()> {
    let history = previous_session
        .map(crate::session::messages_for_agent)
        .unwrap_or_default();
    let runtime = crate::runtime::model_runtime(HashMap::new());
    let report = runtime
        .dry_run_prompt(model_name, system_prompt, prompt, history)
        .await?;

    println!("{}", "═══ Compiled Prompt (dry-run) ═══".green());
    println!("{}", "─ Input Sections ─".dimmed());
    for (s, b) in report.sections.iter().zip(report.budgets.iter()) {
        let label = format!("{:?}", s.layer);
        let budget = match b {
            prompt::TokenBudget::Fixed(n) => format!("Fixed({})", n),
            prompt::TokenBudget::Ratio(r) => format!("Ratio({})", r),
            prompt::TokenBudget::Dynamic => "Dynamic".into(),
        };
        println!(
            "  {}  tokens={:<6} priority={:<3} budget={}",
            label.cyan(),
            s.tokens.to_string().yellow(),
            s.priority.to_string().dimmed(),
            budget.dimmed(),
        );
    }
    println!();
    println!("{}", "─ Rendered Messages ─".dimmed());
    for msg in &report.messages {
        let role_label = match msg.role {
            Role::System => "System".red(),
            Role::User => "User".cyan(),
            Role::Assistant => "Assistant".green(),
            Role::Tool => "Tool".yellow(),
        };
        println!("  [{}] {}", role_label, msg.content);
    }
    println!();
    println!("{}", "─ Stats ─".dimmed());
    println!(
        "  total_tokens={:<6} context_length={}",
        report.rendered.total_tokens.to_string().yellow(),
        report.resolved.context_length.to_string().dimmed(),
    );

    Ok(())
}

/// Run a pipeline: load agent registry, create registries, and execute.
#[allow(clippy::too_many_arguments)]
pub async fn run_pipeline(
    prompt: &str,
    start_agent: &str,
    agents_dir: Option<&str>,
    plugins_dir: Option<&str>,
    _tools_dir: Option<&str>,
    verbose: bool,
    json: bool,
    creds: HashMap<String, String>,
) -> Result<()> {
    let (runtime, dir) = crate::runtime::pipeline_runtime(agents_dir, plugins_dir, creds)?;

    if verbose {
        eprintln!(
            "{}",
            format!("loading agents from {} ...", dir.display()).dimmed()
        );
    }

    let registry = runtime.agent_registry();

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

    if verbose {
        for meta in runtime.plugin_registry().list() {
            eprintln!("  plugin: {} — {}", meta.name, meta.description);
        }
    }

    // Validate pipeline chain before running
    let report = runtime.dry_run_pipeline(start_agent);
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

    let result = runtime.run_pipeline(start_agent, prompt).await;

    if json {
        let out = serde_json::json!({
            "completed": result.completed,
            "duration_ms": result.duration_ms,
            "agents": result.results.iter().map(|r| serde_json::json!({
                "agent": r.agent_name,
                "output": r.output,
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

fn flush_reasoning(buf: &str, verbose: bool, json: bool) {
    if verbose && !json && !buf.is_empty() {
        eprintln!("{} {}", "reasoning:".dimmed(), buf.trim());
    }
}

pub(crate) fn run_turn_spec(
    model: String,
    provider_override: Option<String>,
    system_prompt: Option<String>,
    workspace: String,
) -> AgentTurnSpec {
    AgentTurnSpec {
        profile_name: "run".into(),
        model,
        provider_override,
        system_prompt,
        prompt_spec: None,
        capabilities: lattice_orchestrator::CapabilitiesConfig {
            presets: vec!["minimal".into()],
            bundles: Vec::new(),
        },
        tools: Default::default(),
        behavior: Default::default(),
        handoff: Default::default(),
        memory: Default::default(),
        project_root: Some(workspace.clone()),
        cwd: Some(workspace),
        thinking_effort: None,
    }
}

pub(crate) async fn submit_run_runtime_turn(
    runtime: &Runtime,
    handle_id: &AgentHandleId,
    request: TurnRequest,
    mut renderer: Option<&mut RuntimeEventRenderer>,
) -> Result<TurnCompletion> {
    let stream = runtime.submit_turn(handle_id, request).await?;
    tokio::pin!(stream);
    while let Some(event) = stream.next().await {
        if let Some(renderer) = renderer.as_deref_mut() {
            renderer.render(&event)?;
        }
    }

    let completion = runtime
        .latest_turn_completion(handle_id)?
        .ok_or_else(|| anyhow::anyhow!("runtime turn completed without stored completion"))?;
    crate::coding_agent::require_successful_runtime_completion(completion)
}

pub(crate) fn run_completion_value(completion: &TurnCompletion) -> serde_json::Value {
    serde_json::json!({
        "content": completion.assistant_text,
        "total_tokens": completion
            .token_usage
            .as_ref()
            .map(|usage| usage.total_tokens)
            .unwrap_or_default(),
    })
}

fn render_run_completion(completion: &TurnCompletion, verbose: bool, json: bool) -> Result<()> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&run_completion_value(completion))?
        );
    } else {
        print!("{}", completion.assistant_text);
        println!();
    }
    if verbose && !json {
        if let Some(usage) = completion.token_usage.as_ref() {
            eprintln!(
                "\n{}: {} tok (prompt: {}, completion: {})",
                "usage".dimmed(),
                usage.total_tokens,
                usage.prompt_tokens,
                usage.completion_tokens,
            );
        }
    }
    std::io::stdout().flush()?;
    Ok(())
}

pub(crate) struct RuntimeEventRenderer {
    verbose: bool,
    json: bool,
    content_buf: String,
    reasoning_buf: String,
}

impl RuntimeEventRenderer {
    fn new(verbose: bool, json: bool) -> Self {
        Self {
            verbose,
            json,
            content_buf: String::new(),
            reasoning_buf: String::new(),
        }
    }

    fn render(&mut self, event: &RuntimeTurnEvent) -> Result<()> {
        match event {
            RuntimeTurnEvent::Token { text, .. } => {
                if !self.json {
                    print!("{}", text);
                    std::io::stdout().flush()?;
                }
                self.content_buf.push_str(text);
            }
            RuntimeTurnEvent::Reasoning { text, .. } => {
                self.reasoning_buf.push_str(text);
            }
            RuntimeTurnEvent::ToolStarted {
                batch_count, name, ..
            } => {
                self.flush_reasoning();
                if self.verbose && !self.json {
                    eprintln!("\n{} {} tool call(s)...", "executing".dimmed(), batch_count);
                    eprintln!("{} {}", "tool".dimmed(), name);
                }
            }
            RuntimeTurnEvent::ToolFinished { name, result, .. } => {
                if self.verbose && !self.json {
                    eprintln!(
                        "\n{} {} -> {}",
                        "tool".dimmed(),
                        name,
                        result.chars().take(200).collect::<String>()
                    );
                }
            }
            RuntimeTurnEvent::Completed { .. } => {
                self.flush_reasoning();
            }
            RuntimeTurnEvent::Error { message, .. } => {
                self.flush_reasoning();
                eprintln!("{} {}", "error:".red(), message);
            }
            RuntimeTurnEvent::ManifestReady { .. } => {}
            RuntimeTurnEvent::Cancelled { .. } => {
                self.flush_reasoning();
                eprintln!("{}", "cancelled".yellow());
            }
        }
        Ok(())
    }

    fn finish(&mut self, total_tokens: Option<u64>) -> Result<()> {
        self.flush_reasoning();
        if self.json {
            let out = serde_json::json!({
                "content": self.content_buf,
                "total_tokens": total_tokens.unwrap_or_default(),
            });
            println!("{}", serde_json::to_string_pretty(&out)?);
        } else {
            println!();
        }
        std::io::stdout().flush()?;
        Ok(())
    }

    fn flush_reasoning(&mut self) {
        flush_reasoning(&self.reasoning_buf, self.verbose, self.json);
        self.reasoning_buf.clear();
    }
}

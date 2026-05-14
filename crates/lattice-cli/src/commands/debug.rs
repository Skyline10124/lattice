use anyhow::Result;
use colored::Colorize;
use futures::StreamExt;
use lattice_orchestrator::AgentTurnSpec;
use lattice_runtime::{AgentHandleId, Runtime, RuntimeTurnEvent, TurnCompletion, TurnRequest};
use std::time::{Duration, Instant};

fn flush_reasoning(buf: &str) {
    if !buf.is_empty() {
        eprintln!("  {} {}", "reasoning:".dimmed(), buf.trim());
    }
}

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

    let runtime = crate::runtime::model_runtime(creds.to_hashmap());
    let resolved = runtime.resolve_with_provider(&model, provider_override.as_deref())?;

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

    println!(
        "  {} {}",
        "credential:".dimmed(),
        credential_status_label(&resolved.credential_status).red()
    );

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

    let security = crate::security::default_runtime_security(&std::env::current_dir()?)?;
    let turn_spec = debug_turn_spec(model.clone(), provider_override, ".".into());
    let handle_id = runtime.get_or_create_agent_handle_for_session(turn_spec, security, None)?;

    tracing::info!(prompt_len = prompt_text.len(), "debug: submit_turn start");

    let summary = execute_debug_runtime_turn(
        &runtime,
        &handle_id,
        TurnRequest {
            input: prompt_text.clone(),
            max_turns: 10,
            persist_session: false,
            session_id: None,
            cancellation_id: None,
        },
        start,
    )
    .await?;
    let elapsed = summary.elapsed;

    // Print execution summary
    println!(
        "  {} {}",
        "elapsed:".dimmed(),
        format!("{}ms", elapsed.as_millis()).bold()
    );

    flush_reasoning(&summary.reasoning_text);
    for message in &summary.errors {
        tracing::error!(error = %message, "debug: stream error");
        println!("  {} {}", "error:".red(), message.red());
    }

    let total_tokens = summary
        .completion
        .token_usage
        .as_ref()
        .map(|usage| u64::from(usage.total_tokens))
        .unwrap_or_default();

    println!(
        "  {} {}",
        "tokens:".dimmed(),
        format!("{}", total_tokens).bold()
    );

    if !summary.tool_names.is_empty() {
        println!(
            "  {} {}",
            "tool_calls:".dimmed(),
            summary.tool_names.join(", ").yellow()
        );
    }

    println!(
        "  {} {}",
        "response_len:".dimmed(),
        summary.completion.assistant_text.len()
    );

    println!("{}", "╚═════════════╝".bright_magenta());

    // --- Agent summary ---
    println!("\n{}", "╔══ AGENT SUMMARY ══╗".bright_green());
    println!(
        "  {} {}",
        "model:".dimmed(),
        summary.completion.model.bold()
    );
    println!(
        "  {} {}",
        "provider:".dimmed(),
        summary.completion.provider.cyan()
    );
    let turns = summary.tool_event_count + 1;
    println!("  {} {}", "turns:".dimmed(), turns);
    println!(
        "  {} {}",
        "tool_executions:".dimmed(),
        summary.tool_names.len()
    );
    println!("  {} {}ms", "wall_time:".dimmed(), elapsed.as_millis());
    println!("  {} {}", "total_tokens:".dimmed(), total_tokens);
    println!("{}", "╚═══════════════════╝".bright_green());

    // Print content to stdout
    println!("\n{}", summary.completion.assistant_text);

    Ok(())
}

pub(crate) struct DebugRuntimeSummary {
    pub(crate) completion: TurnCompletion,
    pub(crate) elapsed: Duration,
    pub(crate) tool_names: Vec<String>,
    pub(crate) tool_event_count: u64,
    pub(crate) reasoning_text: String,
    pub(crate) errors: Vec<String>,
}

pub(crate) fn debug_turn_spec(
    model: String,
    provider_override: Option<String>,
    workspace: String,
) -> AgentTurnSpec {
    AgentTurnSpec {
        profile_name: "debug".into(),
        model,
        provider_override,
        system_prompt: None,
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

pub(crate) async fn execute_debug_runtime_turn(
    runtime: &Runtime,
    handle_id: &AgentHandleId,
    request: TurnRequest,
    start: Instant,
) -> Result<DebugRuntimeSummary> {
    let stream = runtime.submit_turn(handle_id, request).await?;
    tokio::pin!(stream);

    let mut reasoning_text = String::new();
    let mut tool_names = Vec::new();
    let mut tool_event_count = 0;
    let mut errors = Vec::new();

    while let Some(event) = stream.next().await {
        match event {
            RuntimeTurnEvent::Token { .. } => {}
            RuntimeTurnEvent::Reasoning { text, .. } => {
                reasoning_text.push_str(&text);
            }
            RuntimeTurnEvent::ToolStarted { name, .. } => {
                tool_event_count += 1;
                tool_names.push(name);
            }
            RuntimeTurnEvent::ToolFinished { name, result, .. } => {
                tracing::info!(tool = %name, result_len = result.len(), "debug: tool result");
            }
            RuntimeTurnEvent::Error { message, .. } => {
                errors.push(message);
            }
            RuntimeTurnEvent::ManifestReady { .. }
            | RuntimeTurnEvent::Completed { .. }
            | RuntimeTurnEvent::Cancelled { .. } => {}
        }
    }

    let elapsed = start.elapsed();
    let completion = runtime
        .latest_turn_completion(handle_id)?
        .ok_or_else(|| anyhow::anyhow!("runtime turn completed without stored completion"))?;

    Ok(DebugRuntimeSummary {
        completion,
        elapsed,
        tool_names,
        tool_event_count,
        reasoning_text,
        errors,
    })
}

fn credential_status_label(status: &lattice_core::CredentialStatus) -> &'static str {
    match status {
        lattice_core::CredentialStatus::Present => "present (redacted)",
        lattice_core::CredentialStatus::Missing => "missing",
        lattice_core::CredentialStatus::NotRequired => "not required",
    }
}

#[cfg(test)]
mod tests {
    use super::credential_status_label;
    use lattice_core::CredentialStatus;

    #[test]
    fn credential_status_label_never_exposes_key_material() {
        assert_eq!(
            credential_status_label(&CredentialStatus::Present),
            "present (redacted)"
        );
        assert_eq!(
            credential_status_label(&CredentialStatus::Missing),
            "missing"
        );
        assert_eq!(
            credential_status_label(&CredentialStatus::NotRequired),
            "not required"
        );
    }
}

#[cfg(test)]
mod debug_runtime {
    #[tokio::test]
    async fn debug_chat_phase_uses_runtime_turn_events() {
        let Some(port) = crate::test_support::spawn_scripted_openai_server(vec![
            crate::test_support::scripted_content_response("debug runtime text"),
        ])
        .await
        else {
            eprintln!("skipping debug runtime eval: loopback bind denied");
            return;
        };
        let workdir = tempfile::tempdir().unwrap();

        let summary = crate::test_support::debug_chat_runtime_turn(
            crate::test_support::DebugRuntimeTurnFixture {
                model: "scripted-debug-runtime".into(),
                base_url: format!("http://127.0.0.1:{port}"),
                workdir: workdir.path().to_path_buf(),
                prompt: "Debug through runtime".into(),
            },
        )
        .await
        .unwrap();

        assert_eq!(summary.completion.assistant_text, "debug runtime text");
        assert_eq!(summary.response_len, "debug runtime text".len());
        assert_eq!(summary.tool_names.len(), 0);
        assert_eq!(summary.model, "scripted-debug-runtime");
        assert_eq!(summary.provider, "ollama");
    }
}

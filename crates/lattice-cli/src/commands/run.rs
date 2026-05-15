use anyhow::Result;
use colored::Colorize;
use futures::StreamExt;
use lattice_core::types::Role;
use lattice_profile::{ProfileError, ProfileSource};
use lattice_prompt as prompt;
use lattice_runtime::{Runtime, RuntimeAgentSession, TurnCompletion, TurnRequest};
use std::collections::HashMap;
use std::io::{IsTerminal, Write};
use std::path::PathBuf;

#[allow(clippy::too_many_arguments)]
pub async fn run(
    prompt: String,
    model: String,
    model_override: Option<String>,
    agent: Option<&str>,
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
    project_root: PathBuf,
    security_config: crate::config::SecurityConfig,
    global_agents_dir: PathBuf,
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
    let task_state_seed =
        crate::session_seed::load_latest_manifest_metadata(&runtime, previous_session.as_ref());
    let security = crate::security::build_runtime_security(&security_config, &project_root)?;
    let source = crate::profile_runtime::profile_source_from_agent_arg(agent);
    let session = prepare_run_profile_session(
        &runtime,
        source,
        model_override,
        project_root.clone(),
        global_agents_dir,
        security.clone(),
        security_config,
        provider_override.map(str::to_string),
        session_id.clone(),
        task_state_seed,
    )?;
    let resolved = runtime.resolve_with_provider(&model, provider_override)?;

    if verbose {
        eprintln!(
            "{}",
            format!("resolved: {}@{}", resolved.canonical_id, resolved.provider).cyan()
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
        submit_run_runtime_turn(&runtime, session, request, None).await?
    } else {
        let mut renderer = crate::runtime_turn::RuntimeEventRenderer::new(verbose, false);
        let completion =
            submit_run_runtime_turn(&runtime, session, request, Some(&mut renderer)).await?;
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
    render_profile_warning(&completion, agent, previous_session.is_some(), json)?;

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

#[allow(clippy::too_many_arguments)]
fn prepare_run_profile_session(
    runtime: &Runtime,
    source: ProfileSource,
    model_override: Option<String>,
    project_root: PathBuf,
    global_agents_dir: PathBuf,
    security: crate::security::RuntimeSecurity,
    security_config: crate::config::SecurityConfig,
    provider_override: Option<String>,
    session_id: Option<String>,
    task_state_seed: Option<lattice_agent::TaskState>,
) -> Result<RuntimeAgentSession> {
    let request = crate::profile_runtime::ProfileRuntimeSessionRequest {
        source: source.clone(),
        project_root: project_root.clone(),
        global_agents_dir: global_agents_dir.clone(),
        security_config,
        model_override: model_override.clone(),
        provider_override: provider_override.clone(),
        session_id: session_id.clone(),
        task_state_seed: task_state_seed.clone(),
    };
    match crate::profile_runtime::prepare_profile_runtime_session(runtime, request) {
        Ok(session) => Ok(session),
        Err(error) if matches!(source, ProfileSource::Default) => {
            let Some(profile_error) = error.downcast_ref::<ProfileError>() else {
                return Err(error);
            };
            let ProfileError::MissingDefault { dir, template } = profile_error else {
                return Err(error);
            };
            if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
                anyhow::bail!("{}", crate::profile_runtime::missing_default_message(dir));
            }
            eprint!(
                "No default agent profile exists. Create .lattice/agents/default/agent.toml and continue this run? [y/N] "
            );
            std::io::stderr().flush()?;
            let mut answer = String::new();
            std::io::stdin().read_line(&mut answer)?;
            if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
                anyhow::bail!("{}", crate::profile_runtime::missing_default_message(dir));
            }
            let path = crate::profile_runtime::create_default_profile(dir, template)?;
            eprintln!(
                "Created {}. Edit it later to change prompt, tools, model, or security settings.",
                path.strip_prefix(&project_root).unwrap_or(&path).display()
            );
            crate::profile_runtime::prepare_profile_runtime_session_with_security(
                runtime,
                ProfileSource::Default,
                project_root,
                global_agents_dir,
                security,
                model_override,
                provider_override,
                session_id,
                task_state_seed,
            )
        }
        Err(error) => Err(error),
    }
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

pub(crate) async fn submit_run_runtime_turn(
    runtime: &Runtime,
    session: RuntimeAgentSession,
    request: TurnRequest,
    mut renderer: Option<&mut crate::runtime_turn::RuntimeEventRenderer>,
) -> Result<TurnCompletion> {
    let submitted = runtime.submit_session_turn(session, request).await?;
    let mut events = submitted.events;
    while let Some(event) = events.next().await {
        if let Some(renderer) = renderer.as_deref_mut() {
            renderer.render(&event)?;
        }
    }

    let completion = submitted.completion.await?;
    crate::runtime_turn::require_successful_runtime_completion(completion)
}

pub(crate) fn run_completion_value(completion: &TurnCompletion) -> serde_json::Value {
    let mut value = serde_json::json!({
        "content": completion.assistant_text,
        "total_tokens": completion
            .token_usage
            .as_ref()
            .map(|usage| usage.total_tokens)
            .unwrap_or_default(),
    });
    if let Some(warning) = completion.profile_drift_warning.as_ref() {
        value["profile_warning"] = serde_json::Value::String(warning.clone());
    }
    value
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

fn render_profile_warning(
    completion: &TurnCompletion,
    agent: Option<&str>,
    restored_session: bool,
    json: bool,
) -> Result<()> {
    if json {
        return Ok(());
    }
    let Some(warning) = completion.profile_drift_warning.as_ref() else {
        return Ok(());
    };
    if restored_session {
        if let Some(agent) = agent {
            let hash = snapshot_hash_from_warning(warning).unwrap_or(warning);
            eprintln!("Ignoring --agent {agent}; restored session is locked to snapshot {hash}.");
        } else {
            eprintln!("{warning}");
        }
    } else {
        eprintln!("{warning}");
    }
    Ok(())
}

fn snapshot_hash_from_warning(warning: &str) -> Option<&str> {
    warning.split("snapshot ").nth(1).map(str::trim)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_runtime::{TurnId, TurnStatus};

    fn completion_with_profile_warning() -> TurnCompletion {
        TurnCompletion {
            session_id: "session".into(),
            turn_id: TurnId::new(),
            status: TurnStatus::Completed,
            assistant_text: "answer".into(),
            token_usage: None,
            task_state_snapshot: lattice_agent::TaskState::new(),
            final_report: None,
            turn_manifests: Vec::new(),
            model: "model".into(),
            provider: "provider".into(),
            saved_session_id: None,
            profile_drift_warning: Some(
                "profile changed on disk; current session remains locked to snapshot abcdef123456"
                    .into(),
            ),
            error: None,
        }
    }

    #[test]
    fn run_completion_json_includes_profile_warning() {
        let value = run_completion_value(&completion_with_profile_warning());

        assert_eq!(value["content"], "answer");
        assert_eq!(
            value["profile_warning"],
            "profile changed on disk; current session remains locked to snapshot abcdef123456"
        );
    }

    #[test]
    fn profile_drift_warning_hash_suffix_is_extracted() {
        let warning = completion_with_profile_warning()
            .profile_drift_warning
            .unwrap();

        assert_eq!(snapshot_hash_from_warning(&warning), Some("abcdef123456"));
    }
}

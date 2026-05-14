use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Result;
use colored::Colorize;
use futures::StreamExt;
use lattice_core::types::Message;
use lattice_orchestrator::AgentTurnSpec;
use lattice_runtime::{
    AgentHandleId, Runtime, RuntimeTurnEvent, TurnCompletion, TurnRequest, TurnStatus,
};

use crate::security::RuntimeSecurity;

pub(crate) struct CodingAgentOptions {
    pub(crate) prompt: String,
    pub(crate) model: String,
    pub(crate) provider_override: Option<String>,
    pub(crate) workdir: PathBuf,
    pub(crate) max_turns: u32,
    pub(crate) stream_output: bool,
    pub(crate) verbose: bool,
    pub(crate) json: bool,
    pub(crate) credentials: HashMap<String, String>,
    pub(crate) save_session: bool,
    pub(crate) previous_session: Option<crate::session::Session>,
    pub(crate) security: RuntimeSecurity,
}

pub(crate) struct CodingAgentBuildOptions {
    pub(crate) model: String,
    pub(crate) provider_override: Option<String>,
    pub(crate) workdir: PathBuf,
    pub(crate) credentials: HashMap<String, String>,
    pub(crate) previous_session: Option<crate::session::Session>,
    pub(crate) prior_messages: Vec<Message>,
    pub(crate) thinking_effort: Option<String>,
    pub(crate) security: RuntimeSecurity,
}

pub(crate) struct BuiltCodingAgent {
    pub(crate) handle_id: AgentHandleId,
    pub(crate) runtime: Runtime,
    pub(crate) model: String,
    pub(crate) provider: String,
    #[allow(dead_code)] // consumed by TUI/runtime adapter plans after the CLI hard cut.
    pub(crate) context_limit: u32,
}

pub(crate) async fn run(options: CodingAgentOptions) -> Result<()> {
    let built = build_coding_agent(CodingAgentBuildOptions {
        model: options.model.clone(),
        provider_override: options.provider_override.clone(),
        workdir: options.workdir.clone(),
        credentials: options.credentials.clone(),
        previous_session: options.previous_session.clone(),
        prior_messages: Vec::new(),
        thinking_effort: None,
        security: options.security.clone(),
    })?;
    let resolved_model = built.model;
    let resolved_provider = built.provider;

    if options.verbose {
        eprintln!(
            "{}",
            format!(
                "coding agent: {}@{} in {} (sandbox={})",
                resolved_model,
                resolved_provider,
                options.workdir.display(),
                options.security.mode_label
            )
            .cyan()
        );
    }

    let request = TurnRequest {
        input: options.prompt.clone(),
        max_turns: options.max_turns,
        persist_session: options.save_session,
        session_id: options.previous_session.as_ref().map(|s| s.id.clone()),
        cancellation_id: None,
    };

    let completion = if options.json || !options.stream_output {
        submit_coding_runtime_turn(&built.runtime, &built.handle_id, request, None).await?
    } else {
        let mut renderer = CodingEventRenderer::new(options.verbose, false);
        let completion = submit_coding_runtime_turn(
            &built.runtime,
            &built.handle_id,
            request,
            Some(&mut renderer),
        )
        .await?;
        let final_report_markdown = completion.final_report.clone().unwrap_or_default();
        renderer.finish(
            completion
                .token_usage
                .as_ref()
                .map(|usage| u64::from(usage.total_tokens)),
            &final_report_markdown,
        )?;
        completion
    };
    crate::security::reap_audit(&options.security).await;

    if options.json || !options.stream_output {
        render_completion(&completion, options.verbose, options.json)?;
    }

    if options.save_session && options.verbose {
        if let Some(session_id) = completion.saved_session_id.as_deref() {
            eprintln!("{}", format!("session saved: {session_id}").dimmed());
        }
    }

    Ok(())
}

pub(crate) fn build_coding_agent(options: CodingAgentBuildOptions) -> Result<BuiltCodingAgent> {
    let runtime = crate::runtime::model_runtime(options.credentials.clone());
    build_coding_agent_with_runtime(runtime, options)
}

pub(crate) fn build_coding_agent_with_runtime(
    runtime: Runtime,
    options: CodingAgentBuildOptions,
) -> Result<BuiltCodingAgent> {
    let messages = if !options.prior_messages.is_empty() {
        options.prior_messages
    } else if let Some(session) = options.previous_session.as_ref() {
        crate::session::messages_for_agent(session)
    } else {
        Vec::new()
    };
    let session_id_for_handle = options.previous_session.as_ref().map(|s| s.id.clone());

    let workdir_str = options.workdir.to_string_lossy().to_string();

    let resolved =
        runtime.resolve_with_provider(&options.model, options.provider_override.as_deref())?;
    let model = resolved.canonical_id.clone();
    let provider = resolved.provider.clone();
    let context_limit = if resolved.context_length > 0 {
        resolved.context_length
    } else {
        128000
    };

    let turn_spec = AgentTurnSpec {
        profile_name: "coding".into(),
        model: options.model,
        provider_override: options.provider_override,
        system_prompt: Some(lattice_runtime::coding_system_prompt(
            &options.workdir,
            &options.workdir,
        )),
        prompt_spec: None,
        capabilities: lattice_orchestrator::CapabilitiesConfig {
            presets: vec!["coding".into()],
            bundles: Vec::new(),
        },
        tools: Default::default(),
        behavior: Default::default(),
        handoff: Default::default(),
        memory: Default::default(),
        project_root: Some(workdir_str.clone()),
        cwd: Some(workdir_str),
        thinking_effort: options.thinking_effort,
        preset: Some(lattice_orchestrator::AgentPreset::Coding),
    };
    let task_state_seed =
        load_latest_manifest_metadata(&runtime, options.previous_session.as_ref());
    let handle_id = runtime.get_or_create_agent_handle_for_session_seeded(
        turn_spec,
        options.security.clone(),
        session_id_for_handle,
        messages,
        task_state_seed,
    )?;
    let _snapshot = runtime.agent_handle_snapshot(&handle_id)?;

    Ok(BuiltCodingAgent {
        handle_id,
        runtime,
        model,
        provider,
        context_limit,
    })
}

pub(crate) async fn submit_coding_runtime_turn(
    runtime: &Runtime,
    handle_id: &AgentHandleId,
    request: TurnRequest,
    mut renderer: Option<&mut CodingEventRenderer>,
) -> Result<TurnCompletion> {
    let stream = runtime.submit_turn(handle_id, request).await?;
    tokio::pin!(stream);
    while let Some(event) = stream.next().await {
        if let Some(renderer) = renderer.as_deref_mut() {
            renderer.render_runtime(&event)?;
        }
    }

    let completion = runtime
        .latest_turn_completion(handle_id)?
        .ok_or_else(|| anyhow::anyhow!("runtime turn completed without stored completion"))?;
    require_successful_runtime_completion(completion)
}

pub(crate) fn require_successful_runtime_completion(
    completion: TurnCompletion,
) -> Result<TurnCompletion> {
    match completion.status {
        TurnStatus::Completed => Ok(completion),
        TurnStatus::Cancelled => anyhow::bail!("runtime turn was cancelled"),
        TurnStatus::Error => anyhow::bail!(
            "{}",
            completion.error.as_deref().unwrap_or("runtime turn failed")
        ),
    }
}

/// Load the latest turn manifest metadata from a previous session
/// and deserialize it into a TaskState seed. Returns `None` when
/// the session has no manifests, no metadata, or deserialization fails.
#[allow(dead_code)] // TUI/CLI runtime hard cut no longer reads manifests directly.
pub(crate) fn load_latest_manifest_metadata(
    runtime: &lattice_runtime::Runtime,
    previous_session: Option<&crate::session::Session>,
) -> Option<lattice_agent::TaskState> {
    let session = previous_session?;
    let store = runtime.session_store()?;
    let manifest_ids = store.latest_turn_manifest_ids(&session.id).ok()?;
    let latest_id = match manifest_ids.last() {
        Some(id) => id.clone(),
        None => {
            let summaries = store.list_turn_manifests(&session.id).ok()?;
            summaries.last()?.id.clone()
        }
    };
    let manifest = store.get_turn_manifest(&latest_id).ok()??;
    if manifest.metadata.is_null() {
        return None;
    }
    serde_json::from_value::<lattice_agent::TaskState>(manifest.metadata).ok()
}

pub(crate) fn authenticated_models(credentials: &HashMap<String, String>) -> Vec<String> {
    let runtime = crate::runtime::model_runtime(credentials.clone());
    runtime.list_authenticated_models()
}

pub(crate) fn authenticated_providers(credentials: &HashMap<String, String>) -> Vec<String> {
    let mut providers = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for model in authenticated_models(credentials) {
        if let Some((provider, _)) = model.split_once('/') {
            if seen.insert(provider.to_string()) {
                providers.push(provider.to_string());
            }
        }
    }
    providers
}

#[allow(dead_code)] // Legacy TUI plugin mutation helper keeps this until plugin handles migrate.
pub(crate) fn coding_system_prompt(workdir: &Path) -> String {
    lattice_runtime::coding_system_prompt(workdir, workdir)
}

pub(crate) struct CodingEventRenderer {
    verbose: bool,
    json: bool,
    content_buf: String,
    reasoning_buf: String,
}

impl CodingEventRenderer {
    fn new(verbose: bool, json: bool) -> Self {
        Self {
            verbose,
            json,
            content_buf: String::new(),
            reasoning_buf: String::new(),
        }
    }

    fn render_runtime(&mut self, event: &RuntimeTurnEvent) -> Result<()> {
        match event {
            RuntimeTurnEvent::Token { text, .. } => {
                if !self.json {
                    print!("{text}");
                    std::io::stdout().flush()?;
                }
                self.content_buf.push_str(text);
            }
            RuntimeTurnEvent::Reasoning { text, .. } => {
                self.reasoning_buf.push_str(text);
            }
            RuntimeTurnEvent::ToolStarted {
                name,
                batch_index,
                batch_count,
                ..
            } => {
                self.flush_reasoning();
                if self.verbose && !self.json {
                    if *batch_count > 1 {
                        eprintln!(
                            "\n{} {}/{} {}",
                            "tool".dimmed(),
                            batch_index,
                            batch_count,
                            name
                        );
                    } else {
                        eprintln!("\n{} {}", "tool".dimmed(), name);
                    }
                }
            }
            RuntimeTurnEvent::ToolFinished { name, result, .. } => {
                if self.verbose && !self.json {
                    let preview: String = result.chars().take(240).collect();
                    eprintln!("\n{} {} -> {}", "tool".dimmed(), name, preview);
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

    fn finish(&mut self, total_tokens: Option<u64>, final_report: &str) -> Result<()> {
        self.flush_reasoning();
        if self.json {
            let out = render_final_json(
                &self.content_buf,
                total_tokens.unwrap_or_default(),
                final_report,
            );
            println!("{}", serde_json::to_string_pretty(&out)?);
        } else {
            print!(
                "{}",
                final_report_stdout_suffix(&self.content_buf, final_report)
            );
        }
        std::io::stdout().flush()?;
        Ok(())
    }

    fn flush_reasoning(&mut self) {
        if self.verbose && !self.json && !self.reasoning_buf.is_empty() {
            eprintln!("{} {}", "reasoning:".dimmed(), self.reasoning_buf.trim());
        }
        self.reasoning_buf.clear();
    }
}

pub(crate) fn render_completion_value(completion: &TurnCompletion) -> serde_json::Value {
    render_final_json(
        &completion.assistant_text,
        completion
            .token_usage
            .as_ref()
            .map(|usage| usage.total_tokens)
            .unwrap_or_default()
            .into(),
        completion.final_report.as_deref().unwrap_or_default(),
    )
}

fn render_completion(completion: &TurnCompletion, verbose: bool, json: bool) -> Result<()> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&render_completion_value(completion))?
        );
    } else {
        print!("{}", completion.assistant_text);
        print!(
            "{}",
            final_report_stdout_suffix(
                &completion.assistant_text,
                completion.final_report.as_deref().unwrap_or_default()
            )
        );
    }
    if verbose && !json {
        if let Some(usage) = completion.token_usage.as_ref() {
            eprintln!("\n{}: {} tokens", "usage".dimmed(), usage.total_tokens);
        }
    }
    std::io::stdout().flush()?;
    Ok(())
}

#[cfg(test)]
fn render_final_report_from_state(task_state: &lattice_agent::TaskState, workdir: &Path) -> String {
    let git = lattice_agent::git_status_snapshot(workdir);
    lattice_agent::FinalReport::from_task_state(task_state, git.as_ref()).render_markdown()
}

#[cfg(test)]
fn append_final_report_to_content(content: &str, final_report: &str) -> String {
    if content.trim().is_empty() {
        final_report.to_string()
    } else {
        format!("{}\n\n{}", content.trim_end(), final_report)
    }
}

fn final_report_stdout_suffix(content: &str, final_report: &str) -> String {
    if content.trim().is_empty() {
        format!("{final_report}\n")
    } else if content.ends_with('\n') {
        format!("\n{final_report}\n")
    } else {
        format!("\n\n{final_report}\n")
    }
}

fn render_final_json(content: &str, total_tokens: u64, final_report: &str) -> serde_json::Value {
    serde_json::json!({
        "content": content,
        "final_report": final_report,
        "total_tokens": total_tokens,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_session::SessionStore;

    #[test]
    fn system_prompt_contains_workdir_and_coding_role() {
        let prompt = coding_system_prompt(Path::new("/tmp/project"));

        assert!(prompt.contains("repo-aware coding agent"));
        assert!(prompt.contains("Never overwrite unrelated local edits"));
        assert!(prompt.contains("workspace_context gives a fresh repo snapshot"));
        assert!(prompt.contains("read_file supports offset"));
        assert!(prompt.contains("/tmp/project"));
    }

    #[test]
    fn final_report_non_json_renderer_includes_sections() {
        let mut task_state = lattice_agent::TaskState::new();
        task_state.record_changed_file("src/lib.rs".into(), "update".into(), None);

        let report = render_final_report_from_state(&task_state, Path::new("/not-a-git-repo"));
        let output = format!("Done{}", final_report_stdout_suffix("Done", &report));

        assert!(output.contains("Done\n\n## Changed Files"));
        assert!(output.contains("## Verification"));
        assert!(output.contains("## Residual Risk"));
    }

    #[test]
    fn final_report_json_renderer_includes_report_fields() {
        let report =
            "## Changed Files\n- None\n\n## Verification\n- None\n\n## Residual Risk\n- None\n";

        let value = render_final_json("Done", 123, report);

        assert_eq!(value["content"], "Done");
        assert_eq!(value["final_report"], report);
        assert_eq!(value["total_tokens"], 123);
    }

    #[test]
    fn final_report_session_save_content_contains_verification() {
        let report =
            "## Changed Files\n- None\n\n## Verification\n- None\n\n## Residual Risk\n- None\n";
        let assistant = append_final_report_to_content("Done", report);

        let session = crate::session::finalize_session_turn(
            None,
            "test-model".into(),
            "test-provider".into(),
            "fix it".into(),
            assistant,
        );

        assert!(session
            .messages
            .iter()
            .any(|message| message.role == "assistant"
                && message.content.contains("## Verification")));
    }

    #[test]
    fn final_report_failed_verification_cannot_render_passed() {
        let mut task_state = lattice_agent::TaskState::new();
        task_state.record_verification_failure("cargo test".into(), "FAILED".into());

        let report = render_final_report_from_state(&task_state, Path::new("/not-a-git-repo"));

        assert!(report.contains("[failed] `cargo test`"));
        assert!(!report.contains("[passed]"));
    }

    fn completion_with_status(status: TurnStatus, error: Option<&str>) -> TurnCompletion {
        TurnCompletion {
            handle_id: AgentHandleId::new(),
            turn_id: lattice_runtime::TurnId::new(),
            status,
            assistant_text: String::new(),
            token_usage: None,
            task_state_snapshot: lattice_agent::TaskState::new(),
            final_report: None,
            turn_manifests: Vec::new(),
            model: "test-model".into(),
            provider: "test-provider".into(),
            saved_session_id: None,
            error: error.map(str::to_string),
        }
    }

    #[test]
    fn runtime_error_completion_becomes_cli_error() {
        let err = require_successful_runtime_completion(completion_with_status(
            TurnStatus::Error,
            Some("provider failed"),
        ))
        .unwrap_err();

        assert_eq!(err.to_string(), "provider failed");
    }

    #[test]
    fn runtime_cancelled_completion_becomes_cli_error() {
        let err = require_successful_runtime_completion(completion_with_status(
            TurnStatus::Cancelled,
            None,
        ))
        .unwrap_err();

        assert_eq!(err.to_string(), "runtime turn was cancelled");
    }

    #[test]
    fn runtime_completed_completion_remains_successful() {
        let completion = completion_with_status(TurnStatus::Completed, Some("ignored on success"));

        assert!(require_successful_runtime_completion(completion).is_ok());
    }

    fn task_meta(content: &str, status: &str) -> serde_json::Value {
        serde_json::json!({
            "todos": [{"id": 1, "content": content, "status": status}],
            "changed_files": [],
            "tool_failures": [],
            "verifications": [],
            "unverified_work": []
        })
    }

    fn manifest_with_metadata(
        session_id: &str,
        turn_index: usize,
        request_id: &str,
        metadata: serde_json::Value,
    ) -> lattice_session::NewTurnManifest {
        lattice_session::NewTurnManifest {
            session_id: session_id.to_string(),
            commit_id: None,
            turn_index,
            request_id: request_id.to_string(),
            parent_manifest_ids: Vec::new(),
            model: "test-model".to_string(),
            provider: "test-provider".to_string(),
            prompt: lattice_session::PromptSnapshot {
                total_tokens: 100,
                effective_context_budget: 4096,
                rendered_fragments: Vec::new(),
                withheld_fragments: Vec::new(),
            },
            hooks: Vec::new(),
            tools: Vec::new(),
            metadata,
        }
    }

    #[test]
    fn load_latest_manifest_metadata_round_trips() {
        let dir = std::env::temp_dir().join(format!(
            "lattice-coding-agent-metadata-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let store = std::sync::Arc::new(lattice_session::SqliteCasSessionStore::new(&dir));

        let session = lattice_session::Session::new("test-model".into(), "test-provider".into());

        let task_meta = serde_json::json!({
            "todos": [
                {"id": 1, "content": "Fix auth bug", "status": "in_progress"},
                {"id": 2, "content": "Run tests", "status": "pending"}
            ],
            "changed_files": [
                {"path": "src/auth.rs", "operation": "update", "diagnostic": "line 42"}
            ],
            "tool_failures": [
                {"call_id": "tc-1", "tool_name": "grep", "message": "no results", "timestamp": "2026-05-12T00:00:00Z"}
            ],
            "verifications": [
                {"command": "cargo test", "output_trimmed": "2 failures", "timestamp": "2026-05-12T00:00:01Z"}
            ],
            "unverified_work": ["review auth changes"]
        });

        let manifest = lattice_session::NewTurnManifest {
            session_id: session.id.clone(),
            commit_id: None,
            turn_index: 0,
            request_id: "req-test".to_string(),
            parent_manifest_ids: Vec::new(),
            model: "test-model".to_string(),
            provider: "test-provider".to_string(),
            prompt: lattice_session::PromptSnapshot {
                total_tokens: 100,
                effective_context_budget: 4096,
                rendered_fragments: Vec::new(),
                withheld_fragments: Vec::new(),
            },
            hooks: Vec::new(),
            tools: Vec::new(),
            metadata: task_meta,
        };

        store
            .save_with_manifests(&session, vec![manifest])
            .expect("save_with_manifests should succeed");

        let runtime = lattice_runtime::Runtime::builder()
            .session_store(store)
            .build();

        let seed = super::load_latest_manifest_metadata(&runtime, Some(&session));
        assert!(seed.is_some(), "should load metadata from saved manifest");

        let task_state = seed.unwrap();
        assert_eq!(task_state.todos.len(), 2);
        assert_eq!(task_state.todos[0].content, "Fix auth bug");
        assert_eq!(
            task_state.todos[0].status,
            lattice_agent::TodoStatus::InProgress
        );
        assert_eq!(task_state.todos[1].content, "Run tests");
        assert_eq!(task_state.changed_files.len(), 1);
        assert_eq!(task_state.changed_files[0].path, "src/auth.rs");
        assert_eq!(task_state.tool_failures.len(), 1);
        assert_eq!(task_state.tool_failures[0].tool_name, "grep");
        assert_eq!(task_state.verifications.len(), 1);
        assert_eq!(task_state.verifications[0].command, "cargo test");
        assert_eq!(task_state.unverified_work.len(), 1);
        assert_eq!(task_state.unverified_work[0], "review auth changes");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_latest_manifest_metadata_returns_none_without_session() {
        let runtime = lattice_runtime::Runtime::builder().build();
        let seed = super::load_latest_manifest_metadata(&runtime, None);
        assert!(seed.is_none());
    }

    #[test]
    fn load_latest_manifest_metadata_returns_none_with_empty_metadata() {
        let dir = std::env::temp_dir().join(format!(
            "lattice-coding-agent-empty-meta-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let store = std::sync::Arc::new(lattice_session::SqliteCasSessionStore::new(&dir));

        let session =
            lattice_session::Session::new("empty-meta-model".into(), "empty-meta-provider".into());
        let manifest = lattice_session::NewTurnManifest {
            session_id: session.id.clone(),
            commit_id: None,
            turn_index: 0,
            request_id: "req-empty".to_string(),
            parent_manifest_ids: Vec::new(),
            model: "test-model".to_string(),
            provider: "test-provider".to_string(),
            prompt: lattice_session::PromptSnapshot {
                total_tokens: 100,
                effective_context_budget: 4096,
                rendered_fragments: Vec::new(),
                withheld_fragments: Vec::new(),
            },
            hooks: Vec::new(),
            tools: Vec::new(),
            metadata: serde_json::Value::Null,
        };

        store
            .save_with_manifests(&session, vec![manifest])
            .expect("save_with_manifests should succeed");

        let runtime = lattice_runtime::Runtime::builder()
            .session_store(store)
            .build();

        let seed = super::load_latest_manifest_metadata(&runtime, Some(&session));
        assert!(seed.is_none(), "null metadata should produce None");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_latest_manifest_metadata_uses_latest_turn() {
        let dir = std::env::temp_dir().join(format!(
            "lattice-coding-agent-latest-turn-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let store = std::sync::Arc::new(lattice_session::SqliteCasSessionStore::new(&dir));

        let session = lattice_session::Session::new(
            "latest-turn-model".into(),
            "latest-turn-provider".into(),
        );

        let meta_turn0 = serde_json::json!({
            "todos": [{"id": 1, "content": "Old task", "status": "completed"}],
            "changed_files": [],
            "tool_failures": [],
            "verifications": [],
            "unverified_work": []
        });

        let manifest0 = lattice_session::NewTurnManifest {
            session_id: session.id.clone(),
            commit_id: None,
            turn_index: 0,
            request_id: "req-turn0".to_string(),
            parent_manifest_ids: Vec::new(),
            model: "test-model".to_string(),
            provider: "test-provider".to_string(),
            prompt: lattice_session::PromptSnapshot {
                total_tokens: 50,
                effective_context_budget: 4096,
                rendered_fragments: Vec::new(),
                withheld_fragments: Vec::new(),
            },
            hooks: Vec::new(),
            tools: Vec::new(),
            metadata: meta_turn0,
        };

        let meta_turn1 = serde_json::json!({
            "todos": [{"id": 1, "content": "New task", "status": "in_progress"}],
            "changed_files": [{"path": "src/new.rs", "operation": "add", "diagnostic": null}],
            "tool_failures": [],
            "verifications": [],
            "unverified_work": ["cargo test"]
        });

        let manifest1 = lattice_session::NewTurnManifest {
            session_id: session.id.clone(),
            commit_id: None,
            turn_index: 1,
            request_id: "req-turn1".to_string(),
            parent_manifest_ids: Vec::new(),
            model: "test-model".to_string(),
            provider: "test-provider".to_string(),
            prompt: lattice_session::PromptSnapshot {
                total_tokens: 100,
                effective_context_budget: 4096,
                rendered_fragments: Vec::new(),
                withheld_fragments: Vec::new(),
            },
            hooks: Vec::new(),
            tools: Vec::new(),
            metadata: meta_turn1,
        };

        store
            .save_with_manifests(&session, vec![manifest0])
            .expect("save turn0");
        store
            .save_with_manifests(&session, vec![manifest1])
            .expect("save turn1");

        let runtime = lattice_runtime::Runtime::builder()
            .session_store(store)
            .build();

        let seed = super::load_latest_manifest_metadata(&runtime, Some(&session));
        assert!(seed.is_some());

        let task_state = seed.unwrap();
        assert_eq!(task_state.todos.len(), 1);
        assert_eq!(task_state.todos[0].content, "New task");
        assert_eq!(
            task_state.todos[0].status,
            lattice_agent::TodoStatus::InProgress
        );
        assert_eq!(task_state.changed_files.len(), 1);
        assert_eq!(task_state.changed_files[0].path, "src/new.rs");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_latest_manifest_metadata_uses_latest_commit_when_turn_indexes_repeat() {
        let dir = std::env::temp_dir().join(format!(
            "lattice-coding-agent-repeated-turn-index-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let store = std::sync::Arc::new(lattice_session::SqliteCasSessionStore::new(&dir));

        let session = lattice_session::Session::new(
            "latest-commit-model".into(),
            "latest-commit-provider".into(),
        );

        let old_turn0 = manifest_with_metadata(
            &session.id,
            0,
            "req-old-0",
            task_meta("Old low", "completed"),
        );
        let old_turn2 = manifest_with_metadata(
            &session.id,
            2,
            "req-old-2",
            task_meta("Old stale high index", "in_progress"),
        );
        store
            .save_with_manifests(&session, vec![old_turn0, old_turn2])
            .expect("save old run");

        let new_turn0 = manifest_with_metadata(
            &session.id,
            0,
            "req-new-0",
            task_meta("New current", "in_progress"),
        );
        store
            .save_with_manifests(&session, vec![new_turn0])
            .expect("save new run");

        let runtime = lattice_runtime::Runtime::builder()
            .session_store(store)
            .build();

        let seed = super::load_latest_manifest_metadata(&runtime, Some(&session));
        assert!(seed.is_some());

        let task_state = seed.unwrap();
        assert_eq!(task_state.todos.len(), 1);
        assert_eq!(task_state.todos[0].content, "New current");
        assert_eq!(
            task_state.todos[0].status,
            lattice_agent::TodoStatus::InProgress
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}

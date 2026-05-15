use std::path::{Path, PathBuf};

use futures::StreamExt;
use lattice_runtime::{
    Runtime, RuntimeAgentSession, RuntimeTurnEvent, TurnId, TurnRequest, TurnStatus,
};
use tokio::sync::mpsc::UnboundedSender;

use super::activity::file_snapshot_for_tool;
use super::event::Event;

pub(super) struct AgentBuildRequest {
    pub(super) turn_id: u64,
    pub(super) tx: UnboundedSender<Event>,
    pub(super) runtime: Runtime,
    pub(super) existing_session: Option<RuntimeAgentSession>,
    pub(super) profile_source: lattice_profile::ProfileSource,
    pub(super) agent_label: String,
    pub(super) current_model: String,
    pub(super) previous_model: String,
    pub(super) provider_override: Option<String>,
    pub(super) workdir: PathBuf,
    pub(super) previous_session: Option<crate::session::Session>,
    pub(super) task_state_seed: Option<lattice_agent::TaskState>,
    pub(super) security: crate::security::RuntimeSecurity,
}

pub(super) struct AgentBuildOutcome {
    pub(super) session: RuntimeAgentSession,
    pub(super) rebuilt: Option<RebuiltAgent>,
}

pub(super) struct RebuiltAgent {
    pub(super) model: String,
    pub(super) provider: String,
    pub(super) context_limit: u32,
}

pub(super) enum AgentBuildStatus {
    Ready(AgentBuildOutcome),
    Failed,
}

pub(super) struct AgentStreamRequest {
    pub(super) runtime: Runtime,
    pub(super) session: RuntimeAgentSession,
    pub(super) prompt_text: String,
    pub(super) turn_id: u64,
    pub(super) tx: UnboundedSender<Event>,
    pub(super) workdir: PathBuf,
    pub(super) security: crate::security::RuntimeSecurity,
    pub(super) persist_session: bool,
    pub(super) session_id: Option<String>,
}

struct RuntimeEventContext<'a> {
    tx: &'a UnboundedSender<Event>,
    local_turn_id: u64,
    session_id: &'a str,
    runtime_turn_id: Option<&'a TurnId>,
    workdir: &'a Path,
    security: &'a crate::security::RuntimeSecurity,
}

#[derive(Debug, Clone)]
struct RuntimeTerminal {
    status: TurnStatus,
    runtime_turn_id: TurnId,
    error: Option<String>,
}

/// Build or reuse the runtime-owned session backing the active TUI turn.
pub(super) async fn ensure_agent(request: AgentBuildRequest) -> AgentBuildStatus {
    let session_id = request.previous_session.as_ref().map(|s| s.id.clone());
    let can_reuse = request.current_model == request.previous_model
        && request.existing_session.as_ref().is_some_and(|session| {
            session.source_label == request.agent_label
                && session.spec.model == request.current_model
                && session.invocation.provider_override == request.provider_override
        });

    if can_reuse {
        if let Some(session) = request.existing_session.as_ref() {
            return AgentBuildStatus::Ready(AgentBuildOutcome {
                session: session.clone(),
                rebuilt: None,
            });
        }
    }

    match build_runtime_session(&request, session_id) {
        Ok(outcome) => {
            let _ = request.tx.send(Event::ModelInfo {
                turn_id: request.turn_id,
                model: outcome
                    .rebuilt
                    .as_ref()
                    .map(|rebuilt| rebuilt.model.clone())
                    .unwrap_or_else(|| request.current_model.clone()),
                provider: outcome
                    .rebuilt
                    .as_ref()
                    .map(|rebuilt| rebuilt.provider.clone())
                    .unwrap_or_default(),
            });
            AgentBuildStatus::Ready(outcome)
        }
        Err(e) => {
            let _ = request.tx.send(pack_error(
                request.turn_id,
                format!("agent build failed: {e}"),
            ));
            AgentBuildStatus::Failed
        }
    }
}

fn build_runtime_session(
    request: &AgentBuildRequest,
    session_id: Option<String>,
) -> anyhow::Result<AgentBuildOutcome> {
    let resolved = request
        .runtime
        .resolve_with_provider(&request.current_model, request.provider_override.as_deref())?;
    let context_limit = if resolved.context_length > 0 {
        resolved.context_length
    } else {
        128000
    };
    let task_state_seed = request.task_state_seed.clone().or_else(|| {
        crate::session_seed::load_latest_manifest_metadata(
            &request.runtime,
            request.previous_session.as_ref(),
        )
    });
    let session = crate::profile_runtime::prepare_profile_runtime_session_with_security(
        &request.runtime,
        request.profile_source.clone(),
        request.workdir.clone(),
        crate::profile_runtime::default_global_agents_dir(),
        request.security.clone(),
        Some(request.current_model.clone()),
        request.provider_override.clone(),
        session_id,
        task_state_seed,
    )?;

    Ok(AgentBuildOutcome {
        session,
        rebuilt: Some(RebuiltAgent {
            model: resolved.canonical_id,
            provider: resolved.provider,
            context_limit,
        }),
    })
}

pub(super) fn spawn_stream(request: AgentStreamRequest) {
    tokio::spawn(async move {
        let turn_request = TurnRequest {
            input: request.prompt_text.clone(),
            max_turns: 10,
            persist_session: request.persist_session,
            session_id: request.session_id.clone(),
            cancellation_id: None,
        };
        let submitted = match request
            .runtime
            .submit_session_turn(request.session.clone(), turn_request)
            .await
        {
            Ok(submitted) => submitted,
            Err(err) => {
                let _ = request.tx.send(pack_error(
                    request.turn_id,
                    format!("runtime turn failed: {err}"),
                ));
                return;
            }
        };

        let session_id = submitted.session_id.clone();
        let mut stream = submitted.events;
        let mut runtime_turn_id: Option<TurnId> = None;
        let mut terminal: Option<RuntimeTerminal> = None;
        while let Some(event) = stream.next().await {
            let event_turn_id = runtime_event_turn_id(&event).clone();
            let event_session_id = event_session_id(&event).to_string();
            if runtime_turn_id.is_none() {
                runtime_turn_id = Some(event_turn_id.clone());
                let _ = request.tx.send(Event::RuntimeTurnStarted {
                    turn_id: request.turn_id,
                    session_id: event_session_id.clone(),
                    runtime_turn_id: event_turn_id.clone(),
                });
            }

            let context = RuntimeEventContext {
                tx: &request.tx,
                local_turn_id: request.turn_id,
                session_id: &session_id,
                runtime_turn_id: runtime_turn_id.as_ref(),
                workdir: &request.workdir,
                security: &request.security,
            };
            if let Some(done) = dispatch_runtime_event(context, event) {
                terminal = Some(done);
            }
        }

        let completion = submitted.completion.await.ok();
        if let Some(done) = terminal {
            let _ = request.tx.send(Event::RuntimeTurnFinished {
                turn_id: request.turn_id,
                session_id,
                runtime_turn_id: done.runtime_turn_id,
                status: done.status,
                completion,
                error: done.error,
            });
        }
    });
}

fn pack_error(turn_id: u64, msg: String) -> Event {
    Event::StreamToken {
        turn_id,
        content: msg.clone(),
        reasoning: None,
        done: true,
        error: Some(msg),
    }
}

fn dispatch_runtime_event(
    context: RuntimeEventContext<'_>,
    event: RuntimeTurnEvent,
) -> Option<RuntimeTerminal> {
    let event_session = event_session_id(&event);
    let event_turn = runtime_event_turn_id(&event);
    if event_session != context.session_id {
        return None;
    }
    if context
        .runtime_turn_id
        .is_some_and(|expected| expected != event_turn)
    {
        return None;
    }

    match event {
        RuntimeTurnEvent::Token { text, .. } => {
            let _ = context.tx.send(Event::StreamToken {
                turn_id: context.local_turn_id,
                content: text,
                reasoning: None,
                done: false,
                error: None,
            });
            None
        }
        RuntimeTurnEvent::Reasoning { text, .. } => {
            let _ = context.tx.send(Event::StreamToken {
                turn_id: context.local_turn_id,
                content: String::new(),
                reasoning: Some(text),
                done: false,
                error: None,
            });
            None
        }
        RuntimeTurnEvent::ToolStarted {
            call_id,
            name,
            arguments,
            ..
        } => {
            let file_before =
                file_snapshot_for_tool(&name, &arguments, context.workdir, context.security);
            let _ = context.tx.send(Event::ToolOutput {
                turn_id: context.local_turn_id,
                call_id,
                name,
                arguments,
                result: None,
                file_before,
            });
            None
        }
        RuntimeTurnEvent::ToolFinished {
            call_id,
            name,
            result,
            ..
        } => {
            let _ = context.tx.send(Event::ToolOutput {
                turn_id: context.local_turn_id,
                call_id,
                name,
                arguments: String::new(),
                result: Some(result),
                file_before: None,
            });
            None
        }
        RuntimeTurnEvent::ManifestReady { .. } => None,
        RuntimeTurnEvent::Completed { turn_id, .. } => {
            let _ = context.tx.send(Event::StreamToken {
                turn_id: context.local_turn_id,
                content: String::new(),
                reasoning: None,
                done: true,
                error: None,
            });
            Some(RuntimeTerminal {
                status: TurnStatus::Completed,
                runtime_turn_id: turn_id,
                error: None,
            })
        }
        RuntimeTurnEvent::Cancelled { turn_id, .. } => Some(RuntimeTerminal {
            status: TurnStatus::Cancelled,
            runtime_turn_id: turn_id,
            error: None,
        }),
        RuntimeTurnEvent::Error {
            turn_id, message, ..
        } => {
            let _ = context.tx.send(Event::StreamToken {
                turn_id: context.local_turn_id,
                content: message.clone(),
                reasoning: None,
                done: true,
                error: Some(message.clone()),
            });
            Some(RuntimeTerminal {
                status: TurnStatus::Error,
                runtime_turn_id: turn_id,
                error: Some(message),
            })
        }
    }
}

fn event_session_id(event: &RuntimeTurnEvent) -> &str {
    match event {
        RuntimeTurnEvent::Token { session_id, .. }
        | RuntimeTurnEvent::Reasoning { session_id, .. }
        | RuntimeTurnEvent::ToolStarted { session_id, .. }
        | RuntimeTurnEvent::ToolFinished { session_id, .. }
        | RuntimeTurnEvent::ManifestReady { session_id, .. }
        | RuntimeTurnEvent::Completed { session_id, .. }
        | RuntimeTurnEvent::Cancelled { session_id, .. }
        | RuntimeTurnEvent::Error { session_id, .. } => session_id,
    }
}

fn runtime_event_turn_id(event: &RuntimeTurnEvent) -> &TurnId {
    match event {
        RuntimeTurnEvent::Token { turn_id, .. }
        | RuntimeTurnEvent::Reasoning { turn_id, .. }
        | RuntimeTurnEvent::ToolStarted { turn_id, .. }
        | RuntimeTurnEvent::ToolFinished { turn_id, .. }
        | RuntimeTurnEvent::ManifestReady { turn_id, .. }
        | RuntimeTurnEvent::Completed { turn_id, .. }
        | RuntimeTurnEvent::Cancelled { turn_id, .. }
        | RuntimeTurnEvent::Error { turn_id, .. } => turn_id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_core::catalog::{ApiProtocol, CatalogProviderEntry, ModelCatalogEntry};
    use std::collections::HashMap;
    use tokio::sync::mpsc::unbounded_channel;

    fn test_context<'a>(
        tx: &'a UnboundedSender<Event>,
        session_id: &'a str,
        runtime_turn_id: Option<&'a TurnId>,
        workdir: &'a Path,
        security: &'a crate::security::RuntimeSecurity,
    ) -> RuntimeEventContext<'a> {
        RuntimeEventContext {
            tx,
            local_turn_id: 7,
            session_id,
            runtime_turn_id,
            workdir,
            security,
        }
    }

    fn register_test_model(runtime: &mut Runtime, name: &str) {
        runtime
            .register_model(ModelCatalogEntry {
                canonical_id: name.to_string(),
                context_length: 8192,
                providers: vec![CatalogProviderEntry {
                    provider_id: "ollama".to_string(),
                    api_model_id: name.to_string(),
                    priority: 1,
                    credential_keys: HashMap::new(),
                    base_url: Some("http://127.0.0.1:9".into()),
                    api_protocol: ApiProtocol::OpenAiChat,
                    provider_specific: HashMap::new(),
                }],
                aliases: Vec::new(),
            })
            .unwrap();
    }

    fn build_request(
        tx: UnboundedSender<Event>,
        runtime: Runtime,
        dir: &Path,
        security: crate::security::RuntimeSecurity,
        profile_name: &str,
        model: &str,
        existing_session: Option<RuntimeAgentSession>,
        previous_model: &str,
    ) -> AgentBuildRequest {
        write_test_profile(dir, profile_name, model);
        AgentBuildRequest {
            turn_id: 42,
            tx,
            runtime,
            existing_session,
            profile_source: lattice_profile::ProfileSource::Name(profile_name.into()),
            agent_label: format!("agent:{profile_name}"),
            current_model: model.into(),
            previous_model: previous_model.into(),
            provider_override: None,
            workdir: dir.to_path_buf(),
            previous_session: None,
            task_state_seed: None,
            security,
        }
    }

    fn write_test_profile(project_root: &Path, name: &str, model: &str) {
        let dir = project_root.join(".lattice").join("agents").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("agent.toml"),
            format!(
                r#"
schema_version = 1

[agent]
name = "{name}"
model = "{model}"

[prompt]
system = "You are a deterministic TUI test agent."

[tools]
bundles = ["workspace", "filesystem"]
enabled = []
"#
            ),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn tui_build_request_returns_runtime_session() {
        let mut runtime = Runtime::builder().build();
        register_test_model(&mut runtime, "tui-runtime-session-model");
        let dir = tempfile::tempdir().unwrap();
        let security = crate::security::default_runtime_security(dir.path()).unwrap();
        let (tx, mut rx) = unbounded_channel();

        let status = ensure_agent(build_request(
            tx,
            runtime.clone(),
            dir.path(),
            security,
            "default",
            "tui-runtime-session-model",
            None,
            "",
        ))
        .await;

        let outcome = match status {
            AgentBuildStatus::Ready(outcome) => outcome,
            AgentBuildStatus::Failed => match rx.try_recv() {
                Ok(Event::StreamToken { error, .. }) => {
                    panic!("runtime handle build should succeed: {error:?}")
                }
                _ => panic!("runtime handle build should succeed"),
            },
        };
        assert_eq!(outcome.session.spec.profile_name, "default");
        assert_eq!(outcome.session.spec.model, "tui-runtime-session-model");
        match rx.try_recv().unwrap() {
            Event::ModelInfo {
                turn_id,
                model,
                provider,
            } => {
                assert_eq!(turn_id, 42);
                assert_eq!(model, "tui-runtime-session-model");
                assert_eq!(provider, "ollama");
            }
            _ => panic!("expected model info event"),
        }
    }

    #[tokio::test]
    async fn tui_runtime_uses_selected_profile_source() {
        let mut runtime = Runtime::builder().build();
        register_test_model(&mut runtime, "tui-profile-isolation-model");
        let dir = tempfile::tempdir().unwrap();
        let security = crate::security::default_runtime_security(dir.path()).unwrap();
        let (tx, _rx) = unbounded_channel();
        let normal_session = match ensure_agent(build_request(
            tx.clone(),
            runtime.clone(),
            dir.path(),
            security.clone(),
            "default",
            "tui-profile-isolation-model",
            None,
            "",
        ))
        .await
        {
            AgentBuildStatus::Ready(outcome) => outcome.session,
            AgentBuildStatus::Failed => panic!("normal session should build"),
        };

        let reviewer_session = match ensure_agent(build_request(
            tx,
            runtime.clone(),
            dir.path(),
            security,
            "reviewer",
            "tui-profile-isolation-model",
            Some(normal_session.clone()),
            "tui-profile-isolation-model",
        ))
        .await
        {
            AgentBuildStatus::Ready(outcome) => outcome.session,
            AgentBuildStatus::Failed => panic!("selected profile session should build"),
        };
        assert_ne!(
            reviewer_session.spec.profile_name, normal_session.spec.profile_name,
            "different selected profiles must not reuse the existing session"
        );
    }

    #[test]
    fn runtime_turn_event_runtime_token_event_becomes_stream_token() {
        let (tx, mut rx) = unbounded_channel();
        let runtime_turn_id = TurnId::new();
        let workdir = std::env::current_dir().unwrap();
        let security = crate::security::default_runtime_security(&workdir).unwrap();

        dispatch_runtime_event(
            test_context(
                &tx,
                "runtime-session",
                Some(&runtime_turn_id),
                &workdir,
                &security,
            ),
            RuntimeTurnEvent::Token {
                session_id: "runtime-session".into(),
                turn_id: runtime_turn_id.clone(),
                text: "hello".into(),
            },
        );

        match rx.try_recv().unwrap() {
            Event::StreamToken {
                turn_id,
                content,
                done,
                ..
            } => {
                assert_eq!(turn_id, 7);
                assert_eq!(content, "hello");
                assert!(!done);
            }
            _ => panic!("unexpected event"),
        }
    }

    #[test]
    fn runtime_turn_event_runtime_reasoning_event_becomes_stream_token() {
        let (tx, mut rx) = unbounded_channel();
        let runtime_turn_id = TurnId::new();
        let workdir = std::env::current_dir().unwrap();
        let security = crate::security::default_runtime_security(&workdir).unwrap();

        dispatch_runtime_event(
            test_context(
                &tx,
                "runtime-session",
                Some(&runtime_turn_id),
                &workdir,
                &security,
            ),
            RuntimeTurnEvent::Reasoning {
                session_id: "runtime-session".into(),
                turn_id: runtime_turn_id.clone(),
                text: "thinking".into(),
            },
        );

        match rx.try_recv().unwrap() {
            Event::StreamToken {
                content,
                reasoning,
                done,
                ..
            } => {
                assert!(content.is_empty());
                assert_eq!(reasoning.as_deref(), Some("thinking"));
                assert!(!done);
            }
            _ => panic!("unexpected event"),
        }
    }

    #[test]
    fn runtime_turn_event_runtime_tool_started_event_captures_file_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("notes.txt"), "before").unwrap();
        let security = crate::security::default_runtime_security(dir.path()).unwrap();
        let (tx, mut rx) = unbounded_channel();
        let runtime_turn_id = TurnId::new();
        let arguments = serde_json::json!({
            "path": "notes.txt",
            "content": "after"
        })
        .to_string();

        dispatch_runtime_event(
            test_context(
                &tx,
                "runtime-session",
                Some(&runtime_turn_id),
                dir.path(),
                &security,
            ),
            RuntimeTurnEvent::ToolStarted {
                session_id: "runtime-session".into(),
                turn_id: runtime_turn_id.clone(),
                call_id: "call-1".into(),
                name: "write_file".into(),
                arguments,
                batch_index: 1,
                batch_count: 1,
            },
        );

        match rx.try_recv().unwrap() {
            Event::ToolOutput {
                turn_id,
                call_id,
                file_before,
                ..
            } => {
                assert_eq!(turn_id, 7);
                assert_eq!(call_id, "call-1");
                assert_eq!(file_before.unwrap().content.as_deref(), Some("before"));
            }
            _ => panic!("unexpected event"),
        }
    }

    #[test]
    fn runtime_turn_event_manifest_ready_is_invisible() {
        let (tx, mut rx) = unbounded_channel();
        let runtime_turn_id = TurnId::new();
        let workdir = std::env::current_dir().unwrap();
        let security = crate::security::default_runtime_security(&workdir).unwrap();

        let terminal = dispatch_runtime_event(
            test_context(
                &tx,
                "runtime-session",
                Some(&runtime_turn_id),
                &workdir,
                &security,
            ),
            RuntimeTurnEvent::ManifestReady {
                session_id: "runtime-session".into(),
                turn_id: runtime_turn_id.clone(),
                count: 1,
            },
        );

        assert!(terminal.is_none());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn runtime_turn_event_runtime_completed_event_queries_completion() {
        let (tx, mut rx) = unbounded_channel();
        let runtime_turn_id = TurnId::new();
        let workdir = std::env::current_dir().unwrap();
        let security = crate::security::default_runtime_security(&workdir).unwrap();

        let terminal = dispatch_runtime_event(
            test_context(
                &tx,
                "runtime-session",
                Some(&runtime_turn_id),
                &workdir,
                &security,
            ),
            RuntimeTurnEvent::Completed {
                session_id: "runtime-session".into(),
                turn_id: runtime_turn_id.clone(),
            },
        )
        .expect("completed event should produce terminal status");

        assert_eq!(terminal.status, TurnStatus::Completed);
        assert_eq!(terminal.runtime_turn_id, runtime_turn_id);
        match rx.try_recv().unwrap() {
            Event::StreamToken { done, .. } => assert!(done),
            _ => panic!("unexpected event"),
        }
    }

    #[test]
    fn runtime_turn_event_cancelled_is_terminal_without_transcript_token() {
        let (tx, mut rx) = unbounded_channel();
        let runtime_turn_id = TurnId::new();
        let workdir = std::env::current_dir().unwrap();
        let security = crate::security::default_runtime_security(&workdir).unwrap();

        let terminal = dispatch_runtime_event(
            test_context(
                &tx,
                "runtime-session",
                Some(&runtime_turn_id),
                &workdir,
                &security,
            ),
            RuntimeTurnEvent::Cancelled {
                session_id: "runtime-session".into(),
                turn_id: runtime_turn_id.clone(),
            },
        )
        .expect("cancelled event should produce terminal status");

        assert_eq!(terminal.status, TurnStatus::Cancelled);
        assert_eq!(terminal.runtime_turn_id, runtime_turn_id);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn runtime_turn_event_error_becomes_terminal_stream_error() {
        let (tx, mut rx) = unbounded_channel();
        let runtime_turn_id = TurnId::new();
        let workdir = std::env::current_dir().unwrap();
        let security = crate::security::default_runtime_security(&workdir).unwrap();

        let terminal = dispatch_runtime_event(
            test_context(
                &tx,
                "runtime-session",
                Some(&runtime_turn_id),
                &workdir,
                &security,
            ),
            RuntimeTurnEvent::Error {
                session_id: "runtime-session".into(),
                turn_id: runtime_turn_id.clone(),
                message: "provider failed".into(),
                retryable: false,
            },
        )
        .expect("error event should produce terminal status");

        assert_eq!(terminal.status, TurnStatus::Error);
        assert_eq!(terminal.error.as_deref(), Some("provider failed"));
        match rx.try_recv().unwrap() {
            Event::StreamToken { error, done, .. } => {
                assert_eq!(error.as_deref(), Some("provider failed"));
                assert!(done);
            }
            _ => panic!("unexpected event"),
        }
    }

    #[test]
    fn runtime_turn_event_stale_runtime_event_is_ignored() {
        let (tx, mut rx) = unbounded_channel();
        let runtime_turn_id = TurnId::new();
        let stale_turn_id = TurnId::new();
        let workdir = std::env::current_dir().unwrap();
        let security = crate::security::default_runtime_security(&workdir).unwrap();

        let terminal = dispatch_runtime_event(
            test_context(
                &tx,
                "runtime-session",
                Some(&runtime_turn_id),
                &workdir,
                &security,
            ),
            RuntimeTurnEvent::Token {
                session_id: "runtime-session".into(),
                turn_id: stale_turn_id,
                text: "ignored".into(),
            },
        );

        assert!(terminal.is_none());
        assert!(rx.try_recv().is_err());
    }
}

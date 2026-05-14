use std::path::{Path, PathBuf};

use futures::StreamExt;
use lattice_orchestrator::AgentTurnSpec;
use lattice_runtime::{AgentHandleId, Runtime, RuntimeTurnEvent, TurnId, TurnRequest, TurnStatus};
use tokio::sync::mpsc::UnboundedSender;

use super::activity::file_snapshot_for_tool;
use super::event::Event;

pub(super) struct AgentBuildRequest {
    pub(super) turn_id: u64,
    pub(super) tx: UnboundedSender<Event>,
    pub(super) runtime: Runtime,
    pub(super) existing_handle: Option<AgentHandleId>,
    pub(super) profile_name: String,
    pub(super) current_model: String,
    pub(super) previous_model: String,
    pub(super) provider_override: Option<String>,
    pub(super) workdir: PathBuf,
    pub(super) previous_session: Option<crate::session::Session>,
    pub(super) thinking_effort: Option<String>,
    pub(super) security: crate::security::RuntimeSecurity,
}

pub(super) struct AgentBuildOutcome {
    pub(super) handle_id: AgentHandleId,
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
    pub(super) handle_id: AgentHandleId,
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
    handle_id: &'a AgentHandleId,
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

/// Build or reuse the runtime-owned handle backing the active TUI turn.
pub(super) async fn ensure_agent(request: AgentBuildRequest) -> AgentBuildStatus {
    let spec = runtime_turn_spec(&request);
    let session_id = request.previous_session.as_ref().map(|s| s.id.clone());
    let needs_rebuild =
        request.existing_handle.is_none() || request.current_model != request.previous_model;

    if !needs_rebuild {
        if let Some(handle_id) = request.existing_handle.as_ref() {
            let matches_request = request
                .runtime
                .agent_handle_matches(handle_id, &spec, &request.security, session_id.as_deref())
                .unwrap_or(false);
            if matches_request {
                return AgentBuildStatus::Ready(AgentBuildOutcome {
                    handle_id: handle_id.clone(),
                    rebuilt: None,
                });
            }
        }
    }

    match build_runtime_handle(&request, spec, session_id) {
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

fn build_runtime_handle(
    request: &AgentBuildRequest,
    spec: AgentTurnSpec,
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
    let seed_messages = request
        .previous_session
        .as_ref()
        .map(crate::session::messages_for_agent)
        .unwrap_or_default();
    let task_state_seed = crate::coding_agent::load_latest_manifest_metadata(
        &request.runtime,
        request.previous_session.as_ref(),
    );
    let handle_id = request
        .runtime
        .get_or_create_agent_handle_for_session_seeded(
            spec,
            request.security.clone(),
            session_id,
            seed_messages,
            task_state_seed,
        )?;

    Ok(AgentBuildOutcome {
        handle_id,
        rebuilt: Some(RebuiltAgent {
            model: resolved.canonical_id,
            provider: resolved.provider,
            context_limit,
        }),
    })
}

fn runtime_turn_spec(request: &AgentBuildRequest) -> AgentTurnSpec {
    let workdir = request.workdir.to_string_lossy().to_string();
    AgentTurnSpec {
        profile_name: request.profile_name.clone(),
        model: request.current_model.clone(),
        provider_override: request.provider_override.clone(),
        system_prompt: Some(lattice_runtime::coding_system_prompt(
            &request.workdir,
            &request.workdir,
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
        project_root: Some(workdir.clone()),
        cwd: Some(workdir),
        thinking_effort: request.thinking_effort.clone(),
    }
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
        let stream = match request
            .runtime
            .submit_turn(&request.handle_id, turn_request)
            .await
        {
            Ok(stream) => stream,
            Err(lattice_runtime::RuntimeError::HandleBusy(_)) => {
                let _ = request.tx.send(Event::RuntimeBusy {
                    turn_id: request.turn_id,
                    prompt_text: request.prompt_text,
                });
                return;
            }
            Err(err) => {
                let _ = request.tx.send(pack_error(
                    request.turn_id,
                    format!("runtime turn failed: {err}"),
                ));
                return;
            }
        };

        tokio::pin!(stream);
        let mut runtime_turn_id: Option<TurnId> = None;
        let mut terminal: Option<RuntimeTerminal> = None;
        while let Some(event) = stream.next().await {
            let event_turn_id = runtime_event_turn_id(&event).clone();
            if runtime_turn_id.is_none() {
                runtime_turn_id = Some(event_turn_id.clone());
                let _ = request.tx.send(Event::RuntimeTurnStarted {
                    turn_id: request.turn_id,
                    handle_id: request.handle_id.clone(),
                    runtime_turn_id: event_turn_id.clone(),
                });
            }

            let context = RuntimeEventContext {
                tx: &request.tx,
                local_turn_id: request.turn_id,
                handle_id: &request.handle_id,
                runtime_turn_id: runtime_turn_id.as_ref(),
                workdir: &request.workdir,
                security: &request.security,
            };
            if let Some(done) = dispatch_runtime_event(context, event) {
                terminal = Some(done);
            }
        }

        if let Some(done) = terminal {
            let _ = request.tx.send(Event::RuntimeTurnFinished {
                turn_id: request.turn_id,
                runtime: request.runtime,
                handle_id: request.handle_id,
                runtime_turn_id: done.runtime_turn_id,
                status: done.status,
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
    let event_handle = runtime_event_handle_id(&event);
    let event_turn = runtime_event_turn_id(&event);
    if event_handle != context.handle_id {
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

fn runtime_event_handle_id(event: &RuntimeTurnEvent) -> &AgentHandleId {
    match event {
        RuntimeTurnEvent::Token { handle_id, .. }
        | RuntimeTurnEvent::Reasoning { handle_id, .. }
        | RuntimeTurnEvent::ToolStarted { handle_id, .. }
        | RuntimeTurnEvent::ToolFinished { handle_id, .. }
        | RuntimeTurnEvent::ManifestReady { handle_id, .. }
        | RuntimeTurnEvent::Completed { handle_id, .. }
        | RuntimeTurnEvent::Cancelled { handle_id, .. }
        | RuntimeTurnEvent::Error { handle_id, .. } => handle_id,
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
        handle_id: &'a AgentHandleId,
        runtime_turn_id: Option<&'a TurnId>,
        workdir: &'a Path,
        security: &'a crate::security::RuntimeSecurity,
    ) -> RuntimeEventContext<'a> {
        RuntimeEventContext {
            tx,
            local_turn_id: 7,
            handle_id,
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

    #[tokio::test]
    async fn tui_build_request_returns_runtime_handle() {
        let mut runtime = Runtime::builder().build();
        register_test_model(&mut runtime, "tui-runtime-handle-model");
        let dir = tempfile::tempdir().unwrap();
        let security = crate::security::default_runtime_security(dir.path()).unwrap();
        let (tx, mut rx) = unbounded_channel();

        let status = ensure_agent(AgentBuildRequest {
            turn_id: 42,
            tx,
            runtime: runtime.clone(),
            existing_handle: None,
            profile_name: "coding".into(),
            current_model: "tui-runtime-handle-model".into(),
            previous_model: String::new(),
            provider_override: None,
            workdir: dir.path().to_path_buf(),
            previous_session: None,
            thinking_effort: None,
            security,
        })
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
        let snapshot = runtime.agent_handle_snapshot(&outcome.handle_id).unwrap();
        assert_eq!(snapshot.model, "tui-runtime-handle-model");
        assert_eq!(snapshot.provider, "ollama");
        match rx.try_recv().unwrap() {
            Event::ModelInfo {
                turn_id,
                model,
                provider,
            } => {
                assert_eq!(turn_id, 42);
                assert_eq!(model, "tui-runtime-handle-model");
                assert_eq!(provider, "ollama");
            }
            _ => panic!("expected model info event"),
        }
    }

    #[tokio::test]
    async fn ensure_agent_rejects_existing_handle_with_wrong_profile() {
        let mut runtime = Runtime::builder().build();
        register_test_model(&mut runtime, "tui-profile-isolation-model");
        let dir = tempfile::tempdir().unwrap();
        let security = crate::security::default_runtime_security(dir.path()).unwrap();
        let (tx, _rx) = unbounded_channel();
        let normal_request = AgentBuildRequest {
            turn_id: 1,
            tx: tx.clone(),
            runtime: runtime.clone(),
            existing_handle: None,
            profile_name: "coding".into(),
            current_model: "tui-profile-isolation-model".into(),
            previous_model: String::new(),
            provider_override: None,
            workdir: dir.path().to_path_buf(),
            previous_session: None,
            thinking_effort: None,
            security: security.clone(),
        };
        let normal_handle = match ensure_agent(normal_request).await {
            AgentBuildStatus::Ready(outcome) => outcome.handle_id,
            AgentBuildStatus::Failed => panic!("normal handle should build"),
        };

        let plugin_status = ensure_agent(AgentBuildRequest {
            turn_id: 2,
            tx,
            runtime: runtime.clone(),
            existing_handle: Some(normal_handle.clone()),
            profile_name: "plugin:CodeReview".into(),
            current_model: "tui-profile-isolation-model".into(),
            previous_model: "tui-profile-isolation-model".into(),
            provider_override: None,
            workdir: dir.path().to_path_buf(),
            previous_session: None,
            thinking_effort: None,
            security,
        })
        .await;

        let plugin_handle = match plugin_status {
            AgentBuildStatus::Ready(outcome) => outcome.handle_id,
            AgentBuildStatus::Failed => panic!("plugin handle should build"),
        };
        assert_ne!(
            plugin_handle, normal_handle,
            "plugin requests must not reuse a coding-profile handle"
        );
    }

    #[test]
    fn runtime_turn_event_runtime_token_event_becomes_stream_token() {
        let (tx, mut rx) = unbounded_channel();
        let handle_id = AgentHandleId::new();
        let runtime_turn_id = TurnId::new();
        let workdir = std::env::current_dir().unwrap();
        let security = crate::security::default_runtime_security(&workdir).unwrap();

        dispatch_runtime_event(
            test_context(&tx, &handle_id, Some(&runtime_turn_id), &workdir, &security),
            RuntimeTurnEvent::Token {
                handle_id: handle_id.clone(),
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
        let handle_id = AgentHandleId::new();
        let runtime_turn_id = TurnId::new();
        let workdir = std::env::current_dir().unwrap();
        let security = crate::security::default_runtime_security(&workdir).unwrap();

        dispatch_runtime_event(
            test_context(&tx, &handle_id, Some(&runtime_turn_id), &workdir, &security),
            RuntimeTurnEvent::Reasoning {
                handle_id: handle_id.clone(),
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
        let handle_id = AgentHandleId::new();
        let runtime_turn_id = TurnId::new();
        let arguments = serde_json::json!({
            "path": "notes.txt",
            "content": "after"
        })
        .to_string();

        dispatch_runtime_event(
            test_context(
                &tx,
                &handle_id,
                Some(&runtime_turn_id),
                dir.path(),
                &security,
            ),
            RuntimeTurnEvent::ToolStarted {
                handle_id: handle_id.clone(),
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
        let handle_id = AgentHandleId::new();
        let runtime_turn_id = TurnId::new();
        let workdir = std::env::current_dir().unwrap();
        let security = crate::security::default_runtime_security(&workdir).unwrap();

        let terminal = dispatch_runtime_event(
            test_context(&tx, &handle_id, Some(&runtime_turn_id), &workdir, &security),
            RuntimeTurnEvent::ManifestReady {
                handle_id: handle_id.clone(),
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
        let handle_id = AgentHandleId::new();
        let runtime_turn_id = TurnId::new();
        let workdir = std::env::current_dir().unwrap();
        let security = crate::security::default_runtime_security(&workdir).unwrap();

        let terminal = dispatch_runtime_event(
            test_context(&tx, &handle_id, Some(&runtime_turn_id), &workdir, &security),
            RuntimeTurnEvent::Completed {
                handle_id: handle_id.clone(),
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
        let handle_id = AgentHandleId::new();
        let runtime_turn_id = TurnId::new();
        let workdir = std::env::current_dir().unwrap();
        let security = crate::security::default_runtime_security(&workdir).unwrap();

        let terminal = dispatch_runtime_event(
            test_context(&tx, &handle_id, Some(&runtime_turn_id), &workdir, &security),
            RuntimeTurnEvent::Cancelled {
                handle_id: handle_id.clone(),
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
        let handle_id = AgentHandleId::new();
        let runtime_turn_id = TurnId::new();
        let workdir = std::env::current_dir().unwrap();
        let security = crate::security::default_runtime_security(&workdir).unwrap();

        let terminal = dispatch_runtime_event(
            test_context(&tx, &handle_id, Some(&runtime_turn_id), &workdir, &security),
            RuntimeTurnEvent::Error {
                handle_id: handle_id.clone(),
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
        let handle_id = AgentHandleId::new();
        let other_handle_id = AgentHandleId::new();
        let runtime_turn_id = TurnId::new();
        let workdir = std::env::current_dir().unwrap();
        let security = crate::security::default_runtime_security(&workdir).unwrap();

        let terminal = dispatch_runtime_event(
            test_context(&tx, &handle_id, Some(&runtime_turn_id), &workdir, &security),
            RuntimeTurnEvent::Token {
                handle_id: other_handle_id,
                turn_id: runtime_turn_id.clone(),
                text: "ignored".into(),
            },
        );

        assert!(terminal.is_none());
        assert!(rx.try_recv().is_err());
    }
}

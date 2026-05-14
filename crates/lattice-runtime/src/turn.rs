//! Runtime-owned turn execution: events, requests, completions, and streaming.
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::Arc;

use futures::{FutureExt, Stream};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use lattice_agent::{LoopEvent, TurnManifestObject};
use lattice_core::streaming::TokenUsage;
use lattice_session::{NewTurnManifest, Session, SessionStore};

use crate::AgentHandleId;
use crate::{Runtime, RuntimeError};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TurnId(String);
impl TurnId {
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4().simple().to_string())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl Default for TurnId {
    fn default() -> Self {
        Self::new()
    }
}
impl std::fmt::Display for TurnId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone)]
pub struct TurnRequest {
    pub input: String,
    pub max_turns: u32,
    pub persist_session: bool,
    pub session_id: Option<String>,
    pub cancellation_id: Option<String>,
}

pub use crate::handle::TurnStatus;

#[derive(Debug, Clone)]
pub enum RuntimeTurnEvent {
    Token {
        handle_id: AgentHandleId,
        turn_id: TurnId,
        text: String,
    },
    Reasoning {
        handle_id: AgentHandleId,
        turn_id: TurnId,
        text: String,
    },
    ToolStarted {
        handle_id: AgentHandleId,
        turn_id: TurnId,
        call_id: String,
        name: String,
        arguments: String,
        batch_index: usize,
        batch_count: usize,
    },
    ToolFinished {
        handle_id: AgentHandleId,
        turn_id: TurnId,
        call_id: String,
        name: String,
        result: String,
    },
    ManifestReady {
        handle_id: AgentHandleId,
        turn_id: TurnId,
        count: usize,
    },
    Completed {
        handle_id: AgentHandleId,
        turn_id: TurnId,
    },
    Cancelled {
        handle_id: AgentHandleId,
        turn_id: TurnId,
    },
    Error {
        handle_id: AgentHandleId,
        turn_id: TurnId,
        message: String,
        retryable: bool,
    },
}

#[derive(Debug, Clone)]
pub struct TurnCompletion {
    pub handle_id: AgentHandleId,
    pub turn_id: TurnId,
    pub status: TurnStatus,
    pub assistant_text: String,
    pub token_usage: Option<TokenUsage>,
    pub task_state_snapshot: lattice_agent::TaskState,
    pub final_report: Option<String>,
    pub turn_manifests: Vec<TurnManifestObject>,
    pub model: String,
    pub provider: String,
    pub saved_session_id: Option<String>,
    pub error: Option<String>,
}

pub type TurnStream = Pin<Box<dyn Stream<Item = RuntimeTurnEvent> + Send>>;

impl Runtime {
    pub async fn submit_turn(
        &self,
        handle_id: &AgentHandleId,
        request: TurnRequest,
    ) -> Result<TurnStream, RuntimeError> {
        let turn_id = TurnId::new();
        let hid = handle_id.clone();
        let tid = turn_id.clone();
        let (agent, model, provider, persist_final_report) = {
            let mut registry = self.handles.lock().unwrap();
            let state = registry
                .by_id
                .get_mut(&hid)
                .ok_or_else(|| RuntimeError::HandleNotFound(hid.to_string()))?;
            if state.active_turn {
                return Err(RuntimeError::HandleBusy(hid.to_string()));
            }
            let model = state.model.canonical_id.clone();
            let provider = state.model.provider.clone();
            let persist_final_report =
                state.spec.preset == Some(lattice_orchestrator::AgentPreset::Coding);
            let agent = state.agent.take().ok_or_else(|| {
                RuntimeError::Config("handle has no internal agent; rebuild required".into())
            })?;
            state.active_turn = true;
            state.active_turn_id = Some(turn_id.clone());
            state.active_task_abort = None;
            state.active_assistant_text.clear();
            state.cancellation_requested = false;
            state.last_used_at = std::time::Instant::now();
            (agent, model, provider, persist_final_report)
        };
        let (tx, rx) = mpsc::unbounded_channel::<RuntimeTurnEvent>();
        let handles = Arc::clone(&self.handles);
        let input = request.input.clone();
        let max_turns = request.max_turns;
        let persist_session = request.persist_session;
        let session_id = request.session_id.clone();
        let session_store = self.session_store();
        let completion_tx = tx.clone();
        let task_hid = hid.clone();
        let task_tid = tid.clone();
        let task_handles = Arc::clone(&handles);
        let task_model = model.clone();
        let task_provider = provider.clone();
        let run_task = tokio::spawn(async move {
            let panic_tx = tx.clone();
            let result = AssertUnwindSafe(execute_turn(
                agent,
                ExecuteTurnContext {
                    handle_id: task_hid.clone(),
                    turn_id: task_tid.clone(),
                    input,
                    max_turns,
                    model: task_model.clone(),
                    provider: task_provider.clone(),
                    handles: Arc::clone(&task_handles),
                    session_id,
                    persist_session,
                    persist_final_report,
                    session_store,
                    tx,
                },
            ))
            .catch_unwind()
            .await;
            match result {
                Ok((returned_agent, completion)) => (Some(returned_agent), completion),
                Err(payload) => {
                    let message = panic_payload_message(&payload);
                    let _ = panic_tx.send(RuntimeTurnEvent::Error {
                        handle_id: task_hid.clone(),
                        turn_id: task_tid.clone(),
                        message: message.clone(),
                        retryable: false,
                    });
                    (
                        None,
                        TurnCompletion {
                            handle_id: task_hid.clone(),
                            turn_id: task_tid.clone(),
                            status: TurnStatus::Error,
                            assistant_text: String::new(),
                            token_usage: None,
                            task_state_snapshot: lattice_agent::TaskState::new(),
                            final_report: None,
                            turn_manifests: Vec::new(),
                            model: task_model,
                            provider: task_provider,
                            saved_session_id: None,
                            error: Some(message),
                        },
                    )
                }
            }
        });
        let abort_handle = run_task.abort_handle();
        {
            let mut registry = self.handles.lock().unwrap();
            if let Some(state) = registry.by_id.get_mut(&hid) {
                state.active_task_abort = Some(abort_handle);
            }
        }
        let runtime_for_abort = self.clone();
        tokio::spawn(async move {
            let (returned_agent, completion) = match run_task.await {
                Ok(result) => result,
                Err(join_error) if join_error.is_cancelled() => {
                    let assistant_text = active_assistant_text(&handles, &hid, &tid);
                    let replacement_agent =
                        replacement_agent_after_abort(&runtime_for_abort, &handles, &hid);
                    let _ = completion_tx.send(RuntimeTurnEvent::Cancelled {
                        handle_id: hid.clone(),
                        turn_id: tid.clone(),
                    });
                    (
                        replacement_agent,
                        TurnCompletion {
                            handle_id: hid.clone(),
                            turn_id: tid.clone(),
                            status: TurnStatus::Cancelled,
                            assistant_text,
                            token_usage: None,
                            task_state_snapshot: lattice_agent::TaskState::new(),
                            final_report: None,
                            turn_manifests: Vec::new(),
                            model,
                            provider,
                            saved_session_id: None,
                            error: None,
                        },
                    )
                }
                Err(join_error) => {
                    let message = format!("turn task join failed: {join_error}");
                    let _ = completion_tx.send(RuntimeTurnEvent::Error {
                        handle_id: hid.clone(),
                        turn_id: tid.clone(),
                        message: message.clone(),
                        retryable: false,
                    });
                    (
                        None,
                        TurnCompletion {
                            handle_id: hid.clone(),
                            turn_id: tid.clone(),
                            status: TurnStatus::Error,
                            assistant_text: String::new(),
                            token_usage: None,
                            task_state_snapshot: lattice_agent::TaskState::new(),
                            final_report: None,
                            turn_manifests: Vec::new(),
                            model,
                            provider,
                            saved_session_id: None,
                            error: Some(message),
                        },
                    )
                }
            };
            let mut registry = handles.lock().unwrap();
            let limit = registry.recent_completion_limit;
            if let Some(state) = registry.by_id.get_mut(&hid) {
                state.agent = returned_agent;
                state.active_turn = false;
                state.active_turn_id = None;
                state.active_task_abort = None;
                state.active_assistant_text.clear();
                state.cancellation_requested = false;
                while state.recent_completions.len() >= limit {
                    state.recent_completions.pop_front();
                }
                state.recent_completions.push_back(completion);
            }
        });
        Ok(Box::pin(futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|event| (event, rx))
        })))
    }

    pub fn cancel_turn(&self, handle_id: &AgentHandleId) -> Result<(), RuntimeError> {
        let mut registry = self.handles.lock().unwrap();
        let state = registry
            .by_id
            .get_mut(handle_id)
            .ok_or_else(|| RuntimeError::HandleNotFound(handle_id.to_string()))?;
        if !state.active_turn || state.active_turn_id.is_none() {
            return Err(RuntimeError::TurnNotFound(format!(
                "no active turn for handle {handle_id}"
            )));
        }
        state.cancellation_requested = true;
        if let Some(task) = state.active_task_abort.take() {
            task.abort();
        }
        state.last_used_at = std::time::Instant::now();
        Ok(())
    }

    pub fn latest_turn_completion(
        &self,
        handle_id: &AgentHandleId,
    ) -> Result<Option<TurnCompletion>, RuntimeError> {
        let registry = self.handles.lock().unwrap();
        let state = registry
            .by_id
            .get(handle_id)
            .ok_or_else(|| RuntimeError::HandleNotFound(handle_id.to_string()))?;
        Ok(state.recent_completions.back().cloned())
    }
}

struct ExecuteTurnContext {
    handle_id: AgentHandleId,
    turn_id: TurnId,
    input: String,
    max_turns: u32,
    model: String,
    provider: String,
    handles: Arc<std::sync::Mutex<crate::handle::AgentHandleRegistry>>,
    session_id: Option<String>,
    persist_session: bool,
    persist_final_report: bool,
    session_store: Option<Arc<dyn SessionStore>>,
    tx: mpsc::UnboundedSender<RuntimeTurnEvent>,
}

async fn execute_turn(
    mut agent: lattice_agent::Agent,
    ctx: ExecuteTurnContext,
) -> (lattice_agent::Agent, TurnCompletion) {
    let ExecuteTurnContext {
        handle_id: hid,
        turn_id: tid,
        input,
        max_turns,
        model,
        provider,
        handles,
        session_id,
        persist_session,
        persist_final_report,
        session_store,
        tx,
    } = ctx;
    let mut assistant_text = String::new();
    let mut manifests: Vec<TurnManifestObject> = Vec::new();
    let mut token_usage: Option<TokenUsage> = None;
    let mut status = TurnStatus::Completed;
    let mut error_msg: Option<String> = None;

    let events = agent
        .run_streaming(&input, max_turns, |event: LoopEvent| match event {
            LoopEvent::Token { text } => {
                if turn_cancelled(&handles, &hid, &tid) {
                    return;
                }
                record_active_assistant_text(&handles, &hid, &tid, &text);
                assistant_text.push_str(&text);
                let _ = tx.send(RuntimeTurnEvent::Token {
                    handle_id: hid.clone(),
                    turn_id: tid.clone(),
                    text,
                });
            }
            LoopEvent::Reasoning { text } => {
                if turn_cancelled(&handles, &hid, &tid) {
                    return;
                }
                let _ = tx.send(RuntimeTurnEvent::Reasoning {
                    handle_id: hid.clone(),
                    turn_id: tid.clone(),
                    text,
                });
            }
            LoopEvent::ToolCallRequired { calls } => {
                if turn_cancelled(&handles, &hid, &tid) {
                    return;
                }
                let batch_count = calls.len();
                for (batch_index, call) in calls.iter().enumerate() {
                    let _ = tx.send(RuntimeTurnEvent::ToolStarted {
                        handle_id: hid.clone(),
                        turn_id: tid.clone(),
                        call_id: call.id.clone(),
                        name: call.function.name.clone(),
                        arguments: call.function.arguments.clone(),
                        batch_index: batch_index + 1,
                        batch_count,
                    });
                }
            }
            LoopEvent::ToolResult { call, result } => {
                if turn_cancelled(&handles, &hid, &tid) {
                    return;
                }
                let _ = tx.send(RuntimeTurnEvent::ToolFinished {
                    handle_id: hid.clone(),
                    turn_id: tid.clone(),
                    call_id: call.id.clone(),
                    name: call.function.name.clone(),
                    result: result.clone(),
                });
            }
            LoopEvent::Manifest { manifest } => {
                if turn_cancelled(&handles, &hid, &tid) {
                    return;
                }
                manifests.push(manifest);
                let _ = tx.send(RuntimeTurnEvent::ManifestReady {
                    handle_id: hid.clone(),
                    turn_id: tid.clone(),
                    count: manifests.len(),
                });
            }
            LoopEvent::Done { usage } => {
                token_usage = usage;
            }
            LoopEvent::Error { message, retryable } => {
                if turn_cancelled(&handles, &hid, &tid) {
                    return;
                }
                status = TurnStatus::Error;
                error_msg = Some(message.clone());
                let _ = tx.send(RuntimeTurnEvent::Error {
                    handle_id: hid.clone(),
                    turn_id: tid.clone(),
                    message,
                    retryable,
                });
            }
        })
        .await;

    if turn_cancelled(&handles, &hid, &tid) {
        status = TurnStatus::Cancelled;
        error_msg = None;
    } else {
        for event in &events {
            if let LoopEvent::Error { message, .. } = event {
                status = TurnStatus::Error;
                error_msg = Some(message.clone());
                break;
            }
        }
    }

    let task_state_snapshot = agent.task_state().clone();
    let final_report = (status != TurnStatus::Cancelled).then(|| {
        lattice_agent::FinalReport::from_task_state(&task_state_snapshot, None).render_markdown()
    });
    let mut saved_session_id = None;
    if status == TurnStatus::Completed && persist_session {
        let persisted_assistant = if persist_final_report {
            assistant_text_with_final_report(&assistant_text, final_report.as_deref())
        } else {
            assistant_text.clone()
        };
        match session_store {
            Some(store) => match persist_turn_session(
                store.as_ref(),
                session_id.as_deref(),
                &model,
                &provider,
                &input,
                &persisted_assistant,
                &manifests,
            ) {
                Ok(id) => saved_session_id = Some(id),
                Err(err) => {
                    status = TurnStatus::Error;
                    error_msg = Some(format!("failed to persist session: {err}"));
                }
            },
            None => {
                status = TurnStatus::Error;
                error_msg =
                    Some("persist_session requested but runtime has no session store".into());
            }
        }
    }

    match status {
        TurnStatus::Completed => {
            let _ = tx.send(RuntimeTurnEvent::Completed {
                handle_id: hid.clone(),
                turn_id: tid.clone(),
            });
        }
        TurnStatus::Cancelled => {
            let _ = tx.send(RuntimeTurnEvent::Cancelled {
                handle_id: hid.clone(),
                turn_id: tid.clone(),
            });
        }
        TurnStatus::Error => {
            if let Some(message) = error_msg.clone() {
                let already_emitted = events.iter().any(|event| {
                    matches!(event, LoopEvent::Error { message: event_message, .. } if event_message == &message)
                });
                if !already_emitted {
                    let _ = tx.send(RuntimeTurnEvent::Error {
                        handle_id: hid.clone(),
                        turn_id: tid.clone(),
                        message,
                        retryable: false,
                    });
                }
            }
        }
    }

    let completion = TurnCompletion {
        handle_id: hid,
        turn_id: tid,
        status,
        assistant_text,
        token_usage,
        task_state_snapshot,
        final_report,
        turn_manifests: manifests,
        model,
        provider,
        saved_session_id,
        error: error_msg,
    };
    (agent, completion)
}

fn turn_cancelled(
    handles: &Arc<std::sync::Mutex<crate::handle::AgentHandleRegistry>>,
    handle_id: &AgentHandleId,
    turn_id: &TurnId,
) -> bool {
    let registry = handles.lock().unwrap();
    registry.by_id.get(handle_id).is_some_and(|state| {
        state.active_turn_id.as_ref() == Some(turn_id) && state.cancellation_requested
    })
}

fn record_active_assistant_text(
    handles: &Arc<std::sync::Mutex<crate::handle::AgentHandleRegistry>>,
    handle_id: &AgentHandleId,
    turn_id: &TurnId,
    text: &str,
) {
    let mut registry = handles.lock().unwrap();
    if let Some(state) = registry.by_id.get_mut(handle_id) {
        if state.active_turn_id.as_ref() == Some(turn_id) {
            state.active_assistant_text.push_str(text);
        }
    }
}

fn active_assistant_text(
    handles: &Arc<std::sync::Mutex<crate::handle::AgentHandleRegistry>>,
    handle_id: &AgentHandleId,
    turn_id: &TurnId,
) -> String {
    let registry = handles.lock().unwrap();
    registry
        .by_id
        .get(handle_id)
        .filter(|state| state.active_turn_id.as_ref() == Some(turn_id))
        .map(|state| state.active_assistant_text.clone())
        .unwrap_or_default()
}

fn replacement_agent_after_abort(
    runtime: &Runtime,
    handles: &Arc<std::sync::Mutex<crate::handle::AgentHandleRegistry>>,
    handle_id: &AgentHandleId,
) -> Option<lattice_agent::Agent> {
    let (spec, security) = {
        let registry = handles.lock().unwrap();
        let state = registry.by_id.get(handle_id)?;
        (state.spec.clone(), state.security.clone())
    };
    let resolved = runtime
        .resolve_with_provider(&spec.model, spec.provider_override.as_deref())
        .ok()?;
    runtime
        .build_internal_agent(&spec, &resolved, &security)
        .ok()
}

fn assistant_text_with_final_report(assistant_text: &str, final_report: Option<&str>) -> String {
    match final_report {
        Some(report) if !report.trim().is_empty() && assistant_text.trim().is_empty() => {
            report.to_string()
        }
        Some(report) if !report.trim().is_empty() => {
            format!("{}\n\n{}", assistant_text.trim_end(), report)
        }
        _ => assistant_text.to_string(),
    }
}

fn persist_turn_session(
    store: &dyn SessionStore,
    requested_session_id: Option<&str>,
    model: &str,
    provider: &str,
    input: &str,
    assistant_text: &str,
    manifests: &[TurnManifestObject],
) -> Result<String, lattice_session::SessionError> {
    let mut session = if let Some(id) = requested_session_id {
        match store.load(id)? {
            Some(session) => session,
            None => {
                let mut session = Session::new(model.to_string(), provider.to_string());
                session.id = id.to_string();
                session
            }
        }
    } else {
        Session::new(model.to_string(), provider.to_string())
    };

    session.append_turn(
        model.to_string(),
        provider.to_string(),
        input.to_string(),
        assistant_text.to_string(),
    );
    let saved_session_id = session.id.clone();
    let new_manifests = manifests
        .iter()
        .enumerate()
        .map(|(index, manifest)| {
            manifest_to_new(manifest, &saved_session_id, model, provider, index)
        })
        .collect::<Vec<_>>();

    if new_manifests.is_empty() {
        store.save(&session)?;
    } else {
        match store.save_with_manifests(&session, new_manifests) {
            Ok(()) => {}
            Err(lattice_session::SessionError::Path(message))
                if message.contains("save_with_manifests is not implemented") =>
            {
                store.save(&session)?;
            }
            Err(err) => return Err(err),
        }
    }

    Ok(saved_session_id)
}

fn manifest_to_new(
    manifest: &TurnManifestObject,
    session_id: &str,
    model: &str,
    provider: &str,
    turn_index: usize,
) -> NewTurnManifest {
    NewTurnManifest {
        session_id: session_id.to_string(),
        commit_id: None,
        turn_index,
        request_id: manifest.request_id.clone(),
        parent_manifest_ids: Vec::new(),
        model: model.to_string(),
        provider: provider.to_string(),
        prompt: lattice_session::PromptSnapshot {
            total_tokens: manifest.prompt_snapshot.token_count,
            effective_context_budget: 0,
            rendered_fragments: manifest
                .prompt_snapshot
                .rendered_fragments
                .iter()
                .map(fragment_to_session)
                .collect(),
            withheld_fragments: manifest
                .prompt_snapshot
                .withheld_fragments
                .iter()
                .map(fragment_to_session)
                .collect(),
        },
        hooks: manifest
            .hook_decisions
            .iter()
            .enumerate()
            .map(|(order, hook)| lattice_session::HookDecisionRecord {
                point: String::new(),
                hook_name: hook.hook_name.clone(),
                order,
                decision_kind: hook_decision_kind(&hook.decision),
                target_fragment_id: None,
                reason: None,
                before_hash: Some(hook.before_hash.clone()),
                after_hash: Some(hook.after_hash.clone()),
            })
            .collect(),
        tools: manifest
            .tool_decisions
            .iter()
            .map(|tool| lattice_session::ToolDecisionRecord {
                tool_name: tool.tool_name.clone(),
                decision_kind: "executed".to_string(),
                command_hash: Some(tool.arguments.clone()),
                command_preview: None,
                result_hash: Some(tool.result_hash.clone()),
                result_preview: None,
            })
            .collect(),
        metadata: manifest.metadata.clone().unwrap_or(serde_json::Value::Null),
    }
}

fn fragment_to_session(
    fragment: &lattice_agent::FragmentRecord,
) -> lattice_session::PromptFragmentRecord {
    lattice_session::PromptFragmentRecord {
        id: fragment.id.clone(),
        layer: fragment.layer.clone(),
        role: fragment.role.clone(),
        authority: fragment.authority.clone(),
        source: fragment.source.clone(),
        cache_scope: fragment.cache_scope.clone(),
        priority: fragment.priority,
        tokens: fragment.tokens,
        content_hash: fragment.content_hash.clone(),
        content_preview: fragment.content_preview.clone(),
        rendered: fragment.rendered,
        withheld_reason: fragment.withheld_reason.clone(),
    }
}

fn hook_decision_kind(decision: &str) -> String {
    serde_json::from_str::<serde_json::Value>(decision)
        .ok()
        .and_then(|value| {
            value
                .get("kind")
                .and_then(|kind| kind.as_str())
                .or_else(|| value.get("decision").and_then(|kind| kind.as_str()))
                .map(str::to_string)
        })
        .unwrap_or_else(|| decision.to_string())
}

fn panic_payload_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        format!("turn task panicked: {message}")
    } else if let Some(message) = payload.downcast_ref::<String>() {
        format!("turn task panicked: {message}")
    } else {
        "turn task panicked".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use lattice_core::catalog::{ApiProtocol, CatalogProviderEntry, ModelCatalogEntry};
    use lattice_core::types::{Message, Role};
    use lattice_orchestrator::AgentTurnSpec;
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    #[test]
    fn turn_id_unique() {
        assert_ne!(TurnId::new(), TurnId::new());
    }

    #[test]
    fn turn_types_exist() {
        let _req = TurnRequest {
            input: "hi".into(),
            max_turns: 5,
            persist_session: false,
            session_id: None,
            cancellation_id: None,
        };
    }

    #[tokio::test]
    async fn runtime_turn_stream_emits_semantic_events() {
        let Some(port) =
            spawn_scripted_openai_server(vec![ScriptedResponse::content("hello runtime")]).await
        else {
            eprintln!("skipping runtime turn stream test: loopback bind denied");
            return;
        };
        let (runtime, root) = make_test_runtime("runtime-turn-stream", port, false);
        let handle_id = make_test_handle(&runtime, "runtime-turn-stream-model", &root);

        let stream = runtime
            .submit_turn(&handle_id, turn_request("say hello", false, None))
            .await
            .unwrap();
        tokio::pin!(stream);

        let mut saw_token = false;
        let mut saw_completed = false;
        while let Some(event) = stream.next().await {
            match event {
                RuntimeTurnEvent::Token {
                    handle_id: event_handle,
                    text,
                    ..
                } => {
                    assert_eq!(event_handle, handle_id);
                    assert_eq!(text, "hello runtime");
                    saw_token = true;
                }
                RuntimeTurnEvent::Completed {
                    handle_id: event_handle,
                    ..
                } => {
                    assert_eq!(event_handle, handle_id);
                    saw_completed = true;
                }
                RuntimeTurnEvent::Error { message, .. } => {
                    panic!("unexpected runtime turn error: {message}");
                }
                _ => {}
            }
        }

        assert!(
            saw_token,
            "runtime stream should emit semantic token events"
        );
        assert!(
            saw_completed,
            "runtime stream should emit a terminal completed event"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn runtime_turn_busy_guard_rejects_same_handle_concurrency() {
        let Some(port) = spawn_scripted_openai_server(vec![ScriptedResponse::content_after(
            "slow response",
            Duration::from_millis(250),
        )])
        .await
        else {
            eprintln!("skipping runtime busy guard test: loopback bind denied");
            return;
        };
        let (runtime, root) = make_test_runtime("runtime-turn-busy", port, false);
        let handle_id = make_test_handle(&runtime, "runtime-turn-busy-model", &root);

        let stream = runtime
            .submit_turn(&handle_id, turn_request("first", false, None))
            .await
            .unwrap();
        let second = runtime
            .submit_turn(&handle_id, turn_request("second", false, None))
            .await;

        assert!(
            matches!(second, Err(RuntimeError::HandleBusy(id)) if id == handle_id.to_string()),
            "same-handle concurrent submit should return HandleBusy"
        );

        tokio::pin!(stream);
        while stream.next().await.is_some() {}
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn runtime_turn_completion_contains_authoritative_fields() {
        let Some(port) =
            spawn_scripted_openai_server(vec![ScriptedResponse::content("stored answer")]).await
        else {
            eprintln!("skipping runtime completion test: loopback bind denied");
            return;
        };
        let (runtime, root) = make_test_runtime("runtime-turn-completion", port, true);
        let handle_id = make_coding_test_handle(&runtime, "runtime-turn-completion-model", &root);

        let stream = runtime
            .submit_turn(
                &handle_id,
                turn_request("persist this", true, Some("runtime-session")),
            )
            .await
            .unwrap();
        tokio::pin!(stream);
        while stream.next().await.is_some() {}

        let completion = runtime
            .latest_turn_completion(&handle_id)
            .unwrap()
            .expect("completion should be stored after stream drains");
        assert_eq!(completion.handle_id, handle_id);
        assert_eq!(completion.status, TurnStatus::Completed);
        assert_eq!(completion.assistant_text, "stored answer");
        assert_eq!(completion.model, "runtime-turn-completion-model");
        assert_eq!(completion.provider, "ollama");
        assert!(
            completion.task_state_snapshot.is_empty(),
            "task state snapshot should be present and cloneable"
        );
        assert!(
            completion
                .final_report
                .as_deref()
                .is_some_and(|report| report.contains("Verification")),
            "completed turns should carry the state-derived final report"
        );
        assert_eq!(
            completion.saved_session_id.as_deref(),
            Some("runtime-session")
        );
        let store = runtime
            .session_store()
            .expect("test runtime should expose a session store");
        let session = store
            .load("runtime-session")
            .unwrap()
            .expect("persisted session should be readable");
        assert!(
            session
                .messages
                .iter()
                .any(|message| message.role == "assistant"
                    && message.content.contains("## Verification")),
            "persisted assistant message should include the final report"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn seeded_session_handle_includes_prior_messages_and_task_metadata() {
        let Some((port, requests)) =
            spawn_scripted_openai_server_with_requests(vec![ScriptedResponse::content(
                "seeded ok",
            )])
            .await
        else {
            eprintln!("skipping seeded handle test: loopback bind denied");
            return;
        };
        let (runtime, root) = make_test_runtime("runtime-seeded-handle", port, false);
        let mut task_state_seed = lattice_agent::TaskState::new();
        task_state_seed.record_verification_failure("cargo test".into(), "not run".into());
        let spec = AgentTurnSpec {
            profile_name: "runtime-test-agent".into(),
            model: "runtime-seeded-handle-model".into(),
            provider_override: None,
            system_prompt: Some("You are a deterministic test agent.".into()),
            prompt_spec: None,
            capabilities: Default::default(),
            tools: Default::default(),
            behavior: Default::default(),
            handoff: Default::default(),
            memory: Default::default(),
            project_root: Some(root.to_string_lossy().to_string()),
            cwd: Some(root.to_string_lossy().to_string()),
            thinking_effort: None,
            preset: None,
        };
        let security =
            crate::build_runtime_security(&crate::RuntimeSecurityConfig::default(), &root).unwrap();
        let handle_id = runtime
            .get_or_create_agent_handle_for_session_seeded(
                spec,
                security,
                Some("seeded-session".into()),
                vec![
                    Message {
                        role: Role::User,
                        content: "earlier user marker".into(),
                        reasoning_content: None,
                        tool_calls: None,
                        tool_call_id: None,
                        name: None,
                    },
                    Message {
                        role: Role::Assistant,
                        content: "earlier assistant marker".into(),
                        reasoning_content: None,
                        tool_calls: None,
                        tool_call_id: None,
                        name: None,
                    },
                ],
                Some(task_state_seed),
            )
            .unwrap();

        let stream = runtime
            .submit_turn(
                &handle_id,
                turn_request("new request marker", false, Some("seeded-session")),
            )
            .await
            .unwrap();
        tokio::pin!(stream);
        while stream.next().await.is_some() {}

        let request = requests.lock().unwrap().join("\n");
        assert!(request.contains("earlier user marker"));
        assert!(request.contains("earlier assistant marker"));
        assert!(request.contains("new request marker"));
        let snapshot = runtime.agent_handle_snapshot(&handle_id).unwrap();
        assert!(
            !snapshot.task_state_empty,
            "fresh seeded handles should preserve task metadata"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn runtime_turn_cancel_marks_completion_cancelled() {
        let Some(port) = spawn_scripted_openai_server(vec![ScriptedResponse::chunked_content(
            "partial",
            " ignored",
            Duration::from_millis(250),
        )])
        .await
        else {
            eprintln!("skipping runtime cancellation test: loopback bind denied");
            return;
        };
        let (runtime, root) = make_test_runtime("runtime-turn-cancel", port, false);
        let handle_id = make_test_handle(&runtime, "runtime-turn-cancel-model", &root);
        let stream = runtime
            .submit_turn(&handle_id, turn_request("cancel me", false, None))
            .await
            .unwrap();
        tokio::pin!(stream);

        let first = stream.next().await.expect("first token should arrive");
        assert!(
            matches!(first, RuntimeTurnEvent::Token { ref text, .. } if text == "partial"),
            "first event should expose partial assistant text"
        );
        runtime.cancel_turn(&handle_id).unwrap();

        let mut saw_cancelled = false;
        while let Some(event) = stream.next().await {
            if matches!(event, RuntimeTurnEvent::Cancelled { .. }) {
                saw_cancelled = true;
            }
        }

        assert!(saw_cancelled, "cancelled turns should emit Cancelled");
        let completion = runtime
            .latest_turn_completion(&handle_id)
            .unwrap()
            .expect("cancelled completion should be stored");
        assert_eq!(completion.status, TurnStatus::Cancelled);
        assert_eq!(completion.error, None);
        assert_eq!(completion.final_report, None);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn runtime_turn_cancel_preserves_partial_assistant_text() {
        let Some(port) = spawn_scripted_openai_server(vec![ScriptedResponse::chunked_content(
            "kept",
            " dropped",
            Duration::from_millis(250),
        )])
        .await
        else {
            eprintln!("skipping runtime partial cancellation test: loopback bind denied");
            return;
        };
        let (runtime, root) = make_test_runtime("runtime-turn-cancel-partial", port, false);
        let handle_id = make_test_handle(&runtime, "runtime-turn-cancel-partial-model", &root);
        let stream = runtime
            .submit_turn(
                &handle_id,
                turn_request("cancel after partial", false, None),
            )
            .await
            .unwrap();
        tokio::pin!(stream);

        while let Some(event) = stream.next().await {
            if matches!(event, RuntimeTurnEvent::Token { .. }) {
                runtime.cancel_turn(&handle_id).unwrap();
                break;
            }
        }
        while stream.next().await.is_some() {}

        let completion = runtime
            .latest_turn_completion(&handle_id)
            .unwrap()
            .expect("cancelled completion should be stored");
        assert_eq!(completion.status, TurnStatus::Cancelled);
        assert_eq!(completion.assistant_text, "kept");
        assert!(!completion.assistant_text.contains("dropped"));
        let snapshot = runtime.agent_handle_snapshot(&handle_id).unwrap();
        assert!(
            !snapshot.active_turn,
            "internal agent should be returned after cancellation"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn runtime_turn_cancel_aborts_before_later_tool_side_effects() {
        let Some(port) =
            spawn_scripted_openai_server(vec![ScriptedResponse::content_then_tool_call_after(
                "partial",
                "write_file",
                serde_json::json!({
                    "path": "cancelled-side-effect.txt",
                    "content": "should not be written"
                })
                .to_string(),
                Duration::from_millis(250),
            )])
            .await
        else {
            eprintln!("skipping runtime cancellation side-effect test: loopback bind denied");
            return;
        };
        let (runtime, root) = make_test_runtime("runtime-turn-cancel-tools", port, false);
        let handle_id = make_coding_test_handle(&runtime, "runtime-turn-cancel-tools-model", &root);
        let target = root.join("cancelled-side-effect.txt");
        let stream = runtime
            .submit_turn(
                &handle_id,
                turn_request("cancel before tool call", false, None),
            )
            .await
            .unwrap();
        tokio::pin!(stream);

        let first = stream.next().await.expect("first token should arrive");
        assert!(
            matches!(first, RuntimeTurnEvent::Token { ref text, .. } if text == "partial"),
            "first event should expose partial assistant text"
        );
        runtime.cancel_turn(&handle_id).unwrap();

        let mut saw_cancelled = false;
        let mut saw_tool_event = false;
        while let Some(event) = stream.next().await {
            if matches!(event, RuntimeTurnEvent::Cancelled { .. }) {
                saw_cancelled = true;
            }
            if matches!(
                event,
                RuntimeTurnEvent::ToolStarted { .. } | RuntimeTurnEvent::ToolFinished { .. }
            ) {
                saw_tool_event = true;
            }
        }

        assert!(saw_cancelled, "cancelled turns should emit Cancelled");
        assert!(!saw_tool_event, "cancelled turns must not emit later tools");
        assert!(
            !target.exists(),
            "cancelled turns must not execute later mutating tool calls"
        );
        let completion = runtime
            .latest_turn_completion(&handle_id)
            .unwrap()
            .expect("cancelled completion should be stored");
        assert_eq!(completion.status, TurnStatus::Cancelled);
        assert_eq!(completion.assistant_text, "partial");
        let _ = std::fs::remove_dir_all(root);
    }

    fn make_test_runtime(label: &str, port: u16, with_session_store: bool) -> (Runtime, PathBuf) {
        let root = temp_test_dir(label);
        let mut builder = Runtime::builder();
        if with_session_store {
            builder = builder.session_store(std::sync::Arc::new(
                lattice_session::FileSessionStore::new(root.join("sessions")),
            ));
        }
        let mut runtime = builder.build();
        register_scripted_model(&mut runtime, &format!("{label}-model"), port);
        (runtime, root)
    }

    fn make_test_handle(runtime: &Runtime, model: &str, root: &Path) -> AgentHandleId {
        let spec = AgentTurnSpec {
            profile_name: "runtime-test-agent".into(),
            model: model.into(),
            provider_override: None,
            system_prompt: Some("You are a deterministic test agent.".into()),
            prompt_spec: None,
            capabilities: Default::default(),
            tools: Default::default(),
            behavior: Default::default(),
            handoff: Default::default(),
            memory: Default::default(),
            project_root: Some(root.to_string_lossy().to_string()),
            cwd: Some(root.to_string_lossy().to_string()),
            thinking_effort: None,
            preset: None,
        };
        let security =
            crate::build_runtime_security(&crate::RuntimeSecurityConfig::default(), root).unwrap();
        runtime
            .get_or_create_agent_handle_for_session(spec, security, Some("session".into()))
            .unwrap()
    }

    fn make_coding_test_handle(runtime: &Runtime, model: &str, root: &Path) -> AgentHandleId {
        let spec = AgentTurnSpec {
            profile_name: "runtime-test-coding-agent".into(),
            model: model.into(),
            provider_override: None,
            system_prompt: Some(crate::coding_system_prompt(root, root)),
            prompt_spec: None,
            capabilities: Default::default(),
            tools: Default::default(),
            behavior: Default::default(),
            handoff: Default::default(),
            memory: Default::default(),
            project_root: Some(root.to_string_lossy().to_string()),
            cwd: Some(root.to_string_lossy().to_string()),
            thinking_effort: None,
            preset: Some(lattice_orchestrator::AgentPreset::Coding),
        };
        let security =
            crate::build_runtime_security(&crate::RuntimeSecurityConfig::default(), root).unwrap();
        runtime
            .get_or_create_agent_handle_for_session(spec, security, Some("session".into()))
            .unwrap()
    }

    fn turn_request(input: &str, persist_session: bool, session_id: Option<&str>) -> TurnRequest {
        TurnRequest {
            input: input.into(),
            max_turns: 2,
            persist_session,
            session_id: session_id.map(str::to_string),
            cancellation_id: None,
        }
    }

    fn register_scripted_model(runtime: &mut Runtime, name: &str, port: u16) {
        runtime
            .register_model(ModelCatalogEntry {
                canonical_id: name.to_string(),
                context_length: 8192,
                providers: vec![CatalogProviderEntry {
                    provider_id: "ollama".to_string(),
                    api_model_id: name.to_string(),
                    priority: 1,
                    credential_keys: HashMap::new(),
                    base_url: Some(format!("http://127.0.0.1:{port}")),
                    api_protocol: ApiProtocol::OpenAiChat,
                    provider_specific: HashMap::new(),
                }],
                aliases: Vec::new(),
            })
            .unwrap();
    }

    fn temp_test_dir(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "lattice-{label}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    struct ScriptedResponse {
        chunks: Vec<ResponseChunk>,
    }

    struct ResponseChunk {
        body: String,
        delay_after: Duration,
    }

    impl ScriptedResponse {
        fn content(text: &str) -> Self {
            Self {
                chunks: vec![ResponseChunk {
                    body: content_chunk(text) + &stop_chunk() + "data: [DONE]\n\n",
                    delay_after: Duration::ZERO,
                }],
            }
        }

        fn content_after(text: &str, delay: Duration) -> Self {
            Self {
                chunks: vec![
                    ResponseChunk {
                        body: String::new(),
                        delay_after: delay,
                    },
                    ResponseChunk {
                        body: content_chunk(text) + &stop_chunk() + "data: [DONE]\n\n",
                        delay_after: Duration::ZERO,
                    },
                ],
            }
        }

        fn chunked_content(first: &str, second: &str, delay_between: Duration) -> Self {
            Self {
                chunks: vec![
                    ResponseChunk {
                        body: content_chunk(first),
                        delay_after: delay_between,
                    },
                    ResponseChunk {
                        body: content_chunk(second) + &stop_chunk() + "data: [DONE]\n\n",
                        delay_after: Duration::ZERO,
                    },
                ],
            }
        }

        fn content_then_tool_call_after(
            first: &str,
            tool_name: &str,
            arguments: String,
            delay_between: Duration,
        ) -> Self {
            Self {
                chunks: vec![
                    ResponseChunk {
                        body: content_chunk(first),
                        delay_after: delay_between,
                    },
                    ResponseChunk {
                        body: tool_call_chunk(tool_name, &arguments)
                            + &tool_call_stop_chunk()
                            + "data: [DONE]\n\n",
                        delay_after: Duration::ZERO,
                    },
                ],
            }
        }
    }

    async fn spawn_scripted_openai_server(responses: Vec<ScriptedResponse>) -> Option<u16> {
        spawn_scripted_openai_server_with_requests(responses)
            .await
            .map(|(port, _requests)| port)
    }

    async fn spawn_scripted_openai_server_with_requests(
        responses: Vec<ScriptedResponse>,
    ) -> Option<(u16, Arc<Mutex<Vec<String>>>)> {
        let listener = match TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => return None,
            Err(error) => panic!("failed to bind scripted OpenAI server: {error}"),
        };
        let port = listener.local_addr().unwrap().port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured_requests = Arc::clone(&requests);

        tokio::spawn(async move {
            for response in responses {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let request = read_http_request(&mut stream).await;
                captured_requests.lock().unwrap().push(request);
                write_sse_response(&mut stream, response).await;
            }
        });

        Some((port, requests))
    }

    async fn write_sse_response(stream: &mut TcpStream, response: ScriptedResponse) {
        let headers = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n";
        let _ = stream.write_all(headers.as_bytes()).await;
        for chunk in response.chunks {
            let _ = stream.write_all(chunk.body.as_bytes()).await;
            let _ = stream.flush().await;
            if !chunk.delay_after.is_zero() {
                tokio::time::sleep(chunk.delay_after).await;
            }
        }
    }

    async fn read_http_request(stream: &mut TcpStream) -> String {
        let mut data = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let Ok(n) = stream.read(&mut buf).await else {
                break;
            };
            if n == 0 {
                break;
            }
            data.extend_from_slice(&buf[..n]);
            if request_complete(&data) || data.len() > 2 * 1024 * 1024 {
                break;
            }
        }
        String::from_utf8_lossy(&data).to_string()
    }

    fn request_complete(data: &[u8]) -> bool {
        let Some(header_end) = data.windows(4).position(|window| window == b"\r\n\r\n") else {
            return false;
        };
        let headers = String::from_utf8_lossy(&data[..header_end]);
        let content_length = headers.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        });
        match content_length {
            Some(len) => data.len() >= header_end + 4 + len,
            None => true,
        }
    }

    fn content_chunk(text: &str) -> String {
        let chunk = serde_json::json!({
            "choices": [{
                "index": 0,
                "delta": { "role": "assistant", "content": text },
                "finish_reason": null
            }]
        });
        format!("data: {}\n\n", chunk)
    }

    fn stop_chunk() -> String {
        let chunk = serde_json::json!({
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "stop"
            }]
        });
        format!("data: {}\n\n", chunk)
    }

    fn tool_call_chunk(name: &str, arguments: &str) -> String {
        let chunk = serde_json::json!({
            "choices": [{
                "index": 0,
                "delta": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_cancelled_side_effect",
                        "type": "function",
                        "function": {
                            "name": name,
                            "arguments": arguments
                        }
                    }]
                },
                "finish_reason": null
            }]
        });
        format!("data: {}\n\n", chunk)
    }

    fn tool_call_stop_chunk() -> String {
        let chunk = serde_json::json!({
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "tool_calls"
            }]
        });
        format!("data: {}\n\n", chunk)
    }
}

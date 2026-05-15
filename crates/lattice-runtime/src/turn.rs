//! Runtime-owned generic session turn execution.

use std::pin::Pin;
use std::sync::Arc;

use futures::Stream;
use lattice_agent::{LoopEvent, TurnManifestObject};
use lattice_core::streaming::TokenUsage;
use lattice_core::types::{Message, Role};
use lattice_session::{
    CommitTurnRequest, NewTurnManifest, OpenTurnRequest, SessionMessage, SessionStore,
    TurnOutcomeStatus,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};

use crate::{Runtime, RuntimeAgentSession, RuntimeError};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TurnStatus {
    Completed,
    Cancelled,
    Error,
}

#[derive(Debug, Clone)]
pub enum RuntimeTurnEvent {
    Token {
        session_id: String,
        turn_id: TurnId,
        text: String,
    },
    Reasoning {
        session_id: String,
        turn_id: TurnId,
        text: String,
    },
    ToolStarted {
        session_id: String,
        turn_id: TurnId,
        call_id: String,
        name: String,
        arguments: String,
        batch_index: usize,
        batch_count: usize,
    },
    ToolFinished {
        session_id: String,
        turn_id: TurnId,
        call_id: String,
        name: String,
        result: String,
    },
    ManifestReady {
        session_id: String,
        turn_id: TurnId,
        count: usize,
    },
    Completed {
        session_id: String,
        turn_id: TurnId,
    },
    Cancelled {
        session_id: String,
        turn_id: TurnId,
    },
    Error {
        session_id: String,
        turn_id: TurnId,
        message: String,
        retryable: bool,
    },
}

#[derive(Debug, Clone)]
pub struct TurnCompletion {
    pub session_id: String,
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
    pub profile_drift_warning: Option<String>,
    pub error: Option<String>,
}

pub type TurnStream = Pin<Box<dyn Stream<Item = RuntimeTurnEvent> + Send>>;

pub struct SubmittedTurn {
    pub session_id: String,
    pub turn_id: TurnId,
    pub events: TurnStream,
    pub completion: oneshot::Receiver<TurnCompletion>,
}

impl Runtime {
    pub async fn submit_session_turn(
        &self,
        session: RuntimeAgentSession,
        request: TurnRequest,
    ) -> Result<SubmittedTurn, RuntimeError> {
        if !request.persist_session {
            return self.submit_transient_session_turn(session, request).await;
        }

        let store = self.session_store().ok_or_else(|| {
            RuntimeError::Config("submit_session_turn requires a session store".into())
        })?;
        let opened = store
            .open_turn(OpenTurnRequest {
                session_id: request.session_id.clone().or(session.session_id.clone()),
                profile_snapshot: session.profile_snapshot.clone(),
                current_source_fingerprint: session.source_fingerprint.clone(),
            })
            .map_err(|err| RuntimeError::Config(format!("failed to open session turn: {err}")))?;
        let locked_spec: lattice_profile::GenericAgentSpec =
            serde_json::from_value(opened.profile_snapshot.canonical_spec.clone()).map_err(
                |err| RuntimeError::Config(format!("locked profile snapshot is invalid: {err}")),
            )?;
        let history = messages_from_session_messages(&opened.history);
        let mut assembly =
            self.assemble_generic_agent(&locked_spec, &session.invocation, history)?;
        assembly.agent = assembly.agent.with_session_context(
            opened.session_id.clone(),
            "main",
            opened.base_commit_id.clone().unwrap_or_default(),
        );

        let model = assembly.model.canonical_id.clone();
        let provider = assembly.model.provider.clone();
        let session_id = opened.session_id.clone();
        let turn_id = TurnId(opened.turn_id.clone());
        let active_key = (session_id.clone(), turn_id.to_string());
        {
            let active_turns = self.session_turns.lock().unwrap();
            if active_turns
                .keys()
                .any(|(active_session, _)| active_session == &session_id)
            {
                return Err(RuntimeError::TurnBusy(format!(
                    "session {session_id} already has an active turn"
                )));
            }
        }

        let profile_drift_warning = opened
            .profile_drift
            .as_ref()
            .map(|drift| drift.warning.clone());
        let profile_hash = opened.profile_snapshot.hash.clone();
        let base_commit_id = opened.base_commit_id.clone();
        let turn_index = opened.turn_index;
        let parent_manifest_ids = opened.parent_manifest_ids.clone();
        let (tx, rx) = mpsc::unbounded_channel::<RuntimeTurnEvent>();
        let (completion_tx, completion_rx) = oneshot::channel();
        let input = request.input.clone();
        let max_turns = request.max_turns;
        let execute_store = Arc::clone(&store);
        let execute_tx = tx.clone();
        let execute_session_id = session_id.clone();
        let execute_turn_id = turn_id.clone();
        let execute_model = model.clone();
        let execute_provider = provider.clone();
        let execute_profile_hash = profile_hash.clone();
        let execute_base_commit_id = base_commit_id.clone();
        let execute_parent_manifest_ids = parent_manifest_ids.clone();
        let execute_drift_warning = profile_drift_warning.clone();

        let run_task = tokio::spawn(async move {
            execute_session_turn(
                assembly.agent,
                SessionExecuteTurnContext {
                    session_id: execute_session_id,
                    turn_id: execute_turn_id,
                    input,
                    max_turns,
                    model: execute_model,
                    provider: execute_provider,
                    store: execute_store,
                    profile_hash: execute_profile_hash,
                    base_commit_id: execute_base_commit_id,
                    turn_index,
                    parent_manifest_ids: execute_parent_manifest_ids,
                    profile_drift_warning: execute_drift_warning,
                    tx: execute_tx,
                },
            )
            .await
        });
        let abort_handle = run_task.abort_handle();
        self.session_turns
            .lock()
            .unwrap()
            .insert(active_key.clone(), abort_handle);

        let active_turns = Arc::clone(&self.session_turns);
        let stream_session_id = session_id.clone();
        let stream_turn_id = turn_id.clone();
        tokio::spawn(async move {
            let completion = match run_task.await {
                Ok(completion) => completion,
                Err(join_error) if join_error.is_cancelled() => commit_cancelled_session_turn(
                    stream_session_id,
                    stream_turn_id,
                    model,
                    provider,
                    store,
                    profile_hash,
                    base_commit_id,
                    turn_index,
                    parent_manifest_ids,
                    profile_drift_warning,
                    request.input.clone(),
                    tx,
                ),
                Err(join_error) => commit_error_session_turn(
                    stream_session_id,
                    stream_turn_id,
                    model,
                    provider,
                    store,
                    profile_hash,
                    base_commit_id,
                    turn_index,
                    parent_manifest_ids,
                    profile_drift_warning,
                    request.input.clone(),
                    format!("turn task join failed: {join_error}"),
                    tx,
                ),
            };
            active_turns.lock().unwrap().remove(&active_key);
            let _ = completion_tx.send(completion);
        });

        Ok(SubmittedTurn {
            session_id,
            turn_id,
            events: Box::pin(futures::stream::unfold(rx, |mut rx| async move {
                rx.recv().await.map(|event| (event, rx))
            })),
            completion: completion_rx,
        })
    }

    async fn submit_transient_session_turn(
        &self,
        session: RuntimeAgentSession,
        request: TurnRequest,
    ) -> Result<SubmittedTurn, RuntimeError> {
        let requested_session_id = request.session_id.clone().or(session.session_id.clone());
        let session_id = requested_session_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string());
        let history = match (self.session_store(), requested_session_id.as_deref()) {
            (Some(store), Some(existing_session_id)) => store
                .load(existing_session_id)
                .map_err(|err| {
                    RuntimeError::Config(format!("failed to load transient session history: {err}"))
                })?
                .map(|session| messages_from_session_messages(&session.messages))
                .unwrap_or_default(),
            _ => Vec::new(),
        };

        let mut assembly =
            self.assemble_generic_agent(&session.spec, &session.invocation, history)?;
        assembly.agent =
            assembly
                .agent
                .with_session_context(session_id.clone(), "main", String::new());

        let model = assembly.model.canonical_id.clone();
        let provider = assembly.model.provider.clone();
        let turn_id = TurnId::new();
        let active_key = (session_id.clone(), turn_id.to_string());
        {
            let active_turns = self.session_turns.lock().unwrap();
            if active_turns
                .keys()
                .any(|(active_session, _)| active_session == &session_id)
            {
                return Err(RuntimeError::TurnBusy(format!(
                    "session {session_id} already has an active turn"
                )));
            }
        }

        let profile_drift_warning = session
            .profile_snapshot
            .drift_for(session.source_fingerprint.clone())
            .map(|drift| drift.warning);
        let (tx, rx) = mpsc::unbounded_channel::<RuntimeTurnEvent>();
        let (completion_tx, completion_rx) = oneshot::channel();
        let execute_session_id = session_id.clone();
        let execute_turn_id = turn_id.clone();
        let execute_input = request.input.clone();
        let execute_model = model.clone();
        let execute_provider = provider.clone();
        let execute_drift_warning = profile_drift_warning.clone();
        let execute_tx = tx.clone();

        let run_task = tokio::spawn(async move {
            let collected = collect_session_turn(
                assembly.agent,
                &execute_session_id,
                &execute_turn_id,
                &execute_input,
                request.max_turns,
                &execute_tx,
            )
            .await;
            complete_transient_session_turn(
                collected.task_state_snapshot,
                collected.status,
                collected.error_msg,
                TransientCompletionContext {
                    session_id: execute_session_id,
                    turn_id: execute_turn_id,
                    assistant_text: collected.assistant_text,
                    token_usage: collected.token_usage,
                    manifests: collected.manifests,
                    model: execute_model,
                    provider: execute_provider,
                    profile_drift_warning: execute_drift_warning,
                    tx: execute_tx,
                },
            )
        });
        let abort_handle = run_task.abort_handle();
        self.session_turns
            .lock()
            .unwrap()
            .insert(active_key.clone(), abort_handle);

        let active_turns = Arc::clone(&self.session_turns);
        let stream_session_id = session_id.clone();
        let stream_turn_id = turn_id.clone();
        tokio::spawn(async move {
            let completion = match run_task.await {
                Ok(completion) => completion,
                Err(join_error) if join_error.is_cancelled() => complete_transient_session_turn(
                    lattice_agent::TaskState::new(),
                    TurnStatus::Cancelled,
                    None,
                    TransientCompletionContext {
                        session_id: stream_session_id,
                        turn_id: stream_turn_id,
                        assistant_text: String::new(),
                        token_usage: None,
                        manifests: Vec::new(),
                        model,
                        provider,
                        profile_drift_warning,
                        tx,
                    },
                ),
                Err(join_error) => complete_transient_session_turn(
                    lattice_agent::TaskState::new(),
                    TurnStatus::Error,
                    Some(format!("turn task join failed: {join_error}")),
                    TransientCompletionContext {
                        session_id: stream_session_id,
                        turn_id: stream_turn_id,
                        assistant_text: String::new(),
                        token_usage: None,
                        manifests: Vec::new(),
                        model,
                        provider,
                        profile_drift_warning,
                        tx,
                    },
                ),
            };
            active_turns.lock().unwrap().remove(&active_key);
            let _ = completion_tx.send(completion);
        });

        Ok(SubmittedTurn {
            session_id,
            turn_id,
            events: Box::pin(futures::stream::unfold(rx, |mut rx| async move {
                rx.recv().await.map(|event| (event, rx))
            })),
            completion: completion_rx,
        })
    }

    pub fn cancel_session_turn(
        &self,
        session_id: &str,
        turn_id: &TurnId,
    ) -> Result<(), RuntimeError> {
        let Some(abort) = self
            .session_turns
            .lock()
            .unwrap()
            .remove(&(session_id.to_string(), turn_id.to_string()))
        else {
            return Err(RuntimeError::TurnNotFound(format!(
                "no active turn {turn_id} for session {session_id}"
            )));
        };
        abort.abort();
        Ok(())
    }
}

struct SessionExecuteTurnContext {
    session_id: String,
    turn_id: TurnId,
    input: String,
    max_turns: u32,
    model: String,
    provider: String,
    store: Arc<dyn SessionStore>,
    profile_hash: String,
    base_commit_id: Option<String>,
    turn_index: usize,
    parent_manifest_ids: Vec<String>,
    profile_drift_warning: Option<String>,
    tx: mpsc::UnboundedSender<RuntimeTurnEvent>,
}

struct CollectedSessionTurn {
    assistant_text: String,
    manifests: Vec<TurnManifestObject>,
    token_usage: Option<TokenUsage>,
    task_state_snapshot: lattice_agent::TaskState,
    status: TurnStatus,
    error_msg: Option<String>,
}

async fn collect_session_turn(
    mut agent: lattice_agent::Agent,
    session_id: &str,
    turn_id: &TurnId,
    input: &str,
    max_turns: u32,
    tx: &mpsc::UnboundedSender<RuntimeTurnEvent>,
) -> CollectedSessionTurn {
    let mut assistant_text = String::new();
    let mut manifests = Vec::new();
    let mut token_usage = None;
    let mut status = TurnStatus::Completed;
    let mut error_msg = None;

    let events = agent
        .run_streaming(input, max_turns, |event: LoopEvent| match event {
            LoopEvent::Token { text } => {
                assistant_text.push_str(&text);
                let _ = tx.send(RuntimeTurnEvent::Token {
                    session_id: session_id.to_string(),
                    turn_id: turn_id.clone(),
                    text,
                });
            }
            LoopEvent::Reasoning { text } => {
                let _ = tx.send(RuntimeTurnEvent::Reasoning {
                    session_id: session_id.to_string(),
                    turn_id: turn_id.clone(),
                    text,
                });
            }
            LoopEvent::ToolCallRequired { calls } => {
                let batch_count = calls.len();
                for (batch_index, call) in calls.iter().enumerate() {
                    let _ = tx.send(RuntimeTurnEvent::ToolStarted {
                        session_id: session_id.to_string(),
                        turn_id: turn_id.clone(),
                        call_id: call.id.clone(),
                        name: call.function.name.clone(),
                        arguments: call.function.arguments.clone(),
                        batch_index: batch_index + 1,
                        batch_count,
                    });
                }
            }
            LoopEvent::ToolResult { call, result } => {
                let _ = tx.send(RuntimeTurnEvent::ToolFinished {
                    session_id: session_id.to_string(),
                    turn_id: turn_id.clone(),
                    call_id: call.id.clone(),
                    name: call.function.name.clone(),
                    result: result.clone(),
                });
            }
            LoopEvent::Manifest { manifest } => {
                manifests.push(manifest);
                let _ = tx.send(RuntimeTurnEvent::ManifestReady {
                    session_id: session_id.to_string(),
                    turn_id: turn_id.clone(),
                    count: manifests.len(),
                });
            }
            LoopEvent::Done { usage } => {
                token_usage = usage;
            }
            LoopEvent::Error { message, retryable } => {
                status = TurnStatus::Error;
                error_msg = Some(message.clone());
                let _ = tx.send(RuntimeTurnEvent::Error {
                    session_id: session_id.to_string(),
                    turn_id: turn_id.clone(),
                    message,
                    retryable,
                });
            }
        })
        .await;

    for event in &events {
        if let LoopEvent::Error { message, .. } = event {
            status = TurnStatus::Error;
            error_msg = Some(message.clone());
            break;
        }
    }

    CollectedSessionTurn {
        assistant_text,
        manifests,
        token_usage,
        task_state_snapshot: agent.task_state().clone(),
        status,
        error_msg,
    }
}

async fn execute_session_turn(
    agent: lattice_agent::Agent,
    ctx: SessionExecuteTurnContext,
) -> TurnCompletion {
    let SessionExecuteTurnContext {
        session_id,
        turn_id,
        input,
        max_turns,
        model,
        provider,
        store,
        profile_hash,
        base_commit_id,
        turn_index,
        parent_manifest_ids,
        profile_drift_warning,
        tx,
    } = ctx;
    let collected =
        collect_session_turn(agent, &session_id, &turn_id, &input, max_turns, &tx).await;

    commit_session_turn(
        collected.task_state_snapshot,
        collected.status,
        collected.error_msg,
        SessionCommitContext {
            session_id,
            turn_id,
            input,
            assistant_text: collected.assistant_text,
            token_usage: collected.token_usage,
            manifests: collected.manifests,
            model,
            provider,
            store,
            profile_hash,
            base_commit_id,
            turn_index,
            parent_manifest_ids,
            profile_drift_warning,
            tx,
        },
    )
}

struct SessionCommitContext {
    session_id: String,
    turn_id: TurnId,
    input: String,
    assistant_text: String,
    token_usage: Option<TokenUsage>,
    manifests: Vec<TurnManifestObject>,
    model: String,
    provider: String,
    store: Arc<dyn SessionStore>,
    profile_hash: String,
    base_commit_id: Option<String>,
    turn_index: usize,
    parent_manifest_ids: Vec<String>,
    profile_drift_warning: Option<String>,
    tx: mpsc::UnboundedSender<RuntimeTurnEvent>,
}

struct TransientCompletionContext {
    session_id: String,
    turn_id: TurnId,
    assistant_text: String,
    token_usage: Option<TokenUsage>,
    manifests: Vec<TurnManifestObject>,
    model: String,
    provider: String,
    profile_drift_warning: Option<String>,
    tx: mpsc::UnboundedSender<RuntimeTurnEvent>,
}

fn complete_transient_session_turn(
    task_state_snapshot: lattice_agent::TaskState,
    status: TurnStatus,
    error_msg: Option<String>,
    ctx: TransientCompletionContext,
) -> TurnCompletion {
    let TransientCompletionContext {
        session_id,
        turn_id,
        assistant_text,
        token_usage,
        manifests,
        model,
        provider,
        profile_drift_warning,
        tx,
    } = ctx;
    let final_report = (status != TurnStatus::Cancelled).then(|| {
        lattice_agent::FinalReport::from_task_state(&task_state_snapshot, None).render_markdown()
    });

    match status {
        TurnStatus::Completed => {
            let _ = tx.send(RuntimeTurnEvent::Completed {
                session_id: session_id.clone(),
                turn_id: turn_id.clone(),
            });
        }
        TurnStatus::Cancelled => {
            let _ = tx.send(RuntimeTurnEvent::Cancelled {
                session_id: session_id.clone(),
                turn_id: turn_id.clone(),
            });
        }
        TurnStatus::Error => {
            if let Some(message) = error_msg.clone() {
                let _ = tx.send(RuntimeTurnEvent::Error {
                    session_id: session_id.clone(),
                    turn_id: turn_id.clone(),
                    message,
                    retryable: false,
                });
            }
        }
    }

    TurnCompletion {
        session_id,
        turn_id,
        status,
        assistant_text,
        token_usage,
        task_state_snapshot,
        final_report,
        turn_manifests: manifests,
        model,
        provider,
        saved_session_id: None,
        profile_drift_warning,
        error: error_msg,
    }
}

fn commit_session_turn(
    task_state_snapshot: lattice_agent::TaskState,
    mut status: TurnStatus,
    mut error_msg: Option<String>,
    ctx: SessionCommitContext,
) -> TurnCompletion {
    let SessionCommitContext {
        session_id,
        turn_id,
        input,
        assistant_text,
        token_usage,
        manifests,
        model,
        provider,
        store,
        profile_hash,
        base_commit_id,
        turn_index,
        parent_manifest_ids,
        profile_drift_warning,
        tx,
    } = ctx;
    let final_report = (status != TurnStatus::Cancelled).then(|| {
        lattice_agent::FinalReport::from_task_state(&task_state_snapshot, None).render_markdown()
    });
    let persisted_assistant = if status == TurnStatus::Completed {
        assistant_text_with_final_report(&assistant_text, final_report.as_deref())
    } else {
        assistant_text.clone()
    };
    let turn_status = match status {
        TurnStatus::Completed => TurnOutcomeStatus::Completed,
        TurnStatus::Error => TurnOutcomeStatus::Error,
        TurnStatus::Cancelled => TurnOutcomeStatus::Cancelled,
    };
    let mut new_manifests = manifests
        .iter()
        .enumerate()
        .map(|(index, manifest)| {
            let mut manifest =
                manifest_to_new(manifest, &session_id, &model, &provider, turn_index + index);
            manifest.parent_manifest_ids = parent_manifest_ids.clone();
            if let Some(warning) = &profile_drift_warning {
                add_profile_drift_metadata(&mut manifest, warning);
            }
            manifest
        })
        .collect::<Vec<_>>();
    if new_manifests.is_empty() {
        let mut manifest = empty_turn_manifest(&session_id, &model, &provider, turn_index);
        manifest.parent_manifest_ids = parent_manifest_ids;
        if let Some(warning) = &profile_drift_warning {
            add_profile_drift_metadata(&mut manifest, warning);
        }
        new_manifests.push(manifest);
    }

    let mut saved_session_id = Some(session_id.clone());
    let messages = vec![
        SessionMessage {
            role: "user".to_string(),
            content: input,
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
            artifact_ids: Vec::new(),
            compact: None,
        },
        SessionMessage {
            role: "assistant".to_string(),
            content: persisted_assistant,
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
            artifact_ids: Vec::new(),
            compact: None,
        },
    ];

    if let Err(err) = store.commit_turn(CommitTurnRequest {
        session_id: session_id.clone(),
        turn_id: turn_id.to_string(),
        base_commit_id,
        profile_hash,
        status: turn_status,
        model: model.clone(),
        provider: provider.clone(),
        messages,
        manifests: new_manifests,
    }) {
        status = TurnStatus::Error;
        error_msg = Some(format!("failed to commit session turn: {err}"));
        saved_session_id = None;
    }

    match status {
        TurnStatus::Completed => {
            let _ = tx.send(RuntimeTurnEvent::Completed {
                session_id: session_id.clone(),
                turn_id: turn_id.clone(),
            });
        }
        TurnStatus::Cancelled => {
            let _ = tx.send(RuntimeTurnEvent::Cancelled {
                session_id: session_id.clone(),
                turn_id: turn_id.clone(),
            });
        }
        TurnStatus::Error => {
            if let Some(message) = error_msg.clone() {
                let _ = tx.send(RuntimeTurnEvent::Error {
                    session_id: session_id.clone(),
                    turn_id: turn_id.clone(),
                    message,
                    retryable: false,
                });
            }
        }
    }

    TurnCompletion {
        session_id: session_id.clone(),
        turn_id,
        status,
        assistant_text,
        token_usage,
        task_state_snapshot,
        final_report,
        turn_manifests: manifests,
        model,
        provider,
        saved_session_id,
        profile_drift_warning,
        error: error_msg,
    }
}

#[allow(clippy::too_many_arguments)]
fn commit_cancelled_session_turn(
    session_id: String,
    turn_id: TurnId,
    model: String,
    provider: String,
    store: Arc<dyn SessionStore>,
    profile_hash: String,
    base_commit_id: Option<String>,
    turn_index: usize,
    parent_manifest_ids: Vec<String>,
    profile_drift_warning: Option<String>,
    input: String,
    tx: mpsc::UnboundedSender<RuntimeTurnEvent>,
) -> TurnCompletion {
    commit_session_turn(
        lattice_agent::TaskState::new(),
        TurnStatus::Cancelled,
        None,
        SessionCommitContext {
            session_id,
            turn_id,
            input,
            assistant_text: String::new(),
            token_usage: None,
            manifests: Vec::new(),
            model,
            provider,
            store,
            profile_hash,
            base_commit_id,
            turn_index,
            parent_manifest_ids,
            profile_drift_warning,
            tx,
        },
    )
}

#[allow(clippy::too_many_arguments)]
fn commit_error_session_turn(
    session_id: String,
    turn_id: TurnId,
    model: String,
    provider: String,
    store: Arc<dyn SessionStore>,
    profile_hash: String,
    base_commit_id: Option<String>,
    turn_index: usize,
    parent_manifest_ids: Vec<String>,
    profile_drift_warning: Option<String>,
    input: String,
    message: String,
    tx: mpsc::UnboundedSender<RuntimeTurnEvent>,
) -> TurnCompletion {
    commit_session_turn(
        lattice_agent::TaskState::new(),
        TurnStatus::Error,
        Some(message),
        SessionCommitContext {
            session_id,
            turn_id,
            input,
            assistant_text: String::new(),
            token_usage: None,
            manifests: Vec::new(),
            model,
            provider,
            store,
            profile_hash,
            base_commit_id,
            turn_index,
            parent_manifest_ids,
            profile_drift_warning,
            tx,
        },
    )
}

fn add_profile_drift_metadata(manifest: &mut NewTurnManifest, warning: &str) {
    let mut metadata = match std::mem::take(&mut manifest.metadata) {
        serde_json::Value::Object(map) => map,
        serde_json::Value::Null => serde_json::Map::new(),
        value => {
            let mut map = serde_json::Map::new();
            map.insert("data".to_string(), value);
            map
        }
    };
    metadata.insert(
        "profile_drift".to_string(),
        serde_json::json!({ "warning": warning }),
    );
    manifest.metadata = serde_json::Value::Object(metadata);
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

fn empty_turn_manifest(
    session_id: &str,
    model: &str,
    provider: &str,
    turn_index: usize,
) -> NewTurnManifest {
    NewTurnManifest {
        session_id: session_id.to_string(),
        commit_id: None,
        turn_index,
        request_id: uuid::Uuid::new_v4().simple().to_string(),
        parent_manifest_ids: Vec::new(),
        model: model.to_string(),
        provider: provider.to_string(),
        prompt: lattice_session::PromptSnapshot {
            total_tokens: 0,
            effective_context_budget: 0,
            rendered_fragments: Vec::new(),
            withheld_fragments: Vec::new(),
        },
        hooks: Vec::new(),
        tools: Vec::new(),
        metadata: serde_json::Value::Null,
    }
}

fn messages_from_session_messages(messages: &[SessionMessage]) -> Vec<Message> {
    messages
        .iter()
        .map(|msg| {
            let role = match msg.role.to_ascii_lowercase().as_str() {
                "assistant" => Role::Assistant,
                "system" => Role::System,
                "tool" => Role::Tool,
                _ => Role::User,
            };
            Message {
                role,
                content: msg.content.clone(),
                reasoning_content: msg.reasoning_content.clone(),
                tool_calls: msg.tool_calls.as_ref().map(|calls| {
                    calls
                        .iter()
                        .map(|call| lattice_core::types::ToolCall {
                            id: call.id.clone(),
                            function: lattice_core::types::FunctionCall {
                                name: call.name.clone(),
                                arguments: call.arguments.clone(),
                            },
                        })
                        .collect()
                }),
                tool_call_id: msg.tool_call_id.clone(),
                name: msg.name.clone(),
            }
        })
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_profile::{
        BehaviorSpec, ContextSpec, GenericAgentSpec, SecuritySpec, ToolSelectionSpec,
    };
    use lattice_session::{
        ProfileSnapshot, ProfileSnapshotDraft, ProfileSnapshotSummary, ProfileSourceAudit,
    };

    #[test]
    fn turn_id_unique() {
        assert_ne!(TurnId::new(), TurnId::new());
    }

    #[test]
    fn turn_completion_types_keep_session_identity() {
        let turn_id = TurnId::new();
        let completion = TurnCompletion {
            session_id: "session-a".into(),
            turn_id: turn_id.clone(),
            status: TurnStatus::Completed,
            assistant_text: "ok".into(),
            token_usage: None,
            task_state_snapshot: lattice_agent::TaskState::new(),
            final_report: None,
            turn_manifests: Vec::new(),
            model: "model".into(),
            provider: "provider".into(),
            saved_session_id: Some("session-a".into()),
            profile_drift_warning: None,
            error: None,
        };

        assert_eq!(completion.session_id, "session-a");
        assert_eq!(completion.turn_id, turn_id);
        assert_eq!(completion.status, TurnStatus::Completed);
    }

    #[test]
    fn turn_completion_commits_cancelled_status() {
        let root = std::env::temp_dir().join(format!(
            "lattice-runtime-cancelled-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        let store = Arc::new(lattice_session::SqliteCasSessionStore::new(&root));
        let snapshot = profile_snapshot();
        let opened = store
            .open_turn(OpenTurnRequest {
                session_id: Some("cancelled-session".into()),
                profile_snapshot: snapshot.clone(),
                current_source_fingerprint: Some("fingerprint-a".into()),
            })
            .unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel();

        let completion = commit_cancelled_session_turn(
            opened.session_id.clone(),
            TurnId(opened.turn_id),
            "model".into(),
            "provider".into(),
            store.clone(),
            snapshot.hash,
            opened.base_commit_id,
            opened.turn_index,
            opened.parent_manifest_ids,
            None,
            "cancel me".into(),
            tx,
        );

        assert_eq!(completion.status, TurnStatus::Cancelled);
        assert!(matches!(
            rx.try_recv().unwrap(),
            RuntimeTurnEvent::Cancelled { .. }
        ));
        let manifest_id = store
            .latest_turn_manifest_ids(&completion.session_id)
            .unwrap()
            .pop()
            .unwrap();
        let manifest = store.get_turn_manifest(&manifest_id).unwrap().unwrap();
        assert_eq!(manifest.metadata["turn_status"], "cancelled");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn turn_completion_does_not_claim_save_after_commit_failure() {
        let root = std::env::temp_dir().join(format!(
            "lattice-runtime-commit-failure-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        let store = Arc::new(lattice_session::SqliteCasSessionStore::new(&root));
        let snapshot = profile_snapshot();
        let opened = store
            .open_turn(OpenTurnRequest {
                session_id: Some("commit-failure-session".into()),
                profile_snapshot: snapshot,
                current_source_fingerprint: Some("fingerprint-a".into()),
            })
            .unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel();

        let completion = commit_session_turn(
            lattice_agent::TaskState::new(),
            TurnStatus::Completed,
            None,
            SessionCommitContext {
                session_id: opened.session_id.clone(),
                turn_id: TurnId(opened.turn_id),
                input: "hello".into(),
                assistant_text: "hi".into(),
                token_usage: None,
                manifests: Vec::new(),
                model: "model".into(),
                provider: "provider".into(),
                store,
                profile_hash: "wrong-profile-hash".into(),
                base_commit_id: opened.base_commit_id,
                turn_index: opened.turn_index,
                parent_manifest_ids: opened.parent_manifest_ids,
                profile_drift_warning: None,
                tx,
            },
        );

        assert_eq!(completion.status, TurnStatus::Error);
        assert_eq!(completion.saved_session_id, None);
        assert!(completion
            .error
            .as_deref()
            .is_some_and(|message| message.contains("failed to commit session turn")));
        assert!(matches!(
            rx.try_recv().unwrap(),
            RuntimeTurnEvent::Error { .. }
        ));

        let _ = std::fs::remove_dir_all(root);
    }

    fn profile_snapshot() -> ProfileSnapshot {
        let spec = GenericAgentSpec {
            profile_name: "default".to_string(),
            model: "model".to_string(),
            description: None,
            system_prompt: "You are a test agent.".to_string(),
            tools: ToolSelectionSpec {
                bundles: vec!["filesystem".to_string()],
                enabled: Vec::new(),
            },
            security: SecuritySpec {
                sandbox_mode: "project".to_string(),
            },
            context: ContextSpec {
                project_instructions: false,
                workspace_context: false,
            },
            behavior: BehaviorSpec {
                max_turns: Some(2),
                reasoning_effort: None,
            },
        };
        ProfileSnapshot::from_draft(ProfileSnapshotDraft {
            source: ProfileSourceAudit::new("agent.toml", Some("fingerprint-a")),
            canonical_spec: serde_json::to_value(&spec).unwrap(),
            summary: ProfileSnapshotSummary {
                profile_name: spec.profile_name,
                source_label: "agent.toml".to_string(),
                model: spec.model,
                tool_bundles: spec.tools.bundles,
                enabled_tools: spec.tools.enabled,
                security_mode: Some(spec.security.sandbox_mode),
                context: serde_json::json!({}),
                behavior: serde_json::json!({}),
            },
            captured_at: Some("2026-05-15T00:00:00+00:00".to_string()),
        })
        .unwrap()
    }
}

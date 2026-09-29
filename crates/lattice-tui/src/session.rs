// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use crate::TuiOptions;
use crate::app::{Block, ToolSource, ToolState};
use anyhow::Context;
use lattice_api::local::{
    LocalClient, LocalHistory, MessageRole, MessageSemantic, Node, ResultPayload, SessionId,
    StepContent, SuccessContent,
};

pub(crate) struct SessionContext {
    pub session_id: Option<SessionId>,
    pub active_agent: String,
}

/// 只读打开；新会话在接受首个请求前没有身份。
pub(crate) async fn open_or_create(
    options: &TuiOptions,
    client: &LocalClient,
) -> anyhow::Result<SessionContext> {
    if let Some(id) = &options.session_id {
        let session = client.resolve_session(id).await?;
        let log = client.log(session.session_id).await?;
        return Ok(SessionContext {
            session_id: Some(session.session_id),
            active_agent: log.current_agent.unwrap_or_default(),
        });
    }
    let prepared = options.clone();
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let loaded = load_profile(&prepared, &prepared.agent)?;
        let root = lattice_api::LocalWorkspaceRoot::from_path(&prepared.project_root)?;
        lattice_api::local::FrozenConfiguration::from_env(loaded, &root)?;
        Ok(())
    })
    .await??;
    Ok(SessionContext {
        session_id: None,
        active_agent: options.agent.clone(),
    })
}

pub(crate) fn load_profile(
    options: &TuiOptions,
    active_agent: &str,
) -> anyhow::Result<lattice_api::LoadedAgentProfile> {
    lattice_api::validate_agents_dir(&options.agents_dir).map_err(anyhow::Error::msg)?;
    let loaded = lattice_api::resolve_named_agent_profile(
        active_agent,
        &options.project_root,
        &options.agents_dir,
    )
    .with_context(|| format!("load agent profile {active_agent}"))?;
    if loaded.profile.agent.name != active_agent {
        anyhow::bail!(
            "agent profile '{active_agent}' declares agent.name='{}'; these must match",
            loaded.profile.agent.name
        );
    }
    Ok(loaded)
}

pub(crate) async fn freeze_configuration(
    agent: String,
    options: TuiOptions,
) -> lattice_api::local::Result<lattice_api::local::FrozenConfiguration> {
    tokio::task::spawn_blocking(move || {
        let root = lattice_api::LocalWorkspaceRoot::from_path(&options.project_root)?;
        let loaded = load_profile(&options, &agent)
            .map_err(|_| lattice_api::ApiError::BadRequest("新 Turn 配置无效".into()))?;
        lattice_api::local::FrozenConfiguration::from_env(loaded, &root)
    })
    .await
    .map_err(|_| lattice_api::ApiError::Internal("配置工作者失败".into()))?
}

pub(crate) fn history_blocks(history: LocalHistory) -> Vec<Block> {
    let mut blocks = Vec::new();
    for entry in history.entries {
        blocks.push(Block::AgentName(entry.agent_name));
        match entry.content {
            Node::Root => {}
            Node::UserMessage {
                turn_id, message, ..
            } => {
                blocks.push(Block::UserTurn(turn_id));
                append_messages(&mut blocks, vec![message]);
            }
            Node::Step {
                turn_id,
                step_ordinal,
                content,
                ..
            } => {
                blocks.push(Block::Step(lattice_api::local::StepKey {
                    turn_id,
                    ordinal: step_ordinal,
                }));
                append_step(&mut blocks, content);
            }
        }
    }
    blocks
}

pub(crate) fn append_step(blocks: &mut Vec<Block>, content: StepContent) {
    append_messages(blocks, vec![content.assistant]);
    for result in content.results {
        let state = match &result.payload {
            ResultPayload::Succeeded(SuccessContent::Json { byte_length, .. }) => ToolState::Ok {
                output_len: *byte_length as usize,
            },
            ResultPayload::Succeeded(SuccessContent::OutputTooLarge { .. }) => {
                ToolState::Unavailable {
                    outcome: "succeeded".into(),
                }
            }
            ResultPayload::Failed(error) => ToolState::Failed {
                output_len: 0,
                error: Some(error.message.clone()),
            },
            other => ToolState::Failed {
                output_len: 0,
                error: Some(other.outcome().as_str().into()),
            },
        };
        let id = result.invocation_id.to_string();
        if let Some(Block::Tool {
            state: previous,
            source,
            ..
        }) = blocks
            .iter_mut()
            .find(|block| matches!(block, Block::Tool { id: known, .. } if *known == id))
        {
            *previous = state;
            *source = ToolSource::HistoryResult;
        } else {
            blocks.push(Block::Tool {
                id,
                name: result.name,
                state,
                source: ToolSource::HistoryResult,
            });
        }
    }
}

pub(crate) fn append_messages(
    blocks: &mut Vec<Block>,
    messages: Vec<lattice_api::local::NativeMessage<lattice_api::local::BoundToolCall>>,
) {
    for message in messages {
        for part in message.parts {
            match part.semantic {
                MessageSemantic::Text { text } => match message.role {
                    MessageRole::User => blocks.push(Block::User(text)),
                    MessageRole::Assistant => append_assistant_text(blocks, text),
                    MessageRole::System | MessageRole::Tool => {}
                },
                MessageSemantic::ToolCall { call } => blocks.push(Block::Tool {
                    id: call.invocation_id.to_string(),
                    name: call.name,
                    state: ToolState::Unknown,
                    source: ToolSource::HistoryCall,
                }),
                MessageSemantic::Reasoning { .. } => {}
            }
        }
    }
}

/// 隐藏 reasoning 不应在重连或持久正文替换时引入实时流没有的换行。
pub(crate) fn append_assistant_text(blocks: &mut Vec<Block>, text: String) {
    match blocks.last_mut() {
        Some(Block::Assistant(previous)) => previous.push_str(&text),
        _ => blocks.push(Block::Assistant(text)),
    }
}

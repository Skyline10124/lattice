// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use crate::TuiOptions;
use crate::app::{Block, ToolSource, ToolState};
use crate::blocks::BlockList;
use crate::connection::closing_status;
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
) -> Result<lattice_api::LoadedAgentProfile, lattice_api::ApiError> {
    lattice_api::resolve_named_agent_profile(
        active_agent,
        &options.project_root,
        &options.agents_dir,
    )
}

pub(crate) async fn freeze_configuration(
    agent: String,
    options: TuiOptions,
) -> lattice_api::local::Result<lattice_api::local::FrozenConfiguration> {
    tokio::task::spawn_blocking(move || {
        let root = lattice_api::LocalWorkspaceRoot::from_path(&options.project_root)?;
        let loaded = load_profile(&options, &agent)?;
        lattice_api::local::FrozenConfiguration::from_env(loaded, &root)
    })
    .await
    .map_err(|_| lattice_api::ApiError::Internal("配置工作者失败".into()))?
}

pub(crate) fn history_blocks(history: LocalHistory) -> Vec<Block> {
    let mut blocks = BlockList::default();
    let mut ordinal = std::num::NonZeroUsize::MIN;
    // 已关闭 Turn 的结局行放在该 Turn 最后一个节点之后；结局取这个节点带回的那份。
    let mut closing = None;
    for entry in history.entries {
        let turn = entry.content.turn_id();
        if let Some((id, summary)) = closing.take_if(|(id, _)| Some(*id) != turn) {
            blocks.push(Block::Status(closing_status(id, summary)));
        }
        closing = turn.zip(entry.summary);
        // Turn 的 agent 在接受时冻结，同一 Turn 的 Step 不再重复标题；路径从根开始，
        // 每个 Step 前必有本 Turn 的 UserMessage。
        if !matches!(entry.content, Node::Step { .. }) {
            blocks.push(Block::AgentName(entry.agent_name));
        }
        match entry.content {
            Node::Root => {}
            Node::UserMessage {
                turn_id, message, ..
            } => {
                blocks.push(Block::UserTurn(turn_id, ordinal));
                ordinal = ordinal.saturating_add(1);
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
                append_history_step(&mut blocks, content, turn_id, step_ordinal);
            }
        }
    }
    if let Some((id, summary)) = closing {
        blocks.push(Block::Status(closing_status(id, summary)));
    }
    blocks
        .entries
        .into_iter()
        .map(|entry| entry.block)
        .collect()
}

fn append_history_step(
    blocks: &mut BlockList,
    content: StepContent,
    turn: lattice_api::local::TurnId,
    step: u32,
) {
    // 与 append_assistant_text 的相邻拼接规则一致；reasoning 不分块，工具调用分块。
    let mut groups: Vec<(usize, usize)> = Vec::new();
    let mut adjacent = false;
    for (index, part) in content.assistant.parts.iter().enumerate() {
        match part.semantic {
            MessageSemantic::Text { .. } => {
                if adjacent {
                    if let Some((_, count)) = groups.last_mut() {
                        *count = count.saturating_add(1);
                    }
                } else {
                    groups.push((index, 1));
                }
                adjacent = true;
            }
            MessageSemantic::ToolCall { .. } => adjacent = false,
            MessageSemantic::Reasoning { .. } => {}
        }
    }
    let start = blocks.len();
    append_step(blocks, content);
    let mut groups = groups.into_iter();
    for block in blocks.iter_mut().skip(start) {
        if let Block::Assistant(text) = block
            && let Some((part, 1)) = groups.next()
        {
            *block = Block::HistoryAssistant(crate::comment::HistoryText {
                fragment: crate::comment::Fragment { turn, step, part },
                text: std::mem::take(text).into(),
            });
        }
    }
}

pub(crate) fn user_block(text: String) -> Block {
    match crate::comment::Message::parse(&text) {
        Some(message) => Block::CommentedUser(message),
        None => Block::User(text),
    }
}

pub(crate) fn append_step(blocks: &mut BlockList, content: StepContent) {
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
    blocks: &mut BlockList,
    messages: Vec<lattice_api::local::NativeMessage<lattice_api::local::BoundToolCall>>,
) {
    for message in messages {
        for part in message.parts {
            match part.semantic {
                MessageSemantic::Text { text } => match message.role {
                    MessageRole::User => blocks.push(user_block(text)),
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
pub(crate) fn append_assistant_text(blocks: &mut BlockList, text: String) {
    match blocks.last_mut() {
        Some(Block::Assistant(previous)) => previous.push_str(&text),
        _ => blocks.push(Block::Assistant(text)),
    }
}

#[cfg(test)]
#[path = "session_config_tests.rs"]
mod config_tests;

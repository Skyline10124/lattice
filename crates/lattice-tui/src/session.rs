// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use crate::TuiOptions;
use crate::app::{Block, ToolState};
use crate::blocks::BlockList;
use crate::connection::{closing_status, retry_status};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use lattice_local::{
    AttemptProgress, ConfigureRequest, ConfigureSession, ConflictReason, ConversationEntry,
    EffortAdjustment, LocalClient, LocalError, LocalErrorKind, MessageRole, MessageSemantic,
    ModelAttemptKey, Node, ResultPayload, SessionHead, SessionId, SlotProgress, StepContent,
    SuccessContent,
};

pub(crate) struct SessionContext {
    pub session_id: SessionId,
    pub initial_head: SessionHead,
    pub initial_adjustment: Option<EffortAdjustment>,
}

/// 启动会话上下文：不带会话时 `configure(New)` 创建草稿；带会话与 `--agent` 时 `configure(Existing)`；
/// 只读打开已有会话时先校验项目路径，再读取一次会话头，不读取 log。
pub(crate) async fn open_or_create(
    options: &TuiOptions,
    workspace: &Path,
    client: &LocalClient,
) -> anyhow::Result<SessionContext> {
    if let Some(id) = &options.session_id {
        let session = client.resolve_session(id).await?;
        if session.project_path.as_bytes() != workspace.as_os_str().as_bytes() {
            return Err(LocalError {
                kind: LocalErrorKind::Conflict,
                message: format!(
                    "会话 {} 属于项目 {}，与当前项目路径不一致",
                    session.session_id,
                    session.project_path.to_string_lossy()
                ),
                conflict: Some(ConflictReason::BindingMismatch),
                recovery: None,
                store: None,
            }
            .into());
        }
        if let Some(agent) = options.agent.clone() {
            let reply = client
                .configure(ConfigureRequest {
                    session: ConfigureSession::Existing(session.session_id),
                    workspace: workspace.to_path_buf(),
                    agent: Some(agent),
                    model: None,
                    effort: None,
                })
                .await
                .map_err(|mut err| {
                    if err.kind == LocalErrorKind::Busy {
                        err.message =
                            format!("会话 {} 正在执行一轮，请稍后再试", session.session_id);
                    } else if err.conflict == Some(ConflictReason::BindingMismatch) {
                        err.message = format!(
                            "会话 {} 属于项目 {}，与当前项目路径不一致",
                            session.session_id,
                            session.project_path.to_string_lossy()
                        );
                    }
                    anyhow::Error::from(err)
                })?;
            return Ok(SessionContext {
                session_id: session.session_id,
                initial_head: reply.head,
                initial_adjustment: reply.adjustment,
            });
        }
        let initial_head = client.session_head(session.session_id).await?;
        return Ok(SessionContext {
            session_id: session.session_id,
            initial_head,
            initial_adjustment: None,
        });
    }
    let agent = options.agent.clone().ok_or_else(|| LocalError {
        kind: LocalErrorKind::BadRequest,
        message: "new Session requires explicit agent".into(),
        conflict: None,
        recovery: None,
        store: None,
    })?;
    let session_id = lattice_local::new_session_id();
    let reply = client
        .configure(ConfigureRequest {
            session: ConfigureSession::New(session_id),
            workspace: workspace.to_path_buf(),
            agent: Some(agent),
            model: None,
            effort: None,
        })
        .await?;
    Ok(SessionContext {
        session_id,
        initial_head: reply.head,
        initial_adjustment: reply.adjustment,
    })
}

/// 生成中的文字：所属尝试与已收到的字节数，以及其中的正文。reasoning 不显示，只计入字节数，
/// 后台按两类文字合计的字节数续发。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Generating {
    pub(crate) attempt: u32,
    pub(crate) received: usize,
    pub(crate) text: String,
}

/// 条目显示生成中的文字所属的尝试已 Ended：完成时由条目中的回复取代，停止时半截文字作废。
pub(crate) fn superseded(entry: &ConversationEntry, generating: &Generating) -> bool {
    match entry {
        ConversationEntry::Step {
            content: Node::Step { attempts, .. },
            ..
        } => attempts
            .get(generating.attempt as usize)
            .is_some_and(|attempt| matches!(attempt.progress, AttemptProgress::Ended { .. })),
        _ => false,
    }
}

/// 一个条目的显示块。`ended` 表示条目所在的一轮已经结束：未到 Terminal 的槽位改按会话库给出的
/// 结论显示，最后一步带出结局；`generating` 只在它的尝试尚未 Ended 时显示在这一步的位置。
pub(crate) fn entry_blocks(
    entry: &ConversationEntry,
    ended: bool,
    generating: Option<&Generating>,
) -> Vec<Block> {
    let mut blocks = BlockList::default();
    match entry {
        ConversationEntry::UserMessage {
            node,
            content,
            ordinal,
        } => {
            if let Node::UserMessage {
                turn_id,
                agent_name,
                message,
                ..
            } = content
            {
                blocks.push(Block::UserTurn {
                    turn_id: *turn_id,
                    node: Some(*node),
                    ordinal: usize::try_from(*ordinal)
                        .ok()
                        .and_then(std::num::NonZeroUsize::new),
                    agent: agent_name.as_str().to_owned(),
                });
                append_messages(&mut blocks, vec![message.clone()]);
            }
        }
        ConversationEntry::Step {
            node,
            content,
            retries,
            summary,
            ..
        } => {
            let Node::Step {
                turn_id,
                step_ordinal,
                slots,
                ..
            } = content
            else {
                return Vec::new();
            };
            blocks.push(Block::Step {
                node: *node,
                key: lattice_local::StepKey {
                    turn_id: *turn_id,
                    ordinal: *step_ordinal,
                },
            });
            // 重试提示排在这一步的回复之前，与它们发生的先后一致。
            for retry in retries {
                let key = ModelAttemptKey {
                    turn_id: *turn_id,
                    model_step_ordinal: *step_ordinal,
                    attempt_ordinal: retry.attempt,
                };
                blocks.push(Block::Status(retry_status(
                    key,
                    retry.delay_ms,
                    retry.notice.clone(),
                )));
            }
            if let Some(step) = content.step_content() {
                // 这一轮仍在执行时，未到 Terminal 的调用还在运行；结束后才取阶段给出的结论。
                let running = slots
                    .iter()
                    .filter(|slot| {
                        !ended && !matches!(slot.progress, SlotProgress::Terminal { .. })
                    })
                    .map(|slot| slot.binding.invocation_id.to_string())
                    .collect();
                append_history_step(&mut blocks, step, *turn_id, *step_ordinal, &running);
            } else if let Some(generating) =
                generating.filter(|generating| !superseded(entry, generating))
            {
                append_assistant_text(&mut blocks, generating.text.clone());
            }
            if ended && let Some(summary) = summary {
                blocks.push(Block::Status(closing_status(*turn_id, summary.clone())));
            }
        }
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
    turn: lattice_local::TurnId,
    step: u32,
    running: &std::collections::HashSet<String>,
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
    append_step(blocks, content, running);
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

fn append_step(
    blocks: &mut BlockList,
    content: StepContent,
    running: &std::collections::HashSet<String>,
) {
    append_messages(blocks, vec![content.assistant]);
    for result in content.results {
        let id = result.invocation_id.to_string();
        let state = if running.contains(&id) {
            ToolState::Running
        } else {
            match &result.payload {
                ResultPayload::Succeeded(SuccessContent::Json { byte_length, .. }) => {
                    ToolState::Ok {
                        output_len: *byte_length as usize,
                    }
                }
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
            }
        };
        if let Some(Block::Tool {
            state: previous, ..
        }) = blocks
            .iter_mut()
            .find(|block| matches!(block, Block::Tool { id: known, .. } if *known == id))
        {
            *previous = state;
        } else {
            blocks.push(Block::Tool {
                id,
                name: result.name,
                state,
            });
        }
    }
}

fn append_messages(
    blocks: &mut BlockList,
    messages: Vec<lattice_local::NativeMessage<lattice_local::BoundToolCall>>,
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
                }),
                MessageSemantic::Reasoning { .. } => {}
            }
        }
    }
}

/// 隐藏 reasoning 不应在生成中的文字由回复取代时引入原来没有的换行。
fn append_assistant_text(blocks: &mut BlockList, text: String) {
    match blocks.entries.last_mut().map(|entry| &mut entry.block) {
        Some(Block::Assistant(previous)) => previous.push_str(&text),
        _ => blocks.push(Block::Assistant(text)),
    }
}

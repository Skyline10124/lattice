// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use crate::app::{Block, StatusKind, StatusPlacement, ToolSource, ToolState};
use anyhow::{Context, Result};
use lattice_api::{
    CreateSessionRequest,
    local::{
        LocalClient, LocalHistory, MessageRole, MessageSemantic, ToolResultOutcome, TurnObservation,
    },
};

use crate::TuiOptions;

pub(crate) struct SessionContext {
    pub session_id: String,
    pub active_agent: String,
    pub project_branch: String,
}

/// 新会话走后台创建；打开既有会话不校验当前 profile/provider。
pub(crate) async fn open_or_create(
    options: &TuiOptions,
    client: &LocalClient,
) -> anyhow::Result<SessionContext> {
    let session = match &options.session_id {
        Some(id) => client.open_session(id).await?,
        None => {
            let prepared = options.clone();
            let (branch, key) = tokio::task::spawn_blocking(move || -> Result<_> {
                let branch =
                    required_project_branch(prepared.project_branch.as_deref())?.to_owned();
                let loaded = load_profile(&prepared, &prepared.agent)?;
                let root = lattice_api::LocalWorkspaceRoot::from_path(&prepared.project_root)?;
                // 新会话在进入界面前验证配置；实际提交仍按当时输入重新冻结。
                lattice_api::local::FrozenConfiguration::from_env(loaded, &root)?;
                Ok((branch, root.store_key_hex()))
            })
            .await??;
            client
                .create_session(CreateSessionRequest {
                    label: None,
                    active_agent: options.agent.clone(),
                    project_branch: branch,
                    project_store_key: key,
                })
                .await?
        }
    };
    Ok(SessionContext {
        session_id: session.session_id,
        active_agent: session.active_agent_name,
        project_branch: session.project_target_branch.context("会话配置无效")?,
    })
}

/// Resolve and validate the named agent profile. Fails when
/// `profile.agent.name != active_agent`, same as today.
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
            "agent profile '{}' declares agent.name='{}'; these must match",
            active_agent,
            loaded.profile.agent.name
        );
    }
    Ok(loaded)
}

/// 固定历史只转换为显示块；读取对象、分页与新旧切点归 LocalTurn。
pub(crate) fn history_blocks(history: LocalHistory) -> Vec<Block> {
    let mut blocks = Vec::new();
    for entry in history.entries {
        blocks.push(Block::AgentName(entry.agent_name));
        append_messages(&mut blocks, entry.messages);
        if let TurnObservation::Committed(turn) = entry.turn {
            let kind = match turn.outcome {
                lattice_api::TurnOutcome::Completed { .. } => StatusKind::Visible {
                    commit_id: turn.commit_id.clone(),
                },
                lattice_api::TurnOutcome::Cancelled { .. } => {
                    StatusKind::Cancelled { reason: None }
                }
                outcome => StatusKind::TurnFailed {
                    reason: format!("{outcome:?}"),
                },
            };
            let needs_confirmation = !matches!(kind, StatusKind::Visible { .. });
            blocks.push(Block::Status {
                kind,
                placement: StatusPlacement::Transcript,
            });
            if needs_confirmation {
                blocks.push(Block::Status {
                    kind: StatusKind::Visible {
                        commit_id: turn.commit_id,
                    },
                    placement: StatusPlacement::Transcript,
                });
            }
        }
    }
    blocks
}

/// 配置来源仍属于 TUI；仅在共同句柄确认需要新配置后调用。
pub(crate) async fn freeze_configuration(
    client: LocalClient,
    session_id: String,
    options: TuiOptions,
) -> lattice_api::local::Result<lattice_api::local::FrozenConfiguration> {
    let metadata = client.open_session(&session_id).await?;
    tokio::task::spawn_blocking(move || {
        let root = lattice_api::LocalWorkspaceRoot::from_path(&options.project_root)?;
        let loaded = load_profile(&options, &metadata.active_agent_name)
            .map_err(|_| lattice_api::ApiError::BadRequest("新 Turn 配置无效".into()))?;
        lattice_api::local::FrozenConfiguration::from_env(loaded, &root)
    })
    .await
    .map_err(|_| {
        lattice_api::local::LocalError::from(lattice_api::ApiError::BadRequest(
            "新 Turn 配置无效".into(),
        ))
    })?
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
                    MessageRole::Assistant => blocks.push(Block::Assistant(text)),
                    MessageRole::System | MessageRole::Tool => {}
                },
                MessageSemantic::ToolCall { call } => {
                    blocks.push(Block::Tool {
                        id: call.invocation_id.to_string(),
                        name: call.name,
                        state: ToolState::Unknown,
                        source: ToolSource::HistoryCall,
                    });
                }
                MessageSemantic::ToolResult {
                    invocation_id,
                    name,
                    outcome,
                    content,
                } => {
                    let state = match content {
                        None => ToolState::Unavailable {
                            outcome: outcome.as_str().into(),
                        },
                        Some(content) if outcome == ToolResultOutcome::Succeeded => ToolState::Ok {
                            output_len: content.len(),
                        },
                        Some(content) => ToolState::Failed {
                            output_len: content.len(),
                            error: Some(format!("{}: {content}", outcome.as_str())),
                        },
                    };
                    if let Some(Block::Tool { state: previous, source, .. }) = blocks.iter_mut().rev().find(|block| matches!(block, Block::Tool { id, .. } if id == &invocation_id.to_string())) {
                        *previous = state;
                        *source = ToolSource::HistoryResult;
                    } else { blocks.push(Block::Tool { id: invocation_id.to_string(), name, state, source: ToolSource::HistoryResult }); }
                }
                MessageSemantic::Reasoning { .. } => {}
            }
        }
    }
}

/// Moved verbatim from `runtime.rs`, together with its three existing tests.
fn required_project_branch(project_branch: Option<&str>) -> Result<&str> {
    match project_branch {
        Some(branch) if !branch.trim().is_empty() => Ok(branch),
        _ => Err(anyhow::anyhow!(
            "--project-branch is required when creating a new TUI session"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_session_project_branch_is_required() {
        let error = required_project_branch(None).expect_err("missing branch must fail");
        assert!(
            error
                .to_string()
                .contains("--project-branch is required when creating a new TUI session")
        );
    }

    #[test]
    fn new_session_project_branch_rejects_blank_values() {
        let error = required_project_branch(Some("  ")).expect_err("blank branch must fail");
        assert!(
            error
                .to_string()
                .contains("--project-branch is required when creating a new TUI session")
        );
    }

    #[test]
    fn new_session_project_branch_preserves_explicit_value() {
        assert_eq!(
            required_project_branch(Some("feature/x")).expect("valid branch"),
            "feature/x"
        );
    }
}

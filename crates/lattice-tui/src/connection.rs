// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use crate::{TuiOptions, app::Block, session};
use lattice_api::local::{
    Acceptance, FrozenConfiguration, LocalClient, LocalError, LocalErrorKind, Observation,
    SessionObservation, TurnId, TurnObservation, TurnRequest,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::mpsc;

pub(crate) enum Update {
    /// 提交回执选定的 Turn；`live` 表示本次请求已接受或仍 pending，取消才有对象。
    /// Recovered / 已提交的 Existing 选定的是已关闭的 Turn，本次新 prompt 未接受。
    SelectedTurn {
        turn_id: TurnId,
        message: &'static str,
        live: bool,
    },
    /// 探测得 NeedsConfiguration 后已登记取消意图：配置提交未发出，请求从未被接受。
    NotSubmitted {
        turn_id: TurnId,
    },
    Snapshot {
        view: Box<SessionObservation>,
        blocks: Option<Vec<Block>>,
        owned: bool,
        confirmed: bool,
        error: Option<LocalError>,
    },
    Event(lattice_api::TurnEvent),
    Gap,
    Error(LocalError),
}

/// 只持有前端连接；磁盘历史重载与配置冻结不阻塞 raw 键盘事件循环。
pub(crate) struct Connection {
    receiver: mpsc::Receiver<Update>,
    task: tokio::task::JoinHandle<()>,
    abandon_unsent: Arc<AtomicBool>,
}
impl Connection {
    pub fn start(
        client: LocalClient,
        session_id: String,
        turn: Option<TurnId>,
        submission: Option<(TuiOptions, TurnRequest)>,
    ) -> Self {
        let (sender, receiver) = mpsc::channel(16);
        let abandon_unsent = Arc::new(AtomicBool::new(false));
        let abandon = abandon_unsent.clone();
        let task = tokio::spawn(async move {
            let result = async {
                let mut selected = turn;
                if let Some((options, request)) = submission {
                    let prior = client.lookup_or_recover(request.clone()).await;
                    match prior {
                        Ok(prior) => {
                            let accepted = if prior == Acceptance::NeedsConfiguration {
                                // 已有取消意图时不再发起配置提交：未发出的请求没有可取消的对象，也不代用户重发。
                                if abandon.load(Ordering::Acquire) {
                                    let _ = sender
                                        .send(Update::NotSubmitted {
                                            turn_id: request.turn_id,
                                        })
                                        .await;
                                    return Ok(());
                                }
                                let metadata = client.open_session(&session_id).await?;
                                let workspace = request.workspace.clone();
                                let config = tokio::task::spawn_blocking(move || {
                                    let root =
                                        lattice_api::LocalWorkspaceRoot::from_path(&workspace)?;
                                    let loaded = session::load_profile(
                                        &options,
                                        &metadata.active_agent_name,
                                    )
                                    .map_err(|_| invalid())?;
                                    FrozenConfiguration::from_env(loaded, &root)
                                })
                                .await
                                .map_err(|_| invalid())??;
                                if abandon.load(Ordering::Acquire) {
                                    let _ = sender
                                        .send(Update::NotSubmitted {
                                            turn_id: request.turn_id,
                                        })
                                        .await;
                                    return Ok(());
                                }
                                client.submit(request, Some(config)).await?
                            } else {
                                prior
                            };
                            let (id, message, live) = match accepted {
                                Acceptance::Accepted { turn_id } => (turn_id, "已确认接受", true),
                                Acceptance::ConfirmationUnknown { turn_id } => {
                                    (turn_id, "接受确认未知；保留此 ID 查询，不分配新请求", true)
                                }
                                Acceptance::Existing {
                                    turn: TurnObservation::Pending(turn),
                                } => (turn.turn_id, "找回既有 pending；未重复接受", true),
                                Acceptance::Existing {
                                    turn: TurnObservation::Committed(turn),
                                } => (turn.turn_id, "找回既有结果；未重复执行", false),
                                Acceptance::Recovered { turn } => (
                                    turn.turn_id,
                                    "旧 Turn 已恢复关闭；本次新 prompt 未接受",
                                    false,
                                ),
                                Acceptance::NeedsConfiguration => return Err(invalid()),
                            };
                            selected = Some(id);
                            let _ = sender
                                .send(Update::SelectedTurn {
                                    turn_id: id,
                                    message,
                                    live,
                                })
                                .await;
                        }
                        Err(error) if error.kind == LocalErrorKind::Busy => {
                            let _ = sender.send(Update::Error(error)).await;
                            selected = None;
                        }
                        Err(error) => return Err(error),
                    }
                }
                let view = client.query(&session_id, selected).await?;
                let selected = match selected
                    .or_else(|| view.active_turn.as_ref().map(|turn| turn.turn_id))
                {
                    Some(id) => Some(id),
                    None => match &view.head_commit_id {
                        Some(head) => Some(client.read_commit(&session_id, head).await?.turn_id),
                        None => None,
                    },
                };
                if let Some(id) = selected {
                    // 后台先登记订阅再给固定快照；历史重载期间的增量由有界队列保护或明确 Gap。
                    let mut subscription = client.subscribe(&session_id, id, None).await?;
                    while let Some(observation) = subscription.next().await? {
                        let (update, finished) = match observation {
                            Observation::Gap { .. } => (Update::Gap, false),
                            Observation::Event { event, .. } => (Update::Event(event), false),
                            Observation::Snapshot { view, owned, .. } => {
                                let blocks = session::load_history(&client, &view).await.ok();
                                // 正文读取失败不撤销固定视图，也不能中止等待所有者的独立完成回执。
                                let error = blocks.is_none().then(read_error);
                                (
                                    Update::Snapshot {
                                        view,
                                        blocks,
                                        owned,
                                        confirmed: false,
                                        error,
                                    },
                                    !owned,
                                )
                            }
                            Observation::Finished { view, error, .. } => {
                                let confirmed = error.is_none();
                                let blocks = session::load_history(&client, &view).await.ok();
                                // 保留完成快照与独立的正文不可用信号，不能因读取失败丢掉已确认终态。
                                let error = error.or_else(|| blocks.is_none().then(read_error));
                                (
                                    Update::Snapshot {
                                        view,
                                        blocks,
                                        owned: false,
                                        confirmed,
                                        error,
                                    },
                                    true,
                                )
                            }
                        };
                        if sender.send(update).await.is_err() || finished {
                            return Ok(());
                        }
                    }
                } else {
                    let blocks = session::load_history(&client, &view)
                        .await
                        .map_err(|_| read_error())?;
                    let _ = sender
                        .send(Update::Snapshot {
                            view: Box::new(view),
                            blocks: Some(blocks),
                            owned: false,
                            confirmed: false,
                            error: None,
                        })
                        .await;
                }
                Ok(())
            }
            .await;
            if let Err(error) = result {
                let _ = sender.send(Update::Error(error)).await;
            }
        });
        Self {
            receiver,
            task,
            abandon_unsent,
        }
    }

    pub async fn next(&mut self) -> Option<Update> {
        self.receiver.recv().await
    }

    /// 用户已请求取消而提交回执未到：尚未发出的配置提交不再发送；已发出的请求仍由其回执决定。
    pub fn abandon_unsent(&self) {
        self.abandon_unsent.store(true, Ordering::Release);
    }
}
impl futures::Stream for Connection {
    type Item = Update;
    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.receiver.poll_recv(cx)
    }
}
impl Drop for Connection {
    fn drop(&mut self) {
        self.task.abort();
    }
}
fn invalid() -> LocalError {
    lattice_api::ApiError::BadRequest("invalid new Turn configuration".into()).into()
}
fn read_error() -> LocalError {
    lattice_api::ApiError::StoreUnavailable("persistent history read failed".into()).into()
}

#[cfg(test)]
#[path = "connection_tests.rs"]
mod tests;

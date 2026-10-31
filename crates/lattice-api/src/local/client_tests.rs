// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::{ipc, types::*, *};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
};

struct Peer {
    _directory: tempfile::TempDir,
    _instance: ipc::Instance,
    listener: UnixListener,
    client: LocalClient,
}
struct Wire {
    socket: UnixStream,
    writer: ipc::ResponseWriter,
}
impl Peer {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let layout = crate::StateLayout::in_state_root(directory.path());
        let (instance, listener) = ipc::Instance::bind(&layout).unwrap();
        Self {
            _directory: directory,
            _instance: instance,
            listener: UnixListener::from_std(listener).unwrap(),
            client: LocalClient::new(layout),
        }
    }
    async fn request(&self) -> (Wire, Request) {
        within(async {
            loop {
                let (mut socket, _) = self.listener.accept().await.unwrap();
                match ipc::read(&mut socket).await {
                    Ok(request) => {
                        return (
                            Wire {
                                socket,
                                writer: ipc::ResponseWriter::default(),
                            },
                            request,
                        );
                    }
                    Err(error) if error.kind == LocalErrorKind::Transport => continue,
                    Err(error) => panic!("request: {error}"),
                }
            }
        })
        .await
    }
}
impl Wire {
    async fn reply(&mut self, response: Response) {
        within(self.writer.write(&mut self.socket, &response))
            .await
            .unwrap();
    }
    async fn head(&mut self, id: TurnId, owner: bool) {
        self.reply(Response::View(ViewUpdate::Head(head(id, owner))))
            .await;
    }
}
async fn within<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(3), future)
        .await
        .expect("IPC progress")
}
fn sid() -> SessionId {
    SessionId::parse("0123456789abcdef0123456789abcdef").unwrap()
}
fn detail(id: TurnId, closed: bool) -> TurnDetail {
    TurnDetail {
        session_id: sid(),
        turn_id: id,
        user_node: NodeId(1),
        agent_name: "local-test".into(),
        accepted_at: "2026-10-17T00:00:00Z".parse().unwrap(),
        ended_at: "2026-10-17T00:00:01Z".parse().unwrap(),
        summary: TurnSummary {
            outcome: if closed {
                PublicTurnOutcome::Completed
            } else {
                PublicTurnOutcome::Failed
            },
            notice: None,
            progress: if closed {
                StepProgress::Closed {
                    end: StepEnd::Finish,
                }
            } else {
                StepProgress::Reserved
            },
        },
        usage: UsageSummary::from_attempts([(
            Some(lattice_conversation::TokenUsage {
                input: Some(0),
                output: Some(0),
                ..Default::default()
            }),
            lattice_conversation::UsageCompleteness::Partial,
            None,
        )])
        .into(),
        steps: vec![],
    }
}
fn head(id: TurnId, owner: bool) -> SessionHead {
    SessionHead {
        session_id: sid(),
        position: NodeId(2),
        agent: Some("local-test".into()),
        turn_id: Some(id),
        owner,
        diagnostic: None,
        path_replaced: false,
    }
}
fn entry(id: TurnId, closed: bool) -> Box<ConversationEntry> {
    let progress = if closed {
        StepProgress::Closed {
            end: StepEnd::Finish,
        }
    } else {
        StepProgress::Reserved
    };
    let key = StepKey {
        turn_id: id,
        ordinal: 0,
    };
    Box::new(ConversationEntry::Step {
        node: NodeId(2),
        content: Node::Step {
            parent: NodeId(1),
            turn_id: id,
            step_ordinal: 0,
            progress: progress.clone(),
            attempts: vec![],
            reply: None,
            slots: vec![],
            updated_at: detail(id, closed).ended_at,
        },
        step: StepDetail {
            node: NodeId(2),
            key,
            progress,
            end: if closed {
                StepEnd::Finish
            } else {
                StepEnd::Stop {
                    cause: lattice_session::store::StopCause::ProcessExit,
                }
            },
            attempts: vec![],
            slots: vec![],
            updated_at: detail(id, closed).ended_at,
        },
        retries: vec![],
        summary: Some(detail(id, closed).summary),
    })
}
fn request(peer: &Peer, id: TurnId) -> TurnRequest {
    TurnRequest {
        turn_id: id,
        session: SubmitSession::New(sid()),
        workspace: peer._directory.path().into(),
        input: "原始输入".into(),
        agent: Some("local-test".into()),
    }
}
fn accepted(id: TurnId) -> Response {
    Response::Acceptance(Acceptance::Accepted {
        turn: TurnView::from(&detail(id, false)),
    })
}
async fn next(turn: &mut LocalTurn) -> LocalUpdate {
    within(turn.next()).await.expect("update")
}
async fn receipt(turn: &mut LocalTurn) -> SubmissionReceipt {
    loop {
        if let LocalUpdate::Submit {
            update: SubmissionUpdate::Receipt(receipt),
        } = next(turn).await
        {
            return receipt;
        }
    }
}
async fn cancel_status(turn: &mut LocalTurn, expected: CancelStatus) {
    loop {
        if matches!(next(turn).await, LocalUpdate::Cancel { status, .. } if status == expected) {
            return;
        }
    }
}
async fn submit(peer: &Peer, id: TurnId) -> (LocalTurn, Wire) {
    let mut turn = peer.client.start_turn(request(peer, id));
    assert!(
        matches!(next(&mut turn).await, LocalUpdate::Submit { update: SubmissionUpdate::Sending { turn_id, session_id, new: true } } if turn_id == id && session_id == sid())
    );
    let (wire, request) = peer.request().await;
    assert!(
        matches!(request, Request::Submit { request: TurnRequest { turn_id, session: SubmitSession::New(_), .. }, .. } if turn_id == id)
    );
    (turn, wire)
}
async fn watch(peer: &Peer) -> Wire {
    let (wire, request) = peer.request().await;
    assert!(matches!(request, Request::Watch { session_id } if session_id == sid()));
    wire
}
async fn cancellation(peer: &Peer, id: TurnId) -> Wire {
    let (wire, request) = peer.request().await;
    assert!(matches!(request, Request::Cancel { turn_id } if turn_id == id));
    wire
}
#[tokio::test]
async fn wait_for_turn_polls_in_flight_then_returns_acceptance_or_the_original_error() {
    for terminal in [
        None,
        Some(LocalErrorKind::NotFound),
        Some(LocalErrorKind::StoreUnavailable),
        Some(LocalErrorKind::Transport),
    ] {
        let peer = Peer::new();
        let id = new_turn_id();
        let client = peer.client.clone();
        let mut waiting = tokio::spawn(async move { client.wait_for_turn(id).await });
        let (mut wire, request) = peer.request().await;
        assert!(matches!(request, Request::Turn { turn_id } if turn_id == id));
        for _ in 0..2 {
            let sent = tokio::time::Instant::now();
            wire.reply(Response::Turn(TurnLookup::InFlight)).await;
            let (next_wire, request) = tokio::select! {
                request = peer.request() => request,
                result = &mut waiting => panic!("在途不能结束等待：{result:?}"),
            };
            assert!(
                sent.elapsed() >= Duration::from_millis(25),
                "找回查询必须遵守共同的 25ms 节奏"
            );
            assert!(matches!(request, Request::Turn { turn_id } if turn_id == id));
            wire = next_wire;
        }
        if terminal == Some(LocalErrorKind::Transport) {
            drop(wire);
        } else if let Some(kind) = terminal {
            wire.reply(Response::Error(LocalError::new(kind, "原始查询错误")))
                .await;
        } else {
            wire.reply(Response::Turn(TurnLookup::Accepted {
                turn: detail(id, false),
            }))
            .await;
        }
        let result = within(waiting).await.unwrap();
        match terminal {
            None => assert_eq!(result.unwrap(), detail(id, false)),
            Some(kind) => {
                let error = result.unwrap_err();
                assert_eq!(error.kind, kind);
                if kind != LocalErrorKind::Transport {
                    assert_eq!(error.message, "原始查询错误");
                }
            }
        }
    }
}
#[tokio::test]
async fn submission_only_preserves_external_completion_before_the_late_receipt() {
    for closed in [false, true] {
        let peer = Peer::new();
        let id = new_turn_id();
        let mut turn = peer.client.start_submission(request(&peer, id));
        next(&mut turn).await;
        let (mut wire, request) = peer.request().await;
        assert!(matches!(request, Request::Submit { .. }));
        turn.apply_view(ViewUpdate::Head(head(id, false))).await;
        turn.apply_view(ViewUpdate::Entry {
            entry: user_entry(id),
        })
        .await;
        turn.apply_view(ViewUpdate::Entry {
            entry: entry(id, closed),
        })
        .await;
        wire.reply(accepted(id)).await;
        assert!(matches!(
            receipt(&mut turn).await.result,
            SubmissionResult::Accepted { .. }
        ));
        let ended = tokio::time::timeout(Duration::from_secs(1), turn.next()).await;
        assert!(
            matches!(ended, Ok(None)),
            "外部视图已确认结束，迟到的接受回执不能遗失结束判定"
        );
        assert_eq!(
            turn.detach().await.summary,
            Some(detail(id, closed).summary)
        );
    }
}
#[tokio::test]
async fn accepted_submission_ignores_backlogged_views_until_its_user_message() {
    async fn deliver(
        turn: &mut LocalTurn,
        view: &mut Option<Wire>,
        update: ViewUpdate,
        visible: bool,
    ) {
        if let Some(view) = view {
            // 若旧实现已关闭观察连接，仍让失败落在句柄的交付断言上。
            let _ = view
                .writer
                .write(&mut view.socket, &Response::View(update.clone()))
                .await;
            if visible {
                assert_eq!(
                    within(turn.next()).await,
                    Some(update.into()),
                    "积压视图不能终结新提交句柄"
                );
            }
        } else {
            turn.apply_view(update).await;
        }
    }
    for own_view in [false, true] {
        for root in [true, false] {
            let peer = Peer::new();
            let id = new_turn_id();
            let mut input = request(&peer, id);
            input.session = SubmitSession::Existing(sid());
            let mut turn = if own_view {
                peer.client.start_turn(input)
            } else {
                peer.client.start_submission(input)
            };
            next(&mut turn).await;
            let (mut submit, request) = peer.request().await;
            assert!(matches!(request, Request::Submit { .. }));
            let mut accepted_turn = TurnView::from(&detail(id, false));
            accepted_turn.user_node = NodeId(3);
            submit
                .reply(Response::Acceptance(Acceptance::Accepted {
                    turn: accepted_turn,
                }))
                .await;
            assert!(matches!(
                receipt(&mut turn).await.result,
                SubmissionResult::Accepted { .. }
            ));
            let mut view = if own_view {
                Some(watch(&peer).await)
            } else {
                None
            };
            let old = new_turn_id();
            let mut old_head = head(old, false);
            if root {
                old_head.position = NodeId(0);
                old_head.turn_id = None;
            }
            deliver(
                &mut turn,
                &mut view,
                ViewUpdate::Head(old_head.clone()),
                true,
            )
            .await;
            if !root {
                deliver(
                    &mut turn,
                    &mut view,
                    ViewUpdate::Entry {
                        entry: entry(old, true),
                    },
                    false,
                )
                .await;
                // 同一 watch 上后一条 head 的交付确认旧轮的当前位置条目已被处理。
                deliver(
                    &mut turn,
                    &mut view,
                    ViewUpdate::Head(old_head.clone()),
                    true,
                )
                .await;
            }
            turn.cancel();
            let (mut cancel, request) = tokio::select! {
                request = peer.request() => request,
                update = turn.next() => panic!("积压视图之后仍须按新 TurnId 发出取消，不能退出：{update:?}"),
            };
            assert!(matches!(request, Request::Cancel { turn_id } if turn_id == id));
            cancel.reply(Response::Cancelled { owned: true }).await;
            cancel_status(&mut turn, CancelStatus::Taken).await;

            let mut user = user_entry(id);
            if let ConversationEntry::UserMessage {
                node,
                ordinal,
                content: Node::UserMessage { parent, .. },
            } = user.as_mut()
            {
                *node = NodeId(3);
                *ordinal = if root { 1 } else { 2 };
                *parent = if root { NodeId(0) } else { NodeId(2) };
            }
            deliver(
                &mut turn,
                &mut view,
                ViewUpdate::Entry { entry: user },
                true,
            )
            .await;
            // UserMessage 不能让此前积压的头重新成为结束证据；它之后的新头才有效。
            turn.cancel();
            let mut cancel = tokio::select! {
                request = cancellation(&peer, id) => request,
                update = turn.next() => panic!("本轮 UserMessage 不能激活此前的旧头：{update:?}"),
            };
            cancel.reply(Response::Cancelled { owned: true }).await;
            cancel_status(&mut turn, CancelStatus::Taken).await;
            deliver(&mut turn, &mut view, ViewUpdate::Head(old_head), true).await;
            if !root {
                deliver(
                    &mut turn,
                    &mut view,
                    ViewUpdate::Entry {
                        entry: entry(old, true),
                    },
                    false,
                )
                .await;
            }
            assert!(within(turn.next()).await.is_none());
            assert_eq!(within(turn.detach()).await.turn_id, Some(id));
        }
    }
}
#[tokio::test]
async fn submission_only_keeps_receipts_recovery_and_cancel_without_a_watch() {
    for recovered in [false, true] {
        for evidence_first in [false, true] {
            let peer = Peer::new();
            let id = new_turn_id();
            let mut turn = peer.client.start_submission(request(&peer, id));
            assert!(
                matches!(next(&mut turn).await, LocalUpdate::Submit { update: SubmissionUpdate::Sending { turn_id, .. } } if turn_id == id)
            );
            let (mut wire, request) = peer.request().await;
            assert!(matches!(request, Request::Submit { .. }));
            if recovered {
                wire.reply(Response::Acceptance(Acceptance::ConfirmationUnknown {
                    turn_id: id,
                    session_id: sid(),
                }))
                .await;
                let (mut lookup, request) = peer.request().await;
                assert!(matches!(request, Request::Turn { turn_id } if turn_id == id));
                lookup.reply(Response::Turn(TurnLookup::InFlight)).await;
                let (mut lookup, request) = peer.request().await;
                assert!(matches!(request, Request::Turn { turn_id } if turn_id == id));
                lookup
                    .reply(Response::Turn(TurnLookup::Accepted {
                        turn: detail(id, false),
                    }))
                    .await;
            } else {
                wire.reply(accepted(id)).await;
            }
            let receipt = receipt(&mut turn).await;
            assert!(matches!(receipt.result, SubmissionResult::Accepted { .. }));
            assert_eq!(receipt.recovered, recovered);
            turn.reconnect();
            turn.cancel();
            let (mut cancel, request) = peer.request().await;
            assert!(
                matches!(request, Request::Cancel { turn_id } if turn_id == id),
                "提交专用句柄接受或重连后不得开启 watch"
            );
            if evidence_first {
                turn.apply_view(ViewUpdate::Head(head(id, true))).await;
            }
            cancel.reply(Response::Cancelled { owned: false }).await;
            cancel_status(&mut turn, CancelStatus::NotTaken).await;
            if !evidence_first {
                turn.apply_view(ViewUpdate::Head(head(id, true))).await;
            }
            let mut retry = cancellation(&peer, id).await;
            retry.reply(Response::Cancelled { owned: true }).await;
            cancel_status(&mut turn, CancelStatus::Taken).await;
            turn.apply_view(ViewUpdate::Head(head(id, false))).await;
            turn.apply_view(ViewUpdate::Entry {
                entry: entry(id, true),
            })
            .await;
            assert!(
                within(turn.next()).await.is_none(),
                "外部视图只供控制判定，不重复转发"
            );
            let report = within(turn.detach()).await;
            assert_eq!(report.cancel, Some(CancelStatus::Taken));
            assert_eq!(report.summary, Some(detail(id, true).summary));
            assert!(
                tokio::time::timeout(Duration::from_millis(25), peer.request())
                    .await
                    .is_err()
            );
        }
    }
}
#[tokio::test]
async fn configuration_rejection_has_identity_and_exactly_one_receipt() {
    let peer = Peer::new();
    let id = new_turn_id();
    let error = LocalError::new(LocalErrorKind::BadRequest, "configuration unavailable");
    let mut turn = peer.client.start_turn(request(&peer, id));
    assert!(
        matches!(next(&mut turn).await, LocalUpdate::Submit { update: SubmissionUpdate::Sending { turn_id, .. } } if turn_id == id)
    );
    let (mut wire, request) = peer.request().await;
    assert!(matches!(request, Request::Submit { .. }));
    wire.reply(Response::Error(error.clone())).await;
    let result = receipt(&mut turn).await;
    assert_eq!(result.result, SubmissionResult::Rejected { error });
    assert!(!result.recovered);
    assert!(within(turn.next()).await.is_none());
    assert!(
        tokio::time::timeout(Duration::from_millis(20), peer.request())
            .await
            .is_err()
    );
}
#[tokio::test]
async fn connected_submission_waits_for_receipt_and_never_queries_early() {
    let peer = Peer::new();
    let id = new_turn_id();
    let (mut turn, mut wire) = submit(&peer, id).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(30), peer.request())
            .await
            .is_err()
    );
    wire.reply(accepted(id)).await;
    assert!(matches!(
        receipt(&mut turn).await,
        SubmissionReceipt {
            recovered: false,
            result: SubmissionResult::Accepted { .. },
            ..
        }
    ));
    let mut view = watch(&peer).await;
    view.head(id, false).await;
    view.reply(Response::View(ViewUpdate::Entry {
        entry: entry(id, true),
    }))
    .await;
    assert!(matches!(next(&mut turn).await, LocalUpdate::Head(_)));
    assert!(matches!(next(&mut turn).await, LocalUpdate::Entry { .. }));
    assert!(within(turn.next()).await.is_none());
}
#[tokio::test]
async fn unknown_or_lost_receipt_queries_by_turn_and_in_flight_is_not_terminal() {
    for lost in [false, true] {
        let peer = Peer::new();
        let id = new_turn_id();
        let (mut turn, mut wire) = submit(&peer, id).await;
        if !lost {
            wire.reply(Response::Acceptance(Acceptance::ConfirmationUnknown {
                turn_id: id,
                session_id: sid(),
            }))
            .await;
        }
        drop(wire);
        let (mut query, request) = peer.request().await;
        assert!(matches!(request, Request::Turn { turn_id } if turn_id == id));
        query.reply(Response::Turn(TurnLookup::InFlight)).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(5), turn.next())
                .await
                .is_err()
        );
        let (mut query, request) = peer.request().await;
        assert!(matches!(request, Request::Turn { turn_id } if turn_id == id));
        query
            .reply(Response::Turn(TurnLookup::Accepted {
                turn: detail(id, false),
            }))
            .await;
        assert!(matches!(
            receipt(&mut turn).await,
            SubmissionReceipt {
                recovered: true,
                result: SubmissionResult::Accepted { .. },
                ..
            }
        ));
        let _view = watch(&peer).await;
        let report = within(turn.detach()).await;
        assert_eq!(report.turn_id, Some(id));
    }
}
#[tokio::test]
async fn not_found_after_disconnection_is_a_terminal_not_accepted_receipt() {
    let peer = Peer::new();
    let id = new_turn_id();
    let (mut turn, wire) = submit(&peer, id).await;
    drop(wire);
    let (mut query, request) = peer.request().await;
    assert!(matches!(request, Request::Turn { turn_id } if turn_id == id));
    query
        .reply(Response::Error(LocalError::new(
            LocalErrorKind::NotFound,
            "not accepted",
        )))
        .await;
    assert!(matches!(
        receipt(&mut turn).await,
        SubmissionReceipt {
            recovered: true,
            result: SubmissionResult::NotAccepted,
            ..
        }
    ));
    assert!(within(turn.next()).await.is_none());
    assert!(
        tokio::time::timeout(Duration::from_millis(30), peer.request())
            .await
            .is_err()
    );
}
#[tokio::test]
async fn backend_rejection_and_failed_lookup_keep_their_receipt_sources() {
    for lookup_failed in [false, true] {
        let peer = Peer::new();
        let id = new_turn_id();
        let (mut turn, mut wire) = submit(&peer, id).await;
        let error = LocalError::new(LocalErrorKind::Busy, "busy");
        if lookup_failed {
            wire.reply(Response::Acceptance(Acceptance::ConfirmationUnknown {
                turn_id: id,
                session_id: sid(),
            }))
            .await;
            let (mut query, _) = peer.request().await;
            query.reply(Response::Error(error.clone())).await;
        } else {
            wire.reply(Response::Error(error.clone())).await;
        }
        let receipt = receipt(&mut turn).await;
        assert_eq!(receipt.recovered, lookup_failed);
        assert_eq!(
            receipt.result,
            if lookup_failed {
                SubmissionResult::Unknown { error: Some(error) }
            } else {
                SubmissionResult::Rejected { error }
            }
        );
        assert!(within(turn.next()).await.is_none());
    }
}
#[tokio::test]
async fn following_a_turn_filters_other_entries_and_preserves_head_and_text() {
    let peer = Peer::new();
    let id = new_turn_id();
    let other = new_turn_id();
    let mut turn = peer.client.follow_turn(detail(id, false));
    let mut view = watch(&peer).await;
    view.head(id, true).await;
    view.reply(Response::View(ViewUpdate::Entry {
        entry: entry(other, true),
    }))
    .await;
    view.reply(Response::View(ViewUpdate::Text(TextUpdate {
        node: NodeId(99),
        attempt: 0,
        offset: 0,
        segments: vec![TextSegment::Text {
            text: "其他轮".into(),
        }],
    })))
    .await;
    view.reply(Response::View(ViewUpdate::Entry {
        entry: entry(id, false),
    }))
    .await;
    view.reply(Response::View(ViewUpdate::Text(TextUpdate {
        node: NodeId(2),
        attempt: 0,
        offset: 0,
        segments: vec![TextSegment::Text {
            text: "本轮".into(),
        }],
    })))
    .await;
    view.reply(Response::View(ViewUpdate::Entry {
        entry: entry(id, true),
    }))
    .await;
    assert!(matches!(next(&mut turn).await, LocalUpdate::Head(h) if h.owner));
    assert!(
        matches!(next(&mut turn).await, LocalUpdate::Entry { entry } if matches!(*entry, ConversationEntry::Step { content: Node::Step { turn_id, .. }, .. } if turn_id == id))
    );
    assert!(matches!(
        next(&mut turn).await,
        LocalUpdate::Text(TextUpdate { offset: 0, .. })
    ));
    assert!(matches!(next(&mut turn).await, LocalUpdate::Entry { .. }));
    assert!(within(turn.next()).await.is_none());
}
#[tokio::test]
async fn unowned_head_waits_for_its_entry_before_finishing_and_keeps_diagnostic() {
    let peer = Peer::new();
    let id = new_turn_id();
    let diagnostic = LocalError::recovery(RecoveryCause::WorkerFailed);
    let mut turn = peer.client.follow_turn(detail(id, false));
    let mut view = watch(&peer).await;
    let mut h = head(id, false);
    h.diagnostic = Some(diagnostic.clone());
    view.reply(Response::View(ViewUpdate::Head(h))).await;
    assert!(matches!(next(&mut turn).await, LocalUpdate::Head(_)));
    assert!(
        tokio::time::timeout(Duration::from_millis(20), turn.next())
            .await
            .is_err()
    );
    view.reply(Response::View(ViewUpdate::Entry {
        entry: entry(id, false),
    }))
    .await;
    assert!(matches!(next(&mut turn).await, LocalUpdate::Entry { .. }));
    assert!(within(turn.next()).await.is_none());
    assert_eq!(
        turn.detach().await.head.unwrap().diagnostic,
        Some(diagnostic)
    );
}
#[tokio::test]
async fn cancellation_false_and_live_evidence_share_one_retry_in_both_orders() {
    for evidence_first in [false, true] {
        let peer = Peer::new();
        let id = new_turn_id();
        let (mut turn, mut wire) = submit(&peer, id).await;
        turn.cancel();
        let mut cancel = cancellation(&peer, id).await;
        wire.reply(accepted(id)).await;
        let mut view = watch(&peer).await;
        if evidence_first {
            view.head(id, true).await;
        }
        cancel.reply(Response::Cancelled { owned: false }).await;
        cancel_status(&mut turn, CancelStatus::NotTaken).await;
        if !evidence_first {
            view.head(id, true).await;
        }
        let mut retry = cancellation(&peer, id).await;
        retry.reply(Response::Cancelled { owned: true }).await;
        cancel_status(&mut turn, CancelStatus::Taken).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(25), peer.request())
                .await
                .is_err()
        );
        let report = within(turn.detach()).await;
        assert_eq!(report.cancel, Some(CancelStatus::Taken));
        assert!(report.cancel_error.is_none());
    }
}
#[tokio::test]
async fn taken_cancel_does_not_wait_for_view_or_retry_and_drop_does_not_cancel() {
    let peer = Peer::new();
    let id = new_turn_id();
    let (mut turn, wire) = submit(&peer, id).await;
    turn.cancel();
    let mut cancel = cancellation(&peer, id).await;
    cancel.reply(Response::Cancelled { owned: true }).await;
    cancel_status(&mut turn, CancelStatus::Taken).await;
    let report = within(turn.detach()).await;
    assert_eq!(report.cancel, Some(CancelStatus::Taken));
    assert_eq!(report.turn_id, Some(id));
    // 没有提交回执仍可交接，后台原 Submit 责任不由客户端 Drop 撤销。
    drop(wire);
    let turn = peer.client.observe(sid());
    let _view = watch(&peer).await;
    turn.reconnect();
    let _view = watch(&peer).await;
    drop(turn);
    assert!(
        tokio::time::timeout(Duration::from_millis(25), peer.request())
            .await
            .is_err()
    );
}
#[tokio::test]
async fn cancellation_timeout_keeps_error_and_identity_in_detach() {
    let peer = Peer::new();
    let id = new_turn_id();
    let (turn, _wire) = submit(&peer, id).await;
    turn.cancel();
    let _cancel = cancellation(&peer, id).await;
    let task = tokio::spawn(turn.detach());
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(6)).await;
    tokio::time::resume();
    let report = within(task).await.unwrap();
    assert_eq!(report.turn_id, Some(id));
    assert_eq!(report.cancel, Some(CancelStatus::Unknown));
    assert_eq!(report.cancel_error.unwrap().kind, LocalErrorKind::Transport);
}
fn user_entry(id: TurnId) -> Box<ConversationEntry> {
    Box::new(ConversationEntry::UserMessage {
        node: NodeId(1),
        ordinal: 1,
        content: Node::UserMessage {
            parent: NodeId(0),
            turn_id: id,
            accepted_at: detail(id, true).accepted_at,
            agent_name: "local-test".into(),
            model: "test".into(),
            provider: "openai-compatible".into(),
            limits: lattice_session::store::TurnExecutionLimits {
                timeout_secs: 300,
                max_model_steps: 32,
                max_model_attempts: 2,
            },
            config_digest: [0; 32],
            message: NativeMessage::new(MessageRole::User, None, vec![]),
        },
    })
}
#[tokio::test]
async fn following_an_already_closed_turn_delivers_its_step_after_the_user_message() {
    let peer = Peer::new();
    let id = new_turn_id();
    let mut turn = peer.client.follow_turn(detail(id, true));
    let mut view = watch(&peer).await;
    view.head(id, false).await;
    view.reply(Response::View(ViewUpdate::Entry {
        entry: user_entry(id),
    }))
    .await;
    view.reply(Response::View(ViewUpdate::Entry {
        entry: entry(id, true),
    }))
    .await;
    assert!(matches!(next(&mut turn).await, LocalUpdate::Head(_)));
    assert!(matches!(
        next(&mut turn).await,
        LocalUpdate::Entry { entry } if matches!(*entry, ConversationEntry::UserMessage { .. })
    ));
    assert!(matches!(
        next(&mut turn).await,
        LocalUpdate::Entry { entry } if matches!(*entry, ConversationEntry::Step { .. })
    ));
    assert!(within(turn.next()).await.is_none());
}
#[tokio::test]
async fn owner_retirement_head_waits_for_the_latest_position_entry() {
    let peer = Peer::new();
    let id = new_turn_id();
    let mut turn = peer.client.follow_turn(detail(id, false));
    let mut view = watch(&peer).await;
    view.head(id, true).await;
    view.reply(Response::View(ViewUpdate::Entry {
        entry: entry(id, false),
    }))
    .await;
    assert!(matches!(next(&mut turn).await, LocalUpdate::Head(_)));
    assert!(matches!(next(&mut turn).await, LocalUpdate::Entry { .. }));
    view.head(id, false).await;
    assert!(matches!(
        next(&mut turn).await,
        LocalUpdate::Head(SessionHead { owner: false, .. })
    ));
    let final_entry = entry(id, true);
    // 旧 worker 此时已经关闭视图；断管不能掩盖“最终条目未交付”的断言。
    let _ = view
        .writer
        .write(
            &mut view.socket,
            &Response::View(ViewUpdate::Entry {
                entry: final_entry.clone(),
            }),
        )
        .await;
    assert!(
        matches!(within(turn.next()).await, Some(LocalUpdate::Entry { entry }) if entry == final_entry),
        "退休会话头之后必须交付最终当前位置条目"
    );
    assert!(within(turn.next()).await.is_none());
    assert_eq!(turn.detach().await.summary, Some(detail(id, true).summary));
}
#[tokio::test]
async fn late_acceptance_after_cancel_handoff_follows_the_submitted_turn() {
    for root in [true, false] {
        for recover in [false, true] {
            let peer = Peer::new();
            let id = new_turn_id();
            let mut input = request(&peer, id);
            input.session = SubmitSession::Existing(sid());
            let mut turn = peer.client.start_turn(input);
            assert!(matches!(
                next(&mut turn).await,
                LocalUpdate::Submit {
                    update: SubmissionUpdate::Sending { new: false, .. }
                }
            ));
            let (mut wire, _) = peer.request().await;
            turn.cancel();
            let mut cancel = cancellation(&peer, id).await;
            cancel.reply(Response::Cancelled { owned: false }).await;
            cancel_status(&mut turn, CancelStatus::NotTaken).await;
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(5)).await;
            tokio::time::resume();
            assert!(matches!(
                receipt(&mut turn).await.result,
                SubmissionResult::Unknown { .. }
            ));
            let mut old_view = watch(&peer).await;
            let old = new_turn_id();
            let mut old_head = head(old, false);
            if root {
                old_head.position = NodeId(0);
                old_head.turn_id = None;
            }
            old_view
                .reply(Response::View(ViewUpdate::Head(old_head.clone())))
                .await;
            assert!(matches!(next(&mut turn).await, LocalUpdate::Head(_)));
            if !root {
                old_view
                    .reply(Response::View(ViewUpdate::Entry {
                        entry: entry(old, true),
                    }))
                    .await;
                // 后继 head 的交付确认前一条旧轮 entry 已被处理。
                old_view
                    .reply(Response::View(ViewUpdate::Head(old_head)))
                    .await;
                assert!(matches!(next(&mut turn).await, LocalUpdate::Head(_)));
            }
            if recover {
                drop(wire);
                let (mut query, request) = peer.request().await;
                assert!(matches!(request, Request::Turn { turn_id } if turn_id == id));
                query
                    .reply(Response::Turn(TurnLookup::Accepted {
                        turn: detail(id, false),
                    }))
                    .await;
            } else {
                wire.reply(accepted(id)).await;
            }
            let (mut current, request) = tokio::select! {
                biased;
                update = turn.next() => panic!("迟到接受后必须继续跟随本轮，提前退出或多余记录：{update:?}"),
                request = peer.request() => request,
            };
            assert!(matches!(request, Request::Watch { .. }));
            current.head(id, false).await;
            current
                .reply(Response::View(ViewUpdate::Entry {
                    entry: entry(id, true),
                }))
                .await;
            assert!(matches!(next(&mut turn).await, LocalUpdate::Head(_)));
            assert!(
                matches!(next(&mut turn).await, LocalUpdate::Entry { entry } if entry == self::entry(id, true))
            );
            assert!(within(turn.next()).await.is_none());
            let report = turn.detach().await;
            assert_eq!(report.summary, Some(detail(id, true).summary));
            assert!(matches!(
                report.submission.unwrap().result,
                SubmissionResult::Accepted { .. }
            ));
        }
    }
}
#[tokio::test]
async fn following_an_unclosed_orphan_away_from_current_position_keeps_its_result() {
    for visible in [true, false] {
        let peer = Peer::new();
        let orphan = new_turn_id();
        let current = new_turn_id();
        let known = detail(orphan, false);
        let mut turn = peer.client.follow_turn(known.clone());
        let mut view = watch(&peer).await;
        let mut current_head = head(current, true);
        current_head.position = NodeId(4);
        view.reply(Response::View(ViewUpdate::Head(current_head)))
            .await;
        if visible {
            view.reply(Response::View(ViewUpdate::Entry {
                entry: user_entry(orphan),
            }))
            .await;
            view.reply(Response::View(ViewUpdate::Entry {
                entry: entry(orphan, false),
            }))
            .await;
        }
        let mut current_entry = entry(current, false);
        if let ConversationEntry::Step {
            node,
            step,
            content: Node::Step { parent, .. },
            ..
        } = current_entry.as_mut()
        {
            *node = NodeId(4);
            step.node = NodeId(4);
            *parent = NodeId(3);
        }
        view.reply(Response::View(ViewUpdate::Entry {
            entry: current_entry,
        }))
        .await;
        assert!(matches!(
            next(&mut turn).await,
            LocalUpdate::Head(SessionHead { owner: true, .. })
        ));
        if visible {
            for _ in 0..2 {
                assert!(matches!(next(&mut turn).await, LocalUpdate::Entry { .. }));
            }
        }
        assert!(
            within(turn.next()).await.is_none(),
            "别的轮有 Owner 不能延长已消失执行者的跟随"
        );
        assert_eq!(turn.detach().await.summary, Some(known.summary));
    }
}
#[tokio::test]
async fn a_new_cancel_intent_does_not_inherit_the_previous_in_flight_receipt() {
    let peer = Peer::new();
    let id = new_turn_id();
    let (mut turn, mut wire) = submit(&peer, id).await;
    turn.cancel();
    let mut old = cancellation(&peer, id).await;
    turn.cancel();
    old.reply(Response::Cancelled { owned: true }).await;
    let mut current = cancellation(&peer, id).await;
    current
        .reply(Response::Error(LocalError::new(
            LocalErrorKind::Busy,
            "new intent failed",
        )))
        .await;
    cancel_status(&mut turn, CancelStatus::Unknown).await;
    wire.reply(Response::Error(LocalError::new(
        LocalErrorKind::BadRequest,
        "request refused",
    )))
    .await;
    receipt(&mut turn).await;
    let report = within(turn.detach()).await;
    assert_eq!(report.cancel_error.unwrap().message, "new intent failed");
    assert_ne!(report.cancel, Some(CancelStatus::Taken));
}
#[tokio::test]
async fn late_definitive_receipts_notify_submission_only_but_keep_one_receipt_with_a_watch() {
    for own_view in [false, true] {
        for outcome in ["accepted", "rejected", "not_accepted"] {
            let peer = Peer::new();
            let id = new_turn_id();
            let mut turn = if own_view {
                peer.client.start_turn(request(&peer, id))
            } else {
                peer.client.start_submission(request(&peer, id))
            };
            next(&mut turn).await;
            let (mut submit, request) = peer.request().await;
            assert!(matches!(request, Request::Submit { .. }));
            turn.cancel();
            let mut cancel = cancellation(&peer, id).await;
            cancel.reply(Response::Cancelled { owned: false }).await;
            cancel_status(&mut turn, CancelStatus::NotTaken).await;
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(5)).await;
            tokio::time::resume();
            let unknown = receipt(&mut turn).await;
            assert!(matches!(unknown.result, SubmissionResult::Unknown { .. }));
            assert!(!unknown.recovered);
            // 保留尚未回复的首次 watch，让迟到回执与观察错误的到达顺序无关。
            let _pending_view = if own_view {
                Some(watch(&peer).await)
            } else {
                None
            };
            let expected = match outcome {
                "accepted" => {
                    submit.reply(accepted(id)).await;
                    SubmissionResult::Accepted {
                        turn: TurnView::from(&detail(id, false)),
                    }
                }
                "rejected" => {
                    let error =
                        LocalError::new(LocalErrorKind::BadRequest, "late backend rejection");
                    submit.reply(Response::Error(error.clone())).await;
                    SubmissionResult::Rejected { error }
                }
                _ => {
                    drop(submit);
                    let (mut lookup, request) = peer.request().await;
                    assert!(matches!(request, Request::Turn { turn_id } if turn_id == id));
                    lookup
                        .reply(Response::Error(LocalError::new(
                            LocalErrorKind::NotFound,
                            "not accepted",
                        )))
                        .await;
                    SubmissionResult::NotAccepted
                }
            };
            if !own_view {
                let late = tokio::time::timeout(Duration::from_secs(3), turn.next()).await;
                assert!(
                    matches!(&late, Ok(Some(LocalUpdate::Submit { update: SubmissionUpdate::Receipt(receipt) })) if receipt.turn_id == id && receipt.result == expected && receipt.recovered == (outcome == "not_accepted")),
                    "无视图句柄必须实时交付 Unknown 之后的确定回执（{outcome}）：{late:?}"
                );
                if outcome == "accepted" {
                    turn.apply_view(ViewUpdate::Head(head(id, false))).await;
                    turn.apply_view(ViewUpdate::Entry {
                        entry: user_entry(id),
                    })
                    .await;
                    turn.apply_view(ViewUpdate::Entry {
                        entry: entry(id, true),
                    })
                    .await;
                }
            } else if outcome == "accepted" {
                let mut view = tokio::select! {
                    view = watch(&peer) => view,
                    update = turn.next() => panic!("自带视图模式不交付第二条回执：{update:?}"),
                };
                view.head(id, false).await;
                view.reply(Response::View(ViewUpdate::Entry {
                    entry: entry(id, true),
                }))
                .await;
                assert!(matches!(next(&mut turn).await, LocalUpdate::Head(_)));
                assert!(matches!(next(&mut turn).await, LocalUpdate::Entry { .. }));
            }
            let extra = within(turn.next()).await;
            assert!(
                extra.is_none(),
                "确定回执不得重复交付（own_view={own_view}, {outcome}）：{extra:?}"
            );
            let report = within(turn.detach()).await;
            assert_eq!(report.submission.unwrap().result, expected);
            if outcome == "accepted" {
                assert_eq!(report.summary, Some(detail(id, true).summary));
            }
        }
    }
}
#[tokio::test]
async fn cancel_handoff_preserves_late_backend_rejection_separately_from_transport() {
    for transport in [false, true] {
        let peer = Peer::new();
        let id = new_turn_id();
        let (mut turn, mut wire) = submit(&peer, id).await;
        turn.cancel();
        let mut cancel = cancellation(&peer, id).await;
        cancel.reply(Response::Cancelled { owned: false }).await;
        cancel_status(&mut turn, CancelStatus::NotTaken).await;
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(5)).await;
        tokio::time::resume();
        assert!(matches!(
            receipt(&mut turn).await.result,
            SubmissionResult::Unknown { .. }
        ));
        let mut view = watch(&peer).await;
        view.reply(Response::Error(LocalError::new(
            LocalErrorKind::NotFound,
            "not yet accepted",
        )))
        .await;
        if transport {
            drop(wire);
            let (mut query, request) = peer.request().await;
            assert!(matches!(request, Request::Turn { turn_id } if turn_id == id));
            query
                .reply(Response::Error(LocalError::new(
                    LocalErrorKind::NotFound,
                    "not accepted",
                )))
                .await;
        } else {
            wire.reply(Response::Error(LocalError::new(
                LocalErrorKind::BadRequest,
                "backend rejected",
            )))
            .await;
        }
        // 继续读至退出，迟到回执不能制造第二条 receipt。
        while let Some(update) = within(turn.next()).await {
            assert!(!matches!(
                update,
                LocalUpdate::Submit {
                    update: SubmissionUpdate::Receipt(_)
                }
            ));
        }
        let report = turn.detach().await;
        if transport {
            assert!(report.submission_diagnostic.is_some());
        } else {
            assert!(
                matches!(report.submission.unwrap().result, SubmissionResult::Rejected { error } if error.message == "backend rejected")
            );
            assert!(report.submission_diagnostic.is_none());
        }
    }
}
#[tokio::test]
async fn updates_round_trip_the_seven_record_shapes() {
    let id = new_turn_id();
    let updates = vec![
        LocalUpdate::Submit {
            update: SubmissionUpdate::Sending {
                turn_id: id,
                session_id: sid(),
                new: true,
            },
        },
        LocalUpdate::Submit {
            update: SubmissionUpdate::Receipt(SubmissionReceipt {
                turn_id: id,
                recovered: true,
                result: SubmissionResult::NotAccepted,
            }),
        },
        LocalUpdate::Head(head(id, true)),
        LocalUpdate::Entry {
            entry: entry(id, true),
        },
        LocalUpdate::Text(TextUpdate {
            node: NodeId(2),
            attempt: 0,
            offset: 3,
            segments: vec![TextSegment::Reasoning { text: "想".into() }],
        }),
        LocalUpdate::Cancel {
            turn_id: id,
            status: CancelStatus::Taken,
            owned: Some(true),
            error: None,
        },
        LocalUpdate::Connection {
            status: ConnectionStatus::Eof,
        },
        LocalUpdate::Error {
            error: LocalError::new(LocalErrorKind::BadRequest, "invalid"),
        },
    ];
    for (update, expected) in updates.into_iter().zip([
        "submit",
        "submit",
        "head",
        "entry",
        "text",
        "cancel",
        "connection",
        "error",
    ]) {
        let wire = serde_json::to_value(&update).unwrap();
        assert_eq!(wire["type"], expected);
        if expected == "text" {
            assert_eq!(wire["segments"][0]["kind"], "reasoning");
        }
        assert_eq!(serde_json::from_value::<LocalUpdate>(wire).unwrap(), update);
    }
}
#[tokio::test(start_paused = true)]
async fn idle_timeout_only_applies_to_an_owned_session_and_preserves_known_summary() {
    let peer = Peer::new();
    let id = new_turn_id();
    let mut turn = peer.client.observe(sid());
    let mut view = watch(&peer).await;
    view.head(id, false).await;
    next(&mut turn).await;
    tokio::time::advance(Duration::from_secs(301)).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(1), turn.next())
            .await
            .is_err()
    );
    view.head(id, true).await;
    next(&mut turn).await;
    view.reply(Response::View(ViewUpdate::Entry {
        entry: entry(id, false),
    }))
    .await;
    next(&mut turn).await;
    tokio::time::advance(Duration::from_secs(301)).await;
    assert!(matches!(
        next(&mut turn).await,
        LocalUpdate::Connection {
            status: ConnectionStatus::Idle
        }
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(1), turn.next())
            .await
            .is_err()
    );
    within(turn.detach()).await;
}
#[tokio::test]
async fn clean_view_eof_and_a_partial_frame_failure_are_connection_records() {
    use tokio::io::AsyncWriteExt;
    for partial in [false, true] {
        let peer = Peer::new();
        let id = new_turn_id();
        let mut turn = peer.client.observe(sid());
        let mut view = watch(&peer).await;
        view.head(id, false).await;
        next(&mut turn).await;
        if partial {
            view.socket.write_all(b"LTLO").await.unwrap();
        }
        drop(view);
        let update = next(&mut turn).await;
        assert!(
            if partial {
                matches!(
                    update,
                    LocalUpdate::Connection {
                        status: ConnectionStatus::Error(_)
                    }
                )
            } else {
                matches!(
                    update,
                    LocalUpdate::Connection {
                        status: ConnectionStatus::Eof
                    }
                )
            },
            "{update:?}"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(1), turn.next())
                .await
                .is_err()
        );
        within(turn.detach()).await;
    }
}
#[tokio::test]
async fn cancel_before_sending_never_writes_a_submit_and_still_finishes_its_receipt() {
    let peer = Peer::new();
    let id = new_turn_id();
    let mut turn = peer.client.start_turn(request(&peer, id));
    turn.cancel();
    assert!(matches!(
        next(&mut turn).await,
        LocalUpdate::Submit {
            update: SubmissionUpdate::Sending { .. }
        }
    ));
    let receipt = receipt(&mut turn).await;
    assert!(
        matches!(receipt.result, SubmissionResult::NotAccepted),
        "发出前取消确定未接受，不是后台拒绝"
    );
    assert!(!receipt.recovered);
    assert!(within(turn.next()).await.is_none());
    assert!(
        tokio::time::timeout(Duration::from_millis(20), peer.request())
            .await
            .is_err()
    );
}
#[tokio::test(start_paused = true)]
async fn reconnect_after_idle_starts_a_fresh_session_watch() {
    let peer = Peer::new();
    let id = new_turn_id();
    let mut turn = peer.client.observe(sid());
    let mut view = watch(&peer).await;
    view.head(id, true).await;
    next(&mut turn).await;
    tokio::time::advance(Duration::from_secs(301)).await;
    assert!(matches!(
        next(&mut turn).await,
        LocalUpdate::Connection {
            status: ConnectionStatus::Idle
        }
    ));
    turn.reconnect();
    let mut fresh = watch(&peer).await;
    fresh.head(id, false).await;
    assert!(matches!(
        next(&mut turn).await,
        LocalUpdate::Head(SessionHead { owner: false, .. })
    ));
    assert_eq!(within(turn.detach()).await.head.unwrap().turn_id, Some(id));
}

async fn status_against(
    backend: impl FnOnce(
        tokio::net::UnixStream,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
    + Send
    + 'static,
) -> LocalError {
    let directory = tempfile::tempdir().unwrap();
    let layout = crate::StateLayout::in_state_root(directory.path());
    let (_instance, listener) = ipc::Instance::bind(&layout).unwrap();
    let listener = UnixListener::from_std(listener).unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        backend(stream).await;
    });
    let error = LocalClient::new(layout).status().await.unwrap_err();
    server.await.unwrap();
    error
}

const OLDER_BACKEND: &str = "the running backend is an incompatible older version; stop it with the older binary's `lattice daemon stop`";
const SILENT_CLOSE: &str = "the backend closed the connection without replying; if an older backend is still running, stop it with the older binary's `lattice daemon stop`";

/// 已发布的旧后台读到不认识的魔数就关连接，不回写任何字节；请求剩下的部分留在它的接收队列里。
#[tokio::test]
async fn status_hints_at_an_older_backend_that_drops_the_connection_mid_request() {
    let error = status_against(|mut stream| {
        Box::pin(async move {
            let mut magic = [0; 8];
            stream.read_exact(&mut magic).await.unwrap();
            drop(stream);
        })
    })
    .await;
    assert_eq!(
        (error.kind, error.message.as_str()),
        (LocalErrorKind::Transport, SILENT_CLOSE)
    );
}

#[tokio::test]
async fn status_hints_when_the_backend_reads_the_request_and_closes_without_replying() {
    let error = status_against(|mut stream| {
        Box::pin(async move {
            let mut magic = [0; 8];
            stream.read_exact(&mut magic).await.unwrap();
            let len = stream.read_u32().await.unwrap() as usize;
            stream.read_exact(&mut vec![0; len]).await.unwrap();
            drop(stream);
        })
    })
    .await;
    assert_eq!(
        (error.kind, error.message.as_str()),
        (LocalErrorKind::Transport, SILENT_CLOSE)
    );
}

/// 从本版本起，后台读到别的魔数会先回写自己的魔数；更旧的魔数要明确报版本不兼容。
#[tokio::test]
async fn status_reports_an_older_backend_that_announces_its_magic() {
    let error = status_against(|mut stream| {
        Box::pin(async move {
            // 字面量不能从当前魔数推导，否则以后升级会把这条回归一起改掉。
            stream.write_all(b"LTLOCAL5").await.unwrap();
            stream.write_u32(4).await.unwrap();
            stream.write_all(b"null").await.unwrap();
            let _held = stream;
        })
    })
    .await;
    assert_eq!(
        (error.kind, error.message.as_str()),
        (LocalErrorKind::IncompatibleBackend, OLDER_BACKEND)
    );
}

#[tokio::test]
async fn status_reports_a_backend_newer_than_the_client() {
    let error = status_against(|mut stream| {
        Box::pin(async move {
            stream.write_all(b"LTLOCAL:").await.unwrap();
            stream.write_u32(4).await.unwrap();
            stream.write_all(b"null").await.unwrap();
            let _held = stream;
        })
    })
    .await;
    assert_eq!(
        (error.kind, error.message.as_str()),
        (
            LocalErrorKind::IncompatibleBackend,
            "the running backend is newer than this client; use the newer binary",
        )
    );
}

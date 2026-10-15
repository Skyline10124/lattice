// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::client::SubmissionReply;
use super::{ipc, types::*, *};
use crate::turn::TextSegment;
use std::time::Duration;
use tokio::net::{UnixListener, UnixStream};

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
    async fn snapshot(&mut self, view: ObservationSnapshot) {
        self.reply(Response::Observation(Observation::Snapshot {
            view: Box::new(view),
        }))
        .await;
    }
    async fn finished(&mut self, view: &ObservationSnapshot) {
        self.reply(Response::Observation(Observation::Finished {
            turn: view.selected_turn.as_ref().map(|t| TurnRef {
                session_id: t.session_id,
                turn_id: t.turn_id,
            }),
            cursor: view.cursor.clone(),
            history: history(view),
        }))
        .await;
    }
    async fn delta(&mut self, id: TurnId, sequence: u64) {
        self.reply(Response::Observation(Observation::Event {
            cursor: cursor(sequence),
            event: crate::TurnEvent::TokenDelta {
                key: key(id),
                content: format!("delta-{sequence}"),
            },
        }))
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
fn key(turn_id: TurnId) -> lancet_protocol::ModelAttemptKey {
    lancet_protocol::ModelAttemptKey {
        turn_id,
        model_step_ordinal: 0,
        attempt_ordinal: 0,
    }
}
fn cursor(sequence: u64) -> EventCursor {
    EventCursor {
        epoch: "peer".into(),
        sequence,
    }
}
fn view(id: TurnId, head: u64, closed: bool, owned: bool) -> ObservationSnapshot {
    let session_id = sid();
    ObservationSnapshot {
        session: SessionRecord {
            session_id,
            project_store_key: crate::ProjectStoreKey::from_canonical_path_bytes(b"/fixture"),
            current: NodeId(if closed { head } else { head + 1 }),
            created_at: "2026-09-27T00:00:00Z".parse().unwrap(),
            updated_at: "2026-09-27T00:00:00Z".parse().unwrap(),
        },
        current_agent: Some("test".into()),
        selected_turn: Some(TurnDetail {
            turn_id: id,
            session_id,
            user_node: NodeId(1),
            agent_name: "test".into(),
            accepted_at: "2026-09-27T00:00:00Z".parse().unwrap(),
            ended_at: "2026-09-27T00:00:01Z".parse().unwrap(),
            summary: TurnSummary {
                outcome: if closed {
                    PublicTurnOutcome::Completed
                } else {
                    PublicTurnOutcome::Failed
                },
                progress: if closed {
                    StepProgress::Closed {
                        end: StepEnd::Finish,
                    }
                } else {
                    StepProgress::Requesting
                },
                notice: None,
            },
            usage: UsageSummary::Complete {
                total: lattice_conversation::TokenUsage {
                    prompt_tokens: 0,
                    completion_tokens: 0,
                    total_tokens: 0,
                },
            },
            steps: vec![],
        }),
        unclosed_step: (!closed).then(|| {
            (
                NodeId(head + 1),
                Node::Step {
                    parent: NodeId(head),
                    turn_id: id,
                    step_ordinal: 0,
                    progress: StepProgress::Requesting,
                    attempts: vec![lattice_session::store::AttemptRecord {
                        usage: None,
                        progress: AttemptProgress::Sent,
                        retry_delay_ms: None,
                    }],
                    reply: None,
                    slots: vec![],
                    updated_at: "2026-09-27T00:00:01Z".parse().unwrap(),
                },
            )
        }),
        history: HistoryAnchor {
            session_id,
            head: NodeId(head),
        },
        attempt: (!closed && owned).then_some(AttemptSegments {
            key: key(id),
            segments: vec![TextSegment::Text {
                text: "prefix".into(),
            }],
        }),
        cursor: cursor(head),
        owned,
    }
}
fn history(view: &ObservationSnapshot) -> LocalHistory {
    let id = view
        .selected_turn
        .as_ref()
        .map_or_else(new_turn_id, |t| t.turn_id);
    LocalHistory {
        anchor: view.history.clone(),
        entries: (1..=view.history.head.0)
            .map(|n| HistoryEntry {
                node: NodeId(n),
                agent_name: "test".into(),
                summary: view.selected_turn.as_ref().unwrap().summary.clone(),
                retries: vec![],
                content: Node::UserMessage {
                    parent: NodeId(n - 1),
                    turn_id: id,
                    accepted_at: "2026-09-27T00:00:00Z".parse().unwrap(),
                    agent_name: "test".into(),
                    model: "model".into(),
                    provider: "openai".into(),
                    limits: lattice_session::store::TurnExecutionLimits {
                        timeout_secs: 300,
                        max_model_steps: 32,
                        max_model_attempts: 3,
                    },
                    config_digest: [0; 32],
                    message: NativeMessage::new(
                        MessageRole::User,
                        None,
                        vec![lattice_conversation::MessagePart::semantic_only(
                            MessageSemantic::Text {
                                text: format!("node-{n} 原文"),
                            },
                        )],
                    ),
                },
            })
            .collect(),
    }
}
fn page(history: LocalHistory) -> HistoryPageView {
    HistoryPageView {
        anchor: history.anchor,
        next: None,
        fragments: history
            .entries
            .into_iter()
            .map(|entry| HistoryFragmentView {
                agent_name: entry.agent_name.clone(),
                summary: entry.summary.clone(),
                fragment: NodeFragment {
                    node: entry.node,
                    utf8_offset: 0,
                    json_fragment: serde_json::to_string(&entry.content).unwrap(),
                    complete: true,
                    agent_name: entry.agent_name,
                    summary: entry.summary,
                    retries: entry.retries,
                },
            })
            .collect(),
    }
}

#[tokio::test]
async fn history_fragment_progress_refreshes_idle_without_live_observation() {
    let peer = Peer::new();
    let id = new_turn_id();
    let fixed = view(id, 1, false, false);
    let mut turn = peer.client.observe(Some(sid()), Some(id));
    let (mut stream, request) = peer.request().await;
    assert!(matches!(request, Request::Subscribe { .. }));
    stream.snapshot(fixed.clone()).await;
    update_until(&mut turn, |u| matches!(u, LocalUpdate::Reset { .. })).await;
    let (mut body, _) = body_request(&peer).await;
    let response = serde_json::to_string(&Response::History(page(history(&fixed)))).unwrap();
    let split = response
        .char_indices()
        .map(|(n, _)| n)
        .find(|&n| n >= response.len() / 2)
        .unwrap();
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(200)).await;
    ipc::write(&mut body.socket, &serde_json::json!({"message_id":0,"offset":0,"json_fragment": &response[..split],"complete":false})).await.unwrap();
    // 给接收任务处理完整第一帧；没有完整逻辑对象时不能交付 History。
    assert!(
        tokio::time::timeout(Duration::from_millis(1), turn.next())
            .await
            .is_err()
    );
    tokio::time::advance(Duration::from_secs(200)).await;
    ipc::write(&mut body.socket, &serde_json::json!({"message_id":0,"offset":split,"json_fragment": &response[split..],"complete":true})).await.unwrap();
    let update = update_until(&mut turn, |u| matches!(u, LocalUpdate::History { .. })).await;
    assert!(
        matches!(
            update,
            LocalUpdate::History {
                result: Ok(_),
                stale: false,
                ..
            }
        ),
        "连续进度不得被整次读取时长误判 idle"
    );
    tokio::time::resume();
    let report = turn.detach().await;
    assert!(report.history_error.is_none());
}
fn request(peer: &Peer, id: TurnId) -> TurnRequest {
    TurnRequest {
        session_id: None,
        turn_id: id,
        workspace: peer._directory.path().into(),
        input: "byte-exact 原文".into(),
        agent: Some("test".into()),
    }
}
fn accepted(id: TurnId) -> Acceptance {
    Acceptance::Accepted {
        turn: TurnView::from(view(id, 1, false, true).selected_turn.as_ref().unwrap()),
    }
}
fn needs_configuration() -> Response {
    Response::Acceptance(Acceptance::NeedsConfiguration {
        agent_name: "test".into(),
    })
}
async fn update_until(
    turn: &mut LocalTurn,
    predicate: impl Fn(&LocalUpdate) -> bool,
) -> LocalUpdate {
    within(async {
        loop {
            let update = turn.next().await.expect("update");
            if predicate(&update) {
                return update;
            }
        }
    })
    .await
}
async fn state(turn: &mut LocalTurn) {
    update_until(turn, |u| matches!(u, LocalUpdate::State { .. })).await;
}
async fn cancel_status(turn: &mut LocalTurn, wanted: CancelStatus) -> LocalUpdate {
    update_until(
        turn,
        |u| matches!(u, LocalUpdate::Cancel { status, .. } if *status == wanted),
    )
    .await
}
async fn body_request(peer: &Peer) -> (Wire, HistoryAnchor) {
    let (wire, request) = peer.request().await;
    let Request::History {
        anchor,
        cursor: None,
        ..
    } = request
    else {
        panic!("history request")
    };
    (wire, anchor)
}
async fn body(peer: &Peer, fixed: &ObservationSnapshot) {
    let (mut wire, anchor) = body_request(peer).await;
    assert_eq!(anchor, fixed.history);
    wire.reply(Response::History(page(history(fixed)))).await;
}
async fn cancel_request(peer: &Peer, id: TurnId) -> Wire {
    loop {
        let (mut wire, request) = peer.request().await;
        match request {
            Request::Cancel { target } => {
                assert_eq!(target.turn_id, id);
                return wire;
            }
            Request::History { anchor, .. } => {
                // 已取代的正文连接允许拒绝响应，不能阻塞控制请求。
                let response =
                    Response::History(page(history(&view(id, anchor.head.0, false, true))));
                let _ = wire.writer.write(&mut wire.socket, &response).await;
            }
            _ => panic!("unexpected request while cancelling"),
        }
    }
}
async fn initial(peer: &Peer, expected: &TurnRequest) -> (Wire, Wire) {
    let mut probe = None;
    let mut observation = None;
    for _ in 0..2 {
        let (wire, req) = peer.request().await;
        match req {
            Request::Submit {
                request,
                configuration: None,
            } => {
                assert!(request == *expected);
                probe = Some(wire);
            }
            Request::Subscribe {
                target,
                cursor: None,
            } => {
                assert_eq!(target.turn_id, expected.turn_id);
                assert_eq!(target.session_id, expected.session_id);
                observation = Some(wire);
            }
            _ => panic!("initial operations"),
        }
    }
    (probe.unwrap(), observation.unwrap())
}
async fn no_cancel(peer: &Peer) {
    let result = tokio::time::timeout(Duration::from_millis(80), async {
        loop {
            let (mut wire, request) = peer.request().await;
            if let Request::History { anchor, .. } = request {
                let _ = wire
                    .writer
                    .write(
                        &mut wire.socket,
                        &Response::History(page(history(&view(
                            new_turn_id(),
                            anchor.head.0,
                            false,
                            true,
                        )))),
                    )
                    .await;
            } else {
                panic!("unexpected additional control request");
            }
        }
    })
    .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn fixed_root_path_preserves_utf8_fragments_and_agent_attribution() {
    let peer = Peer::new();
    let id = new_turn_id();
    let fixed = view(id, 2, true, false);
    let mut expected = history(&fixed);
    // 节点跨页期间 Turn 关闭，取完成节点的那一片带回的结论。
    expected.entries[1].summary = TurnSummary {
        outcome: PublicTurnOutcome::Completed,
        progress: StepProgress::Closed {
            end: StepEnd::Finish,
        },
        notice: None,
    };
    let client = peer.client.clone();
    let reading =
        tokio::spawn(async move { client.read_history_with_progress(&fixed, || {}).await });
    let mut full = page(expected.clone());
    let last = full.fragments.pop().unwrap();
    let split = last.fragment.json_fragment.find("原").unwrap();
    let next = HistoryCursor {
        anchor: expected.anchor.clone(),
        node: NodeId(2),
        utf8_offset: split as u64,
    };
    full.fragments.push(HistoryFragmentView {
        agent_name: last.agent_name.clone(),
        summary: view(id, 2, false, true)
            .selected_turn
            .unwrap()
            .summary
            .clone(),
        fragment: NodeFragment {
            node: NodeId(2),
            utf8_offset: 0,
            json_fragment: last.fragment.json_fragment[..split].into(),
            complete: false,
            agent_name: last.fragment.agent_name.clone(),
            summary: view(id, 2, false, true).selected_turn.unwrap().summary,
            retries: last.fragment.retries.clone(),
        },
    });
    full.next = Some(next.clone());
    let (mut wire, anchor) = body_request(&peer).await;
    assert_eq!(anchor, expected.anchor);
    wire.reply(Response::History(full)).await;
    let (mut wire, req) = peer.request().await;
    assert!(
        matches!(req, Request::History { anchor, cursor: Some(c), .. } if anchor == expected.anchor && c == next)
    );
    wire.reply(Response::History(HistoryPageView {
        anchor: expected.anchor.clone(),
        next: None,
        fragments: vec![HistoryFragmentView {
            agent_name: last.agent_name,
            summary: last.summary,
            fragment: NodeFragment {
                node: NodeId(2),
                utf8_offset: split as u64,
                json_fragment: last.fragment.json_fragment[split..].into(),
                complete: true,
                agent_name: last.fragment.agent_name,
                summary: last.fragment.summary,
                retries: last.fragment.retries,
            },
        }],
    }))
    .await;
    assert_eq!(within(reading).await.unwrap().unwrap(), expected);
}

#[tokio::test]
async fn summary_precedes_stalled_body_and_finished_requires_complete_history() {
    use tokio::io::AsyncReadExt;
    let peer = Peer::new();
    let id = new_turn_id();
    let mut turn = peer.client.observe(Some(sid()), Some(id));
    let (mut stream, _) = peer.request().await;
    stream.snapshot(view(id, 1, false, true)).await;
    state(&mut turn).await;
    let (mut old, _) = body_request(&peer).await;
    let final_view = view(id, 2, true, false);
    stream.snapshot(final_view.clone()).await;
    let update = update_until(&mut turn, |u| matches!(u, LocalUpdate::State { view, .. } if view.selected_turn.as_ref().unwrap().summary.progress.is_closed())).await;
    assert!(
        matches!(update, LocalUpdate::State { view, phase: ObservationPhase::NoOwner, .. } if *view == final_view)
    );
    assert!(matches!(
        update_until(&mut turn, |u| matches!(u, LocalUpdate::Reset { .. })).await,
        LocalUpdate::Reset { generation: 2, .. }
    ));
    assert_eq!(
        within(old.socket.read_u8()).await.unwrap_err().kind(),
        std::io::ErrorKind::UnexpectedEof
    );
    let (_stalled, _) = body_request(&peer).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(40), turn.next())
            .await
            .is_err()
    );
    stream.finished(&final_view).await;
    let update = within(turn.next()).await.unwrap();
    assert!(
        matches!(update, LocalUpdate::History { result: Ok(h), generation: 2, stale: false, .. } if *h == history(&final_view))
    );
    assert!(matches!(
        within(turn.next()).await,
        Some(LocalUpdate::Finished { generation: 2, .. })
    ));
    let report = within(turn.detach()).await;
    assert_eq!(report.state.unwrap().phase, ObservationPhase::Finished);
    assert!(report.history_error.is_none());
}

#[tokio::test]
async fn final_history_error_or_idle_preserves_summary_without_finished() {
    for idle in [false, true] {
        let peer = Peer::new();
        let id = new_turn_id();
        let fixed = view(id, 2, true, false);
        let mut turn = peer.client.observe(Some(sid()), Some(id));
        let (mut stream, _) = peer.request().await;
        stream.snapshot(fixed.clone()).await;
        state(&mut turn).await;
        update_until(&mut turn, |u| matches!(u, LocalUpdate::Reset { .. })).await;
        let (_stalled, _) = body_request(&peer).await;
        if idle {
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(crate::STREAM_IDLE_TIMEOUT_SECS + 1)).await;
        } else {
            stream
                .reply(Response::HistoryError(HistoryReadError {
                    context: HistoryReadContext::Page {
                        anchor: fixed.history.clone(),
                        cursor: None,
                    },
                    error: LocalError::new(LocalErrorKind::StoreUnavailable, "history unavailable"),
                }))
                .await;
        }
        let update = update_until(&mut turn, |u| {
            matches!(u, LocalUpdate::History { result: Err(_), .. })
        })
        .await;
        assert!(
            matches!(update, LocalUpdate::History { anchor, stale: false, .. } if anchor == fixed.history)
        );
        if idle {
            tokio::time::resume();
        }
        let report = within(turn.detach()).await;
        assert!(report.history_error.is_some());
        let state = report.state.unwrap();
        assert_eq!(*state.view, fixed);
        assert_ne!(state.phase, ObservationPhase::Finished);
    }
}

#[tokio::test]
async fn body_error_context_survives_reconnect_until_new_history_succeeds() {
    let peer = Peer::new();
    let id = new_turn_id();
    let old = view(id, 1, false, true);
    let mut turn = peer.client.observe(Some(sid()), Some(id));
    let (mut stream, _) = peer.request().await;
    stream.snapshot(old.clone()).await;
    state(&mut turn).await;
    let (mut wire, anchor) = body_request(&peer).await;
    let failure = LocalError::new(LocalErrorKind::StoreUnavailable, "old body unavailable");
    wire.reply(Response::Error(failure.clone())).await;
    let update = update_until(&mut turn, |u| {
        matches!(u, LocalUpdate::History { result: Err(_), .. })
    })
    .await;
    assert!(
        matches!(update, LocalUpdate::History { result: Err(HistoryReadError { context: HistoryReadContext::Page { anchor: a, cursor: None }, error }), .. } if a == anchor && error == failure)
    );
    turn.reconnect();
    let (mut fresh, req) = peer.request().await;
    assert!(matches!(req, Request::Subscribe { .. }));
    let fixed = view(id, 2, true, false);
    fresh.snapshot(fixed.clone()).await;
    state(&mut turn).await;
    let (_stalled, _) = body_request(&peer).await;
    fresh.finished(&fixed).await;
    update_until(&mut turn, |u| matches!(u, LocalUpdate::Finished { .. })).await;
    let report = within(turn.detach()).await;
    assert!(report.history_error.is_none());
    assert_eq!(*report.state.unwrap().view, fixed);
}

#[tokio::test]
async fn suffix_survives_history_and_reconnect_supplies_attempt_segments() {
    let peer = Peer::new();
    let id = new_turn_id();
    let fixed = view(id, 1, false, true);
    let mut turn = peer.client.observe(Some(sid()), Some(id));
    let (mut stream, _) = peer.request().await;
    stream.snapshot(fixed.clone()).await;
    update_until(&mut turn, |u| matches!(u, LocalUpdate::Reset { .. })).await;
    let (mut wire, _) = body_request(&peer).await;
    stream.delta(id, 2).await;
    assert!(
        matches!(within(turn.next()).await, Some(LocalUpdate::Event { event: crate::TurnEvent::TokenDelta { key: actual, .. }, .. }) if actual == key(id))
    );
    wire.reply(Response::History(page(history(&fixed)))).await;
    assert!(matches!(
        within(turn.next()).await,
        Some(LocalUpdate::History { result: Ok(_), .. })
    ));
    stream.delta(id, 3).await;
    assert!(matches!(
        within(turn.next()).await,
        Some(LocalUpdate::Event { .. })
    ));
    turn.reconnect();
    let (mut fresh, _) = peer.request().await;
    let mut next = fixed;
    next.cursor = cursor(3);
    next.attempt
        .as_mut()
        .unwrap()
        .segments
        .push(TextSegment::Reasoning {
            text: "reason".into(),
        });
    fresh.snapshot(next.clone()).await;
    let update = update_until(&mut turn, |u| matches!(u, LocalUpdate::Reset { .. })).await;
    assert!(matches!(update, LocalUpdate::Reset { view, generation: 2 } if *view == next));
    within(turn.detach()).await;
}

#[tokio::test]
async fn cancel_handoff_both_orders_shares_one_retry_and_true_never_retries() {
    for evidence_first in [false, true] {
        for first_receipt in [Some(false), Some(true), None] {
            let peer = Peer::new();
            let id = new_turn_id();
            let mut turn = peer.client.observe(None, Some(id));
            let (mut stream, _) = peer.request().await;
            turn.cancel();
            let mut first = cancel_request(&peer, id).await;
            if evidence_first {
                stream.snapshot(view(id, 1, false, true)).await;
                state(&mut turn).await;
            }
            let wanted = match first_receipt {
                Some(owned) => {
                    first.reply(Response::Cancelled { owned }).await;
                    if owned {
                        CancelStatus::Taken
                    } else {
                        CancelStatus::NotTaken
                    }
                }
                None => {
                    drop(first);
                    CancelStatus::Unknown
                }
            };
            cancel_status(&mut turn, wanted).await;
            if !evidence_first {
                stream.snapshot(view(id, 1, false, true)).await;
            }
            if first_receipt != Some(true) {
                let mut retry = cancel_request(&peer, id).await;
                retry.reply(Response::Cancelled { owned: false }).await;
                cancel_status(&mut turn, CancelStatus::NotTaken).await;
            }
            for _ in 0..3 {
                stream.snapshot(view(id, 1, false, true)).await;
                state(&mut turn).await;
            }
            no_cancel(&peer).await;
            let report = within(turn.detach()).await;
            assert_eq!(report.turn_id, Some(id));
            assert_eq!(
                report.cancel,
                Some(if first_receipt == Some(true) {
                    CancelStatus::Taken
                } else {
                    CancelStatus::NotTaken
                })
            );
        }
    }
}

#[tokio::test]
async fn new_intent_does_not_inherit_old_receipt_or_error() {
    for late_error in [false, true] {
        let peer = Peer::new();
        let id = new_turn_id();
        let mut turn = peer.client.observe(None, Some(id));
        let (mut stream, _) = peer.request().await;
        stream.snapshot(view(id, 1, false, true)).await;
        state(&mut turn).await;
        turn.cancel();
        let mut first = cancel_request(&peer, id).await;
        cancel_status(&mut turn, CancelStatus::Pending).await;
        if late_error {
            first
                .reply(Response::Error(LocalError::new(
                    LocalErrorKind::Permission,
                    "first denial",
                )))
                .await;
            cancel_status(&mut turn, CancelStatus::Unknown).await;
            first = cancel_request(&peer, id).await;
            let update = cancel_status(&mut turn, CancelStatus::Pending).await;
            assert!(matches!(update, LocalUpdate::Cancel { error: Some(_), .. }));
        }
        turn.cancel();
        assert!(matches!(
            cancel_status(&mut turn, CancelStatus::Pending).await,
            LocalUpdate::Cancel { error: None, .. }
        ));
        no_cancel(&peer).await;
        first
            .reply(if late_error {
                Response::Error(LocalError::new(
                    LocalErrorKind::Permission,
                    "late old denial",
                ))
            } else {
                Response::Cancelled { owned: true }
            })
            .await;
        let mut next = cancel_request(&peer, id).await;
        assert!(matches!(
            cancel_status(&mut turn, CancelStatus::Pending).await,
            LocalUpdate::Cancel { error: None, .. }
        ));
        next.reply(Response::Cancelled { owned: true }).await;
        cancel_status(&mut turn, CancelStatus::Taken).await;
        let report = within(turn.detach()).await;
        assert_eq!(report.cancel, Some(CancelStatus::Taken));
        assert_eq!(report.cancel_error, None);
    }
}

#[tokio::test]
async fn cancel_diagnostic_survives_detach_and_success_clears_it() {
    for success in [None, Some(false), Some(true)] {
        for before_detach in [false, true] {
            let peer = Peer::new();
            let id = new_turn_id();
            let mut turn = peer.client.observe(None, Some(id));
            let (mut stream, _) = peer.request().await;
            stream.snapshot(view(id, 1, false, true)).await;
            state(&mut turn).await;
            turn.cancel();
            let mut first = cancel_request(&peer, id).await;
            let error = LocalError::new(LocalErrorKind::Permission, "original cancel denied");
            first.reply(Response::Error(error.clone())).await;
            let mut retry = cancel_request(&peer, id).await;
            let response = success.map_or_else(
                || Response::Error(error.clone()),
                |owned| Response::Cancelled { owned },
            );
            let report = if before_detach {
                retry.reply(response).await;
                let wanted = success.map_or(CancelStatus::Unknown, |owned| {
                    if owned {
                        CancelStatus::Taken
                    } else {
                        CancelStatus::NotTaken
                    }
                });
                // 收到的第一条 Unknown 可能是首请求，因此以任务接管进展等待退出报告作最终断言。
                if success.is_some() {
                    cancel_status(&mut turn, wanted).await;
                }
                within(turn.detach()).await
            } else {
                let task = tokio::spawn(turn.detach());
                retry.reply(response).await;
                within(task).await.unwrap()
            };
            assert_eq!(report.turn_id, Some(id));
            assert_eq!(
                report.cancel_error,
                if success.is_some() { None } else { Some(error) }
            );
            assert_eq!(
                report.cancel,
                Some(success.map_or(CancelStatus::Unknown, |owned| if owned {
                    CancelStatus::Taken
                } else {
                    CancelStatus::NotTaken
                }))
            );
        }
    }
}

#[tokio::test]
async fn detach_timeout_preserves_unknown_identity_and_closes_cancel_socket() {
    use tokio::io::AsyncReadExt;
    let peer = Peer::new();
    let id = new_turn_id();
    let turn = peer.client.observe(None, Some(id));
    let (_stream, _) = peer.request().await;
    turn.cancel();
    let mut receipt = cancel_request(&peer, id).await;
    let start = tokio::time::Instant::now();
    let report = tokio::time::timeout(Duration::from_secs(6), turn.detach())
        .await
        .unwrap();
    assert!(start.elapsed() >= Duration::from_secs(5));
    assert_eq!(report.turn_id, Some(id));
    assert_eq!(report.cancel, Some(CancelStatus::Unknown));
    assert_eq!(
        within(receipt.socket.read_u8()).await.unwrap_err().kind(),
        std::io::ErrorKind::UnexpectedEof
    );
}

#[tokio::test]
async fn late_probe_does_not_erase_taken_or_load_configuration() {
    let peer = Peer::new();
    let id = new_turn_id();
    let original = request(&peer, id);
    let mut turn = peer.client.start_turn_with(original.clone(), |_| async {
        panic!("cancelled probe must not load")
    });
    let (mut probe, mut stream) = initial(&peer, &original).await;
    turn.cancel();
    let mut receipt = cancel_request(&peer, id).await;
    receipt.reply(Response::Cancelled { owned: true }).await;
    cancel_status(&mut turn, CancelStatus::Taken).await;
    stream.snapshot(view(id, 1, false, true)).await;
    state(&mut turn).await;
    probe.reply(needs_configuration()).await;
    assert!(matches!(
        update_until(&mut turn, |u| matches!(u, LocalUpdate::NotSubmitted { .. })).await,
        LocalUpdate::NotSubmitted {
            stage: NotSubmittedStage::NeedsConfiguration,
            ..
        }
    ));
    let report = within(turn.detach()).await;
    assert_eq!(report.cancel, Some(CancelStatus::Taken));
    assert_eq!(report.state.unwrap().phase, ObservationPhase::Live);
}

#[tokio::test]
async fn detach_handoffs_reverse_receipts_without_ui_or_body_progress() {
    let peer = Peer::new();
    let id = new_turn_id();
    let original = request(&peer, id);
    let turn = peer
        .client
        .start_turn_with(original.clone(), |_| async { panic!("shared Owner") });
    let (mut probe, mut stream) = initial(&peer, &original).await;
    turn.cancel();
    let mut first = cancel_request(&peer, id).await;
    stream.snapshot(view(id, 1, false, true)).await;
    let (_body, _) = body_request(&peer).await;
    for n in 2..=49 {
        stream.delta(id, n).await;
    }
    probe.reply(Response::Acceptance(accepted(id))).await;
    let detaching = tokio::spawn(turn.detach());
    first.reply(Response::Cancelled { owned: false }).await;
    let mut reads = Vec::new();
    let mut retry = loop {
        let (wire, req) = peer.request().await;
        match req {
            Request::Cancel { target } => {
                assert_eq!(target.turn_id, id);
                break wire;
            }
            Request::Subscribe { target, .. } => {
                assert_eq!(target.turn_id, id);
                reads.push(wire);
            }
            Request::History { .. } => reads.push(wire),
            _ => panic!("control handoff only"),
        }
        assert!(reads.len() <= 4);
    };
    retry.reply(Response::Cancelled { owned: true }).await;
    let report = within(detaching).await.unwrap();
    assert_eq!(report.submission, Some(Ok(accepted(id))));
    assert_eq!(report.cancel, Some(CancelStatus::Taken));
}

#[tokio::test]
async fn lazy_handshake_preserves_identity_and_loads_selected_agent_once() {
    for controlled in [false, true] {
        let peer = Peer::new();
        let id = new_turn_id();
        let original = request(&peer, id);
        let configuration = super::tests::frozen(peer._directory.path(), "http://127.0.0.1:9/v1");
        let loads = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = loads.clone();
        let client = peer.client.clone();
        let sent = original.clone();
        let task = tokio::spawn(async move {
            let load = move |name: String| async move {
                assert_eq!(name, "test");
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(configuration)
            };
            if controlled {
                let mut turn = client.start_turn_with(sent, load);
                update_until(&mut turn, |u| matches!(u, LocalUpdate::Submission { .. })).await;
                turn.detach().await.submission.unwrap()
            } else {
                match client.submit_handshake(sent, load, None).await? {
                    SubmissionReply::Accepted(value) => Ok(value),
                    _ => panic!("handshake must accept"),
                }
            }
        });
        let mut subscriptions = Vec::new();
        loop {
            let (mut wire, req) = peer.request().await;
            match req {
                Request::Subscribe { .. } => subscriptions.push(wire),
                Request::Submit {
                    request,
                    configuration,
                } => {
                    assert!(request == original);
                    if let Some(configuration) = configuration {
                        assert_eq!(
                            serde_json::to_value(configuration).unwrap()["system_prompt"],
                            "test"
                        );
                        wire.reply(Response::Acceptance(accepted(id))).await;
                        break;
                    }
                    assert_eq!(loads.load(std::sync::atomic::Ordering::SeqCst), 0);
                    wire.reply(needs_configuration()).await;
                }
                _ => panic!("handshake"),
            }
        }
        assert_eq!(within(task).await.unwrap(), Ok(accepted(id)));
        assert_eq!(loads.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn late_blocking_configuration_cannot_send_after_cancel_or_drop() {
    for cancel in [false, true] {
        let peer = Peer::new();
        let id = new_turn_id();
        let original = request(&peer, id);
        let config = super::tests::frozen(peer._directory.path(), "http://127.0.0.1:9/v1");
        let (arrived, ready) = tokio::sync::oneshot::channel();
        let (release, gate) = std::sync::mpsc::channel();
        let (done, finished) = tokio::sync::oneshot::channel();
        let mut turn = peer
            .client
            .start_turn_with(original.clone(), |_| async move {
                tokio::task::spawn_blocking(move || {
                    arrived.send(()).unwrap();
                    gate.recv_timeout(Duration::from_secs(3)).unwrap();
                    let _ = done.send(());
                    Ok(config)
                })
                .await
                .unwrap()
            });
        let (mut probe, _stream) = initial(&peer, &original).await;
        probe.reply(needs_configuration()).await;
        within(ready).await.unwrap();
        if cancel {
            turn.cancel();
            let mut receipt = cancel_request(&peer, id).await;
            receipt.reply(Response::Cancelled { owned: true }).await;
            cancel_status(&mut turn, CancelStatus::Taken).await;
            assert_eq!(
                within(turn.detach()).await.not_submitted,
                Some(NotSubmittedStage::FreezingConfiguration)
            );
        } else {
            drop(turn);
        }
        release.send(()).unwrap();
        within(finished).await.unwrap();
        no_cancel(&peer).await;
    }
}

#[tokio::test]
async fn local_configuration_failure_preserves_live_cancel_target() {
    for kind in [LocalErrorKind::BadRequest, LocalErrorKind::Transport] {
        let peer = Peer::new();
        let id = new_turn_id();
        let original = request(&peer, id);
        let error = LocalError::new(kind, "local configuration invalid");
        let expected = error.clone();
        let (release, gate) = tokio::sync::oneshot::channel();
        let mut turn = peer
            .client
            .start_turn_with(original.clone(), |_| async move {
                gate.await.unwrap();
                Err(error)
            });
        let (mut probe, mut stream) = initial(&peer, &original).await;
        probe.reply(needs_configuration()).await;
        stream.snapshot(view(id, 1, false, true)).await;
        state(&mut turn).await;
        release.send(()).unwrap();
        assert!(
            matches!(update_until(&mut turn, |u| matches!(u, LocalUpdate::Submission { .. })).await, LocalUpdate::Submission { result: Err(error), .. } if error == expected)
        );
        turn.cancel();
        let mut receipt = cancel_request(&peer, id).await;
        receipt.reply(Response::Cancelled { owned: true }).await;
        cancel_status(&mut turn, CancelStatus::Taken).await;
        let report = within(turn.detach()).await;
        assert_eq!(report.submission, Some(Err(expected)));
        assert_eq!(report.cancel, Some(CancelStatus::Taken));
        assert!(report.submission_diagnostic.is_none());
        assert_eq!(report.state.unwrap().phase, ObservationPhase::Live);
    }
}

#[tokio::test]
async fn explicit_backend_rejection_ends_cancel_handoff() {
    let peer = Peer::new();
    let id = new_turn_id();
    let original = request(&peer, id);
    let configuration = super::tests::frozen(peer._directory.path(), "http://127.0.0.1:9/v1");
    let mut turn = peer
        .client
        .start_turn_with(original.clone(), |_| async { Ok(configuration) });
    let (mut probe, _stream) = initial(&peer, &original).await;
    probe.reply(needs_configuration()).await;
    let (mut configured, req) = peer.request().await;
    assert!(matches!(
        req,
        Request::Submit {
            configuration: Some(_),
            ..
        }
    ));
    let error = LocalError::new(LocalErrorKind::BadRequest, "configured operation rejected");
    configured.reply(Response::Error(error.clone())).await;
    update_until(&mut turn, |u| matches!(u, LocalUpdate::Submission { .. })).await;
    turn.cancel();
    cancel_status(&mut turn, CancelStatus::Moot).await;
    let report = within(turn.detach()).await;
    assert_eq!(report.submission, Some(Err(error)));
    assert_eq!(report.cancel, Some(CancelStatus::Moot));
    no_cancel(&peer).await;
}

#[tokio::test]
async fn unknown_submission_keeps_late_transport_distinct_from_backend_errors() {
    for configured in [false, true] {
        for backend_kind in [
            None,
            Some(LocalErrorKind::BadRequest),
            Some(LocalErrorKind::Transport),
        ] {
            let peer = Peer::new();
            let id = new_turn_id();
            let original = request(&peer, id);
            let configuration =
                super::tests::frozen(peer._directory.path(), "http://127.0.0.1:9/v1");
            let mut turn = peer
                .client
                .start_turn_with(original.clone(), move |_| async move {
                    assert!(configured);
                    Ok(configuration)
                });
            let (mut submit, mut stream) = initial(&peer, &original).await;
            if configured {
                submit.reply(needs_configuration()).await;
                let (wire, req) = peer.request().await;
                assert!(
                    matches!(req, Request::Submit { request, configuration: Some(_) } if request == original)
                );
                submit = wire;
            }
            stream.snapshot(view(id, 1, false, true)).await;
            body(&peer, &view(id, 1, false, true)).await;
            update_until(&mut turn, |u| matches!(u, LocalUpdate::History { .. })).await;
            turn.cancel();
            let mut receipt = cancel_request(&peer, id).await;
            receipt.reply(Response::Cancelled { owned: true }).await;
            cancel_status(&mut turn, CancelStatus::Taken).await;
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(5)).await;
            update_until(&mut turn, |u| {
                matches!(u, LocalUpdate::SubmissionUnknown { error: None, .. })
            })
            .await;
            tokio::time::resume();
            let (mut fresh, req) = peer.request().await;
            assert!(
                matches!(req, Request::Subscribe { target, cursor: None } if target.turn_id == id)
            );
            let fixed = view(id, if configured { 2 } else { 1 }, configured, !configured);
            fresh.snapshot(fixed.clone()).await;
            state(&mut turn).await;
            let (_stalled, _) = body_request(&peer).await;
            if configured {
                fresh.finished(&fixed).await;
                update_until(&mut turn, |u| matches!(u, LocalUpdate::Finished { .. })).await;
            }
            let error = backend_kind.map(|kind| LocalError::new(kind, "backend submit error"));
            if let Some(error) = &error {
                submit.reply(Response::Error(error.clone())).await;
            }
            drop(submit);
            let update = update_until(&mut turn, |u| {
                matches!(
                    u,
                    LocalUpdate::Submission { .. }
                        | LocalUpdate::SubmissionUnknown { error: Some(_), .. }
                )
            })
            .await;
            match (&update, &error) {
                (
                    LocalUpdate::SubmissionUnknown {
                        turn_id,
                        error: Some(e),
                    },
                    None,
                ) => {
                    assert_eq!(*turn_id, id);
                    assert_eq!(e.kind, LocalErrorKind::Transport);
                }
                (
                    LocalUpdate::Submission {
                        result: Err(actual),
                        ..
                    },
                    Some(expected),
                ) => assert_eq!(actual, expected),
                _ => panic!("source classification: {update:?}"),
            }
            if !configured && error.is_none() {
                fresh.delta(id, 2).await;
                assert!(matches!(
                    within(turn.next()).await,
                    Some(LocalUpdate::Event { .. })
                ));
            }
            let report = within(turn.detach()).await;
            assert_eq!(report.submission, error.clone().map(Err));
            assert_eq!(report.cancel, Some(CancelStatus::Taken));
            assert_eq!(*report.state.unwrap().view, fixed);
            assert_eq!(report.submission_diagnostic.is_some(), error.is_none());
            no_cancel(&peer).await;
        }
    }
}

#[tokio::test]
async fn unclosed_no_owner_delivers_diagnostic_and_history_then_finishes_without_cancel() {
    for failed in [false, true] {
        let peer = Peer::new();
        let id = new_turn_id();
        let fixed = view(id, 1, false, false);
        let diagnostic =
            failed.then(|| LocalError::new(LocalErrorKind::Internal, "close write failed"));
        let mut turn = peer.client.observe(Some(sid()), Some(id));
        let (mut stream, _) = peer.request().await;
        stream.snapshot(fixed.clone()).await;
        let update = update_until(&mut turn, |u| matches!(u, LocalUpdate::State { .. })).await;
        assert!(
            matches!(update, LocalUpdate::State { view, phase: ObservationPhase::NoOwner, .. }
            if *view == fixed && !view.selected_turn.as_ref().unwrap().summary.progress.is_closed())
        );
        // 历史尚未装入也已知道执行者消失，取消不能再发给后台。
        turn.cancel();
        cancel_status(&mut turn, CancelStatus::Moot).await;
        body(&peer, &fixed).await;
        if let Some(error) = &diagnostic {
            stream
                .reply(Response::Observation(Observation::Diagnostic {
                    error: error.clone(),
                }))
                .await;
            let update = update_until(&mut turn, |u| {
                matches!(u, LocalUpdate::State { error: Some(_), .. })
            })
            .await;
            assert!(matches!(update, LocalUpdate::State { error, .. } if error == diagnostic));
        }
        stream.finished(&fixed).await;
        let update = update_until(&mut turn, |u| {
            matches!(u, LocalUpdate::History { result: Ok(_), .. })
        })
        .await;
        assert!(
            matches!(update, LocalUpdate::History { result: Ok(h), .. } if *h == history(&fixed))
        );
        update_until(&mut turn, |u| matches!(u, LocalUpdate::Finished { .. })).await;
        let report = within(turn.detach()).await;
        let state = report.state.unwrap();
        assert_eq!(*state.view, fixed);
        assert_eq!(state.phase, ObservationPhase::Finished);
        assert_eq!(state.error, diagnostic);
        no_cancel(&peer).await;
    }
}

#[tokio::test]
async fn unknown_submission_late_acceptance_and_missing_target_do_not_end_wait_early() {
    for initially_owned in [false, true] {
        let peer = Peer::new();
        let id = new_turn_id();
        let original = request(&peer, id);
        let mut turn = peer
            .client
            .start_turn_with(original.clone(), |_| async { panic!("existing request") });
        let (mut probe, mut stream) = initial(&peer, &original).await;
        if initially_owned {
            stream.snapshot(view(id, 1, false, true)).await;
            body(&peer, &view(id, 1, false, true)).await;
            update_until(&mut turn, |u| matches!(u, LocalUpdate::History { .. })).await;
        } else {
            // 尚未接受的身份可以稍后得到 Owner；已经接受但失去 Owner 的 Turn 不会复活。
            stream
                .reply(Response::Error(LocalError::new(
                    LocalErrorKind::NotFound,
                    "not accepted yet",
                )))
                .await;
        }
        turn.cancel();
        let mut receipt = cancel_request(&peer, id).await;
        receipt
            .reply(Response::Cancelled {
                owned: initially_owned,
            })
            .await;
        cancel_status(
            &mut turn,
            if initially_owned {
                CancelStatus::Taken
            } else {
                CancelStatus::NotTaken
            },
        )
        .await;
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(4)).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(10), turn.next())
                .await
                .is_err()
        );
        tokio::time::advance(Duration::from_secs(1)).await;
        update_until(&mut turn, |u| {
            matches!(u, LocalUpdate::SubmissionUnknown { .. })
        })
        .await;
        tokio::time::resume();
        let (mut fresh, req) = peer.request().await;
        assert!(matches!(req, Request::Subscribe { target, .. } if target.turn_id == id));
        fresh.snapshot(view(id, 1, false, true)).await;
        state(&mut turn).await;
        if !initially_owned {
            let mut retry = cancel_request(&peer, id).await;
            retry.reply(Response::Cancelled { owned: true }).await;
            cancel_status(&mut turn, CancelStatus::Taken).await;
        }
        probe.reply(Response::Acceptance(accepted(id))).await;
        update_until(&mut turn, |u| matches!(u, LocalUpdate::Submission { .. })).await;
        let report = within(turn.detach()).await;
        assert_eq!(report.submission, Some(Ok(accepted(id))));
        assert_eq!(report.cancel, Some(CancelStatus::Taken));
        assert_eq!(report.session_id, Some(sid()));
    }
}

#[tokio::test]
async fn observe_selects_backend_target_and_empty_root_finishes_without_subscribe() {
    for selected in [None, Some(new_turn_id())] {
        let peer = Peer::new();
        let mut fixed = view(selected.unwrap_or_else(new_turn_id), 0, true, false);
        if selected.is_none() {
            fixed.selected_turn = None;
            fixed.current_agent = None;
        }
        let mut turn = peer.client.observe(Some(sid()), None);
        let (mut wire, req) = peer.request().await;
        assert!(matches!(req, Request::Query { turn_id: None, .. }));
        wire.reply(Response::View(Box::new(fixed.clone()))).await;
        if let Some(id) = selected {
            let (_wire, req) = peer.request().await;
            assert!(matches!(req, Request::Subscribe { target, .. } if target.turn_id == id));
        } else {
            body(&peer, &fixed).await;
            let update =
                update_until(&mut turn, |u| matches!(u, LocalUpdate::History { .. })).await;
            assert!(
                matches!(update, LocalUpdate::History { result: Ok(h), .. } if h.entries.is_empty() && h.anchor == fixed.history)
            );
            assert!(matches!(
                within(turn.next()).await,
                Some(LocalUpdate::Finished { turn: None, .. })
            ));
            no_cancel(&peer).await;
        }
        within(turn.detach()).await;
    }
}

#[tokio::test]
async fn reconnect_after_idle_gets_a_fresh_window() {
    let peer = Peer::new();
    let id = new_turn_id();
    let fixed = view(id, 1, false, true);
    let mut turn = peer.client.observe(Some(sid()), Some(id));
    let (mut wire, _) = peer.request().await;
    wire.snapshot(fixed.clone()).await;
    body(&peer, &fixed).await;
    update_until(&mut turn, |u| matches!(u, LocalUpdate::History { .. })).await;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(crate::STREAM_IDLE_TIMEOUT_SECS + 1)).await;
    update_until(&mut turn, |u| {
        matches!(u, LocalUpdate::Connection(ConnectionStatus::Idle))
    })
    .await;
    tokio::time::resume();
    turn.reconnect();
    let (mut fresh, _) = peer.request().await;
    fresh.snapshot(fixed).await;
    assert!(matches!(
        update_until(&mut turn, |u| matches!(u, LocalUpdate::State { .. })).await,
        LocalUpdate::State {
            phase: ObservationPhase::Live,
            ..
        }
    ));
    within(turn.detach()).await;
}

// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::{ipc, types::*, *};
use std::time::Duration;
use tokio::net::{UnixListener, UnixStream};

struct Peer {
    _directory: tempfile::TempDir,
    _instance: ipc::Instance,
    listener: UnixListener,
    client: LocalClient,
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

    async fn request(&self) -> (UnixStream, Request) {
        within(async {
            loop {
                let (mut stream, _) = self.listener.accept().await.unwrap();
                match ipc::read(&mut stream).await {
                    Ok(request) => return (stream, request),
                    // 旧正文可能在 connect 之后、首帧写入之前被退场。
                    Err(error) if error.kind == LocalErrorKind::Transport => continue,
                    Err(error) => panic!("IPC request: {error}"),
                }
            }
        })
        .await
    }
}

/// 负数窗口：旧在途取消回执未结束前不得并发发送新的取消请求。
/// 同一固定切点的正文连接按请求类型回空页，只有 Cancel 失败，不用计时代替请求分类。
async fn assert_no_concurrent_cancel(peer: &Peer) {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(200);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return;
        }
        let Ok(Ok((mut stream, _))) = tokio::time::timeout(remaining, peer.listener.accept()).await
        else {
            return;
        };
        match ipc::read(&mut stream).await {
            Ok(Request::Transcript { .. }) => {
                // 与 cancel_request 一致：已被取代的正文连接可以拒绝响应。
                let _ = ipc::write(
                    &mut stream,
                    &Response::Transcript(TranscriptPage {
                        jsonl: String::new(),
                        next_offset: None,
                    }),
                )
                .await;
            }
            Ok(Request::Cancel { .. }) => {
                panic!("旧在途回执未结束前不得并发发送新的取消请求")
            }
            Ok(_) => panic!("取消意图注册期间出现意外请求"),
            Err(error) if error.kind == LocalErrorKind::Transport => continue,
            Err(error) => panic!("IPC request: {error}"),
        }
    }
}

async fn within<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(2), future)
        .await
        .expect("IPC progress")
}

fn view(id: TurnId, object: &str, committed: bool) -> SessionObservation {
    let pending = PendingObservation {
        turn_id: id,
        journal_oid: object.into(),
    };
    SessionObservation {
        state_oid: "fixed-state".into(),
        session: crate::GraphSession {
            session_id: "0123456789abcdef0123456789abcdef".into(),
            label: None,
            active_agent_name: "pending-agent".into(),
            created_at: String::new(),
            updated_at: String::new(),
            project_target_branch: Some("main".into()),
            project_store_key: "key".into(),
        },
        head: crate::GraphHead::Branch {
            name: "main".into(),
        },
        head_commit_id: committed.then(|| object.into()),
        branches: vec![],
        commit_count: usize::from(committed),
        active_turn: (!committed).then(|| pending.clone()),
        requested_turn: Some(if committed {
            TurnObservation::Committed(Box::new(commit(id, object)))
        } else {
            TurnObservation::Pending(pending)
        }),
    }
}

fn commit(id: TurnId, object: &str) -> CommittedTurnObservation {
    CommittedTurnObservation {
        commit_id: object.into(),
        turn_id: id,
        parent_ids: vec![],
        agent_name: "saved-agent".into(),
        model: "test".into(),
        provider: "test".into(),
        outcome: crate::TurnOutcome::Completed {
            finish_reason: lattice_session::TurnFinishReason::Stop,
        },
        origin: crate::ClosureOrigin::Live,
        usage: None,
        workspace_history_checkpoint: crate::WorkspaceHistoryCheckpoint::Empty,
        created_at: String::new(),
    }
}

async fn reply(stream: &mut UnixStream, response: Response) {
    within(ipc::write(stream, &response)).await.unwrap();
}

async fn transcript(peer: &Peer, object: &str, text: &str) {
    let (mut stream, request) = peer.request().await;
    assert!(
        matches!(request, Request::Transcript { commit_id, offset: 0, .. } if commit_id == object)
    );
    let message = NativeMessage::<BoundToolCall>::new(
        MessageRole::User,
        None,
        vec![lattice_conversation::MessagePart::semantic_only(
            MessageSemantic::Text { text: text.into() },
        )],
    );
    reply(
        &mut stream,
        Response::Transcript(TranscriptPage {
            jsonl: serde_json::to_string(&message).unwrap() + "\n",
            next_offset: None,
        }),
    )
    .await;
}

#[tokio::test]
async fn o3_fixed_session_merge_pending_and_selected_pin() {
    let peer = Peer::new();
    let id = new_turn_id();
    let mut fixed = view(id, "old-journal", false);
    fixed.head_commit_id = Some("merge".into());
    let expected = fixed.clone();
    let client = peer.client.clone();
    let read =
        tokio::spawn(async move { client.read_history(&fixed, HistoryScope::Session).await });
    let (mut stream, request) = peer.request().await;
    assert!(
        matches!(request, Request::History { head, offset: 0, limit: 128, .. } if head == "merge")
    );
    reply(
        &mut stream,
        Response::History(vec!["base".into(), "side".into(), "merge".into()]),
    )
    .await;
    for object in ["base", "side", "merge"] {
        let (mut stream, request) = peer.request().await;
        assert!(matches!(request, Request::Commit { commit_id, .. } if commit_id == object));
        let mut saved = commit(new_turn_id(), object);
        if object == "merge" {
            saved.parent_ids = vec!["base".into(), "side".into()];
        }
        reply(&mut stream, Response::Commit(Box::new(saved))).await;
        transcript(&peer, object, object).await;
    }
    let (mut stream, request) = peer.request().await;
    assert!(matches!(request, Request::History { head, offset: 3, .. } if head == "merge"));
    reply(&mut stream, Response::History(vec![])).await;
    // 对端的 head/pending 此时可以已移动；只允许旧切点的 journal，不提供二次 query。
    transcript(&peer, "old-journal", "pending").await;
    let history = within(read).await.unwrap().unwrap();
    assert_eq!(history.view, expected);
    assert_eq!(history.entries.len(), 4);
    let TurnObservation::Committed(merge) = &history.entries[2].turn else {
        panic!("commit")
    };
    assert_eq!(merge.parent_ids, ["base", "side"]);
    assert_eq!(merge.commit_id, "merge");
    assert_eq!(history.entries[3].agent_name, "pending-agent");
    assert_eq!(history.entries[3].turn, expected.requested_turn.unwrap());

    let mut pin = view(id, "retained-pin", true);
    pin.head_commit_id = Some("different-head-after-reset".into());
    let client = peer.client.clone();
    let read =
        tokio::spawn(async move { client.read_history(&pin, HistoryScope::SelectedTurn).await });
    transcript(&peer, "retained-pin", "saved").await;
    let history = within(read).await.unwrap().unwrap();
    assert_eq!(history.entries.len(), 1);
    assert_eq!(
        history.entries[0].turn,
        TurnObservation::Committed(Box::new(commit(id, "retained-pin")))
    );
}

fn cursor(sequence: u64) -> EventCursor {
    EventCursor {
        epoch: "peer".into(),
        sequence,
    }
}

async fn snapshot(stream: &mut UnixStream, view: SessionObservation, owned: bool) {
    reply(
        stream,
        Response::Observation(Observation::Snapshot {
            view: Box::new(view),
            cursor: cursor(0),
            owned,
        }),
    )
    .await;
}

#[tokio::test]
async fn o1_finished_precedes_stalled_body_and_retires_old_connection() {
    use tokio::io::AsyncReadExt;
    let peer = Peer::new();
    let id = new_turn_id();
    let pending = view(id, "pending", false);
    let mut turn = peer.client.observe(
        pending.session.session_id.clone(),
        Some(id),
        HistoryScope::SelectedTurn,
    );
    let (mut subscription, request) = peer.request().await;
    assert!(matches!(request, Request::Subscribe { turn_id, .. } if turn_id == id));
    snapshot(&mut subscription, pending, true).await;
    assert!(matches!(
        within(turn.next()).await,
        Some(LocalUpdate::State {
            phase: ObservationPhase::Live,
            ..
        })
    ));
    assert!(matches!(
        within(turn.next()).await,
        Some(LocalUpdate::Reset { .. })
    ));
    let (mut old_body, request) = peer.request().await;
    assert!(matches!(request, Request::Transcript { commit_id, .. } if commit_id == "pending"));
    reply(
        &mut subscription,
        Response::Observation(Observation::Finished {
            view: Box::new(view(id, "final", true)),
            cursor: cursor(1),
            error: None,
        }),
    )
    .await;
    let Some(LocalUpdate::State {
        view: fixed,
        phase,
        confirmation,
        ..
    }) = within(turn.next()).await
    else {
        panic!("State before body")
    };
    assert_eq!(phase, ObservationPhase::Finished);
    assert_eq!(confirmation, CompletionConfirmation::Confirmed);
    assert_eq!(fixed.requested_turn, view(id, "final", true).requested_turn);
    assert!(matches!(
        within(turn.next()).await,
        Some(LocalUpdate::Reset { .. })
    ));
    assert_eq!(
        within(old_body.read_u8()).await.unwrap_err().kind(),
        std::io::ErrorKind::UnexpectedEof
    );
    let (_final_body, request) = peer.request().await;
    assert!(matches!(request, Request::Transcript { commit_id, .. } if commit_id == "final"));
    // 最终正文也不返回，detach 仍交付固定终态，不等正文或 UI。
    let report = within(turn.detach()).await;
    assert_eq!(
        report.state.unwrap().confirmation,
        CompletionConfirmation::Confirmed
    );
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

async fn cancel_request(peer: &Peer, id: TurnId) -> UnixStream {
    loop {
        let (mut stream, request) = peer.request().await;
        match request {
            Request::Cancel { turn_id, .. } => {
                assert_eq!(turn_id, id);
                return stream;
            }
            Request::Transcript { .. } => {
                // 新 Snapshot 会退场旧正文连接，已被取代的请求可以拒绝响应。
                let _ = ipc::write(
                    &mut stream,
                    &Response::Transcript(TranscriptPage {
                        jsonl: String::new(),
                        next_offset: None,
                    }),
                )
                .await;
            }
            _ => panic!("unexpected request while cancelling"),
        }
    }
}

#[tokio::test]
async fn cancelled_submission_receipt_wait_refixes_and_detach_does_not_wait_again() {
    use tokio::io::AsyncReadExt;
    let peer = Peer::new();
    let id = new_turn_id();
    let pending = view(id, "before-handoff", false);
    let request = TurnRequest {
        session_id: pending.session.session_id.clone(),
        turn_id: id,
        workspace: peer._directory.path().into(),
        input: "original".into(),
    };
    let mut turn =
        peer.client
            .start_turn_with(request.clone(), HistoryScope::SelectedTurn, || async {
                panic!("cancelled probe must not load configuration")
            });
    let mut probe = None;
    let mut subscription = None;
    for _ in 0..2 {
        let (stream, actual) = peer.request().await;
        match actual {
            Request::Submit {
                request: actual,
                configuration: None,
            } => {
                assert!(actual == request);
                probe = Some(stream);
            }
            Request::Subscribe {
                session_id,
                turn_id,
                cursor: None,
            } => {
                assert_eq!(session_id, request.session_id);
                assert_eq!(turn_id, id);
                subscription = Some(stream);
            }
            _ => panic!("only original probe and read-only observation"),
        }
    }
    snapshot(subscription.as_mut().unwrap(), pending, false).await;
    transcript(&peer, "before-handoff", "saved before handoff").await;
    update_until(&mut turn, |u| matches!(u, LocalUpdate::History { .. })).await;
    let start = tokio::time::Instant::now();
    turn.cancel();
    let mut cancel = cancel_request(&peer, id).await;
    reply(&mut cancel, Response::Cancelled { owned: false }).await;
    update_until(&mut turn, |u| {
        matches!(
            u,
            LocalUpdate::Cancel {
                status: CancelStatus::NotTaken,
                ..
            }
        )
    })
    .await;
    tokio::time::sleep(Duration::from_secs(4)).await;
    let (mut refixed, actual) = peer.request().await;
    assert!(
        start.elapsed() >= Duration::from_secs(5),
        "false/NoOwner cannot end receipt waiting early"
    );
    assert!(
        matches!(actual, Request::Subscribe { session_id, turn_id, cursor: None } if session_id == request.session_id && turn_id == id)
    );
    assert!(matches!(
        update_until(&mut turn, |u| matches!(u, LocalUpdate::SubmissionUnknown { .. })).await,
        LocalUpdate::SubmissionUnknown { turn_id, error: None } if turn_id == id
    ));
    snapshot(&mut refixed, view(id, "after-handoff", false), false).await;
    transcript(
        &peer,
        "after-handoff",
        "still pending, not proof of no execution",
    )
    .await;
    update_until(&mut turn, |u| matches!(u, LocalUpdate::History { .. })).await;
    let report = within(turn.detach()).await;
    assert_eq!(report.turn_id, Some(id));
    assert_eq!(report.submission, None);
    assert_eq!(report.not_submitted, None);
    assert_eq!(report.cancel, Some(CancelStatus::NotTaken));
    assert_eq!(
        report.state.unwrap().view,
        Box::new(view(id, "after-handoff", false))
    );
    assert_eq!(
        within(probe.as_mut().unwrap().read(&mut [0]))
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn submission_timeout_keeps_live_observation_and_late_acceptance_cancel_identity() {
    for live_before_timeout in [true, false] {
        let peer = Peer::new();
        let id = new_turn_id();
        let pending = view(id, "stalled-initial", false);
        let original = TurnRequest {
            session_id: pending.session.session_id.clone(),
            turn_id: id,
            workspace: peer._directory.path().into(),
            input: "same input".into(),
        };
        let mut turn =
            peer.client
                .start_turn_with(original.clone(), HistoryScope::SelectedTurn, || async {
                    panic!("late probe must not load or resubmit configuration")
                });
        let mut probe = None;
        let mut subscription = None;
        for _ in 0..2 {
            let (stream, request) = peer.request().await;
            match request {
                Request::Submit {
                    request,
                    configuration: None,
                } => {
                    assert!(request == original);
                    probe = Some(stream);
                }
                Request::Subscribe {
                    session_id,
                    turn_id,
                    cursor: None,
                } => {
                    assert_eq!(session_id, original.session_id);
                    assert_eq!(turn_id, id);
                    subscription = Some(stream);
                }
                _ => panic!("only original probe and read-only subscription"),
            }
        }
        snapshot(subscription.as_mut().unwrap(), pending, live_before_timeout).await;
        let (_stalled_body, request) = peer.request().await;
        assert!(matches!(request, Request::Transcript { .. }));
        update_until(&mut turn, |u| matches!(u, LocalUpdate::State { .. })).await;
        turn.cancel();
        let mut first = cancel_request(&peer, id).await;
        reply(
            &mut first,
            Response::Cancelled {
                owned: live_before_timeout,
            },
        )
        .await;
        update_until(&mut turn, |u| {
            matches!(
                u,
                LocalUpdate::Cancel {
                    status: CancelStatus::Taken | CancelStatus::NotTaken,
                    ..
                }
            )
        })
        .await;
        tokio::time::sleep(Duration::from_secs(4)).await;
        let (mut refixed, request) = peer.request().await;
        assert!(
            matches!(request, Request::Subscribe { session_id, turn_id, cursor: None } if session_id == original.session_id && turn_id == id)
        );
        assert!(matches!(
            update_until(&mut turn, |u| matches!(u, LocalUpdate::SubmissionUnknown { .. })).await,
            LocalUpdate::SubmissionUnknown { turn_id, error: None } if turn_id == id
        ));
        snapshot(&mut refixed, view(id, "live-after-timeout", false), true).await;
        update_until(&mut turn, |u| {
            matches!(
                u,
                LocalUpdate::State {
                    phase: ObservationPhase::Live,
                    ..
                }
            )
        })
        .await;
        let retry = if live_before_timeout {
            None
        } else {
            Some(cancel_request(&peer, id).await)
        };
        reply(
            probe.as_mut().unwrap(),
            Response::Acceptance(Acceptance::Accepted { turn_id: id }),
        )
        .await;
        assert!(matches!(
            update_until(&mut turn, |u| matches!(u, LocalUpdate::Submission { .. })).await,
            LocalUpdate::Submission { result: Ok(Acceptance::Accepted { turn_id }), .. } if turn_id == id
        ));
        let mut accepted_observation = loop {
            let (mut stream, request) = peer.request().await;
            match request {
                Request::Subscribe {
                    session_id,
                    turn_id,
                    cursor: None,
                } => {
                    assert_eq!(session_id, original.session_id);
                    assert_eq!(turn_id, id);
                    break stream;
                }
                Request::Transcript { .. } => {
                    let _ = ipc::write(
                        &mut stream,
                        &Response::Transcript(TranscriptPage {
                            jsonl: String::new(),
                            next_offset: None,
                        }),
                    )
                    .await;
                }
                _ => panic!(
                    "late acceptance cannot resend Submit or consume another Cancel allowance"
                ),
            }
        };
        snapshot(
            &mut accepted_observation,
            view(id, "accepted-live", false),
            true,
        )
        .await;
        if let Some(mut retry) = retry {
            reply(&mut retry, Response::Cancelled { owned: true }).await;
            update_until(&mut turn, |u| {
                matches!(
                    u,
                    LocalUpdate::Cancel {
                        status: CancelStatus::Taken,
                        ..
                    }
                )
            })
            .await;
        }
        reply(
            &mut accepted_observation,
            Response::Observation(Observation::Event {
                cursor: cursor(1),
                event: crate::TurnEvent::TokenDelta {
                    content: "still observing".into(),
                },
            }),
        )
        .await;
        assert!(matches!(
            update_until(&mut turn, |u| matches!(u, LocalUpdate::Event { .. })).await,
            LocalUpdate::Event { event: crate::TurnEvent::TokenDelta { content }, .. } if content == "still observing"
        ));
        let report = within(turn.detach()).await;
        assert_eq!(report.turn_id, Some(id));
        assert_eq!(
            report.submission,
            Some(Ok(Acceptance::Accepted { turn_id: id }))
        );
        assert_eq!(report.cancel, Some(CancelStatus::Taken));
        assert_eq!(report.state.unwrap().phase, ObservationPhase::Live);
        // 只允许已发出的固定正文请求；true/晚到接受/重复 live 均不再补发取消。
        let extras = tokio::time::timeout(Duration::from_millis(50), async {
            loop {
                let (_, request) = peer.request().await;
                assert!(matches!(request, Request::Transcript { .. }));
            }
        })
        .await;
        assert!(extras.is_err());
    }
}

#[tokio::test]
async fn l1_cancel_handoff_both_orders_shares_one_retry_and_true_never_retries() {
    for evidence_first in [false, true] {
        for first_receipt in [Some(false), Some(true), None] {
            let taken = first_receipt == Some(true);
            let peer = Peer::new();
            let id = new_turn_id();
            let pending = view(id, "pending", false);
            let mut turn = peer.client.observe(
                pending.session.session_id.clone(),
                Some(id),
                HistoryScope::SelectedTurn,
            );
            let (mut subscription, request) = peer.request().await;
            assert!(matches!(request, Request::Subscribe { .. }));
            turn.cancel();
            let mut first = cancel_request(&peer, id).await;
            if evidence_first {
                snapshot(&mut subscription, pending.clone(), true).await;
                update_until(&mut turn, |u| matches!(u, LocalUpdate::State { .. })).await;
            }
            let status = match first_receipt {
                Some(taken) => {
                    reply(&mut first, Response::Cancelled { owned: taken }).await;
                    if taken {
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
            update_until(&mut turn, |u| matches!(u, LocalUpdate::Cancel { status: received, .. } if *received == status)).await;
            if !evidence_first {
                snapshot(&mut subscription, pending.clone(), true).await;
            }
            if !taken {
                let mut retry = cancel_request(&peer, id).await;
                reply(&mut retry, Response::Cancelled { owned: false }).await;
                update_until(&mut turn, |u| {
                    matches!(
                        u,
                        LocalUpdate::Cancel {
                            status: CancelStatus::NotTaken,
                            ..
                        }
                    )
                })
                .await;
            }
            // 重复 owned 证据不得重新武装；失败的补发也不循环。
            for _ in 0..3 {
                snapshot(&mut subscription, pending.clone(), true).await;
                update_until(&mut turn, |u| matches!(u, LocalUpdate::State { .. })).await;
            }
            let extras = tokio::time::timeout(Duration::from_millis(50), async {
                loop {
                    let (mut stream, request) = peer.request().await;
                    assert!(
                        !matches!(request, Request::Cancel { .. }),
                        "no third Cancel / no retry after true"
                    );
                    let _ = ipc::write(
                        &mut stream,
                        &Response::Transcript(TranscriptPage {
                            jsonl: String::new(),
                            next_offset: None,
                        }),
                    )
                    .await;
                }
            })
            .await;
            assert!(extras.is_err());
            let report = within(turn.detach()).await;
            assert_eq!(
                report.cancel,
                Some(if taken {
                    CancelStatus::Taken
                } else {
                    CancelStatus::NotTaken
                })
            );
        }
    }
}

#[tokio::test]
async fn l3_late_probe_does_not_erase_taken_or_load_configuration() {
    let peer = Peer::new();
    let id = new_turn_id();
    let pending = view(id, "pending", false);
    let request = TurnRequest {
        session_id: pending.session.session_id.clone(),
        turn_id: id,
        workspace: peer._directory.path().into(),
        input: "original".into(),
    };
    let mut turn = peer
        .client
        .start_turn_with(request, HistoryScope::SelectedTurn, || async {
            panic!("cancelled probe must not freeze configuration")
        });
    let mut probe = None;
    let mut subscription = None;
    for _ in 0..2 {
        let (stream, request) = peer.request().await;
        match request {
            Request::Submit {
                configuration: None,
                ..
            } => probe = Some(stream),
            Request::Subscribe { .. } => subscription = Some(stream),
            _ => panic!("initial operations"),
        }
    }
    turn.cancel();
    let mut cancellation = cancel_request(&peer, id).await;
    reply(&mut cancellation, Response::Cancelled { owned: true }).await;
    update_until(&mut turn, |u| {
        matches!(
            u,
            LocalUpdate::Cancel {
                status: CancelStatus::Taken,
                ..
            }
        )
    })
    .await;
    snapshot(subscription.as_mut().unwrap(), pending.clone(), true).await;
    update_until(&mut turn, |u| {
        matches!(
            u,
            LocalUpdate::State {
                phase: ObservationPhase::Live,
                ..
            }
        )
    })
    .await;
    reply(
        probe.as_mut().unwrap(),
        Response::Acceptance(Acceptance::NeedsConfiguration),
    )
    .await;
    assert!(
        matches!(update_until(&mut turn, |u| matches!(u, LocalUpdate::NotSubmitted { .. })).await,
        LocalUpdate::NotSubmitted { turn_id, stage: NotSubmittedStage::NeedsConfiguration } if turn_id == id)
    );
    let report = within(turn.detach()).await;
    assert_eq!(report.turn_id, Some(id));
    assert_eq!(report.cancel, Some(CancelStatus::Taken));
    assert_eq!(report.state.unwrap().phase, ObservationPhase::Live);
}

#[tokio::test]
async fn l2_detach_handoffs_reverse_receipts_without_ui_or_body_progress() {
    let peer = Peer::new();
    let id = new_turn_id();
    let pending = view(id, "pending", false);
    let request = TurnRequest {
        session_id: pending.session.session_id.clone(),
        turn_id: id,
        workspace: peer._directory.path().into(),
        input: "original".into(),
    };
    let turn = peer
        .client
        .start_turn_with(request, HistoryScope::SelectedTurn, || async {
            panic!("shared Owner needs no config")
        });
    let mut probe = None;
    let mut subscription = None;
    for _ in 0..2 {
        let (stream, request) = peer.request().await;
        match request {
            Request::Submit {
                configuration: None,
                ..
            } => probe = Some(stream),
            Request::Subscribe { .. } => subscription = Some(stream),
            _ => panic!("initial operations"),
        }
    }
    turn.cancel();
    let mut first = cancel_request(&peer, id).await;
    snapshot(subscription.as_mut().unwrap(), pending, true).await;
    let (_body, request) = peer.request().await;
    assert!(matches!(request, Request::Transcript { .. }));
    for sequence in 1..=48 {
        reply(
            subscription.as_mut().unwrap(),
            Response::Observation(Observation::Event {
                cursor: cursor(sequence),
                event: crate::TurnEvent::TokenDelta {
                    content: "live".into(),
                },
            }),
        )
        .await;
    }
    reply(
        probe.as_mut().unwrap(),
        Response::Acceptance(Acceptance::Accepted { turn_id: id }),
    )
    .await;
    let detaching = tokio::spawn(turn.detach());
    reply(&mut first, Response::Cancelled { owned: false }).await;
    // 接受之后客户端可已发起只读重固定；它和正文一样不应阻塞退出取消交接。
    let mut reads = Vec::new();
    let mut retry = loop {
        assert!(reads.len() <= 4, "bounded in-flight reads during detach");
        let (stream, request) = peer.request().await;
        match request {
            Request::Cancel { turn_id, .. } => {
                assert_eq!(turn_id, id);
                break stream;
            }
            Request::Subscribe { turn_id, .. } => {
                assert_eq!(turn_id, id);
                reads.push(stream);
            }
            Request::Transcript { .. } => reads.push(stream),
            _ => panic!("only read-only refix may accompany cancellation"),
        }
    };
    reply(&mut retry, Response::Cancelled { owned: true }).await;
    let report = within(detaching).await.unwrap();
    assert_eq!(report.turn_id, Some(id));
    assert_eq!(
        report.submission,
        Some(Ok(Acceptance::Accepted { turn_id: id }))
    );
    assert_eq!(report.cancel, Some(CancelStatus::Taken));
}

#[tokio::test]
async fn controlled_and_legacy_submission_share_lazy_identity_and_configuration() {
    for controlled in [false, true] {
        let peer = Peer::new();
        let id = new_turn_id();
        let pending = view(id, "pending", false);
        let request = TurnRequest {
            session_id: pending.session.session_id.clone(),
            turn_id: id,
            workspace: peer._directory.path().into(),
            input: "byte-exact 原文".into(),
        };
        let expected = request.clone();
        let config = super::tests::frozen(peer._directory.path(), "http://127.0.0.1:9/v1");
        let loads = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let called = loads.clone();
        let client = peer.client.clone();
        let task = tokio::spawn(async move {
            let load = move || async move {
                called.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(config)
            };
            if controlled {
                let mut turn = client.start_turn_with(request, HistoryScope::SelectedTurn, load);
                update_until(&mut turn, |u| matches!(u, LocalUpdate::Submission { .. })).await;
                turn.detach().await.submission.unwrap()
            } else {
                client.submit_with(request, load).await
            }
        });
        let mut subscription = None;
        let mut submitted = false;
        while !submitted {
            let (mut stream, wire) = peer.request().await;
            match wire {
                Request::Submit {
                    request,
                    configuration,
                } => {
                    assert!(request == expected, "identity/input/workspace preserved");
                    if let Some(configuration) = configuration {
                        assert_eq!(
                            serde_json::to_value(&configuration).unwrap()["system_prompt"],
                            "test"
                        );
                        reply(
                            &mut stream,
                            Response::Acceptance(Acceptance::Accepted { turn_id: id }),
                        )
                        .await;
                        submitted = true;
                    } else {
                        assert_eq!(loads.load(std::sync::atomic::Ordering::SeqCst), 0);
                        reply(
                            &mut stream,
                            Response::Acceptance(Acceptance::NeedsConfiguration),
                        )
                        .await;
                    }
                }
                Request::Subscribe { .. } => subscription = Some(stream),
                _ => panic!("unexpected handshake operation"),
            }
        }
        assert_eq!(
            within(task).await.unwrap(),
            Ok(Acceptance::Accepted { turn_id: id })
        );
        assert_eq!(loads.load(std::sync::atomic::Ordering::SeqCst), 1);
        drop(subscription);
    }
}

#[tokio::test]
async fn o2_mid_segment_suffix_survives_history_and_resets_only_at_new_cut() {
    let peer = Peer::new();
    let id = new_turn_id();
    let pending = view(id, "pending", false);
    let mut turn = peer.client.observe(
        pending.session.session_id.clone(),
        Some(id),
        HistoryScope::SelectedTurn,
    );
    let (mut stream, _) = peer.request().await;
    snapshot(&mut stream, pending.clone(), true).await;
    update_until(&mut turn, |u| matches!(u, LocalUpdate::Reset { .. })).await;
    let (mut body, _) = peer.request().await;
    let event = crate::TurnEvent::TokenDelta {
        content: "new suffix".into(),
    };
    reply(
        &mut stream,
        Response::Observation(Observation::Event {
            cursor: cursor(1),
            event: event.clone(),
        }),
    )
    .await;
    assert!(
        matches!(within(turn.next()).await, Some(LocalUpdate::Event { event: original, text: TextAction::StartIncomplete, .. }) if original == event)
    );
    reply(
        &mut body,
        Response::Transcript(TranscriptPage {
            jsonl: String::new(),
            next_offset: None,
        }),
    )
    .await;
    assert!(matches!(
        within(turn.next()).await,
        Some(LocalUpdate::History {
            stale: false,
            result: Ok(_),
            ..
        })
    ));
    reply(
        &mut stream,
        Response::Observation(Observation::Event {
            cursor: cursor(2),
            event: event.clone(),
        }),
    )
    .await;
    assert!(matches!(
        within(turn.next()).await,
        Some(LocalUpdate::Event {
            text: TextAction::Continue,
            ..
        })
    ));
    turn.reconnect();
    assert!(matches!(
        within(turn.next()).await,
        Some(LocalUpdate::Reset {
            reason: ResetReason::Reconnect,
            ..
        })
    ));
    let (mut stream, request) = peer.request().await;
    assert!(matches!(request, Request::Subscribe { .. }));
    snapshot(&mut stream, pending.clone(), true).await;
    update_until(&mut turn, |u| matches!(u, LocalUpdate::Reset { .. })).await;
    let (_body, _) = peer.request().await;
    reply(
        &mut stream,
        Response::Observation(Observation::Event {
            cursor: cursor(3),
            event,
        }),
    )
    .await;
    assert!(matches!(
        within(turn.next()).await,
        Some(LocalUpdate::Event {
            text: TextAction::StartIncomplete,
            ..
        })
    ));
    reply(
        &mut stream,
        Response::Observation(Observation::Gap { cursor: cursor(6) }),
    )
    .await;
    assert!(matches!(
        within(turn.next()).await,
        Some(LocalUpdate::Reset {
            reason: ResetReason::Gap,
            ..
        })
    ));
    snapshot(&mut stream, pending, true).await;
    update_until(&mut turn, |u| matches!(u, LocalUpdate::Reset { .. })).await;
    for (sequence, event, action) in [
        (
            7,
            crate::TurnEvent::ModelStarted {
                provider: None,
                model: "new".into(),
            },
            TextAction::Start,
        ),
        (
            8,
            crate::TurnEvent::TokenDelta {
                content: "normal".into(),
            },
            TextAction::Continue,
        ),
        (
            9,
            crate::TurnEvent::ModelFinished {
                finish_reason: "stop".into(),
                usage: None,
            },
            TextAction::Finish,
        ),
    ] {
        reply(
            &mut stream,
            Response::Observation(Observation::Event {
                cursor: cursor(sequence),
                event: event.clone(),
            }),
        )
        .await;
        assert!(
            matches!(within(turn.next()).await, Some(LocalUpdate::Event { event: original, text, .. }) if original == event && text == action)
        );
    }
    within(turn.detach()).await;
}

#[tokio::test]
async fn visible_result_without_completion_is_not_an_independent_finished_receipt() {
    // 在途 Owner 的可见 Committed 结果只给出 Snapshot；真实存储失败才作为独立
    // Finished 携带诊断。两者都不能凭空变成确认。
    for store_failure in [false, true] {
        let peer = Peer::new();
        let id = new_turn_id();
        let fixed = view(id, "pin", true);
        let mut turn = peer.client.observe(
            fixed.session.session_id.clone(),
            Some(id),
            HistoryScope::SelectedTurn,
        );
        let (mut subscription, _) = peer.request().await;
        let error = store_failure.then(|| {
            LocalError::new(
                LocalErrorKind::StoreUnavailable,
                "store unavailable; inspect durable state",
            )
        });
        match &error {
            Some(error) => {
                reply(
                    &mut subscription,
                    Response::Observation(Observation::Finished {
                        view: Box::new(fixed.clone()),
                        cursor: cursor(0),
                        error: Some(error.clone()),
                    }),
                )
                .await;
            }
            None => snapshot(&mut subscription, fixed.clone(), true).await,
        }
        let Some(LocalUpdate::State {
            view,
            phase,
            confirmation,
            error: diagnostic,
            ..
        }) = within(turn.next()).await
        else {
            panic!("fixed state before transcript")
        };
        assert_eq!(*view, fixed);
        assert_eq!(confirmation, CompletionConfirmation::Unknown);
        assert_eq!(diagnostic, error);
        assert_eq!(
            phase,
            if store_failure {
                ObservationPhase::Finished
            } else {
                ObservationPhase::Live
            }
        );
        let report = within(turn.detach()).await;
        assert_eq!(report.state.unwrap().phase, phase);
    }
}

#[tokio::test]
async fn o1_body_error_keeps_its_fixed_object_and_completion_survives_reconnect() {
    let peer = Peer::new();
    let id = new_turn_id();
    let pending = view(id, "old-journal", false);
    let mut turn = peer.client.observe(
        pending.session.session_id.clone(),
        Some(id),
        HistoryScope::SelectedTurn,
    );
    let (mut subscription, _) = peer.request().await;
    snapshot(&mut subscription, pending.clone(), true).await;
    update_until(&mut turn, |u| matches!(u, LocalUpdate::Reset { .. })).await;
    let (mut body, _) = peer.request().await;
    let failure = LocalError::new(LocalErrorKind::StoreUnavailable, "old body unavailable");
    reply(&mut body, Response::Error(failure.clone())).await;
    let Some(LocalUpdate::History {
        view: failed_view,
        result: Err(error),
        ..
    }) = within(turn.next()).await
    else {
        panic!("body diagnostic")
    };
    assert_eq!(*failed_view, pending);
    assert_eq!(error.error, failure);
    assert_eq!(
        error.context,
        HistoryReadContext::Transcript {
            object_id: "old-journal".into()
        }
    );
    reply(
        &mut subscription,
        Response::Observation(Observation::Finished {
            view: Box::new(view(id, "final", true)),
            cursor: cursor(1),
            error: None,
        }),
    )
    .await;
    assert!(matches!(
        within(turn.next()).await,
        Some(LocalUpdate::State {
            confirmation: CompletionConfirmation::Confirmed,
            ..
        })
    ));
    update_until(&mut turn, |u| matches!(u, LocalUpdate::Reset { .. })).await;
    let (_body, _) = peer.request().await;
    turn.reconnect();
    assert!(matches!(
        within(turn.next()).await,
        Some(LocalUpdate::Reset {
            reason: ResetReason::Reconnect,
            ..
        })
    ));
    let (mut subscription, request) = peer.request().await;
    assert!(matches!(request, Request::Subscribe { .. }));
    // 重连后真实存储失败仍按独立 Finished 携带诊断交付；已取得的固定结果确认保留。
    let unavailable = LocalError::new(
        LocalErrorKind::StoreUnavailable,
        "store unavailable; inspect durable state",
    );
    reply(
        &mut subscription,
        Response::Observation(Observation::Finished {
            view: Box::new(view(id, "final", true)),
            cursor: EventCursor {
                epoch: "restarted".into(),
                sequence: 0,
            },
            error: Some(unavailable.clone()),
        }),
    )
    .await;
    assert!(
        matches!(within(turn.next()).await, Some(LocalUpdate::State { phase: ObservationPhase::Finished, confirmation: CompletionConfirmation::Confirmed, error: Some(error), .. }) if error == unavailable)
    );
    let report = within(turn.detach()).await;
    assert_eq!(
        report.history_error.unwrap().context,
        HistoryReadContext::Transcript {
            object_id: "old-journal".into()
        }
    );
    assert_eq!(
        report.state.unwrap().confirmation,
        CompletionConfirmation::Confirmed
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn l3_late_blocking_configuration_cannot_send_after_cancel_or_drop() {
    for cancel in [false, true] {
        let peer = Peer::new();
        let id = new_turn_id();
        let request = TurnRequest {
            session_id: view(id, "pending", false).session.session_id,
            turn_id: id,
            workspace: peer._directory.path().into(),
            input: "original".into(),
        };
        let config = super::tests::frozen(peer._directory.path(), "http://127.0.0.1:9/v1");
        let (arrived, ready) = tokio::sync::oneshot::channel();
        let (release, gate) = std::sync::mpsc::channel();
        let (done, finished) = tokio::sync::oneshot::channel();
        let mut turn =
            peer.client
                .start_turn_with(request, HistoryScope::SelectedTurn, || async move {
                    tokio::task::spawn_blocking(move || {
                        arrived.send(()).unwrap();
                        gate.recv_timeout(Duration::from_secs(2)).unwrap();
                        let _ = done.send(());
                        Ok(config)
                    })
                    .await
                    .unwrap()
                });
        let mut subscription = None;
        for _ in 0..2 {
            let (mut stream, request) = peer.request().await;
            match request {
                Request::Submit {
                    configuration: None,
                    ..
                } => {
                    reply(
                        &mut stream,
                        Response::Acceptance(Acceptance::NeedsConfiguration),
                    )
                    .await
                }
                Request::Subscribe { .. } => subscription = Some(stream),
                _ => panic!("initial handshake"),
            }
        }
        within(ready).await.unwrap();
        if cancel {
            turn.cancel();
            let mut receipt = cancel_request(&peer, id).await;
            reply(&mut receipt, Response::Cancelled { owned: true }).await;
            update_until(&mut turn, |u| {
                matches!(
                    u,
                    LocalUpdate::Cancel {
                        status: CancelStatus::Taken,
                        ..
                    }
                )
            })
            .await;
            let report = within(turn.detach()).await;
            assert_eq!(
                report.not_submitted,
                Some(NotSubmittedStage::FreezingConfiguration)
            );
        } else {
            drop(turn);
        }
        release.send(()).unwrap();
        within(finished).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), peer.listener.accept())
                .await
                .is_err(),
            "late config and Drop must not send Submit/Cancel"
        );
        drop(subscription);
    }
}

#[tokio::test]
async fn l1_new_intent_does_not_inherit_an_old_inflight_receipt() {
    let peer = Peer::new();
    let id = new_turn_id();
    let pending = view(id, "pending", false);
    let mut turn = peer.client.observe(
        pending.session.session_id.clone(),
        Some(id),
        HistoryScope::SelectedTurn,
    );
    let (mut subscription, _) = peer.request().await;
    snapshot(&mut subscription, pending, true).await;
    update_until(&mut turn, |u| matches!(u, LocalUpdate::State { .. })).await;
    turn.cancel();
    let mut old = cancel_request(&peer, id).await;
    update_until(&mut turn, |u| {
        matches!(
            u,
            LocalUpdate::Cancel {
                status: CancelStatus::Pending,
                ..
            }
        )
    })
    .await;
    turn.cancel();
    update_until(&mut turn, |u| {
        matches!(
            u,
            LocalUpdate::Cancel {
                status: CancelStatus::Pending,
                ..
            }
        )
    })
    .await;
    // 负向检查：旧在途回执未结束前不得并发发送第二轮；只对 Cancel 类型失败。
    assert_no_concurrent_cancel(&peer).await;
    // 旧回执只结束旧一轮：新意图必须自己发出并取得自己的回执，
    // 旧 true 不能直接变成新意图的 Taken。
    reply(&mut old, Response::Cancelled { owned: true }).await;
    let mut new = cancel_request(&peer, id).await;
    let update = update_until(&mut turn, |u| matches!(u, LocalUpdate::Cancel { .. })).await;
    let LocalUpdate::Cancel { status, .. } = update else {
        unreachable!("Cancel update")
    };
    assert_eq!(status, CancelStatus::Pending, "旧回执不得成为新意图的结果");
    reply(&mut new, Response::Cancelled { owned: true }).await;
    update_until(&mut turn, |u| {
        matches!(
            u,
            LocalUpdate::Cancel {
                status: CancelStatus::Taken,
                ..
            }
        )
    })
    .await;
    assert_eq!(
        within(turn.detach()).await.cancel,
        Some(CancelStatus::Taken)
    );
}

#[tokio::test]
async fn cancel_diagnostic_survives_detach_without_update_consumption() {
    for error_before_detach in [false, true] {
        let peer = Peer::new();
        let id = new_turn_id();
        let pending = view(id, "pending", false);
        let mut turn = peer.client.observe(
            pending.session.session_id.clone(),
            Some(id),
            HistoryScope::SelectedTurn,
        );
        let (mut subscription, _) = peer.request().await;
        snapshot(&mut subscription, pending, true).await;
        update_until(&mut turn, |u| matches!(u, LocalUpdate::State { .. })).await;
        turn.cancel();
        let mut first = cancel_request(&peer, id).await;
        let error = LocalError::new(LocalErrorKind::Permission, "original cancel denied");
        let mut retry = if error_before_detach {
            reply(&mut first, Response::Error(error.clone())).await;
            Some(cancel_request(&peer, id).await)
        } else {
            None
        };
        let report = tokio::select! {
            biased;
            report = within(turn.detach()) => report,
            _ = async {
                if retry.is_none() {
                    reply(&mut first, Response::Error(error.clone())).await;
                    retry = Some(cancel_request(&peer, id).await);
                }
                reply(retry.as_mut().unwrap(), Response::Error(error.clone())).await;
                std::future::pending::<()>().await;
            } => unreachable!(),
        };
        assert_eq!(report.turn_id, Some(id));
        assert_eq!(report.cancel, Some(CancelStatus::Unknown));
        assert_eq!(report.cancel_error, Some(error));
        assert!(
            tokio::time::timeout(Duration::from_millis(50), peer.listener.accept())
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn cancel_diagnostic_is_cleared_by_a_successful_receipt() {
    for owned in [false, true] {
        let peer = Peer::new();
        let id = new_turn_id();
        let pending = view(id, "pending", false);
        let mut turn = peer.client.observe(
            pending.session.session_id.clone(),
            Some(id),
            HistoryScope::SelectedTurn,
        );
        let (mut subscription, _) = peer.request().await;
        snapshot(&mut subscription, pending, true).await;
        update_until(&mut turn, |u| matches!(u, LocalUpdate::State { .. })).await;
        turn.cancel();
        let mut first = cancel_request(&peer, id).await;
        let error = LocalError::new(LocalErrorKind::Permission, "first cancel denied");
        reply(&mut first, Response::Error(error.clone())).await;
        update_until(&mut turn, |u| {
            matches!(
                u,
                LocalUpdate::Cancel {
                    status: CancelStatus::Unknown,
                    ..
                }
            )
        })
        .await;
        let retry_update = update_until(&mut turn, |u| {
            matches!(
                u,
                LocalUpdate::Cancel {
                    status: CancelStatus::Pending,
                    ..
                }
            )
        })
        .await;
        assert!(
            matches!(retry_update, LocalUpdate::Cancel { error: Some(actual), .. } if actual == error)
        );
        let mut retry = cancel_request(&peer, id).await;
        reply(&mut retry, Response::Cancelled { owned }).await;
        let expected = if owned {
            CancelStatus::Taken
        } else {
            CancelStatus::NotTaken
        };
        let update = update_until(
            &mut turn,
            |u| matches!(u, LocalUpdate::Cancel { status, .. } if *status == expected),
        )
        .await;
        assert!(matches!(update, LocalUpdate::Cancel { error: None, .. }));
        let report = within(turn.detach()).await;
        assert_eq!(report.cancel, Some(expected));
        assert_eq!(report.cancel_error, None);
    }
}

#[tokio::test]
async fn new_cancel_intent_clears_diagnostic_and_ignores_the_old_retry_error() {
    let peer = Peer::new();
    let id = new_turn_id();
    let pending = view(id, "pending", false);
    let mut turn = peer.client.observe(
        pending.session.session_id.clone(),
        Some(id),
        HistoryScope::SelectedTurn,
    );
    let (mut subscription, _) = peer.request().await;
    snapshot(&mut subscription, pending, true).await;
    update_until(&mut turn, |u| matches!(u, LocalUpdate::State { .. })).await;
    turn.cancel();
    let mut first = cancel_request(&peer, id).await;
    reply(
        &mut first,
        Response::Error(LocalError::new(LocalErrorKind::Permission, "old denial")),
    )
    .await;
    let mut old_retry = cancel_request(&peer, id).await;
    update_until(&mut turn, |u| {
        matches!(
            u,
            LocalUpdate::Cancel {
                status: CancelStatus::Unknown,
                ..
            }
        )
    })
    .await;
    update_until(&mut turn, |u| {
        matches!(
            u,
            LocalUpdate::Cancel {
                status: CancelStatus::Pending,
                ..
            }
        )
    })
    .await;
    turn.cancel();
    let new_intent = update_until(&mut turn, |u| {
        matches!(
            u,
            LocalUpdate::Cancel {
                status: CancelStatus::Pending,
                ..
            }
        )
    })
    .await;
    assert!(matches!(
        new_intent,
        LocalUpdate::Cancel { error: None, .. }
    ));
    reply(
        &mut old_retry,
        Response::Error(LocalError::new(
            LocalErrorKind::Permission,
            "late old denial",
        )),
    )
    .await;
    let mut new = cancel_request(&peer, id).await;
    let pending = update_until(&mut turn, |u| matches!(u, LocalUpdate::Cancel { .. })).await;
    assert!(matches!(
        pending,
        LocalUpdate::Cancel {
            status: CancelStatus::Pending,
            error: None,
            ..
        }
    ));
    reply(&mut new, Response::Cancelled { owned: true }).await;
    update_until(&mut turn, |u| {
        matches!(
            u,
            LocalUpdate::Cancel {
                status: CancelStatus::Taken,
                ..
            }
        )
    })
    .await;
    let report = within(turn.detach()).await;
    assert_eq!(report.turn_id, Some(id));
    assert_eq!(report.cancel, Some(CancelStatus::Taken));
    assert_eq!(report.cancel_error, None);
}

#[tokio::test]
async fn l2_detach_timeout_preserves_original_identity_and_unknown_receipt() {
    use tokio::io::AsyncReadExt;
    let peer = Peer::new();
    let id = new_turn_id();
    let turn = peer.client.observe(
        view(id, "pending", false).session.session_id,
        Some(id),
        HistoryScope::SelectedTurn,
    );
    let (_subscription, _) = peer.request().await;
    turn.cancel();
    let mut receipt = cancel_request(&peer, id).await;
    let start = tokio::time::Instant::now();
    let report = tokio::time::timeout(Duration::from_secs(6), turn.detach())
        .await
        .expect("five-second handoff deadline");
    assert!(start.elapsed() >= Duration::from_secs(5));
    assert_eq!(report.turn_id, Some(id));
    assert_eq!(report.cancel, Some(CancelStatus::Unknown));
    assert_eq!(
        within(receipt.read_u8()).await.unwrap_err().kind(),
        std::io::ErrorKind::UnexpectedEof
    );
}

#[tokio::test]
async fn observe_session_selects_active_then_head_and_empty_needs_no_subscription() {
    for selection in ["active", "head", "empty"] {
        let peer = Peer::new();
        let active_id = new_turn_id();
        let head_id = new_turn_id();
        let mut fixed = view(active_id, "pending", false);
        fixed.requested_turn = None;
        fixed.head_commit_id = Some("head".into());
        if selection != "active" {
            fixed.active_turn = None;
        }
        if selection == "empty" {
            fixed.head_commit_id = None;
        }
        let mut turn = peer.client.observe(
            fixed.session.session_id.clone(),
            None,
            HistoryScope::Session,
        );
        let (mut stream, request) = peer.request().await;
        assert!(matches!(request, Request::Query { turn_id: None, .. }));
        reply(&mut stream, Response::View(Box::new(fixed.clone()))).await;
        if selection == "head" {
            let (mut stream, request) = peer.request().await;
            assert!(matches!(request, Request::Commit { commit_id, .. } if commit_id == "head"));
            reply(
                &mut stream,
                Response::Commit(Box::new(commit(head_id, "head"))),
            )
            .await;
        }
        if selection != "empty" {
            let (_stream, request) = peer.request().await;
            let expected = if selection == "active" {
                active_id
            } else {
                head_id
            };
            assert!(matches!(request, Request::Subscribe { turn_id, .. } if turn_id == expected));
        } else {
            let update =
                update_until(&mut turn, |u| matches!(u, LocalUpdate::History { .. })).await;
            assert!(
                matches!(update, LocalUpdate::History { result: Ok(history), .. } if history.entries.is_empty() && history.view == fixed)
            );
            assert!(
                tokio::time::timeout(Duration::from_millis(50), peer.listener.accept())
                    .await
                    .is_err()
            );
        }
        within(turn.detach()).await;
    }
}

#[tokio::test]
async fn legacy_submit_with_accepts_borrowed_non_send_loader() {
    let peer = Peer::new();
    let id = new_turn_id();
    let request = TurnRequest {
        session_id: view(id, "pending", false).session.session_id,
        turn_id: id,
        workspace: peer._directory.path().into(),
        input: "original".into(),
    };
    let called = std::rc::Rc::new(std::cell::Cell::new(false));
    let load_error = LocalError::new(LocalErrorKind::BadRequest, "configuration unavailable");
    let (result, ()) = tokio::join!(
        peer.client.submit_with(request, || async {
            called.set(true);
            Err(load_error.clone())
        }),
        async {
            let (mut stream, request) = peer.request().await;
            assert!(matches!(
                request,
                Request::Submit {
                    configuration: None,
                    ..
                }
            ));
            reply(
                &mut stream,
                Response::Acceptance(Acceptance::NeedsConfiguration),
            )
            .await;
        }
    );
    assert_eq!(result, Err(load_error));
    assert!(called.get());
}

#[tokio::test]
async fn recovered_observation_never_receives_the_original_cancel_intent() {
    let peer = Peer::new();
    let original = new_turn_id();
    let recovered = new_turn_id();
    let request = TurnRequest {
        session_id: view(original, "pending", false).session.session_id,
        turn_id: original,
        workspace: peer._directory.path().into(),
        input: "new prompt".into(),
    };
    let mut turn = peer
        .client
        .start_turn_with(request, HistoryScope::SelectedTurn, || async {
            panic!("recovery must not load new config")
        });
    let mut probe = None;
    let mut original_stream = None;
    for _ in 0..2 {
        let (stream, request) = peer.request().await;
        match request {
            Request::Submit {
                configuration: None,
                ..
            } => probe = Some(stream),
            Request::Subscribe { turn_id, .. } => {
                assert_eq!(turn_id, original);
                original_stream = Some(stream);
            }
            _ => panic!("initial operations"),
        }
    }
    turn.cancel();
    let mut receipt = cancel_request(&peer, original).await;
    reply(&mut receipt, Response::Cancelled { owned: true }).await;
    update_until(&mut turn, |u| {
        matches!(
            u,
            LocalUpdate::Cancel {
                status: CancelStatus::Taken,
                ..
            }
        )
    })
    .await;
    reply(
        probe.as_mut().unwrap(),
        Response::Acceptance(Acceptance::Recovered {
            turn: Box::new(commit(recovered, "recovered-pin")),
        }),
    )
    .await;
    assert!(
        matches!(update_until(&mut turn, |u| matches!(u, LocalUpdate::Submission { .. })).await,
        LocalUpdate::Submission { turn_id, result: Ok(Acceptance::Recovered { .. }) } if turn_id == original)
    );
    let (mut stream, request) = peer.request().await;
    assert!(matches!(request, Request::Subscribe { turn_id, .. } if turn_id == recovered));
    snapshot(&mut stream, view(recovered, "recovered-pin", true), false).await;
    update_until(&mut turn, |u| matches!(u, LocalUpdate::State { .. })).await;
    assert_eq!(turn.turn_id(), Some(original));
    assert_eq!(turn.observed_turn_id(), Some(recovered));
    turn.cancel(); // 已关闭的原目标无须新取消，但既有 Taken 回执仍须保留。
    let report = within(turn.detach()).await;
    assert_eq!(report.turn_id, Some(original));
    assert_eq!(report.cancel, Some(CancelStatus::Taken));
    assert_eq!(
        report.state.unwrap().view.requested_turn,
        view(recovered, "recovered-pin", true).requested_turn
    );
    drop(original_stream);
}

#[tokio::test]
async fn reconnect_after_idle_gets_a_fresh_observation_window() {
    let peer = Peer::new();
    let id = new_turn_id();
    let pending = view(id, "pending", false);
    let mut turn = peer.client.observe(
        pending.session.session_id.clone(),
        Some(id),
        HistoryScope::SelectedTurn,
    );
    let (mut stream, _) = peer.request().await;
    snapshot(&mut stream, pending.clone(), true).await;
    update_until(&mut turn, |u| matches!(u, LocalUpdate::Reset { .. })).await;
    transcript(&peer, "pending", "saved prefix").await;
    update_until(&mut turn, |u| matches!(u, LocalUpdate::History { .. })).await;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(crate::STREAM_IDLE_TIMEOUT_SECS + 1)).await;
    update_until(&mut turn, |u| {
        matches!(u, LocalUpdate::Connection(ConnectionStatus::Idle))
    })
    .await;
    tokio::time::resume();
    turn.reconnect();
    update_until(&mut turn, |u| {
        matches!(
            u,
            LocalUpdate::Reset {
                reason: ResetReason::Reconnect,
                ..
            }
        )
    })
    .await;
    let (mut stream, _) = peer.request().await;
    snapshot(&mut stream, pending, true).await;
    assert!(matches!(
        within(turn.next()).await,
        Some(LocalUpdate::State {
            phase: ObservationPhase::Live,
            ..
        })
    ));
    within(turn.detach()).await;
}

#[tokio::test]
async fn stalled_final_history_times_out_without_revoking_completion() {
    let peer = Peer::new();
    let id = new_turn_id();
    let fixed = view(id, "final", true);
    let mut turn = peer.client.observe(
        fixed.session.session_id.clone(),
        Some(id),
        HistoryScope::SelectedTurn,
    );
    let (mut stream, _) = peer.request().await;
    reply(
        &mut stream,
        Response::Observation(Observation::Finished {
            view: Box::new(fixed.clone()),
            cursor: cursor(1),
            error: None,
        }),
    )
    .await;
    update_until(&mut turn, |u| matches!(u, LocalUpdate::Reset { .. })).await;
    let (_body, _) = peer.request().await;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(crate::STREAM_IDLE_TIMEOUT_SECS + 1)).await;
    let update = within(turn.next()).await;
    tokio::time::resume();
    assert!(
        matches!(update, Some(LocalUpdate::History { view, result: Err(_), .. }) if *view == fixed)
    );
    let report = within(turn.detach()).await;
    assert!(report.history_error.is_some());
    assert_eq!(
        report.state.unwrap().confirmation,
        CompletionConfirmation::Confirmed
    );
}

#[tokio::test]
async fn lc1_acceptance_replaces_a_delayed_pre_submit_no_owner_cut() {
    let peer = Peer::new();
    let id = new_turn_id();
    let mut old = view(id, "not-yet-started", false);
    old.active_turn = None;
    old.requested_turn = None;
    let request = TurnRequest {
        session_id: old.session.session_id.clone(),
        turn_id: id,
        workspace: peer._directory.path().into(),
        input: "original".into(),
    };
    let configuration = super::tests::frozen(peer._directory.path(), "http://127.0.0.1:9/v1");
    let mut turn =
        peer.client
            .start_turn_with(request.clone(), HistoryScope::SelectedTurn, || async {
                Ok(configuration)
            });
    let mut probe = None;
    let mut pre_submit = None;
    for _ in 0..2 {
        let (stream, request) = peer.request().await;
        match request {
            Request::Submit {
                configuration: None,
                ..
            } => probe = Some(stream),
            Request::Subscribe { turn_id, .. } if turn_id == id => pre_submit = Some(stream),
            _ => panic!("initial operations"),
        }
    }
    // 对端已固定无 Owner 的视图，但其帧交付晚于另一个连接的接受回执。
    reply(
        probe.as_mut().unwrap(),
        Response::Acceptance(Acceptance::NeedsConfiguration),
    )
    .await;
    let (mut configured, sent) = peer.request().await;
    assert!(
        matches!(sent, Request::Submit { request: original, configuration: Some(_), .. } if original == request)
    );
    reply(
        &mut configured,
        Response::Acceptance(Acceptance::Accepted { turn_id: id }),
    )
    .await;
    update_until(&mut turn, |u| {
        matches!(
            u,
            LocalUpdate::Submission {
                result: Ok(Acceptance::Accepted { .. }),
                ..
            }
        )
    })
    .await;
    // 正确实现可已退场旧连接；写失败只允许出现在这个已被取代的帧。
    let _ = ipc::write(
        pre_submit.as_mut().unwrap(),
        &Response::Observation(Observation::Snapshot {
            view: Box::new(old),
            cursor: cursor(0),
            owned: false,
        }),
    )
    .await;
    let (mut fresh, sent) = peer.request().await;
    assert!(
        matches!(sent, Request::Subscribe { turn_id, cursor: None, .. } if turn_id == id),
        "must resubscribe, not resubmit configuration"
    );
    snapshot(&mut fresh, view(id, "pending", false), true).await;
    update_until(&mut turn, |u| {
        matches!(
            u,
            LocalUpdate::State {
                phase: ObservationPhase::Live,
                ..
            }
        )
    })
    .await;
    let (_pending_body, sent) = peer.request().await;
    assert!(matches!(sent, Request::Transcript { commit_id, .. } if commit_id == "pending"));
    reply(
        &mut fresh,
        Response::Observation(Observation::Finished {
            view: Box::new(view(id, "final", true)),
            cursor: cursor(1),
            error: None,
        }),
    )
    .await;
    update_until(&mut turn, |u| {
        matches!(
            u,
            LocalUpdate::State {
                phase: ObservationPhase::Finished,
                confirmation: CompletionConfirmation::Confirmed,
                ..
            }
        )
    })
    .await;
    transcript(&peer, "final", "final answer").await;
    let history = update_until(&mut turn, |u| {
        matches!(u, LocalUpdate::History { result: Ok(_), .. })
    })
    .await;
    assert!(
        matches!(history, LocalUpdate::History { view: fixed, stale: false, .. } if *fixed == view(id, "final", true))
    );
    assert_eq!(
        within(turn.detach()).await.submission,
        Some(Ok(Acceptance::Accepted { turn_id: id }))
    );
}

#[tokio::test]
async fn lc2_local_configuration_error_preserves_live_cancel_target() {
    for kind in [LocalErrorKind::BadRequest, LocalErrorKind::Transport] {
        local_configuration_error_preserves_live_cancel_target(kind).await;
    }
}

async fn local_configuration_error_preserves_live_cancel_target(kind: LocalErrorKind) {
    let peer = Peer::new();
    let id = new_turn_id();
    let pending = view(id, "other-client-journal", false);
    let request = TurnRequest {
        session_id: pending.session.session_id.clone(),
        turn_id: id,
        workspace: peer._directory.path().into(),
        input: "original".into(),
    };
    let failure = LocalError::new(kind, "local configuration invalid");
    let expected = failure.clone();
    let (release, loading) = tokio::sync::oneshot::channel();
    let mut turn =
        peer.client
            .start_turn_with(request, HistoryScope::SelectedTurn, || async move {
                loading.await.unwrap();
                Err(failure)
            });
    let mut subscription = None;
    for _ in 0..2 {
        let (mut stream, request) = peer.request().await;
        match request {
            Request::Submit {
                configuration: None,
                ..
            } => {
                reply(
                    &mut stream,
                    Response::Acceptance(Acceptance::NeedsConfiguration),
                )
                .await
            }
            Request::Subscribe { .. } => subscription = Some(stream),
            _ => panic!("initial operations"),
        }
    }
    // 另一客户端的同身份执行已被观察，不能以本地配置错误宣布目标关闭。
    snapshot(subscription.as_mut().unwrap(), pending, true).await;
    update_until(&mut turn, |u| {
        matches!(
            u,
            LocalUpdate::State {
                phase: ObservationPhase::Live,
                ..
            }
        )
    })
    .await;
    release.send(()).unwrap();
    let submitted = update_until(&mut turn, |u| matches!(u, LocalUpdate::Submission { .. })).await;
    assert!(
        matches!(submitted, LocalUpdate::Submission { result: Err(error), .. } if error == expected)
    );
    turn.cancel();
    let mut receipt = cancel_request(&peer, id).await;
    reply(&mut receipt, Response::Cancelled { owned: true }).await;
    update_until(&mut turn, |u| {
        matches!(
            u,
            LocalUpdate::Cancel {
                status: CancelStatus::Taken,
                ..
            }
        )
    })
    .await;
    let report = within(turn.detach()).await;
    assert_eq!(report.submission, Some(Err(expected)));
    assert_eq!(report.submission_diagnostic, None);
    assert_eq!(report.cancel, Some(CancelStatus::Taken));
    assert_eq!(report.state.unwrap().phase, ObservationPhase::Live);
}

#[tokio::test]
async fn lc3_received_finished_survives_ui_backpressure_and_idle() {
    for detach_without_consuming in [true, false] {
        let mut peer = Peer::new();
        let received = std::sync::Arc::new(tokio::sync::Notify::new());
        peer.client.finished_received = Some(received.clone());
        let id = new_turn_id();
        let mut turn = peer.client.observe(
            view(id, "pending", false).session.session_id,
            Some(id),
            HistoryScope::SelectedTurn,
        );
        let (mut stream, _) = peer.request().await;
        snapshot(&mut stream, view(id, "pending", false), true).await;
        update_until(&mut turn, |u| matches!(u, LocalUpdate::Reset { .. })).await;
        transcript(&peer, "pending", "saved prefix").await;
        update_until(&mut turn, |u| matches!(u, LocalUpdate::History { .. })).await;
        // 16 UI + 16 pending 已满，后续控制帧仍能被8槽帧 reader 解码。
        for sequence in 1..=36 {
            reply(
                &mut stream,
                Response::Observation(Observation::Event {
                    cursor: cursor(sequence),
                    event: crate::TurnEvent::TokenDelta {
                        content: "live".into(),
                    },
                }),
            )
            .await;
        }
        let final_view = view(id, "final", true);
        reply(
            &mut stream,
            Response::Observation(Observation::Finished {
                view: Box::new(final_view.clone()),
                cursor: cursor(37),
                error: None,
            }),
        )
        .await;
        within(received.notified()).await;
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(crate::STREAM_IDLE_TIMEOUT_SECS + 1)).await;
        tokio::task::yield_now().await;
        tokio::time::resume();
        if !detach_without_consuming {
            let mut gap = false;
            let mut count = 0;
            loop {
                let update = within(turn.next()).await.unwrap();
                count += 1;
                assert!(count < 64, "presentation backlog must stay bounded");
                match update {
                    LocalUpdate::Reset {
                        reason: ResetReason::Gap,
                        ..
                    } => gap = true,
                    LocalUpdate::State {
                        phase: ObservationPhase::Finished,
                        view,
                        confirmation,
                        ..
                    } => {
                        assert!(gap, "discarded temporary events require an explicit Gap");
                        assert_eq!(*view, final_view);
                        assert_eq!(confirmation, CompletionConfirmation::Confirmed);
                        break;
                    }
                    LocalUpdate::Connection(ConnectionStatus::Idle) => {
                        panic!("received Finished is not observation idle")
                    }
                    _ => {}
                }
            }
        }
        let report = within(turn.detach()).await;
        let state = report.state.unwrap();
        assert_eq!(
            state.phase,
            ObservationPhase::Finished,
            "received completion must not depend on UI consumption"
        );
        assert_eq!(state.confirmation, CompletionConfirmation::Confirmed);
        assert_eq!(*state.view, final_view);
    }
}

#[derive(Default)]
pub(super) struct BodyRetirementGate {
    pub retiring: tokio::sync::Notify,
    pub read_done: tokio::sync::Notify,
    pub release: tokio::sync::Notify,
}

impl BodyRetirementGate {
    pub fn matches(&self, view: &SessionObservation) -> bool {
        matches!(&view.requested_turn, Some(TurnObservation::Pending(turn)) if turn.journal_oid == "retiring-body")
    }
}

#[tokio::test]
async fn v1_completed_old_ipc_read_is_harvested_only_for_failure() {
    for fails in [false, true] {
        let mut peer = Peer::new();
        let gate = std::sync::Arc::new(BodyRetirementGate::default());
        peer.client.body_retirement = Some(gate.clone());
        let id = new_turn_id();
        let old = view(id, "retiring-body", false);
        let final_view = view(id, "final", true);
        let mut turn = peer.client.observe(
            old.session.session_id.clone(),
            Some(id),
            HistoryScope::SelectedTurn,
        );
        let (mut stream, _) = peer.request().await;
        snapshot(&mut stream, old.clone(), true).await;
        update_until(&mut turn, |u| matches!(u, LocalUpdate::Reset { .. })).await;
        let (mut body, request) = peer.request().await;
        assert!(
            matches!(request, Request::Transcript { commit_id, .. } if commit_id == "retiring-body")
        );
        reply(
            &mut stream,
            Response::Observation(Observation::Finished {
                view: Box::new(final_view.clone()),
                cursor: cursor(1),
                error: None,
            }),
        )
        .await;
        within(gate.retiring.notified()).await;
        let error = LocalError::new(
            LocalErrorKind::StoreUnavailable,
            "old fixed object unavailable",
        );
        reply(
            &mut body,
            if fails {
                Response::Error(error.clone())
            } else {
                Response::Transcript(TranscriptPage {
                    jsonl: String::new(),
                    next_offset: None,
                })
            },
        )
        .await;
        within(gate.read_done.notified()).await;
        gate.release.notify_one();
        // 生产退场处还核对 JoinHandle::is_finished，不能以已写回 socket 冒充任务完成。
        assert!(matches!(
            within(turn.next()).await,
            Some(LocalUpdate::State {
                phase: ObservationPhase::Finished,
                confirmation: CompletionConfirmation::Confirmed,
                ..
            })
        ));
        assert!(matches!(
            within(turn.next()).await,
            Some(LocalUpdate::Reset { .. })
        ));
        if fails {
            let update = within(turn.next()).await.unwrap();
            assert!(
                matches!(update, LocalUpdate::History { view: fixed, result: Err(HistoryReadError { context: HistoryReadContext::Transcript { object_id }, error: failure }), stale: true } if *fixed == old && object_id == "retiring-body" && failure == error)
            );
        }
        transcript(&peer, "final", "current final result").await;
        let update = within(turn.next()).await.unwrap();
        assert!(
            matches!(update, LocalUpdate::History { view, result: Ok(_), stale: false } if *view == final_view),
            "old success must never publish after a new cut"
        );
        let report = within(turn.detach()).await;
        assert_eq!(
            report.state.unwrap().confirmation,
            CompletionConfirmation::Confirmed
        );
        if fails {
            assert_eq!(
                report.history_error,
                Some(HistoryReadError {
                    context: HistoryReadContext::Transcript {
                        object_id: "retiring-body".into()
                    },
                    error
                })
            );
        } else {
            assert_eq!(report.history_error, None);
        }
    }
}

#[derive(Default)]
pub(super) struct RefixRetirementGate {
    pub retiring: tokio::sync::Notify,
    pub snapshot_queued: tokio::sync::Notify,
    pub release: tokio::sync::Notify,
}

impl RefixRetirementGate {
    pub fn matches(&self, observation: &Result<Observation>) -> bool {
        matches!(observation, Ok(Observation::Snapshot { view, owned: true, .. })
            if matches!(&view.requested_turn, Some(TurnObservation::Pending(turn))
                if turn.journal_oid == "retiring-live"))
    }
}

#[tokio::test]
async fn lc3_gap_refixes_live_suffix_and_snapshot_flood_keeps_each_finished() {
    for queue_live in [false, true] {
        gap_refix_and_snapshot_flood(queue_live).await;
    }
}

async fn gap_refix_and_snapshot_flood(queue_live: bool) {
    let mut peer = Peer::new();
    let received = std::sync::Arc::new(tokio::sync::Notify::new());
    peer.client.finished_received = Some(received.clone());
    let gate = std::sync::Arc::new(RefixRetirementGate::default());
    peer.client.refix_retirement = queue_live.then(|| gate.clone());
    let id = new_turn_id();
    let mut turn = peer.client.observe(
        view(id, "pending", false).session.session_id,
        Some(id),
        HistoryScope::SelectedTurn,
    );
    let (mut stream, _) = peer.request().await;
    snapshot(&mut stream, view(id, "pending", false), true).await;
    update_until(&mut turn, |u| matches!(u, LocalUpdate::Reset { .. })).await;
    transcript(&peer, "pending", "prefix").await;
    update_until(&mut turn, |u| matches!(u, LocalUpdate::History { .. })).await;
    for sequence in 1..=36 {
        reply(
            &mut stream,
            Response::Observation(Observation::Event {
                cursor: cursor(sequence),
                event: crate::TurnEvent::TokenDelta {
                    content: "overflow".into(),
                },
            }),
        )
        .await;
    }
    update_until(&mut turn, |u| {
        matches!(
            u,
            LocalUpdate::Reset {
                reason: ResetReason::Gap,
                ..
            }
        )
    })
    .await;
    if queue_live {
        within(gate.retiring.notified()).await;
        // Worker 已进入真实自动退场；reader 仍可解码并排队新的后台切点。
        let retiring_view = view(id, "retiring-live", false);
        reply(
            &mut stream,
            Response::Observation(Observation::Gap { cursor: cursor(37) }),
        )
        .await;
        reply(
            &mut stream,
            Response::Observation(Observation::Snapshot {
                view: Box::new(retiring_view.clone()),
                cursor: cursor(38),
                owned: true,
            }),
        )
        .await;
        within(gate.snapshot_queued.notified()).await;
        gate.release.notify_one();
        update_until(&mut turn, |u| {
            matches!(u, LocalUpdate::State { view, phase: ObservationPhase::Live, .. }
                if **view == retiring_view)
        })
        .await;
    }
    // 没有 Finished，也没有调用者 reconnect，库必须主动重新固定而非无限吞事件。
    let mut fresh = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let (mut stream, request) = peer.request().await;
            match request {
                Request::Subscribe {
                    turn_id,
                    cursor: None,
                    ..
                } if turn_id == id => break stream,
                Request::Transcript { commit_id, .. } if commit_id == "retiring-live" => {
                    reply(
                        &mut stream,
                        Response::Transcript(TranscriptPage {
                            jsonl: String::new(),
                            next_offset: None,
                        }),
                    )
                    .await;
                }
                _ => panic!("automatic refix must be read-only for the same Turn"),
            }
        }
    })
    .await
    .expect("CF1: closing a Live snapshot must be followed by a new read-only Subscribe");
    snapshot(&mut fresh, view(id, "refixed", false), true).await;
    update_until(&mut turn, |u| {
        matches!(u, LocalUpdate::State { view: fixed, phase: ObservationPhase::Live, .. }
            if **fixed == view(id, "refixed", false))
    })
    .await;
    update_until(&mut turn, |u| {
        matches!(
            u,
            LocalUpdate::Reset {
                reason: ResetReason::Snapshot,
                ..
            }
        )
    })
    .await;
    reply(
        &mut fresh,
        Response::Observation(Observation::Event {
            cursor: cursor(40),
            event: crate::TurnEvent::TokenDelta {
                content: "new suffix".into(),
            },
        }),
    )
    .await;
    assert!(matches!(
        within(turn.next()).await,
        Some(LocalUpdate::Event {
            text: TextAction::StartIncomplete,
            ..
        })
    ));
    transcript(&peer, "refixed", "saved prefix").await;
    assert!(matches!(
        within(turn.next()).await,
        Some(LocalUpdate::History { result: Ok(_), .. })
    ));
    for _ in 0..96 {
        snapshot(&mut fresh, view(id, "refixed", false), true).await;
    }
    reply(
        &mut fresh,
        Response::Observation(Observation::Finished {
            view: Box::new(view(id, "final", true)),
            cursor: cursor(41),
            error: None,
        }),
    )
    .await;
    within(received.notified()).await;
    if !queue_live {
        turn.reconnect();
    }
    let mut count = 0;
    loop {
        let update = within(turn.next()).await.unwrap();
        count += 1;
        assert!(count < 64, "snapshots cannot grow the UI backlog");
        if let LocalUpdate::State {
            phase: ObservationPhase::Finished,
            confirmation,
            ..
        } = update
        {
            assert_eq!(confirmation, CompletionConfirmation::Confirmed);
            break;
        }
    }
    if queue_live {
        // CF1 的 Event 和 Finished 均已在无显式 reconnect 的情况下交付。
        turn.reconnect();
    }
    let mut reconnected = loop {
        let (mut stream, request) = peer.request().await;
        match request {
            Request::Subscribe { .. } => break stream,
            Request::Transcript { .. } => {
                let _ = ipc::write(
                    &mut stream,
                    &Response::Transcript(TranscriptPage {
                        jsonl: String::new(),
                        next_offset: None,
                    }),
                )
                .await;
            }
            _ => panic!("read-only refix"),
        }
    };
    let error = LocalError::new(
        LocalErrorKind::StoreUnavailable,
        "new epoch confirmation unknown",
    );
    reply(
        &mut reconnected,
        Response::Observation(Observation::Finished {
            view: Box::new(view(id, "final", true)),
            cursor: EventCursor {
                epoch: "new".into(),
                sequence: 0,
            },
            error: Some(error.clone()),
        }),
    )
    .await;
    let update = update_until(&mut turn, |u| {
        matches!(
            u,
            LocalUpdate::State {
                phase: ObservationPhase::Finished,
                ..
            }
        )
    })
    .await;
    assert!(
        matches!(update, LocalUpdate::State { confirmation: CompletionConfirmation::Confirmed, error: Some(actual), .. } if actual == error)
    );
    within(turn.detach()).await;
}

#[tokio::test]
async fn lc2_sent_operation_rejection_still_ends_cancel_handoff() {
    let peer = Peer::new();
    let id = new_turn_id();
    let request = TurnRequest {
        session_id: view(id, "pending", false).session.session_id,
        turn_id: id,
        workspace: peer._directory.path().into(),
        input: "original".into(),
    };
    let configuration = super::tests::frozen(peer._directory.path(), "http://127.0.0.1:9/v1");
    let mut turn = peer
        .client
        .start_turn_with(request, HistoryScope::SelectedTurn, || async {
            Ok(configuration)
        });
    let mut subscription = None;
    let mut probe = None;
    for _ in 0..2 {
        let (stream, request) = peer.request().await;
        match request {
            Request::Subscribe { .. } => subscription = Some(stream),
            Request::Submit {
                configuration: None,
                ..
            } => probe = Some(stream),
            _ => panic!("initial operations"),
        }
    }
    reply(
        probe.as_mut().unwrap(),
        Response::Acceptance(Acceptance::NeedsConfiguration),
    )
    .await;
    let (mut sent, request) = peer.request().await;
    assert!(
        matches!(request, Request::Submit { request, configuration: Some(_), .. } if request.turn_id == id)
    );
    let rejected = LocalError::new(LocalErrorKind::BadRequest, "configured operation rejected");
    reply(&mut sent, Response::Error(rejected.clone())).await;
    let update = update_until(&mut turn, |u| matches!(u, LocalUpdate::Submission { .. })).await;
    assert!(
        matches!(update, LocalUpdate::Submission { result: Err(error), .. } if error == rejected)
    );
    turn.cancel();
    update_until(&mut turn, |u| {
        matches!(
            u,
            LocalUpdate::Cancel {
                status: CancelStatus::Moot,
                ..
            }
        )
    })
    .await;
    let report = within(turn.detach()).await;
    assert_eq!(report.submission, Some(Err(rejected)));
    assert_eq!(report.cancel, Some(CancelStatus::Moot));
    assert!(
        tokio::time::timeout(Duration::from_millis(50), peer.listener.accept())
            .await
            .is_err(),
        "no Cancel or resubmission after explicit rejection"
    );
    drop(subscription);
}

#[tokio::test]
async fn late_submission_transport_is_nonterminal_but_backend_errors_are_not() {
    for configured in [false, true] {
        for backend_kind in [
            None,
            Some(LocalErrorKind::BadRequest),
            Some(LocalErrorKind::Transport),
        ] {
            let peer = Peer::new();
            let id = new_turn_id();
            let original = TurnRequest {
                session_id: view(id, "pending", false).session.session_id,
                turn_id: id,
                workspace: peer._directory.path().into(),
                input: "original".into(),
            };
            let configuration =
                super::tests::frozen(peer._directory.path(), "http://127.0.0.1:9/v1");
            let mut turn = peer.client.start_turn_with(
                original.clone(),
                HistoryScope::SelectedTurn,
                move || async move {
                    assert!(configured, "probe must not load configuration");
                    Ok(configuration)
                },
            );
            let mut submit = None;
            let mut subscription = None;
            for _ in 0..2 {
                let (stream, request) = peer.request().await;
                match request {
                    Request::Submit {
                        request,
                        configuration: None,
                    } => {
                        assert!(request == original);
                        submit = Some(stream);
                    }
                    Request::Subscribe {
                        session_id,
                        turn_id,
                        cursor: None,
                    } => {
                        assert_eq!(session_id, original.session_id);
                        assert_eq!(turn_id, id);
                        subscription = Some(stream);
                    }
                    _ => panic!("initial requests only"),
                }
            }
            let mut submit = submit.unwrap();
            if configured {
                reply(
                    &mut submit,
                    Response::Acceptance(Acceptance::NeedsConfiguration),
                )
                .await;
                let (sent, request) = peer.request().await;
                assert!(
                    matches!(request, Request::Submit { request, configuration: Some(_) } if request == original)
                );
                submit = sent;
            }
            snapshot(
                subscription.as_mut().unwrap(),
                view(id, "pending", false),
                true,
            )
            .await;
            transcript(&peer, "pending", "saved").await;
            update_until(&mut turn, |u| matches!(u, LocalUpdate::History { .. })).await;
            turn.cancel();
            let mut cancel = cancel_request(&peer, id).await;
            reply(&mut cancel, Response::Cancelled { owned: true }).await;
            update_until(&mut turn, |u| {
                matches!(
                    u,
                    LocalUpdate::Cancel {
                        status: CancelStatus::Taken,
                        ..
                    }
                )
            })
            .await;
            tokio::time::sleep(Duration::from_secs(4)).await;
            let (mut refixed, request) = peer.request().await;
            assert!(
                matches!(request, Request::Subscribe { session_id, turn_id, cursor: None } if session_id == original.session_id && turn_id == id)
            );
            assert!(matches!(
                update_until(&mut turn, |u| matches!(u, LocalUpdate::SubmissionUnknown { .. })).await,
                LocalUpdate::SubmissionUnknown { turn_id, error: None } if turn_id == id
            ));
            let fixed = view(id, if configured { "final" } else { "live" }, configured);
            let phase = if configured {
                ObservationPhase::Finished
            } else {
                ObservationPhase::Live
            };
            let observation = if configured {
                Observation::Finished {
                    view: Box::new(fixed.clone()),
                    cursor: cursor(2),
                    error: None,
                }
            } else {
                Observation::Snapshot {
                    view: Box::new(fixed.clone()),
                    cursor: cursor(2),
                    owned: true,
                }
            };
            reply(&mut refixed, Response::Observation(observation)).await;
            update_until(
                &mut turn,
                |u| matches!(u, LocalUpdate::State { phase: actual, .. } if *actual == phase),
            )
            .await;
            let (_body, request) = peer.request().await;
            assert!(matches!(request, Request::Transcript { .. }));
            let backend_error =
                backend_kind.map(|kind| LocalError::new(kind, "backend submit error"));
            if let Some(error) = &backend_error {
                reply(&mut submit, Response::Error(error.clone())).await;
            }
            drop(submit);
            let update = update_until(&mut turn, |u| {
                matches!(
                    u,
                    LocalUpdate::Submission { .. } | LocalUpdate::SubmissionUnknown { .. }
                )
            })
            .await;
            let diagnostic = match (update, &backend_error) {
                (
                    LocalUpdate::SubmissionUnknown {
                        turn_id,
                        error: Some(error),
                    },
                    None,
                ) => {
                    assert_eq!(turn_id, id);
                    assert_eq!(error.kind, LocalErrorKind::Transport);
                    Some(error)
                }
                (
                    LocalUpdate::Submission {
                        turn_id,
                        result: Err(error),
                    },
                    Some(expected),
                ) => {
                    assert_eq!(turn_id, id);
                    assert_eq!(&error, expected);
                    None
                }
                other => panic!("submission source must determine update semantics: {other:?}"),
            };
            if !configured && backend_error.is_none() {
                reply(
                    &mut refixed,
                    Response::Observation(Observation::Event {
                        cursor: cursor(3),
                        event: crate::TurnEvent::TokenDelta {
                            content: "after receipt failure".into(),
                        },
                    }),
                )
                .await;
                assert!(matches!(
                    within(turn.next()).await,
                    Some(LocalUpdate::Event { event: crate::TurnEvent::TokenDelta { content }, .. }) if content == "after receipt failure"
                ));
            }
            let report = within(turn.detach()).await;
            assert_eq!(report.submission, backend_error.map(Err));
            assert_eq!(report.submission_diagnostic, diagnostic);
            assert_eq!(report.not_submitted, None);
            assert_eq!(report.cancel, Some(CancelStatus::Taken));
            let state = report.state.unwrap();
            assert_eq!(*state.view, fixed);
            assert_eq!(state.phase, phase);
            assert_eq!(
                state.confirmation,
                if configured {
                    CompletionConfirmation::Confirmed
                } else {
                    CompletionConfirmation::Unknown
                }
            );
            assert!(
                tokio::time::timeout(Duration::from_millis(50), peer.listener.accept())
                    .await
                    .is_err(),
                "no resubmission, reconnect or extra Cancel"
            );
        }
    }
}

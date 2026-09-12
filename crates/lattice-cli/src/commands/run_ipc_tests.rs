// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

const SESSION: &str = "11111111111141118111111111111111";
const PENDING: &str = "1111111111111111111111111111111111111111";
const COMMIT: &str = "2222222222222222222222222222222222222222";

struct Peer {
    _root: tempfile::TempDir,
    listener: UnixListener,
    client: LocalClient,
}

impl Peer {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let layout = lattice_api::StateLayout::in_state_root(root.path());
        let local = layout.lattice_root().join("local");
        std::fs::create_dir_all(&local).unwrap();
        std::fs::set_permissions(&local, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = local.join("api.sock");
        let listener = UnixListener::bind(&path).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        Self {
            _root: root,
            listener,
            client: LocalClient::new(layout),
        }
    }

    async fn request(&self, operation: &str) -> (UnixStream, Value) {
        let (stream, request) = self.request_any().await;
        assert_eq!(request["operation"], operation);
        (stream, request)
    }

    async fn request_any(&self) -> (UnixStream, Value) {
        let (mut stream, _) = self.listener.accept().await.unwrap();
        let mut magic = [0; 8];
        stream.read_exact(&mut magic).await.unwrap();
        assert_eq!(&magic, b"LTLOCAL2");
        let len = stream.read_u32().await.unwrap();
        assert!(len <= 16 * 1024 * 1024);
        let mut bytes = vec![0; len as usize];
        stream.read_exact(&mut bytes).await.unwrap();
        (stream, serde_json::from_slice(&bytes).unwrap())
    }
}

async fn reply(stream: &mut UnixStream, response: &str, value: Value) {
    let bytes = serde_json::to_vec(&json!({"response": response, "value": value})).unwrap();
    stream.write_all(b"LTLOCAL2").await.unwrap();
    stream.write_u32(bytes.len() as u32).await.unwrap();
    stream.write_all(&bytes).await.unwrap();
}

fn view(turn_id: TurnId, committed: bool) -> SessionObservation {
    let pending = lattice_api::local::PendingObservation {
        turn_id,
        journal_oid: PENDING.into(),
    };
    SessionObservation {
        state_oid: "3333333333333333333333333333333333333333".into(),
        session: lattice_api::GraphSession {
            session_id: SESSION.into(),
            label: None,
            active_agent_name: "unused".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
            project_target_branch: Some("main".into()),
            project_store_key: "0".repeat(64),
        },
        head: lattice_api::GraphHead::Branch {
            name: "main".into(),
        },
        head_commit_id: committed.then(|| COMMIT.into()),
        branches: vec![],
        commit_count: usize::from(committed),
        active_turn: (!committed).then(|| pending.clone()),
        requested_turn: Some(if committed {
            TurnObservation::Committed(Box::new(lattice_api::local::CommittedTurnObservation {
                commit_id: COMMIT.into(),
                turn_id,
                parent_ids: vec![],
                agent_name: "unused".into(),
                model: "unused".into(),
                provider: "unused".into(),
                outcome: TurnOutcome::Completed {
                    finish_reason: lattice_api::TurnFinishReason::Stop,
                },
                origin: lattice_api::ClosureOrigin::Live,
                usage: None,
                workspace_history_checkpoint: lattice_api::WorkspaceHistoryCheckpoint::Empty,
                created_at: "2026-01-01T00:00:00Z".into(),
            }))
        } else {
            TurnObservation::Pending(pending)
        }),
    }
}

async fn snapshot(stream: &mut UnixStream, view: SessionObservation, finished: bool) {
    let observation = if finished {
        json!({"type":"finished", "view":view, "cursor":{"epoch":"peer", "sequence":2}, "error":null})
    } else {
        json!({"type":"snapshot", "view":view, "cursor":{"epoch":"peer", "sequence":1}, "owned":true})
    };
    reply(stream, "observation", observation).await;
}

/// 客户端读完回复即断开单次连接；对端看到 EOF 就能确定该次读取已结束或被中止。
async fn closed(stream: &mut UnixStream) {
    assert_eq!(stream.read(&mut [0; 1]).await.unwrap(), 0);
}

/// tokio 的 SIGINT 监听是进程共享的；持有信号流或向自身发信号的测试必须串行。
pub(super) async fn signal_guard() -> tokio::sync::MutexGuard<'static, ()> {
    static SIGNALS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    SIGNALS.lock().await
}

async fn run_observe<D: std::future::Future<Output = ()>>(
    driver: D,
    observe: impl std::future::Future<Output = Result<std::process::ExitCode>>,
    expected: u8,
) {
    time::timeout(Duration::from_secs(5), async {
        tokio::pin!(driver, observe);
        tokio::select! {
            result = &mut observe => {
                assert!(result.is_ok(), "真实观察循环不得以错误结束: {result:?}");
                driver.await;
                assert_eq!(result.unwrap(), std::process::ExitCode::from(expected));
            }
            _ = &mut driver => {
                assert_eq!(observe.await.unwrap(), std::process::ExitCode::from(expected));
            }
        }
    })
    .await
    .expect("真实观察循环需在时限内处理控制消息");
}

#[tokio::test]
async fn pending_body_error_does_not_discard_finished() {
    let _serial = signal_guard().await;
    let peer = Peer::new();
    let turn = lattice_api::local::new_turn_id();
    let driver = async {
        let (mut subscription, _) = peer.request("subscribe").await;
        snapshot(&mut subscription, view(turn, false), false).await;
        let (mut body, request) = peer.request("transcript").await;
        assert_eq!(request["commit_id"], PENDING);
        reply(
            &mut body,
            "error",
            json!({"kind":"store_unavailable", "message":"body unavailable", "recovery":null}),
        )
        .await;
        closed(&mut body).await;
        snapshot(&mut subscription, view(turn, true), true).await;
        let (mut body, request) = peer.request("transcript").await;
        assert_eq!(request["commit_id"], COMMIT);
        reply(
            &mut body,
            "transcript",
            json!({"jsonl":"", "next_offset":null}),
        )
        .await;
    };
    run_observe(
        driver,
        observe_turn(&peer.client, SESSION, turn, true, false, true),
        1,
    )
    .await;
}

#[tokio::test]
async fn stalled_pending_body_does_not_block_finished_and_is_aborted() {
    let _serial = signal_guard().await;
    let peer = Peer::new();
    let turn = lattice_api::local::new_turn_id();
    let driver = async {
        let (mut subscription, _) = peer.request("subscribe").await;
        snapshot(&mut subscription, view(turn, false), false).await;
        let (mut stalled, request) = peer.request("transcript").await;
        assert_eq!(request["commit_id"], PENDING);
        // pending 正文永不回复；控制循环仍须处理 Finished 并读取最终正文。
        snapshot(&mut subscription, view(turn, true), true).await;
        let (mut body, request) = peer.request("transcript").await;
        assert_eq!(request["commit_id"], COMMIT);
        closed(&mut stalled).await;
        reply(
            &mut body,
            "transcript",
            json!({"jsonl":"", "next_offset":null}),
        )
        .await;
    };
    run_observe(
        driver,
        observe_turn(&peer.client, SESSION, turn, true, false, true),
        0,
    )
    .await;
}

/// 向测试进程自身发送真实 SIGINT；`run_session_prompt` 已登记 tokio 信号流，进程不会因此退出。
fn interrupt() {
    let status = std::process::Command::new("kill")
        .args(["-INT", &std::process::id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
}

async fn open_session(peer: &Peer, turn: TurnId) {
    let (mut open, _) = peer.request("open").await;
    reply(
        &mut open,
        "session",
        serde_json::to_value(view(turn, false).session).unwrap(),
    )
    .await;
}

async fn cancel_request(peer: &Peer, turn: TurnId, owned: bool) {
    let (mut cancel, request) = peer.request("cancel").await;
    assert_eq!(request["turn_id"], json!(turn));
    assert_eq!(request["session_id"], SESSION);
    reply(&mut cancel, "cancelled", json!({"owned": owned})).await;
}

fn send(
    peer: &Peer,
    turn: TurnId,
) -> impl std::future::Future<Output = Result<std::process::ExitCode>> + '_ {
    run_session_prompt(
        &peer.client,
        "prompt".into(),
        SESSION.into(),
        Some(turn),
        false,
        true,
    )
}

#[tokio::test]
async fn sigint_during_submit_keeps_identity_and_cancels_the_accepted_turn() {
    let _serial = signal_guard().await;
    let peer = Peer::new();
    let turn = lattice_api::local::new_turn_id();
    let driver = async {
        open_session(&peer, turn).await;
        let (mut submit, request) = peer.request("submit").await;
        assert_eq!(request["request"]["turn_id"], json!(turn));
        interrupt();
        // 取消先于所有者登记返回 false；随后同一提交仍被接受，客户端必须对同一 TurnId 再取消。
        cancel_request(&peer, turn, false).await;
        reply(
            &mut submit,
            "acceptance",
            json!({"acceptance":"accepted", "turn_id":turn}),
        )
        .await;
        cancel_request(&peer, turn, true).await;
        let (mut subscription, request) = peer.request("subscribe").await;
        assert_eq!(request["turn_id"], json!(turn));
        snapshot(&mut subscription, view(turn, true), true).await;
        let (mut body, request) = peer.request("transcript").await;
        assert_eq!(request["commit_id"], COMMIT);
        reply(
            &mut body,
            "transcript",
            json!({"jsonl":"", "next_offset":null}),
        )
        .await;
    };
    run_observe(driver, send(&peer, turn), 0).await;
}

#[tokio::test]
async fn sigint_before_acceptance_does_not_submit_configuration_or_observe() {
    let _serial = signal_guard().await;
    let peer = Peer::new();
    let turn = lattice_api::local::new_turn_id();
    let driver = async {
        open_session(&peer, turn).await;
        let (mut submit, request) = peer.request("submit").await;
        assert!(request["configuration"].is_null());
        interrupt();
        cancel_request(&peer, turn, false).await;
        reply(
            &mut submit,
            "acceptance",
            json!({"acceptance":"needs_configuration"}),
        )
        .await;
    };
    let exit = time::timeout(Duration::from_secs(5), async {
        let (exit, ()) = tokio::join!(send(&peer, turn), driver);
        exit.unwrap()
    })
    .await
    .expect("未接受的请求在中断后必须有界结束");
    assert_eq!(exit, std::process::ExitCode::from(1));
    // 已取消的未发送请求不得再配置提交，也不得进入观察。
    assert!(
        time::timeout(Duration::from_millis(100), peer.listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn second_sigint_while_submission_unknown_exits_without_new_identity() {
    let _serial = signal_guard().await;
    let peer = Peer::new();
    let turn = lattice_api::local::new_turn_id();
    let driver = async {
        open_session(&peer, turn).await;
        let (mut submit, _) = peer.request("submit").await;
        interrupt();
        cancel_request(&peer, turn, false).await;
        // 后台始终不回执；第二次 Ctrl+C 只结束等待，提交连接随之关闭。
        interrupt();
        closed(&mut submit).await;
    };
    let exit = time::timeout(Duration::from_secs(5), async {
        let (exit, ()) = tokio::join!(send(&peer, turn), driver);
        exit.unwrap()
    })
    .await
    .expect("提交结果未知时第二次中断必须有界退出");
    assert_eq!(exit, std::process::ExitCode::from(1));
    assert!(
        time::timeout(Duration::from_millis(100), peer.listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn pending_cancel_intent_is_resent_once_a_live_owner_is_observed() {
    let _serial = signal_guard().await;
    let peer = Peer::new();
    let turn = lattice_api::local::new_turn_id();
    let driver = async {
        let (mut subscription, request) = peer.request("subscribe").await;
        assert_eq!(request["turn_id"], json!(turn));
        // 提交回执丢失后只带着未确认的取消意图观察；owned Snapshot 证明本 Turn 有 live 所有者，才补发同 TurnId 取消。
        snapshot(&mut subscription, view(turn, false), false).await;
        let mut cancelled = false;
        for _ in 0..2 {
            let (mut stream, request) = peer.request_any().await;
            match request["operation"].as_str().unwrap() {
                "cancel" => {
                    assert_eq!(request["turn_id"], json!(turn));
                    reply(&mut stream, "cancelled", json!({"owned": true})).await;
                    cancelled = true;
                }
                "transcript" => {
                    assert_eq!(request["commit_id"], PENDING);
                    reply(
                        &mut stream,
                        "transcript",
                        json!({"jsonl":"", "next_offset":null}),
                    )
                    .await;
                }
                other => panic!("unexpected operation {other}"),
            }
        }
        assert!(cancelled, "owned Snapshot 后必须补发取消");
        snapshot(&mut subscription, view(turn, true), true).await;
        let (mut body, request) = peer.request("transcript").await;
        assert_eq!(request["commit_id"], COMMIT);
        reply(
            &mut body,
            "transcript",
            json!({"jsonl":"", "next_offset":null}),
        )
        .await;
    };
    let sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()).unwrap();
    let observe = observe_with_signal(
        &peer.client,
        SESSION,
        turn,
        false,
        true,
        sigint,
        CancelIntent::Pending,
    );
    run_observe(driver, observe, 0).await;
}

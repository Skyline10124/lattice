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
        0,
    )
    .await;
}

#[tokio::test]
async fn reconnected_body_read_uses_final_result_for_exit() {
    let _serial = signal_guard().await;
    for final_body_available in [true, false] {
        let peer = Peer::new();
        let turn = lattice_api::local::new_turn_id();
        let driver = async {
            let (mut submit, mut subscription) = begin_submit(&peer, turn).await;
            snapshot(&mut subscription, view(turn, false), false).await;
            let (mut body, request) = peer.request("transcript").await;
            assert_eq!(request["commit_id"], PENDING);
            // 正文连接真实 EOF；接受回执稍后到达，触发同一任务的只读重连。
            body.shutdown().await.unwrap();
            closed(&mut body).await;
            reply(
                &mut submit,
                "acceptance",
                json!({"acceptance":"existing", "turn":view(turn, false).requested_turn.unwrap()}),
            )
            .await;
            let (mut reconnected, request) = peer.request("subscribe").await;
            assert_eq!(request["turn_id"], json!(turn));
            assert_eq!(request["session_id"], SESSION);
            snapshot(&mut reconnected, view(turn, true), true).await;
            let (mut body, request) = peer.request("transcript").await;
            assert_eq!(request["commit_id"], COMMIT);
            if final_body_available {
                let message = lattice_session::test_helpers::assistant_msg("重连后读取到的回答");
                reply(
                    &mut body,
                    "transcript",
                    json!({"jsonl":serde_json::to_string(&message).unwrap() + "\n", "next_offset":null}),
                )
                .await;
            } else {
                body.shutdown().await.unwrap();
            }
            closed(&mut body).await;
        };
        run_observe(
            driver,
            send(&peer, turn),
            if final_body_available { 0 } else { 1 },
        )
        .await;
    }
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
#[tokio::test]
async fn cancel_error_received_during_detach_is_rendered_with_original_identity() {
    const MODE: &str = "LATTICE_TEST_DETACH_DIAGNOSTIC";
    const ID: &str = "22222222-2222-4222-8222-222222222222";
    const MESSAGE: &str = "cancel denied during detach";
    if let Ok(mode) = std::env::var(MODE) {
        let _serial = signal_guard().await;
        let peer = Peer::new();
        let id: TurnId = ID.parse().unwrap();
        let driver = async {
            let (mut subscription, _) = peer.request("subscribe").await;
            snapshot(&mut subscription, view(id, false), false).await;
            let (mut body, _) = peer.request("transcript").await;
            interrupt();
            let (mut first, request) = peer.request("cancel").await;
            assert_eq!(request["turn_id"], ID);
            interrupt();
            // detach 已主动结束正文后才释放控制错误，UI next 不再参与交付。
            closed(&mut body).await;
            let error = json!({"kind":"permission", "message":MESSAGE, "recovery":null});
            reply(&mut first, "error", error.clone()).await;
            let (mut retry, request) = peer.request("cancel").await;
            assert_eq!(request["turn_id"], ID);
            reply(&mut retry, "error", error).await;
            closed(&mut retry).await;
            assert!(
                time::timeout(Duration::from_millis(50), peer.listener.accept())
                    .await
                    .is_err()
            );
        };
        run_observe(
            driver,
            observe_turn(&peer.client, SESSION, id, true, false, mode == "json"),
            1,
        )
        .await;
        return;
    }
    // 同一真实 CLI 观察循环在隔离子测试进程中运行，才能断言 stdout/stderr 实际接线。
    for mode in ["json", "human"] {
        let root = tempfile::tempdir().unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        child.env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", root.path())
            .env("XDG_CONFIG_HOME", root.path().join("config"))
            .env("XDG_STATE_HOME", root.path().join("state"))
            .env("XDG_CACHE_HOME", root.path().join("cache"))
            .env("XDG_RUNTIME_DIR", root.path().join("runtime"))
            .env(MODE, mode)
            .args(["--exact", "commands::run::ipc_tests::cancel_error_received_during_detach_is_rendered_with_original_identity", "--nocapture"]);
        let output = tokio::task::spawn_blocking(move || child.output().unwrap())
            .await
            .unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(output.status.success(), "{stdout}\n{stderr}");
        if mode == "json" {
            let records: Vec<Value> = stdout
                .lines()
                .filter_map(|line| serde_json::from_str(line).ok())
                .collect();
            assert_eq!(
                records
                    .iter()
                    .filter(|record| record["type"] == "error"
                        && record["kind"] == "permission"
                        && record["message"] == MESSAGE)
                    .count(),
                1
            );
            assert!(records.iter().any(|record| {
                record["type"] == "connection"
                    && record["message"].as_str().is_some_and(|message| {
                        message.contains("取消诊断")
                            && message.contains(SESSION)
                            && message.contains(ID)
                    })
            }));
        } else {
            assert_eq!(stderr.matches(MESSAGE).count(), 1);
            assert!(stderr.lines().any(|line| line.contains("取消诊断")
                && line.contains(SESSION)
                && line.contains(ID)));
        }
    }
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

/// 提交入口同时开始只读观察；两个独立 IPC 的到达顺序不属于前端契约。
async fn begin_submit(peer: &Peer, turn: TurnId) -> (UnixStream, UnixStream) {
    open_session(peer, turn).await;
    let mut submit = None;
    let mut subscription = None;
    for _ in 0..2 {
        let (stream, request) = peer.request_any().await;
        match request["operation"].as_str().unwrap() {
            "submit" => {
                assert!(submit.is_none());
                assert_eq!(request["request"]["turn_id"], json!(turn));
                assert_eq!(request["request"]["session_id"], SESSION);
                assert_eq!(request["request"]["input"], "prompt");
                assert!(request["configuration"].is_null());
                submit = Some(stream);
            }
            "subscribe" => {
                assert!(subscription.is_none());
                assert_eq!(request["turn_id"], json!(turn));
                assert_eq!(request["session_id"], SESSION);
                assert!(request["cursor"].is_null());
                subscription = Some(stream);
            }
            other => panic!("unexpected initial operation {other}"),
        }
    }
    (submit.unwrap(), subscription.unwrap())
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
        let (mut submit, mut initial) = begin_submit(&peer, turn).await;
        interrupt();
        // 取消先于所有者登记返回 false；随后同一提交仍被接受，客户端必须对同一 TurnId 再取消。
        cancel_request(&peer, turn, false).await;
        reply(
            &mut submit,
            "acceptance",
            json!({"acceptance":"accepted", "turn_id":turn}),
        )
        .await;
        closed(&mut initial).await;
        let mut subscription = None;
        let mut cancelled = false;
        for _ in 0..2 {
            let (mut stream, request) = peer.request_any().await;
            assert_eq!(request["turn_id"], json!(turn));
            assert_eq!(request["session_id"], SESSION);
            match request["operation"].as_str().unwrap() {
                "cancel" => {
                    assert!(!cancelled);
                    reply(&mut stream, "cancelled", json!({"owned":true})).await;
                    cancelled = true;
                }
                "subscribe" => {
                    assert!(subscription.is_none());
                    subscription = Some(stream);
                }
                other => panic!("unexpected handoff operation {other}"),
            }
        }
        assert!(cancelled);
        let mut subscription = subscription.unwrap();
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
        let (mut submit, _initial) = begin_submit(&peer, turn).await;
        interrupt();
        cancel_request(&peer, turn, false).await;
        reply(
            &mut submit,
            "acceptance",
            json!({"acceptance":"needs_configuration"}),
        )
        .await;
    };
    // LocalTurn 的退出交接窗口为五秒；外层只增加一秒调度余量，不延长产品窗口。
    let exit = time::timeout(Duration::from_secs(6), async {
        let (exit, ()) = tokio::join!(send(&peer, turn), driver);
        exit.unwrap()
    })
    .await
    .expect("未接受的请求在中断后必须有界结束");
    assert_eq!(exit, std::process::ExitCode::from(1));
    // 接受前的共同只读观察已核对；退出后不得再配置提交或建立新观察。
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
        let (mut submit, _initial) = begin_submit(&peer, turn).await;
        interrupt();
        cancel_request(&peer, turn, false).await;
        // 后台始终不回执；第二次 Ctrl+C 进入共同五秒 detach，不再自行丢弃提交 future。
        interrupt();
        closed(&mut submit).await;
    };
    let exit = time::timeout(Duration::from_secs(6), async {
        let (exit, ()) = tokio::join!(send(&peer, turn), driver);
        exit.unwrap()
    })
    .await
    .expect("提交结果未知时第二次中断必须经有限交接退出");
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
        // 真实中断先得到未接管回执，随后 owned Snapshot 触发共同客户端的一次交接。
        interrupt();
        cancel_request(&peer, turn, false).await;
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
    run_observe(
        driver,
        observe_turn(&peer.client, SESSION, turn, true, false, true),
        0,
    )
    .await;
}

async fn empty_transcript(stream: &mut UnixStream) {
    reply(
        stream,
        "transcript",
        json!({"jsonl":"", "next_offset":null}),
    )
    .await;
}

#[tokio::test]
async fn sigint_late_not_taken_after_owned_snapshot_is_handed_off() {
    let _serial = signal_guard().await;
    let peer = Peer::new();
    let turn = lattice_api::local::new_turn_id();
    let driver = async {
        let (mut subscription, request) = peer.request("subscribe").await;
        assert_eq!(request["turn_id"], json!(turn));
        snapshot(&mut subscription, view(turn, false), false).await;
        let (mut body, request) = peer.request("transcript").await;
        assert_eq!(request["commit_id"], PENDING);
        empty_transcript(&mut body).await;
        closed(&mut body).await;
        interrupt();
        let (mut cancel, request) = peer.request("cancel").await;
        assert_eq!(request["turn_id"], json!(turn));
        assert_eq!(request["session_id"], SESSION);
        // 回执还在途时已看到 owned；最后到达的 false 仍须触发同一意图的有限交接。
        snapshot(&mut subscription, view(turn, false), false).await;
        let (mut body, request) = peer.request("transcript").await;
        assert_eq!(request["commit_id"], PENDING);
        empty_transcript(&mut body).await;
        reply(&mut cancel, "cancelled", json!({"owned":false})).await;
        cancel_request(&peer, turn, true).await;
        snapshot(&mut subscription, view(turn, true), true).await;
        let (mut body, request) = peer.request("transcript").await;
        assert_eq!(request["commit_id"], COMMIT);
        empty_transcript(&mut body).await;
    };
    run_observe(
        driver,
        observe_turn(&peer.client, SESSION, turn, true, false, true),
        0,
    )
    .await;
    assert!(
        time::timeout(Duration::from_millis(100), peer.listener.accept())
            .await
            .is_err(),
        "Taken 后不得第三次补发或重新提交"
    );
}

#[tokio::test]
async fn finished_body_interrupt_keeps_result_and_does_not_cancel() {
    let _serial = signal_guard().await;
    let peer = Peer::new();
    let turn = lattice_api::local::new_turn_id();
    let driver = async {
        let (mut subscription, _) = peer.request("subscribe").await;
        snapshot(&mut subscription, view(turn, false), false).await;
        let (mut pending, request) = peer.request("transcript").await;
        assert_eq!(request["commit_id"], PENDING);
        snapshot(&mut subscription, view(turn, true), true).await;
        let (mut final_body, request) = peer.request("transcript").await;
        assert_eq!(request["commit_id"], COMMIT);
        closed(&mut pending).await;
        interrupt();
        closed(&mut final_body).await;
    };
    run_observe(
        driver,
        observe_turn(&peer.client, SESSION, turn, true, false, true),
        1,
    )
    .await;
    assert!(
        time::timeout(Duration::from_millis(100), peer.listener.accept())
            .await
            .is_err(),
        "已完成后中断只放弃正文，不发送 Cancel 或重新执行"
    );
}

#[tokio::test]
async fn query_reads_its_fixed_view_and_body_not_found_is_exit_one() {
    for fail_body in [false, true] {
        let peer = Peer::new();
        let turn = lattice_api::local::new_turn_id();
        let mut fixed = view(turn, true);
        if let Some(TurnObservation::Committed(saved)) = &mut fixed.requested_turn {
            saved.outcome = TurnOutcome::Cancelled {
                reason: lattice_session::pending::TurnCancellation::UserCancelled,
            };
        }
        let driver = async {
            let (mut query, request) = peer.request("query").await;
            assert_eq!(request["turn_id"], json!(turn));
            reply(&mut query, "view", serde_json::to_value(&fixed).unwrap()).await;
            let (mut body, request) = peer.request("transcript").await;
            assert_eq!(request["commit_id"], COMMIT);
            if fail_body {
                reply(
                    &mut body,
                    "error",
                    json!({"kind":"not_found", "message":"fixed body unavailable", "recovery":null}),
                )
                .await;
            } else {
                empty_transcript(&mut body).await;
            }
        };
        time::timeout(Duration::from_secs(5), async {
            let (result, ()) = tokio::join!(
                observe_turn(&peer.client, SESSION, turn, false, false, true),
                driver
            );
            if fail_body {
                let error = result.unwrap_err();
                let (code, record) = error_record(&error);
                assert_eq!(code, 1);
                assert_eq!(record["kind"], "not_found");
                assert_eq!(report_error(&error, true), std::process::ExitCode::from(1));
            } else {
                assert_eq!(
                    result.unwrap(),
                    std::process::ExitCode::SUCCESS,
                    "纯查询成功不要求业务 outcome 成功或完成确认"
                );
            }
        })
        .await
        .unwrap();
        assert!(
            time::timeout(Duration::from_millis(100), peer.listener.accept())
                .await
                .is_err(),
            "固定读取不得再次 query、订阅或提交"
        );
    }
}

#[tokio::test]
async fn committed_snapshot_before_acceptance_waits_for_confirmation() {
    let _serial = signal_guard().await;
    let peer = Peer::new();
    let turn = lattice_api::local::new_turn_id();
    let driver = async {
        let (mut submit, mut subscription) = begin_submit(&peer, turn).await;
        let fixed = view(turn, true);
        // 原 Owner 仍在收尾时，已可见的 Committed 结果只以 owned 快照交付；
        // 该快照不是独立 completion。
        reply(
            &mut subscription,
            "observation",
            json!({"type":"snapshot", "view":fixed,
            "cursor":{"epoch":"peer", "sequence":0}, "owned":true}),
        )
        .await;
        let (mut body, request) = peer.request("transcript").await;
        assert_eq!(request["commit_id"], COMMIT);
        empty_transcript(&mut body).await;
        closed(&mut body).await;
        assert!(
            time::timeout(Duration::from_millis(100), submit.read(&mut [0]))
                .await
                .is_err(),
            "在途 Owner 的可见结果不能使 CLI 在原提交回执到达前 detach"
        );
        reply(
            &mut submit,
            "acceptance",
            json!({"acceptance":"existing", "turn":fixed.requested_turn}),
        )
        .await;
        closed(&mut submit).await;
        let (mut refixed, request) = peer.request("subscribe").await;
        assert_eq!(request["turn_id"], json!(turn));
        assert_eq!(request["session_id"], SESSION);
        assert!(request["cursor"].is_null());
        snapshot(&mut refixed, fixed, true).await;
        let (mut body, request) = peer.request("transcript").await;
        assert_eq!(request["commit_id"], COMMIT);
        empty_transcript(&mut body).await;
    };
    run_observe(driver, send(&peer, turn), 0).await;
    assert!(
        time::timeout(Duration::from_millis(100), peer.listener.accept())
            .await
            .is_err(),
        "原身份找回不得再提交配置、发送 Cancel 或额外重订阅"
    );
}

#[tokio::test]
async fn finished_without_submission_receipt_does_not_wait_for_acceptance() {
    let _serial = signal_guard().await;
    for failed in [false, true] {
        let peer = Peer::new();
        let turn = lattice_api::local::new_turn_id();
        let driver = async {
            let (mut submit, mut subscription) = begin_submit(&peer, turn).await;
            let error = failed.then(|| {
                json!({"kind":"store_unavailable",
                "message":"store unavailable; inspect durable state", "recovery":null})
            });
            reply(
                &mut subscription,
                "observation",
                json!({"type":"finished", "view":view(turn, true),
                    "cursor":{"epoch":"peer", "sequence":2}, "error":error}),
            )
            .await;
            let (mut body, request) = peer.request("transcript").await;
            assert_eq!(request["commit_id"], COMMIT);
            empty_transcript(&mut body).await;
            // 真正完成的成功或失败都不依赖原提交回执；始终不给 acceptance。
            closed(&mut submit).await;
        };
        run_observe(driver, send(&peer, turn), u8::from(failed)).await;
    }
}

#[tokio::test]
async fn late_needs_configuration_keeps_taken_and_stops_only_local_submission() {
    let _serial = signal_guard().await;
    let peer = Peer::new();
    let turn = lattice_api::local::new_turn_id();
    let driver = async {
        let (mut submit, mut subscription) = begin_submit(&peer, turn).await;
        snapshot(&mut subscription, view(turn, false), false).await;
        let (mut body, request) = peer.request("transcript").await;
        assert_eq!(request["commit_id"], PENDING);
        empty_transcript(&mut body).await;
        closed(&mut body).await;
        interrupt();
        let (mut cancel, request) = peer.request("cancel").await;
        assert_eq!(request["turn_id"], json!(turn));
        reply(&mut cancel, "cancelled", json!({"owned":true})).await;
        closed(&mut cancel).await;
        reply(
            &mut submit,
            "acceptance",
            json!({"acceptance":"needs_configuration"}),
        )
        .await;
        closed(&mut subscription).await;
    };
    run_observe(driver, send(&peer, turn), 1).await;
    assert!(
        time::timeout(Duration::from_millis(100), peer.listener.accept())
            .await
            .is_err(),
        "晚 NeedsConfiguration 不得撤销 Taken 后补发 Cancel 或提交配置"
    );
}

#[tokio::test]
async fn first_sigint_unknown_submission_keeps_observing_live_before_or_after_grace() {
    let _serial = signal_guard().await;
    late_submit_failure_after_handoff(false).await;
}

#[tokio::test]
async fn first_sigint_unknown_submission_finished_survives_late_transport_failure() {
    let _serial = signal_guard().await;
    late_submit_failure_after_handoff(true).await;
}

async fn late_submit_failure_after_handoff(finished_before_failure: bool) {
    for live_before in [true, false] {
        let peer = Peer::new();
        let turn = lattice_api::local::new_turn_id();
        let driver = async {
            let (mut submit, mut subscription) = begin_submit(&peer, turn).await;
            reply(
                &mut subscription,
                "observation",
                json!({"type":"snapshot", "view":view(turn, false),
                    "cursor":{"epoch":"peer", "sequence":1}, "owned":live_before}),
            )
            .await;
            let (mut body, _) = peer.request("transcript").await;
            empty_transcript(&mut body).await;
            closed(&mut body).await;
            interrupt();
            cancel_request(&peer, turn, live_before).await;
            let (mut refixed, request) = peer.request("subscribe").await;
            assert_eq!(request["turn_id"], json!(turn));
            assert!(request["cursor"].is_null());
            snapshot(&mut refixed, view(turn, false), false).await;
            let mut body = None;
            let mut cancelled = live_before;
            while body.is_none() || !cancelled {
                let (mut stream, request) = peer.request_any().await;
                match request["operation"].as_str().unwrap() {
                    "transcript" => {
                        assert_eq!(request["commit_id"], PENDING);
                        empty_transcript(&mut stream).await;
                        closed(&mut stream).await;
                        body = Some(stream);
                    }
                    "cancel" if !cancelled => {
                        assert_eq!(request["turn_id"], json!(turn));
                        reply(&mut stream, "cancelled", json!({"owned":true})).await;
                        cancelled = true;
                    }
                    other => panic!("unexpected operation after handoff: {other}"),
                }
            }
            // 已过五秒且 live 正文已读完，首次 SIGINT 仍须继续观察，不能直接 detach。
            assert!(
                time::timeout(Duration::from_millis(100), refixed.read(&mut [0]))
                    .await
                    .is_err()
            );
            if !finished_before_failure {
                // 主动结束原 Submit 回执，不关闭同身份观察。
                submit.shutdown().await.unwrap();
                closed(&mut submit).await;
                assert!(
                    time::timeout(Duration::from_millis(100), refixed.read(&mut [0]))
                        .await
                        .is_err(),
                    "迟到 Submit 传输失败不得中断 Live 观察"
                );
            }
            snapshot(&mut refixed, view(turn, true), true).await;
            let (mut final_body, request) = peer.request("transcript").await;
            assert_eq!(request["commit_id"], COMMIT);
            if finished_before_failure {
                // 正文请求证明 Finished 已交付；正文仍挂起时才关闭原 Submit。
                submit.shutdown().await.unwrap();
                closed(&mut submit).await;
                assert!(
                    time::timeout(Duration::from_millis(100), final_body.read(&mut [0]))
                        .await
                        .is_err(),
                    "迟到 Submit 传输失败不得撤销 Finished 或放弃最终正文"
                );
            }
            empty_transcript(&mut final_body).await;
        };
        let exit = time::timeout(Duration::from_secs(6), async {
            let (result, ()) = tokio::join!(send(&peer, turn), driver);
            result.unwrap()
        })
        .await
        .expect("live observation must survive the receipt grace and consume Finished");
        assert_eq!(exit, std::process::ExitCode::SUCCESS);
        assert!(
            time::timeout(Duration::from_millis(50), peer.listener.accept())
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn first_sigint_unknown_submission_with_no_owner_has_bounded_exit() {
    let _serial = signal_guard().await;
    let peer = Peer::new();
    let turn = lattice_api::local::new_turn_id();
    let driver = async {
        let (mut submit, mut subscription) = begin_submit(&peer, turn).await;
        reply(
            &mut subscription,
            "observation",
            json!({"type":"snapshot", "view":view(turn, false),
                "cursor":{"epoch":"peer", "sequence":1}, "owned":false}),
        )
        .await;
        let (mut body, request) = peer.request("transcript").await;
        assert_eq!(request["commit_id"], PENDING);
        empty_transcript(&mut body).await;
        closed(&mut body).await;
        interrupt();
        cancel_request(&peer, turn, false).await;
        // 五秒交接后只读重新固定；旧 NoOwner 不证明后台后来没有接管。
        // 不发第二次中断、不伪造接受，也不放宽六秒总等待。
        let (mut refixed, request) = peer.request("subscribe").await;
        assert_eq!(request["session_id"], SESSION);
        assert_eq!(request["turn_id"], json!(turn));
        assert!(request["cursor"].is_null());
        reply(
            &mut refixed,
            "observation",
            json!({"type":"snapshot", "view":view(turn, false),
                "cursor":{"epoch":"peer", "sequence":2}, "owned":false}),
        )
        .await;
        let (mut body, request) = peer.request("transcript").await;
        assert_eq!(request["commit_id"], PENDING);
        empty_transcript(&mut body).await;
        closed(&mut submit).await;
    };
    let exit = time::timeout(Duration::from_secs(6), async {
        let (result, ()) = tokio::join!(send(&peer, turn), driver);
        result.unwrap()
    })
    .await
    .expect("首次中断后，提交回执未知且观察已无 Owner，仍须有限退出等待");
    assert_eq!(exit, std::process::ExitCode::from(1));
}

// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 以私有 Unix 对端替代后台，驱动真实 LocalTurn、真实事件循环与退出交接；
//! 键盘事件按脚本注入，绘制回调用于观察界面状态。

use super::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use lattice_api::local::LocalError;
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{oneshot, watch};

const SESSION: &str = "11111111111141118111111111111111";
const PENDING: &str = "1111111111111111111111111111111111111111";
const COMMIT: &str = "2222222222222222222222222222222222222222";

/// 对端应答方式；除显式 Close 故障外，连接保持打开至测试结束。
enum Reply {
    Now(Value),
    Frames(Vec<Value>),
    Hold(oneshot::Receiver<Value>),
    Close(oneshot::Receiver<()>),
    Never,
    Stream(UnboundedReceiver<Value>),
}

struct Peer {
    _root: tempfile::TempDir,
    client: LocalClient,
    seen: watch::Receiver<Vec<Value>>,
}

impl Peer {
    /// `handler` 按请求 JSON 决定应答；所有请求按到达顺序记录供断言。
    fn start(mut handler: impl FnMut(&Value) -> Reply + Send + 'static) -> Self {
        let root = tempfile::tempdir().unwrap();
        let layout = lattice_api::StateLayout::in_state_root(root.path());
        let local = layout.lattice_root().join("local");
        std::fs::create_dir_all(&local).unwrap();
        std::fs::set_permissions(&local, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = local.join("api.sock");
        let listener = UnixListener::bind(&path).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let (record, seen) = watch::channel(Vec::new());
        tokio::spawn(async move {
            let held: Arc<Mutex<Vec<UnixStream>>> = Arc::default();
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut magic = [0; 8];
                stream.read_exact(&mut magic).await.unwrap();
                assert_eq!(&magic, b"LTLOCAL2");
                let len = stream.read_u32().await.unwrap();
                let mut bytes = vec![0; len as usize];
                stream.read_exact(&mut bytes).await.unwrap();
                let request: Value = serde_json::from_slice(&bytes).unwrap();
                let reply = handler(&request);
                record.send_modify(|seen| seen.push(request));
                let held = held.clone();
                tokio::spawn(async move {
                    match reply {
                        Reply::Now(value) => write_frame(&mut stream, &value).await,
                        Reply::Frames(values) => {
                            for value in values {
                                write_frame(&mut stream, &value).await;
                            }
                        }
                        Reply::Hold(receiver) => {
                            if let Ok(value) = receiver.await {
                                write_frame(&mut stream, &value).await;
                            }
                        }
                        Reply::Stream(mut receiver) => {
                            while let Some(value) = receiver.next().await {
                                write_frame(&mut stream, &value).await;
                            }
                        }
                        Reply::Close(receiver) => {
                            let _ = receiver.await;
                            return;
                        }
                        Reply::Never => {}
                    }
                    held.lock().unwrap().push(stream);
                });
            }
        });
        Self {
            _root: root,
            client: LocalClient::new(layout),
            seen,
        }
    }

    async fn wait_for(&self, predicate: impl FnMut(&Vec<Value>) -> bool) -> Vec<Value> {
        let mut seen = self.seen.clone();
        tokio::time::timeout(Duration::from_secs(5), seen.wait_for(predicate))
            .await
            .expect("对端未在限时内收到预期请求")
            .unwrap()
            .clone()
    }

    fn seen(&self) -> Vec<Value> {
        self.seen.borrow().clone()
    }
}

async fn write_frame(stream: &mut UnixStream, value: &Value) {
    let bytes = serde_json::to_vec(value).unwrap();
    if stream.write_all(b"LTLOCAL2").await.is_err() {
        return;
    }
    let _ = stream.write_u32(bytes.len() as u32).await;
    let _ = stream.write_all(&bytes).await;
}

fn response(kind: &str, value: Value) -> Value {
    json!({"response": kind, "value": value})
}

fn count(seen: &[Value], operation: &str) -> usize {
    seen.iter()
        .filter(|request| request["operation"] == operation)
        .count()
}

fn cancels(seen: &[Value]) -> Vec<Value> {
    seen.iter()
        .filter(|request| request["operation"] == "cancel")
        .map(|request| request["turn_id"].clone())
        .collect()
}

fn submitted_turn(seen: &[Value]) -> TurnId {
    let submit = seen
        .iter()
        .find(|request| request["operation"] == "submit")
        .expect("submit");
    serde_json::from_value(submit["request"]["turn_id"].clone()).unwrap()
}

fn committed_turn(turn_id: TurnId) -> lattice_api::local::CommittedTurnObservation {
    lattice_api::local::CommittedTurnObservation {
        commit_id: COMMIT.into(),
        turn_id,
        parent_ids: vec![],
        agent_name: "unused".into(),
        model: "unused".into(),
        provider: "unused".into(),
        outcome: lattice_api::TurnOutcome::Completed {
            finish_reason: lattice_api::TurnFinishReason::Stop,
        },
        origin: lattice_api::ClosureOrigin::Live,
        usage: None,
        workspace_history_checkpoint: lattice_api::WorkspaceHistoryCheckpoint::Empty,
        created_at: "2026-01-01T00:00:00Z".into(),
    }
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
            lattice_api::local::TurnObservation::Committed(Box::new(committed_turn(turn_id)))
        } else {
            lattice_api::local::TurnObservation::Pending(pending)
        }),
    }
}

fn observation(kind: &str, view: &SessionObservation, owned: bool) -> Value {
    let cursor = json!({"epoch":"peer", "sequence":1});
    response(
        "observation",
        if kind == "finished" {
            json!({"type":"finished", "view":view, "cursor":cursor, "error":null})
        } else {
            json!({"type":"snapshot", "view":view, "cursor":cursor, "owned":owned})
        },
    )
}

fn cancelled(owned: bool) -> Reply {
    Reply::Now(response("cancelled", json!({"owned": owned})))
}

fn transcript_page() -> Reply {
    Reply::Now(response(
        "transcript",
        json!({"jsonl":"", "next_offset":null}),
    ))
}

/// 已关闭 Turn 的只读应答：查询、订阅完成回执与正文页。
fn committed_reads(request: &Value, turn_id: TurnId) -> Option<Reply> {
    Some(match request["operation"].as_str().unwrap() {
        "query" => Reply::Now(response("view", json!(view(turn_id, true)))),
        "subscribe" => Reply::Frames(vec![observation("finished", &view(turn_id, true), false)]),
        "history" => Reply::Now(response(
            "history",
            json!(if request["offset"] == 0 {
                vec![COMMIT]
            } else {
                vec![]
            }),
        )),
        "commit" => Reply::Now(response("commit", json!(committed_turn(turn_id)))),
        "transcript" => transcript_page(),
        _ => return None,
    })
}

#[derive(Clone)]
struct Frame {
    phase: TurnPhase,
    current_turn: Option<TurnId>,
    connected: bool,
    input: String,
    text: String,
    blocks: Vec<Block>,
    cancel_status: Option<CancelStatus>,
    state: Option<LocalState>,
}

fn frame_of(app: &App) -> Frame {
    Frame {
        blocks: app.blocks.clone(),
        cancel_status: app.cancel_status,
        state: app.state.clone(),
        phase: app.phase,
        current_turn: app.current_turn,
        connected: app.connected,
        input: app.input.text().to_owned(),
        text: crate::view::transcript_lines(app, 200)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

struct Harness {
    options: TuiOptions,
    app: App,
    events: UnboundedReceiver<std::io::Result<Event>>,
    observer: watch::Sender<Frame>,
    remote: Remote,
    _dirs: tempfile::TempDir,
}

/// 测试侧句柄：注入按键、观察绘制回调发布的界面状态；与运行中的循环无借用冲突。
#[derive(Clone)]
struct Remote {
    keys: UnboundedSender<std::io::Result<Event>>,
    frames: watch::Receiver<Frame>,
}

impl Harness {
    fn new() -> Self {
        let dirs = tempfile::tempdir().unwrap();
        let options = TuiOptions {
            session_id: Some(SESSION.into()),
            agent: "unused".into(),
            project_branch: Some("main".into()),
            session_dir: dirs.path().join("session"),
            agents_dir: dirs.path().join("agents"),
            project_root: dirs.path().to_path_buf(),
        };
        let app = App::new(
            SessionContext {
                session_id: SESSION.into(),
                active_agent: "unused".into(),
                project_branch: "main".into(),
            },
            false,
            &options.session_dir,
        );
        let (keys, events) = unbounded();
        let (observer, frames) = watch::channel(frame_of(&app));
        Self {
            options,
            app,
            events,
            observer,
            remote: Remote { keys, frames },
            _dirs: dirs,
        }
    }

    fn remote(&self) -> Remote {
        self.remote.clone()
    }

    /// 运行真实事件循环；绘制回调把界面状态发布给测试。
    async fn run(&mut self, client: &LocalClient) -> LoopExit {
        let observer = self.observer.clone();
        run_loop(
            &self.options,
            &mut self.app,
            client,
            None,
            &mut self.events,
            |app| {
                observer.send_replace(frame_of(app));
                Ok(())
            },
        )
        .await
    }
}

impl Remote {
    fn press(&self, code: KeyCode, modifiers: KeyModifiers) {
        self.keys
            .unbounded_send(Ok(Event::Key(KeyEvent::new(code, modifiers))))
            .unwrap();
    }

    fn type_line(&self, text: &str) {
        for c in text.chars() {
            self.press(KeyCode::Char(c), KeyModifiers::NONE);
        }
        self.press(KeyCode::Enter, KeyModifiers::NONE);
    }

    fn ctrl(&self, c: char) {
        self.press(KeyCode::Char(c), KeyModifiers::CONTROL);
    }

    async fn wait_frame(&self, predicate: impl FnMut(&Frame) -> bool) -> Frame {
        let mut frames = self.frames.clone();
        tokio::time::timeout(Duration::from_secs(5), frames.wait_for(predicate))
            .await
            .expect("界面未在限时内到达预期状态")
            .unwrap()
            .clone()
    }
}

/// 提交后按 Ctrl+C：对端在所有者登记前收到取消并返回 owned=false。
async fn submit_and_cancel_before_owner(remote: &Remote, peer: &Peer) -> TurnId {
    remote.type_line("hi");
    let seen = peer.wait_for(|seen| count(seen, "submit") == 1).await;
    assert!(seen[0]["configuration"].is_null());
    remote.ctrl('c');
    peer.wait_for(|seen| count(seen, "cancel") == 1).await;
    submitted_turn(&seen)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Receipt {
    Accepted,
    Rejected,
    Never,
}

#[tokio::test]
async fn first_ctrl_c_keeps_observing_after_submission_receipt_grace() {
    let (release, receipt) = oneshot::channel();
    let mut receipt = Some(receipt);
    let (frames, incoming) = unbounded();
    let mut incoming = Some(incoming);
    let mut subscriptions = 0;
    let mut cancel_count = 0;
    let peer = Peer::start(
        move |request| match request["operation"].as_str().unwrap() {
            "submit" => {
                assert!(request["configuration"].is_null());
                Reply::Hold(receipt.take().expect("only one Submit"))
            }
            "subscribe" => {
                let id = serde_json::from_value(request["turn_id"].clone()).unwrap();
                assert_eq!(request["session_id"], SESSION);
                assert!(request["cursor"].is_null());
                subscriptions += 1;
                match subscriptions {
                    1 => Reply::Now(observation("snapshot", &view(id, false), false)),
                    2 => Reply::Stream(incoming.take().unwrap()),
                    3 => Reply::Frames(vec![
                        observation("snapshot", &view(id, false), true),
                        event_frame(
                            2,
                            TurnEvent::TokenDelta {
                                content: "after late acceptance".into(),
                            },
                        ),
                    ]),
                    _ => panic!("no frontend reconnect loop"),
                }
            }
            "transcript" => Reply::Never,
            "cancel" => {
                cancel_count += 1;
                assert!(cancel_count <= 2);
                cancelled(cancel_count == 2)
            }
            other => panic!("unexpected operation: {other}"),
        },
    );
    let mut harness = Harness::new();
    let remote = harness.remote();
    let driver = async {
        let id = submit_and_cancel_before_owner(&remote, &peer).await;
        tokio::time::sleep(Duration::from_secs(4)).await;
        let unknown = remote
            .wait_frame(|frame| frame.text.contains("提交结果未知"))
            .await;
        assert_eq!(unknown.current_turn, Some(id));
        assert!(unknown.text.contains(&id.to_string()));
        frames
            .unbounded_send(observation("snapshot", &view(id, false), true))
            .unwrap();
        frames
            .unbounded_send(event_frame(
                1,
                TurnEvent::TokenDelta {
                    content: "after grace".into(),
                },
            ))
            .unwrap();
        let live = remote
            .wait_frame(|frame| {
                frame.text.contains("after grace") && frame.text.contains("后台已接管取消")
            })
            .await;
        assert_eq!(live.current_turn, Some(id));
        assert!(live.connected);
        release
            .send(response(
                "acceptance",
                json!({"acceptance":"accepted", "turn_id":id}),
            ))
            .unwrap();
        let late = remote
            .wait_frame(|frame| frame.text.contains("after late acceptance"))
            .await;
        assert_eq!(late.current_turn, Some(id));
        remote.ctrl('d');
        id
    };
    let (id, exit) = tokio::time::timeout(Duration::from_secs(6), async {
        tokio::join!(driver, harness.run(&peer.client))
    })
    .await
    .expect("first Ctrl+C must keep the loop available after the five-second handoff");
    assert!(exit.result.is_ok());
    let report = exit.turn.unwrap().detach().await;
    assert_eq!(
        report.submission,
        Some(Ok(Acceptance::Accepted { turn_id: id }))
    );
    assert_eq!(report.cancel, Some(CancelStatus::Taken));
    assert_eq!(report.state.unwrap().phase, ObservationPhase::Live);
    let seen = peer.seen();
    assert_eq!(count(&seen, "submit"), 1);
    assert_eq!(cancels(&seen), [json!(id), json!(id)]);
}

#[tokio::test]
async fn late_submit_transport_failure_keeps_run_loop_live_and_finished() {
    let (close, receipt) = oneshot::channel();
    let mut receipt = Some(receipt);
    let (frames, incoming) = unbounded();
    let mut incoming = Some(incoming);
    let mut subscriptions = 0;
    let peer = Peer::start(
        move |request| match request["operation"].as_str().unwrap() {
            "submit" => {
                assert!(request["configuration"].is_null());
                Reply::Close(receipt.take().expect("one Submit"))
            }
            "subscribe" => {
                assert_eq!(request["session_id"], SESSION);
                assert!(request["cursor"].is_null());
                subscriptions += 1;
                let id = serde_json::from_value(request["turn_id"].clone()).unwrap();
                match subscriptions {
                    1 => Reply::Now(observation("snapshot", &view(id, false), true)),
                    2 => Reply::Stream(incoming.take().unwrap()),
                    _ => panic!("receipt failure must not reconnect"),
                }
            }
            "transcript" | "history" => Reply::Never,
            "cancel" => cancelled(true),
            other => panic!("unexpected operation: {other}"),
        },
    );
    let mut harness = Harness::new();
    let remote = harness.remote();
    let driver = async {
        remote.type_line("hi");
        let seen = peer
            .wait_for(|seen| count(seen, "submit") == 1 && count(seen, "transcript") == 1)
            .await;
        let id = submitted_turn(&seen);
        remote.ctrl('c');
        tokio::time::sleep(Duration::from_secs(4)).await;
        remote
            .wait_frame(|frame| frame.text.contains("提交结果未知"))
            .await;
        frames
            .unbounded_send(observation("snapshot", &view(id, false), true))
            .unwrap();
        frames
            .unbounded_send(event_frame(
                1,
                TurnEvent::TokenDelta {
                    content: "before receipt failure".into(),
                },
            ))
            .unwrap();
        remote
            .wait_frame(|frame| frame.text.contains("before receipt failure"))
            .await;
        close.send(()).unwrap();
        let diagnostic = remote
            .wait_frame(|frame| frame.text.contains("local connection failed"))
            .await;
        assert_eq!(diagnostic.current_turn, Some(id));
        assert_eq!(diagnostic.phase, TurnPhase::Cancelling);
        assert!(diagnostic.connected);
        assert!(
            diagnostic.input.is_empty(),
            "unknown receipt cannot restore the submitted prompt"
        );
        assert!(!diagnostic.text.contains("被拒绝或回执失败"));
        frames
            .unbounded_send(event_frame(
                2,
                TurnEvent::TokenDelta {
                    content: "after receipt failure".into(),
                },
            ))
            .unwrap();
        remote
            .wait_frame(|frame| frame.text.contains("after receipt failure"))
            .await;
        frames
            .unbounded_send(observation("finished", &view(id, true), false))
            .unwrap();
        let finished = remote
            .wait_frame(|frame| frame.phase == TurnPhase::Idle && frame.text.contains("Completed"))
            .await;
        assert!(finished.connected);
        assert_eq!(finished.current_turn, None);
        remote.ctrl('d');
        id
    };
    let (id, exit) = tokio::time::timeout(Duration::from_secs(6), async {
        tokio::join!(driver, harness.run(&peer.client))
    })
    .await
    .expect("receipt transport failure must not stop the run_loop");
    assert!(exit.result.is_ok());
    let report = exit.turn.unwrap().detach().await;
    assert_eq!(report.submission, None);
    assert_eq!(
        report.submission_diagnostic.as_ref().unwrap().kind,
        lattice_api::local::LocalErrorKind::Transport
    );
    assert_eq!(report.cancel, Some(CancelStatus::Taken));
    let state = report.state.as_ref().unwrap();
    assert_eq!(state.phase, ObservationPhase::Finished);
    assert_eq!(state.confirmation, CompletionConfirmation::Confirmed);
    assert_eq!(*state.view, view(id, true));
    let lines = crate::connection::detach_lines(report).join("\n");
    assert!(lines.contains("提交结果未知") && lines.contains("local connection failed"));
    assert!(!lines.contains("被拒绝或回执失败"));
    assert!(lines.contains("Completed") && lines.contains(COMMIT));
    let seen = peer.seen();
    assert_eq!(count(&seen, "submit"), 1);
    assert_eq!(cancels(&seen), [json!(id)]);
}

/// 提交在途、取消先返回 owned=false，然后在提交回执到达前退出：
/// 退出仍交给同一 LocalTurn 有界处理回执，按接受/拒绝/未知分别处理并带原 ID。
#[tokio::test]
async fn exit_before_receipt_settles_cancel_intent_by_turn_id() {
    for receipt in [Receipt::Accepted, Receipt::Rejected, Receipt::Never] {
        let (release_tx, release_rx) = oneshot::channel::<Value>();
        let mut release_rx = Some(release_rx);
        let mut cancel_count = 0;
        let peer = Peer::start(
            move |request| match request["operation"].as_str().unwrap() {
                "submit" => Reply::Hold(release_rx.take().expect("只允许一次提交")),
                // 首次取消早于所有者登记；接受后的再次取消才由所有者接管。
                "cancel" => {
                    cancel_count += 1;
                    cancelled(cancel_count > 1)
                }
                _ => Reply::Never,
            },
        );
        let mut harness = Harness::new();
        let remote = harness.remote();
        let run = harness.run(&peer.client);
        tokio::pin!(run);
        let driver = async {
            let turn_id = submit_and_cancel_before_owner(&remote, &peer).await;
            // 第二次 Ctrl+C 在两秒内：请求退出，此时提交回执尚未到达。
            remote.ctrl('c');
            turn_id
        };
        let (turn_id, loop_exit) = tokio::join!(driver, &mut run);
        assert!(loop_exit.result.is_ok());
        let receipt_value = match receipt {
            Receipt::Accepted => Some(response(
                "acceptance",
                json!({"acceptance":"accepted", "turn_id":turn_id}),
            )),
            Receipt::Rejected => Some(response(
                "error",
                json!(LocalError::from(lattice_api::ApiError::Conflict(
                    "identity bound elsewhere".into()
                ))),
            )),
            Receipt::Never => None,
        };
        // 终端复原后才交付回执，对应退出先于 Submission。
        let started = std::time::Instant::now();
        let lines = tokio::select! {
            report = loop_exit.turn.unwrap().detach() => detach_lines(report),
            _ = async {
                if let Some(value) = receipt_value {
                    let _ = release_tx.send(value);
                }
                std::future::pending::<()>().await
            } => unreachable!(),
        };
        let elapsed = started.elapsed();
        let seen = peer.seen();
        let text = lines.join("\n");
        match receipt {
            Receipt::Accepted => {
                assert_eq!(
                    cancels(&seen),
                    vec![json!(turn_id), json!(turn_id)],
                    "同一 TurnId 被接受后必须再取消一次: {text}"
                );
                assert!(text.contains(&turn_id.to_string()), "{text}");
            }
            Receipt::Rejected => {
                assert_eq!(cancels(&seen), vec![json!(turn_id)], "{text}");
                // LocalError 只保留分类化的拒绝原因；退出消息须带原 ID，且不再报未确认取消。
                assert!(
                    text.contains("被拒绝")
                        && text.contains(&turn_id.to_string())
                        && !text.contains("取消未确认"),
                    "{text}"
                );
            }
            Receipt::Never => {
                assert_eq!(cancels(&seen), vec![json!(turn_id)], "{text}");
                assert!(
                    text.contains("取消未确认")
                        && text.contains(SESSION)
                        && text.contains(&turn_id.to_string()),
                    "{text}"
                );
                assert!(
                    elapsed >= Duration::from_secs(4),
                    "退出前必须有界等待回执: {elapsed:?}"
                );
            }
        }
        assert!(
            elapsed < Duration::from_secs(8),
            "退出收尾必须有界: {elapsed:?}"
        );
        assert_eq!(count(&seen, "submit"), 1, "不得以新身份重发: {seen:?}");
    }
}

/// 提交回执选定的是恢复出的旧 Turn：本次新 prompt 未接受，取消意图无对象，不得扩散到旧 Turn。
#[tokio::test]
async fn recovered_old_turn_does_not_receive_the_new_requests_cancel() {
    let old = lattice_api::local::new_turn_id();
    let (release_tx, release_rx) = oneshot::channel::<Value>();
    let mut release_rx = Some(release_rx);
    let peer = Peer::start(
        move |request| match request["operation"].as_str().unwrap() {
            "submit" => Reply::Hold(release_rx.take().expect("只允许一次提交")),
            "cancel" => cancelled(false),
            // 接受前的共同观察仍针对原新请求，不能给它冒充旧 Turn 的完成视图。
            "subscribe" if request["turn_id"] != json!(old) => Reply::Never,
            _ => committed_reads(request, old).unwrap_or(Reply::Never),
        },
    );
    let mut harness = Harness::new();
    let remote = harness.remote();
    let run = harness.run(&peer.client);
    tokio::pin!(run);
    let driver = async {
        let turn_id = submit_and_cancel_before_owner(&remote, &peer).await;
        // 取消已先于回执发出；回执才选定恢复出的旧 Turn。
        release_tx
            .send(response(
                "acceptance",
                json!({"acceptance":"recovered", "turn": committed_turn(old)}),
            ))
            .unwrap();
        remote
            .wait_frame(|frame| frame.phase == TurnPhase::Idle && frame.text.contains("未接受"))
            .await;
        remote.ctrl('d');
        turn_id
    };
    let (turn_id, loop_exit) = tokio::join!(driver, &mut run);
    assert!(loop_exit.result.is_ok());
    let lines = detach_lines(loop_exit.turn.unwrap().detach().await);
    let seen = peer.seen();
    assert!(
        cancels(&seen).iter().all(|id| *id == json!(turn_id)),
        "旧 Turn {old} 不得收到本次请求的取消: {seen:?}"
    );
    assert!(
        !lines.iter().any(|line| line.contains("取消未确认")),
        "{lines:?}"
    );
}

/// NeedsConfiguration 晚到时停止后续配置；共同句柄已开始的原身份只读观察不等于提交。
#[tokio::test]
async fn cancel_intent_before_configuration_does_not_submit() {
    let (release_tx, release_rx) = oneshot::channel::<Value>();
    let mut release_rx = Some(release_rx);
    let peer = Peer::start(
        move |request| match request["operation"].as_str().unwrap() {
            "submit" => Reply::Hold(release_rx.take().expect("只允许一次提交")),
            "cancel" => cancelled(false),
            "open" => Reply::Now(response(
                "session",
                json!(view(lattice_api::local::new_turn_id(), false).session),
            )),
            _ => Reply::Never,
        },
    );
    let mut harness = Harness::new();
    let remote = harness.remote();
    let run = harness.run(&peer.client);
    tokio::pin!(run);
    let driver = async {
        let turn_id = submit_and_cancel_before_owner(&remote, &peer).await;
        release_tx
            .send(response(
                "acceptance",
                json!({"acceptance":"needs_configuration"}),
            ))
            .unwrap();
        // 初始帧也是 Idle，须等本客户端停止配置的提示，不能仅检查空闲。
        let frame = remote
            .wait_frame(|frame| {
                frame.phase == TurnPhase::Idle && frame.text.contains("已停止后续配置")
            })
            .await;
        assert!(frame.text.contains(&turn_id.to_string()), "{}", frame.text);
        assert_eq!(frame.input, "hi", "输入交还用户，不自动重发");
        assert!(frame.current_turn.is_none());
        // 空闲时 Ctrl+C 只清空输入，两秒内再按退出。
        remote.ctrl('c');
        remote.ctrl('c');
        turn_id
    };
    let (turn_id, loop_exit) = tokio::join!(driver, &mut run);
    assert!(loop_exit.result.is_ok());
    let report = loop_exit.turn.unwrap().detach().await;
    assert!(report.not_submitted.is_some());
    assert_eq!(report.cancel, Some(CancelStatus::NotTaken));
    let lines = detach_lines(report);
    let seen = peer.seen();
    assert_eq!(count(&seen, "submit"), 1, "{seen:?}");
    assert_eq!(count(&seen, "open"), 0, "取消后不得再走配置提交: {seen:?}");
    assert_eq!(
        count(&seen, "subscribe"),
        1,
        "共同句柄开始时只读观察原身份：{seen:?}"
    );
    assert!(
        seen.iter()
            .filter(|r| r["operation"] == "subscribe")
            .all(|r| r["turn_id"] == json!(turn_id))
    );
    assert_eq!(cancels(&seen), vec![json!(turn_id)]);
    assert!(
        lines
            .iter()
            .any(|line| line.contains("本客户端") && line.contains(&turn_id.to_string())),
        "{lines:?}"
    );
    assert!(
        lines.iter().any(|line| line.contains("未接管")),
        "本地停止配置不证明取消没有对象：{lines:?}"
    );
}

/// 无取消意图的 Ctrl+D 只断开：不新增取消，也不等待。
#[tokio::test]
async fn quit_without_cancel_intent_only_disconnects() {
    let (_release_tx, release_rx) = oneshot::channel::<Value>();
    let mut release_rx = Some(release_rx);
    let peer = Peer::start(
        move |request| match request["operation"].as_str().unwrap() {
            "submit" => Reply::Hold(release_rx.take().expect("只允许一次提交")),
            _ => Reply::Never,
        },
    );
    let mut harness = Harness::new();
    let remote = harness.remote();
    let run = harness.run(&peer.client);
    tokio::pin!(run);
    let driver = async {
        remote.type_line("hi");
        peer.wait_for(|seen| count(seen, "submit") == 1).await;
        remote.ctrl('d');
    };
    let ((), loop_exit) = tokio::join!(driver, &mut run);
    assert!(loop_exit.result.is_ok());
    let started = std::time::Instant::now();
    let lines = detach_lines(loop_exit.turn.unwrap().detach().await);
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(lines.is_empty(), "{lines:?}");
    assert!(cancels(&peer.seen()).is_empty());
}

/// 在线 owned pending 快照的正文读取失败只是正文不可用：不是完成，也不是断开。
#[tokio::test]
async fn pending_body_failure_keeps_owned_turn_online() {
    let turn_id = lattice_api::local::new_turn_id();
    let peer = Peer::start(
        move |request| match request["operation"].as_str().unwrap() {
            "query" => Reply::Now(response("view", json!(view(turn_id, false)))),
            "subscribe" => {
                assert_eq!(request["turn_id"], json!(turn_id));
                Reply::Frames(vec![observation("snapshot", &view(turn_id, false), true)])
            }
            // pending 正文不可读；完成后的固定正文可读。
            "transcript" if request["commit_id"] == PENDING => Reply::Now(response(
                "error",
                json!(LocalError::from(lattice_api::ApiError::StoreUnavailable(
                    "journal unreadable".into()
                ))),
            )),
            _ => committed_reads(request, turn_id).unwrap_or(Reply::Never),
        },
    );
    let connection = peer
        .client
        .observe(SESSION.into(), Some(turn_id), HistoryScope::Session);
    let mut harness = Harness::new();
    let observer = harness.observer.clone();
    let remote = harness.remote();
    let run = run_loop(
        &harness.options,
        &mut harness.app,
        &peer.client,
        Some(connection),
        &mut harness.events,
        |app| {
            observer.send_replace(frame_of(app));
            Ok(())
        },
    );
    let driver = async {
        let frame = remote
            .wait_frame(|frame| frame.text.contains("正文不可用"))
            .await;
        remote.ctrl('d');
        frame
    };
    let (frame, exit) = tokio::join!(driver, run);
    assert!(exit.result.is_ok());
    let report = exit.turn.unwrap().detach().await;
    assert!(report.history_error.is_some());
    let state = report.state.unwrap();
    assert_eq!(state.phase, ObservationPhase::Live);
    assert_eq!(state.confirmation, CompletionConfirmation::Unknown);
    assert!(frame.connected, "正文失败不得推断断开: {}", frame.text);
    assert_eq!(frame.phase, TurnPhase::Streaming);
    assert_eq!(frame.current_turn, Some(turn_id));
    assert!(frame.text.contains("正文不可用"), "{}", frame.text);
    assert!(!frame.text.contains("完成快照"), "{}", frame.text);
    assert!(!frame.text.contains("offline"), "{}", frame.text);
}

/// 正文连接保持挂起，独立 Finished 仍须先到真实绘制回调，并允许普通退出。
#[tokio::test]
async fn finished_is_drawn_while_pending_and_final_history_are_stalled() {
    let turn_id = lattice_api::local::new_turn_id();
    let (frames, incoming) = unbounded();
    let mut incoming = Some(incoming);
    let peer = Peer::start(
        move |request| match request["operation"].as_str().unwrap() {
            "query" => Reply::Now(response("view", json!(view(turn_id, false)))),
            "subscribe" => Reply::Stream(incoming.take().unwrap()),
            "transcript" => Reply::Never,
            _ => committed_reads(request, turn_id).expect("仅允许只读请求"),
        },
    );
    let mut harness = Harness::new();
    let remote = harness.remote();
    let observer = harness.observer.clone();
    let initial = peer
        .client
        .observe(SESSION.into(), Some(turn_id), HistoryScope::Session);
    let run = run_loop(
        &harness.options,
        &mut harness.app,
        &peer.client,
        Some(initial),
        &mut harness.events,
        |app| {
            observer.send_replace(frame_of(app));
            Ok(())
        },
    );
    let driver = async {
        peer.wait_for(|seen| count(seen, "subscribe") == 1).await;
        frames
            .unbounded_send(observation("snapshot", &view(turn_id, false), true))
            .unwrap();
        peer.wait_for(|seen| count(seen, "transcript") == 1).await;
        frames
            .unbounded_send(observation("finished", &view(turn_id, true), false))
            .unwrap();
        let frame = remote
            .wait_frame(|frame| frame.text.contains(&format!("committed {COMMIT}")))
            .await;
        assert_eq!(frame.phase, TurnPhase::Idle);
        assert!(frame.current_turn.is_none());
        assert!(frame.connected);
        assert!(
            frame.text.contains("Completed") && frame.text.contains(&turn_id.to_string()),
            "{}",
            frame.text
        );
        peer.wait_for(|seen| count(seen, "transcript") == 2).await;
        remote.ctrl('d');
    };
    let ((), exit) = tokio::join!(driver, run);
    assert!(exit.result.is_ok());
    let lines = detach_lines(
        tokio::time::timeout(Duration::from_secs(1), exit.turn.unwrap().detach())
            .await
            .unwrap(),
    );
    assert!(cancels(&peer.seen()).is_empty());
    assert!(!lines.iter().any(|line| line.contains("取消")), "{lines:?}");
}

/// 原绝对 idle 回归迁到真实循环：帧 ticker 不断重建 next 等待，但不能重置库的窗口。
#[tokio::test]
async fn idle_deadline_survives_repeated_poll_rebuilds() {
    let turn_id = lattice_api::local::new_turn_id();
    let peer = Peer::start(
        move |request| match request["operation"].as_str().unwrap() {
            "subscribe" => {
                Reply::Frames(vec![observation("snapshot", &view(turn_id, false), true)])
            }
            "transcript" => transcript_page(),
            _ => panic!("只允许固定观察"),
        },
    );
    let mut harness = Harness::new();
    let remote = harness.remote();
    let observer = harness.observer.clone();
    let initial = peer
        .client
        .observe(SESSION.into(), Some(turn_id), HistoryScope::Session);
    let run = run_loop(
        &harness.options,
        &mut harness.app,
        &peer.client,
        Some(initial),
        &mut harness.events,
        |app| {
            observer.send_replace(frame_of(app));
            Ok(())
        },
    );
    let driver = async {
        remote
            .wait_frame(|frame| frame.phase == TurnPhase::Streaming)
            .await;
        peer.wait_for(|seen| count(seen, "transcript") == 1).await;
        tokio::time::pause();
        let deadline = tokio::time::Instant::now()
            + Duration::from_secs(lattice_api::STREAM_IDLE_TIMEOUT_SECS);
        for _ in 0..lattice_api::STREAM_IDLE_TIMEOUT_SECS - 1 {
            tokio::time::advance(Duration::from_secs(1)).await;
            for _ in 0..8 {
                tokio::task::yield_now().await;
            }
            assert!(remote.frames.borrow().connected, "idle 不得提前到期");
        }
        tokio::time::advance(Duration::from_secs(1)).await;
        let frame = remote.wait_frame(|frame| !frame.connected).await;
        let fired_at = tokio::time::Instant::now();
        assert!(
            fired_at.saturating_duration_since(deadline) < Duration::from_secs(1),
            "重建等待延长了原 idle 窗口"
        );
        assert_eq!(frame.current_turn, Some(turn_id));
        assert_eq!(frame.phase, TurnPhase::Streaming);
        assert!(frame.text.contains("无增量") && !frame.text.contains("committed"));
        tokio::time::resume();
        remote.ctrl('d');
    };
    let ((), exit) = tokio::join!(driver, run);
    assert!(exit.result.is_ok());
    assert_eq!(exit.turn.unwrap().detach().await.cancel, None);
}

fn event_frame(sequence: u64, event: TurnEvent) -> Value {
    response(
        "observation",
        json!({"type":"event", "cursor":{"epoch":"peer", "sequence":sequence}, "event":event}),
    )
}

fn message(role: &str, semantics: Vec<Value>) -> Value {
    let value = json!({"role":role, "parts":semantics.into_iter().map(|semantic| json!({"semantic":semantic})).collect::<Vec<_>>()});
    // 夹具同样经过公开消息解码，不能靠不合法的持久消息制造顺序。
    let _: lattice_api::local::NativeMessage<lattice_api::local::BoundToolCall> =
        serde_json::from_value(value.clone()).unwrap();
    value
}

fn transcript(messages: Vec<Value>) -> Value {
    response(
        "transcript",
        json!({"jsonl":messages.iter().map(|m| serde_json::to_string(m).unwrap() + "\n").collect::<String>(), "next_offset":null}),
    )
}

fn tool_call(turn: TurnId, id: &str) -> Value {
    json!({"tool_call":{"call":{
        "invocation_id":id, "provider_call_id":"provider-x", "name":"observe.workspace",
        "arguments":{"path":"a.txt"},
        "arguments_digest":"5fd582703776c899a2ecb12f3d699bd7244dff1f443ef5ae3f0067bcf3ce279e",
        "address":{"message":{"session_id":SESSION,"turn_id":turn,"model_step_ordinal":0},"part_ordinal":0}
    }}})
}

/// U1：Finished 先到且 Started 在 cut 前，晚到 ToolCall 历史不能丢失、重复或降级完成行。
#[tokio::test]
async fn tool_finished_before_history_remains_visible_once() {
    let turn_id = lattice_api::local::new_turn_id();
    let invocation = "22222222-2222-4222-8222-222222222222";
    let (frames, incoming) = unbounded();
    let mut incoming = Some(incoming);
    let (history_tx, history_rx) = oneshot::channel();
    let mut history_rx = Some(history_rx);
    let peer = Peer::start(
        move |request| match request["operation"].as_str().unwrap() {
            "subscribe" => Reply::Stream(incoming.take().unwrap()),
            "transcript" => Reply::Hold(history_rx.take().unwrap()),
            _ => panic!("仅允许观察与固定正文"),
        },
    );
    let mut harness = Harness::new();
    let remote = harness.remote();
    let observer = harness.observer.clone();
    let initial = peer
        .client
        .observe(SESSION.into(), Some(turn_id), HistoryScope::Session);
    let run = run_loop(
        &harness.options,
        &mut harness.app,
        &peer.client,
        Some(initial),
        &mut harness.events,
        |app| {
            observer.send_replace(frame_of(app));
            Ok(())
        },
    );
    let driver = async {
        peer.wait_for(|seen| count(seen, "subscribe") == 1).await;
        frames
            .unbounded_send(observation("snapshot", &view(turn_id, false), true))
            .unwrap();
        peer.wait_for(|seen| count(seen, "transcript") == 1).await;
        frames
            .unbounded_send(event_frame(
                2,
                TurnEvent::ToolCallFinished {
                    id: invocation.into(),
                    name: "observe.workspace".into(),
                    ok: true,
                    output_len: 17,
                    error_summary: None,
                },
            ))
            .unwrap();
        let frame = remote.wait_frame(|frame| frame.text.contains("17 B")).await;
        assert_eq!(frame.phase, TurnPhase::Streaming);
        assert_eq!(frame.text.matches("observe.workspace").count(), 1);
        frames
            .unbounded_send(event_frame(
                3,
                TurnEvent::ToolCallStarted {
                    id: invocation.into(),
                    name: "observe.workspace".into(),
                },
            ))
            .unwrap();
        frames
            .unbounded_send(event_frame(
                4,
                TurnEvent::TokenDelta {
                    content: "工具之后的新文字".into(),
                },
            ))
            .unwrap();
        let frame = remote
            .wait_frame(|frame| frame.text.contains("工具之后的新文字"))
            .await;
        assert!(
            frame.text.contains("17 B"),
            "迟到 Started 不得降级：{}",
            frame.text
        );
        history_tx
            .send(transcript(vec![
                message("user", vec![json!({"text":{"text":"固定工具历史"}})]),
                message("assistant", vec![tool_call(turn_id, invocation)]),
            ]))
            .unwrap();
        let frame = remote
            .wait_frame(|frame| frame.text.contains("固定工具历史"))
            .await;
        assert_eq!(frame.phase, TurnPhase::Streaming);
        assert_eq!(frame.text.matches("observe.workspace").count(), 1);
        assert!(
            frame.text.contains("17 B") && frame.text.contains("工具之后的新文字"),
            "{}",
            frame.text
        );
        assert!(!frame.text.contains("committed"));
        remote.ctrl('d');
    };
    let ((), exit) = tokio::join!(driver, run);
    assert!(exit.result.is_ok());
    assert_eq!(exit.turn.unwrap().detach().await.cancel, None);
}

/// U2：历史 ToolCall 的事件覆盖在新切点失效，持久 outcome 与材料优先且保留来源。
#[tokio::test]
async fn gap_resets_tool_observations_and_durable_material_overrides_events() {
    for (outcome, content, expected) in [
        (
            "succeeded",
            None,
            ToolState::Unavailable {
                outcome: "succeeded".into(),
            },
        ),
        (
            "failed",
            Some("持久失败材料"),
            ToolState::Failed {
                output_len: "持久失败材料".len(),
                error: Some("failed: 持久失败材料".into()),
            },
        ),
    ] {
        let turn_id = lattice_api::local::new_turn_id();
        let invocation = "22222222-2222-4222-8222-222222222222";
        let (frames, incoming) = unbounded();
        let mut incoming = Some(incoming);
        let (late_tx, late_rx) = oneshot::channel();
        let mut late_rx = Some(late_rx);
        let peer =
            Peer::start(
                move |request| match request["operation"].as_str().unwrap() {
                    "subscribe" => Reply::Stream(incoming.take().unwrap()),
                    "transcript" if request["commit_id"] == PENDING => Reply::Now(transcript(
                        vec![message("assistant", vec![tool_call(turn_id, invocation)])],
                    )),
                    "transcript" => Reply::Hold(late_rx.take().unwrap()),
                    _ => panic!("仅允许观察与固定正文"),
                },
            );
        let mut harness = Harness::new();
        let remote = harness.remote();
        let observer = harness.observer.clone();
        let initial = peer
            .client
            .observe(SESSION.into(), Some(turn_id), HistoryScope::Session);
        let run = run_loop(
            &harness.options,
            &mut harness.app,
            &peer.client,
            Some(initial),
            &mut harness.events,
            |app| {
                observer.send_replace(frame_of(app));
                Ok(())
            },
        );
        let driver = async {
            peer.wait_for(|seen| count(seen, "subscribe") == 1).await;
            frames
                .unbounded_send(observation("snapshot", &view(turn_id, false), true))
                .unwrap();
            remote
                .wait_frame(|frame| frame.text.contains("observe.workspace"))
                .await;
            frames
                .unbounded_send(event_frame(
                    2,
                    TurnEvent::ToolCallFinished {
                        id: invocation.into(),
                        name: "observe.workspace".into(),
                        ok: true,
                        output_len: 17,
                        error_summary: None,
                    },
                ))
                .unwrap();
            let frame = remote.wait_frame(|frame| frame.text.contains("17 B")).await;
            assert!(frame.blocks.iter().any(|block| matches!(
                block,
                Block::Tool {
                    source: ToolSource::Event,
                    ..
                }
            )));
            frames
                .unbounded_send(response(
                    "observation",
                    json!({"type":"gap","cursor":{"epoch":"peer","sequence":4}}),
                ))
                .unwrap();
            let mut next = view(turn_id, false);
            next.active_turn.as_mut().unwrap().journal_oid = "next-journal".into();
            next.requested_turn = next.active_turn.clone().map(TurnObservation::Pending);
            frames
                .unbounded_send(observation("snapshot", &next, true))
                .unwrap();
            peer.wait_for(|seen| count(seen, "transcript") == 2).await;
            let frame = remote
                .wait_frame(|frame| frame.text.contains("缺口") && !frame.text.contains("17 B"))
                .await;
            assert!(frame.blocks.iter().any(|block| matches!(
                block,
                Block::Tool {
                    state: ToolState::Unknown,
                    source: ToolSource::HistoryCall,
                    ..
                }
            )));
            frames
                .unbounded_send(event_frame(
                    5,
                    TurnEvent::ToolCallFinished {
                        id: invocation.into(),
                        name: "observe.workspace".into(),
                        ok: true,
                        output_len: 88,
                        error_summary: None,
                    },
                ))
                .unwrap();
            remote.wait_frame(|frame| frame.text.contains("88 B")).await;
            late_tx.send(transcript(vec![
                message("assistant", vec![tool_call(turn_id, invocation)]),
                message("tool", vec![json!({"tool_result":{"invocation_id":invocation,"name":"observe.workspace","outcome":outcome,"content":content}})]),
            ])).unwrap();
            let frame = remote.wait_frame(|frame| frame.blocks.iter().any(|block| matches!(block, Block::Tool { source:ToolSource::HistoryResult, state, .. } if *state == expected))).await;
            assert_eq!(frame.text.matches("observe.workspace").count(), 1);
            assert!(!frame.text.contains("88 B"), "{}", frame.text);
            frames
                .unbounded_send(event_frame(
                    6,
                    TurnEvent::ToolCallStarted {
                        id: invocation.into(),
                        name: "observe.workspace".into(),
                    },
                ))
                .unwrap();
            frames
                .unbounded_send(event_frame(
                    7,
                    TurnEvent::ToolCallFinished {
                        id: invocation.into(),
                        name: "observe.workspace".into(),
                        ok: true,
                        output_len: 99,
                        error_summary: None,
                    },
                ))
                .unwrap();
            frames
                .unbounded_send(event_frame(
                    8,
                    TurnEvent::TokenDelta {
                        content: "材料接管后的进展".into(),
                    },
                ))
                .unwrap();
            let frame = remote
                .wait_frame(|frame| frame.text.contains("材料接管后的进展"))
                .await;
            assert!(frame.blocks.iter().any(|block| matches!(block, Block::Tool { source:ToolSource::HistoryResult, state, .. } if *state == expected)));
            assert_eq!(frame.text.matches("observe.workspace").count(), 1);
            assert!(!frame.text.contains("99 B") && frame.text.contains("缺口"));
            assert_eq!(frame.phase, TurnPhase::Streaming);
            remote.ctrl('d');
        };
        let ((), exit) = tokio::join!(driver, run);
        assert!(exit.result.is_ok());
        assert_eq!(exit.turn.unwrap().detach().await.cancel, None);
    }
}

/// O2：真实 /reconnect 后不重放 cut 前文字；新 delta 与同切点迟到历史并存，直到下个 Reset。
#[tokio::test]
async fn reconnect_suffix_is_drawn_before_commit_and_survives_late_history() {
    let turn_id = lattice_api::local::new_turn_id();
    let (first_tx, first_rx) = unbounded();
    let (next_tx, next_rx) = unbounded();
    let mut streams = std::collections::VecDeque::from([first_rx, next_rx]);
    let (history_tx, history_rx) = oneshot::channel();
    let mut history_rx = Some(history_rx);
    let peer = Peer::start(
        move |request| match request["operation"].as_str().unwrap() {
            "subscribe" => {
                assert_eq!(request["turn_id"], json!(turn_id));
                assert!(request["cursor"].is_null());
                Reply::Stream(streams.pop_front().expect("仅一次显式重连"))
            }
            "transcript" if request["commit_id"] == "reconnected" => {
                Reply::Hold(history_rx.take().unwrap())
            }
            "transcript" => Reply::Now(transcript(vec![
                message("user", vec![json!({"text":{"text":"请求"}})]),
                message("assistant", vec![json!({"text":{"text":"持久前缀"}})]),
            ])),
            _ => panic!("重连不得提交或恢复"),
        },
    );
    let mut harness = Harness::new();
    let remote = harness.remote();
    let observer = harness.observer.clone();
    let initial = peer
        .client
        .observe(SESSION.into(), Some(turn_id), HistoryScope::Session);
    let run = run_loop(
        &harness.options,
        &mut harness.app,
        &peer.client,
        Some(initial),
        &mut harness.events,
        |app| {
            observer.send_replace(frame_of(app));
            Ok(())
        },
    );
    let driver = async {
        peer.wait_for(|seen| count(seen, "subscribe") == 1).await;
        first_tx
            .unbounded_send(observation("snapshot", &view(turn_id, false), true))
            .unwrap();
        remote
            .wait_frame(|frame| frame.text.contains("持久前缀"))
            .await;
        first_tx
            .unbounded_send(event_frame(
                2,
                TurnEvent::ModelStarted {
                    provider: None,
                    model: "model".into(),
                },
            ))
            .unwrap();
        first_tx
            .unbounded_send(event_frame(
                3,
                TurnEvent::TokenDelta {
                    content: "重连前临时文字".into(),
                },
            ))
            .unwrap();
        remote
            .wait_frame(|frame| frame.text.contains("重连前临时文字"))
            .await;
        remote.type_line("/reconnect");
        peer.wait_for(|seen| count(seen, "subscribe") == 2).await;
        let mut fixed = view(turn_id, false);
        fixed.active_turn.as_mut().unwrap().journal_oid = "reconnected".into();
        fixed.requested_turn = fixed.active_turn.clone().map(TurnObservation::Pending);
        next_tx
            .unbounded_send(observation("snapshot", &fixed, true))
            .unwrap();
        peer.wait_for(|seen| count(seen, "transcript") == 2).await;
        next_tx
            .unbounded_send(event_frame(
                4,
                TurnEvent::TokenDelta {
                    content: "新的不完整后缀".into(),
                },
            ))
            .unwrap();
        let frame = remote
            .wait_frame(|frame| frame.text.contains("新的不完整后缀"))
            .await;
        assert_eq!(frame.phase, TurnPhase::Streaming);
        assert_eq!(frame.text.matches("持久前缀").count(), 1);
        assert!(
            frame.text.contains("前文可能缺失") && !frame.text.contains("重连前临时文字"),
            "{}",
            frame.text
        );
        history_tx
            .send(transcript(vec![
                message("user", vec![json!({"text":{"text":"请求"}})]),
                message(
                    "assistant",
                    vec![
                        json!({"text":{"text":"持久前缀"}}),
                        json!({"text":{"text":"迟到历史标记"}}),
                    ],
                ),
            ]))
            .unwrap();
        let frame = remote
            .wait_frame(|frame| frame.text.contains("迟到历史标记"))
            .await;
        assert_eq!(frame.text.matches("持久前缀").count(), 1);
        assert_eq!(frame.text.matches("新的不完整后缀").count(), 1);
        assert!(frame.text.contains("前文可能缺失") && !frame.text.contains("committed"));
        assert!(
            frame
                .blocks
                .iter()
                .any(|b| matches!(b, Block::Assistant(text) if text == "新的不完整后缀"))
        );
        next_tx
            .unbounded_send(response(
                "observation",
                json!({"type":"gap","cursor":{"epoch":"peer","sequence":6}}),
            ))
            .unwrap();
        fixed.active_turn.as_mut().unwrap().journal_oid = "after-gap".into();
        fixed.requested_turn = fixed.active_turn.clone().map(TurnObservation::Pending);
        next_tx
            .unbounded_send(observation("snapshot", &fixed, true))
            .unwrap();
        peer.wait_for(|seen| count(seen, "transcript") == 3).await;
        next_tx
            .unbounded_send(event_frame(
                7,
                TurnEvent::TokenDelta {
                    content: "缺口之后的新后缀".into(),
                },
            ))
            .unwrap();
        let frame = remote
            .wait_frame(|frame| frame.text.contains("缺口之后的新后缀"))
            .await;
        assert!(!frame.text.contains("新的不完整后缀") && frame.text.contains("缺口"));
        next_tx
            .unbounded_send(event_frame(
                8,
                TurnEvent::ModelFinished {
                    finish_reason: "stop".into(),
                    usage: None,
                },
            ))
            .unwrap();
        next_tx
            .unbounded_send(event_frame(
                9,
                TurnEvent::ModelStarted {
                    provider: None,
                    model: "next".into(),
                },
            ))
            .unwrap();
        next_tx
            .unbounded_send(event_frame(
                10,
                TurnEvent::TokenDelta {
                    content: "正常新段".into(),
                },
            ))
            .unwrap();
        let frame = remote
            .wait_frame(|frame| frame.text.contains("正常新段"))
            .await;
        assert!(
            frame
                .blocks
                .iter()
                .any(|b| matches!(b, Block::Assistant(text) if text == "正常新段"))
        );
        assert_eq!(frame.text.matches("持久前缀").count(), 1);
        assert!(!frame.text.contains("committed"));
        remote.ctrl('d');
    };
    let ((), exit) = tokio::join!(driver, run);
    assert!(exit.result.is_ok());
    assert_eq!(exit.turn.unwrap().detach().await.cancel, None);
}

/// 同一 raw 按键路径核验反向回执，以及晚 NeedsConfiguration 不覆盖另一客户端的 Taken/live。
#[tokio::test]
async fn raw_cancel_keeps_reverse_receipts_and_late_probe_facts() {
    for late_probe in [false, true] {
        let (submit_tx, submit_rx) = oneshot::channel();
        let mut submit_rx = Some(submit_rx);
        let (cancel_tx, cancel_rx) = oneshot::channel();
        let mut cancel_rx = Some(cancel_rx);
        let mut cancel_count = 0;
        let peer = Peer::start(
            move |request| match request["operation"].as_str().unwrap() {
                "submit" => {
                    assert!(request["configuration"].is_null());
                    Reply::Hold(submit_rx.take().unwrap())
                }
                "subscribe" => {
                    let id = serde_json::from_value(request["turn_id"].clone()).unwrap();
                    Reply::Frames(vec![observation("snapshot", &view(id, false), true)])
                }
                "transcript" => Reply::Never,
                "cancel" => {
                    cancel_count += 1;
                    if cancel_count == 1 {
                        Reply::Hold(cancel_rx.take().unwrap())
                    } else {
                        cancelled(true)
                    }
                }
                _ => panic!("既有操作不得加载新配置或重发"),
            },
        );
        let mut harness = Harness::new();
        let remote = harness.remote();
        let run = harness.run(&peer.client);
        let driver = async {
            remote.type_line("hi");
            let seen = peer
                .wait_for(|seen| count(seen, "submit") == 1 && count(seen, "subscribe") == 1)
                .await;
            let id = submitted_turn(&seen);
            remote
                .wait_frame(|frame| frame.text.contains("pending Turn"))
                .await;
            remote.ctrl('c');
            peer.wait_for(|seen| count(seen, "cancel") == 1).await;
            if late_probe {
                cancel_tx
                    .send(response("cancelled", json!({"owned":true})))
                    .unwrap();
                remote
                    .wait_frame(|frame| frame.text.contains("后台已接管取消"))
                    .await;
                submit_tx
                    .send(response(
                        "acceptance",
                        json!({"acceptance":"needs_configuration"}),
                    ))
                    .unwrap();
                let frame = remote
                    .wait_frame(|frame| frame.text.contains("已停止后续配置"))
                    .await;
                assert!(
                    frame.text.contains("后台已接管取消")
                        && frame.text.contains("不表示该身份全局未执行"),
                    "{}",
                    frame.text
                );
                assert_eq!(frame.phase, TurnPhase::Cancelling);
                assert_eq!(frame.current_turn, Some(id));
            } else {
                submit_tx
                    .send(response(
                        "acceptance",
                        json!({"acceptance":"accepted","turn_id":id}),
                    ))
                    .unwrap();
                remote
                    .wait_frame(|frame| frame.text.contains("已确认接受"))
                    .await;
                peer.wait_for(|seen| count(seen, "subscribe") == 2).await;
                cancel_tx
                    .send(response("cancelled", json!({"owned":false})))
                    .unwrap();
                peer.wait_for(|seen| count(seen, "cancel") == 2).await;
                let frame = remote
                    .wait_frame(|frame| frame.text.contains("后台已接管取消"))
                    .await;
                assert_eq!(frame.current_turn, Some(id));
            }
            // 缓冲可能被归还，第二次 Ctrl+C 始终保留退出选择。
            remote.ctrl('c');
            id
        };
        let (id, exit) = tokio::join!(driver, run);
        assert!(exit.result.is_ok());
        let report = tokio::time::timeout(Duration::from_secs(1), exit.turn.unwrap().detach())
            .await
            .unwrap();
        assert_eq!(report.turn_id, Some(id));
        assert_eq!(report.cancel, Some(CancelStatus::Taken));
        let seen = peer.seen();
        assert_eq!(
            cancels(&seen),
            vec![json!(id); if late_probe { 1 } else { 2 }]
        );
        assert_eq!(count(&seen, "submit"), 1);
        assert_eq!(count(&seen, "open"), 0);
    }
}

/// 同 Session 的 active 可以不同于句柄 requested；新用户动作必须选择 active 的句柄。
async fn select_new_active_after_completion(cancel: Option<KeyEvent>) {
    let old = lattice_api::local::new_turn_id();
    let active = lattice_api::local::new_turn_id();
    let mut subscriptions = 0;
    let peer = Peer::start(
        move |request| match request["operation"].as_str().unwrap() {
            "subscribe" => {
                subscriptions += 1;
                let selected: TurnId = serde_json::from_value(request["turn_id"].clone()).unwrap();
                if subscriptions == 1 {
                    assert_eq!(selected, old);
                    Reply::Now(observation("finished", &view(old, true), false))
                } else if selected == old {
                    let mut fixed = view(old, true);
                    fixed.active_turn = view(active, false).active_turn;
                    Reply::Now(observation("finished", &fixed, false))
                } else {
                    assert_eq!(selected, active);
                    Reply::Frames(vec![
                        observation("snapshot", &view(active, false), true),
                        event_frame(
                            2,
                            TurnEvent::TokenDelta {
                                content: "B 的重连新文字".into(),
                            },
                        ),
                    ])
                }
            }
            "cancel" => cancelled(true),
            _ => committed_reads(request, old)
                .unwrap_or_else(|| panic!("只读选择不能提交、恢复或加载配置：{request:?}")),
        },
    );
    let mut harness = Harness::new();
    let remote = harness.remote();
    let run = harness.run(&peer.client);
    let driver = async {
        remote.type_line("/reconnect");
        remote
            .wait_frame(|frame| frame.text.contains("committed"))
            .await;
        remote.type_line("/reconnect");
        remote
            .wait_frame(|frame| frame.current_turn == Some(active))
            .await;
        assert!(cancels(&peer.seen()).is_empty(), "重连不能隐式取消");
        if let Some(key) = cancel {
            remote.press(key.code, key.modifiers);
            let seen = peer.wait_for(|seen| count(seen, "cancel") != 0).await;
            assert_eq!(cancels(&seen), vec![json!(active)], "新 Esc 必须只取消 B");
            remote
                .wait_frame(|frame| {
                    frame
                        .text
                        .contains(&format!("后台已接管取消；持久终态仍待确认：{active}"))
                })
                .await;
        } else {
            remote.type_line("/reconnect");
            let seen = peer.wait_for(|seen| count(seen, "subscribe") >= 3).await;
            assert_eq!(
                seen.iter()
                    .rev()
                    .find(|r| r["operation"] == "subscribe")
                    .unwrap()["turn_id"],
                json!(active),
                "再次 /reconnect 必须选择 B，而不是旧句柄 A"
            );
            remote
                .wait_frame(|frame| {
                    frame.current_turn == Some(active)
                        && frame.phase == TurnPhase::Streaming
                        && frame.text.contains("B 的重连新文字")
                })
                .await;
        }
        remote.ctrl('d');
    };
    let ((), exit) = tokio::join!(driver, run);
    assert!(exit.result.is_ok());
    let report = exit.turn.unwrap().detach().await;
    assert_eq!(report.turn_id, Some(active));
    assert_eq!(report.cancel, cancel.map(|_| CancelStatus::Taken));
    let seen = peer.seen();
    assert_eq!(count(&seen, "submit"), 0);
    assert_eq!(
        count(&seen, "open"),
        0,
        "freeze_configuration 必先 open metadata"
    );
    assert_eq!(
        cancels(&seen),
        if cancel.is_some() {
            vec![json!(active)]
        } else {
            vec![]
        }
    );
}

#[tokio::test]
async fn new_active_turn_esc_targets_the_displayed_turn() {
    select_new_active_after_completion(Some(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))).await;
}

#[tokio::test]
async fn new_active_turn_reconnect_selects_its_new_text() {
    select_new_active_after_completion(None).await;
}

#[tokio::test]
async fn new_active_turn_first_ctrl_c_targets_the_displayed_turn() {
    select_new_active_after_completion(Some(KeyEvent::new(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL,
    )))
    .await;
}

/// 旧句柄的在途 Cancel 在 detach 中失败，交接报告仍须保留其原目标与具体诊断。
#[tokio::test]
async fn detached_cancel_keeps_the_original_target_diagnostic() {
    let target = lattice_api::local::new_turn_id();
    let diagnostic = "original target cancellation denied";
    let error = response(
        "error",
        json!(LocalError {
            kind: lattice_api::local::LocalErrorKind::Permission,
            message: diagnostic.into(),
            recovery: None,
        }),
    );
    let retry_error = error.clone();
    let (cancel_tx, cancel_rx) = oneshot::channel();
    let mut cancel_rx = Some(cancel_rx);
    let peer = Peer::start(
        move |request| match request["operation"].as_str().unwrap() {
            "query" => Reply::Now(response("view", json!(view(target, false)))),
            "subscribe" => Reply::Now(observation("snapshot", &view(target, false), true)),
            "transcript" => transcript_page(),
            "cancel" => match cancel_rx.take() {
                Some(receiver) => Reply::Hold(receiver),
                None => Reply::Now(retry_error.clone()),
            },
            _ => panic!("观察及退出不提交或加载配置"),
        },
    );
    let mut harness = Harness::new();
    let remote = harness.remote();
    let run = harness.run(&peer.client);
    let driver = async {
        remote.type_line("/reconnect");
        remote
            .wait_frame(|frame| frame.current_turn == Some(target))
            .await;
        remote.press(KeyCode::Esc, KeyModifiers::NONE);
        peer.wait_for(|seen| count(seen, "cancel") == 1).await;
        remote.ctrl('d');
    };
    let ((), exit) = tokio::join!(driver, run);
    assert!(exit.result.is_ok());
    let report = tokio::select! {
        biased;
        report = exit.turn.unwrap().detach() => report,
        _ = async {
            cancel_tx.send(error).unwrap();
            std::future::pending::<()>().await;
        } => unreachable!(),
    };
    assert_eq!(report.turn_id, Some(target));
    assert_eq!(report.cancel, Some(CancelStatus::Unknown));
    assert_eq!(cancels(&peer.seen()), vec![json!(target); 2]);
    let lines = detach_lines(report);
    assert!(
        lines
            .iter()
            .any(|line| line.contains(diagnostic) && line.contains(&target.to_string())),
        "旧目标具体取消诊断在公开交接中丢失：{lines:?}"
    );
}

#[tokio::test]
async fn switching_targets_keeps_old_receipts_and_diagnostics_out_of_the_new_state() {
    for old_taken in [false, true] {
        let old = lattice_api::local::new_turn_id();
        let active = lattice_api::local::new_turn_id();
        let (old_events, old_stream) = unbounded();
        let mut old_stream = Some(old_stream);
        let (old_reply, old_receipt) = oneshot::channel();
        let mut old_receipt = Some(old_receipt);
        let peer = Peer::start(
            move |request| match request["operation"].as_str().unwrap() {
                "query" => Reply::Now(response("view", json!(view(old, false)))),
                "subscribe" if request["turn_id"] == json!(old) => {
                    Reply::Stream(old_stream.take().expect("one A subscription"))
                }
                "subscribe" => {
                    assert_eq!(request["turn_id"], json!(active));
                    Reply::Frames(vec![
                        observation("snapshot", &view(active, false), true),
                        event_frame(
                            2,
                            TurnEvent::TokenDelta {
                                content: "B is still running".into(),
                            },
                        ),
                    ])
                }
                "cancel" if request["turn_id"] == json!(old) => {
                    Reply::Hold(old_receipt.take().expect("closed A cannot retry"))
                }
                "cancel" => {
                    assert_eq!(request["turn_id"], json!(active));
                    cancelled(true)
                }
                "transcript" | "history" => Reply::Never,
                _ => panic!("read-only target handoff: {request:?}"),
            },
        );
        let mut harness = Harness::new();
        let remote = harness.remote();
        let run = harness.run(&peer.client);
        let driver = async {
            remote.type_line("/reconnect");
            peer.wait_for(|seen| count(seen, "subscribe") == 1).await;
            old_events
                .unbounded_send(observation("snapshot", &view(old, false), true))
                .unwrap();
            remote
                .wait_frame(|frame| frame.current_turn == Some(old))
                .await;
            peer.wait_for(|seen| count(seen, "transcript") == 1).await;
            remote.press(KeyCode::Esc, KeyModifiers::NONE);
            peer.wait_for(|seen| count(seen, "cancel") == 1).await;
            let mut finished = view(old, true);
            finished.active_turn = view(active, false).active_turn;
            old_events
                .unbounded_send(observation("finished", &finished, false))
                .unwrap();
            remote
                .wait_frame(|frame| frame.current_turn == Some(active))
                .await;
            if old_taken {
                remote.type_line("/reconnect");
            } else {
                remote.press(KeyCode::Esc, KeyModifiers::NONE);
                // A 的正文和取消均未返回，B 已取得自己的取消回执。
                remote
                    .wait_frame(|frame| {
                        frame
                            .text
                            .contains(&format!("后台已接管取消；持久终态仍待确认：{active}"))
                    })
                    .await;
            }
            remote
                .wait_frame(|frame| frame.text.contains("B is still running"))
                .await;
            old_reply.send(if old_taken {
                response("cancelled", json!({"owned":true}))
            } else {
                response("error", json!({"kind":"permission", "message":"A cancellation denied", "recovery":null}))
            }).unwrap();
            let marker = if old_taken {
                "历史交接：后台已接管取消"
            } else {
                "A cancellation denied"
            };
            let frame = remote.wait_frame(|frame| frame.text.contains(marker)).await;
            assert_eq!(frame.current_turn, Some(active));
            assert_eq!(
                frame.cancel_status,
                if old_taken {
                    None
                } else {
                    Some(CancelStatus::Taken)
                }
            );
            assert_eq!(
                frame.phase,
                if old_taken {
                    TurnPhase::Streaming
                } else {
                    TurnPhase::Cancelling
                }
            );
            let state = frame.state.unwrap();
            assert_eq!(*state.view, view(active, false));
            assert_eq!(state.confirmation, CompletionConfirmation::Unknown);
            assert_eq!(state.error, None);
            assert!(
                frame
                    .text
                    .contains(&format!("历史交接：Turn {old}：Completed"))
            );
            assert!(frame.text.contains("完成确认 Confirmed"));
            assert!(
                frame
                    .text
                    .lines()
                    .any(|line| line.contains(marker) && line.contains(&old.to_string()))
            );
            remote.ctrl('d');
        };
        let ((), exit) = tokio::join!(driver, run);
        assert!(exit.result.is_ok());
        assert!(exit.handoffs.is_empty());
        let report = exit.turn.unwrap().detach().await;
        assert_eq!(report.turn_id, Some(active));
        assert_eq!(
            report.cancel,
            if old_taken {
                None
            } else {
                Some(CancelStatus::Taken)
            }
        );
        assert_eq!(report.cancel_error, None);
        let seen = peer.seen();
        assert_eq!(
            cancels(&seen),
            if old_taken {
                vec![json!(old)]
            } else {
                vec![json!(old), json!(active)]
            }
        );
        assert_eq!(count(&seen, "submit"), 0);
        assert_eq!(count(&seen, "open"), 0);
    }
}

#[tokio::test]
async fn busy_submission_can_select_and_cancel_the_visible_active_turn() {
    let active = lattice_api::local::new_turn_id();
    let mut subscriptions = 0;
    let peer = Peer::start(
        move |request| match request["operation"].as_str().unwrap() {
            "submit" => Reply::Now(response(
                "error",
                json!({"kind":"busy", "message":"session is busy", "recovery":null}),
            )),
            "subscribe" => {
                subscriptions += 1;
                if subscriptions == 1 {
                    Reply::Never
                } else if request["turn_id"] == json!(active) {
                    Reply::Now(observation("snapshot", &view(active, false), true))
                } else {
                    let mut fixed = view(active, false);
                    fixed.requested_turn = None;
                    Reply::Now(observation("snapshot", &fixed, false))
                }
            }
            "cancel" => {
                assert_eq!(request["turn_id"], json!(active));
                cancelled(true)
            }
            "transcript" => transcript_page(),
            _ => panic!("Busy must not freeze configuration: {request:?}"),
        },
    );
    let mut harness = Harness::new();
    let remote = harness.remote();
    let run = harness.run(&peer.client);
    let driver = async {
        remote.type_line("hi");
        let seen = peer
            .wait_for(|seen| count(seen, "submit") == 1 && count(seen, "subscribe") == 1)
            .await;
        let original = submitted_turn(&seen);
        remote
            .wait_frame(|frame| frame.input == "hi" && frame.text.contains("session is busy"))
            .await;
        remote.press(KeyCode::Esc, KeyModifiers::NONE);
        remote.type_line("/reconnect");
        remote
            .wait_frame(|frame| frame.current_turn == Some(active))
            .await;
        remote.press(KeyCode::Esc, KeyModifiers::NONE);
        remote
            .wait_frame(|frame| {
                frame
                    .text
                    .contains(&format!("后台已接管取消；持久终态仍待确认：{active}"))
            })
            .await;
        let frame = remote
            .wait_frame(|frame| frame.text.contains("历史交接：请求"))
            .await;
        assert!(
            frame
                .text
                .lines()
                .any(|line| line.contains("历史交接：请求")
                    && line.contains(&original.to_string())
                    && line.contains("session is busy"))
        );
        assert_eq!(frame.current_turn, Some(active));
        remote.ctrl('d');
    };
    let ((), exit) = tokio::join!(driver, run);
    assert!(exit.result.is_ok());
    assert!(exit.handoffs.is_empty());
    let report = exit.turn.unwrap().detach().await;
    assert_eq!(report.turn_id, Some(active));
    assert_eq!(report.cancel, Some(CancelStatus::Taken));
    let seen = peer.seen();
    assert_eq!(count(&seen, "submit"), 1);
    assert_eq!(count(&seen, "open"), 0);
    assert_eq!(cancels(&seen), vec![json!(active)]);
}

#[tokio::test]
async fn recovered_read_target_and_original_control_target_are_not_interchangeable() {
    let recovered = lattice_api::local::new_turn_id();
    let mut original = None;
    let mut recovered_subscriptions = 0;
    let peer = Peer::start(
        move |request| match request["operation"].as_str().unwrap() {
            "submit" => {
                original = Some(
                    serde_json::from_value::<TurnId>(request["request"]["turn_id"].clone())
                        .unwrap(),
                );
                Reply::Now(response(
                    "acceptance",
                    json!({"acceptance":"recovered", "turn":committed_turn(recovered)}),
                ))
            }
            "subscribe" if request["turn_id"] == json!(recovered) => {
                recovered_subscriptions += 1;
                if recovered_subscriptions == 1 {
                    Reply::Now(observation("finished", &view(recovered, true), false))
                } else {
                    assert_eq!(recovered_subscriptions, 2, "新目标不应继续订阅恢复目标");
                    let mut fixed = view(recovered, true);
                    fixed.active_turn = view(original.unwrap(), false).active_turn;
                    let mut frame = observation("finished", &fixed, false);
                    frame["value"]["error"] = json!({
                        "kind":"store_unavailable",
                        "message":"store unavailable; inspect durable state",
                        "recovery":null,
                    });
                    Reply::Now(frame)
                }
            }
            "subscribe" if recovered_subscriptions < 2 => Reply::Never,
            "subscribe" => {
                assert_eq!(request["turn_id"], json!(original.unwrap()));
                Reply::Now(observation(
                    "snapshot",
                    &view(original.unwrap(), false),
                    true,
                ))
            }
            "cancel" => {
                assert_eq!(request["turn_id"], json!(original.unwrap()));
                cancelled(true)
            }
            "history" | "transcript" => Reply::Never,
            _ => panic!("recovery/reconnect must not submit configuration: {request:?}"),
        },
    );
    let mut harness = Harness::new();
    let remote = harness.remote();
    let run = harness.run(&peer.client);
    let driver = async {
        remote.type_line("hi");
        let original = submitted_turn(&peer.wait_for(|seen| count(seen, "submit") == 1).await);
        remote
            .wait_frame(|frame| {
                frame
                    .state
                    .as_ref()
                    .is_some_and(|state| state.confirmation == CompletionConfirmation::Confirmed)
            })
            .await;
        remote.type_line("/reconnect");
        let frame = remote
            .wait_frame(|frame| frame.current_turn == Some(original))
            .await;
        assert_eq!(
            frame.state.unwrap().confirmation,
            CompletionConfirmation::Confirmed,
            "同观察目标重连保留确认"
        );
        assert!(cancels(&peer.seen()).is_empty(), "恢复与重连不转移原意图");
        remote.press(KeyCode::Esc, KeyModifiers::NONE);
        let frame = remote
            .wait_frame(|frame| {
                frame
                    .text
                    .contains(&format!("后台已接管取消；持久终态仍待确认：{original}"))
                    && frame
                        .state
                        .as_ref()
                        .is_some_and(|state| state.phase == ObservationPhase::Live)
            })
            .await;
        assert_eq!(
            frame.state.unwrap().confirmation,
            CompletionConfirmation::Unknown
        );
        let frame = remote
            .wait_frame(|frame| frame.text.contains("历史交接：请求"))
            .await;
        assert!(
            frame
                .text
                .lines()
                .any(|line| line.contains("历史交接：请求")
                    && line.contains(&original.to_string())
                    && line.contains(&recovered.to_string()))
        );
        remote.ctrl('d');
        original
    };
    let (original, exit) = tokio::join!(driver, run);
    assert!(exit.result.is_ok());
    assert!(exit.handoffs.is_empty());
    let turn = exit.turn.unwrap();
    assert_eq!(turn.turn_id(), Some(original));
    assert_eq!(turn.observed_turn_id(), Some(original));
    let report = turn.detach().await;
    assert_eq!(report.submission, None, "新句柄只有观察，没有重发原输入");
    assert_eq!(report.cancel, Some(CancelStatus::Taken));
    assert_eq!(count(&peer.seen(), "submit"), 1);
    assert_eq!(count(&peer.seen(), "open"), 0);
    assert_eq!(cancels(&peer.seen()), vec![json!(original)]);
}

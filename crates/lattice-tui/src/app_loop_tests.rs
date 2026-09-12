// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 以私有 Unix 对端替代后台，驱动真实 `Connection`、真实事件循环与退出收尾；
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

/// 对端对单次请求的应答方式；连接在应答后保持打开，由测试结束时统一丢弃。
enum Reply {
    Now(Value),
    Frames(Vec<Value>),
    Hold(oneshot::Receiver<Value>),
    Never,
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
}

fn frame_of(app: &App) -> Frame {
    Frame {
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

/// 提交在途、取消先返回 owned=false，然后在提交回执到达前退出：
/// 收尾必须保留 Connection 直到有界处理回执，按接受/拒绝/未知分别处理并带原 ID。
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
        // 终端复原后才交付回执，对应“退出先于 SelectedTurn”。
        let started = std::time::Instant::now();
        let lines = tokio::select! {
            lines = settle_exit(&peer.client, SESSION, loop_exit.control) => lines,
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
    let lines = settle_exit(&peer.client, SESSION, loop_exit.control).await;
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

/// 探测得 NeedsConfiguration 时已有取消意图：不再打开会话加载配置、不发配置提交、不观察。
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
        // 初始帧也是 Idle，须以“未接受”提示区分提交前后的帧。
        let frame = remote
            .wait_frame(|frame| frame.phase == TurnPhase::Idle && frame.text.contains("未接受"))
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
    let lines = settle_exit(&peer.client, SESSION, loop_exit.control).await;
    let seen = peer.seen();
    assert_eq!(count(&seen, "submit"), 1, "{seen:?}");
    assert_eq!(count(&seen, "open"), 0, "取消后不得再走配置提交: {seen:?}");
    assert_eq!(count(&seen, "subscribe"), 0, "{seen:?}");
    assert_eq!(cancels(&seen), vec![json!(turn_id)]);
    assert!(lines.is_empty(), "{lines:?}");
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
    let lines = settle_exit(&peer.client, SESSION, loop_exit.control).await;
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
    let mut connection =
        Connection::start(peer.client.clone(), SESSION.into(), Some(turn_id), None);
    let Some(Update::Snapshot {
        view,
        blocks,
        owned,
        confirmed,
        error,
    }) = tokio::time::timeout(Duration::from_secs(5), connection.next())
        .await
        .unwrap()
    else {
        panic!("expected pending snapshot");
    };
    assert!(owned && !confirmed && blocks.is_none() && error.is_some());
    let mut harness = Harness::new();
    harness
        .app
        .apply_snapshot(*view, blocks, owned, confirmed, error);
    let frame = frame_of(&harness.app);
    assert!(frame.connected, "正文失败不得推断断开: {}", frame.text);
    assert_eq!(frame.phase, TurnPhase::Streaming);
    assert_eq!(frame.current_turn, Some(turn_id));
    assert!(frame.text.contains("正文不可用"), "{}", frame.text);
    assert!(!frame.text.contains("完成快照"), "{}", frame.text);
    assert!(!frame.text.contains("offline"), "{}", frame.text);
}

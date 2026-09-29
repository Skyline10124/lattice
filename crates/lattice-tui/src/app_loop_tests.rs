// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 真实 LocalTurn、Unix socket 与 run_loop；对端只替代 IPC 系统边界。
use super::*;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{oneshot, watch};

const SESSION: &str = "11111111111141118111111111111111";
fn sid() -> SessionId {
    SessionId::parse(SESSION).unwrap()
}
fn key(id: TurnId, step: u32, attempt: u32) -> ModelAttemptKey {
    ModelAttemptKey {
        turn_id: id,
        model_step_ordinal: step,
        attempt_ordinal: attempt,
    }
}
fn view(id: TurnId, closed: bool) -> ObservationSnapshot {
    let mut value = json!({
        "session":{"session_id":SESSION,"project_store_key":"0".repeat(64),"current":1,"active_turn":id,"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"},
        "current_agent":"unused","selected_turn":{"session_id":SESSION,"turn_id":id,"user_node":1,"agent_name":"unused","accepted_at":"2026-01-01T00:00:00Z","ended_at":null,"summary":null,"usage":{"kind":"partial","known":{"prompt_tokens":0,"completion_tokens":0,"total_tokens":0},"unknown_attempts":1},"steps":[]},
        "active":{"key":{"turn_id":id,"ordinal":0},"state":{"state":"requesting","attempt":1}},
        "history":{"session_id":SESSION,"head":1},"attempt":{"key":key(id,0,1),"segments":[]},"cursor":{"epoch":"peer","sequence":0},"owned":true
    });
    if closed {
        value["session"]["current"] = json!(2);
        value["history"]["head"] = json!(2);
        value["selected_turn"]["steps"] = json!([{ "key":{"turn_id":id,"ordinal":0}, "attempts":[{"attempt":1,"origin":"open_ai_compatible","usage":null,"end":{"end":"completed","step":{"session_id":SESSION,"turn_id":id,"model_step_ordinal":0}}}], "state":{"state":"closed","end":{"end":"finish","node":2},"slots":[]} }]);
        value["session"]["active_turn"] = Value::Null;
        value["selected_turn"]["ended_at"] = json!("2026-01-01T00:00:01Z");
        value["selected_turn"]["summary"] = json!({"outcome":"completed","notice":null});
        value["active"] = Value::Null;
        value["attempt"] = Value::Null;
        value["owned"] = json!(false);
    }
    serde_json::from_value(value).unwrap()
}
fn response(kind: &str, value: Value) -> Value {
    json!({"response":kind,"value":value})
}
fn snapshot(view: &ObservationSnapshot) -> Value {
    response("observation", json!({"type":"snapshot","view":view}))
}
fn accepted(id: TurnId) -> Value {
    response(
        "acceptance",
        json!({"acceptance":"accepted","turn":lattice_api::local::TurnView::from(view(id,false).selected_turn.as_ref().unwrap())}),
    )
}
fn error(message: &str) -> Value {
    response(
        "error",
        json!({"kind":"store_unavailable","message":message,"recovery":null,"store":null}),
    )
}
fn event(seq: u64, event: TurnEvent) -> Value {
    response(
        "observation",
        json!({"type":"event","cursor":{"epoch":"peer","sequence":seq},"event":event}),
    )
}
fn history_entries(view: &ObservationSnapshot) -> Value {
    let id = view.selected_turn.as_ref().unwrap().turn_id;
    let mut entries = vec![
        json!({"node":1,"agent_name":"unused","content":{"kind":"user_message","parent":0,"turn_id":id,"message":{"role":"user","parts":[{"semantic":{"text":{"text":"original prompt"}}}]}}}),
    ];
    if view.history.head.0 == 2 {
        entries.push(json!({"node":2,"agent_name":"unused","content":{"kind":"step","parent":1,"turn_id":id,"step_ordinal":0,"content":{"assistant":{"role":"assistant","origin":"open_ai_compatible","parts":[{"semantic":{"text":{"text":"live body"}}}]},"results":[]}}}));
    }
    json!(entries)
}
fn history(view: &ObservationSnapshot) -> Value {
    let fragments:Vec<_>=history_entries(view).as_array().unwrap().iter().map(|entry|json!({"agent_name":entry["agent_name"],"fragment":{"node":entry["node"],"utf8_offset":0,"json_fragment":serde_json::to_string(&entry["content"]).unwrap(),"complete":true}})).collect();
    response(
        "history",
        json!({"anchor":view.history,"fragments":fragments,"next":null}),
    )
}
fn finished(view: &ObservationSnapshot) -> Value {
    response(
        "observation",
        json!({"type":"finished","turn":{"session_id":sid(),"turn_id":view.selected_turn.as_ref().unwrap().turn_id},"cursor":view.cursor,"history":{"anchor":view.history,"entries":history_entries(view)}}),
    )
}

enum Reply {
    Now(Value),
    Hold(oneshot::Receiver<Value>),
    Close(oneshot::Receiver<()>),
    Stream(UnboundedReceiver<Value>),
    Never,
}
struct Peer {
    _root: tempfile::TempDir,
    client: LocalClient,
    seen: watch::Receiver<Vec<Value>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Peer {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Peer {
    fn start(mut handler: impl FnMut(&Value) -> Reply + Send + 'static) -> Self {
        let root = tempfile::tempdir().unwrap();
        let layout = lattice_api::StateLayout::in_state_root(root.path());
        let local = layout.lattice_root().join("local");
        std::fs::create_dir_all(&local).unwrap();
        std::fs::set_permissions(&local, std::fs::Permissions::from_mode(0o700)).unwrap();
        let socket = local.join("api.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
        let (record, seen) = watch::channel(Vec::new());
        let task = tokio::spawn(async move {
            let mut tasks = tokio::task::JoinSet::new();
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut magic = [0; 8];
                stream.read_exact(&mut magic).await.unwrap();
                assert_eq!(&magic, b"LTLOCAL3");
                let len = stream.read_u32().await.unwrap();
                assert!(len <= 16 * 1024 * 1024);
                let mut bytes = vec![0; len as usize];
                stream.read_exact(&mut bytes).await.unwrap();
                let request: Value = serde_json::from_slice(&bytes).unwrap();
                let reply = handler(&request);
                record.send_modify(|seen| seen.push(request));
                tasks.spawn(async move {
                    let mut seq = 0;
                    match reply {
                        Reply::Now(value) => send(&mut stream, &value, &mut seq).await,
                        Reply::Hold(receiver) => {
                            if let Ok(value) = receiver.await {
                                send(&mut stream, &value, &mut seq).await;
                            }
                        }
                        Reply::Close(receiver) => {
                            let _ = receiver.await;
                            return;
                        }
                        Reply::Stream(mut receiver) => {
                            while let Some(value) = receiver.next().await {
                                send(&mut stream, &value, &mut seq).await;
                            }
                        }
                        Reply::Never => {}
                    }
                    let _stream = stream;
                    std::future::pending::<()>().await;
                });
            }
        });
        Self {
            _root: root,
            client: LocalClient::new(layout),
            seen,
            task,
        }
    }
    async fn wait(&self, predicate: impl FnMut(&Vec<Value>) -> bool) -> Vec<Value> {
        tokio::time::timeout(
            Duration::from_secs(5),
            self.seen.clone().wait_for(predicate),
        )
        .await
        .unwrap()
        .unwrap()
        .clone()
    }
}
async fn send(stream: &mut UnixStream, value: &Value, seq: &mut u64) {
    let bytes = serde_json::to_vec(&json!({"message_id":*seq,"offset":0,"json_fragment":serde_json::to_string(value).unwrap(),"complete":true})).unwrap();
    *seq += 1;
    if stream.write_all(b"LTLOCAL3").await.is_err() {
        return;
    }
    let _ = stream.write_u32(bytes.len() as u32).await;
    let _ = stream.write_all(&bytes).await;
}
fn count(seen: &[Value], op: &str) -> usize {
    seen.iter().filter(|r| r["operation"] == op).count()
}
fn cancels(seen: &[Value]) -> Vec<Value> {
    seen.iter()
        .filter(|r| r["operation"] == "cancel")
        .map(|r| r["target"]["turn_id"].clone())
        .collect()
}
fn submitted(seen: &[Value]) -> TurnId {
    serde_json::from_value(
        seen.iter().find(|r| r["operation"] == "submit").unwrap()["request"]["turn_id"].clone(),
    )
    .unwrap()
}

#[derive(Clone)]
struct Frame {
    phase: TurnPhase,
    current: Option<TurnId>,
    connected: bool,
    input: String,
    text: String,
    cancel: Option<CancelStatus>,
    finished: bool,
}
fn frame(app: &App) -> Frame {
    Frame {
        phase: app.phase,
        current: app.current_turn,
        connected: app.connected,
        input: app.input.text(),
        text: crate::view::transcript_lines(app, 200)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
        cancel: app.cancel_status,
        finished: app
            .state
            .as_ref()
            .is_some_and(|s| s.phase == ObservationPhase::Finished),
    }
}
struct Harness {
    options: TuiOptions,
    app: App,
    events: UnboundedReceiver<std::io::Result<Event>>,
    observer: watch::Sender<Frame>,
    remote: Remote,
    _root: tempfile::TempDir,
}
#[derive(Clone)]
struct Remote {
    keys: UnboundedSender<std::io::Result<Event>>,
    frames: watch::Receiver<Frame>,
}
impl Harness {
    fn new(new_session: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let options = TuiOptions {
            session_id: (!new_session).then(|| SESSION.into()),
            agent: "unused".into(),
            session_dir: root.path().join("history"),
            agents_dir: root.path().join("agents"),
            project_root: root.path().into(),
        };
        let app = App::new(
            SessionContext {
                session_id: (!new_session).then(sid),
                active_agent: "unused".into(),
            },
            false,
            &options.session_dir,
        );
        let (keys, events) = unbounded();
        let (observer, frames) = watch::channel(frame(&app));
        Self {
            options,
            app,
            events,
            observer,
            remote: Remote { keys, frames },
            _root: root,
        }
    }
    async fn run(&mut self, client: &LocalClient, turn: Option<LocalTurn>) -> LoopExit {
        let observer = self.observer.clone();
        run_loop(
            &self.options,
            &mut self.app,
            client,
            turn,
            &mut self.events,
            |app| {
                observer.send_replace(frame(app));
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
    fn ctrl(&self, c: char) {
        self.press(KeyCode::Char(c), KeyModifiers::CONTROL);
    }
    fn line(&self, text: &str) {
        for c in text.chars() {
            self.press(KeyCode::Char(c), KeyModifiers::NONE);
        }
        self.press(KeyCode::Enter, KeyModifiers::NONE);
    }
    async fn wait(&self, predicate: impl FnMut(&Frame) -> bool) -> Frame {
        tokio::time::timeout(
            Duration::from_secs(5),
            self.frames.clone().wait_for(predicate),
        )
        .await
        .unwrap()
        .unwrap()
        .clone()
    }
}

#[tokio::test]
async fn live_observation_waits_for_user_history_before_drawing_header() {
    let id = lattice_api::local::new_turn_id();
    let live = view(id, false);
    let (history_tx, history_rx) = oneshot::channel();
    let mut history_rx = Some(history_rx);
    let response_history = history(&live);
    let peer = Peer::start(move |r| match r["operation"].as_str().unwrap() {
        "subscribe" => Reply::Now(snapshot(&live)),
        "history" => Reply::Hold(history_rx.take().unwrap()),
        _ => panic!("read only"),
    });
    let mut h = Harness::new(false);
    let remote = h.remote.clone();
    let driver = async {
        let before = remote.wait(|f| f.phase == TurnPhase::Streaming).await;
        assert!(!before.text.contains("── unused ─"));
        peer.wait(|s| count(s, "history") == 1).await;
        history_tx.send(response_history).unwrap();
        let after = remote.wait(|f| f.text.contains("original prompt")).await;
        assert_eq!(
            after.text.matches("── unused ─").count(),
            1,
            "{}",
            after.text
        );
        assert_eq!(after.text.matches(" #1 ──").count(), 2);
        assert!(!after.text.contains("live body"));
        remote.line("/quit");
    };
    let (exit, ()) = tokio::join!(
        h.run(
            &peer.client,
            Some(peer.client.observe(Some(sid()), Some(id)))
        ),
        driver
    );
    exit.result.unwrap();
}

#[test]
fn unaccepted_submissions_never_start_an_assistant_segment() {
    use lattice_api::local::{LocalError, LocalErrorKind, NotSubmittedStage};
    for kind in [
        Some(LocalErrorKind::Busy),
        Some(LocalErrorKind::BadRequest),
        None,
    ] {
        let mut h = Harness::new(true);
        h.app.input.set_text("prompt");
        h.app.apply_action(KeyAction::Submit).unwrap();
        let turn_id = h.app.current_turn.unwrap();
        let update = match kind {
            Some(kind) => LocalUpdate::Submission {
                turn_id,
                result: Err(LocalError {
                    kind,
                    message: "not accepted".into(),
                    recovery: None,
                    store: None,
                }),
            },
            None => LocalUpdate::NotSubmitted {
                turn_id,
                stage: NotSubmittedStage::FreezingConfiguration,
            },
        };
        h.app.apply_update(update);
        assert_eq!(h.app.input.text(), "prompt");
        assert!(!frame(&h.app).text.contains("── unused ─"));
        assert!(
            !h.app
                .blocks
                .iter()
                .any(|block| matches!(block, Block::AssistantStart { .. }))
        );
    }
}

#[test]
fn off_path_summary_is_independent_and_history_assigns_observed_ordinals() {
    let observed = lattice_api::local::new_turn_id();
    let other = lattice_api::local::new_turn_id();
    let mut h = Harness::new(false);
    let closed = view(observed, true);
    h.app.apply_state(LocalState {
        view: Box::new(closed.clone()),
        phase: ObservationPhase::Finished,
        error: None,
    });
    assert!(!frame(&h.app).text.contains("── unused ─"));
    let other_history = serde_json::from_value(
        json!({"anchor":view(other,false).history,"entries":history_entries(&view(other,false))}),
    )
    .unwrap();
    h.app
        .replace_history(crate::session::history_blocks(other_history));
    let lines = crate::view::transcript_lines(&h.app, 200);
    assert!(
        !frame(&h.app).text.contains("── unused ─"),
        "off-path outcome must not start another Turn's assistant"
    );
    let summary = lines
        .iter()
        .find(|line| line.to_string().contains("completed"))
        .unwrap();
    assert!(
        summary
            .spans
            .iter()
            .skip(1)
            .all(|span| span.style == crate::view::role_style(crate::view::Role::DimMeta))
    );
    let loaded =
        serde_json::from_value(json!({"anchor":closed.history,"entries":history_entries(&closed)}))
            .unwrap();
    h.app
        .replace_history(crate::session::history_blocks(loaded));
    assert_eq!(frame(&h.app).text.matches("── unused ─").count(), 1);
}

#[test]
fn history_and_local_submission_use_user_positions_not_node_numbers() {
    let first = lattice_api::local::new_turn_id();
    let second = lattice_api::local::new_turn_id();
    let mut entries = history_entries(&view(first, true))
        .as_array()
        .unwrap()
        .clone();
    entries.push(json!({"node":7,"agent_name":"second-agent","content":{"kind":"user_message","parent":2,"turn_id":second,"message":{"role":"user","parts":[{"semantic":{"text":{"text":"second prompt"}}}]}}}));
    let history =
        serde_json::from_value(json!({"anchor":{"session_id":sid(),"head":7},"entries":entries}))
            .unwrap();
    let blocks = crate::session::history_blocks(history);
    let ordinals: Vec<_> = blocks
        .iter()
        .filter_map(|block| match block {
            Block::UserTurn(id, ordinal) => Some((*id, ordinal.get())),
            _ => None,
        })
        .collect();
    assert_eq!(ordinals, [(first, 1), (second, 2)]);
    let mut h = Harness::new(false);
    h.app.replace_history(blocks);
    assert_eq!(
        (
            h.app.model.as_ref(),
            h.app.provider.as_ref(),
            h.app.context_tokens
        ),
        (None, None, None)
    );
    h.app.input.set_text("third prompt");
    assert_eq!(
        h.app.apply_action(KeyAction::Submit).as_deref(),
        Some("third prompt")
    );
    let text = frame(&h.app).text;
    let user_headers: Vec<_> = text
        .lines()
        .filter(|line| line.contains("── USER ─"))
        .collect();
    assert_eq!(user_headers.len(), 3, "{text}");
    for (line, suffix) in user_headers.iter().zip([" #1 ──", " #2 ──", " #3 ──"]) {
        assert!(line.ends_with(suffix), "{line}");
    }
    assert!(
        text.lines()
            .any(|line| line.contains("── unused ─") && line.ends_with(" #1 ──"))
    );
    assert!(!text.contains("── second-agent ─"), "no reply yet");
}

#[tokio::test]
async fn accepted_and_existing_start_one_assistant_header_before_body_and_history() {
    for acceptance in ["accepted", "existing"] {
        let (submit_tx, submit_rx) = oneshot::channel();
        let mut submit_rx = Some(submit_rx);
        let (stream_tx, stream_rx) = unbounded();
        let mut stream_rx = Some(stream_rx);
        let (history_tx, history_rx) = oneshot::channel();
        let mut history_rx = Some(history_rx);
        let mut subscriptions = 0;
        let peer = Peer::start(move |r| match r["operation"].as_str().unwrap() {
            "submit" => Reply::Hold(submit_rx.take().unwrap()),
            "subscribe" => {
                subscriptions += 1;
                if subscriptions == 1 {
                    Reply::Never
                } else {
                    Reply::Stream(stream_rx.take().unwrap())
                }
            }
            "history" => history_rx.take().map_or(Reply::Never, Reply::Hold),
            _ => Reply::Never,
        });
        let mut h = Harness::new(true);
        let remote = h.remote.clone();
        let driver = async {
            remote.line("original prompt");
            let id = submitted(&peer.wait(|s| count(s, "submit") == 1).await);
            let before = remote.wait(|f| f.text.contains("original prompt")).await;
            assert!(
                before
                    .text
                    .lines()
                    .any(|l| l.contains("── USER ─") && l.ends_with(" #1 ──"))
            );
            assert!(!before.text.contains("── accepted-agent ─"));
            let mut live = view(id, false);
            live.selected_turn.as_mut().unwrap().agent_name = "accepted-agent".into();
            peer.wait(|s| count(s, "subscribe") == 1).await;
            submit_tx.send(response("acceptance", json!({"acceptance":acceptance,"turn":lattice_api::local::TurnView::from(live.selected_turn.as_ref().unwrap())}))).unwrap();
            let accepted = remote
                .wait(|f| {
                    f.text.contains(if acceptance == "accepted" {
                        "已确认接受"
                    } else {
                        "找回既有请求"
                    })
                })
                .await;
            assert_eq!(
                accepted.text.matches("── accepted-agent ─").count(),
                1,
                "{}",
                accepted.text
            );
            assert!(!accepted.text.contains("live body"));
            stream_tx.unbounded_send(snapshot(&live)).unwrap();
            peer.wait(|s| count(s, "history") == 1).await;
            let mut loaded = history(&live);
            loaded["value"]["fragments"][0]["agent_name"] = json!("accepted-agent");
            history_tx.send(loaded).unwrap();
            // A token after the loaded history also must not create a second header.
            stream_tx
                .unbounded_send(event(
                    1,
                    TurnEvent::TokenDelta {
                        key: key(id, 0, 1),
                        content: "live body".into(),
                    },
                ))
                .unwrap();
            let frame = remote.wait(|f| f.text.contains("live body")).await;
            assert_eq!(
                frame.text.matches("── accepted-agent ─").count(),
                1,
                "{}",
                frame.text
            );
            let mut done = view(id, true);
            done.selected_turn.as_mut().unwrap().agent_name = "accepted-agent".into();
            done.cursor.sequence = 1;
            stream_tx.unbounded_send(snapshot(&done)).unwrap();
            let mut loaded = finished(&done);
            for entry in loaded["value"]["history"]["entries"]
                .as_array_mut()
                .unwrap()
            {
                entry["agent_name"] = json!("accepted-agent");
            }
            stream_tx.unbounded_send(loaded).unwrap();
            let final_frame = remote.wait(|f| f.finished).await;
            assert_eq!(
                final_frame.text.matches("── accepted-agent ─").count(),
                1,
                "{}",
                final_frame.text
            );
            remote.line("/quit");
        };
        let (exit, ()) = tokio::join!(h.run(&peer.client, None), driver);
        exit.result.unwrap();
        assert_eq!(frame(&h.app).text.matches("── accepted-agent ─").count(), 1);
        assert!(
            !h.app
                .blocks
                .iter()
                .any(|block| matches!(block, Block::AssistantStart { .. })),
            "loaded Step owns the assistant segment"
        );
    }
}

#[tokio::test]
async fn existing_session_waits_for_first_history_but_builtin_commands_still_work() {
    let id = lattice_api::local::new_turn_id();
    let closed = view(id, true);
    let (history_tx, history_rx) = oneshot::channel();
    let mut history_rx = Some(history_rx);
    let response_history = history(&closed);
    let peer = Peer::start(move |r| match r["operation"].as_str().unwrap() {
        "subscribe" => Reply::Now(snapshot(&closed)),
        "history" => Reply::Hold(history_rx.take().unwrap()),
        _ => Reply::Never,
    });
    let mut h = Harness::new(false);
    let remote = h.remote.clone();
    let driver = async {
        peer.wait(|s| count(s, "history") == 1).await;
        remote.line("kept prompt");
        // /help is a barrier after Enter: no sleep-based absence assertion.
        remote.press(KeyCode::Esc, KeyModifiers::NONE);
        remote.line("/help");
        remote.wait(|f| f.text.contains("/reconnect")).await;
        assert_eq!(count(&peer.seen.borrow(), "submit"), 0);
        history_tx.send(response_history).unwrap();
        remote.wait(|f| f.text.contains("live body")).await;
        remote.line("after history");
        peer.wait(|s| count(s, "submit") == 1).await;
        remote.line("/quit");
    };
    let (exit, ()) = tokio::join!(
        h.run(
            &peer.client,
            Some(peer.client.observe(Some(sid()), Some(id)))
        ),
        driver
    );
    exit.result.unwrap();
}

#[tokio::test]
async fn first_submission_obtains_session_only_after_acceptance_and_quit_does_not_cancel() {
    let (tx, rx) = oneshot::channel();
    let mut rx = Some(rx);
    let peer = Peer::start(move |r| match r["operation"].as_str().unwrap() {
        "submit" => {
            assert!(r["request"]["session_id"].is_null());
            assert_eq!(r["request"]["agent"], "unused");
            Reply::Hold(rx.take().unwrap())
        }
        _ => Reply::Never,
    });
    let mut h = Harness::new(true);
    let remote = h.remote.clone();
    let driver = async {
        remote.line(" original ");
        let seen = peer.wait(|s| count(s, "submit") == 1).await;
        let id = submitted(&seen);
        assert_eq!(
            seen.iter().find(|r| r["operation"] == "submit").unwrap()["request"]["input"],
            " original "
        );
        tx.send(accepted(id)).unwrap();
        remote.wait(|f| f.text.contains("已确认接受")).await;
        remote.ctrl('d');
    };
    let ((), exit) = tokio::join!(driver, h.run(&peer.client, None));
    assert!(exit.result.is_ok());
    assert_eq!(h.app.ctx.session_id, Some(sid()));
    let report = tokio::time::timeout(Duration::from_secs(1), exit.turn.unwrap().detach())
        .await
        .unwrap();
    assert_eq!(report.cancel, None);
    assert!(cancels(&peer.seen.borrow()).is_empty());
}

#[tokio::test]
async fn cancel_receipt_orders_and_late_probe_keep_original_intent() {
    for order in [0, 1, 2] {
        let mut subscriptions = 0;
        let (stx, srx) = oneshot::channel();
        let mut srx = Some(srx);
        let (ctx, crx) = oneshot::channel();
        let mut crx = Some(crx);
        let peer = Peer::start(move |r| match r["operation"].as_str().unwrap() {
            "submit" => Reply::Hold(srx.take().unwrap()),
            "subscribe" => {
                subscriptions += 1;
                let id = serde_json::from_value(r["target"]["turn_id"].clone()).unwrap();
                let mut fixed = view(id, false);
                fixed.owned = order != 0 || subscriptions > 1;
                Reply::Now(snapshot(&fixed))
            }
            "history" => Reply::Never,
            "cancel" => match crx.take() {
                Some(rx) => Reply::Hold(rx),
                None => Reply::Now(response("cancelled", json!({"owned":true}))),
            },
            _ => panic!("no configuration reload"),
        });
        let mut h = Harness::new(false);
        h.app.replace_history(Vec::new()); // 已装入的空路径；本测试从提交窗口开始。
        let remote = h.remote.clone();
        let driver = async {
            remote.line("hi");
            let id = submitted(&peer.wait(|s| count(s, "submit") == 1).await);
            remote.ctrl('c');
            peer.wait(|s| count(s, "cancel") == 1).await;
            if order == 1 {
                stx.send(accepted(id)).unwrap();
                remote.wait(|f| f.text.contains("已确认接受")).await;
                ctx.send(response("cancelled", json!({"owned":false})))
                    .unwrap();
            } else {
                ctx.send(response("cancelled", json!({"owned":order==2})))
                    .unwrap();
                remote
                    .wait(|f| {
                        f.cancel
                            == Some(if order == 2 {
                                CancelStatus::Taken
                            } else {
                                CancelStatus::NotTaken
                            })
                    })
                    .await;
                stx.send(if order == 2 {
                    response(
                        "acceptance",
                        json!({"acceptance":"needs_configuration","agent_name":"unused"}),
                    )
                } else {
                    accepted(id)
                })
                .unwrap();
            }
            remote
                .wait(|f| {
                    f.cancel == Some(CancelStatus::Taken)
                        && (order != 2 || f.text.contains("已停止后续配置"))
                })
                .await;
            remote.ctrl('c');
            id
        };
        let (id, exit) = tokio::join!(driver, h.run(&peer.client, None));
        assert!(exit.result.is_ok());
        let report = tokio::time::timeout(Duration::from_secs(1), exit.turn.unwrap().detach())
            .await
            .unwrap();
        assert_eq!(report.cancel, Some(CancelStatus::Taken));
        assert_eq!(report.turn_id, Some(id));
        assert_eq!(count(&peer.seen.borrow(), "submit"), 1);
        assert_eq!(
            cancels(&peer.seen.borrow()),
            vec![json!(id); if order == 2 { 1 } else { 2 }]
        );
    }
}

#[tokio::test]
async fn exit_before_receipt_settles_cancel_and_configuration_abandonment() {
    for receipt in ["accepted", "rejected", "needs_configuration", "never"] {
        let (tx, rx) = oneshot::channel();
        let mut rx = Some(rx);
        let mut count_cancel = 0;
        let peer = Peer::start(move |r| match r["operation"].as_str().unwrap() {
            "submit" => Reply::Hold(rx.take().unwrap()),
            "cancel" => {
                count_cancel += 1;
                Reply::Now(response("cancelled", json!({"owned":count_cancel>1})))
            }
            _ => Reply::Never,
        });
        let mut h = Harness::new(false);
        h.app.replace_history(Vec::new()); // 已装入的空路径；本测试从提交窗口开始。
        let remote = h.remote.clone();
        let driver = async {
            remote.line("hi");
            let id = submitted(&peer.wait(|s| count(s, "submit") == 1).await);
            remote.ctrl('c');
            peer.wait(|s| count(s, "cancel") == 1).await;
            remote.ctrl('c');
            id
        };
        let (id, exit) = tokio::join!(driver, h.run(&peer.client, None));
        assert!(exit.result.is_ok());
        if receipt != "never" {
            tx.send(match receipt {
                "accepted" => accepted(id),
                "rejected" => error("rejected"),
                _ => response(
                    "acceptance",
                    json!({"acceptance":"needs_configuration","agent_name":"unused"}),
                ),
            })
            .unwrap();
        }
        let started = Instant::now();
        let report = tokio::time::timeout(Duration::from_secs(8), exit.turn.unwrap().detach())
            .await
            .unwrap();
        assert_eq!(report.turn_id, Some(id));
        if receipt == "never" {
            assert!(started.elapsed() >= Duration::from_secs(4));
            assert!(matches!(
                report.cancel,
                Some(CancelStatus::NotTaken | CancelStatus::Unknown)
            ));
        }
        if receipt == "accepted" {
            assert_eq!(report.cancel, Some(CancelStatus::Taken));
        }
        if receipt == "needs_configuration" {
            assert!(report.not_submitted.is_some());
        }
        assert_eq!(count(&peer.seen.borrow(), "submit"), 1);
        assert_eq!(
            cancels(&peer.seen.borrow()),
            vec![json!(id); if receipt == "accepted" { 2 } else { 1 }]
        );
    }
}

#[tokio::test]
async fn late_transport_error_keeps_observation_after_submission_grace() {
    let (close, rx) = oneshot::channel();
    let mut rx = Some(rx);
    let (events, incoming) = unbounded();
    let mut incoming = Some(incoming);
    let mut subscriptions = 0;
    let peer = Peer::start(move |r| match r["operation"].as_str().unwrap() {
        "submit" => Reply::Close(rx.take().unwrap()),
        "subscribe" => {
            subscriptions += 1;
            if subscriptions == 1 {
                let id = serde_json::from_value(r["target"]["turn_id"].clone()).unwrap();
                Reply::Now(snapshot(&view(id, false)))
            } else {
                Reply::Stream(incoming.take().unwrap())
            }
        }
        "history" => Reply::Never,
        "cancel" => Reply::Now(response("cancelled", json!({"owned":true}))),
        _ => panic!("unexpected request"),
    });
    let mut h = Harness::new(false);
    h.app.replace_history(Vec::new()); // 已装入的空路径；本测试从提交窗口开始。
    let remote = h.remote.clone();
    let driver = async {
        remote.line("hi");
        let id = submitted(
            &peer
                .wait(|s| count(s, "submit") == 1 && count(s, "subscribe") == 1)
                .await,
        );
        remote.ctrl('c');
        tokio::time::sleep(Duration::from_secs(4)).await;
        remote.wait(|f| f.text.contains("提交结果未知")).await;
        peer.wait(|s| count(s, "subscribe") == 2).await;
        events.unbounded_send(snapshot(&view(id, false))).unwrap();
        events
            .unbounded_send(event(
                1,
                TurnEvent::TokenDelta {
                    key: key(id, 0, 1),
                    content: "before receipt failure".into(),
                },
            ))
            .unwrap();
        remote
            .wait(|f| f.text.contains("before receipt failure"))
            .await;
        close.send(()).unwrap();
        let frame = remote
            .wait(|f| f.text.contains("local connection failed"))
            .await;
        assert_eq!(frame.current, Some(id));
        assert!(frame.input.is_empty());
        assert!(frame.connected);
        events
            .unbounded_send(event(
                2,
                TurnEvent::TokenDelta {
                    key: key(id, 0, 1),
                    content: " after receipt failure".into(),
                },
            ))
            .unwrap();
        remote
            .wait(|f| f.text.contains("after receipt failure"))
            .await;
        events.unbounded_send(snapshot(&view(id, true))).unwrap();
        events.unbounded_send(finished(&view(id, true))).unwrap();
        remote
            .wait(|f| f.phase == TurnPhase::Idle && f.text.contains("completed"))
            .await;
        remote.ctrl('d');
        id
    };
    let (id, exit) = tokio::join!(driver, h.run(&peer.client, None));
    assert!(exit.result.is_ok());
    let report = exit.turn.unwrap().detach().await;
    assert_eq!(report.turn_id, Some(id));
    assert!(report.submission_diagnostic.is_some());
    assert_eq!(report.cancel, Some(CancelStatus::Taken));
}

#[tokio::test]
async fn idle_deadline_survives_repeated_poll_rebuilds() {
    let id = lattice_api::local::new_turn_id();
    let peer = Peer::start(move |r| match r["operation"].as_str().unwrap() {
        "subscribe" => Reply::Now(snapshot(&view(id, false))),
        "history" => Reply::Now(history(&view(id, false))),
        _ => panic!("read only"),
    });
    let mut h = Harness::new(false);
    let remote = h.remote.clone();
    let initial = peer.client.observe(Some(sid()), Some(id));
    let driver = async {
        remote.wait(|f| f.phase == TurnPhase::Streaming).await;
        peer.wait(|s| count(s, "history") == 1).await;
        tokio::time::pause();
        for _ in 0..lattice_api::STREAM_IDLE_TIMEOUT_SECS - 1 {
            tokio::time::advance(Duration::from_secs(1)).await;
            for _ in 0..8 {
                tokio::task::yield_now().await;
            }
            assert!(remote.frames.borrow().connected);
        }
        tokio::time::advance(Duration::from_secs(1)).await;
        let frame = remote.wait(|f| !f.connected).await;
        assert_eq!(frame.current, Some(id));
        assert_eq!(frame.phase, TurnPhase::Streaming);
        assert!(frame.text.contains("无增量"));
        tokio::time::resume();
        remote.ctrl('d');
    };
    let ((), exit) = tokio::join!(driver, h.run(&peer.client, Some(initial)));
    assert!(exit.result.is_ok());
    assert_eq!(exit.turn.unwrap().detach().await.cancel, None);
}

#[tokio::test]
async fn switching_targets_keeps_old_receipts_and_diagnostics_out_of_new_state() {
    // Esc 只清空输入，取消与选择活动目标由首次 Ctrl+C 或 /reconnect 触发。
    for gesture in [1, 2] {
        let old = lattice_api::local::new_turn_id();
        let active = lattice_api::local::new_turn_id();
        let (old_tx, old_rx) = unbounded();
        let mut old_rx = Some(old_rx);
        let (cancel_tx, cancel_rx) = oneshot::channel();
        let mut cancel_rx = Some(cancel_rx);
        let peer = Peer::start(move |r| match r["operation"].as_str().unwrap() {
            "query" => Reply::Now(response("view", json!(view(old, false)))),
            "subscribe" if r["target"]["turn_id"] == json!(old) => {
                Reply::Stream(old_rx.take().unwrap())
            }
            "subscribe" => {
                assert_eq!(r["target"]["turn_id"], json!(active));
                Reply::Now(snapshot(&view(active, false)))
            }
            "cancel" if r["target"]["turn_id"] == json!(old) => {
                Reply::Hold(cancel_rx.take().unwrap())
            }
            "cancel" => {
                assert_eq!(r["target"]["turn_id"], json!(active));
                Reply::Now(response("cancelled", json!({"owned":true})))
            }
            "history" => Reply::Never,
            _ => panic!("read only target selection"),
        });
        let mut h = Harness::new(false);
        let remote = h.remote.clone();
        let driver = async {
            remote.line("/reconnect");
            peer.wait(|s| count(s, "subscribe") == 1).await;
            old_tx.unbounded_send(snapshot(&view(old, false))).unwrap();
            remote.wait(|f| f.current == Some(old)).await;
            remote.ctrl('c');
            peer.wait(|s| count(s, "cancel") == 1).await;
            let mut closed = view(old, true);
            closed.session.active_turn = Some(active);
            old_tx.unbounded_send(snapshot(&closed)).unwrap();
            remote.wait(|f| f.current == Some(active)).await;
            match gesture {
                1 => {
                    // 第二次是对 B 的新取消，不是两秒窗口内的退出。
                    tokio::time::sleep(CTRL_C_WINDOW + Duration::from_millis(50)).await;
                    remote.ctrl('c');
                }
                _ => remote.line("/reconnect"),
            };
            remote
                .wait(|f| {
                    f.current == Some(active)
                        && if gesture == 2 {
                            f.phase == TurnPhase::Streaming
                        } else {
                            f.cancel == Some(CancelStatus::Taken)
                        }
                })
                .await;
            cancel_tx
                .send(if gesture == 2 {
                    response("cancelled", json!({"owned":true}))
                } else {
                    error("A cancellation denied")
                })
                .unwrap();
            let f = remote
                .wait(|f| {
                    f.text.contains(if gesture == 2 {
                        "历史交接：后台已接管取消"
                    } else {
                        "A cancellation denied"
                    })
                })
                .await;
            assert_eq!(f.current, Some(active));
            assert_eq!(
                f.cancel,
                if gesture == 2 {
                    None
                } else {
                    Some(CancelStatus::Taken)
                }
            );
            assert!(
                f.text
                    .lines()
                    .any(|line| line.contains("历史交接：Turn") && line.contains(&old.to_string()))
            );
            remote.ctrl('d');
        };
        let ((), exit) = tokio::join!(driver, h.run(&peer.client, None));
        assert!(exit.result.is_ok());
        assert!(exit.handoffs.is_empty());
        let report = exit.turn.unwrap().detach().await;
        assert_eq!(report.turn_id, Some(active));
        assert!(report.cancel_error.is_none());
        assert_eq!(
            cancels(&peer.seen.borrow()),
            if gesture == 2 {
                vec![json!(old)]
            } else {
                vec![json!(old), json!(active)]
            }
        );
        assert_eq!(count(&peer.seen.borrow(), "submit"), 0);
    }
}

#[tokio::test]
async fn detached_cancel_keeps_original_target_diagnostic() {
    let id = lattice_api::local::new_turn_id();
    let (tx, rx) = oneshot::channel();
    let mut rx = Some(rx);
    let peer = Peer::start(move |r| match r["operation"].as_str().unwrap() {
        "subscribe" => Reply::Now(snapshot(&view(id, false))),
        "history" => Reply::Now(history(&view(id, false))),
        "cancel" => match rx.take() {
            Some(rx) => Reply::Hold(rx),
            None => Reply::Now(error("original cancellation denied")),
        },
        _ => panic!("read only"),
    });
    let mut h = Harness::new(false);
    let remote = h.remote.clone();
    let initial = peer.client.observe(Some(sid()), Some(id));
    let driver = async {
        remote.wait(|f| f.current == Some(id)).await;
        remote.ctrl('c');
        peer.wait(|s| count(s, "cancel") == 1).await;
        remote.ctrl('d');
    };
    let ((), exit) = tokio::join!(driver, h.run(&peer.client, Some(initial)));
    assert!(exit.result.is_ok());
    tx.send(error("original cancellation denied")).unwrap();
    let report = exit.turn.unwrap().detach().await;
    assert_eq!(report.turn_id, Some(id));
    assert_eq!(report.cancel, Some(CancelStatus::Unknown));
    assert_eq!(cancels(&peer.seen.borrow()), vec![json!(id); 2]);
    assert!(detach_lines(report).iter().any(
        |line| line.contains("original cancellation denied") && line.contains(&id.to_string())
    ));
}

#[tokio::test]
async fn busy_submission_preserves_draft_and_can_cancel_displayed_active_turn() {
    let active = lattice_api::local::new_turn_id();
    let mut subs = 0;
    let peer = Peer::start(move |r| match r["operation"].as_str().unwrap() {
        "submit" => Reply::Now(response(
            "error",
            json!({"kind":"busy","message":"session is busy","recovery":null,"store":null}),
        )),
        "subscribe" => {
            subs += 1;
            if subs == 1 {
                Reply::Never
            } else {
                let mut v = view(active, false);
                v.owned = r["target"]["turn_id"] == json!(active);
                if !v.owned {
                    v.selected_turn = None;
                }
                Reply::Now(snapshot(&v))
            }
        }
        "history" => Reply::Never,
        "cancel" => {
            assert_eq!(r["target"]["turn_id"], json!(active));
            Reply::Now(response("cancelled", json!({"owned":true})))
        }
        _ => panic!("no config loading"),
    });
    let mut h = Harness::new(false);
    h.app.replace_history(Vec::new()); // 已装入的空路径；本测试从提交窗口开始。
    let remote = h.remote.clone();
    let driver = async {
        remote.line("hi");
        let id = submitted(
            &peer
                .wait(|s| count(s, "submit") == 1 && count(s, "subscribe") == 1)
                .await,
        );
        remote
            .wait(|f| f.input == "hi" && f.text.contains("session is busy"))
            .await;
        remote.press(KeyCode::Esc, KeyModifiers::NONE);
        remote.line("/reconnect");
        remote.wait(|f| f.current == Some(active)).await;
        remote.ctrl('c');
        let f = remote
            .wait(|f| f.cancel == Some(CancelStatus::Taken) && f.text.contains("历史交接：请求"))
            .await;
        assert!(f.text.contains(&id.to_string()));
        remote.ctrl('d');
    };
    let ((), exit) = tokio::join!(driver, h.run(&peer.client, None));
    assert!(exit.result.is_ok());
    assert_eq!(exit.turn.unwrap().detach().await.turn_id, Some(active));
    assert_eq!(count(&peer.seen.borrow(), "submit"), 1);
    assert_eq!(cancels(&peer.seen.borrow()), vec![json!(active)]);
}

#[tokio::test]
async fn existing_submission_keeps_explicit_agent() {
    let peer = Peer::start(|r| match r["operation"].as_str().unwrap() {
        "submit" => Reply::Now(response(
            "error",
            json!({"kind":"busy","message":"busy","recovery":null,"store":null}),
        )),
        _ => Reply::Never,
    });
    let mut h = Harness::new(false);
    h.app.replace_history(Vec::new()); // 已装入的空路径；本测试从提交窗口开始。
    h.options.agent = "override-agent".into();
    let remote = h.remote.clone();
    let driver = async {
        remote.line("hi");
        let seen = peer.wait(|s| count(s, "submit") == 1).await;
        assert_eq!(
            seen.iter().find(|r| r["operation"] == "submit").unwrap()["request"]["agent"],
            "override-agent"
        );
        remote.wait(|f| f.input == "hi").await;
        remote.press(KeyCode::Esc, KeyModifiers::NONE);
        remote.line("/quit");
    };
    let ((), exit) = tokio::join!(driver, h.run(&peer.client, None));
    assert!(exit.result.is_ok());
}

// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0
use super::*;
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
const SESSION: &str = "11111111111141118111111111111111";
fn sid() -> SessionId {
    SESSION.parse().unwrap()
}
struct Peer {
    _root: tempfile::TempDir,
    listener: UnixListener,
    client: LocalClient,
}
struct Wire {
    stream: UnixStream,
    sequence: u64,
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
    async fn request_any(&self) -> (Wire, Value) {
        let (mut stream, _) = self.listener.accept().await.unwrap();
        let mut magic = [0; 8];
        stream.read_exact(&mut magic).await.unwrap();
        assert_eq!(&magic, b"LTLOCAL3");
        let len = stream.read_u32().await.unwrap();
        assert!(len <= 16 * 1024 * 1024);
        let mut bytes = vec![0; len as usize];
        stream.read_exact(&mut bytes).await.unwrap();
        (
            Wire {
                stream,
                sequence: 0,
            },
            serde_json::from_slice(&bytes).unwrap(),
        )
    }
    async fn request(&self, op: &str) -> (Wire, Value) {
        let (stream, value) = self.request_any().await;
        assert_eq!(value["operation"], op);
        (stream, value)
    }
}
impl Wire {
    async fn reply(&mut self, response: &str, value: Value) {
        let fragment = serde_json::to_string(&json!({"response":response,"value":value})).unwrap();
        let bytes=serde_json::to_vec(&json!({"message_id":self.sequence,"offset":0,"json_fragment":fragment,"complete":true})).unwrap();
        self.sequence += 1;
        self.stream.write_all(b"LTLOCAL3").await.unwrap();
        self.stream.write_u32(bytes.len() as u32).await.unwrap();
        self.stream.write_all(&bytes).await.unwrap();
    }
    async fn closed(&mut self) {
        assert_eq!(self.stream.read(&mut [0; 1]).await.unwrap(), 0);
    }
    async fn snapshot(&mut self, view: &ObservationSnapshot) {
        self.reply("observation", json!({"type":"snapshot","view":view}))
            .await;
    }
    async fn finished(&mut self, view: &ObservationSnapshot) {
        self.reply("observation",json!({"type":"finished","turn":{"session_id":sid(),"turn_id":view.selected_turn.as_ref().unwrap().turn_id},"cursor":view.cursor,"history":history(view)})).await;
    }
}
fn view(id: TurnId, outcome: Option<&str>) -> ObservationSnapshot {
    let summary=outcome.map(|outcome|json!({"outcome":outcome,"notice":match outcome {"completed"=>Value::Null,"cancelled"=>json!({"level":"info","text":"cancelled by user"}),_=>json!({"level":"error","text":"provider failed"})}}));
    let steps = match outcome {
        Some("completed") => {
            json!([{"key":{"turn_id":id,"ordinal":0},"attempts":[],"state":{"state":"closed","end":{"end":"finish","node":2},"slots":[]}}])
        }
        Some(outcome) => {
            json!([{"key":{"turn_id":id,"ordinal":0},"attempts":[],"state":{"state":"closed","end":{"end":"stop","node":null,"cause":{"cause":if outcome=="cancelled" {"cancelled"} else {"process_exit"}}},"slots":[]}}])
        }
        None => json!([]),
    };
    serde_json::from_value(json!({
        "session":{"session_id":sid(),"project_store_key":"a".repeat(64),"current":if outcome==Some("completed") {2} else {1},"active_turn":if outcome.is_none(){json!(id)}else{Value::Null},"created_at":"2026-09-27T00:00:00Z","updated_at":"2026-09-27T00:00:00Z"},
        "current_agent":"unused","selected_turn":{"session_id":sid(),"turn_id":id,"user_node":1,"agent_name":"unused","accepted_at":"2026-09-27T00:00:00Z","ended_at":outcome.map(|_|"2026-09-27T00:00:01Z"),"summary":summary,"usage":{"kind":"complete","total":{"prompt_tokens":1,"completion_tokens":2,"total_tokens":3}},"steps":steps},
        "active":null,"history":{"session_id":sid(),"head":if outcome==Some("completed"){2}else{1}},"attempt":null,"cursor":{"epoch":"peer","sequence":if outcome.is_some(){2}else{1}},"owned":outcome.is_none()
    })).unwrap()
}
fn history(view: &ObservationSnapshot) -> LocalHistory {
    let id = view.selected_turn.as_ref().unwrap().turn_id;
    let mut entries = vec![lattice_api::local::HistoryEntry {
        node: lattice_api::local::NodeId(1),
        agent_name: "unused".into(),
        content: Node::UserMessage {
            parent: lattice_api::local::NodeId(0),
            turn_id: id,
            message: lattice_session::test_helpers::user_msg("prompt"),
        },
    }];
    if view.history.head.0 == 2 {
        entries.push(lattice_api::local::HistoryEntry {
            node: lattice_api::local::NodeId(2),
            agent_name: "unused".into(),
            content: Node::Step {
                parent: lattice_api::local::NodeId(1),
                turn_id: id,
                step_ordinal: 0,
                content: lattice_api::local::StepContent {
                    assistant: lattice_session::test_helpers::assistant_msg("answer"),
                    results: vec![],
                },
            },
        });
    }
    LocalHistory {
        anchor: view.history.clone(),
        entries,
    }
}
async fn body_reply(wire: &mut Wire, view: &ObservationSnapshot) {
    let fragments:Vec<_>=history(view).entries.into_iter().map(|entry|json!({"fragment":{"node":entry.node,"utf8_offset":0,"json_fragment":serde_json::to_string(&entry.content).unwrap(),"complete":true},"agent_name":entry.agent_name})).collect();
    wire.reply(
        "history",
        json!({"anchor":view.history,"fragments":fragments,"next":null}),
    )
    .await;
}
async fn final_delivery(peer: &Peer, subscription: &mut Wire, id: TurnId, outcome: &str) {
    let view = view(id, Some(outcome));
    subscription.snapshot(&view).await;
    // 真实 LocalTurn 并行正文读取；最终响应的正文取代这次读取。
    let (mut body, request) = peer.request("history").await;
    assert_eq!(request["anchor"], json!(view.history));
    subscription.finished(&view).await;
    body.closed().await;
}
pub(super) async fn signal_guard() -> tokio::sync::MutexGuard<'static, ()> {
    static SIGNALS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    SIGNALS.lock().await
}
fn interrupt() {
    assert!(
        std::process::Command::new("kill")
            .args(["-INT", &std::process::id().to_string()])
            .status()
            .unwrap()
            .success()
    );
}
async fn run_observe(
    driver: impl std::future::Future<Output = ()>,
    observe: impl std::future::Future<Output = Result<std::process::ExitCode>>,
    expected: u8,
) {
    time::timeout(Duration::from_secs(6), async {
        let (result, ()) = tokio::join!(observe, driver);
        assert_eq!(result.unwrap(), std::process::ExitCode::from(expected));
    })
    .await
    .expect("bounded real CLI loop");
}
fn send(
    peer: &Peer,
    id: TurnId,
    new_session: bool,
) -> impl std::future::Future<Output = Result<std::process::ExitCode>> + '_ {
    run_session_prompt(
        &peer.client,
        super::super::session::SendArgs {
            prompt: "prompt".into(),
            session_id: (!new_session).then(|| SESSION.into()),
            agent: new_session.then(|| "unused".into()),
            turn_id: Some(id),
        },
        false,
        true,
    )
}
async fn begin_submit(peer: &Peer, id: TurnId, new_session: bool) -> (Wire, Wire) {
    if !new_session {
        let (mut resolve, _) = peer.request("resolve_session").await;
        resolve
            .reply("session", json!(view(id, None).session))
            .await;
    }
    let mut submit = None;
    let mut subscription = None;
    for _ in 0..2 {
        let (wire, request) = peer.request_any().await;
        match request["operation"].as_str().unwrap() {
            "submit" => {
                assert_eq!(request["request"]["turn_id"], json!(id));
                assert_eq!(
                    request["request"]["session_id"],
                    if new_session {
                        Value::Null
                    } else {
                        json!(sid())
                    }
                );
                assert_eq!(request["request"]["input"], "prompt");
                assert!(request["configuration"].is_null());
                submit = Some(wire);
            }
            "subscribe" => {
                assert_eq!(request["target"]["turn_id"], json!(id));
                assert!(request["cursor"].is_null());
                subscription = Some(wire);
            }
            _ => panic!("unexpected {request}"),
        }
    }
    (submit.unwrap(), subscription.unwrap())
}
async fn accepted(wire: &mut Wire, id: TurnId) {
    let detail = view(id, None).selected_turn.unwrap();
    wire.reply(
        "acceptance",
        json!({"acceptance":"existing","turn":lattice_api::local::TurnView::from(&detail)}),
    )
    .await;
}
async fn cancel(peer: &Peer, id: TurnId, owned: bool) {
    let (mut wire, request) = peer.request("cancel").await;
    assert_eq!(request["target"]["turn_id"], json!(id));
    wire.reply("cancelled", json!({"owned":owned})).await;
}

#[tokio::test]
async fn pending_body_error_or_stall_does_not_discard_finished() {
    let _guard = signal_guard().await;
    for error in [false, true] {
        let peer = Peer::new();
        let id = lattice_api::local::new_turn_id();
        run_observe(async{
            let(mut sub,_)=peer.request("subscribe").await;sub.snapshot(&view(id,None)).await;
            let(mut body,_)=peer.request("history").await;
            if error {body.reply("error",json!({"kind":"store_unavailable","message":"body unavailable","recovery":null,"store":null})).await;body.closed().await;}
            final_delivery(&peer,&mut sub,id,"completed").await;
            if !error {body.closed().await;}
        },observe_turn(&peer.client,sid(),id,false,true),0).await;
    }
}
#[tokio::test]
async fn closed_snapshot_waits_for_body_but_not_submission_receipt() {
    let _guard = signal_guard().await;
    for new_session in [false, true] {
        let peer = Peer::new();
        let id = lattice_api::local::new_turn_id();
        run_observe(
            async {
                let (mut submit, mut sub) = begin_submit(&peer, id, new_session).await;
                let view = view(id, Some("completed"));
                sub.snapshot(&view).await;
                let (mut body, _) = peer.request("history").await;
                body_reply(&mut body, &view).await;
                body.closed().await;
                assert!(
                    time::timeout(Duration::from_millis(50), sub.stream.read(&mut [0]))
                        .await
                        .is_err()
                );
                sub.finished(&view).await;
                submit.closed().await;
            },
            send(&peer, id, new_session),
            0,
        )
        .await;
    }
}
#[tokio::test]
async fn sigint_false_then_acceptance_retries_same_target_once() {
    let _guard = signal_guard().await;
    let peer = Peer::new();
    let id = lattice_api::local::new_turn_id();
    run_observe(
        async {
            let (mut submit, mut initial) = begin_submit(&peer, id, true).await;
            interrupt();
            cancel(&peer, id, false).await;
            accepted(&mut submit, id).await;
            initial.closed().await;
            let mut sub = None;
            let mut cancelled = false;
            for _ in 0..2 {
                let (mut wire, request) = peer.request_any().await;
                assert_eq!(request["target"]["turn_id"], json!(id));
                match request["operation"].as_str().unwrap() {
                    "cancel" => {
                        wire.reply("cancelled", json!({"owned":true})).await;
                        cancelled = true;
                    }
                    "subscribe" => sub = Some(wire),
                    _ => panic!("{request}"),
                }
            }
            assert!(cancelled);
            final_delivery(&peer, &mut sub.unwrap(), id, "completed").await;
        },
        send(&peer, id, true),
        0,
    )
    .await;
}
#[tokio::test]
async fn sigint_not_taken_and_owned_both_orders_share_one_retry() {
    let _guard = signal_guard().await;
    for owned_first in [false, true] {
        let peer = Peer::new();
        let id = lattice_api::local::new_turn_id();
        run_observe(
            async {
                let (mut sub, _) = peer.request("subscribe").await;
                if owned_first {
                    sub.snapshot(&view(id, None)).await;
                    let (mut body, _) = peer.request("history").await;
                    body_reply(&mut body, &view(id, None)).await;
                    body.closed().await;
                }
                interrupt();
                let (mut first, request) = peer.request("cancel").await;
                assert_eq!(request["target"]["turn_id"], json!(id));
                first.reply("cancelled", json!({"owned":false})).await;
                if !owned_first {
                    sub.snapshot(&view(id, None)).await;
                }
                let mut got_retry = false;
                let mut got_body = owned_first;
                while !got_retry || !got_body {
                    let (mut wire, request) = peer.request_any().await;
                    match request["operation"].as_str().unwrap() {
                        "cancel" => {
                            assert!(!got_retry);
                            assert_eq!(request["target"]["turn_id"], json!(id));
                            wire.reply("cancelled", json!({"owned":true})).await;
                            got_retry = true;
                        }
                        "history" => {
                            body_reply(&mut wire, &view(id, None)).await;
                            got_body = true;
                        }
                        _ => panic!("{request}"),
                    }
                }
                final_delivery(&peer, &mut sub, id, "cancelled").await;
            },
            observe_turn(&peer.client, sid(), id, false, true),
            2,
        )
        .await;
        assert!(
            time::timeout(Duration::from_millis(50), peer.listener.accept())
                .await
                .is_err()
        );
    }
}
#[tokio::test]
async fn sigint_before_configuration_stops_local_submission() {
    let _guard = signal_guard().await;
    let peer = Peer::new();
    let id = lattice_api::local::new_turn_id();
    run_observe(
        async {
            let (mut submit, _initial) = begin_submit(&peer, id, true).await;
            interrupt();
            cancel(&peer, id, false).await;
            submit
                .reply(
                    "acceptance",
                    json!({"acceptance":"needs_configuration","agent_name":"unused"}),
                )
                .await;
        },
        send(&peer, id, true),
        1,
    )
    .await;
    assert!(
        time::timeout(Duration::from_millis(50), peer.listener.accept())
            .await
            .is_err()
    );
}
#[tokio::test]
async fn second_sigint_unknown_submission_has_bounded_detach() {
    let _guard = signal_guard().await;
    let peer = Peer::new();
    let id = lattice_api::local::new_turn_id();
    run_observe(
        async {
            let (mut submit, _sub) = begin_submit(&peer, id, true).await;
            interrupt();
            cancel(&peer, id, false).await;
            interrupt();
            submit.closed().await;
        },
        send(&peer, id, true),
        1,
    )
    .await;
}
#[tokio::test]
async fn finished_body_interrupt_keeps_result_and_does_not_cancel() {
    let _guard = signal_guard().await;
    let peer = Peer::new();
    let id = lattice_api::local::new_turn_id();
    run_observe(
        async {
            let (mut sub, _) = peer.request("subscribe").await;
            sub.snapshot(&view(id, Some("completed"))).await;
            let (mut body, _) = peer.request("history").await;
            interrupt();
            body.closed().await;
        },
        observe_turn(&peer.client, sid(), id, false, true),
        1,
    )
    .await;
    assert!(
        time::timeout(Duration::from_millis(50), peer.listener.accept())
            .await
            .is_err()
    );
}
#[tokio::test]
async fn late_needs_configuration_keeps_taken_and_stops_only_local_submission() {
    let _guard = signal_guard().await;
    let peer = Peer::new();
    let id = lattice_api::local::new_turn_id();
    run_observe(
        async {
            let (mut submit, mut sub) = begin_submit(&peer, id, true).await;
            sub.snapshot(&view(id, None)).await;
            let (mut body, _) = peer.request("history").await;
            body_reply(&mut body, &view(id, None)).await;
            body.closed().await;
            interrupt();
            cancel(&peer, id, true).await;
            submit
                .reply(
                    "acceptance",
                    json!({"acceptance":"needs_configuration","agent_name":"unused"}),
                )
                .await;
        },
        send(&peer, id, true),
        1,
    )
    .await;
    assert!(
        time::timeout(Duration::from_millis(50), peer.listener.accept())
            .await
            .is_err()
    );
}
#[tokio::test]
async fn unknown_submission_survives_late_transport_error_before_or_after_summary() {
    let _guard = signal_guard().await;
    for closed_first in [false, true] {
        let peer = Peer::new();
        let id = lattice_api::local::new_turn_id();
        run_observe(
            async {
                let (mut submit, mut sub) = begin_submit(&peer, id, true).await;
                sub.snapshot(&view(id, None)).await;
                let (mut body, _) = peer.request("history").await;
                body_reply(&mut body, &view(id, None)).await;
                body.closed().await;
                interrupt();
                cancel(&peer, id, true).await;
                let (mut reconnected, request) = peer.request("subscribe").await;
                assert_eq!(request["target"]["turn_id"], json!(id));
                let view = view(id, closed_first.then_some("completed"));
                reconnected.snapshot(&view).await;
                let (mut body, _) = peer.request("history").await;
                submit.stream.shutdown().await.unwrap();
                submit.closed().await;
                assert!(
                    time::timeout(Duration::from_millis(50), body.stream.read(&mut [0]))
                        .await
                        .is_err()
                );
                if closed_first {
                    reconnected.finished(&view).await;
                    body.closed().await;
                } else {
                    body_reply(&mut body, &view).await;
                    body.closed().await;
                    final_delivery(&peer, &mut reconnected, id, "completed").await;
                }
            },
            send(&peer, id, true),
            0,
        )
        .await;
    }
}
#[tokio::test]
async fn first_sigint_unknown_submission_no_owner_is_bounded() {
    let _guard = signal_guard().await;
    let peer = Peer::new();
    let id = lattice_api::local::new_turn_id();
    run_observe(
        async {
            let (mut submit, mut sub) = begin_submit(&peer, id, true).await;
            let mut view = view(id, None);
            view.owned = false;
            sub.snapshot(&view).await;
            let (mut body, _) = peer.request("history").await;
            body_reply(&mut body, &view).await;
            body.closed().await;
            interrupt();
            cancel(&peer, id, false).await;
            let (mut sub, _) = peer.request("subscribe").await;
            sub.snapshot(&view).await;
            let (mut body, _) = peer.request("history").await;
            body_reply(&mut body, &view).await;
            submit.closed().await;
        },
        send(&peer, id, true),
        1,
    )
    .await;
}
#[tokio::test]
async fn cancel_error_received_during_detach_is_rendered_with_original_identity() {
    const MODE: &str = "LATTICE_TEST_DETACH_DIAGNOSTIC";
    const ID: &str = "22222222-2222-4222-8222-222222222222";
    if let Ok(mode) = std::env::var(MODE) {
        let _guard = signal_guard().await;
        let peer = Peer::new();
        let id = ID.parse().unwrap();
        run_observe(async{
            let(mut sub,_)=peer.request("subscribe").await;sub.snapshot(&view(id,None)).await;
            let(mut body,_)=peer.request("history").await;interrupt();let(mut cancel,request)=peer.request("cancel").await;assert_eq!(request["target"]["turn_id"],ID);
            interrupt();body.closed().await;
            let error=json!({"kind":"permission","message":"cancel denied during detach","recovery":null,"store":null});
            cancel.reply("error",error.clone()).await;let(mut retry,request)=peer.request("cancel").await;assert_eq!(request["target"]["turn_id"],ID);retry.reply("error",error).await;retry.closed().await;
        },observe_turn(&peer.client,sid(),id,false,mode=="json"),1).await;
        return;
    }
    for mode in ["json", "human"] {
        let root = tempfile::tempdir().unwrap();
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command.env_clear().env("PATH","/usr/bin:/bin").env("HOME",root.path()).env("XDG_STATE_HOME",root.path().join("state")).env(MODE,mode).args(["--exact","commands::run::ipc_tests::cancel_error_received_during_detach_is_rendered_with_original_identity","--nocapture"]);
        let output = tokio::task::spawn_blocking(move || command.output().unwrap())
            .await
            .unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(output.status.success(), "{stdout}\n{stderr}");
        if mode == "json" {
            let records: Vec<Value> = stdout
                .lines()
                .filter_map(|s| serde_json::from_str(s).ok())
                .collect();
            assert_eq!(
                records
                    .iter()
                    .filter(
                        |r| r["type"] == "error" && r["message"] == "cancel denied during detach"
                    )
                    .count(),
                1
            );
            assert!(records.iter().any(|r| {
                r["type"] == "connection"
                    && r["message"]
                        .as_str()
                        .is_some_and(|s| s.contains(SESSION) && s.contains(ID))
            }));
        } else {
            assert_eq!(stderr.matches("cancel denied during detach").count(), 1);
            assert!(
                stderr
                    .lines()
                    .any(|s| s.contains("取消诊断") && s.contains(SESSION) && s.contains(ID))
            );
        }
    }
}

#[test]
fn human_summary_is_printed_once_across_event_snapshot_and_detach() {
    for outcome in ["completed", "failed", "cancelled"] {
        for sources in [
            ["event", "snapshot", "detach"],
            ["snapshot", "event", "detach"],
            ["detach", "snapshot", "event"],
            ["snapshot", "snapshot", "detach"],
            ["detach", "detach", "detach"],
        ] {
            let id = lattice_api::local::new_turn_id();
            let snapshot = view(id, Some(outcome));
            let detail = snapshot.selected_turn.as_ref().unwrap();
            let summary = detail.summary.clone().unwrap();
            let turn = lattice_api::local::TurnRef {
                session_id: sid(),
                turn_id: id,
            };
            let step = detail.steps[0].clone();
            let event = match outcome {
                "completed" => TurnEvent::TurnCompleted {
                    turn,
                    step,
                    summary,
                },
                "failed" => TurnEvent::TurnFailed {
                    turn,
                    step,
                    summary,
                },
                _ => TurnEvent::TurnCancelled {
                    turn,
                    step,
                    summary,
                },
            };
            let mut render = RunRenderState::default();
            let mut out = Vec::new();
            let mut err = Vec::new();
            // State 可先记录结局；这不表示人类摘要已经打印。
            render.last_summary = detail.summary.clone();
            for source in sources {
                if source == "event" {
                    render_human_event(&event, &mut render, &mut out, &mut err, false).unwrap();
                    apply_event_to_state(&event, &mut render);
                } else {
                    // State 与 detach 共用快照渲染；NoOwner/已关闭重连可完全没有事件。
                    render_snapshot(&snapshot, &mut render, false, &mut err).unwrap();
                }
            }
            let expected = summary_text(detail.summary.as_ref().unwrap());
            assert_eq!(
                String::from_utf8(err.clone())
                    .unwrap()
                    .lines()
                    .filter(|line| *line == expected)
                    .count(),
                1,
                "{outcome}: {sources:?}"
            );
            // 同一命令若观察到另一个 Turn，相同文案也应显示。
            let next = view(lattice_api::local::new_turn_id(), Some(outcome));
            render_snapshot(&next, &mut render, false, &mut err).unwrap();
            assert_eq!(
                String::from_utf8(err)
                    .unwrap()
                    .lines()
                    .filter(|line| *line == expected)
                    .count(),
                2
            );
        }
    }
}

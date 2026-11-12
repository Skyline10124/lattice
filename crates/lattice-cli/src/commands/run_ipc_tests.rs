// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::test_support::signal_guard;
use serde_json::{Value, json};
use std::{os::unix::fs::PermissionsExt, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
};
const SESSION: &str = "11111111111141118111111111111111";
pub(super) fn sid() -> SessionId {
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
    request: Value,
}
impl Peer {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let layout = lattice_local::StateLayout::in_state_root(root.path());
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
    async fn next_request(&self) -> Wire {
        let (mut stream, _) = self.listener.accept().await.unwrap();
        let mut magic = [0; 8];
        stream.read_exact(&mut magic).await.unwrap();
        assert_eq!(&magic, lattice_local::test_support::FRAME_MAGIC);
        let length = stream.read_u32().await.unwrap();
        let mut bytes = vec![0; length as usize];
        stream.read_exact(&mut bytes).await.unwrap();
        let request: Value = serde_json::from_slice(&bytes).unwrap();
        Wire {
            stream,
            sequence: 0,
            request,
        }
    }
    async fn request(&self, op: &str) -> Wire {
        let wire = self.next_request().await;
        assert_eq!(wire.request["operation"], op);
        if op == "watch" || op == "head" {
            assert_eq!(wire.request["session_id"], SESSION);
        }
        wire
    }
    async fn assert_no_pending_request(&self) {
        let polled =
            std::future::poll_fn(|cx| std::task::Poll::Ready(self.listener.poll_accept(cx))).await;
        if let Ok((mut stream, _)) = match polled {
            std::task::Poll::Ready(res) => res,
            std::task::Poll::Pending => return,
        } {
            let mut magic = [0; 8];
            stream.read_exact(&mut magic).await.unwrap();
            let length = stream.read_u32().await.unwrap();
            let mut bytes = vec![0; length as usize];
            stream.read_exact(&mut bytes).await.unwrap();
            let extra: Value = serde_json::from_slice(&bytes).unwrap();
            panic!(
                "单次 configure 结束后不应残留未消费的 IPC 请求，实际收到: {}",
                extra["operation"]
            );
        }
    }
}
impl Wire {
    async fn reply(&mut self, response: &str, value: Value) {
        let fragment = serde_json::to_string(&json!({"response":response,"value":value})).unwrap();
        let bytes=serde_json::to_vec(&json!({"message_id":self.sequence,"offset":0,"json_fragment":fragment,"complete":true})).unwrap();
        self.sequence += 1;
        self.stream
            .write_all(lattice_local::test_support::FRAME_MAGIC)
            .await
            .unwrap();
        self.stream.write_u32(bytes.len() as u32).await.unwrap();
        self.stream.write_all(&bytes).await.unwrap();
    }
    async fn reply_head(&mut self, head: SessionHead) {
        self.reply("head", serde_json::to_value(head).unwrap())
            .await;
    }
    async fn view(&mut self, update: ViewUpdate) {
        self.reply("view", serde_json::to_value(update).unwrap())
            .await;
    }
}
async fn resolve_session(peer: &Peer) {
    let mut wire = peer.request("resolve_session").await;
    wire.reply("session", json!({"session_id":sid(),"project_path":b"/test-project".as_slice(),"current":2,"created_at":"2026-10-17T00:00:00Z","updated_at":"2026-10-17T00:00:01Z"})).await;
}

#[tokio::test]
async fn models_refresh_renders_confirmation_and_preserves_failure_reason_and_exit_class() {
    for json_mode in [false, true] {
        for (kind, expected_code) in [
            (None, 0),
            (Some("transport"), 1),
            (Some("internal"), 1),
            (Some("stopping"), 1),
            (Some("bad_request"), 2),
        ] {
            let peer = Peer::new();
            let mut out = Vec::new();
            let command = super::super::models::refresh(&peer.client, json_mode, &mut out);
            let reply = async {
                let mut wire = peer.request("models_refresh").await;
                assert_eq!(wire.request, json!({"operation":"models_refresh"}));
                if let Some(kind) = kind {
                    wire.reply(
                        "error",
                        json!({"kind":kind,"message":"specific refresh failure"}),
                    )
                    .await;
                } else {
                    wire.reply("models_refreshed", Value::Null).await;
                }
            };
            let (result, ()) = tokio::join!(command, reply);
            match result {
                Ok(()) => {
                    assert_eq!(expected_code, 0);
                    if json_mode {
                        assert_eq!(
                            serde_json::from_slice::<Value>(&out).unwrap(),
                            json!({"type":"models_refresh","status":"refreshed"})
                        );
                    } else {
                        assert_eq!(
                            String::from_utf8(out).unwrap(),
                            "Model metadata refreshed.\n"
                        );
                    }
                }
                Err(error) => {
                    assert!(out.is_empty());
                    let (code, value) = error_record(&error);
                    assert_eq!(code, expected_code);
                    assert_eq!(value["kind"], kind.unwrap());
                    assert_eq!(value["message"], "specific refresh failure");
                    let mut human = Vec::new();
                    render_human_error(&value, &mut human).unwrap();
                    assert_eq!(
                        String::from_utf8(human).unwrap(),
                        "error: specific refresh failure\n"
                    );
                }
            }
        }
    }
}

#[tokio::test]
async fn session_cancel_preserves_identity_and_exit_codes_across_reply_loss() {
    for json_mode in [true, false] {
        for case in ["unknown", "taken", "not_taken", "none", "missing"] {
            let peer = Peer::new();
            let id = lattice_local::new_turn_id();
            let mut out = Vec::new();
            let mut err = Vec::new();
            let command = super::super::session::cancel_current_turn(
                &peer.client,
                SESSION,
                json_mode,
                &mut out,
                &mut err,
            );
            let driver = async {
                if case == "missing" {
                    let mut wire = peer.request("resolve_session").await;
                    wire.reply(
                        "error",
                        json!({"kind":"not_found","message":"session not found"}),
                    )
                    .await;
                    return;
                }
                resolve_session(&peer).await;
                let mut wire = peer.request("head").await;
                wire.reply_head(head(id, case != "none")).await;
                if case != "none" {
                    let mut cancel = peer.request("cancel").await;
                    assert_eq!(cancel.request["turn_id"], id.to_string());
                    if case != "unknown" {
                        cancel
                            .reply("cancelled", json!({"owned":case=="taken"}))
                            .await;
                    }
                }
            };
            let (result, ()) = tokio::time::timeout(Duration::from_secs(3), async {
                tokio::join!(command, driver)
            })
            .await
            .unwrap();
            if case == "missing" {
                let error = result.unwrap_err();
                assert_eq!(error_record(&error).0, 2);
                continue;
            }
            assert!(
                result.is_ok(),
                "取消结果不明也要交付原 TurnId 的 cancel 记录，不能返回普通 error：{result:?}"
            );
            assert_eq!(
                result.unwrap(),
                std::process::ExitCode::from(u8::from(case == "unknown"))
            );
            let output = String::from_utf8(out).unwrap();
            if json_mode {
                let value: Value = serde_json::from_str(&output).unwrap();
                assert_eq!(value["type"], "cancel");
                assert_eq!(value["session_id"], SESSION);
                if case == "none" {
                    assert!(value["turn_id"].is_null());
                    assert_eq!(value["status"], "none");
                } else {
                    assert_eq!(value["turn_id"], id.to_string());
                    assert_eq!(value["status"], case);
                    if case == "unknown" {
                        assert_eq!(value["error"]["kind"], "transport");
                        assert!(value.get("owned").is_none());
                    } else {
                        assert_eq!(value["owned"], case == "taken");
                    }
                }
            } else if case == "unknown" {
                assert!(output.contains(&id.to_string()));
                assert!(output.contains("unknown"));
                assert!(!err.is_empty());
            }
        }
    }
}
#[tokio::test]
async fn in_flight_turn_query_uses_human_or_json_output_and_exits_zero() {
    for json_mode in [true, false] {
        let peer = Peer::new();
        let id = lattice_local::new_turn_id();
        let mut out = Vec::new();
        let mut err = Vec::new();
        let command = super::super::session::show_turn(
            &peer.client,
            id,
            false,
            json_mode,
            false,
            &mut out,
            &mut err,
        );
        let driver = async {
            let mut query = peer.request("turn").await;
            assert_eq!(query.request["turn_id"], id.to_string());
            query.reply("turn", json!({"status":"in_flight"})).await;
        };
        let (result, ()) = tokio::time::timeout(Duration::from_secs(3), async {
            tokio::join!(command, driver)
        })
        .await
        .unwrap();
        assert_eq!(result.unwrap(), std::process::ExitCode::SUCCESS);
        let text = String::from_utf8(out).unwrap();
        assert!(err.is_empty());
        assert_eq!(text.lines().count(), 1);
        if json_mode {
            assert_eq!(
                serde_json::from_str::<Value>(&text).unwrap(),
                json!({"status":"in_flight","turn_id":id})
            );
        } else {
            assert!(
                serde_json::from_str::<Value>(&text).is_err(),
                "人类模式在途说明不能是 JSON：{text}"
            );
            assert!(text.contains(&id.to_string()));
            assert!(text.contains("in flight"));
        }
    }
}
#[tokio::test]
async fn in_flight_follow_waits_for_acceptance_or_not_found() {
    let _guard = signal_guard().await;
    for json_mode in [true, false] {
        for accepted in [true, false] {
            let peer = Peer::new();
            let id = lattice_local::new_turn_id();
            let mut out = Vec::new();
            let mut err = Vec::new();
            let command = super::super::session::show_turn(
                &peer.client,
                id,
                true,
                json_mode,
                false,
                &mut out,
                &mut err,
            );
            let driver = async {
                let mut initial = peer.request("turn").await;
                initial.reply("turn", json!({"status":"in_flight"})).await;
                let mut pending =
                    tokio::time::timeout(Duration::from_secs(1), peer.request("turn"))
                        .await
                        .expect("follow 不能在准入仍在途时退出，必须等待接受或 NotFound");
                assert_eq!(pending.request["turn_id"], id.to_string());
                pending.reply("turn", json!({"status":"in_flight"})).await;
                let mut settled = peer.request("turn").await;
                assert_eq!(settled.request["turn_id"], id.to_string());
                if !accepted {
                    settled
                        .reply(
                            "error",
                            json!({"kind":"not_found","message":"turn not accepted"}),
                        )
                        .await;
                    return;
                }
                settled
                    .reply(
                        "turn",
                        json!({"status":"accepted","turn":detail(id,"completed")}),
                    )
                    .await;
                let mut view = peer.request("watch").await;
                view.view(ViewUpdate::Head(head(id, false))).await;
                view.view(ViewUpdate::Entry {
                    entry: entry(id, "completed"),
                })
                .await;
            };
            let (result, ()) = tokio::time::timeout(Duration::from_secs(3), async {
                tokio::join!(command, driver)
            })
            .await
            .unwrap();
            if accepted {
                assert_eq!(result.unwrap(), std::process::ExitCode::SUCCESS);
                let text = String::from_utf8(out).unwrap();
                if json_mode {
                    let values: Vec<Value> = text
                        .lines()
                        .map(|line| serde_json::from_str(line).unwrap())
                        .collect();
                    assert_eq!(
                        values
                            .iter()
                            .map(|v| v["type"].as_str().unwrap())
                            .collect::<Vec<_>>(),
                        ["head", "entry"]
                    );
                } else {
                    assert_eq!(text, "answer\n");
                    assert!(String::from_utf8(err).unwrap().contains("completed"));
                }
            } else {
                let error = result.unwrap_err();
                let (code, record) = error_record(&error);
                assert_eq!(code, 2);
                assert_eq!(record["kind"], "not_found");
                assert!(out.is_empty());
                assert!(err.is_empty());
            }
        }
    }
}
pub(super) fn detail(id: TurnId, outcome: &str) -> TurnDetail {
    let end = if outcome == "completed" {
        json!({"end":"finish"})
    } else {
        json!({"end":"stop","cause":{"cause":if outcome=="cancelled"{"cancelled"}else{"process_exit"}}})
    };
    let progress = if outcome == "reserved" {
        json!({"progress":"reserved"})
    } else {
        json!({"progress":"closed","end":end})
    };
    serde_json::from_value(json!({"session_id":sid(),"turn_id":id,"user_node":1,"agent_name":"test","accepted_at":"2026-10-17T00:00:00Z","ended_at":"2026-10-17T00:00:01Z","summary":{"outcome":if outcome=="reserved"{"failed"}else{outcome},"progress":progress,"notice":if outcome=="completed"{Value::Null}else{json!({"level":"error","text":"exact notice"})}},"usage":lattice_local::UsageSummary::from_attempts([]),"steps":[{"node":2,"key":{"turn_id":id,"ordinal":0},"progress":progress,"end":end,"attempts":[],"slots":[],"updated_at":"2026-10-17T00:00:01Z"}]})).unwrap()
}
pub(super) fn head(id: TurnId, owner: bool) -> SessionHead {
    SessionHead {
        model: Some(lattice_local::ModelId::parse("test").unwrap()),
        provider: Some(lattice_local::ProviderId::parse("openai-compatible").unwrap()),
        effort: None,
        settings_version: serde_json::from_value(json!("11".repeat(32))).unwrap(),
        session_id: sid(),
        position: NodeId(2),
        agent: "test".into(),
        turn_id: Some(id),
        owner,
        diagnostic: None,
        path_replaced: false,
    }
}
pub(super) fn entry(id: TurnId, outcome: &str) -> Box<ConversationEntry> {
    let turn = detail(id, outcome);
    Box::new(ConversationEntry::Step {
        node: NodeId(2),
        content: Node::Step {
            parent: NodeId(1),
            turn_id: id,
            step_ordinal: 0,
            progress: turn.summary.progress.clone(),
            attempts: vec![],
            reply: (outcome == "completed")
                .then(|| lattice_session::test_helpers::assistant_msg("answer")),
            slots: vec![],
            updated_at: turn.ended_at,
        },
        step: turn.steps[0].clone(),
        retries: vec![],
        summary: Some(turn.summary),
    })
}

#[tokio::test]
async fn follow_uses_watch_entries_or_one_detail_when_outside_the_initial_path() {
    let _guard = signal_guard().await;
    let peer = Peer::new();
    let id = lattice_local::new_turn_id();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let following = follow_with_output(
        &peer.client,
        detail(id, "completed"),
        false,
        true,
        &mut out,
        &mut err,
    );
    let driver = async {
        let mut wire = peer.request("watch").await;
        wire.view(ViewUpdate::Head(head(id, false))).await;
        wire.view(ViewUpdate::Entry {
            entry: entry(id, "completed"),
        })
        .await;
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(3), async {
        tokio::join!(following, driver)
    })
    .await
    .expect("watch progress");
    assert_eq!(result.unwrap(), std::process::ExitCode::SUCCESS);
    let records: Vec<Value> = String::from_utf8(out)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(
        records
            .iter()
            .map(|v| v["type"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["head", "entry"]
    );
    assert!(err.is_empty());
}
#[tokio::test]
async fn ended_turn_outside_window_or_rewound_to_root_is_one_detail() {
    let _guard = signal_guard().await;
    for root in [false, true] {
        for outcome in ["completed", "failed", "cancelled"] {
            let peer = Peer::new();
            let id = lattice_local::new_turn_id();
            let target = detail(id, outcome);
            let mut out = Vec::new();
            let mut err = Vec::new();
            let following = follow_with_output(
                &peer.client,
                target.clone(),
                false,
                true,
                &mut out,
                &mut err,
            );
            let driver = async {
                let other = lattice_local::new_turn_id();
                let mut wire = peer.request("watch").await;
                let mut h = head(other, true);
                if root {
                    h.position = NodeId(0);
                    h.turn_id = None;
                    h.owner = false;
                }
                wire.view(ViewUpdate::Head(h)).await;
                if !root {
                    wire.view(ViewUpdate::Entry {
                        entry: entry(other, "reserved"),
                    })
                    .await;
                }
                let mut query = peer.request("turn").await;
                assert_eq!(query.request["turn_id"], id.to_string());
                query
                    .reply("turn", json!({"status":"accepted","turn":target.clone()}))
                    .await;
            };
            let (result, ()) = tokio::time::timeout(Duration::from_secs(3), async {
                tokio::join!(following, driver)
            })
            .await
            .unwrap();
            assert_eq!(result.unwrap(), summary_exit(Some(&target.summary)));
            assert_eq!(
                String::from_utf8(out).unwrap(),
                format!("{}\n", serde_json::to_string(&target).unwrap())
            );
            assert!(err.is_empty());
        }
    }
}
#[tokio::test]
async fn known_completed_turn_with_incomplete_view_exits_one_with_connection_record() {
    let _guard = signal_guard().await;
    let peer = Peer::new();
    let id = lattice_local::new_turn_id();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let following = follow_with_output(
        &peer.client,
        detail(id, "completed"),
        false,
        true,
        &mut out,
        &mut err,
    );
    let driver = async {
        let mut wire = peer.request("watch").await;
        wire.view(ViewUpdate::Head(head(id, false))).await;
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(3), async {
        tokio::join!(following, driver)
    })
    .await
    .unwrap();
    assert_eq!(result.unwrap(), std::process::ExitCode::from(1));
    let text = String::from_utf8(out).unwrap();
    let records: Vec<Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(
        records
            .iter()
            .map(|r| r["type"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["head", "connection"]
    );
    assert_eq!(records[1]["status"], "eof");
}
#[tokio::test]
async fn follow_outside_window_refreshes_a_turn_that_completed_after_the_first_lookup() {
    let _guard = signal_guard().await;
    let peer = Peer::new();
    let id = lattice_local::new_turn_id();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let following = follow_with_output(
        &peer.client,
        detail(id, "reserved"),
        false,
        true,
        &mut out,
        &mut err,
    );
    let driver = async {
        let mut view = peer.request("watch").await;
        let other = lattice_local::new_turn_id();
        view.view(ViewUpdate::Head(head(other, false))).await;
        view.view(ViewUpdate::Entry {
            entry: entry(other, "completed"),
        })
        .await;
        let mut query = tokio::time::timeout(Duration::from_secs(1), peer.request("turn"))
            .await
            .expect("窗口外的结果必须重新查询，不能沿用打开 watch 前的在途投影");
        assert_eq!(query.request["turn_id"], id.to_string());
        query
            .reply(
                "turn",
                json!({"status":"accepted","turn":detail(id,"completed")}),
            )
            .await;
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(3), async {
        tokio::join!(following, driver)
    })
    .await
    .unwrap();
    assert_eq!(result.unwrap(), std::process::ExitCode::SUCCESS);
    assert_eq!(
        String::from_utf8(out).unwrap(),
        format!(
            "{}\n",
            serde_json::to_string(&detail(id, "completed")).unwrap()
        )
    );
}
#[tokio::test]
async fn follow_requeries_latest_conclusion_after_rewind_into_the_selected_turn() {
    let _guard = signal_guard().await;
    for user_position in [true, false] {
        for json_mode in [true, false] {
            let peer = Peer::new();
            let id = lattice_local::new_turn_id();
            let mut out = Vec::new();
            let mut err = Vec::new();
            let mut latest = detail(id, "completed");
            let mut middle = entry(id, "completed");
            if let ConversationEntry::Step {
                content: Node::Step { progress, .. },
                step,
                summary,
                ..
            } = middle.as_mut()
            {
                *progress = StepProgress::Closed {
                    end: StepEnd::Continue,
                };
                step.progress = progress.clone();
                step.end = StepEnd::Continue;
                *summary = None;
                latest.steps.insert(0, step.clone());
                latest.steps[1].node = NodeId(3);
                latest.steps[1].key.ordinal = 1;
            }
            if user_position {
                *middle = ConversationEntry::UserMessage {
                    node: NodeId(1),
                    ordinal: 1,
                    content: Node::UserMessage {
                        effort: None,
                        parent: NodeId(0),
                        turn_id: id,
                        accepted_at: latest.accepted_at,
                        agent_name: lattice_local::AgentName::parse("test").unwrap(),
                        model: lattice_local::ModelId::parse("test").unwrap(),
                        provider: lattice_local::ProviderId::parse("test").unwrap(),
                        limits: lattice_session::store::TurnExecutionLimits {
                            timeout_secs: lattice_session::store::TurnTimeout::default(),
                            max_model_steps: 32,
                            max_model_attempts: 1,
                        },
                        config_digest: [0; 32],
                        message: lattice_session::test_helpers::user_msg("input"),
                    },
                };
            }
            let command = super::super::session::show_turn(
                &peer.client,
                id,
                true,
                json_mode,
                false,
                &mut out,
                &mut err,
            );
            let driver = async {
                let mut initial = peer.request("turn").await;
                initial
                    .reply(
                        "turn",
                        json!({"status":"accepted","turn":detail(id,"reserved")}),
                    )
                    .await;
                // 首次查询后完成，再退回本轮中途；开头有本轮条目，但没有最后一步。
                let mut view = peer.request("watch").await;
                let mut h = head(id, false);
                h.position = middle.node();
                view.view(ViewUpdate::Head(h)).await;
                view.view(ViewUpdate::Entry { entry: middle }).await;
                let mut query = tokio::time::timeout(Duration::from_secs(1), peer.request("turn"))
                    .await
                    .expect("只收到中途条目必须重查最新结局，不能沿用首查投影");
                assert_eq!(query.request["turn_id"], id.to_string());
                query
                    .reply("turn", json!({"status":"accepted","turn":latest.clone()}))
                    .await;
            };
            let (result, ()) = tokio::time::timeout(Duration::from_secs(3), async {
                tokio::join!(command, driver)
            })
            .await
            .unwrap();
            assert_eq!(result.unwrap(), std::process::ExitCode::SUCCESS);
            let expected = if json_mode {
                format!("{}\n", serde_json::to_string(&latest).unwrap())
            } else {
                super::super::session::render_turn(&latest)
            };
            assert_eq!(
                String::from_utf8(out).unwrap(),
                expected,
                "中途条目与 head 均不能泄漏到详情回退输出"
            );
            assert!(err.is_empty());
        }
    }
}
#[tokio::test]
async fn sigint_uses_the_original_turn_and_second_interrupt_detaches() {
    let _guard = signal_guard().await;
    let peer = Peer::new();
    let id = lattice_local::new_turn_id();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let following = follow_with_output(
        &peer.client,
        detail(id, "reserved"),
        false,
        true,
        &mut out,
        &mut err,
    );
    let driver = async {
        let mut wire = peer.request("watch").await;
        wire.view(ViewUpdate::Head(head(id, true))).await;
        wire.view(ViewUpdate::Entry {
            entry: entry(id, "reserved"),
        })
        .await;
        assert!(
            std::process::Command::new("kill")
                .args(["-INT", &std::process::id().to_string()])
                .status()
                .unwrap()
                .success()
        );
        let mut cancel = peer.request("cancel").await;
        assert_eq!(cancel.request["turn_id"], id.to_string());
        cancel.reply("cancelled", json!({"owned":true})).await;
        assert!(
            std::process::Command::new("kill")
                .args(["-INT", &std::process::id().to_string()])
                .status()
                .unwrap()
                .success()
        );
        let mut byte = [0];
        assert_eq!(wire.stream.read(&mut byte).await.unwrap(), 0);
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(7), async {
        tokio::join!(following, driver)
    })
    .await
    .unwrap();
    assert_eq!(result.unwrap(), std::process::ExitCode::from(1));
    let records: Vec<Value> = String::from_utf8(out)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(
        records
            .iter()
            .filter(|r| r["type"] == "cancel"
                && r["turn_id"] == id.to_string()
                && r["status"] == "taken")
            .count(),
        1
    );
}

#[tokio::test]
async fn late_acceptance_keeps_the_printed_unknown_receipt_and_exit_one() {
    let _guard = signal_guard().await;
    let peer = Peer::new();
    let id = lattice_local::new_turn_id();
    let turn = peer.client.start_turn(TurnRequest {
        turn_id: id,
        session: sid(),
        workspace: peer._root.path().into(),
        input: "input".into(),
        expected_settings: None,
    });
    let mut submit = peer.request("submit").await;
    turn.cancel();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let signal = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()).unwrap();
    let command = consume(
        turn,
        None,
        RunRenderState::default(),
        false,
        true,
        signal,
        &mut out,
        &mut err,
    );
    let driver = async {
        let mut cancel = peer.request("cancel").await;
        cancel.reply("cancelled", json!({"owned":true})).await;
        // 首次取消等待五秒后给出 unknown，连接仍在；随后才交付迟到的接受回执。
        let mut waiting = peer.request("watch").await;
        waiting.view(ViewUpdate::Head(head(id, true))).await;
        submit
            .reply(
                "acceptance",
                json!({"acceptance":"accepted","turn":TurnView::from(&detail(id,"completed"))}),
            )
            .await;
        let mut final_view = peer.request("watch").await;
        final_view.view(ViewUpdate::Head(head(id, false))).await;
        final_view
            .view(ViewUpdate::Entry {
                entry: entry(id, "completed"),
            })
            .await;
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(8), async {
        tokio::join!(command, driver)
    })
    .await
    .unwrap();
    let records: Vec<Value> = String::from_utf8(out)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let receipts: Vec<_> = records
        .iter()
        .filter(|record| record["stage"] == "receipt")
        .collect();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0]["result"], "unknown");
    assert!(
        records
            .iter()
            .any(|record| record["summary"]["outcome"] == "completed")
    );
    assert_eq!(result.unwrap(), std::process::ExitCode::from(1));
}

#[tokio::test]
async fn send_configure_outcomes_and_exit_codes() {
    let _guard = signal_guard().await;
    // 1. configure 被拒绝：只发一次 configure，依次输出 configure(rejected) 与同一错误的 submit(rejected)，不发 submit。
    for (kind, expected_exit) in [
        ("bad_request", 2u8),
        ("conflict", 2u8),
        ("store_unavailable", 1u8),
        ("busy", 1u8),
    ] {
        let peer = Peer::new();
        let mut out = Vec::new();
        let mut err = Vec::new();
        let workspace = peer._root.path().to_path_buf();
        let (result, new_sid) = {
            let send = run_session_prompt_with_io(
                &peer.client,
                super::super::session::SendArgs {
                    prompt: "hello".into(),
                    session_id: None,
                    agent: Some(lattice_local::AgentName::parse("test").unwrap()),
                    model: None,
                    effort: None,
                },
                false,
                true,
                Some(workspace),
                &mut out,
                &mut err,
            );
            tokio::pin!(send);
            let new_sid = tokio::time::timeout(Duration::from_secs(3), async {
                tokio::select! {
                    res = &mut send => panic!("configure 被消费前命令不应提前退出：{res:?}"),
                    mut cfg = peer.request("configure") => {
                        let sid_str = cfg.request["request"]["session"]["new"]
                            .as_str()
                            .expect("新建会话必须发送 configure(new)")
                            .to_string();
                        assert!(SessionId::parse(&sid_str).is_ok(), "SessionId 格式无效：{sid_str}");
                        assert_eq!(cfg.request["request"]["agent"], "test");
                        cfg.reply(
                            "error",
                            json!({"kind":kind,"message":"exact configure failure","recovery":null,"store":null}),
                        )
                        .await;
                        sid_str
                    }
                }
            })
            .await
            .expect("等待首次 configure 请求超时");
            let result = tokio::time::timeout(Duration::from_secs(3), async {
                tokio::select! {
                    res = &mut send => res,
                    extra = peer.next_request() => {
                        panic!(
                            "configure({kind}) 被拒后只应发一次 configure 且不发 submit，实际又收到 IPC 请求：{}",
                            extra.request["operation"]
                        );
                    }
                }
            })
            .await
            .expect("configure 被拒后命令未在时限内退出");
            (result, new_sid)
        };
        peer.assert_no_pending_request().await;
        assert_eq!(
            result.unwrap(),
            std::process::ExitCode::from(expected_exit),
            "{kind} 退出码不符"
        );
        let records: Vec<Value> = String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(
            records.len(),
            3,
            "{kind} 被拒时应恰好输出 sending + configure(rejected) + receipt(rejected)：{records:?}"
        );
        assert_eq!(records[0]["type"], "submit");
        assert_eq!(records[0]["stage"], "sending");
        assert_eq!(records[0]["session_id"], new_sid);
        assert_eq!(records[1]["type"], "configure");
        assert_eq!(records[1]["session_id"], new_sid);
        assert_eq!(records[1]["result"], "rejected");
        assert_eq!(records[1]["error"]["kind"], kind);
        assert_eq!(records[1]["error"]["message"], "exact configure failure");
        assert_eq!(records[2]["type"], "submit");
        assert_eq!(records[2]["stage"], "receipt");
        assert_eq!(records[2]["turn_id"], records[0]["turn_id"]);
        assert_eq!(records[2]["result"], "rejected");
        assert_eq!(records[2]["recovered"], false);
        assert_eq!(records[2]["error"], records[1]["error"]);
    }

    // 2. configure 结果不明（请求写出后连接关闭、读不到回复）：新建与已有会话均只发一次 configure，输出 configure(unknown) 与 submit(not_accepted)，退出 1，不发 submit。
    for existing in [false, true] {
        let peer = Peer::new();
        let mut out = Vec::new();
        let mut err = Vec::new();
        let workspace = peer._root.path().to_path_buf();
        let (result, target_sid) = {
            let send = run_session_prompt_with_io(
                &peer.client,
                super::super::session::SendArgs {
                    prompt: "reply lost".into(),
                    session_id: existing.then(|| "1111".into()),
                    agent: (!existing).then(|| lattice_local::AgentName::parse("test").unwrap()),
                    model: existing
                        .then(|| lattice_local::ModelRef::parse("local/org/model").unwrap()),
                    effort: None,
                },
                false,
                true,
                Some(workspace),
                &mut out,
                &mut err,
            );
            tokio::pin!(send);
            let target_sid = tokio::time::timeout(Duration::from_secs(3), async {
                tokio::select! {
                    res = &mut send => panic!("configure 断连前命令不应提前退出：{res:?}"),
                    sid_str = async {
                        if existing {
                            resolve_session(&peer).await;
                        }
                        let wire = peer.request("configure").await;
                        let sid_str = if existing {
                            assert_eq!(
                                wire.request["request"]["session"],
                                json!({"existing":sid()})
                            );
                            SESSION.to_string()
                        } else {
                            wire.request["request"]["session"]["new"]
                                .as_str()
                                .expect("新建会话必须发送 configure(new)")
                                .to_string()
                        };
                        drop(wire);
                        sid_str
                    } => sid_str,
                }
            })
            .await
            .expect("等待 configure 请求超时");
            let result = tokio::time::timeout(Duration::from_secs(3), async {
                tokio::select! {
                    res = &mut send => res,
                    extra = peer.next_request() => {
                        panic!(
                            "configure 结果不明后不应重发 configure、查询 head 或发送 submit（existing={existing}），实际收到 IPC 请求：{}",
                            extra.request["operation"]
                        );
                    }
                }
            })
            .await
            .expect("configure 结果不明后命令未在时限内退出");
            (result, target_sid)
        };
        peer.assert_no_pending_request().await;
        assert_eq!(
            result.unwrap(),
            std::process::ExitCode::from(1),
            "configure 结果不明必须以退出码 1 结束（existing={existing}）"
        );
        let records: Vec<Value> = String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(
            records.len(),
            3,
            "configure 结果不明时应恰好输出 sending + configure(unknown) + receipt(not_accepted)：{records:?}"
        );
        assert_eq!(records[0]["type"], "submit");
        assert_eq!(records[0]["stage"], "sending");
        assert_eq!(records[0]["session_id"], target_sid);
        assert_eq!(records[1]["type"], "configure");
        assert_eq!(records[1]["session_id"], target_sid);
        assert_eq!(records[1]["result"], "unknown");
        assert_eq!(records[1]["error"]["kind"], "transport");
        assert_eq!(records[2]["type"], "submit");
        assert_eq!(records[2]["stage"], "receipt");
        assert_eq!(records[2]["turn_id"], records[0]["turn_id"]);
        assert_eq!(records[2]["result"], "not_accepted");
        assert_eq!(records[2]["recovered"], false);
    }

    // 3. configure 等待回复期间收到 SIGINT：输出无错误的 configure(unknown) 与 submit(not_accepted)，退出 1，不重发且不发 submit。
    {
        let peer = Peer::new();
        let mut out = Vec::new();
        let mut err = Vec::new();
        let workspace = peer._root.path().to_path_buf();
        let (result, new_sid) = {
            let send = run_session_prompt_with_io(
                &peer.client,
                super::super::session::SendArgs {
                    prompt: "interrupted configure".into(),
                    session_id: None,
                    agent: Some(lattice_local::AgentName::parse("test").unwrap()),
                    model: None,
                    effort: None,
                },
                false,
                true,
                Some(workspace),
                &mut out,
                &mut err,
            );
            tokio::pin!(send);
            let held_wire = tokio::time::timeout(Duration::from_secs(3), async {
                tokio::select! {
                    res = &mut send => panic!("收到 SIGINT 前命令不应退出：{res:?}"),
                    wire = peer.request("configure") => wire,
                }
            })
            .await
            .expect("等待 configure 请求超时");
            let new_sid = held_wire.request["request"]["session"]["new"]
                .as_str()
                .unwrap()
                .to_string();
            assert!(
                std::process::Command::new("kill")
                    .args(["-INT", &std::process::id().to_string()])
                    .status()
                    .unwrap()
                    .success()
            );
            let result = tokio::time::timeout(Duration::from_secs(3), async {
                tokio::select! {
                    res = &mut send => res,
                    extra = peer.next_request() => {
                        panic!(
                            "configure 期间收到 SIGINT 后不应再发起 IPC 请求，实际收到：{}",
                            extra.request["operation"]
                        );
                    }
                }
            })
            .await
            .expect("SIGINT 中断 configure 后命令未在时限内退出");
            drop(held_wire);
            (result, new_sid)
        };
        peer.assert_no_pending_request().await;
        assert_eq!(result.unwrap(), std::process::ExitCode::from(1));
        let records: Vec<Value> = String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(records.len(), 3, "{records:?}");
        assert_eq!(records[0]["stage"], "sending");
        assert_eq!(records[0]["session_id"], new_sid);
        assert_eq!(records[1]["type"], "configure");
        assert_eq!(records[1]["session_id"], new_sid);
        assert_eq!(records[1]["result"], "unknown");
        assert!(records[1]["error"].is_null(), "{:?}", records[1]);
        assert_eq!(records[2]["stage"], "receipt");
        assert_eq!(records[2]["turn_id"], records[0]["turn_id"]);
        assert_eq!(records[2]["result"], "not_accepted");
        assert_eq!(records[2]["recovered"], false);
    }
}

#[tokio::test]
async fn send_configures_new_and_explicit_sessions_before_submitting() {
    const MODE: &str = "LATTICE_TEST_CLI_SUBMISSION_MODE";
    if let Ok(mode) = std::env::var(MODE) {
        let peer = Peer::new();
        let existing = mode == "existing" || mode == "explicit";
        let explicit = mode == "explicit";
        let send = run_session_prompt(
            &peer.client,
            super::super::session::SendArgs {
                prompt: "original input".into(),
                session_id: existing.then(|| "1111".into()),
                agent: (!existing).then(|| lattice_local::AgentName::parse("test").unwrap()),
                model: explicit.then(|| lattice_local::ModelRef::parse("local/org/model").unwrap()),
                effort: explicit.then_some(lattice_local::Effort::Off),
            },
            false,
            true,
        );
        let driver = async {
            if existing {
                let mut resolve = peer.request("resolve_session").await;
                resolve.reply("session",json!({"session_id":sid(),"project_path":b"/test-project".as_slice(),"current":2,"created_at":"2026-10-17T00:00:00Z","updated_at":"2026-10-17T00:00:01Z"})).await;
            }
            let expected_sid = if mode == "new" || explicit {
                let mut cfg = peer.request("configure").await;
                let creq = &cfg.request["request"];
                let target_sid: SessionId = if mode == "new" {
                    let parsed: SessionId =
                        creq["session"]["new"].as_str().unwrap().parse().unwrap();
                    assert_eq!(creq["agent"], "test");
                    assert!(creq["model"].is_null());
                    assert!(creq["effort"].is_null());
                    parsed
                } else {
                    assert_eq!(creq["session"], json!({"existing":sid()}));
                    assert!(creq["agent"].is_null());
                    assert_eq!(creq["model"], "local/org/model");
                    assert_eq!(creq["effort"], "off");
                    sid()
                };
                let mut configured_head = head(lattice_local::new_turn_id(), false);
                configured_head.session_id = target_sid;
                cfg.reply(
                    "configure",
                    json!({"head":configured_head,"adjustment":null}),
                )
                .await;
                target_sid
            } else {
                sid()
            };
            let mut wire = peer.request("submit").await;
            let request = &wire.request["request"];
            assert_eq!(request["input"], "original input");
            assert_eq!(request["session"], expected_sid.to_string());
            assert!(request["expected_settings"].is_null());
            assert!(TurnId::parse(request["turn_id"].as_str().unwrap()).is_ok());
            assert_eq!(
                wire.request
                    .as_object()
                    .unwrap()
                    .keys()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
                vec!["operation", "request"]
            );
            wire.reply("error",json!({"kind":"bad_request","message":"mock rejection","recovery":null,"store":null})).await;
        };
        let (result, ()) =
            tokio::time::timeout(Duration::from_secs(5), async { tokio::join!(send, driver) })
                .await
                .expect("submission progress");
        assert_eq!(result.unwrap(), std::process::ExitCode::from(2));
        return;
    }
    for mode in ["new", "existing", "explicit"] {
        let root = tempfile::tempdir().unwrap();
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command.env_clear().env("PATH","/usr/bin:/bin").env("HOME",root.path()).env(MODE,mode).current_dir(root.path()).args(["--exact","commands::run::ipc_tests::send_configures_new_and_explicit_sessions_before_submitting","--nocapture"]);
        let output = tokio::task::spawn_blocking(move || command.output().unwrap())
            .await
            .unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(output.status.success(), "{stdout}\n{stderr}");
        let records: Vec<Value> = stdout
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        if mode == "existing" {
            assert_eq!(records.len(), 2, "不带选择的已有会话追加不发 configure");
            assert_eq!(records[0]["stage"], "sending");
            assert_eq!(records[1]["stage"], "receipt");
            assert_eq!(records[1]["result"], "rejected");
            assert_eq!(records[0]["turn_id"], records[1]["turn_id"]);
        } else {
            assert_eq!(records.len(), 3);
            assert_eq!(records[0]["stage"], "sending");
            assert_eq!(records[1]["type"], "configure");
            assert_eq!(records[1]["result"], "configured");
            assert_eq!(records[2]["stage"], "receipt");
            assert_eq!(records[2]["result"], "rejected");
            assert_eq!(records[0]["turn_id"], records[2]["turn_id"]);
        }
    }
}

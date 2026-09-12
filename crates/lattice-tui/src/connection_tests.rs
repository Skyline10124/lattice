// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::app::StatusKind;
use serde_json::{Value, json};
use std::{os::unix::fs::PermissionsExt, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixListener,
};

#[tokio::test]
async fn reopening_visible_history_reads_completion_without_confirming() {
    for (message, body_unavailable, snapshot_first, outcome) in [
        (
            Some("original durability failure"),
            false,
            false,
            json!({"completed":{"finish_reason":"stop"}}),
        ),
        (
            Some("durability confirmation unknown"),
            false,
            false,
            json!({"completed":{"finish_reason":"stop"}}),
        ),
        (
            None,
            true,
            false,
            json!({"completed":{"finish_reason":"stop"}}),
        ),
        (
            Some("original durability failure"),
            true,
            false,
            json!({"failed":{"reason":"model_request_failed"}}),
        ),
        (
            None,
            true,
            false,
            json!({"cancelled":{"reason":"user_cancelled"}}),
        ),
        (
            None,
            true,
            true,
            json!({"completed":{"finish_reason":"stop"}}),
        ),
        (
            Some("original durability failure"),
            true,
            true,
            json!({"failed":{"reason":"model_request_failed"}}),
        ),
        (
            None,
            true,
            true,
            json!({"cancelled":{"reason":"user_cancelled"}}),
        ),
        (
            None,
            true,
            true,
            json!({"interrupted":{"reason":"process_interrupted_before_model_attempt"}}),
        ),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let layout = lattice_api::StateLayout::in_state_root(directory.path());
        let local = layout.lattice_root().join("local");
        std::fs::create_dir_all(&local).unwrap();
        std::fs::set_permissions(&local, std::fs::Permissions::from_mode(0o700)).unwrap();
        let socket = local.join("api.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
        let turn_id = lattice_api::local::new_turn_id();
        let commit = lattice_api::local::CommittedTurnObservation {
            commit_id: "fixed-head".into(),
            turn_id,
            parent_ids: vec![],
            agent_name: "test".into(),
            model: "test".into(),
            provider: "test".into(),
            outcome: serde_json::from_value(outcome).unwrap(),
            origin: lattice_api::ClosureOrigin::Live,
            usage: None,
            workspace_history_checkpoint: lattice_api::WorkspaceHistoryCheckpoint::Empty,
            created_at: String::new(),
        };
        let view = SessionObservation {
            state_oid: "state".into(),
            session: lattice_api::GraphSession {
                session_id: "session".into(),
                label: None,
                active_agent_name: "test".into(),
                created_at: String::new(),
                updated_at: String::new(),
                project_target_branch: Some("main".into()),
                project_store_key: "key".into(),
            },
            head: lattice_api::GraphHead::Branch {
                name: "main".into(),
            },
            head_commit_id: Some(commit.commit_id.clone()),
            branches: vec![],
            commit_count: 1,
            active_turn: None,
            requested_turn: None,
        };
        let error = message.map(|message| LocalError {
            kind: LocalErrorKind::StoreUnavailable,
            message: message.into(),
            recovery: None,
        });
        let expected = error.clone().or_else(|| body_unavailable.then(read_error));
        let expected_outcome = commit.outcome.clone();
        // 只替代 IPC 对端：真实 Connection、历史重载和 DTO 解码均运行，任何写操作都会失败。
        let (finish_tx, mut finish_rx) = tokio::sync::oneshot::channel();
        let mut finish_tx = Some(finish_tx);
        let server = tokio::spawn(async move {
            let mut completion = None;
            loop {
                let mut stream = tokio::select! {
                    accepted = listener.accept() => accepted.unwrap().0,
                    _ = &mut finish_rx, if completion.is_some() => {
                        let (mut stream, response) = completion.take().unwrap();
                        send_response(&mut stream, &response).await;
                        continue;
                    }
                };
                let mut magic = [0; 8];
                stream.read_exact(&mut magic).await.unwrap();
                assert_eq!(&magic, b"LTLOCAL2");
                let length = stream.read_u32().await.unwrap() as usize;
                assert!(length <= 16 * 1024 * 1024);
                let mut bytes = vec![0; length];
                stream.read_exact(&mut bytes).await.unwrap();
                let request: Value = serde_json::from_slice(&bytes).unwrap();
                let response = match request["operation"].as_str().unwrap() {
                    "query" => json!({"response":"view", "value":view}),
                    "commit" => json!({"response":"commit", "value":commit}),
                    "history" => {
                        json!({"response":"history", "value": if request["offset"] == 0 { vec!["fixed-head"] } else { vec![] }})
                    }
                    "transcript" if body_unavailable => {
                        json!({"response":"error", "value":read_error()})
                    }
                    "transcript" => {
                        json!({"response":"transcript", "value":{"jsonl":"", "next_offset":null}})
                    }
                    "subscribe" => {
                        assert_eq!(request["turn_id"], json!(turn_id));
                        let mut fixed = view.clone();
                        fixed.requested_turn =
                            Some(TurnObservation::Committed(Box::new(commit.clone())));
                        let finished = json!({"response":"observation", "value":Observation::Finished {
                            view: Box::new(fixed.clone()), cursor: lattice_api::local::EventCursor { epoch:"test".into(), sequence:1 }, error:error.clone(),
                        }});
                        if snapshot_first {
                            let snapshot = json!({"response":"observation", "value":Observation::Snapshot {
                                view: Box::new(fixed), cursor: lattice_api::local::EventCursor { epoch:"test".into(), sequence:0 }, owned:true,
                            }});
                            send_response(&mut stream, &snapshot).await;
                            // 第一份快照及正文读取失败交接后，才允许独立完成回执到达。
                            completion = Some((stream, finished));
                            continue;
                        }
                        finished
                    }
                    other => panic!("reconnect performed non-read operation: {other}"),
                };
                send_response(&mut stream, &response).await;
            }
        });
        let mut connection =
            Connection::start(LocalClient::new(layout), "session".into(), None, None);
        for index in 0..=usize::from(snapshot_first) {
            let snapshot = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    match connection.next().await {
                        Some(update @ Update::Snapshot { .. }) => break update,
                        Some(
                            Update::SelectedTurn { .. }
                            | Update::NotSubmitted { .. }
                            | Update::Gap
                            | Update::Event(_),
                        ) => {}
                        Some(Update::Error(error)) => panic!("{error:?}"),
                        None => panic!("no snapshot"),
                    }
                }
            })
            .await
            .unwrap();
            let Update::Snapshot {
                view,
                blocks,
                error,
                owned,
                confirmed,
            } = snapshot
            else {
                unreachable!()
            };
            let first_owned = snapshot_first && index == 0;
            assert_eq!(owned, first_owned);
            assert_eq!(confirmed, !first_owned && message.is_none());
            assert!(
                matches!(view.requested_turn, Some(TurnObservation::Committed(ref turn)) if turn.turn_id == turn_id && turn.commit_id == "fixed-head" && turn.outcome == expected_outcome)
            );
            assert_eq!(
                error,
                if first_owned {
                    Some(read_error())
                } else {
                    expected.clone()
                }
            );
            assert_eq!(blocks.is_none(), body_unavailable);
            if let Some(blocks) = blocks {
                assert!(!blocks.iter().any(|block| matches!(
                    block,
                    Block::Status {
                        kind: StatusKind::Committed { .. },
                        ..
                    }
                )));
            }
            if first_owned {
                finish_tx.take().unwrap().send(()).unwrap();
            }
        }
        assert!(connection.next().await.is_none());
        server.abort();
        let _ = server.await;
    }
}

async fn send_response(stream: &mut tokio::net::UnixStream, response: &Value) {
    let bytes = serde_json::to_vec(response).unwrap();
    stream.write_all(b"LTLOCAL2").await.unwrap();
    stream.write_u32(bytes.len() as u32).await.unwrap();
    stream.write_all(&bytes).await.unwrap();
}

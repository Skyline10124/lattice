// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use lattice_api::{StateLayout, local::*};
use lattice_session::store::*;
use std::time::Duration;

fn session_id() -> SessionId {
    SessionId::parse("5e550000000000000000000000000001").unwrap()
}
fn turn_id() -> TurnId {
    TurnId::parse("00000000-0000-4000-8000-000000000001").unwrap()
}
fn seed(layout: &StateLayout, root: &std::path::Path) {
    let store = SessionStore::create(layout).unwrap();
    seed_session(&store, root, session_id(), turn_id());
}
fn seed_session(store: &SessionStore, root: &std::path::Path, sid: SessionId, tid: TurnId) {
    let guard = store.try_lock_session(sid).unwrap();
    store
        .accept_turn(
            &guard,
            AcceptTurn {
                session: AcceptSession::New { id: sid },
                turn_id: tid,
                project_store_key: lattice_api::LocalWorkspaceRoot::from_path(root)
                    .unwrap()
                    .store_key(),
                input: "原始输入".into(),
                accepted_at: chrono::Utc::now(),
                agent_selection: AgentSelection::Explicit,
                config: FrozenTurnConfig {
                    agent_name: "alpha".into(),
                    model: "test".into(),
                    provider: "openai-compatible".into(),
                    system_prompt: None,
                    catalog: ToolCatalogSnapshot::new("test".into(), vec![]).unwrap(),
                    limits: TurnExecutionLimits {
                        timeout_secs: 300,
                        max_model_steps: 32,
                        max_model_attempts: 2,
                    },
                },
            },
        )
        .unwrap();
}

#[tokio::test]
async fn usage_identity_resolution_skips_conversation_and_configuration() {
    tokio::time::timeout(Duration::from_secs(10), async {
        for (unrelated, field) in [
            (false, "message"),
            (false, "config_digest"),
            (true, "message"),
        ] {
            let state = tempfile::tempdir().unwrap();
            let project = tempfile::tempdir().unwrap();
            let layout = StateLayout::in_state_root(state.path());
            let store = std::sync::Arc::new(SessionStore::create(&layout).unwrap());
            let other = SessionId::parse("ffff0000000000000000000000000001").unwrap();
            seed_session(&store, project.path(), session_id(), turn_id());
            seed_session(
                &store,
                project.path(),
                other,
                TurnId::parse("00000000-0000-4000-8000-000000000002").unwrap(),
            );
            let api = lattice_api::SessionApi::from_store(store.clone());
            let id = session_id().to_string();
            let expected = api.usage(id.clone()).await.unwrap();
            store
                .edit_test_node(
                    NodeRef {
                        session_id: if unrelated { other } else { session_id() },
                        node: NodeId(1),
                    },
                    |wire| {
                        wire[field] = if field == "config_digest" {
                            serde_json::json!("0".repeat(64))
                        } else {
                            serde_json::json!(false)
                        };
                    },
                )
                .unwrap();
            for input in [&id, &id[..8]] {
                assert_eq!(
                    api.usage(input.to_owned()).await.unwrap_or_else(|error| {
                        panic!("unrelated={unrelated} field={field} input={input}: {error}")
                    }),
                    expected,
                    "unrelated={unrelated} field={field} input={input}"
                );
            }
        }
    })
    .await
    .expect("usage identity resolution timed out");
}
async fn ready(client: &LocalClient) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while client.status().await.is_err() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn lookup_and_watch_an_orphan_without_changing_its_facts() {
    let state = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let layout = StateLayout::in_state_root(state.path());
    seed(&layout, project.path());
    let serving = tokio::spawn(serve_for_test(layout.clone(), Some(state.path().into())));
    let client = LocalClient::new(layout);
    ready(&client).await;
    let lookup = client.turn(turn_id()).await;
    assert!(
        matches!(lookup, Ok(TurnLookup::Accepted { ref turn }) if turn.turn_id == turn_id() && !turn.summary.progress.is_closed()),
        "已持久接受且没有 Owner 的 Turn 仍必须能按身份查询：{lookup:?}"
    );
    let mut view = client.watch(session_id()).await.unwrap();
    let head = view.next().await.unwrap().unwrap();
    assert!(matches!(
        head,
        ViewUpdate::Head(SessionHead {
            position: NodeId(2),
            owner: false,
            path_replaced: false,
            ..
        })
    ));
    assert!(
        matches!(view.next().await.unwrap().unwrap(), ViewUpdate::Entry { entry } if matches!(*entry, ConversationEntry::UserMessage { node: NodeId(1), ordinal: 1, .. }))
    );
    assert!(
        matches!(view.next().await.unwrap().unwrap(), ViewUpdate::Entry { entry } if matches!(*entry, ConversationEntry::Step { node: NodeId(2), summary: Some(_), .. }))
    );
    assert!(
        client
            .history(session_id(), NodeId(1), 10)
            .await
            .unwrap()
            .entries
            .is_empty()
    );
    assert_eq!(
        client
            .history(session_id(), NodeId(2), 10)
            .await
            .unwrap_err()
            .kind,
        LocalErrorKind::BadRequest
    );
    assert_eq!(
        client
            .history(session_id(), NodeId(1), 0)
            .await
            .unwrap_err()
            .kind,
        LocalErrorKind::BadRequest
    );
    assert!(
        client
            .history(session_id(), NodeId(1), 100)
            .await
            .unwrap()
            .entries
            .is_empty()
    );
    for turns in [101, u32::MAX] {
        assert_eq!(
            client
                .history(session_id(), NodeId(1), turns)
                .await
                .unwrap_err()
                .kind,
            LocalErrorKind::BadRequest
        );
    }
    assert!(!client.list_sessions().await.unwrap()[0].owner);
    assert_eq!(client.turn(turn_id()).await.unwrap(), lookup.unwrap());
    drop(view);
    client.stop().await.unwrap();
    serving.await.unwrap().unwrap();
}

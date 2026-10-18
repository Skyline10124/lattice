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
    let sid = session_id();
    let guard = store.try_lock_session(sid).unwrap();
    store
        .accept_turn(
            &guard,
            AcceptTurn {
                session: AcceptSession::New { id: sid },
                turn_id: turn_id(),
                project_store_key: lattice_api::LocalWorkspaceRoot::from_path(root)
                    .unwrap()
                    .store_key(),
                input: "原始输入".into(),
                accepted_at: chrono::Utc::now(),
                agent_selection: AgentSelection::Explicit,
                config: FrozenTurnConfig {
                    agent_name: "alpha".into(),
                    model: "test".into(),
                    provider: "openai_compatible".into(),
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
    let serving = tokio::spawn(serve(layout.clone()));
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

// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn acceptance_deduplicates_configs_and_preserves_abandoned_nodes() {
    let (_d, layout, db) = store();
    let lock = db.try_lock_session(sid(1)).unwrap();
    accept_prepared(&db, &lock, accept(sid(1), tid(1))).unwrap();
    let old = node_at(&db, 2);
    let mut beta_settings = SessionSettings::for_test(&config());
    beta_settings.profile.agent_name = agent("beta");
    db.update_settings(&lock, &project(1), beta_settings, now())
        .unwrap();
    let mut next = accept(sid(1), tid(2));
    next.config.agent_name = agent("beta");
    accept_prepared(&db, &lock, next).unwrap();
    assert_eq!(records(&db).configs.len(), 1);
    assert_eq!(node_at(&db, 2), old);
    assert_eq!(
        db.begin_read().unwrap().current_agent(sid(1)).unwrap(),
        Some(agent("beta"))
    );
    assert!(matches!(
        db.start_request(&lock, key(tid(1), 0, 0), origin(), now()),
        Err(StoreError::InvalidTransition { .. })
    ));
    let mut retry = accept(sid(1), tid(1));
    retry.config.agent_name = agent("ignored");
    assert!(matches!(
        accept_prepared(&db, &lock, retry).unwrap(),
        AcceptedTurn::Existing(t) if t.session_id == sid(1)
    ));
    let lock2 = db.try_lock_session(sid(2)).unwrap();
    for case in 0..3 {
        let mut conflicting = accept(sid(1), tid(1));
        match case {
            0 => conflicting.input = TurnInput::text("用户问题🌳!".into()).unwrap(),
            1 => conflicting.project_path = project(2),
            _ => conflicting.session_id = sid(2),
        }
        assert!(
            matches!(
                accept_prepared(&db, if case == 2 { &lock2 } else { &lock }, conflicting),
                Err(StoreError::Conflict { .. })
            ),
            "重放已有 TurnId 时输入、项目路径或会话 ID 不一致必须返回冲突（case={case}）"
        );
    }
    let mut wrong_project = accept(sid(1), tid(99));
    wrong_project.project_path = project(2);
    assert!(
        matches!(
            accept_prepared(&db, &lock, wrong_project),
            Err(StoreError::BindingMismatch { .. })
        ),
        "向已有会话写入新 Turn 时项目路径不一致必须被拒绝"
    );
    let current = db.begin_read().unwrap().session(sid(1)).unwrap().current;
    assert!(
        matches!(
            db.rewind(&lock, NodeId(0), &project(2), now()),
            Err(StoreError::BindingMismatch { .. })
        ),
        "rewind 的项目路径不一致必须在会话库写入时被拒绝"
    );
    assert_eq!(
        db.begin_read().unwrap().session(sid(1)).unwrap().current,
        current,
        "被拒绝的 rewind 不能移动当前位置"
    );
    db.rewind(&lock, NodeId(0), &project(1), now()).unwrap();
    let mut next = accept(sid(1), tid(3));
    next.config.system_prompt = Some("different".into());
    accept_prepared(&db, &lock, next).unwrap();
    assert_eq!(records(&db).configs.len(), 2);
    assert_eq!(node_at(&db, 2), old);
    assert_eq!(node_at(&db, 5).parent(), Some(NodeId(0)));
    assert_eq!(
        db.begin_read().unwrap().list(None, 10).unwrap().items[0].unclosed_turn,
        Some(tid(3))
    );
    assert_eq!(
        db.begin_read().unwrap().log(sid(1)).unwrap().unclosed_turn,
        Some(tid(3))
    );
    drop(lock2);
    drop(lock);
    drop(db);
    let db = SessionStore::open(&layout).unwrap();
    assert_eq!(node_at(&db, 2), old);
}

#[test]
fn invalid_acceptance_and_lock_capabilities_leave_no_facts() {
    let (_d, _l, db) = store();
    let lock = db.try_lock_session(sid(1)).unwrap();
    assert!(
        matches!(
            db.accept_turn(&lock, accept(sid(1), tid(1))),
            Err(StoreError::NotFound {
                kind: "session",
                ..
            })
        ),
        "不存在的会话调用 accept_turn 必须返回 NotFound"
    );
    assert!(records(&db).nodes.is_empty());

    let mut bad_settings = SessionSettings::for_test(&config());
    bad_settings.system_prompt = Some("x".repeat(2 * 1024 * 1024 + 1));
    assert!(
        matches!(
            db.create_session(&lock, project(1), bad_settings, now()),
            Err(StoreError::InvalidInput { .. })
        ),
        "设置无效时 create_session 必须拒绝且不落库"
    );
    assert!(records(&db).nodes.is_empty());

    let base_settings = SessionSettings::for_test(&config());
    let base_version = base_settings.version().unwrap();
    db.create_session(&lock, project(1), base_settings.clone(), now())
        .unwrap();
    assert!(matches!(
        db.create_session(&lock, project(1), base_settings, now()),
        Err(StoreError::SessionCollision { session_id }) if session_id == sid(1)
    ));
    let before = records(&db);

    for case in 1..8 {
        let mut a = accept(sid(1), tid(1));
        a.settings_version = base_version;
        let expected_detail = match case {
            1 => {
                a.config.system_prompt = Some("x".repeat(2 * 1024 * 1024 + 1));
                None
            }
            2 => {
                a.config
                    .catalog
                    .entries
                    .push(a.config.catalog.entries[0].clone());
                None
            }
            3 => {
                a.config.limits.max_model_steps = 33;
                Some("invalid frozen configuration")
            }
            4 => {
                a.config.limits.max_model_attempts = 0;
                Some("invalid frozen configuration")
            }
            5 => {
                a.config.catalog.digest = [0; 32];
                None
            }
            6 => {
                a.config.limits.max_model_attempts = 12;
                Some("invalid frozen configuration")
            }
            7 => {
                use sha2::{Digest, Sha256};
                let first = a.config.catalog.entries[0].clone();
                let mut z = first.clone();
                z.name = "z_tool".into();
                a.config.catalog.entries = vec![z, first];
                let canonical = codec::canonical(&a.config.catalog.entries).unwrap();
                let mut hasher = Sha256::new();
                hasher.update(b"lattice-tool-catalog-snapshot-v1\0");
                hasher.update((canonical.len() as u64).to_be_bytes());
                hasher.update(&canonical);
                a.config.catalog.digest = hasher.finalize().into();
                None
            }
            _ => unreachable!(),
        };
        let result = db.accept_turn(&lock, a);
        if let Some(expected_detail) = expected_detail {
            assert!(
                matches!(&result, Err(StoreError::InvalidInput { field: "config", detail }) if detail == expected_detail),
                "{case}: {result:?}"
            );
        } else {
            assert!(result.is_err(), "{case}");
        }
        assert!(before == records(&db), "{case} 修改了持久记录");
    }
    assert!(matches!(
        db.try_lock_session(sid(1)),
        Err(StoreError::Busy { .. })
    ));
    let other = db.try_lock_session(sid(2)).unwrap();
    assert!(matches!(
        db.accept_turn(&other, accept(sid(1), tid(1))),
        Err(StoreError::InvalidLockCapability)
    ));
    let (_d2, _l2, db2) = store();
    assert!(matches!(
        db2.create_session(
            &lock,
            project(1),
            SessionSettings::for_test(&config()),
            now()
        ),
        Err(StoreError::InvalidLockCapability)
    ));
    assert!(matches!(
        db2.update_settings(
            &lock,
            &project(1),
            SessionSettings::for_test(&config()),
            now()
        ),
        Err(StoreError::InvalidLockCapability)
    ));
    assert!(matches!(
        db2.accept_turn(&lock, accept(sid(1), tid(1))),
        Err(StoreError::InvalidLockCapability)
    ));
    let mut a = accept(sid(1), tid(1));
    a.input = TurnInput::text("x".repeat(lattice_conversation::MAX_USER_TEXT_BYTES)).unwrap();
    a.settings_version = base_version;
    db.accept_turn(&lock, a).unwrap();
    assert_eq!(
        db.begin_read().unwrap().session(sid(1)).unwrap().current,
        NodeId(2)
    );
}

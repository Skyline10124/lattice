// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn timeout(secs: u64) -> TurnTimeout {
    TurnTimeout::new(secs).unwrap()
}

#[test]
fn timeout_bounds_preserve_frozen_limits_and_session_settings_on_reopen() {
    for timeout_secs in [1, 300, 3600, 3601, MAX_TURN_TIMEOUT_SECS] {
        let (_dir, layout, db) = store();
        let lock = db.try_lock_session(sid(1)).unwrap();
        let mut first = accept(sid(1), tid(1));
        first.config.limits.timeout_secs = timeout(timeout_secs);
        let expected_first = first.config.clone();
        let expected_first_settings = SessionSettings::for_test(&first.config);
        first.settings_version = expected_first_settings.version().unwrap();
        db.create_session(&lock, project(1), expected_first_settings.clone(), now())
            .unwrap();
        db.accept_turn(&lock, first)
            .unwrap_or_else(|error| panic!("timeout_secs={timeout_secs}: {error}"));
        let read = db.begin_read().unwrap();
        assert_eq!(read.turn_config(sid(1), tid(1)).unwrap(), expected_first);
        assert_eq!(
            read.session_settings(sid(1)).unwrap(),
            expected_first_settings
        );
        drop(read);
        drop(lock);
        drop(db);

        let reopened = SessionStore::open(&layout)
            .unwrap_or_else(|error| panic!("timeout_secs={timeout_secs}: {error}"));
        let lock = reopened.try_lock_session(sid(1)).unwrap();
        let mut second = accept(sid(1), tid(2));
        second.config.agent_name = agent("beta");
        second.config.limits.timeout_secs = TurnTimeout::default();
        let expected_second = second.config.clone();
        let expected_second_settings = SessionSettings::for_test(&second.config);
        reopened
            .update_settings(
                &lock,
                &project(1),
                expected_second_settings.clone(),
                now() + chrono::Duration::seconds(1),
            )
            .unwrap();
        second.settings_version = expected_second_settings.version().unwrap();
        reopened.accept_turn(&lock, second).unwrap();
        drop(lock);
        drop(reopened);

        let reopened = SessionStore::open(&layout)
            .unwrap_or_else(|error| panic!("timeout_secs={timeout_secs}: {error}"));
        let read = reopened.begin_read().unwrap();
        assert_eq!(read.turn_config(sid(1), tid(1)).unwrap(), expected_first);
        assert_eq!(read.turn_config(sid(1), tid(2)).unwrap(), expected_second);
        assert_eq!(
            read.session_settings(sid(1)).unwrap(),
            expected_second_settings
        );
    }

    let (_dir, layout, db) = store();
    let lock = db.try_lock_session(sid(1)).unwrap();
    accept_prepared(&db, &lock, accept(sid(1), tid(1))).unwrap();
    let mut second = accept(sid(1), tid(2));
    second.config.agent_name = agent("beta");
    db.update_settings(
        &lock,
        &project(1),
        SessionSettings::for_test(&second.config),
        now() + chrono::Duration::seconds(1),
    )
    .unwrap();
    accept_prepared(&db, &lock, second).unwrap();
    let base = records(&db);
    drop(lock);
    drop(db);
    for invalid_timeout in [0, MAX_TURN_TIMEOUT_SECS + 1] {
        let raw = redb::Database::open(layout.lattice_root().join("lattice.redb")).unwrap();
        let tx = raw.begin_write().unwrap();
        overwrite_records(&tx, &base);
        {
            let mut nodes = tx.open_table(NODES).unwrap();
            let mut wire = serde_json::to_value(&base.nodes[&(sid(1), NodeId(1))]).unwrap();
            wire["limits"]["timeout_secs"] = json!(invalid_timeout);
            nodes
                .insert(
                    node_key(sid(1), NodeId(1)).as_slice(),
                    encode(&wire).unwrap().as_slice(),
                )
                .unwrap();
        }
        tx.commit().unwrap();
        drop(raw);
        assert!(
            matches!(
                SessionStore::open(&layout),
                Err(StoreError::Corruption { .. })
            ),
            "invalid_timeout={invalid_timeout}"
        );
    }
}

#[test]
fn draft_session_reopens_with_readable_projections_and_empty_configs() {
    let (_dir, layout, db) = store();
    let lock = db.try_lock_session(sid(1)).expect("获取会话执行锁");
    let mut draft_settings = SessionSettings::for_test(&config());
    draft_settings.system_prompt = Some("草稿内联系统提示".into());
    draft_settings.model = None;
    draft_settings.effort = None;
    let created = db
        .create_session(&lock, project(1), draft_settings.clone(), now())
        .expect("创建草稿会话");
    assert_eq!(created.session_id, sid(1), "草稿会话 ID 应一致");
    assert_eq!(created.current, NodeId(0), "草稿当前位置应为根节点 0");
    assert_eq!(created.created_at, now(), "created_at 应等于传入时间");
    assert_eq!(created.updated_at, now(), "updated_at 应等于 created_at");
    assert!(
        records(&db).configs.is_empty(),
        "草稿的系统提示内联保存，configs 表不能产生新条目"
    );
    drop(lock);
    drop(db);

    let reopened = SessionStore::open(&layout).expect("草稿会话应能通过严格打开");
    assert!(
        records(&reopened).configs.is_empty(),
        "重开后 configs 表仍应为空"
    );
    let read = reopened.begin_read().expect("开启只读事务");
    let head_record = read.session(sid(1)).expect("读取草稿 SessionRecord");
    assert_eq!(head_record, created, "重开后草稿会话头记录应保持一致");
    assert_eq!(
        read.path_node(sid(1), head_record.current)
            .expect("读取草稿当前路径根节点"),
        Node::Root,
        "草稿当前路径节点应为根节点"
    );
    assert_eq!(
        read.session_settings(sid(1)).expect("读取草稿会话设置"),
        draft_settings,
        "重开后草稿会话设置应保持一致"
    );
    let view = read.view(sid(1), 10).expect("读取草稿会话视图");
    assert_eq!(view.session, created);
    assert_eq!(view.current_agent, Some(agent("alpha")));
    assert!(view.entries.is_empty(), "草稿视图的对话条目应为空");

    let listed = read.list(None, 10).expect("读取草稿列表投影");
    assert_eq!(
        listed.items,
        vec![SessionListRow {
            session_id: sid(1),
            project_path: project(1),
            current: NodeId(0),
            node_count: 1,
            unclosed_turn: None,
            updated_at: now(),
        }],
        "列表应包含仅有 1 个根节点的草稿"
    );

    let log = read.log(sid(1)).expect("读取草稿日志投影");
    assert_eq!(log.session, created);
    assert_eq!(log.current_agent, Some(agent("alpha")));
    assert_eq!(log.unclosed_turn, None);
    assert_eq!(
        log.nodes,
        vec![LogRow {
            node: NodeHeader {
                id: NodeId(0),
                parent: None,
                turn_id: None,
                kind: NodeKind::Root,
            },
            current: true,
            turn_start: None,
        }],
        "日志投影应只包含当前根节点"
    );

    assert!(
        matches!(
            read.history(sid(1), NodeId(0), 10),
            Err(StoreError::InvalidInput {
                field: "before",
                ..
            })
        ),
        "草稿在根节点分页应按非用户消息边界返回 InvalidInput 而非损坏"
    );

    let usage = read.usage(sid(1)).expect("读取草稿用量投影");
    assert_eq!(usage.session_id, sid(1));
    assert_eq!(usage.position, NodeId(0));
    assert!(usage.turns.is_empty(), "草稿没有轮次用量");
    assert_eq!(usage.path_total.attempts, 0);
    assert_eq!(usage.session_total.attempts, 0);
}

#[test]
fn update_settings_preserves_frozen_turn_configs() {
    let (_dir, layout, db) = store();
    let lock = db.try_lock_session(sid(1)).expect("获取会话执行锁");
    let mut first_cfg = config();
    first_cfg.system_prompt = Some("第一轮冻结系统提示".into());
    first_cfg.effort = Some(lattice_conversation::Effort::Medium);
    let initial_settings = SessionSettings::for_test(&first_cfg);
    db.create_session(&lock, project(1), initial_settings.clone(), now())
        .expect("创建会话");
    let mut first_turn = accept(sid(1), tid(1));
    first_turn.config = first_cfg.clone();
    first_turn.settings_version = initial_settings.version().expect("计算初始设置版本");
    db.accept_turn(&lock, first_turn).expect("接受第一轮");

    let later = now() + chrono::Duration::seconds(30);
    let mut updated_settings = SessionSettings::for_test(&first_cfg);
    updated_settings.profile.agent_name = agent("beta");
    updated_settings.profile.description = Some("新 agent 描述".into());
    updated_settings.profile.timeout_secs = timeout(900);
    updated_settings.system_prompt = Some("修改后的下一轮系统提示".into());
    updated_settings.model = None;
    updated_settings.effort = None;

    assert!(
        matches!(
            db.update_settings(&lock, &project(2), updated_settings.clone(), later),
            Err(StoreError::BindingMismatch { session_id }) if session_id == sid(1)
        ),
        "项目绑定不一致时 update_settings 必须返回 BindingMismatch"
    );

    let updated_record = db
        .update_settings(&lock, &project(1), updated_settings.clone(), later)
        .expect("更新会话设置");
    assert_eq!(updated_record.created_at, now(), "created_at 必须保持不变");
    assert_eq!(
        updated_record.updated_at, later,
        "update_settings 必须刷新 updated_at"
    );
    assert_eq!(
        records(&db).configs.len(),
        1,
        "update_settings 不能向 configs 表追加配置条目"
    );
    drop(lock);
    drop(db);

    let reopened = SessionStore::open(&layout).expect("修改设置与最后一轮不同时仍可通过严格打开");
    let read = reopened.begin_read().expect("开启只读事务");
    assert_eq!(
        read.session_settings(sid(1)).expect("读取更新后的设置"),
        updated_settings,
        "重开后会话设置应为更新后的值"
    );
    assert_eq!(
        read.current_agent(sid(1)).expect("读取当前 agent"),
        Some(agent("beta")),
        "当前 agent 应取自更新后的会话设置"
    );
    assert_eq!(
        read.turn_config(sid(1), tid(1))
            .expect("读取第一轮冻结配置"),
        first_cfg,
        "已有轮次的冻结配置不能受 update_settings 影响"
    );
    assert_eq!(
        read.turn_detail(sid(1), tid(1))
            .expect("读取第一轮详情")
            .agent_name,
        agent("alpha"),
        "已有轮次节点上的 agent_name 必须保持第一轮冻结值"
    );
}

#[test]
fn accept_turn_rejects_mismatched_settings_version_without_writing_nodes() {
    let (_dir, layout, db) = store();
    let lock = db.try_lock_session(sid(1)).expect("获取会话执行锁");
    let initial_settings = SessionSettings::for_test(&config());
    db.create_session(&lock, project(1), initial_settings.clone(), now())
        .expect("创建草稿会话");

    let mut other_settings = initial_settings.clone();
    other_settings.effort = Some(lattice_conversation::Effort::High);
    let wrong_version = other_settings.version().expect("计算不同设置版本");

    let before_draft = records(&db);
    let mut stale_on_draft = accept(sid(1), tid(1));
    stale_on_draft.settings_version = wrong_version;
    let draft_err = db
        .accept_turn(&lock, stale_on_draft)
        .expect_err("草稿会话上版本不符应拒绝接受");
    assert!(
        matches!(draft_err, StoreError::SettingsChanged { session_id } if session_id == sid(1)),
        "预期返回 SettingsChanged，实际为 {draft_err:?}"
    );
    assert!(
        before_draft == records(&db),
        "草稿上 SettingsChanged 不能写入任何节点或配置"
    );

    let mut valid_first = accept(sid(1), tid(1));
    valid_first.settings_version = initial_settings.version().expect("计算初始版本");
    db.accept_turn(&lock, valid_first.clone())
        .expect("版本匹配时应正常接受第一轮");

    let later = now() + chrono::Duration::seconds(5);
    db.update_settings(&lock, &project(1), other_settings.clone(), later)
        .expect("更新会话设置");
    let before_second = records(&db);

    // 同一 TurnId 重放优先于设置版本核对，仍返回 Existing。
    assert!(
        matches!(
            db.accept_turn(&lock, valid_first),
            Ok(AcceptedTurn::Existing(detail)) if detail.turn_id == tid(1)
        ),
        "已接受 TurnId 重放在设置更新后仍应返回 Existing"
    );
    assert!(before_second == records(&db), "重放不能修改持久记录");

    let mut stale_second = accept(sid(1), tid(2));
    stale_second.settings_version = initial_settings.version().expect("计算旧版本");
    let second_err = db
        .accept_turn(&lock, stale_second)
        .expect_err("设置更新后携带旧版本应返回 SettingsChanged");
    assert!(
        matches!(second_err, StoreError::SettingsChanged { session_id } if session_id == sid(1)),
        "预期返回 SettingsChanged，实际为 {second_err:?}"
    );
    assert!(
        before_second == records(&db),
        "SettingsChanged 不能写入任何新节点或索引"
    );

    drop(lock);
    drop(db);
    assert!(
        before_second == records(&SessionStore::open(&layout).expect("重开会话库")),
        "重开后记录与拒绝前完全一致"
    );
}

#[test]
fn session_settings_strict_open_checks_shape_draft_and_bounds() {
    // 只有根节点的会话应当通过严格打开。
    {
        let (_dir, layout, db) = store();
        let lock = db.try_lock_session(sid(1)).expect("获取会话执行锁");
        let mut draft = SessionSettings::for_test(&config());
        draft.model = None;
        draft.effort = None;
        db.create_session(&lock, project(1), draft.clone(), now())
            .expect("创建只有根节点的会话");
        let expected = records(&db);
        drop(lock);
        drop(db);
        let reopened = SessionStore::open(&layout).expect("只有根节点的会话必须通过严格打开");
        assert!(expected == records(&reopened));
    }

    let (_dir, layout, db) = store();
    let lock = db.try_lock_session(sid(1)).expect("获取会话执行锁");
    accept_prepared(&db, &lock, accept(sid(1), tid(1))).expect("接受基准轮次");
    let base = records(&db);
    drop(lock);
    drop(db);

    for case in ["missing", "orphan", "oversized_prompt"] {
        let raw = redb::Database::open(layout.lattice_root().join("lattice.redb")).unwrap();
        let tx = raw.begin_write().unwrap();
        let mut changed = base.clone();
        match case {
            "missing" => {
                changed.session_settings.remove(&sid(1));
            }
            "orphan" => {
                changed
                    .session_settings
                    .insert(sid(2), base.session_settings[&sid(1)].clone());
            }
            "oversized_prompt" => {
                changed
                    .session_settings
                    .get_mut(&sid(1))
                    .unwrap()
                    .system_prompt = Some("a".repeat(2 * 1024 * 1024 + 1));
            }
            _ => unreachable!(),
        }
        overwrite_records(&tx, &changed);
        tx.commit().unwrap();
        drop(raw);
        assert!(
            matches!(
                SessionStore::open(&layout),
                Err(StoreError::Corruption { .. })
            ),
            "严格打开必须拒绝损坏或自身无效的设置（case={case}）"
        );
    }

    for case in [
        "unknown",
        "legacy_profile_field",
        "effort",
        "agent_name",
        "default_model",
        "timeout_secs",
        "retry",
        "model",
        "version",
        "key",
        "canonical",
        "truncated",
        "table",
    ] {
        let raw = redb::Database::open(layout.lattice_root().join("lattice.redb")).unwrap();
        let tx = raw.begin_write().unwrap();
        overwrite_records(&tx, &base);
        if case == "table" {
            tx.delete_table(SESSION_SETTINGS).unwrap();
        } else {
            let mut table = tx.open_table(SESSION_SETTINGS).unwrap();
            let mut value = serde_json::to_value(&base.session_settings[&sid(1)]).unwrap();
            match case {
                "unknown" => value["unknown"] = json!(true),
                "legacy_profile_field" => value["profile"]["enabled_tools"] = json!(["observe"]),
                "effort" => value["effort"] = json!("default"),
                "agent_name" => value["profile"]["agent_name"] = json!("../bad"),
                "default_model" => value["profile"]["default_model"] = json!("missing-slash"),
                "timeout_secs" => value["profile"]["timeout_secs"] = json!(0),
                "retry" => value["profile"]["retry"]["max_retries"] = json!(11),
                "model" => value["model"] = json!("Bad_id/gpt-5"),
                _ => {}
            }
            let mut bytes = encode(&value).unwrap();
            if case == "version" {
                bytes[..4].copy_from_slice(&7_u32.to_be_bytes());
            }
            if case == "canonical" {
                bytes.push(b' ');
            }
            if case == "truncated" {
                bytes.truncate(3);
            }
            table
                .insert(sid(1).to_string().as_str(), bytes.as_slice())
                .unwrap();
            if case == "key" {
                table
                    .insert("invalid", encode(&value).unwrap().as_slice())
                    .unwrap();
            }
        }
        tx.commit().unwrap();
        drop(raw);
        let reopened = SessionStore::open(&layout).map(|_| ());
        if case == "version" {
            assert!(
                matches!(
                    reopened,
                    Err(StoreError::UnsupportedVersion {
                        found: 7,
                        supported: VERSION,
                        ..
                    })
                ),
                "{case}: {reopened:?}"
            );
        } else {
            assert!(
                matches!(reopened, Err(StoreError::Corruption { .. })),
                "{case}: {reopened:?}"
            );
        }
    }
}

#[test]
fn delete_draft_checks_strict_cutoff_and_passes_reopen_validation() {
    let (_dir, layout, db) = store();
    let cutoff = now() + chrono::Duration::hours(24);
    let settings = SessionSettings::for_test(&config());

    let lock_expired = db.try_lock_session(sid(1)).expect("锁定过期草稿");
    db.create_session(
        &lock_expired,
        project(1),
        settings.clone(),
        cutoff - chrono::Duration::seconds(1),
    )
    .expect("创建严格早于截止时间的草稿");

    let lock_equal = db.try_lock_session(sid(2)).expect("锁定等于截止时间的草稿");
    db.create_session(&lock_equal, project(1), settings.clone(), cutoff)
        .expect("创建等于截止时间的草稿");

    let lock_newer = db.try_lock_session(sid(3)).expect("锁定晚于截止时间的草稿");
    db.create_session(
        &lock_newer,
        project(1),
        settings,
        cutoff + chrono::Duration::seconds(1),
    )
    .expect("创建晚于截止时间的草稿");

    let lock_missing = db.try_lock_session(sid(4)).expect("锁定不存在的会话");

    assert_eq!(
        db.begin_read()
            .expect("开启只读事务")
            .expired_drafts(cutoff)
            .expect("扫描过期草稿"),
        vec![sid(1)],
        "只读扫描应仅返回 updated_at 严格早于截止时间的草稿"
    );

    assert!(
        !db.delete_draft(&lock_missing, cutoff)
            .expect("删除不存在的草稿不应报错"),
        "不存在的会话调用 delete_draft 应返回 false"
    );
    assert!(
        db.delete_draft(&lock_expired, cutoff)
            .expect("删除严格早于截止时间的草稿"),
        "updated_at 严格早于截止时间的草稿应被删除"
    );
    assert!(
        !db.delete_draft(&lock_equal, cutoff)
            .expect("检查等于截止时间的草稿"),
        "updated_at 等于截止时间的草稿应保留"
    );
    assert!(
        !db.delete_draft(&lock_newer, cutoff)
            .expect("检查晚于截止时间的草稿"),
        "updated_at 晚于截止时间的草稿应保留"
    );

    let lock_file = layout.session_lock(&format!("{}.lock", sid(1)));
    drop(lock_expired);
    drop(lock_equal);
    drop(lock_newer);
    drop(lock_missing);
    drop(db);

    assert!(lock_file.exists(), "删除草稿不应删除会话锁文件");

    let reopened = SessionStore::open(&layout).expect("删除草稿后重新打开会话库应通过严格校验");
    let read = reopened.begin_read().expect("开启只读事务");
    assert!(
        read.find_session(sid(1)).expect("查询已删草稿").is_none(),
        "已删草稿不应再有 Session 记录"
    );
    assert!(
        matches!(
            read.session_settings(sid(1)),
            Err(StoreError::NotFound { .. })
        ),
        "已删草稿不应再有会话设置记录"
    );
    let listed: Vec<SessionId> = read
        .list(None, 10)
        .expect("列出会话")
        .items
        .into_iter()
        .map(|row| row.session_id)
        .collect();
    assert_eq!(
        listed,
        vec![sid(2), sid(3)],
        "会话列表中应不再包含已删草稿且保留等于或晚于截止时间的草稿"
    );
}

#[test]
fn delete_draft_keeps_sessions_with_turns_even_after_rewind_to_root() {
    let (_dir, layout, db) = store();
    let cutoff = now() + chrono::Duration::hours(24);
    let lock = db.try_lock_session(sid(1)).expect("锁定会话");
    accept_prepared(&db, &lock, accept(sid(1), tid(1))).expect("接受首轮");

    assert!(
        db.begin_read()
            .expect("开启只读事务")
            .expired_drafts(cutoff)
            .expect("扫描过期草稿")
            .is_empty(),
        "有轮次的会话不应出现在过期草稿扫描结果中"
    );
    assert!(
        !db.delete_draft(&lock, cutoff)
            .expect("有轮次的会话调用 delete_draft"),
        "有轮次的会话不应被作为草稿删除"
    );

    db.rewind(
        &lock,
        NodeId(0),
        &project(1),
        now() + chrono::Duration::seconds(1),
    )
    .expect("回退到根节点");
    assert_eq!(
        db.begin_read()
            .expect("开启只读事务")
            .session(sid(1))
            .expect("读取回退后的会话")
            .current,
        NodeId(0),
        "回退后当前位置应在根节点"
    );
    assert!(
        db.begin_read()
            .expect("开启只读事务")
            .expired_drafts(cutoff)
            .expect("回退到根节点后扫描过期草稿")
            .is_empty(),
        "回退到根节点后整棵树仍有轮次节点，不应被扫描为草稿"
    );
    assert!(
        !db.delete_draft(&lock, cutoff)
            .expect("回退到根节点后调用 delete_draft"),
        "rewind 0 后当前位置在根节点的会话仍不应被删除"
    );

    drop(lock);
    drop(db);
    let reopened = SessionStore::open(&layout).expect("重开会话库应通过严格校验");
    assert_eq!(
        reopened
            .begin_read()
            .expect("开启只读事务")
            .log(sid(1))
            .expect("读取会话日志")
            .nodes
            .len(),
        3,
        "回退到根节点的会话整棵树节点应完整保留"
    );
}

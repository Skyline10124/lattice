// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn creation_and_legacy() {
    let dir = tempfile::tempdir().unwrap();
    let layout = StateLayout::in_state_root(dir.path());
    assert!(matches!(
        SessionStore::open(&layout),
        Err(StoreError::NotInitialized { .. })
    ));
    assert!(!layout.lattice_root().exists());
    let legacy_proj = layout.projects_dir().join("p9");
    fs::create_dir_all(&legacy_proj).unwrap();
    let old = layout.lattice_root().join("sessions.git");
    fs::write(&old, b"old").unwrap();
    let inv = legacy_proj.join("invocations.git");
    symlink("missing", &inv).unwrap();
    for create in [false, true] {
        let result = if create {
            SessionStore::create(&layout)
        } else {
            SessionStore::open(&layout)
        };
        match result {
            Err(StoreError::LegacyStores { paths }) => {
                assert_eq!(paths.len(), 2);
                assert!(paths.contains(&old));
                assert!(paths.contains(&inv));
            }
            _ => panic!("expected legacy refusal"),
        }
        assert!(!layout.lattice_root().join("lattice.redb").exists());
        assert_eq!(fs::read(&old).unwrap(), b"old");
    }
    fs::remove_file(&old).unwrap();
    fs::remove_file(&inv).unwrap();
    for kind in 0..4 {
        let path = layout.lattice_root().join("lattice.redb");
        match kind {
            0 => fs::write(&path, []).unwrap(),
            1 => fs::write(&path, b"random private payload").unwrap(),
            2 => fs::create_dir(&path).unwrap(),
            _ => symlink("missing", &path).unwrap(),
        };
        assert!(SessionStore::open(&layout).is_err());
        assert!(SessionStore::create(&layout).is_err());
        if kind == 2 {
            fs::remove_dir(path).unwrap();
        } else {
            fs::remove_file(path).unwrap();
        }
    }
    fs::remove_dir_all(layout.projects_dir()).unwrap();
    fs::write(layout.projects_dir(), b"not a directory").unwrap();
    assert!(matches!(
        SessionStore::open(&layout),
        Err(StoreError::Io { .. }) | Err(StoreError::Corruption { .. })
    ));
    fs::remove_file(layout.projects_dir()).unwrap();
    let db = SessionStore::create(&layout).unwrap();
    assert!(
        db.begin_read()
            .unwrap()
            .list(None, 10)
            .unwrap()
            .items
            .is_empty()
    );
}

#[test]
fn creation_failures() {
    for point in [
        FaultPoint::Probe,
        FaultPoint::CreateCommit,
        FaultPoint::BeforeRename,
        FaultPoint::AfterRename,
        FaultPoint::Cleanup,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let layout = StateLayout::in_state_root(dir.path());
        fs::create_dir_all(layout.lattice_root()).unwrap();
        if point == FaultPoint::Cleanup {
            fs::write(
                layout
                    .lattice_root()
                    .join(".lattice.redb-create-00000000-0000-4000-8000-000000000001"),
                b"stale",
            )
            .unwrap();
        }
        let result = with_fault(point, || SessionStore::create(&layout));
        assert!(result.is_err(), "{point:?}");
        assert_eq!(
            layout.lattice_root().join("lattice.redb").exists(),
            point == FaultPoint::AfterRename
        );
        let db = SessionStore::create(&layout).unwrap();
        let lock = db.try_lock_session(sid(1)).unwrap();
        accept_prepared(&db, &lock, accept(sid(1), tid(1))).unwrap();
        drop(lock);
        drop(db);
        let db = SessionStore::create(&layout).unwrap();
        assert!(
            db.begin_read()
                .unwrap()
                .turn_node(tid(1))
                .unwrap()
                .is_some()
        );
    }
    let dir = tempfile::tempdir().unwrap();
    let layout = StateLayout::in_state_root(dir.path());
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..2)
            .map(|_| {
                scope.spawn(|| match SessionStore::create(&layout) {
                    Ok(db) => {
                        drop(db);
                        true
                    }
                    Err(StoreError::Database(redb::DatabaseError::DatabaseAlreadyOpen)) => false,
                    Err(e) => panic!("{e:?}"),
                })
            })
            .collect();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert!(results.contains(&true));
    });
    assert!(SessionStore::open(&layout).is_ok());
}

#[test]
fn schema_and_legacy_rejection_matrix() {
    for old_kind in 0..3 {
        for location in 0..3 {
            let dir = tempfile::tempdir().unwrap();
            let layout = StateLayout::in_state_root(dir.path());
            fs::create_dir_all(layout.projects_dir().join("p9")).unwrap();
            let path = if location == 0 {
                layout.lattice_root().join("sessions.git")
            } else {
                layout
                    .projects_dir()
                    .join(if location == 1 { "p1" } else { "p9" })
                    .join("invocations.git")
            };
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            match old_kind {
                0 => fs::create_dir(&path).unwrap(),
                1 => fs::write(&path, b"old").unwrap(),
                _ => symlink("missing", &path).unwrap(),
            };
            for result in [SessionStore::open(&layout), SessionStore::create(&layout)] {
                assert!(
                    matches!(result, Err(StoreError::LegacyStores { paths }) if paths == vec![path.clone()])
                );
            }
            assert!(!layout.lattice_root().join("lattice.redb").exists());
        }
    }
    for case in 0..12 {
        let (_d, layout, db) = store();
        drop(db);
        let raw = redb::Database::open(layout.lattice_root().join("lattice.redb")).unwrap();
        let tx = raw.begin_write().unwrap();
        match case {
            0 => {
                tx.open_table(META)
                    .unwrap()
                    .insert(
                        "store_kind",
                        encode(&String::from("wrong")).unwrap().as_slice(),
                    )
                    .unwrap();
            }
            1 => {
                tx.open_table(META)
                    .unwrap()
                    .insert(
                        "store_format_version",
                        encode(&(VERSION + 1)).unwrap().as_slice(),
                    )
                    .unwrap();
            }
            2 => {
                tx.delete_table(SESSIONS).unwrap();
                tx.open_table(redb::TableDefinition::<u64, &[u8]>::new("sessions"))
                    .unwrap();
            }
            3 => {
                tx.delete_table(CONFIGS).unwrap();
            }
            4 | 5 | 7 | 8 | 9 | 10 | 11 => {
                let mut meta = tx.open_table(META).unwrap();
                // 固定旧版元信息，独立于当前编码器核对严格打开。
                let old_version = match case {
                    4 => 3_u32,
                    5 => 4_u32,
                    7 => 6_u32,
                    8 => 7_u32,
                    9 => 8_u32,
                    10 => 9_u32,
                    _ => 10_u32,
                };
                for (key, value) in [
                    ("store_kind", "\"lattice-session\"".to_owned()),
                    ("store_format_version", old_version.to_string()),
                ] {
                    let mut bytes = old_version.to_be_bytes().to_vec();
                    bytes.extend(value.as_bytes());
                    meta.insert(key, bytes.as_slice()).unwrap();
                }
            }
            _ => {
                tx.open_table(META)
                    .unwrap()
                    .insert("store_kind", &[0u8, 0, 0, 99, b'"', b'x', b'"'][..])
                    .unwrap();
            }
        }
        tx.commit().unwrap();
        drop(raw);
        let error = SessionStore::open(&layout)
            .err()
            .unwrap_or_else(|| panic!("case {case}"));
        if matches!(case, 4 | 5 | 7 | 8 | 9 | 10 | 11) {
            let expected = match case {
                4 => 3,
                5 => 4,
                7 => 6,
                8 => 7,
                9 => 8,
                10 => 9,
                _ => 10,
            };
            let path = layout.lattice_root().join("lattice.redb");
            assert!(error.to_string().contains(&path.display().to_string()));
            assert!(matches!(
                error,
                StoreError::UnsupportedVersion { path: found_path, found, supported: VERSION, .. }
                    if found_path == path && found == expected
            ));
        }
    }
    let (_d, layout, db) = store();
    assert!(matches!(
        SessionStore::open(&layout),
        Err(StoreError::Database(
            redb::DatabaseError::DatabaseAlreadyOpen
        ))
    ));
    drop(db);
}

#[test]
fn value_versions_and_schema_rejections_name_the_database() {
    for table in [META, SESSIONS, SESSION_SETTINGS, CONFIGS, TURN_INDEX, INDEX] {
        let (_d, layout, db) = store();
        let (lock, _calls) = running(&db, sid(1), tid(1), 1, 1);
        drop(lock);
        drop(db);
        let raw = redb::Database::open(layout.lattice_root().join("lattice.redb")).unwrap();
        let tx = raw.begin_write().unwrap();
        {
            let mut t = tx.open_table(table).unwrap();
            let (key, mut bytes) = {
                let (key, value) = t.first().unwrap().unwrap();
                (key.value().to_string(), value.value().to_vec())
            };
            bytes[..4].copy_from_slice(&1u32.to_be_bytes());
            t.insert(key.as_str(), bytes.as_slice()).unwrap();
        }
        tx.commit().unwrap();
        drop(raw);
        assert!(
            matches!(SessionStore::open(&layout),Err(StoreError::UnsupportedVersion{path,found:1,supported:VERSION,..}) if path==layout.lattice_root().join("lattice.redb"))
        );
    }
    {
        let (_d, layout, db) = store();
        let (lock, _calls) = running(&db, sid(1), tid(1), 1, 1);
        drop(lock);
        drop(db);
        let raw = redb::Database::open(layout.lattice_root().join("lattice.redb")).unwrap();
        let tx = raw.begin_write().unwrap();
        {
            let mut t = tx.open_table(NODES).unwrap();
            let bytes = 1u32.to_be_bytes();
            t.insert(node_key(sid(1), NodeId(0)).as_slice(), bytes.as_slice())
                .unwrap();
        }
        tx.commit().unwrap();
        drop(raw);
        assert!(
            matches!(SessionStore::open(&layout),Err(StoreError::UnsupportedVersion{path,found:1,..}) if path==layout.lattice_root().join("lattice.redb"))
        );
    }
    let (_d, layout, db) = store();
    drop(db);
    corrupt_json(&layout, META, "store_kind", |v| *v = json!("wrong"));
    assert!(matches!(
        SessionStore::open(&layout),
        Err(StoreError::Corruption { .. })
    ));
}

#[test]
fn non_utf8_project_path_persists_across_reopen() {
    let (_dir, layout, db) = store();
    let non_utf8 = ProjectPath::from_bytes(b"/tmp/proj-\xff\xfe/sub".as_slice())
        .expect("构造非 UTF-8 项目路径");
    let lock = db.try_lock_session(sid(1)).expect("获取会话锁");
    let mut input = accept(sid(1), tid(1));
    input.project_path = non_utf8.clone();
    accept_prepared(&db, &lock, input).expect("写入非 UTF-8 项目路径会话");
    drop(lock);
    drop(db);

    let reopened = SessionStore::open(&layout).expect("重新打开会话库");
    let read = reopened.begin_read().expect("开启只读事务");
    let session = read.session(sid(1)).expect("读取会话记录");
    assert_eq!(
        session.project_path, non_utf8,
        "重开后读取 SessionRecord 的非 UTF-8 项目路径应保持不变"
    );
    let listed = read.list(None, 10).expect("列出会话");
    assert_eq!(
        listed.items[0].project_path, non_utf8,
        "重开后会话列表投影中的非 UTF-8 项目路径应保持不变"
    );
    let log = read.log(sid(1)).expect("读取会话日志");
    assert_eq!(
        log.session.project_path, non_utf8,
        "重开后会话日志中的非 UTF-8 项目路径应保持不变"
    );
}

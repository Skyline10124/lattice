// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn assistant_replies_with_images_are_rejected_without_mutating_records() {
    use lattice_conversation::{ImageMediaType, ImageRef};

    let (_dir, _layout, db) = store();
    let lock = db.try_lock_session(sid(1)).unwrap();
    accept_prepared(&db, &lock, accept(sid(1), tid(1))).unwrap();
    db.start_request(&lock, key(tid(1), 0, 0), origin(), now())
        .unwrap();

    let image_part = MessagePart::semantic_only(MessageSemantic::Image {
        image: ImageRef {
            sha256: [0x22; 32],
            media_type: ImageMediaType::Png,
            byte_length: 1024,
        },
    });
    let before = records(&db);

    // 1. Finish 路径含图片被拒绝
    let mut finish_reply = reply(key(tid(1), 0, 0), 0);
    finish_reply.message.parts.push(image_part.clone());
    let finish_err = db
        .close_step(&lock, CloseStep::Finish(finish_reply), now())
        .expect_err("Finish 回复包含图片时必须拒绝");
    assert!(
        matches!(
            &finish_err,
            StoreError::InvalidInput { field: "reply", detail } if detail.contains("助手消息不能包含图片")
        ),
        "预期返回中文助手图片拒绝错误，实际为：{finish_err:?}"
    );
    assert!(before == records(&db), "Finish 拒绝图片后不得修改持久记录");

    // 2. register_calls 路径含图片被拒绝
    let mut call_reply = reply(key(tid(1), 0, 0), 1);
    call_reply.message.parts.push(image_part);
    let register_err = db
        .register_calls(&lock, call_reply, &[iid(1)], now())
        .expect_err("工具批次回复包含图片时必须拒绝");
    assert!(
        matches!(
            &register_err,
            StoreError::InvalidInput { field: "reply", detail } if detail.contains("助手消息不能包含图片")
        ),
        "预期返回中文助手图片拒绝错误，实际为：{register_err:?}"
    );
    assert!(
        before == records(&db),
        "register_calls 拒绝图片后不得修改持久记录"
    );
}

#[test]
fn opening_v11_store_with_dangling_user_image_ref_reports_corruption() {
    let (_dir, layout, db) = store();
    let lock = db.try_lock_session(sid(1)).unwrap();
    accept_prepared(&db, &lock, accept(sid(1), tid(1))).unwrap();

    db.edit_test_node(
        NodeRef {
            session_id: sid(1),
            node: NodeId(1),
        },
        |wire| {
            wire["input"]["message"]["parts"]
                .as_array_mut()
                .unwrap()
                .push(json!({
                    "semantic": {
                        "image": {
                            "image": {
                                "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                                "media_type": "png",
                                "byte_length": 256
                            }
                        }
                    },
                    "replay_state": null
                }));
        },
    )
    .expect("测试注入含悬空图片引用的 Input 节点应成功提交");

    drop(lock);
    drop(db);

    let err = match SessionStore::open(&layout) {
        Ok(_) => panic!("含悬空图片引用的格式 11 会话库打开时必须报损坏"),
        Err(err) => err,
    };
    assert!(
        matches!(
            &err,
            StoreError::Corruption { table, key, .. }
                if table == "nodes" && key == &format!("{}/1", sid(1))
        ),
        "打开含悬空图片引用 Input 节点的错误与预期不符：{err:?}"
    );
}

fn png_bytes(tag: &[u8]) -> Vec<u8> {
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    out.extend_from_slice(tag);
    out
}

fn jpeg_bytes(tag: &[u8]) -> Vec<u8> {
    let mut out = b"\xff\xd8\xff\xe0".to_vec();
    out.extend_from_slice(tag);
    out
}

fn blob_count(layout: &StateLayout) -> usize {
    use redb::{ReadableDatabase, ReadableTableMetadata};
    let raw = redb::Database::open(layout.lattice_root().join("lattice.redb")).unwrap();
    let tx = raw.begin_read().unwrap();
    let table = tx.open_table(BLOBS).unwrap();
    table.len().unwrap() as usize
}

#[test]
fn turn_input_validates_limits_deduplicates_blobs_preserves_order_and_redacts_debug() {
    use lattice_conversation::{
        ImageMediaType, ImageRef, MAX_IMAGE_BYTES, MAX_USER_IMAGE_TOTAL_BYTES,
        MAX_USER_IMAGES_PER_TURN, UserInputError,
    };

    assert_eq!(TurnInput::new(vec![]).unwrap_err(), UserInputError::Empty);
    assert_eq!(
        TurnInput::new(vec![TurnInputPart::Text { text: "".into() }]).unwrap_err(),
        UserInputError::Empty
    );
    assert_eq!(
        TurnInput::text(String::new()).unwrap_err(),
        UserInputError::Empty
    );

    // 长度与魔数错误均带从 1 开始的图片序号。
    let img_ok = png_bytes(b"first-valid");
    assert_eq!(
        TurnInput::new(vec![
            TurnInputPart::Text {
                text: "前缀".into()
            },
            TurnInputPart::Image {
                bytes: img_ok.clone()
            },
            TurnInputPart::Image { bytes: vec![] },
        ])
        .unwrap_err(),
        UserInputError::InvalidImageLength {
            image: 2,
            actual: 0
        }
    );
    let mut oversized = vec![0x22; MAX_IMAGE_BYTES + 1];
    oversized[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
    assert_eq!(
        TurnInput::new(vec![
            TurnInputPart::Image {
                bytes: img_ok.clone()
            },
            TurnInputPart::Image { bytes: oversized },
        ])
        .unwrap_err(),
        UserInputError::InvalidImageLength {
            image: 2,
            actual: (MAX_IMAGE_BYTES + 1) as u64
        }
    );
    assert_eq!(
        TurnInput::new(vec![
            TurnInputPart::Image {
                bytes: img_ok.clone()
            },
            TurnInputPart::Image {
                bytes: b"<svg>bad</svg>".to_vec()
            },
        ])
        .unwrap_err(),
        UserInputError::UnsupportedImageFormat { image: 2 }
    );

    // 超过 8 张图片拒绝（即使全部相同）。
    let nine_same = (0..=MAX_USER_IMAGES_PER_TURN)
        .map(|_| TurnInputPart::Image {
            bytes: img_ok.clone(),
        })
        .collect();
    assert_eq!(
        TurnInput::new(nine_same).unwrap_err(),
        UserInputError::TooManyImages {
            actual: MAX_USER_IMAGES_PER_TURN + 1
        }
    );

    // 总量超过 10 MiB 拒绝。
    let mut chunk_a = vec![0x11; 3_932_160];
    chunk_a[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
    let mut chunk_b = vec![0x22; 3_932_160];
    chunk_b[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
    let mut chunk_c = vec![0x33; 2_621_441];
    chunk_c[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
    assert_eq!(
        TurnInput::new(vec![
            TurnInputPart::Image { bytes: chunk_a },
            TurnInputPart::Image { bytes: chunk_b },
            TurnInputPart::Image { bytes: chunk_c },
        ])
        .unwrap_err(),
        UserInputError::ImageTotalTooLarge {
            actual: (MAX_USER_IMAGE_TOTAL_BYTES + 1) as u64
        }
    );

    // 重复图片保留每次出现并只存一份字节，相邻非空文本合并、空文本剔除。
    let secret_bytes = png_bytes(b"SENSITIVE_RAW_PIXEL_MARKER");
    let secret_array = format!("{secret_bytes:?}");
    let expected_ref = ImageRef::from_bytes(&secret_bytes).unwrap();
    let input = TurnInput::new(vec![
        TurnInputPart::Text { text: "".into() },
        TurnInputPart::Text {
            text: "第一段".into(),
        },
        TurnInputPart::Text {
            text: "合并".into(),
        },
        TurnInputPart::Image {
            bytes: secret_bytes.clone(),
        },
        TurnInputPart::Text { text: "".into() },
        TurnInputPart::Image {
            bytes: secret_bytes,
        },
        TurnInputPart::Text {
            text: "尾段".into(),
        },
    ])
    .expect("合法图文输入应构造成功");

    assert_eq!(input.message().role, MessageRole::User);
    assert_eq!(
        input.message().parts,
        vec![
            MessagePart::semantic_only(MessageSemantic::Text {
                text: "第一段合并".into()
            }),
            MessagePart::semantic_only(MessageSemantic::Image {
                image: expected_ref
            }),
            MessagePart::semantic_only(MessageSemantic::Image {
                image: expected_ref
            }),
            MessagePart::semantic_only(MessageSemantic::Text {
                text: "尾段".into()
            }),
        ]
    );
    let debug = format!("{input:?}");
    assert!(
        !debug.contains("SENSITIVE_RAW_PIXEL_MARKER"),
        "Debug 输出不得包含图片原始字节内容：{debug}"
    );
    // 派生的 Debug 会把字节映射按十进制数组输出，标记文本检查不出来，所以另外核对数组形式。
    assert!(
        !debug.contains(&secret_array),
        "Debug 输出不得包含图片原始字节数组：{debug}"
    );
    assert!(debug.contains("TurnInput"));
    assert_eq!(expected_ref.media_type, ImageMediaType::Png);

    let (_message, blobs) = input.into_parts();
    assert_eq!(blobs.len(), 1, "重复图片在内部字节映射中只保留一份");
}

#[test]
fn blobs_deduplicate_persist_idempotency_atomicity_and_with_blob() {
    let img1 = png_bytes(b"image-one");
    let img2 = jpeg_bytes(b"image-two");
    let ref1 = lattice_conversation::ImageRef::from_bytes(&img1).unwrap();
    let ref2 = lattice_conversation::ImageRef::from_bytes(&img2).unwrap();

    let (_dir, layout, db) = store();
    let lock1 = db.try_lock_session(sid(1)).unwrap();

    // 1. 原子性：因设置版本变化被拒时不写图片。
    let mut rejected = accept(sid(1), tid(1));
    rejected.inputs = vec![(
        channel("ipc"),
        TurnInput::new(vec![
            TurnInputPart::Text {
                text: "不应落盘".into(),
            },
            TurnInputPart::Image {
                bytes: img1.clone(),
            },
        ])
        .unwrap(),
    )];
    let stale_settings = SessionSettings::for_test(&rejected.config);
    db.create_session(&lock1, project(1), stale_settings, now())
        .unwrap();
    rejected.settings_version = {
        let mut altered = config();
        altered.system_prompt = Some("已变化".into());
        SessionSettings::for_test(&altered).version().unwrap()
    };
    assert!(matches!(
        db.accept_turn(&lock1, rejected),
        Err(StoreError::SettingsChanged { .. })
    ));

    // 2. 原子性：注入提交失败时图片与节点都不存在。
    let mut faulty = accept(sid(1), tid(1));
    faulty.inputs = vec![(
        channel("ipc"),
        TurnInput::new(vec![
            TurnInputPart::Text {
                text: "提交失败回滚".into(),
            },
            TurnInputPart::Image {
                bytes: img1.clone(),
            },
        ])
        .unwrap(),
    )];
    faulty.settings_version = db
        .begin_read()
        .unwrap()
        .session_settings(sid(1))
        .unwrap()
        .version()
        .unwrap();
    let fault_err = with_fault(FaultPoint::Poisoned, || db.accept_turn(&lock1, faulty));
    assert!(matches!(fault_err, Err(StoreError::CommitFailed { .. })));

    // 释放被写闸锁死的句柄，重开库核对节点与图片均未写入。
    drop(lock1);
    drop(db);
    assert_eq!(blob_count(&layout), 0, "失败事务不得写入 blobs 表");
    let db = SessionStore::open(&layout).unwrap();
    assert!(
        db.begin_read()
            .unwrap()
            .turn_node(tid(1))
            .unwrap()
            .is_none()
    );

    // 3. 接受含图轮次、去重与幂等/冲突判定。
    let lock1 = db.try_lock_session(sid(1)).unwrap();
    let mut turn1 = accept(sid(1), tid(1));
    turn1.inputs = vec![(
        channel("ipc"),
        TurnInput::new(vec![
            TurnInputPart::Text {
                text: "先看".into(),
            },
            TurnInputPart::Image {
                bytes: img1.clone(),
            },
            TurnInputPart::Text {
                text: "再看".into(),
            },
            TurnInputPart::Image {
                bytes: img2.clone(),
            },
        ])
        .unwrap(),
    )];
    turn1.settings_version = db
        .begin_read()
        .unwrap()
        .session_settings(sid(1))
        .unwrap()
        .version()
        .unwrap();
    assert!(matches!(
        db.accept_turn(&lock1, turn1.clone()).unwrap(),
        AcceptedTurn::Accepted(_)
    ));

    // turn_input 返回完整保序片段。
    let stored_parts = db.begin_read().unwrap().turn_input(sid(1), tid(1)).unwrap();
    assert_eq!(stored_parts, turn1.inputs[0].1.message().parts);

    // 相同 TurnId + 相同保序图文 -> Existing。
    assert!(matches!(
        db.accept_turn(&lock1, turn1.clone()).unwrap(),
        AcceptedTurn::Existing(_)
    ));

    // 相同 TurnId + 文本相同但图片顺序不同 -> Conflict。
    let mut swapped = turn1.clone();
    swapped.inputs = vec![(
        channel("ipc"),
        TurnInput::new(vec![
            TurnInputPart::Text {
                text: "先看".into(),
            },
            TurnInputPart::Image {
                bytes: img2.clone(),
            },
            TurnInputPart::Text {
                text: "再看".into(),
            },
            TurnInputPart::Image {
                bytes: img1.clone(),
            },
        ])
        .unwrap(),
    )];
    assert!(matches!(
        db.accept_turn(&lock1, swapped),
        Err(StoreError::Conflict { turn_id }) if turn_id == tid(1)
    ));

    // 相同 TurnId + 图片摘要不同 -> Conflict。
    let mut other_digest = turn1.clone();
    other_digest.inputs = vec![(
        channel("ipc"),
        TurnInput::new(vec![
            TurnInputPart::Text {
                text: "先看".into(),
            },
            TurnInputPart::Image {
                bytes: png_bytes(b"different-image"),
            },
            TurnInputPart::Text {
                text: "再看".into(),
            },
            TurnInputPart::Image {
                bytes: img2.clone(),
            },
        ])
        .unwrap(),
    )];
    assert!(matches!(
        db.accept_turn(&lock1, other_digest),
        Err(StoreError::Conflict { turn_id }) if turn_id == tid(1)
    ));

    // 同会话第二轮、不同会话分别复用同一张 img1（并带重复出现）-> blobs 仍只有 2 条。
    let mut turn2 = accept(sid(1), tid(2));
    turn2.inputs = vec![(
        channel("ipc"),
        TurnInput::new(vec![
            TurnInputPart::Image {
                bytes: img1.clone(),
            },
            TurnInputPart::Image {
                bytes: img1.clone(),
            },
        ])
        .unwrap(),
    )];
    turn2.settings_version = turn1.settings_version;
    db.accept_turn(&lock1, turn2).unwrap();

    let lock2 = db.try_lock_session(sid(2)).unwrap();
    let mut turn3 = accept(sid(2), tid(3));
    turn3.inputs = vec![(
        channel("ipc"),
        TurnInput::new(vec![TurnInputPart::Image {
            bytes: img1.clone(),
        }])
        .unwrap(),
    )];
    accept_prepared(&db, &lock2, turn3).unwrap();

    // 创建草稿会话 3 并删除，然后对会话 1 rewind 到根节点 0。
    let lock3 = db.try_lock_session(sid(3)).unwrap();
    let draft_time = now();
    db.create_session(
        &lock3,
        project(1),
        SessionSettings::for_test(&config()),
        draft_time,
    )
    .unwrap();
    assert!(
        db.delete_draft(&lock3, draft_time + chrono::Duration::seconds(1))
            .unwrap()
    );
    db.rewind(&lock1, NodeId(0), &project(1), now()).unwrap();

    // with_blob 借出字节与原始字节相同；不存在的摘要报 Corruption。
    {
        let read = db.begin_read().unwrap();
        let got1 = read
            .with_blob(&ref1.sha256, |bytes| bytes.to_vec())
            .unwrap();
        let got2 = read
            .with_blob(&ref2.sha256, |bytes| bytes.to_vec())
            .unwrap();
        assert_eq!(got1, img1);
        assert_eq!(got2, img2);
        let missing_err = read
            .with_blob(&[0xEE; 32], |_| ())
            .expect_err("缺失摘要必须报损坏");
        assert!(matches!(
            missing_err,
            StoreError::Corruption { table, .. } if table == "blobs"
        ));
    }

    drop(lock1);
    drop(lock2);
    drop(lock3);
    drop(db);

    assert_eq!(
        blob_count(&layout),
        2,
        "跨轮次跨会话重复图片只能存一份，rewind 与 delete_draft 不回收 blobs"
    );
    let reopened = SessionStore::open(&layout).expect("含图会话库重开校验必须通过");
    let read = reopened.begin_read().unwrap();
    assert_eq!(
        read.with_blob(&ref1.sha256, |bytes| bytes.to_vec())
            .unwrap(),
        img1
    );
}

#[test]
fn opening_v10_and_corrupted_blobs_and_node_refs_fail_with_typed_errors() {
    let img = png_bytes(b"valid-png-data");
    let valid_ref = lattice_conversation::ImageRef::from_bytes(&img).unwrap();
    let valid_key = blob_key(&valid_ref.sha256);

    let seed_store = || {
        let (dir, layout, db) = store();
        let lock = db.try_lock_session(sid(1)).unwrap();
        let mut a = accept(sid(1), tid(1));
        a.inputs = vec![(
            channel("ipc"),
            TurnInput::new(vec![
                TurnInputPart::Text { text: "图".into() },
                TurnInputPart::Image { bytes: img.clone() },
            ])
            .unwrap(),
        )];
        accept_prepared(&db, &lock, a).unwrap();
        drop(lock);
        drop(db);
        (dir, layout)
    };

    // 1. 元数据版本 9 与 10 均以 UnsupportedVersion 拒绝打开。
    for old_ver in [9_u32, 10_u32] {
        let (_dir, layout) = seed_store();
        let raw = redb::Database::open(layout.lattice_root().join("lattice.redb")).unwrap();
        let tx = raw.begin_write().unwrap();
        if old_ver == 9 {
            tx.delete_table(BLOBS).unwrap();
        }
        {
            let mut meta = tx.open_table(META).unwrap();
            for (k, v) in [
                ("store_kind", "\"lattice-session\"".to_owned()),
                ("store_format_version", old_ver.to_string()),
            ] {
                let mut bytes = old_ver.to_be_bytes().to_vec();
                bytes.extend_from_slice(v.as_bytes());
                meta.insert(k, bytes.as_slice()).unwrap();
            }
        }
        tx.commit().unwrap();
        drop(raw);
        let err = SessionStore::open(&layout)
            .err()
            .expect("旧版本会话库必须拒绝打开");
        assert!(matches!(
            err,
            StoreError::UnsupportedVersion {
                found,
                supported: 11,
                ..
            } if found == old_ver
        ));
    }

    // 2. blobs 值版本头不是 11 时，打开与 with_blob 均报 UnsupportedVersion。
    for bad_ver in [10_u32, 12_u32] {
        let (_dir, layout) = seed_store();
        {
            let raw = redb::Database::open(layout.lattice_root().join("lattice.redb")).unwrap();
            let tx = raw.begin_write().unwrap();
            {
                let mut table = tx.open_table(BLOBS).unwrap();
                let mut bytes = bad_ver.to_be_bytes().to_vec();
                bytes.extend_from_slice(&img);
                table.insert(valid_key.as_str(), bytes.as_slice()).unwrap();
            }
            tx.commit().unwrap();
        }
        let open_err = SessionStore::open(&layout)
            .err()
            .expect("blobs 值版本头不符必须拒绝打开");
        assert!(matches!(
            open_err,
            StoreError::UnsupportedVersion {
                ref table,
                found,
                supported: 11,
                ..
            } if table == "blobs" && found == bad_ver
        ));
    }

    // 2b. Input 消息未规范化（如空文本片段或相邻文本未合并）在打开时按损坏拒绝。
    for bad_parts in [
        json!([]),
        json!([{"semantic": {"text": {"text": ""}}, "replay_state": null}]),
        json!([
            {"semantic": {"text": {"text": "甲"}}, "replay_state": null},
            {"semantic": {"text": {"text": "乙"}}, "replay_state": null}
        ]),
    ] {
        let (_dir, layout, db) = store();
        let lock = db.try_lock_session(sid(1)).unwrap();
        accept_prepared(&db, &lock, accept(sid(1), tid(1))).unwrap();
        db.edit_test_node(
            NodeRef {
                session_id: sid(1),
                node: NodeId(1),
            },
            |wire| {
                wire["input"]["message"]["parts"] = bad_parts;
            },
        )
        .unwrap();
        drop(lock);
        drop(db);
        let err = SessionStore::open(&layout)
            .err()
            .expect("未规范化的用户消息打开时必须报损坏");
        assert!(matches!(
            err,
            StoreError::Corruption { ref table, ref key, .. }
                if table == "nodes" && key == &format!("{}/1", sid(1))
        ));
    }

    // 3. blobs 各种损坏均报 Corruption。
    for case in 0..5 {
        let (_dir, layout) = seed_store();
        let raw = redb::Database::open(layout.lattice_root().join("lattice.redb")).unwrap();
        let tx = raw.begin_write().unwrap();
        {
            let mut table = tx.open_table(BLOBS).unwrap();
            match case {
                // 篡改图片字节（保持有效 PNG 魔数与长度，使摘要不符）
                0 => {
                    let mut tampered = img.clone();
                    *tampered.last_mut().unwrap() ^= 0xFF;
                    let encoded = encode_blob(&tampered);
                    table
                        .insert(valid_key.as_str(), encoded.as_slice())
                        .unwrap();
                }
                // 值短于 4 字节
                1 => {
                    table
                        .insert(valid_key.as_str(), &[0_u8, 0, 10][..])
                        .unwrap();
                }
                // 伪造魔数（同时重算新内容键，使失败原因落在魔数上）
                2 => {
                    use sha2::Digest;
                    let fake = b"NOT_SUPPORTED_IMAGE_DATA";
                    let fake_digest: [u8; 32] = sha2::Sha256::digest(fake).into();
                    let fake_key = blob_key(&fake_digest);
                    let encoded = encode_blob(fake);
                    table.insert(fake_key.as_str(), encoded.as_slice()).unwrap();
                }
                // 正文为空（以空切片的真实摘要为键）
                3 => {
                    use sha2::Digest;
                    let empty_digest: [u8; 32] = sha2::Sha256::digest(b"").into();
                    let empty_key = blob_key(&empty_digest);
                    let encoded = encode_blob(b"");
                    table
                        .insert(empty_key.as_str(), encoded.as_slice())
                        .unwrap();
                }
                // 键不是 64 位小写十六进制
                _ => {
                    let encoded = encode_blob(&img);
                    table.insert("INVALID_KEY", encoded.as_slice()).unwrap();
                }
            }
        }
        tx.commit().unwrap();
        drop(raw);
        let err = SessionStore::open(&layout)
            .err()
            .unwrap_or_else(|| panic!("case {case} 损坏必须报 Corruption"));
        assert!(
            matches!(&err, StoreError::Corruption { table, .. } if table == "blobs"),
            "case {case} 期望 blobs Corruption，实际为：{err:?}"
        );
    }

    // 4. 删去被引用的条目（包含不在当前路径上的 rewind 分支节点）。
    {
        let (_dir, layout, db) = store();
        let lock = db.try_lock_session(sid(1)).unwrap();
        let mut a = accept(sid(1), tid(1));
        a.inputs = vec![(
            channel("ipc"),
            TurnInput::new(vec![TurnInputPart::Image { bytes: img.clone() }]).unwrap(),
        )];
        accept_prepared(&db, &lock, a).unwrap();
        db.rewind(&lock, NodeId(0), &project(1), now()).unwrap();
        drop(lock);
        drop(db);

        let raw = redb::Database::open(layout.lattice_root().join("lattice.redb")).unwrap();
        let tx = raw.begin_write().unwrap();
        tx.open_table(BLOBS)
            .unwrap()
            .remove(valid_key.as_str())
            .unwrap();
        tx.commit().unwrap();
        drop(raw);

        let err = SessionStore::open(&layout)
            .err()
            .expect("非当前路径分支引用缺失图片时也必须报 Corruption");
        assert!(matches!(
            &err,
            StoreError::Corruption { table, key, .. }
                if table == "nodes" && key == &format!("{}/1", sid(1))
        ));
    }

    // 5. 节点中 ImageRef 的 media_type 或 byte_length 与 blobs 条目不符。
    for mismatch in ["media_type", "byte_length"] {
        let (_dir, layout) = seed_store();
        let db = SessionStore::open(&layout).unwrap();
        db.edit_test_node(
            NodeRef {
                session_id: sid(1),
                node: NodeId(1),
            },
            |wire| {
                let image = &mut wire["input"]["message"]["parts"][1]["semantic"]["image"]["image"];
                if mismatch == "media_type" {
                    image["media_type"] = json!("jpeg");
                } else {
                    image["byte_length"] = json!(valid_ref.byte_length + 1);
                }
            },
        )
        .unwrap();
        drop(db);

        let err = SessionStore::open(&layout)
            .err()
            .unwrap_or_else(|| panic!("{mismatch} 不符必须报 Corruption"));
        assert!(
            matches!(
                &err,
                StoreError::Corruption { table, key, .. }
                    if table == "nodes" && key == &format!("{}/1", sid(1))
            ),
            "{mismatch} 不符期望 nodes 损坏，实际为：{err:?}"
        );
    }
}

// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! v11 节点演进、四类写原语、全 7 变体 `ContextEdit`、双读路径与全库校验测试。

use super::*;
use lattice_conversation::{
    ConversationItem, ImageRef, MAX_USER_IMAGE_TOTAL_BYTES, MAX_USER_IMAGES_PER_TURN,
    MAX_USER_TEXT_BYTES,
};
use std::collections::BTreeMap;

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

fn create_draft(db: &SessionStore, lock: &SessionExecutionLock) -> SettingsVersion {
    let settings = SessionSettings::for_test(&config());
    let version = settings.version().unwrap();
    db.create_session(lock, project(1), settings, now())
        .expect("创建草稿会话");
    version
}

#[test]
fn feed_input_and_trigger_turn_and_idempotency() {
    let (_dir, layout, db) = store();
    let lock = db.try_lock_session(sid(1)).unwrap();
    let version = create_draft(&db, &lock);

    // 1. 草稿会话当前有效上下文为空，独立触发必须拒绝且不落库。
    let empty_trigger = TriggerTurn {
        session_id: sid(1),
        turn_id: tid(1),
        project_path: project(1),
        trigger_channel: channel("cli"),
        config: config(),
        accepted_at: now(),
        settings_version: version,
        expected_position: None,
    };
    let err = db
        .trigger_turn(&lock, empty_trigger.clone())
        .expect_err("草稿空上下文独立触发必须拒绝");
    assert!(matches!(
        err,
        StoreError::InvalidInput {
            field: "context",
            ..
        }
    ));

    // 2. 纯投喂不开启 Turn，写入 Node::Input 与去重 blobs，推进 current。
    let img = png_bytes(b"feed-image");
    let img_ref = ImageRef::from_bytes(&img).unwrap();
    let n1 = db
        .feed_input(
            &lock,
            FeedInput {
                session_id: sid(1),
                project_path: project(1),
                channel: channel("screenshot"),
                input: TurnInput::new(vec![
                    TurnInputPart::Text {
                        text: "截屏说明".into(),
                    },
                    TurnInputPart::Image { bytes: img.clone() },
                ])
                .unwrap(),
                accepted_at: now(),
                expected_position: Some(NodeId(0)),
            },
        )
        .expect("纯投喂图文输入");
    assert_eq!(n1, NodeId(1));

    // 纯投喂后 view 直接返回末尾未触发块，无任何 Step；effective_context 包含该输入。
    {
        let read = db.begin_read().unwrap();
        let view = read.view(sid(1), 10).unwrap();
        assert_eq!(entry_nodes(&view.entries), vec![1]);
        assert!(!view.has_more);
        assert!(matches!(
            &view.entries[0],
            ConversationEntry::Input {
                node: NodeId(1),
                ..
            }
        ));
        let (older, has_more) = read.history(sid(1), NodeId(1), 1).unwrap();
        assert!(older.is_empty() && !has_more);
        assert_eq!(read.blob_count().unwrap(), 1);
        let eff = read.effective_context(sid(1), NodeId(1)).unwrap();
        assert_eq!(eff.project().len(), 1);
        assert_eq!(eff.inspect().images, vec![img_ref]);
    }

    // 3. 独立触发成功：Step 0 的 input_nodes 为空，turn_index 指向 Step 0。
    let accepted = db
        .trigger_turn(&lock, empty_trigger.clone())
        .expect("已有前置投喂时独立触发应成功");
    let AcceptedTurn::Accepted(detail) = accepted else {
        panic!("首次触发必须返回 Accepted");
    };
    assert_eq!(detail.start_node, NodeId(2));
    assert!(
        detail.input_nodes.is_empty(),
        "独立触发的 input_nodes 必须为空"
    );
    assert_eq!(detail.steps.len(), 1);
    assert_eq!(detail.steps[0].node, NodeId(2));

    // 4. 独立触发精确幂等与冲突核对（即使携带旧 expected_position 与旧设置版本，幂等匹配必须优先返回 Existing）。
    let mut replay_with_old_pos = empty_trigger.clone();
    replay_with_old_pos.expected_position = Some(NodeId(1));
    assert!(matches!(
        db.trigger_turn(&lock, replay_with_old_pos).unwrap(),
        AcceptedTurn::Existing(existing) if existing == detail
    ));
    let mut wrong_channel = empty_trigger.clone();
    wrong_channel.trigger_channel = channel("tui");
    assert!(matches!(
        db.trigger_turn(&lock, wrong_channel),
        Err(StoreError::Conflict { turn_id }) if turn_id == tid(1)
    ));
    // 用 accept_turn 复用独立触发的 TurnId 必须返回 Conflict。
    assert!(matches!(
        db.accept_turn(&lock, accept(sid(1), tid(1))),
        Err(StoreError::Conflict { turn_id }) if turn_id == tid(1)
    ));

    // 5. 关闭第 1 轮后，立即对第 1 轮的结果再次独立触发（Step 0 紧跟 Step 0），核验分页边界 is_unit_start。
    db.start_request(&lock, key(tid(1), 0, 0), origin(), now())
        .unwrap();
    db.close_step(&lock, CloseStep::Finish(reply(key(tid(1), 0, 0), 0)), now())
        .unwrap();
    let mut second_trigger = empty_trigger;
    second_trigger.turn_id = tid(2);
    let AcceptedTurn::Accepted(detail2) = db.trigger_turn(&lock, second_trigger).unwrap() else {
        panic!("第二次独立触发应成功");
    };
    assert_eq!(detail2.start_node, NodeId(3));

    {
        let read = db.begin_read().unwrap();
        // NodeId(3) 是父节点为 Step 0 (NodeId(2)) 的独立触发 Step 0，是合法分页单元起点。
        let (older, has_more) = read.history(sid(1), NodeId(3), 1).unwrap();
        assert_eq!(entry_nodes(&older), vec![1, 2]);
        assert!(!has_more);
        // NodeId(2) 的父节点是 Input (NodeId(1))，不能作为分页起点切碎前置输入。
        assert!(matches!(
            read.history(sid(1), NodeId(2), 1),
            Err(StoreError::InvalidInput {
                field: "before",
                ..
            })
        ));
    }

    drop(lock);
    drop(db);
    SessionStore::open(&layout).expect("含纯投喂与独立触发的 v11 会话库必须通过严格打开");
}

#[test]
fn apply_context_op_covers_all_seven_variants_and_blob_guards() {
    let (_dir, layout, db) = store();
    let lock = db.try_lock_session(sid(1)).unwrap();
    let version = create_draft(&db, &lock);

    let img_a = png_bytes(b"img-a");
    let img_b = jpeg_bytes(b"img-b");
    let img_c = png_bytes(b"img-c");
    let ref_a = ImageRef::from_bytes(&img_a).unwrap();
    let ref_b = ImageRef::from_bytes(&img_b).unwrap();
    let ref_c = ImageRef::from_bytes(&img_c).unwrap();

    // 投喂 Input(1)（含 img_a）与 Input(2)（纯文本），再执行含工具调用与推理的 Turn 1：
    // Input(1) -> Input(2) -> Input(3) -> Step0(4)（2 次调用 iid(1), iid(2) + 推理）-> Step1(5)（最终纯文本回复）
    let n1 = db
        .feed_input(
            &lock,
            FeedInput {
                session_id: sid(1),
                project_path: project(1),
                channel: channel("tui"),
                input: TurnInput::new(vec![
                    TurnInputPart::Text {
                        text: "图A说明".into(),
                    },
                    TurnInputPart::Image {
                        bytes: img_a.clone(),
                    },
                ])
                .unwrap(),
                accepted_at: now(),
                expected_position: None,
            },
        )
        .unwrap();
    assert_eq!(n1, NodeId(1));

    let n2 = db
        .feed_input(
            &lock,
            FeedInput {
                session_id: sid(1),
                project_path: project(1),
                channel: channel("tui/comment"),
                input: TurnInput::text("待替换批注".into()).unwrap(),
                accepted_at: now(),
                expected_position: None,
            },
        )
        .unwrap();
    assert_eq!(n2, NodeId(2));

    let mut turn1 = accept(sid(1), tid(1));
    turn1.settings_version = version;
    turn1.inputs = vec![(channel("tui"), TurnInput::text("执行查询".into()).unwrap())];
    let AcceptedTurn::Accepted(t1) = db.accept_turn(&lock, turn1).unwrap() else {
        panic!("接受 Turn 1");
    };
    assert_eq!(t1.input_nodes, vec![NodeId(3)]);
    assert_eq!(t1.start_node, NodeId(4));

    db.start_request(&lock, key(tid(1), 0, 0), origin(), now())
        .unwrap();
    let calls = db
        .register_calls(&lock, reply(key(tid(1), 0, 0), 2), &[iid(1), iid(2)], now())
        .unwrap();
    finish(&db, &lock, &calls.bindings[0], ok_reply());
    finish(
        &db,
        &lock,
        &calls.bindings[1],
        failed_reply("execution_failed"),
    );
    db.close_step(
        &lock,
        CloseStep::Continue {
            key: step(tid(1), 0),
        },
        now(),
    )
    .unwrap();
    db.start_request(&lock, key(tid(1), 1, 0), origin(), now())
        .unwrap();
    db.close_step(&lock, CloseStep::Finish(reply(key(tid(1), 1, 0), 0)), now())
        .unwrap();

    // 校验 apply_context_op 的图片载荷守卫：
    // (a) 多余未引用图片被拒绝；
    let mut unused_blobs = BTreeMap::new();
    unused_blobs.insert(ref_c.sha256, img_c.clone());
    let unused_err = db
        .apply_context_op(
            &lock,
            ApplyContextOp::new(
                sid(1),
                project(1),
                ContextOp::new(
                    channel("tui"),
                    vec![ContextEdit::strip_reasoning(NodeId(4))],
                )
                .unwrap(),
                now(),
            )
            .with_blobs(unused_blobs),
        )
        .expect_err("携带未被 ReplaceInput 引用的多余图片必须拒绝");
    assert!(matches!(
        unused_err,
        StoreError::InvalidInput { field: "blobs", .. }
    ));

    // (b) ReplaceInput 引用库中与请求中均不存在的图片被拒绝；
    let (missing_blob_edit, _) = TurnInput::new(vec![TurnInputPart::Image {
        bytes: img_c.clone(),
    }])
    .unwrap()
    .into_replace_input(NodeId(2));
    let missing_err = db
        .apply_context_op(
            &lock,
            ApplyContextOp::new(
                sid(1),
                project(1),
                ContextOp::new(channel("tui"), vec![missing_blob_edit]).unwrap(),
                now(),
            ),
        )
        .expect_err("ReplaceInput 引用缺失图片必须拒绝");
    assert!(matches!(
        missing_err,
        StoreError::InvalidInput { field: "blobs", .. }
    ));

    // (c) 摘要键与字节不匹配被拒绝；
    let mut bad_key_blobs = BTreeMap::new();
    bad_key_blobs.insert([0xAA; 32], img_c.clone());
    let (bad_key_edit, _) = TurnInput::new(vec![TurnInputPart::Image {
        bytes: img_c.clone(),
    }])
    .unwrap()
    .into_replace_input(NodeId(2));
    assert!(matches!(
        db.apply_context_op(
            &lock,
            ApplyContextOp::new(
                sid(1),
                project(1),
                ContextOp::new(channel("tui"), vec![bad_key_edit]).unwrap(),
                now(),
            )
            .with_blobs(bad_key_blobs),
        ),
        Err(StoreError::InvalidInput { field: "blobs", .. })
    ));

    // (d) 非法上下文操作（引用不存在节点）返回 StoreError::Context 且不写入 blobs。
    let blobs_before = db.begin_read().unwrap().blob_count().unwrap();
    let (invalid_target_edit, invalid_target_blobs) =
        TurnInput::new(vec![TurnInputPart::Image { bytes: img_c }])
            .unwrap()
            .into_replace_input(NodeId(999));
    let ctx_err = db
        .apply_context_op(
            &lock,
            ApplyContextOp::new(
                sid(1),
                project(1),
                ContextOp::new(channel("tui"), vec![invalid_target_edit]).unwrap(),
                now(),
            )
            .with_blobs(invalid_target_blobs),
        )
        .expect_err("引用不存在节点必须返回 StoreError::Context");
    assert!(matches!(
        ctx_err,
        StoreError::Context(ContextError::UnknownNode { node: NodeId(999) })
    ));
    assert_eq!(
        db.begin_read().unwrap().blob_count().unwrap(),
        blobs_before,
        "被拒绝的 ContextOp 不得写入 blobs"
    );

    // 施加全部 7 个 ContextEdit 变体（含新图片 img_b 与复用已有图片 img_a）：
    // 1) ReplaceImage (Some) 替换 Input(1) 中的 ref_a
    // 2) ReplaceInput 用含 img_b + img_a 的消息替换 Input(2)
    // 3) OmitNode 移出 Input(3)
    // 4) ReplaceToolResult 精简 iid(1) 工具输出
    // 5) OmitInvocation 成对移出 iid(2)
    // 6) StripReasoning 剥离 Step0(4) 推理
    // 7) ReplaceStep 精简 Step1(5) 为纯文本结论
    let (replace_input_edit, mut op_blobs) = TurnInput::new(vec![
        TurnInputPart::Text {
            text: "已替换为新图".into(),
        },
        TurnInputPart::Image {
            bytes: img_b.clone(),
        },
        TurnInputPart::Image { bytes: img_a },
    ])
    .unwrap()
    .into_replace_input(NodeId(2));
    // 移除已在库中的 img_a，验证 ReplaceInput 可直接复用已存 blobs，只随附新图 img_b。
    op_blobs.remove(&ref_a.sha256);
    assert_eq!(op_blobs.len(), 1);

    let op = ContextOp::new(
        channel("tui"),
        vec![
            ContextEdit::replace_image(NodeId(1), ref_a, Some("[图片已压缩]".into())).unwrap(),
            replace_input_edit,
            ContextEdit::omit_node(NodeId(3)),
            ContextEdit::replace_tool_result(iid(1), "精简结果").unwrap(),
            ContextEdit::omit_invocation(iid(2)),
            ContextEdit::strip_reasoning(NodeId(4)),
            ContextEdit::replace_step(NodeId(5), "最终精简结论").unwrap(),
        ],
    )
    .unwrap();
    let op_node = db
        .apply_context_op(
            &lock,
            ApplyContextOp::new(sid(1), project(1), op, now()).with_blobs(op_blobs),
        )
        .expect("全部 7 个变体的批量 ContextOp 应成功落树");
    assert_eq!(op_node, NodeId(6));

    // 双读路径验证：
    {
        let read = db.begin_read().unwrap();
        assert_eq!(read.blob_count().unwrap(), 2, "应去重保存 img_a 与 img_b");
        assert_eq!(
            read.with_blob(&ref_b.sha256, |bytes| bytes.to_vec())
                .unwrap(),
            img_b
        );

        // 路径一（Transcript）：被 Omit 的 Input(3) 与全部原始节点均完整保留，末尾包含 ContextOp(6)。
        let view = read.view(sid(1), 10).unwrap();
        assert_eq!(entry_nodes(&view.entries), vec![1, 2, 3, 4, 5, 6]);
        assert!(matches!(
            &view.entries[5],
            ConversationEntry::ContextOp {
                node: NodeId(6),
                ..
            }
        ));
        // ContextOp(6) 的父节点是 Step1(5)，是末尾未触发块的起点，可作为 history 分页边界。
        let (before_op, before_more) = read.history(sid(1), NodeId(6), 1).unwrap();
        assert_eq!(entry_nodes(&before_op), vec![1, 2, 3, 4, 5]);
        assert!(!before_more);

        // 路径二（Effective Model Context）：反映全部 7 项编辑结果。
        let eff = read.effective_context(sid(1), NodeId(6)).unwrap();
        let inspection = eff.inspect();
        assert_eq!(
            inspection.images,
            vec![ref_b, ref_a],
            "Input(1) 图片被替换为文本，Input(2) 替换为 [ref_b, ref_a]"
        );
        let projected = eff.project();
        // 有效条目：Input(1) + Input(2) + Step0(4) + Step1(5)（Input(3) 已 Omit）
        assert_eq!(projected.len(), 4);
        match &projected[2] {
            ConversationItem::Step(step_content) => {
                assert!(
                    step_content.assistant.provider_origin.is_none(),
                    "OmitInvocation/StripReasoning 后必须清除 provider_origin"
                );
                assert_eq!(step_content.results.len(), 1, "iid(2) 已被成对移出");
                assert_eq!(step_content.results[0].invocation_id, iid(1));
                assert!(
                    !step_content
                        .assistant
                        .parts
                        .iter()
                        .any(|p| matches!(p.semantic, MessageSemantic::Reasoning { .. })),
                    "Reasoning 必须已全部剥离"
                );
            }
            other => panic!("期望 Step 投影，实际为 {other:?}"),
        }
        match &projected[3] {
            ConversationItem::Step(step_content) => {
                assert!(step_content.results.is_empty());
                assert_eq!(
                    crate::test_helpers::text(&step_content.assistant),
                    Some("最终精简结论")
                );
            }
            other => panic!("期望 ReplaceStep 后的 Step 投影，实际为 {other:?}"),
        }

        // effective_context_with_pending 叠加一条待提交输入后与实际投影一致。
        let pending = ContextInput::new(
            channel("cli"),
            vec![MessageSemantic::Text {
                text: "追加提问".into(),
            }],
        )
        .unwrap();
        let with_pending = read
            .effective_context_with_pending(sid(1), NodeId(6), &[pending])
            .unwrap();
        assert_eq!(with_pending.project().len(), 5);
    }

    // 验证 ReplaceImage { text: None } 与 OmitNode 使整条路径为空时拒绝 trigger_turn。
    let omit_all = ContextOp::new(
        channel("tui"),
        vec![
            ContextEdit::omit_node(NodeId(1)),
            ContextEdit::omit_node(NodeId(2)),
            ContextEdit::omit_node(NodeId(4)),
            ContextEdit::omit_node(NodeId(5)),
        ],
    )
    .unwrap();
    db.apply_context_op(
        &lock,
        ApplyContextOp::new(sid(1), project(1), omit_all, now()),
    )
    .unwrap();
    let empty_err = db
        .trigger_turn(
            &lock,
            TriggerTurn {
                session_id: sid(1),
                turn_id: tid(9),
                project_path: project(1),
                trigger_channel: channel("tui"),
                config: config(),
                accepted_at: now(),
                settings_version: version,
                expected_position: None,
            },
        )
        .expect_err("全部节点被 Omit 后有效上下文为空，独立触发必须拒绝");
    assert!(matches!(
        empty_err,
        StoreError::InvalidInput {
            field: "context",
            ..
        }
    ));

    drop(lock);
    drop(db);
    SessionStore::open(&layout).expect("含全部 7 类 ContextEdit 与多分支的库重开校验必须通过");
}

#[test]
fn multi_input_accept_turn_batch_limits_and_idempotency() {
    let (_dir, layout, db) = store();
    let lock = db.try_lock_session(sid(1)).unwrap();
    let version = create_draft(&db, &lock);

    // 1. 跨整批单轮上限校验：空批、文本超 2 MiB、图片超 8 张、图片总字节超 10 MiB。
    assert!(matches!(
        validate_turn_input_batch(&[]),
        Err(StoreError::InvalidInput { field: "inputs", detail }) if detail.contains("用户输入不能为空")
    ));
    let half_text = MAX_USER_TEXT_BYTES / 2;
    let over_text_batch = vec![
        (
            channel("tui/comment"),
            TurnInput::text("a".repeat(half_text)).unwrap(),
        ),
        (
            channel("tui"),
            TurnInput::text("b".repeat(MAX_USER_TEXT_BYTES - half_text + 1)).unwrap(),
        ),
    ];
    assert!(matches!(
        validate_turn_input_batch(&over_text_batch),
        Err(StoreError::InvalidInput { field: "inputs", detail }) if detail.contains("2,097,152")
    ));

    let small_png = png_bytes(b"small");
    let five_imgs = TurnInput::new(
        (0..5)
            .map(|_| TurnInputPart::Image {
                bytes: small_png.clone(),
            })
            .collect(),
    )
    .unwrap();
    let four_imgs = TurnInput::new(
        (0..4)
            .map(|_| TurnInputPart::Image {
                bytes: small_png.clone(),
            })
            .collect(),
    )
    .unwrap();
    assert_eq!(5 + 4, MAX_USER_IMAGES_PER_TURN + 1);
    assert!(matches!(
        validate_turn_input_batch(&[
            (channel("tui/comment"), five_imgs),
            (channel("tui"), four_imgs),
        ]),
        Err(StoreError::InvalidInput { field: "inputs", detail }) if detail.contains("不能超过 8 张")
    ));

    let mut big_a = vec![0x11; 3_932_160];
    big_a[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
    let mut big_b = vec![0x22; 3_932_160];
    big_b[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
    let mut big_c = vec![0x33; 2_621_441];
    big_c[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
    assert_eq!(
        big_a.len() + big_b.len() + big_c.len(),
        MAX_USER_IMAGE_TOTAL_BYTES + 1
    );
    let over_img_bytes = vec![
        (
            channel("tui/comment"),
            TurnInput::new(vec![
                TurnInputPart::Image { bytes: big_a },
                TurnInputPart::Image { bytes: big_b },
            ])
            .unwrap(),
        ),
        (
            channel("tui"),
            TurnInput::new(vec![TurnInputPart::Image { bytes: big_c }]).unwrap(),
        ),
    ];
    assert!(matches!(
        validate_turn_input_batch(&over_img_bytes),
        Err(StoreError::InvalidInput { field: "inputs", detail }) if detail.contains("10,485,760")
    ));

    // 2. 多条输入原子写入并链式衔接至 Step 0。
    let req = AcceptTurn {
        session_id: sid(1),
        turn_id: tid(1),
        project_path: project(1),
        trigger_channel: channel("tui"),
        inputs: vec![
            (
                channel("tui/comment"),
                TurnInput::text("划词批注".into()).unwrap(),
            ),
            (
                channel("tui"),
                TurnInput::text("请按批注修改".into()).unwrap(),
            ),
        ],
        config: config(),
        accepted_at: now(),
        settings_version: version,
        expected_position: Some(NodeId(0)),
    };
    let AcceptedTurn::Accepted(detail) = db.accept_turn(&lock, req.clone()).unwrap() else {
        panic!("多输入 accept_turn 应返回 Accepted");
    };
    assert_eq!(detail.input_nodes, vec![NodeId(1), NodeId(2)]);
    assert_eq!(detail.start_node, NodeId(3));

    // 精确幂等重放成功（即使原 expected_position 为提交前的 NodeId(0)，也须先命中幂等返回 Existing）；任一条通道或内容不一致均返回 Conflict。
    assert!(matches!(
        db.accept_turn(&lock, req.clone()).unwrap(),
        AcceptedTurn::Existing(existing) if existing == detail
    ));
    let mut diff_channel = req.clone();
    diff_channel.inputs[0].0 = channel("cli");
    assert!(matches!(
        db.accept_turn(&lock, diff_channel),
        Err(StoreError::Conflict { turn_id }) if turn_id == tid(1)
    ));
    let mut diff_len = req;
    diff_len.inputs.pop();
    assert!(matches!(
        db.accept_turn(&lock, diff_len),
        Err(StoreError::Conflict { turn_id }) if turn_id == tid(1)
    ));

    // 3. 验证 ConversationEntry 与 Node 的 redacted_debug! 不泄露正文。
    let read = db.begin_read().unwrap();
    let entries = read.view(sid(1), 10).unwrap().entries;
    for entry in &entries {
        let dbg = format!("{entry:?}");
        assert_eq!(dbg, "ConversationEntry { … }");
        let node = read
            .node(NodeRef {
                session_id: sid(1),
                node: entry.node(),
            })
            .unwrap();
        assert_eq!(format!("{node:?}"), "Node { … }");
    }
    drop(read);
    drop(lock);
    drop(db);
    SessionStore::open(&layout).expect("多输入轮次库重开校验必须通过");
}

#[test]
fn open_rejects_corrupted_context_op_on_rewound_branch_and_empty_trigger_step0() {
    // 1. 非当前分支（rewind 遗留分叉）上损坏的 ContextOp 必须在 open() 被全树叶子路径校验捕获。
    {
        let (_dir, layout, db) = store();
        let lock = db.try_lock_session(sid(1)).unwrap();
        let version = create_draft(&db, &lock);
        db.feed_input(
            &lock,
            FeedInput {
                session_id: sid(1),
                project_path: project(1),
                channel: channel("tui"),
                input: TurnInput::text("分支 A 输入".into()).unwrap(),
                accepted_at: now(),
                expected_position: None,
            },
        )
        .unwrap();
        let op_id = db
            .apply_context_op(
                &lock,
                ApplyContextOp::new(
                    sid(1),
                    project(1),
                    ContextOp::new(channel("tui"), vec![ContextEdit::omit_node(NodeId(1))])
                        .unwrap(),
                    now(),
                ),
            )
            .unwrap();
        assert_eq!(op_id, NodeId(2));
        // 回退到 Root 并开启另一条干净分支。
        db.rewind(&lock, NodeId(0), &project(1), now()).unwrap();
        let mut clean = accept(sid(1), tid(1));
        clean.settings_version = version;
        db.accept_turn(&lock, clean).unwrap();

        // 篡改遗留分叉上的 NodeId(2)，使其引用本路径不存在的 NodeId(99)。
        db.edit_test_node(
            NodeRef {
                session_id: sid(1),
                node: NodeId(2),
            },
            |wire| {
                wire["op"]["edits"][0]["node"] = json!(99);
            },
        )
        .unwrap();
        drop(lock);
        drop(db);
        assert!(matches!(
            SessionStore::open(&layout),
            Err(StoreError::Corruption { ref table, .. }) if table == "nodes"
        ));
    }

    // 2. 独立触发 Step 0（input_nodes 为空）的父路径有效上下文为空时，open() 必须报 Corruption。
    {
        let (_dir, layout, db) = store();
        let lock = db.try_lock_session(sid(1)).unwrap();
        let version = create_draft(&db, &lock);
        db.feed_input(
            &lock,
            FeedInput {
                session_id: sid(1),
                project_path: project(1),
                channel: channel("tui"),
                input: TurnInput::text("前置输入".into()).unwrap(),
                accepted_at: now(),
                expected_position: None,
            },
        )
        .unwrap();
        db.trigger_turn(
            &lock,
            TriggerTurn {
                session_id: sid(1),
                turn_id: tid(1),
                project_path: project(1),
                trigger_channel: channel("tui"),
                config: config(),
                accepted_at: now(),
                settings_version: version,
                expected_position: None,
            },
        )
        .unwrap();
        // 将 Step 0 (NodeId(2)) 的 parent 篡改为 Root (NodeId(0))，使独立触发父路径为空。
        db.edit_test_node(
            NodeRef {
                session_id: sid(1),
                node: NodeId(2),
            },
            |wire| {
                wire["parent"] = json!(0);
            },
        )
        .unwrap();
        drop(lock);
        drop(db);
        assert!(matches!(
            SessionStore::open(&layout),
            Err(StoreError::Corruption { ref table, ref key, .. })
                if table == "nodes" && key == &format!("{}/2", sid(1))
        ));
    }

    // 3. Step 0 的 input_nodes 非空但链断裂（未紧邻 parent）时，open() 必须报 Corruption。
    {
        let (_dir, layout, db) = store();
        let lock = db.try_lock_session(sid(1)).unwrap();
        accept_prepared(&db, &lock, accept(sid(1), tid(1))).unwrap();
        db.edit_test_node(
            NodeRef {
                session_id: sid(1),
                node: NodeId(2),
            },
            |wire| {
                wire["turn_start"]["input_nodes"] = json!([0]);
            },
        )
        .unwrap();
        drop(lock);
        drop(db);
        assert!(matches!(
            SessionStore::open(&layout),
            Err(StoreError::Corruption { .. })
        ));
    }
}

#[test]
fn accept_turn_rejects_empty_batch_before_idempotency_and_never_mutates_store() {
    let (_dir, layout, db) = store();
    let lock = db.try_lock_session(sid(1)).unwrap();
    let version = create_draft(&db, &lock);

    db.feed_input(
        &lock,
        FeedInput {
            session_id: sid(1),
            project_path: project(1),
            channel: channel("cli"),
            input: TurnInput::text("先投喂再独立触发".into()).unwrap(),
            accepted_at: now(),
            expected_position: None,
        },
    )
    .expect("前置投喂应成功");

    let trigger_req = TriggerTurn {
        session_id: sid(1),
        turn_id: tid(1),
        project_path: project(1),
        trigger_channel: channel("cli"),
        config: config(),
        accepted_at: now(),
        settings_version: version,
        expected_position: None,
    };
    let AcceptedTurn::Accepted(triggered_detail) = db
        .trigger_turn(&lock, trigger_req.clone())
        .expect("独立触发应成功")
    else {
        panic!("首次独立触发必须返回 Accepted");
    };
    assert!(triggered_detail.input_nodes.is_empty());

    let snapshot = || {
        let read = db.begin_read().unwrap();
        (
            read.log(sid(1)).unwrap(),
            read.turn_detail(sid(1), tid(1)).unwrap(),
            read.blob_count().unwrap(),
        )
    };
    let baseline = snapshot();

    // 1. 以相同 TurnId 调用空批 accept_turn：必须在查询 turn_index 前直接返回 InvalidInput，
    //    绝不能误命中独立触发的幂等结果，且持久库零改动。
    let mut empty_same_tid = accept(sid(1), tid(1));
    empty_same_tid.trigger_channel = channel("cli");
    empty_same_tid.settings_version = version;
    empty_same_tid.inputs = Vec::new();
    let err_same = db
        .accept_turn(&lock, empty_same_tid)
        .expect_err("相同 TurnId 的空批 accept_turn 必须返回 InvalidInput");
    assert!(
        matches!(
            err_same,
            StoreError::InvalidInput {
                field: "inputs",
                ..
            }
        ),
        "期望 InvalidInput(inputs)，实际得到 {err_same:?}"
    );
    assert_eq!(snapshot(), baseline, "空批拒绝后持久库记录不得产生任何变化");

    // 2. 以全新 TurnId 调用空批 accept_turn：同样返回 InvalidInput 且不写入任何记录。
    let mut empty_fresh_tid = accept(sid(1), tid(2));
    empty_fresh_tid.trigger_channel = channel("cli");
    empty_fresh_tid.settings_version = version;
    empty_fresh_tid.inputs = Vec::new();
    let err_fresh = db
        .accept_turn(&lock, empty_fresh_tid)
        .expect_err("全新 TurnId 的空批 accept_turn 必须返回 InvalidInput");
    assert!(
        matches!(
            err_fresh,
            StoreError::InvalidInput {
                field: "inputs",
                ..
            }
        ),
        "期望 InvalidInput(inputs)，实际得到 {err_fresh:?}"
    );
    assert!(matches!(
        db.begin_read().unwrap().turn_detail(sid(1), tid(2)),
        Err(StoreError::NotFound { .. })
    ));
    assert_eq!(snapshot(), baseline, "全新 TurnId 空批拒绝后持久库不得改动");

    // 3. 原独立触发幂等仍正常命中，且重开严格校验通过。
    assert!(matches!(
        db.trigger_turn(&lock, trigger_req).unwrap(),
        AcceptedTurn::Existing(existing) if existing == triggered_detail
    ));
    drop(lock);
    drop(db);
    SessionStore::open(&layout).expect("空批拒绝后重开会话库必须通过");
}

#[test]
fn open_validates_large_branched_tree_and_repeated_empty_triggers_linearly() {
    // 1. 构造「250 节点长共享前缀 + 250 个末端分叉（均含 ContextOp）+ 40 次连续独立触发 Step 0」
    //    的大规模分叉会话树，验证单遍 DFS + ContextCursor 线性通过严格重开校验，并精准捕获非当前分叉损坏。
    {
        let (_dir, layout, db) = store();
        let lock = db.try_lock_session(sid(1)).unwrap();
        let version = create_draft(&db, &lock);
        db.feed_input(
            &lock,
            FeedInput {
                session_id: sid(1),
                project_path: project(1),
                channel: channel("cli"),
                input: TurnInput::text("共享前缀首输入".into()).unwrap(),
                accepted_at: now(),
                expected_position: None,
            },
        )
        .unwrap();
        db.trigger_turn(
            &lock,
            TriggerTurn {
                session_id: sid(1),
                turn_id: tid(1),
                project_path: project(1),
                trigger_channel: channel("cli"),
                config: config(),
                accepted_at: now(),
                settings_version: version,
                expected_position: None,
            },
        )
        .unwrap();
        db.start_request(&lock, key(tid(1), 0, 0), origin(), now())
            .unwrap();
        db.close_step(&lock, CloseStep::Finish(reply(key(tid(1), 0, 0), 0)), now())
            .unwrap();
        drop(lock);
        drop(db);

        // 在单个写事务内批量追加长共享前缀、大量末端 ContextOp 分叉与连续独立触发 Step 0，
        // 避免数百次独立 fsync 拖慢测试，同时完全遵循 v11 规范编码与索引不变量。
        let s = sid(1);
        let prefix_len: u64 = 250;
        let branch_count: u64 = 250;
        let trigger_count: u64 = 40;
        let prefix_start: u64 = 3;
        let prefix_end: u64 = prefix_start + prefix_len - 1; // 252
        let branch_start: u64 = prefix_end + 1; // 253
        let trigger_start: u64 = branch_start + branch_count; // 503
        let last_node_id = NodeId(trigger_start + trigger_count - 1);

        {
            let raw = redb::Database::open(layout.lattice_root().join("lattice.redb")).unwrap();
            let tx = raw.begin_write().unwrap();
            {
                let mut nodes = tx.open_table(NODES).unwrap();
                let mut turn_index = tx.open_table(TURN_INDEX).unwrap();
                let mut sessions = tx.open_table(SESSIONS).unwrap();
                let template_step: Node = decode(
                    "nodes",
                    &format!("{s}/2"),
                    nodes
                        .get(node_key(s, NodeId(2)).as_slice())
                        .unwrap()
                        .unwrap()
                        .value(),
                )
                .unwrap();

                // 250 个链式共享前缀 Input 节点（NodeId(3)..=NodeId(252)）。
                for i in 0..prefix_len {
                    let id = NodeId(prefix_start + i);
                    let parent = NodeId(id.0 - 1);
                    let node = Node::Input {
                        parent,
                        accepted_at: now(),
                        input: ContextInput::new(
                            channel("cli"),
                            vec![MessageSemantic::Text {
                                text: format!("共享前缀节点-{i}"),
                            }],
                        )
                        .unwrap(),
                    };
                    nodes
                        .insert(
                            node_key(s, id).as_slice(),
                            encode(&node).unwrap().as_slice(),
                        )
                        .unwrap();
                }

                // 250 个挂在共享前缀末端 NodeId(252) 下的单节点 ContextOp 分叉，各移出一个不同前置节点。
                for b in 0..branch_count {
                    let id = NodeId(branch_start + b);
                    let target = NodeId(prefix_start + b);
                    let node = Node::ContextOp {
                        parent: NodeId(prefix_end),
                        accepted_at: now(),
                        op: ContextOp::new(channel("cli"), vec![ContextEdit::omit_node(target)])
                            .unwrap(),
                    };
                    nodes
                        .insert(
                            node_key(s, id).as_slice(),
                            encode(&node).unwrap().as_slice(),
                        )
                        .unwrap();
                }

                // 40 次连续独立触发 Step 0（input_nodes 为空），链式挂在共享前缀末端之后。
                let mut prev = NodeId(prefix_end);
                for k in 0..trigger_count {
                    let id = NodeId(trigger_start + k);
                    let turn_id = tid(100 + k);
                    let mut node = template_step.clone();
                    let Node::Step {
                        parent,
                        turn_id: node_turn_id,
                        ..
                    } = &mut node
                    else {
                        panic!("NodeId(2) 必须为已关闭的独立触发 Step 0");
                    };
                    *parent = prev;
                    *node_turn_id = turn_id;
                    nodes
                        .insert(
                            node_key(s, id).as_slice(),
                            encode(&node).unwrap().as_slice(),
                        )
                        .unwrap();
                    let tkey = turn_id.to_string();
                    turn_index
                        .insert(
                            tkey.as_str(),
                            encode(&NodeRef {
                                session_id: s,
                                node: id,
                            })
                            .unwrap()
                            .as_slice(),
                        )
                        .unwrap();
                    prev = id;
                }

                // 将当前位置移至末次独立触发节点，使 250 个 ContextOp 分叉全部处于非当前历史分支。
                let skey = s.to_string();
                let mut rec: SessionRecord = decode(
                    "sessions",
                    &skey,
                    sessions.get(skey.as_str()).unwrap().unwrap().value(),
                )
                .unwrap();
                rec.current = last_node_id;
                sessions
                    .insert(skey.as_str(), encode(&rec).unwrap().as_slice())
                    .unwrap();
            }
            tx.commit().unwrap();
        }

        // 合法的大规模分叉树应快速通过严格打开校验。
        let reopened = SessionStore::open(&layout)
            .expect("含长共享前缀、大量末端分叉与连续独立触发的大规模会话树应通过打开校验");
        assert_eq!(
            reopened.begin_read().unwrap().log(s).unwrap().nodes.len(),
            (last_node_id.0 + 1) as usize
        );

        // 篡改其中一个非当前历史分叉（第 120 号分叉）的 ContextOp 使其引用不存在节点，open() 必须精准捕获。
        let corrupt_branch = NodeId(branch_start + 120);
        reopened
            .edit_test_node(
                NodeRef {
                    session_id: s,
                    node: corrupt_branch,
                },
                |wire| {
                    wire["op"]["edits"][0]["node"] = json!(999_999);
                },
            )
            .unwrap();
        drop(reopened);
        assert!(matches!(
            SessionStore::open(&layout),
            Err(StoreError::Corruption { ref table, ref key, .. })
                if table == "nodes" && key == &format!("{s}/{}", corrupt_branch.0)
        ));
    }

    // 2. 在将前置有效条目全部 OmitNode 移出的非根分叉上注入独立触发 Step 0，open() 必须返回 Corruption。
    {
        let (_dir, layout, db) = store();
        let lock = db.try_lock_session(sid(1)).unwrap();
        let version = create_draft(&db, &lock);
        let n1 = db
            .feed_input(
                &lock,
                FeedInput {
                    session_id: sid(1),
                    project_path: project(1),
                    channel: channel("tui"),
                    input: TurnInput::text("将被分叉移出的前置输入".into()).unwrap(),
                    accepted_at: now(),
                    expected_position: None,
                },
            )
            .unwrap();
        assert_eq!(n1, NodeId(1));
        let n2 = db
            .apply_context_op(
                &lock,
                ApplyContextOp::new(
                    sid(1),
                    project(1),
                    ContextOp::new(channel("tui"), vec![ContextEdit::omit_node(NodeId(1))])
                        .unwrap(),
                    now(),
                ),
            )
            .unwrap();
        assert_eq!(n2, NodeId(2));

        // 退回到 NodeId(1) 合法创建一个独立触发 Step 0（NodeId(3)），再将其父指针篡改挂到
        // 已将全部前置有效条目 OmitNode 清空的 NodeId(2) 下，模拟落盘篡改。
        db.rewind(&lock, NodeId(1), &project(1), now()).unwrap();
        let AcceptedTurn::Accepted(detail) = db
            .trigger_turn(
                &lock,
                TriggerTurn {
                    session_id: sid(1),
                    turn_id: tid(1),
                    project_path: project(1),
                    trigger_channel: channel("tui"),
                    config: config(),
                    accepted_at: now(),
                    settings_version: version,
                    expected_position: None,
                },
            )
            .unwrap()
        else {
            panic!("预期独立触发成功");
        };
        assert_eq!(detail.start_node, NodeId(3));

        db.edit_test_node(
            NodeRef {
                session_id: sid(1),
                node: NodeId(3),
            },
            |wire| {
                wire["parent"] = json!(2);
            },
        )
        .unwrap();
        drop(lock);
        drop(db);

        assert!(matches!(
            SessionStore::open(&layout),
            Err(StoreError::Corruption { ref table, ref key, .. })
                if table == "nodes" && key == &format!("{}/3", sid(1))
        ));
    }
}

#[test]
fn expected_position_guards_all_four_writes_and_inspect_context_reads_any_position() {
    let (_dir, _layout, db) = store();
    let lock = db.try_lock_session(sid(1)).unwrap();
    let version = create_draft(&db, &lock);

    // 1. 草稿会话检视默认与显式根节点均返回空统计，未知节点返回 NotFound。
    {
        let read = db.begin_read().unwrap();
        let (pos, insp) = read.inspect_context(sid(1), None).unwrap();
        assert_eq!(pos, NodeId(0));
        assert!(insp.entries.is_empty());
        assert_eq!(
            read.inspect_context(sid(1), Some(NodeId(0))).unwrap().0,
            NodeId(0)
        );
        assert!(matches!(
            read.inspect_context(sid(1), Some(NodeId(99))),
            Err(StoreError::NotFound { kind: "node", .. })
        ));
        assert!(matches!(
            read.inspect_context(sid(99), None),
            Err(StoreError::NotFound {
                kind: "session",
                ..
            })
        ));
    }

    // 2. 四类写操作在 expected_position 不匹配时均返回 PositionChanged 且不落库。
    assert!(matches!(
        db.feed_input(
            &lock,
            FeedInput {
                session_id: sid(1),
                project_path: project(1),
                channel: channel("cli"),
                input: TurnInput::text("错位投喂".into()).unwrap(),
                accepted_at: now(),
                expected_position: Some(NodeId(1)),
            },
        ),
        Err(StoreError::PositionChanged {
            expected: NodeId(1),
            actual: NodeId(0),
            ..
        })
    ));
    let n1 = db
        .feed_input(
            &lock,
            FeedInput {
                session_id: sid(1),
                project_path: project(1),
                channel: channel("cli"),
                input: TurnInput::text("首条投喂".into()).unwrap(),
                accepted_at: now(),
                expected_position: Some(NodeId(0)),
            },
        )
        .unwrap();
    assert_eq!(n1, NodeId(1));

    assert!(matches!(
        db.apply_context_op(
            &lock,
            ApplyContextOp::new(
                sid(1),
                project(1),
                ContextOp::new(channel("cli"), vec![ContextEdit::omit_node(NodeId(1))]).unwrap(),
                now(),
            )
            .with_expected_position(Some(NodeId(0))),
        ),
        Err(StoreError::PositionChanged {
            expected: NodeId(0),
            actual: NodeId(1),
            ..
        })
    ));

    assert!(matches!(
        db.trigger_turn(
            &lock,
            TriggerTurn {
                session_id: sid(1),
                turn_id: tid(1),
                project_path: project(1),
                trigger_channel: channel("cli"),
                config: config(),
                accepted_at: now(),
                settings_version: version,
                expected_position: Some(NodeId(0)),
            },
        ),
        Err(StoreError::PositionChanged {
            expected: NodeId(0),
            actual: NodeId(1),
            ..
        })
    ));

    let mut wrong_accept = accept(sid(1), tid(2));
    wrong_accept.settings_version = version;
    wrong_accept.expected_position = Some(NodeId(0));
    assert!(matches!(
        db.accept_turn(&lock, wrong_accept),
        Err(StoreError::PositionChanged {
            expected: NodeId(0),
            actual: NodeId(1),
            ..
        })
    ));

    // 3. 正确 expected_position 写入 ContextOp 后，inspect_context 可分别检视当前节点与历史节点。
    let n2 = db
        .apply_context_op(
            &lock,
            ApplyContextOp::new(
                sid(1),
                project(1),
                ContextOp::new(channel("cli"), vec![ContextEdit::omit_node(NodeId(1))]).unwrap(),
                now(),
            )
            .with_expected_position(Some(NodeId(1))),
        )
        .unwrap();
    assert_eq!(n2, NodeId(2));

    let read = db.begin_read().unwrap();
    let (cur_pos, cur_insp) = read.inspect_context(sid(1), None).unwrap();
    assert_eq!(cur_pos, NodeId(2));
    assert!(cur_insp.entries.is_empty());
    assert_eq!(cur_insp.operated_nodes.len(), 1);

    let (old_pos, old_insp) = read.inspect_context(sid(1), Some(NodeId(1))).unwrap();
    assert_eq!(old_pos, NodeId(1));
    assert_eq!(old_insp.entries.len(), 1);
    assert!(old_insp.operated_nodes.is_empty());
}

// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! S3 上下文闭环测试：`feed` / `context_op` / `trigger` / `inspect`、`submit` 多条带通道输入、
//! `expected_position` 冲突、四类写操作统一错误优先级与实时 `watch` 推送。

use super::service_tests::{TestEnv, bounded, configure_new_request};
use super::*;
use lattice_context::{Channel, ContextEdit, ContextOp, NodeStatus};
use lattice_conversation::{ImageRef, MAX_IMAGE_BYTES, MessageSemantic};
use lattice_local::{LocalUpdate, new_session_id, new_turn_id};
use lattice_session::store::{ConversationEntry, NodeId, PublicTurnOutcome};

fn channel(name: &str) -> Channel {
    Channel::parse(name).expect("测试通道名应合法")
}

fn configure_local_with_models(project: &std::path::Path, endpoint: &str, models_toml: &str) {
    super::super::tests::configure(project, endpoint);
    let providers_path = project.join(".lattice/providers.toml");
    let mut contents = std::fs::read_to_string(&providers_path).expect("读取 providers.toml");
    contents.push('\n');
    contents.push_str(models_toml);
    std::fs::write(&providers_path, contents).expect("写入 providers.toml");
}

#[tokio::test]
async fn feed_inspect_context_op_trigger_and_watch_push_end_to_end() {
    let env = TestEnv::start().await;
    let provider = tests::Provider::start().await;
    configure_local_with_models(
        env.dir.path(),
        &provider.endpoint,
        "[providers.local.models.\"local-test\"]\ninput = [\"text\", \"image\"]\n",
    );
    let workspace = env.workspace();
    let sid = new_session_id();
    env.client
        .configure(configure_new_request(&workspace, sid))
        .await
        .expect("创建测试会话应成功");

    let mut watcher = env.client.watch(sid).await.expect("建立 watch 应成功");
    let Some(ViewUpdate::Head(initial_head)) = bounded(watcher.next()).await.unwrap() else {
        panic!("首帧必须是 SessionHead");
    };
    assert_eq!(initial_head.position, NodeId(0));
    assert_eq!(
        initial_head.has_older,
        Some(false),
        "空会话初始窗口头 has_older 应为 Some(false)"
    );

    // 1. 空上下文上独立 trigger 必须返回 BadRequest 且不落盘。
    let empty_trigger_err = env
        .client
        .trigger(TriggerRequest {
            turn_id: new_turn_id(),
            session: sid,
            workspace: workspace.clone(),
            trigger_channel: channel("cli"),
            expected_settings: None,
            expected_position: Some(NodeId(0)),
        })
        .await
        .expect_err("空有效上下文上独立 trigger 必须被拒绝");
    assert_eq!(
        empty_trigger_err.kind,
        LocalErrorKind::BadRequest,
        "空上下文 trigger 必须返回 BadRequest"
    );

    // 2. feed 追加一条输入，观察者必须实时收到 Head（has_older == None）与 Input 条目。
    let feed1 = env
        .client
        .feed(FeedRequest {
            session: sid,
            workspace: workspace.clone(),
            channel: channel("cli"),
            parts: vec![UserInputPart::Text {
                text: "第一条背景输入".into(),
            }],
            expected_position: Some(NodeId(0)),
        })
        .await
        .expect("feed 应成功");
    assert_eq!(feed1.session_id, sid);
    assert_eq!(feed1.node, NodeId(1));

    let feed_head = loop {
        match bounded(watcher.next()).await.unwrap() {
            Some(ViewUpdate::Head(head)) if head.position == NodeId(1) => break head,
            Some(ViewUpdate::Head(head)) if head.position == NodeId(0) => continue,
            other => panic!("feed 后观察者应收到 NodeId(1) 的 SessionHead，实际收到：{other:?}"),
        }
    };
    assert_eq!(feed_head.position, NodeId(1));
    assert_eq!(feed_head.turn_id, None);
    let Some(ViewUpdate::Entry { entry }) = bounded(watcher.next()).await.unwrap() else {
        panic!("feed 后观察者应收到 Input 条目");
    };
    assert!(
        matches!(
            entry.as_ref(),
            ConversationEntry::Input {
                node: NodeId(1),
                ..
            }
        ),
        "feed 推送条目必须为 NodeId(1) 的 Input"
    );

    // 3. 附带新图片执行 context_op ReplaceInput，并核验 watch 推送 ContextOp 与 inspect 结果。
    let replacement_png = b"\x89PNG\r\n\x1a\nreplace-input-image".to_vec();
    let img_ref = ImageRef::from_bytes(&replacement_png).expect("构造图片引用");
    let op = ContextOp::new(
        channel("tui"),
        vec![
            ContextEdit::replace_input(
                NodeId(1),
                vec![
                    MessageSemantic::Text {
                        text: "替换后的图文背景".into(),
                    },
                    MessageSemantic::Image { image: img_ref },
                ],
            )
            .expect("ReplaceInput 编辑应合法"),
        ],
    )
    .expect("ContextOp 应合法");
    let op_reply = env
        .client
        .context_op(ContextOpRequest {
            session: sid,
            workspace: workspace.clone(),
            op,
            images: vec![ContextImage {
                image: replacement_png,
            }],
            expected_position: Some(NodeId(1)),
        })
        .await
        .expect("带新图片的 context_op 应成功");
    assert_eq!(op_reply.node, NodeId(2));

    let Some(ViewUpdate::Head(op_head)) = bounded(watcher.next()).await.unwrap() else {
        panic!("context_op 后观察者应收到 SessionHead");
    };
    assert_eq!(op_head.position, NodeId(2));
    let Some(ViewUpdate::Entry { entry: op_entry }) = bounded(watcher.next()).await.unwrap() else {
        panic!("context_op 后观察者应收到 ContextOp 条目");
    };
    assert!(
        matches!(
            op_entry.as_ref(),
            ConversationEntry::ContextOp {
                node: NodeId(2),
                ..
            }
        ),
        "context_op 推送条目必须为 NodeId(2) 的 ContextOp"
    );

    // 4. inspect 分别核对当前位置（None）与历史节点（NodeId(1)）。
    let inspected_now = env
        .client
        .inspect(sid, None)
        .await
        .expect("inspect 当前位置应成功");
    assert_eq!(inspected_now.position, NodeId(2));
    assert_eq!(inspected_now.inspection.images, vec![img_ref]);
    assert_eq!(inspected_now.inspection.operated_nodes.len(), 1);
    assert_eq!(
        inspected_now.inspection.operated_nodes[0].status,
        NodeStatus::Modified
    );
    let inspected_old = env
        .client
        .inspect(sid, Some(NodeId(1)))
        .await
        .expect("inspect 历史节点应成功");
    assert_eq!(inspected_old.position, NodeId(1));
    assert!(
        inspected_old.inspection.images.is_empty(),
        "历史节点 NodeId(1) 尚无图片"
    );

    // 5. 通过 LocalClient::start_trigger 触发并流式跟随独立轮次。
    let trigger_id = new_turn_id();
    let mut handle = env.client.start_trigger(TriggerRequest {
        turn_id: trigger_id,
        session: sid,
        workspace: workspace.clone(),
        trigger_channel: channel("cli"),
        expected_settings: None,
        expected_position: Some(NodeId(2)),
    });
    let mut saw_accepted = false;
    let mut saw_step = false;
    let mut saw_input_or_op = false;
    while let Some(update) = bounded(handle.next()).await {
        match update {
            LocalUpdate::Submit {
                update:
                    SubmissionUpdate::Receipt(SubmissionReceipt {
                        result: SubmissionResult::Accepted { turn },
                        ..
                    }),
            } => {
                assert_eq!(turn.turn_id, trigger_id);
                assert!(
                    turn.input_nodes.is_empty(),
                    "独立 trigger 的 input_nodes 必须为空"
                );
                saw_accepted = true;
            }
            LocalUpdate::Entry { entry } => match entry.as_ref() {
                ConversationEntry::Step { step, .. } => {
                    assert_eq!(step.key.turn_id, trigger_id);
                    saw_step = true;
                }
                ConversationEntry::Input { .. } | ConversationEntry::ContextOp { .. } => {
                    saw_input_or_op = true;
                }
            },
            _ => {}
        }
    }
    assert!(saw_accepted, "start_trigger 必须收到 Accepted 回执");
    assert!(saw_step, "start_trigger 必须收到本轮 Step 条目");
    assert!(
        !saw_input_or_op,
        "单轮跟随模式不得交付 Input 或 ContextOp 条目"
    );
    let report = bounded(handle.detach()).await;
    assert_eq!(
        report.summary.expect("应有终态摘要").outcome,
        PublicTurnOutcome::Completed
    );

    // 6. 重放已提交的 trigger TurnId 必须返回 Conflict(TurnIdUsed)（ADR 0008 §7）。
    let reuse_err = env
        .client
        .trigger(TriggerRequest {
            turn_id: trigger_id,
            session: sid,
            workspace: std::path::PathBuf::from("relative/invalid"),
            trigger_channel: channel("cli"),
            expected_settings: None,
            expected_position: Some(NodeId(999)),
        })
        .await
        .expect_err("重用已提交的 trigger TurnId 必须返回 TurnIdUsed");
    assert_eq!(reuse_err.kind, LocalErrorKind::Conflict);
    assert_eq!(
        reuse_err.conflict,
        Some(ConflictReason::TurnIdUsed),
        "TurnIdUsed 必须优先于无效工作区和过期的 expected_position"
    );

    drop(watcher);
    assert_eq!(provider.finish().await.len(), 1);
    env.shutdown().await;
}

#[tokio::test]
async fn submit_multi_channel_inputs_writes_all_inputs_in_order() {
    let env = TestEnv::start().await;
    let provider = tests::Provider::start().await;
    super::super::tests::configure(env.dir.path(), &provider.endpoint);
    let workspace = env.workspace();
    let sid = new_session_id();
    env.client
        .configure(configure_new_request(&workspace, sid))
        .await
        .expect("创建会话应成功");

    let tid = new_turn_id();
    let accepted = env
        .client
        .submit(TurnRequest {
            turn_id: tid,
            session: sid,
            workspace,
            trigger_channel: channel("cli"),
            inputs: vec![
                TurnInputItem {
                    channel: channel("cli"),
                    parts: vec![UserInputPart::Text {
                        text: "来自 CLI 的指令".into(),
                    }],
                },
                TurnInputItem {
                    channel: channel("tui"),
                    parts: vec![UserInputPart::Text {
                        text: "来自 TUI 的补充".into(),
                    }],
                },
            ],
            expected_settings: None,
            expected_position: Some(NodeId(0)),
        })
        .await
        .expect("多条带通道输入的 submit 应成功");
    let Acceptance::Accepted { turn } = accepted else {
        panic!("预期 Accepted");
    };
    assert_eq!(turn.input_nodes, vec![NodeId(1), NodeId(2)]);
    assert_eq!(turn.start_node, NodeId(3));

    env.wait_idle().await;
    let bodies = provider.finish().await;
    assert_eq!(bodies.len(), 1);
    let messages = bodies[0]["messages"].as_array().expect("messages 数组");
    let user_texts: Vec<&str> = messages
        .iter()
        .filter(|m| m["role"] == "user")
        .filter_map(|m| m["content"].as_str())
        .collect();
    assert_eq!(
        user_texts,
        vec![
            "<lattice-context channel=\"cli\">\n来自 CLI 的指令\n</lattice-context>",
            "<lattice-context channel=\"tui\">\n来自 TUI 的补充\n</lattice-context>",
        ],
        "多条输入必须按顺序以各自通道标签投影发送给模型"
    );
    env.shutdown().await;
}

#[tokio::test]
async fn trigger_precheck_enforces_historical_image_modality_and_budget() {
    let env = TestEnv::start().await;
    let provider = tests::Provider::start().await;
    configure_local_with_models(
        env.dir.path(),
        &provider.endpoint,
        "[providers.local.models.\"text-only\"]\ninput = [\"text\"]\n\
         [providers.local.models.\"vision\"]\ninput = [\"text\", \"image\"]\n",
    );
    let workspace = env.workspace();

    // 1. 纯文本模型会话经 feed 写入图片后，独立 trigger 必须因图片模态不兼容在落盘前被拒。
    let sid_text = new_session_id();
    let mut cfg = configure_new_request(&workspace, sid_text);
    cfg.model = Some(ModelRef::parse("local/text-only").expect("模型引用"));
    env.client
        .configure(cfg)
        .await
        .expect("创建纯文本会话应成功");
    let small_png = b"\x89PNG\r\n\x1a\nmodality-test".to_vec();
    env.client
        .feed(FeedRequest {
            session: sid_text,
            workspace: workspace.clone(),
            channel: channel("cli"),
            parts: vec![UserInputPart::Image { image: small_png }],
            expected_position: Some(NodeId(0)),
        })
        .await
        .expect("feed 含图输入应成功");

    let mod_err = env
        .client
        .trigger(TriggerRequest {
            turn_id: new_turn_id(),
            session: sid_text,
            workspace: workspace.clone(),
            trigger_channel: channel("cli"),
            expected_settings: None,
            expected_position: Some(NodeId(1)),
        })
        .await
        .expect_err("纯文本模型上存在历史图片时 trigger 必须被拒绝");
    assert_eq!(mod_err.kind, LocalErrorKind::BadRequest);
    assert!(
        mod_err.message.contains("不支持图片"),
        "应报告模型不支持图片：{mod_err}"
    );

    // 2. 视觉模型会话经多次 feed 累计超 32 MiB 后，trigger 必须在落盘前因本地预算超限被拒。
    let sid_budget = new_session_id();
    let mut cfg_v = configure_new_request(&workspace, sid_budget);
    cfg_v.model = Some(ModelRef::parse("local/vision").expect("模型引用"));
    env.client
        .configure(cfg_v)
        .await
        .expect("创建视觉会话应成功");
    let mut big_png = vec![0xCD; MAX_IMAGE_BYTES];
    big_png[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
    for _ in 0..4 {
        env.client
            .feed(FeedRequest {
                session: sid_budget,
                workspace: workspace.clone(),
                channel: channel("cli"),
                parts: vec![
                    UserInputPart::Image {
                        image: big_png.clone(),
                    },
                    UserInputPart::Image {
                        image: big_png.clone(),
                    },
                ],
                expected_position: None,
            })
            .await
            .expect("feed 双大图应成功");
    }
    let budget_err = env
        .client
        .trigger(TriggerRequest {
            turn_id: new_turn_id(),
            session: sid_budget,
            workspace,
            trigger_channel: channel("cli"),
            expected_settings: None,
            expected_position: None,
        })
        .await
        .expect_err("累计图片超 32 MiB 时 trigger 必须在落盘前被拒");
    assert_eq!(budget_err.kind, LocalErrorKind::BadRequest);
    assert!(
        budget_err.message.contains("32 MiB"),
        "应报告超过 32 MiB 本地请求体预算：{budget_err}"
    );

    assert_eq!(
        provider.finish_stats().await.request_count(),
        0,
        "预检被拒的 trigger 不得发起任何上游 HTTP 请求"
    );
    env.shutdown().await;
}

#[tokio::test]
async fn write_ops_error_priority_and_concurrent_inspect_during_active_turn() {
    let env = TestEnv::start().await;
    let mut provider = tests::Provider::start().await;
    provider.release.send_replace(false);
    super::super::tests::configure(env.dir.path(), &provider.endpoint);
    let workspace = env.workspace();
    let sid = new_session_id();
    let created = env
        .client
        .configure(configure_new_request(&workspace, sid))
        .await
        .expect("创建会话应成功");

    // 1. 先检查空闲会话下四类写操作的 expected_position 冲突与 context_op 校验。
    let pos_feed = env
        .client
        .feed(FeedRequest {
            session: sid,
            workspace: workspace.clone(),
            channel: channel("cli"),
            parts: vec![UserInputPart::Text { text: "a".into() }],
            expected_position: Some(NodeId(99)),
        })
        .await
        .expect_err("过期 expected_position 的 feed 必须返回 PositionChanged");
    assert_eq!(pos_feed.conflict, Some(ConflictReason::PositionChanged));

    let pos_op = env
        .client
        .context_op(ContextOpRequest {
            session: sid,
            workspace: workspace.clone(),
            op: ContextOp::new(channel("cli"), vec![ContextEdit::omit_node(NodeId(1))]).unwrap(),
            images: Vec::new(),
            expected_position: Some(NodeId(99)),
        })
        .await
        .expect_err("过期 expected_position 的 context_op 必须返回 PositionChanged");
    assert_eq!(pos_op.conflict, Some(ConflictReason::PositionChanged));

    let pos_submit = env
        .client
        .submit(TurnRequest::single(
            new_turn_id(),
            sid,
            workspace.clone(),
            channel("cli"),
            vec![UserInputPart::Text { text: "b".into() }],
            None,
            Some(NodeId(99)),
        ))
        .await
        .expect_err("过期 expected_position 的 submit 必须返回 PositionChanged");
    assert_eq!(pos_submit.conflict, Some(ConflictReason::PositionChanged));

    let pos_trigger = env
        .client
        .trigger(TriggerRequest {
            turn_id: new_turn_id(),
            session: sid,
            workspace: workspace.clone(),
            trigger_channel: channel("cli"),
            expected_settings: None,
            expected_position: Some(NodeId(99)),
        })
        .await
        .expect_err("过期 expected_position 的 trigger 必须返回 PositionChanged");
    assert_eq!(pos_trigger.conflict, Some(ConflictReason::PositionChanged));

    // context_op 携带重复图片摘要或非法上下文目标时返回 BadRequest。
    let dup_png = b"\x89PNG\r\n\x1a\ndup".to_vec();
    let dup_err = env
        .client
        .context_op(ContextOpRequest {
            session: sid,
            workspace: workspace.clone(),
            op: ContextOp::new(channel("cli"), vec![ContextEdit::omit_node(NodeId(1))]).unwrap(),
            images: vec![
                ContextImage {
                    image: dup_png.clone(),
                },
                ContextImage { image: dup_png },
            ],
            expected_position: None,
        })
        .await
        .expect_err("重复图片摘要的 context_op 必须返回 BadRequest");
    assert_eq!(dup_err.kind, LocalErrorKind::BadRequest);

    let unknown_node_err = env
        .client
        .context_op(ContextOpRequest {
            session: sid,
            workspace: workspace.clone(),
            op: ContextOp::new(channel("cli"), vec![ContextEdit::omit_node(NodeId(77))]).unwrap(),
            images: Vec::new(),
            expected_position: Some(NodeId(0)),
        })
        .await
        .expect_err("目标节点不存在的 context_op 必须返回 BadRequest");
    assert_eq!(unknown_node_err.kind, LocalErrorKind::BadRequest);

    // 2. 启动一轮阻塞中的 Turn，核验活跃 Owner 期间：
    //    (a) inspect 只读不返回 Busy；
    //    (b) 四类写操作同会话 Busy 严格优先于输入非法 BadRequest / SettingsChanged / PositionChanged；
    //    (c) 复用在途 TurnId 返回 TurnIdUsed（优先于 Busy / 无效工作区）。
    let active_turn_id = new_turn_id();
    env.client
        .submit(TurnRequest::single(
            active_turn_id,
            sid,
            workspace.clone(),
            channel("cli"),
            vec![UserInputPart::Text {
                text: "占住会话执行".into(),
            }],
            Some(created.head.settings_version),
            Some(NodeId(0)),
        ))
        .await
        .expect("阻塞轮次提交应被接受");
    bounded(&mut provider.arrived).await.unwrap();

    // (a) inspect 并发可读。
    let live_inspect = env
        .client
        .inspect(sid, None)
        .await
        .expect("活跃 Turn 期间 inspect 必须只读成功而不返回 Busy");
    assert_eq!(live_inspect.inspection.entries.len(), 1);

    // (b) 四类写操作带非法输入/过期设置/过期位置时，均必须先报 Busy！
    let stale_settings: lattice_session::store::SettingsVersion =
        serde_json::from_value(serde_json::json!("ff".repeat(32))).unwrap();

    let busy_submit = env
        .client
        .submit(TurnRequest {
            turn_id: new_turn_id(),
            session: sid,
            workspace: workspace.clone(),
            trigger_channel: channel("cli"),
            inputs: Vec::new(), // 非法空输入
            expected_settings: Some(stale_settings),
            expected_position: Some(NodeId(99)),
        })
        .await
        .expect_err("活跃会话上的 submit 必须优先返回 Busy");
    assert_eq!(
        busy_submit.kind,
        LocalErrorKind::Busy,
        "submit 同会话 Busy 必须优先于空输入 BadRequest / SettingsChanged / PositionChanged"
    );

    let busy_trigger = env
        .client
        .trigger(TriggerRequest {
            turn_id: new_turn_id(),
            session: sid,
            workspace: workspace.clone(),
            trigger_channel: channel("cli"),
            expected_settings: Some(stale_settings),
            expected_position: Some(NodeId(99)),
        })
        .await
        .expect_err("活跃会话上的 trigger 必须优先返回 Busy");
    assert_eq!(
        busy_trigger.kind,
        LocalErrorKind::Busy,
        "trigger 同会话 Busy 必须优先于 SettingsChanged / PositionChanged"
    );

    let busy_feed = env
        .client
        .feed(FeedRequest {
            session: sid,
            workspace: workspace.clone(),
            channel: channel("cli"),
            parts: Vec::new(), // 非法空输入
            expected_position: Some(NodeId(99)),
        })
        .await
        .expect_err("活跃会话上的 feed 必须优先返回 Busy");
    assert_eq!(
        busy_feed.kind,
        LocalErrorKind::Busy,
        "feed 同会话 Busy 必须优先于空输入 BadRequest 与 PositionChanged"
    );

    let busy_op = env
        .client
        .context_op(ContextOpRequest {
            session: sid,
            workspace: workspace.clone(),
            op: ContextOp::new(channel("cli"), vec![ContextEdit::omit_node(NodeId(77))]).unwrap(),
            images: vec![ContextImage {
                image: b"not-a-valid-image".to_vec(), // 非法图片字节
            }],
            expected_position: Some(NodeId(99)),
        })
        .await
        .expect_err("活跃会话上的 context_op 必须优先返回 Busy");
    assert_eq!(
        busy_op.kind,
        LocalErrorKind::Busy,
        "context_op 同会话 Busy 必须优先于非法图片 BadRequest 与 PositionChanged"
    );

    // 即使会话活跃，工作区不匹配仍排在同会话 Busy 之前（BindingMismatch）。
    let other_ws = tempfile::tempdir().expect("创建其他工作区");
    let other_ws_path = LocalWorkspaceRoot::from_path(other_ws.path())
        .unwrap()
        .canonical_path()
        .to_path_buf();
    let bind_feed = env
        .client
        .feed(FeedRequest {
            session: sid,
            workspace: other_ws_path.clone(),
            channel: channel("cli"),
            parts: vec![UserInputPart::Text { text: "x".into() }],
            expected_position: None,
        })
        .await
        .expect_err("错误工作区的 feed 必须先报 BindingMismatch");
    assert_eq!(bind_feed.conflict, Some(ConflictReason::BindingMismatch));

    let bind_op = env
        .client
        .context_op(ContextOpRequest {
            session: sid,
            workspace: other_ws_path,
            op: ContextOp::new(channel("cli"), vec![ContextEdit::omit_node(NodeId(1))]).unwrap(),
            images: Vec::new(),
            expected_position: None,
        })
        .await
        .expect_err("错误工作区的 context_op 必须先报 BindingMismatch");
    assert_eq!(bind_op.conflict, Some(ConflictReason::BindingMismatch));

    // (c) 复用在途 TurnId 返回 TurnIdUsed。
    let inflight_dup = env
        .client
        .trigger(TriggerRequest {
            turn_id: active_turn_id,
            session: sid,
            workspace: std::path::PathBuf::from("relative"),
            trigger_channel: channel("cli"),
            expected_settings: None,
            expected_position: None,
        })
        .await
        .expect_err("复用在途 TurnId 必须立即返回 TurnIdUsed");
    assert_eq!(inflight_dup.conflict, Some(ConflictReason::TurnIdUsed));

    provider.release.send_replace(true);
    env.wait_idle().await;
    assert_eq!(provider.finish().await.len(), 1);
    env.shutdown().await;
}

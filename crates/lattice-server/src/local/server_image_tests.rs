// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::service_tests::{TestEnv, bounded, configure_new_request, turn_request};
use super::*;
use lattice_conversation::MAX_IMAGE_BYTES;
use lattice_local::{new_session_id, new_turn_id};
use lattice_session::store::{NodeKind, TurnInput, TurnInputPart};

const BUDGET_EXCEEDED_MESSAGE: &str = "编码后的模型请求超过 LATTICE 本地请求体预算（32 MiB），请减少图片、执行 lattice session rewind 退回较早节点或新建会话";

/// 经同一后台的 `SessionApi`、`RequestGateway` 与当前会话设置构造 `PreparedModel`，
/// 再通过 `start_execution` 走真实网关准入预检与执行路径。
async fn submit_with_images_via_gateway(
    env: &TestEnv,
    session_id: SessionId,
    turn_id: TurnId,
    input: TurnInput,
) -> (
    std::result::Result<TurnAcceptance, ApiError>,
    std::result::Result<TurnCompletion, ApiError>,
) {
    let api = env.backend.open().await.expect("打开测试会话库应成功");
    let io = CoordinatorStoreIo::acquire_api(api.clone(), session_id)
        .await
        .map_err(ApiError::from)
        .expect("获取会话执行锁应成功");
    let (settings, settings_version) = io
        .read(move |read| {
            read.session(session_id)?;
            let settings = read.session_settings(session_id)?;
            let version = settings.version()?;
            Ok((settings, version))
        })
        .await
        .map_err(ApiError::from)
        .expect("读取测试会话设置应成功");
    let gateway = env
        .backend
        .gateway
        .as_ref()
        .expect("测试后台应已构造请求网关");
    let prepared_model =
        match crate::turn::execute::PreparedModel::resolve(gateway, &settings).await {
            Ok(model) => model,
            Err(error) => return (Err(error.clone()), Err(error)),
        };
    let workspace = LocalWorkspaceRoot::from_path(&env.workspace()).expect("解析测试工作区应成功");
    let shutdown = api.shutdown();
    let execution = crate::turn::start_execution(crate::turn::execute::ExecutionInput {
        session: api,
        turn_id,
        workspace,
        request: RunTurnRequest {
            input,
            session_id,
            expected_settings: Some(settings_version),
            cancel_flag: None,
            tools: Some(env.backend.tools.clone()),
            #[cfg(any(test, feature = "test-utils"))]
            test_llm: None,
        },
        prepared_model: Some(prepared_model),
        prepared_settings: Some((settings, settings_version)),
        io: Some(io),
        shutdown,
        observer: Arc::default(),
    });
    let acceptance = bounded(execution.acceptance)
        .await
        .expect("等待接受回执不应超时或关闭通道");
    let completion = bounded(execution.completion)
        .await
        .expect("等待完成结果不应超时或关闭通道");
    (acceptance, completion)
}

fn configure_local_with_models(project: &std::path::Path, endpoint: &str, models_toml: &str) {
    super::super::tests::configure(project, endpoint);
    let providers_path = project.join(".lattice/providers.toml");
    let mut contents = std::fs::read_to_string(&providers_path).expect("读取测试 providers.toml");
    contents.push('\n');
    contents.push_str(models_toml);
    std::fs::write(&providers_path, contents).expect("写入带模型输入能力的 providers.toml");
}

fn encode_base64_standard(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((triple >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((triple >> 12) & 0x3f) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[((triple >> 6) & 0x3f) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(triple & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[tokio::test]
async fn image_input_is_rejected_before_acceptance_for_text_model() {
    let env = TestEnv::start().await;
    let provider = tests::Provider::start().await;
    configure_local_with_models(
        env.dir.path(),
        &provider.endpoint,
        "[providers.local.models.\"local-test\"]\ninput = [\"text\"]\n",
    );
    let workspace = env.workspace();
    let sid = new_session_id();
    env.client
        .configure(configure_new_request(&workspace, sid))
        .await
        .expect("创建纯文本模型会话应成功");
    let before_log = env.client.log(sid).await.expect("读取接受前会话日志");

    let turn_id = new_turn_id();
    let png = b"\x89PNG\r\n\x1a\ntext-only-rejected".to_vec();
    let input = TurnInput::new(vec![
        TurnInputPart::Text {
            text: "请看这张图".into(),
        },
        TurnInputPart::Image { bytes: png },
    ])
    .expect("构造测试含图输入");

    let (acceptance, completion) = submit_with_images_via_gateway(&env, sid, turn_id, input).await;
    let Err(ApiError::Configuration(rejection)) = acceptance else {
        panic!("纯文本模型遇到含图输入必须在接受前返回配置拒绝");
    };
    let local_err = LocalError::from(ApiError::Configuration(rejection));
    assert_eq!(local_err.kind, LocalErrorKind::BadRequest);
    assert!(
        local_err.message.contains("local/local-test") && local_err.message.contains("不支持图片"),
        "拒绝文案应指出不支持图片的模型：{local_err}"
    );
    assert!(
        matches!(completion, Err(ApiError::Configuration(_))),
        "完成通道应返回相同的配置拒绝"
    );

    env.wait_idle().await;
    let after_log = env.client.log(sid).await.expect("读取拒绝后会话日志");
    assert_eq!(after_log.session.current, before_log.session.current);
    assert_eq!(after_log.nodes.len(), before_log.nodes.len());
    assert_eq!(after_log.nodes, before_log.nodes);
    assert_eq!(after_log.session.updated_at, before_log.session.updated_at);
    assert_eq!(
        env.client
            .turn(turn_id)
            .await
            .expect_err("被拒 TurnId 不得写入会话库")
            .kind,
        LocalErrorKind::NotFound
    );
    let api = env.backend.open().await.expect("打开会话库核对 blobs");
    assert_eq!(
        api.read(|read| read.blob_count())
            .await
            .expect("读取 blobs 计数"),
        0,
        "准入前被拒不得写入 blobs 表"
    );
    drop(api);

    assert_eq!(
        provider.finish_stats().await.request_count(),
        0,
        "纯文本模型拒绝含图输入时不得向 provider 发送请求"
    );
    env.shutdown().await;
}

#[tokio::test]
async fn historical_images_count_toward_request_budget_before_acceptance() {
    let env = TestEnv::start().await;
    let provider = tests::Provider::start_with(Some(64 * 1024 * 1024), false).await;
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
        .expect("创建视觉模型会话应成功");

    let mut max_png = vec![0xAB; MAX_IMAGE_BYTES];
    max_png[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");

    // 前三轮每轮提交两张同一大图（共 6 张、约 30 MiB base64），每轮都在单轮上限与 32 MiB 预算内。
    for round in 1..=3 {
        let turn_id = new_turn_id();
        let input = TurnInput::new(vec![
            TurnInputPart::Image {
                bytes: max_png.clone(),
            },
            TurnInputPart::Image {
                bytes: max_png.clone(),
            },
        ])
        .expect("构造单轮双图输入");
        let (acceptance, completion) =
            submit_with_images_via_gateway(&env, sid, turn_id, input).await;
        assert!(
            matches!(acceptance, Ok(TurnAcceptance::Accepted { .. })),
            "第 {round} 轮未超预算应被接受：{acceptance:?}"
        );
        let Ok(TurnCompletion::Closed(detail)) = completion else {
            panic!("第 {round} 轮应正常结束：{completion:?}");
        };
        assert_eq!(
            detail.summary.outcome,
            PublicTurnOutcome::Completed,
            "第 {round} 轮应正常完成"
        );
    }

    let before_fourth = env.client.log(sid).await.expect("读取第四轮前会话日志");
    assert_eq!(before_fourth.nodes.len(), 7, "前三轮后整棵树应有 7 个节点");

    // 第四轮再提交一张图（累计第 7 张、约 35 MiB base64），在接受前触发 32 MiB 本地请求体预算拒绝。
    let fourth_turn = new_turn_id();
    let fourth_input =
        TurnInput::new(vec![TurnInputPart::Image { bytes: max_png }]).expect("构造第四轮单图输入");
    let (acceptance, completion) =
        submit_with_images_via_gateway(&env, sid, fourth_turn, fourth_input).await;

    let Err(budget_err) = acceptance else {
        panic!("累计超过 32 MiB 本地预算的第四轮必须在接受前被拒绝：{acceptance:?}");
    };
    assert!(
        matches!(&budget_err, ApiError::RequestBudgetExceeded(msg) if msg == BUDGET_EXCEEDED_MESSAGE),
        "第四轮必须以 RequestBudgetExceeded 且 ADR 5.3 原文拒绝：{budget_err:?}"
    );
    let local_err = LocalError::from(budget_err);
    assert_eq!(
        local_err.kind,
        LocalErrorKind::BadRequest,
        "RequestBudgetExceeded 转 LocalError 后必须为 BadRequest"
    );
    assert_eq!(
        local_err.message, BUDGET_EXCEEDED_MESSAGE,
        "RequestBudgetExceeded 转 LocalError 后必须保留 ADR 5.3 原文"
    );
    assert!(
        matches!(completion, Err(ApiError::RequestBudgetExceeded(_))),
        "完成通道应返回相同的超预算拒绝"
    );

    env.wait_idle().await;
    let after_fourth = env.client.log(sid).await.expect("读取第四轮被拒后日志");
    assert_eq!(
        after_fourth, before_fourth,
        "第四轮被拒后整棵树、current 与 updated_at 都必须保持不变"
    );
    assert_eq!(
        env.client
            .turn(fourth_turn)
            .await
            .expect_err("超预算被拒的第四轮不得记录 TurnId")
            .kind,
        LocalErrorKind::NotFound
    );

    let api = env.backend.open().await.expect("打开会话库核对 blobs");
    assert_eq!(
        api.read(|read| read.blob_count())
            .await
            .expect("读取 blobs 条目数"),
        1,
        "同一张大图多次引用时 blobs 表必须只有 1 个条目"
    );
    drop(api);

    let stats = provider.finish_stats().await;
    assert_eq!(
        stats.request_count(),
        3,
        "前三轮共发 3 次 HTTP 请求，第四轮不得发起 HTTP 请求"
    );
    env.shutdown().await;
}

#[tokio::test]
async fn configure_to_text_model_rejects_image_history_until_rewind() {
    let env = TestEnv::start().await;
    let provider = tests::Provider::start().await;
    configure_local_with_models(
        env.dir.path(),
        &provider.endpoint,
        "[providers.local.models.\"vision-model\"]\ninput = [\"text\", \"image\"]\n\
         [providers.local.models.\"text-model\"]\ninput = [\"text\"]\n",
    );
    let workspace = env.workspace();
    let sid = new_session_id();
    let vision_model = ModelRef::parse("local/vision-model").expect("解析视觉模型引用");
    let text_model = ModelRef::parse("local/text-model").expect("解析纯文本模型引用");

    let mut create_req = configure_new_request(&workspace, sid);
    create_req.model = Some(vision_model);
    env.client
        .configure(create_req)
        .await
        .expect("创建视觉模型会话应成功");

    // 1. 先用视觉模型接受并完成一个含图轮次。
    let vision_turn = new_turn_id();
    let png = b"\x89PNG\r\n\x1a\nhistory-image".to_vec();
    let vision_input = TurnInput::new(vec![
        TurnInputPart::Text {
            text: "请记住这张图".into(),
        },
        TurnInputPart::Image { bytes: png },
    ])
    .expect("构造视觉轮次输入");
    let (acceptance, completion) =
        submit_with_images_via_gateway(&env, sid, vision_turn, vision_input).await;
    assert!(matches!(acceptance, Ok(TurnAcceptance::Accepted { .. })));
    let Ok(TurnCompletion::Closed(detail)) = completion else {
        panic!("视觉轮次应正常结束");
    };
    assert_eq!(detail.summary.outcome, PublicTurnOutcome::Completed);

    // 2. 用正式的 configure(Existing) 把模型换成纯文本模型，配置本身必须成功。
    let configured = env
        .client
        .configure(ConfigureRequest {
            session: ConfigureSession::Existing(sid),
            workspace: workspace.clone(),
            agent: None,
            model: Some(text_model),
            effort: None,
        })
        .await
        .expect("在已有含图历史的会话上切换到纯文本模型应成功");
    assert_eq!(
        configured.head.model.as_ref().map(|m| m.as_str()),
        Some("text-model")
    );
    let after_configure_log = env.client.log(sid).await.expect("读取切换模型后日志");
    assert_eq!(after_configure_log.nodes.len(), 3);

    // 3. 紧接着提交纯文本输入，因当前路径历史含图而在接受前被配置拒绝。
    let rejected_turn = turn_request(&workspace, sid, "只发纯文本问题");
    let rejected_id = rejected_turn.turn_id;
    let submit_err = env
        .client
        .submit(rejected_turn)
        .await
        .expect_err("历史含图且当前模型为纯文本时，提交纯文本必须被拒绝");
    assert_eq!(submit_err.kind, LocalErrorKind::BadRequest);
    assert!(
        submit_err.message.contains("local/text-model")
            && submit_err.message.contains("不支持图片"),
        "配置拒绝应指出切换到纯文本模型后历史含图：{submit_err}"
    );
    env.wait_idle().await;
    let after_reject_log = env.client.log(sid).await.expect("读取拒绝后日志");
    assert_eq!(
        after_reject_log, after_configure_log,
        "提交被拒后会话树与 updated_at 必须与 configure 完成之后一致"
    );
    assert_eq!(
        env.client
            .turn(rejected_id)
            .await
            .expect_err("被拒 TurnId 不得记录")
            .kind,
        LocalErrorKind::NotFound
    );

    // 4. 用正式的 rewind 退回根节点后，纯文本提交成功，且原含图分支仍在树上。
    env.client
        .rewind(sid, NodeId(0), workspace.clone())
        .await
        .expect("退回根节点应成功");
    let accepted = env
        .client
        .submit(turn_request(&workspace, sid, "退回后的纯文本提交"))
        .await
        .expect("退回无图根节点后纯文本提交应被接受");
    assert!(matches!(accepted, Acceptance::Accepted { .. }));
    env.wait_idle().await;

    let final_log = env.client.log(sid).await.expect("读取退回续写后的会话日志");
    assert_eq!(
        final_log.nodes.len(),
        5,
        "整棵树应同时保留原含图分支与退回后的纯文本分支（共 5 个节点）"
    );
    assert_eq!(
        final_log.session.current,
        NodeId(4),
        "当前位置应停在退回后新分支的 Step 节点"
    );
    assert!(
        final_log
            .nodes
            .iter()
            .any(|row| row.node.id == NodeId(1) && row.node.kind == NodeKind::Input),
        "原含图分支的 Input 节点必须仍在会话树上"
    );
    assert!(
        final_log
            .nodes
            .iter()
            .any(|row| row.node.id == NodeId(2) && row.node.turn_id == Some(vision_turn)),
        "原含图分支的 Step 0 必须仍在会话树上"
    );
    assert!(
        final_log
            .nodes
            .iter()
            .any(|row| row.node.id == NodeId(3) && row.node.parent == Some(NodeId(0))),
        "退回后新提交的纯文本分支应从根节点分叉"
    );

    assert_eq!(
        provider.finish().await.len(),
        2,
        "provider 应只收到视觉轮次与退回后纯文本轮次共 2 次请求"
    );
    env.shutdown().await;
}

#[tokio::test]
async fn small_interleaved_images_are_sent_as_ordered_data_urls() {
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
        .expect("创建视觉模型会话应成功");

    let png = b"\x89PNG\r\n\x1a\nsmall-png-payload".to_vec();
    let jpeg = b"\xff\xd8\xff\xe0small-jpeg-payload".to_vec();
    let input = TurnInput::new(vec![
        TurnInputPart::Text {
            text: "第一段文字".into(),
        },
        TurnInputPart::Image { bytes: png.clone() },
        TurnInputPart::Text {
            text: "两图之间".into(),
        },
        TurnInputPart::Image {
            bytes: jpeg.clone(),
        },
        TurnInputPart::Text {
            text: "末尾文字".into(),
        },
    ])
    .expect("构造图文交错输入");

    let turn_id = new_turn_id();
    let (acceptance, completion) = submit_with_images_via_gateway(&env, sid, turn_id, input).await;
    assert!(matches!(acceptance, Ok(TurnAcceptance::Accepted { .. })));
    let Ok(TurnCompletion::Closed(detail)) = completion else {
        panic!("小图片交错请求应正常结束");
    };
    assert_eq!(detail.summary.outcome, PublicTurnOutcome::Completed);

    let bodies = provider.finish().await;
    assert_eq!(bodies.len(), 1, "应向 provider 发送 1 次请求");
    let messages = bodies[0]["messages"]
        .as_array()
        .expect("chat-completions 请求必须包含 messages 数组");
    let user_msg = messages
        .iter()
        .find(|msg| msg["role"] == "user")
        .expect("请求中必须包含 user 消息");
    assert_eq!(
        user_msg["content"],
        serde_json::json!([
            { "type": "text", "text": "<lattice-context channel=\"ipc\">\n第一段文字" },
            {
                "type": "image_url",
                "image_url": {
                    "url": format!("data:image/png;base64,{}", encode_base64_standard(&png))
                }
            },
            { "type": "text", "text": "两图之间" },
            {
                "type": "image_url",
                "image_url": {
                    "url": format!("data:image/jpeg;base64,{}", encode_base64_standard(&jpeg))
                }
            },
            { "type": "text", "text": "末尾文字\n</lattice-context>" }
        ]),
        "请求体中的用户消息必须按原顺序包含通道包裹的文本块与 data URL 图片块"
    );
    env.shutdown().await;
}

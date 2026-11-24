// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::{test_support::*, types::TurnEvent};
use lattice_agent::LlmClient;
use lattice_conversation::*;
use lattice_llm::TokenUsage;
use lattice_llm::testing::ScriptEvent;
use lattice_session::store::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[tokio::test]
async fn run_turn_result_and_saved_nodes_preserve_contract_fields() {
    let fixture = setup_with_events(happy_events(), &[]);
    let result = super::execute::run_turn(
        fixture.api.clone(),
        fixture.root.clone(),
        fixture.request.clone(),
    )
    .await
    .unwrap();
    assert_eq!(result.summary.outcome, PublicTurnOutcome::Completed);
    assert_eq!(
        result.usage,
        UsageSummary::from_attempts([(
            Some(TokenUsage {
                input: Some(10),
                output: Some(5),
                ..Default::default()
            }),
            lattice_conversation::UsageCompleteness::Partial,
            None
        )])
    );
    let read = fixture.store.begin_read().unwrap();
    assert_eq!(read.list(None, 10).unwrap().items.len(), 1);
    let detail = read
        .turn_detail(result.turn.session_id, result.turn.turn_id)
        .unwrap();
    assert_eq!(detail.steps, result.steps);
    assert_eq!(
        serde_json::to_value(&detail.steps[0].attempts[0].origin).unwrap(),
        serde_json::json!({"protocol":"chat-completions","provider_id":"local"})
    );
    let Node::Step {
        turn_start: Some(meta),
        ..
    } = read
        .node(NodeRef {
            session_id: result.turn.session_id,
            node: detail.start_node,
        })
        .unwrap()
    else {
        panic!("预期读取到带 turn_start 的 Step 0 节点")
    };
    assert_eq!(meta.provider, "local");
    assert_eq!(detail.input_nodes, vec![NodeId(1)]);
    assert_eq!(detail.start_node, NodeId(2));
    assert_eq!(read.log(result.turn.session_id).unwrap().nodes.len(), 3);
    let node = read
        .node(NodeRef {
            session_id: result.turn.session_id,
            node: NodeId(2),
        })
        .unwrap();
    let content = node.step_content().expect("complete reply");
    assert!(
        matches!(&content.assistant.parts[0].semantic, MessageSemantic::Text { text } if text == "test response")
    );
    assert!(content.results.is_empty());
}

#[tokio::test]
async fn provider_failure_preserves_notice_in_step_without_partial_reply() {
    let fixture = setup_with_events(
        vec![
            ScriptEvent::Text {
                content: "partial".into(),
            },
            ScriptEvent::Failure {
                failure: lattice_llm::ProviderFailure {
                    message: "provider unavailable".into(),
                    retryable: false,
                },
            },
        ],
        &[],
    );
    let (detail, events) = fixture.run(fixture.request.clone()).await;
    assert_eq!(detail.summary.outcome, PublicTurnOutcome::Failed);
    assert_eq!(
        serde_json::to_value(&detail.steps[0].attempts[0].origin).unwrap(),
        serde_json::json!({"protocol":"chat-completions","provider_id":"local"})
    );
    assert_eq!(
        detail.summary.notice.as_ref().unwrap().text,
        "provider unavailable"
    );
    assert!(
        matches!(&events.last().unwrap(), TurnEvent::TurnFailed { summary, step, .. } if summary == &detail.summary && step == &detail.steps[0])
    );
    assert_eq!(
        fixture
            .store
            .begin_read()
            .unwrap()
            .log(detail.session_id)
            .unwrap()
            .nodes
            .len(),
        3
    );
    assert_eq!(
        *detail.usage,
        UsageSummary::from_attempts([(
            None,
            lattice_conversation::UsageCompleteness::Partial,
            None
        )])
    );
}

struct CancelOnDone(Arc<AtomicBool>);
#[async_trait::async_trait]
impl LlmClient for CancelOnDone {
    async fn stream_chat(
        &self,
        request: lattice_llm::ChatRequest,
        _images: Arc<dyn lattice_llm::ImageSource>,
        control: lattice_llm::StreamControl,
    ) -> std::result::Result<lattice_llm::LlmStream, Box<dyn std::error::Error + Send + Sync>> {
        let cancel = self.0.clone();
        let message = NativeMessage::new(
            MessageRole::Assistant,
            Some(lattice_conversation::ReplayOrigin {
                protocol: Protocol::ChatCompletions,
                provider_id: lattice_conversation::ProviderId::parse("local").unwrap(),
            }),
            vec![MessagePart::semantic_only(MessageSemantic::ToolCall {
                call: UnboundToolCall {
                    provider_call_id: Some("call".into()),
                    name: "observe__workspace".into(),
                    arguments: serde_json::json!({"request": {"mode":"read_file","path":"src/lib.rs"}}),
                },
            })],
        );
        let mut events = lattice_llm::testing::parts_events(message.parts);
        events.push(ScriptEvent::Done {
            finish_reason: "tool_calls".into(),
            usage: Some(TokenUsage {
                input: Some(5),
                output: Some(2),
                ..Default::default()
            }),
        });
        Ok(Box::pin(
            lattice_llm::testing::script_stream(
                ReplayOrigin {
                    protocol: Protocol::ChatCompletions,
                    provider_id: request.target.model.provider().clone(),
                },
                events,
                control,
            )
            .map(move |event| {
                if matches!(event, lattice_llm::LlmEvent::Finished { .. }) {
                    cancel.store(true, Ordering::Release);
                }
                event
            }),
        ))
    }
}
use futures::StreamExt;
#[tokio::test]
async fn completed_attempt_usage_survives_cancellation_before_dispatch() {
    let cancel = Arc::new(AtomicBool::new(false));
    let fixture = setup_fixture(
        Arc::new(CancelOnDone(cancel.clone())),
        &["observe__workspace"],
    );
    let mut request = fixture.request.clone();
    request.cancel_flag = Some(cancel);
    let (detail, _) = fixture.run(request).await;
    assert_eq!(detail.summary.outcome, PublicTurnOutcome::Cancelled);
    assert_eq!(
        *detail.usage,
        UsageSummary::from_attempts([(
            Some(TokenUsage {
                input: Some(5),
                output: Some(2),
                ..Default::default()
            }),
            lattice_conversation::UsageCompleteness::Partial,
            None
        )])
    );
    assert!(matches!(
        &detail.steps[0].progress,
        StepProgress::Closed {
            end: StepEnd::Stop {
                cause: StopCause::Cancelled
            }
        }
    ));
}

#[tokio::test]
async fn run_turn_request_with_images_preserves_order_and_reads_blobs_via_session_images() {
    use std::sync::Mutex;

    struct RecordingImageClient {
        expected: Vec<(ImageRef, Vec<u8>)>,
        seen_user_parts: Mutex<Option<Vec<MessagePart<BoundToolCall>>>>,
        read_blobs: Mutex<Vec<(ImageRef, Vec<u8>)>>,
    }

    #[async_trait::async_trait]
    impl LlmClient for RecordingImageClient {
        async fn stream_chat(
            &self,
            request: lattice_llm::ChatRequest,
            images: Arc<dyn lattice_llm::ImageSource>,
            control: lattice_llm::StreamControl,
        ) -> std::result::Result<lattice_llm::LlmStream, Box<dyn std::error::Error + Send + Sync>>
        {
            let user_parts = request
                .messages
                .iter()
                .find_map(|item| match item {
                    ConversationItem::Message(msg) if msg.role == MessageRole::User => {
                        Some(msg.parts.clone())
                    }
                    _ => None,
                })
                .expect("请求必须包含用户消息");
            *self.seen_user_parts.lock().expect("记录用户消息") = Some(user_parts);

            let expected = self.expected.clone();
            let borrowed = tokio::task::spawn_blocking(move || {
                let mut collected = Vec::new();
                images.with_reader(&mut |reader| {
                    for (image_ref, _) in &expected {
                        reader.with_image(image_ref, &mut |bytes| {
                            collected.push((*image_ref, bytes.to_vec()));
                            Ok(())
                        })?;
                    }
                    Ok(())
                })?;
                Ok::<_, lattice_llm::LlmError>(collected)
            })
            .await
            .expect("阻塞读取线程不得 panic")?;
            *self.read_blobs.lock().expect("记录借出的图片字节") = borrowed;

            Ok(lattice_llm::testing::script_stream(
                ReplayOrigin {
                    protocol: Protocol::ChatCompletions,
                    provider_id: request.target.model.provider().clone(),
                },
                happy_events(),
                control,
            ))
        }
    }

    let png = b"\x89PNG\r\n\x1a\nfirst-image".to_vec();
    let webp = b"RIFF\x24\x00\x00\x00WEBPsecond-image".to_vec();
    let ref_png = ImageRef::from_bytes(&png).expect("构造第一张图片引用");
    let ref_webp = ImageRef::from_bytes(&webp).expect("构造第二张图片引用");

    let client = Arc::new(RecordingImageClient {
        expected: vec![(ref_png, png.clone()), (ref_webp, webp.clone())],
        seen_user_parts: Mutex::new(None),
        read_blobs: Mutex::new(Vec::new()),
    });
    let mut fixture = setup_fixture(client.clone(), &[]);
    let input = TurnInput::new(vec![
        TurnInputPart::Text {
            text: "图一之前".into(),
        },
        TurnInputPart::Image { bytes: png.clone() },
        TurnInputPart::Text {
            text: "两图之间".into(),
        },
        TurnInputPart::Image {
            bytes: webp.clone(),
        },
        TurnInputPart::Text {
            text: "图二之后".into(),
        },
    ])
    .expect("构造图文交错轮次输入");
    let expected_parts = input.message().parts.clone();
    fixture.request.input = input;

    let result = super::execute::run_turn(
        fixture.api.clone(),
        fixture.root.clone(),
        fixture.request.clone(),
    )
    .await
    .expect("提交含图轮次应成功完成");
    assert_eq!(result.summary.outcome, PublicTurnOutcome::Completed);

    // 验证会话日志与节点中的用户消息保留图文顺序及 ImageRef，且 BLOBS 存在对应字节。
    let read = fixture.store.begin_read().expect("打开会话库读事务");
    let log = read.log(result.turn.session_id).expect("读取会话日志");
    assert_eq!(log.nodes.len(), 3);
    let input_node_id = log
        .nodes
        .iter()
        .find(|row| row.node.kind == NodeKind::Input)
        .expect("日志应包含输入节点")
        .node
        .id;
    let Node::Input { input, .. } = read
        .node(NodeRef {
            session_id: result.turn.session_id,
            node: input_node_id,
        })
        .expect("读取输入节点")
    else {
        panic!("期望读取到 Input 节点");
    };
    assert_eq!(
        input.message().parts,
        expected_parts,
        "会话节点输入消息必须保留图文顺序与 ImageRef"
    );
    assert_eq!(
        read.turn_input(result.turn.session_id, result.turn.turn_id)
            .expect("读取轮次输入"),
        expected_parts,
        "turn_input 必须返回完整保序图文片段"
    );
    assert_eq!(
        read.with_blob(&ref_png.sha256, |b| b.to_vec())
            .expect("读取第一张图片字节"),
        png
    );
    assert_eq!(
        read.with_blob(&ref_webp.sha256, |b| b.to_vec())
            .expect("读取第二张图片字节"),
        webp
    );

    // 验证模型客户端收到的请求片段与通过 SessionImages 借出的原始字节一致。
    let seen_parts = client
        .seen_user_parts
        .lock()
        .expect("读取记录的用户消息")
        .clone()
        .expect("必须收到用户消息");
    assert_eq!(
        seen_parts,
        vec![
            MessagePart::semantic_only(MessageSemantic::Text {
                text: "<lattice-context channel=\"ipc\">\n图一之前".into()
            }),
            MessagePart::semantic_only(MessageSemantic::Image { image: ref_png }),
            MessagePart::semantic_only(MessageSemantic::Text {
                text: "两图之间".into()
            }),
            MessagePart::semantic_only(MessageSemantic::Image { image: ref_webp }),
            MessagePart::semantic_only(MessageSemantic::Text {
                text: "图二之后\n</lattice-context>".into()
            }),
        ]
    );
    assert_eq!(
        *client.read_blobs.lock().expect("读取借出的图片"),
        vec![(ref_png, png), (ref_webp, webp)]
    );
}

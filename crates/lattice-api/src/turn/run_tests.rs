// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::{test_support::*, types::TurnEvent};
use lattice_agent::LlmClient;
use lattice_conversation::*;
use lattice_llm::{LlmEvent, TokenUsage};
use lattice_session::store::*;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

#[tokio::test]
async fn run_turn_result_and_saved_nodes_preserve_contract_fields() {
    let fixture = setup_with_events(happy_events(), &[]);
    let result = super::execute::run_turn(
        fixture.api.clone(),
        fixture.layout.clone(),
        fixture.request.clone(),
    )
    .await
    .unwrap();
    assert_eq!(result.summary.outcome, PublicTurnOutcome::Completed);
    assert_eq!(
        result.usage,
        UsageSummary::Complete {
            total: TokenUsage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15
            }
        }
    );
    let read = fixture.store.begin_read().unwrap();
    assert_eq!(read.list(None, 10).unwrap().items.len(), 1);
    let detail = read
        .turn_detail(result.turn.session_id, result.turn.turn_id)
        .unwrap();
    assert_eq!(detail.steps, result.steps);
    assert_eq!(detail.user_node, NodeId(1));
    assert_eq!(read.log(result.turn.session_id).unwrap().nodes.len(), 3);
    let Node::Step { content, .. } = read
        .node(NodeRef {
            session_id: result.turn.session_id,
            node: NodeId(2),
        })
        .unwrap()
    else {
        panic!("Step")
    };
    assert!(
        matches!(&content.assistant.parts[0].semantic, MessageSemantic::Text { text } if text == "test response")
    );
    assert!(content.results.is_empty());
}

struct Capture(Mutex<Vec<lattice_llm::ChatRequest>>);
#[async_trait::async_trait]
impl LlmClient for Capture {
    async fn stream_chat(
        &self,
        request: lattice_llm::ChatRequest,
    ) -> std::result::Result<lattice_llm::LlmStream, Box<dyn std::error::Error + Send + Sync>> {
        self.0.lock().unwrap().push(request);
        let mut events = happy_events();
        events.insert(
            1,
            LlmEvent::MessageComplete {
                message: NativeMessage::new(
                    MessageRole::Assistant,
                    Some(ProviderOrigin::OpenAiCompatible),
                    vec![MessagePart::semantic_only(MessageSemantic::Text {
                        text: "test response".into(),
                    })],
                ),
            },
        );
        Ok(Box::pin(futures::stream::iter(events)))
    }
}
#[tokio::test]
async fn next_turn_uses_root_path_parent_and_frozen_model_provider_prompt() {
    let llm = Arc::new(Capture(Mutex::default()));
    let fixture = setup_fixture(llm.clone(), &[]);
    let mut request = fixture.request.clone();
    request.loaded_profile.as_mut().unwrap().profile.agent.model = "compat:profile-model".into();
    let (first, _) = fixture.run(request.clone()).await;
    request.session_id = Some(first.session_id);
    request.agent = None;
    request.input = "follow-up".into();
    let (second, _) = fixture.run(request).await;
    let read = fixture.store.begin_read().unwrap();
    assert_eq!(second.user_node, NodeId(3));
    assert!(matches!(
        read.node(NodeRef {
            session_id: first.session_id,
            node: second.user_node
        })
        .unwrap(),
        Node::UserMessage {
            parent: NodeId(2),
            ..
        }
    ));
    assert_eq!(read.log(first.session_id).unwrap().nodes.len(), 5);
    let stored = read.turn_record(second.turn_id).unwrap().unwrap();
    assert_eq!(stored.config.model, "compat:profile-model");
    assert_eq!(stored.config.provider, "openai_compatible");
    let requests = llm.0.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].target.model, "compat:profile-model");
    assert_eq!(
        requests[1].target.provider.as_deref(),
        Some("openai_compatible")
    );
    assert_eq!(requests[1].messages.len(), 4);
    assert!(
        matches!(&requests[1].messages[0], ConversationItem::Message(message) if message.role == MessageRole::System)
    );
    assert!(
        matches!(&requests[1].messages[1], ConversationItem::Message(message) if matches!(&message.parts[0].semantic, MessageSemantic::Text { text } if text == "hello"))
    );
    assert!(
        matches!(&requests[1].messages[2], ConversationItem::Step(step) if matches!(&step.assistant.parts[0].semantic, MessageSemantic::Text { text } if text == "test response"))
    );
}

#[tokio::test]
async fn provider_failure_closes_with_the_same_persistent_notice_and_no_partial_node() {
    let fixture = setup_with_events(
        vec![
            LlmEvent::Token {
                content: "partial".into(),
            },
            LlmEvent::Error {
                failure: lattice_llm::ProviderFailure {
                    message: "provider unavailable".into(),
                    retryable: false,
                },
            },
        ],
        &[],
    );
    let (detail, events) = fixture.run(fixture.request.clone()).await;
    assert_eq!(
        detail.summary.as_ref().unwrap().outcome,
        PublicTurnOutcome::Failed
    );
    assert_eq!(
        detail
            .summary
            .as_ref()
            .unwrap()
            .notice
            .as_ref()
            .unwrap()
            .text,
        "provider unavailable"
    );
    assert!(
        matches!(&events.last().unwrap(), TurnEvent::TurnFailed { summary, step, .. } if Some(summary) == detail.summary.as_ref() && step == &detail.steps[0])
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
        2
    );
    assert_eq!(
        detail.usage,
        UsageSummary::Partial {
            known: TokenUsage {
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0
            },
            unknown_attempts: 1
        }
    );
}

struct CancelOnDone(Arc<AtomicBool>);
#[async_trait::async_trait]
impl LlmClient for CancelOnDone {
    async fn stream_chat(
        &self,
        _: lattice_llm::ChatRequest,
    ) -> std::result::Result<lattice_llm::LlmStream, Box<dyn std::error::Error + Send + Sync>> {
        let cancel = self.0.clone();
        let events = vec![
            LlmEvent::MessageComplete {
                message: NativeMessage::new(
                    MessageRole::Assistant,
                    Some(ProviderOrigin::OpenAiCompatible),
                    vec![MessagePart::semantic_only(MessageSemantic::ToolCall {
                        call: UnboundToolCall {
                            provider_call_id: Some("call".into()),
                            name: "observe.workspace".into(),
                            arguments: serde_json::json!({"mode":"read_file","path":"src/lib.rs"}),
                        },
                    })],
                ),
            },
            LlmEvent::Done {
                finish_reason: "tool_calls".into(),
                usage: Some(TokenUsage {
                    prompt_tokens: 5,
                    completion_tokens: 2,
                    total_tokens: 7,
                }),
            },
        ];
        Ok(Box::pin(futures::stream::iter(events).map(move |event| {
            if matches!(event, LlmEvent::Done { .. }) {
                cancel.store(true, Ordering::Release);
            }
            event
        })))
    }
}
use futures::StreamExt;
#[tokio::test]
async fn completed_attempt_usage_survives_cancellation_before_dispatch() {
    let cancel = Arc::new(AtomicBool::new(false));
    let fixture = setup_fixture(
        Arc::new(CancelOnDone(cancel.clone())),
        &["observe.workspace"],
    );
    let mut request = fixture.request.clone();
    request.cancel_flag = Some(cancel);
    let (detail, _) = fixture.run(request).await;
    assert_eq!(
        detail.summary.as_ref().unwrap().outcome,
        PublicTurnOutcome::Cancelled
    );
    assert_eq!(
        detail.usage,
        UsageSummary::Complete {
            total: TokenUsage {
                prompt_tokens: 5,
                completion_tokens: 2,
                total_tokens: 7
            }
        }
    );
    assert!(matches!(
        &detail.steps[0].state,
        StepDetailState::Closed {
            end: StepEnd::Stop {
                cause: StopCause::Cancelled,
                ..
            },
            ..
        }
    ));
}

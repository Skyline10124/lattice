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
    assert_eq!(
        serde_json::to_value(&detail.steps[0].attempts[0].origin).unwrap(),
        serde_json::json!({"protocol":"chat-completions","provider_id":"local"})
    );
    let Node::UserMessage { provider, .. } = read
        .node(NodeRef {
            session_id: result.turn.session_id,
            node: detail.user_node,
        })
        .unwrap()
    else {
        panic!("user message")
    };
    assert_eq!(provider, "local");
    assert_eq!(detail.user_node, NodeId(1));
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
        control: lattice_llm::StreamControl,
    ) -> std::result::Result<lattice_llm::LlmStream, Box<dyn std::error::Error + Send + Sync>> {
        let cancel = self.0.clone();
        let message = NativeMessage::new(
            MessageRole::Assistant,
            Some(ProviderOrigin::OpenAiCompatible),
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
                prompt_tokens: 5,
                completion_tokens: 2,
                total_tokens: 7,
            }),
        });
        Ok(Box::pin(
            lattice_llm::testing::script_stream(events, control).map(move |event| {
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
        &detail.steps[0].progress,
        StepProgress::Closed {
            end: StepEnd::Stop {
                cause: StopCause::Cancelled
            }
        }
    ));
}

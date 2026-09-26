// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

#![cfg(feature = "test-utils")]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use futures::{StreamExt, stream};
use lattice_agent::{
    LlmClient, ModelStepAttempt, ModelStepDriver, ModelStepRequest, ModelStepStatus,
};
use lattice_conversation::{AttemptInterruption, LimitKind, MessageSemantic};
use lattice_llm::OpenAiSseParser;
use lattice_llm::{ChatRequest, LlmStream};
use serde_json::{Value, json};

// Only the transport is replaced: parse frames on demand so consumption past
// the offending fragment is observable, including the completion projection.
struct OpenAiFrames {
    frames: Vec<String>,
    consumed: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl LlmClient for OpenAiFrames {
    async fn stream_chat(
        &self,
        _: ChatRequest,
    ) -> Result<LlmStream, Box<dyn std::error::Error + Send + Sync>> {
        let mut parser = OpenAiSseParser::new();
        let consumed = self.consumed.clone();
        Ok(Box::pin(stream::iter(self.frames.clone()).flat_map(
            move |frame| {
                consumed.fetch_add(1, Ordering::SeqCst);
                stream::iter(parser.parse_chunk(None, &frame))
            },
        )))
    }
}

fn delta(value: Value) -> String {
    json!({"choices": [{"delta": value}]}).to_string()
}

fn metadata(frames: &mut Vec<String>, index: u32) {
    frames.push(delta(json!({"tool_calls": [{
        "index": index, "id": format!("call-{index}"),
        "function": {"name": "observe__workspace"}
    }]})));
}

fn arguments(frames: &mut Vec<String>, index: u32, text: &str) {
    // Fixtures use ASCII, so these byte boundaries are valid UTF-8 boundaries.
    for chunk in text.as_bytes().chunks(128 * 1024) {
        frames.push(delta(json!({"tool_calls": [{
            "index": index,
            "function": {"arguments": std::str::from_utf8(chunk).unwrap()}
        }]})));
    }
}

fn finish(frames: &mut Vec<String>) {
    frames.push(json!({"choices": [{"delta": {}, "finish_reason": "tool_calls"}]}).to_string());
    frames.push("[DONE]".to_string());
}

async fn run(frames: Vec<String>) -> (ModelStepAttempt, usize) {
    let consumed = Arc::new(AtomicUsize::new(0));
    let driver = ModelStepDriver::new(Arc::new(OpenAiFrames {
        frames,
        consumed: consumed.clone(),
    }));
    let attempt = driver
        .run(
            &ModelStepRequest {
                system_prompt: None,
                history: vec![],
                model: "test-model".to_string(),
                provider: Some("openai".to_string()),
                tools: vec![],
                temperature: None,
                max_tokens: None,
                cancel: None,
                deadline: None,
            },
            None,
        )
        .await
        .unwrap();
    (attempt, consumed.load(Ordering::SeqCst))
}

#[tokio::test]
async fn order_f13_interleaved_legal_arguments_complete() {
    let payload = format!("{{\"x\":\"{}\"}}", "a".repeat(700_000));
    for (a, b, late_metadata) in [(0, 1, false), (u32::MAX, 7, true)] {
        let mut frames = vec![];
        if !late_metadata {
            metadata(&mut frames, a);
        }
        arguments(&mut frames, a, &payload[..8]);
        metadata(&mut frames, b);
        arguments(&mut frames, b, &payload);
        if late_metadata {
            metadata(&mut frames, a);
        }
        arguments(&mut frames, a, &payload[8..]);
        finish(&mut frames);
        let (attempt, _) = run(frames).await;
        assert_eq!(attempt.status, ModelStepStatus::Complete);
        let message = attempt.message.unwrap();
        assert_eq!(message.parts.len(), 2);
        for part in message.parts {
            let MessageSemantic::ToolCall { call } = part.semantic else {
                panic!("expected tool call");
            };
            assert_eq!(call.arguments, json!({"x": "a".repeat(700_000)}));
        }
    }
}

#[tokio::test]
async fn order_f14_interleaved_limit_stops_at_crossing_fragment() {
    let payload = format!("{{\"x\":\"{}\"}}", "a".repeat(1_100_000));
    for late_metadata in [false, true] {
        let mut frames = vec![];
        if !late_metadata {
            metadata(&mut frames, 9);
        }
        arguments(&mut frames, 9, &payload[..600_000]);
        metadata(&mut frames, 2);
        arguments(&mut frames, 2, "{}");
        if late_metadata {
            metadata(&mut frames, 9);
        }
        arguments(&mut frames, 9, &payload[600_000..1_048_576]);
        arguments(&mut frames, 9, &payload[1_048_576..1_048_577]);
        let crossing_frame = frames.len();
        arguments(&mut frames, 9, &payload[1_048_577..]);
        finish(&mut frames);
        let (attempt, consumed) = run(frames).await;
        assert_eq!(
            attempt.status,
            ModelStepStatus::Interrupted(AttemptInterruption::OutputLimitExceeded {
                kind: LimitKind::ToolCallArguments,
                limit: 1_048_576,
                actual: 1_048_577,
            })
        );
        assert!(attempt.message.is_none());
        assert_eq!(consumed, crossing_frame);
    }
}

#[tokio::test]
async fn interleaved_arguments_share_total_with_text_and_reasoning() {
    let mut frames = vec![];
    for field in ["content", "reasoning_content"] {
        for _ in 0..16 {
            frames.push(delta(json!({field: "a".repeat(128 * 1024)})));
        }
    }
    // 12 calls, each exactly 1 MiB, plus 4 MiB text/reasoning = 16 MiB.
    let payload = format!("{{\"x\":\"{}\"}}", "a".repeat(1_048_568));
    for index in 0..12 {
        metadata(&mut frames, index);
        arguments(&mut frames, index, &payload[..8]);
    }
    for index in (0..12).rev() {
        arguments(&mut frames, index, &payload[8..]);
    }
    // A new call has per-call headroom, but not a separate total allowance.
    metadata(&mut frames, 12);
    arguments(&mut frames, 12, "{");
    let crossing_frame = frames.len();
    arguments(&mut frames, 12, "}");
    finish(&mut frames);
    let (attempt, consumed) = run(frames).await;
    assert_eq!(
        attempt.status,
        ModelStepStatus::Interrupted(AttemptInterruption::OutputLimitExceeded {
            kind: LimitKind::ModelAttemptTotal,
            limit: 16_777_216,
            actual: 16_777_217,
        })
    );
    assert!(attempt.message.is_none());
    assert_eq!(consumed, crossing_frame);
}

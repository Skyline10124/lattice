// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 惰性消费协议增量；一旦累积器结束，停止消费并只交付一次 Finished。

use crate::LlmStream;
use crate::reply::{ProtocolEvent, ReplyAccumulator, ReplyProtocolError, StreamStop, Update};
use crate::types::{AttemptReport, LlmEvent};
use futures::{Stream, StreamExt, stream};
use lattice_conversation::ReplayOrigin;
use std::pin::Pin;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

pub type ProtocolStream = Pin<Box<dyn Stream<Item = ProtocolEvent> + Send>>;

/// 请求打开与流消费共用的取消、deadline 轮询间隔。
pub const CONTROL_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// 打开与消费共享同一取消载体；终态一经决定，不被迟到取消改写。
#[derive(Debug, Clone, Default)]
pub struct StreamControl {
    pub cancel: Option<Arc<AtomicBool>>,
    pub deadline: Option<tokio::time::Instant>,
}
impl StreamControl {
    pub fn stop(&self) -> Option<StreamStop> {
        if self
            .cancel
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Acquire))
        {
            Some(StreamStop::Cancelled)
        } else if self
            .deadline
            .is_some_and(|deadline| tokio::time::Instant::now() >= deadline)
        {
            Some(StreamStop::Deadline)
        } else {
            None
        }
    }
}

pub(crate) struct ReplyDelivery {
    accumulator: ReplyAccumulator,
    report: AttemptReport,
}
impl ReplyDelivery {
    pub(crate) fn new(origin: Option<ReplayOrigin>) -> Self {
        Self {
            accumulator: ReplyAccumulator::new(origin),
            report: AttemptReport::default(),
        }
    }
    pub(crate) fn accept(&mut self, event: ProtocolEvent) -> Option<LlmEvent> {
        if self.accumulator.is_ended() {
            return None;
        }
        let update =
            match event {
                ProtocolEvent::Part(delta) => self.accumulator.push(delta),
                ProtocolEvent::Usage(usage) => {
                    if let Some(usage) = usage.normalized() {
                        self.report.usage = Some(usage);
                    }
                    return None;
                }
                ProtocolEvent::Done { finish_reason } => {
                    self.report.finish_reason = Some(finish_reason);
                    self.accumulator.finish()
                }
                ProtocolEvent::Failed(reason) => self.accumulator.interrupt(reason),
                ProtocolEvent::UnsupportedContentBlock {
                    protocol,
                    index,
                    block_type,
                } => self.accumulator.interrupt(StreamStop::Protocol(
                    ReplyProtocolError::unsupported(protocol, index, &block_type),
                )),
            }
            .expect("delivery stops as soon as the accumulator ends");
        match update {
            Update::Accepted { live } => live.map(LlmEvent::Delta),
            Update::Ended => Some(LlmEvent::Finished {
                outcome: self
                    .accumulator
                    .take_outcome()
                    .expect("one outcome per terminal transition"),
                report: std::mem::take(&mut self.report),
            }),
        }
    }
}

/// 生产协议、stub 和测试输入都通过这个包装器进入同一个累积器。
pub fn assemble_reply_stream(
    origin: Option<ReplayOrigin>,
    input: ProtocolStream,
    control: StreamControl,
) -> LlmStream {
    struct State {
        input: ProtocolStream,
        delivery: ReplyDelivery,
        control: StreamControl,
        started: bool,
    }
    Box::pin(stream::unfold(
        State {
            input,
            delivery: ReplyDelivery::new(origin),
            control,
            started: false,
        },
        |mut state| async move {
            if state.delivery.accumulator.is_ended() {
                return None;
            }
            if !state.started {
                state.started = true;
                return Some((LlmEvent::Started, state));
            }
            loop {
                if let Some(stop) = state.control.stop() {
                    let event = state
                        .delivery
                        .accept(ProtocolEvent::Failed(stop))
                        .expect("interruption is terminal");
                    return Some((event, state));
                }
                let input = tokio::select! {
                    biased;
                    event = state.input.next() => event,
                    _ = tokio::time::sleep(CONTROL_POLL_INTERVAL), if state.control.cancel.is_some() || state.control.deadline.is_some() => continue,
                };
                let input = input.unwrap_or(ProtocolEvent::Failed(StreamStop::UnexpectedEof));
                // 已就绪的协议结束优先于同时到达的取消；其他事件在接收后再核对控制条件。
                let input = if matches!(input, ProtocolEvent::Done { .. }) {
                    input
                } else if let Some(stop) = state.control.stop() {
                    ProtocolEvent::Failed(stop)
                } else {
                    input
                };
                if let Some(event) = state.delivery.accept(input) {
                    return Some((event, state));
                }
            }
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reply::{PartDelta, PartKind, ReplyOutcome, ReplyStop};
    use crate::{LiveDelta, ProviderFailure};
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn protocol_snapshots_keep_native_components_across_partial_updates_and_errors() {
        use crate::protocol::projection::{ProjectionError, wire_usage};
        use lattice_conversation::{Protocol, TokenUsage};
        use serde_json::json;
        for (protocol, frames, expected, detail) in [
            (
                Protocol::ChatCompletions,
                vec![
                    (
                        None,
                        json!({"usage":{"prompt_tokens":10,"prompt_tokens_details":{"cached_tokens":4,"cache_write_tokens":2}}}),
                    ),
                    (
                        None,
                        json!({"usage":{"completion_tokens":3,"completion_tokens_details":{"reasoning_tokens":8}}}),
                    ),
                    (
                        None,
                        json!({"usage":{"prompt_tokens":0,"completion_tokens":0,"prompt_tokens_details":{"cache_write_tokens":null}}}),
                    ),
                    (None, json!({"usage":{"completion_tokens":"bad"}})),
                ],
                TokenUsage {
                    input: Some(0),
                    cache_read: Some(4),
                    cache_write: Some(2),
                    output: Some(0),
                    reasoning: Some(8),
                },
                "usage counter must be a u64 integer",
            ),
            (
                Protocol::Messages,
                vec![
                    (
                        Some("message_start"),
                        json!({"message":{"usage":{"input_tokens":16}}}),
                    ),
                    (
                        Some("message_delta"),
                        json!({"usage":{"output_tokens":3,"input_tokens":0,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}),
                    ),
                    (
                        Some("message_delta"),
                        json!({"usage":{"output_tokens":3,"input_tokens":null}}),
                    ),
                    (
                        Some("message_delta"),
                        json!({"usage":{"output_tokens":"bad"}}),
                    ),
                ],
                TokenUsage {
                    input: Some(16),
                    cache_read: Some(0),
                    cache_write: Some(0),
                    output: Some(3),
                    reasoning: None,
                },
                "usage counter must be a u64 integer",
            ),
            (
                Protocol::GenerateContent,
                vec![
                    (
                        None,
                        json!({"usageMetadata":{"promptTokenCount":10,"toolUsePromptTokenCount":2,"cachedContentTokenCount":4,"thoughtsTokenCount":3}}),
                    ),
                    (None, json!({"usageMetadata":{"candidatesTokenCount":5}})),
                    (
                        None,
                        json!({"usageMetadata":{"candidatesTokenCount":5,"thoughtsTokenCount":null}}),
                    ),
                    (
                        None,
                        json!({"usageMetadata":{"candidatesTokenCount":"bad"}}),
                    ),
                ],
                TokenUsage {
                    input: Some(12),
                    cache_read: Some(4),
                    cache_write: None,
                    output: Some(8),
                    reasoning: Some(3),
                },
                "usage counter must be a u64 integer",
            ),
            (
                Protocol::Messages,
                vec![
                    (
                        Some("message_start"),
                        json!({"message":{"usage":{"input_tokens":10,"cache_read_input_tokens":4,"cache_creation_input_tokens":2,"output_tokens":3}}}),
                    ),
                    (
                        Some("message_delta"),
                        json!({"usage":{"input_tokens":u64::MAX,"cache_read_input_tokens":1,"cache_creation_input_tokens":0,"output_tokens":99}}),
                    ),
                ],
                TokenUsage {
                    input: Some(16),
                    cache_read: Some(4),
                    cache_write: Some(2),
                    output: Some(3),
                    reasoning: None,
                },
                "normalized usage exceeds u64",
            ),
            (
                Protocol::GenerateContent,
                vec![
                    (
                        None,
                        json!({"usageMetadata":{"promptTokenCount":10,"toolUsePromptTokenCount":2,"cachedContentTokenCount":4,"candidatesTokenCount":5,"thoughtsTokenCount":3}}),
                    ),
                    (
                        None,
                        json!({"usageMetadata":{"promptTokenCount":u64::MAX,"toolUsePromptTokenCount":1,"candidatesTokenCount":99}}),
                    ),
                ],
                TokenUsage {
                    input: Some(12),
                    cache_read: Some(4),
                    cache_write: None,
                    output: Some(8),
                    reasoning: Some(3),
                },
                "normalized usage exceeds u64",
            ),
            (
                Protocol::GenerateContent,
                vec![
                    (
                        None,
                        json!({"usageMetadata":{"promptTokenCount":10,"toolUsePromptTokenCount":2,"cachedContentTokenCount":4,"candidatesTokenCount":5,"thoughtsTokenCount":3}}),
                    ),
                    (
                        None,
                        json!({"usageMetadata":{"promptTokenCount":99,"candidatesTokenCount":u64::MAX,"thoughtsTokenCount":1}}),
                    ),
                ],
                TokenUsage {
                    input: Some(12),
                    cache_read: Some(4),
                    cache_write: None,
                    output: Some(8),
                    reasoning: Some(3),
                },
                "normalized usage exceeds u64",
            ),
        ] {
            assert!(matches!(
                wire_usage(protocol, &frames.last().unwrap().1),
                Err(ProjectionError::MalformedUsage { provider, detail: actual }) if provider == protocol && actual == detail
            ));
            let mut parser = crate::test_reply::Parser::new(protocol);
            let mut events = Vec::new();
            for (name, frame) in frames {
                events.extend(parser.parse_chunk(name, &frame.to_string()));
            }
            let [
                LlmEvent::Finished {
                    outcome:
                        ReplyOutcome::Interrupted {
                            reason: ReplyStop::Stream(StreamStop::Provider(failure)),
                        },
                    report,
                },
            ] = events.as_slice()
            else {
                panic!("{protocol:?}: {events:?}");
            };
            assert_eq!(
                failure,
                &ProviderFailure {
                    message: format!("malformed {protocol} usage block: {detail}"),
                    retryable: false,
                }
            );
            assert_eq!(report.usage, Some(expected), "{protocol:?}: {detail}");
        }
    }

    #[tokio::test]
    async fn protocol_completion_applies_input_zero_rules_and_gemini_defaults() {
        use lattice_conversation::Protocol;
        use serde_json::json;
        tokio::time::timeout(Duration::from_secs(5), async {
            for (name, protocol, frames, expected, finish) in [
                ("messages zero preserves all known inputs", Protocol::Messages, vec![
                    (Some("message_start"), json!({"message":{"usage":{"input_tokens":10,"cache_read_input_tokens":4,"cache_creation_input_tokens":2,"output_tokens":3}}})),
                    (Some("message_delta"), json!({"usage":{"input_tokens":0,"cache_read_input_tokens":0,"cache_creation_input_tokens":0,"output_tokens":0},"delta":{"stop_reason":"end_turn"}})),
                ], json!([16,4,2,0,null]), Some("stop")),
                ("messages first report of zero inputs", Protocol::Messages, vec![
                    (Some("message_start"), json!({"message":{"usage":{}}})),
                    (Some("message_delta"), json!({"usage":{"input_tokens":0,"cache_read_input_tokens":0,"cache_creation_input_tokens":0,"output_tokens":3},"delta":{"stop_reason":"end_turn"}})),
                ], json!([0,0,0,3,null]), Some("stop")),
                ("messages positive replaces inputs and null preserves", Protocol::Messages, vec![
                    (Some("message_start"), json!({"message":{"usage":{"input_tokens":10,"cache_read_input_tokens":4,"cache_creation_input_tokens":2}}})),
                    (Some("message_delta"), json!({"usage":{"input_tokens":12,"cache_read_input_tokens":5,"cache_creation_input_tokens":1,"output_tokens":3}})),
                    (Some("message_delta"), json!({"usage":{"input_tokens":null,"cache_read_input_tokens":null,"cache_creation_input_tokens":null},"delta":{"stop_reason":"end_turn"}})),
                ], json!([18,5,1,3,null]), Some("stop")),
                ("gemini omitted zero fields", Protocol::GenerateContent, vec![
                    (None, json!({"candidates":[{"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":8,"candidatesTokenCount":2}})),
                ], json!([8,0,null,2,0]), Some("stop")),
                ("gemini thoughts exhaust output budget", Protocol::GenerateContent, vec![
                    (None, json!({"candidates":[{"finishReason":"MAX_TOKENS"}],"usageMetadata":{"promptTokenCount":8,"thoughtsTokenCount":2}})),
                ], json!([8,0,null,2,2]), Some("length")),
                ("gemini candidate safety", Protocol::GenerateContent, vec![
                    (None, json!({"candidates":[{"finishReason":"SAFETY"}],"usageMetadata":{"promptTokenCount":8}})),
                ], json!([8,0,null,0,0]), Some("content_filter")),
                ("gemini missing prompt", Protocol::GenerateContent, vec![
                    (None, json!({"candidates":[{"finishReason":"STOP"}],"usageMetadata":{"toolUsePromptTokenCount":2,"thoughtsTokenCount":3}})),
                ], json!([null,0,null,3,3]), Some("stop")),
                ("gemini final block has no metadata", Protocol::GenerateContent, vec![
                    (None, json!({"usageMetadata":{"promptTokenCount":8}})),
                    (None, json!({"candidates":[{"finishReason":"STOP"}]})),
                ], json!([8,0,null,0,0]), Some("stop")),
                ("gemini explicit zero replaces", Protocol::GenerateContent, vec![
                    (None, json!({"usageMetadata":{"promptTokenCount":8,"candidatesTokenCount":5,"thoughtsTokenCount":2,"cachedContentTokenCount":3,"toolUsePromptTokenCount":1}})),
                    (None, json!({"candidates":[{"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":0,"candidatesTokenCount":0,"thoughtsTokenCount":0,"cachedContentTokenCount":0,"toolUsePromptTokenCount":0}})),
                ], json!([0,0,null,0,0]), Some("stop")),
                ("gemini interruption preserves unknown output", Protocol::GenerateContent, vec![
                    (None, json!({"usageMetadata":{"promptTokenCount":8}})),
                ], json!([8,null,null,null,null]), None),
                ("gemini interruption preserves reported components", Protocol::GenerateContent, vec![
                    (None, json!({"usageMetadata":{"promptTokenCount":10,"toolUsePromptTokenCount":2,"cachedContentTokenCount":4,"thoughtsTokenCount":3}})),
                ], json!([12,4,null,null,3]), None),
            ] {
                let mut parser = crate::test_reply::Parser::new(protocol);
                let mut input = Vec::new();
                for (event, frame) in frames {
                    parser.parse_chunk(event, &frame.to_string());
                    input.append(&mut parser.raw);
                }
                let events = assemble_reply_stream(None, Box::pin(stream::iter(input)), StreamControl::default()).collect::<Vec<_>>().await;
                let Some(LlmEvent::Finished { outcome, report }) = events.last() else { panic!("{name}: {events:?}") };
                assert_eq!(report.finish_reason.as_deref(), finish, "{name}");
                if finish.is_some() {
                    assert!(matches!(outcome, ReplyOutcome::Complete { .. }), "{name}: {outcome:?}");
                } else {
                    assert!(matches!(outcome, ReplyOutcome::Interrupted { reason: ReplyStop::Stream(StreamStop::UnexpectedEof) }), "{name}: {outcome:?}");
                    assert_eq!(report.usage_kind, lattice_conversation::UsageCompleteness::Partial, "{name}");
                }
                let usage = serde_json::to_value(report.usage.unwrap()).unwrap();
                assert_eq!(json!([usage["input"], usage["cache_read"], usage["cache_write"], usage["output"], usage["reasoning"]]), expected, "{name}");
            }
        }).await.expect("protocol completion must finish");
    }

    fn done() -> ProtocolEvent {
        ProtocolEvent::Done {
            finish_reason: "stop".into(),
        }
    }
    fn failure() -> ProtocolEvent {
        ProtocolEvent::error("failed", true)
    }

    #[tokio::test]
    async fn each_terminal_path_emits_one_finished_and_stops_polling() {
        for (events, consumed_expected) in [
            (vec![done(), done(), failure()], 1),
            (vec![failure(), done()], 1),
            (vec![], 0),
            (
                vec![
                    ProtocolEvent::Part(PartDelta::Begin {
                        index: 0,
                        kind: PartKind::Text,
                    }),
                    ProtocolEvent::Part(PartDelta::Text {
                        index: 0,
                        delta: "x".repeat(lattice_conversation::MAX_ASSISTANT_CONTENT_BYTES + 1),
                    }),
                    done(),
                ],
                2,
            ),
        ] {
            let consumed = Arc::new(AtomicUsize::new(0));
            let count = consumed.clone();
            let input = stream::iter(events).inspect(move |_| {
                count.fetch_add(1, Ordering::SeqCst);
            });
            let mut output = assemble_reply_stream(None, Box::pin(input), StreamControl::default());
            assert_eq!(consumed.load(Ordering::SeqCst), 0, "construction is lazy");
            assert_eq!(output.next().await, Some(LlmEvent::Started));
            assert_eq!(
                consumed.load(Ordering::SeqCst),
                0,
                "Started does not consume input"
            );
            let events = output.collect::<Vec<_>>().await;
            assert_eq!(
                events
                    .iter()
                    .filter(|event| matches!(event, LlmEvent::Finished { .. }))
                    .count(),
                1
            );
            assert!(
                !events
                    .iter()
                    .any(|event| matches!(event, LlmEvent::Delta(_))),
                "no out-of-bounds fragment reaches live output"
            );
            assert_eq!(consumed.load(Ordering::SeqCst), consumed_expected);
        }
    }

    #[tokio::test]
    async fn reported_usage_is_replaced_and_preserved_on_rejection_and_interruption() {
        let first = lattice_conversation::TokenUsage {
            input: Some(1),
            output: Some(2),
            ..Default::default()
        };
        let last = lattice_conversation::TokenUsage {
            input: Some(1),
            output: Some(4),
            ..Default::default()
        };
        for end in [failure(), done()] {
            let input = vec![
                ProtocolEvent::Usage(first),
                ProtocolEvent::Usage(last),
                ProtocolEvent::Part(PartDelta::Begin {
                    index: 9,
                    kind: PartKind::ToolCall,
                }),
                ProtocolEvent::Part(PartDelta::End { index: 9 }),
                end,
            ];
            let events = assemble_reply_stream(
                None,
                Box::pin(stream::iter(input)),
                StreamControl::default(),
            )
            .collect::<Vec<_>>()
            .await;
            let Some(LlmEvent::Finished { outcome, report }) = events.last() else {
                panic!("finished");
            };
            assert_eq!(
                report.usage,
                Some(last),
                "cumulative reports are not summed"
            );
            assert!(matches!(
                outcome,
                ReplyOutcome::Rejected { .. } | ReplyOutcome::Interrupted { .. }
            ));
        }
    }

    #[tokio::test]
    async fn unfinished_eof_is_retryable_and_keeps_accepted_live_content() {
        let input = vec![
            ProtocolEvent::Part(PartDelta::Begin {
                index: 0,
                kind: PartKind::Text,
            }),
            ProtocolEvent::Part(PartDelta::Text {
                index: 0,
                delta: "partial".into(),
            }),
        ];
        let events = assemble_reply_stream(
            None,
            Box::pin(stream::iter(input)),
            StreamControl::default(),
        )
        .collect::<Vec<_>>()
        .await;
        assert_eq!(
            events,
            vec![
                LlmEvent::Started,
                LlmEvent::Delta(LiveDelta::Text("partial".into())),
                LlmEvent::Finished {
                    outcome: ReplyOutcome::Interrupted {
                        reason: ReplyStop::Stream(StreamStop::UnexpectedEof)
                    },
                    report: AttemptReport::default()
                }
            ]
        );
    }

    #[tokio::test]
    async fn done_ready_with_cancel_keeps_its_decided_outcome() {
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let input = stream::iter(vec![done(), failure()]).inspect(move |event| {
            if matches!(event, ProtocolEvent::Done { .. }) {
                flag.store(true, Ordering::Release);
            }
        });
        let events = assemble_reply_stream(
            None,
            Box::pin(input),
            StreamControl {
                cancel: Some(cancel),
                deadline: None,
            },
        )
        .collect::<Vec<_>>()
        .await;
        assert!(matches!(
            &events[..],
            [
                LlmEvent::Started,
                LlmEvent::Finished {
                    outcome: ReplyOutcome::Complete { .. },
                    ..
                }
            ]
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn cancellation_and_deadline_interrupt_a_stalled_stream_once() {
        for deadline in [false, true] {
            let cancel = Arc::new(AtomicBool::new(false));
            let control = StreamControl {
                cancel: Some(cancel.clone()),
                deadline: deadline.then(|| tokio::time::Instant::now() + Duration::from_millis(20)),
            };
            let consumed = Arc::new(AtomicUsize::new(0));
            let count = consumed.clone();
            let input = stream::iter(vec![ProtocolEvent::Usage(
                lattice_conversation::TokenUsage {
                    input: Some(1),
                    output: Some(2),
                    ..Default::default()
                },
            )])
            .inspect(move |_| {
                count.fetch_add(1, Ordering::SeqCst);
            })
            .chain(stream::pending());
            let mut output = assemble_reply_stream(None, Box::pin(input), control);
            let stop = if deadline {
                StreamStop::Deadline
            } else {
                StreamStop::Cancelled
            };
            assert_eq!(output.next().await, Some(LlmEvent::Started));
            let event = {
                let next = output.next();
                tokio::pin!(next);
                assert!(futures::poll!(&mut next).is_pending());
                assert_eq!(
                    consumed.load(Ordering::SeqCst),
                    1,
                    "usage is consumed before time advances"
                );
                if !deadline {
                    cancel.store(true, Ordering::Release);
                }
                tokio::time::advance(CONTROL_POLL_INTERVAL).await;
                tokio::time::timeout(Duration::from_secs(2), next)
                    .await
                    .unwrap()
            };
            assert!(matches!(event, Some(LlmEvent::Finished {
                outcome: ReplyOutcome::Interrupted { reason: ReplyStop::Stream(actual) }, report
            }) if actual == stop && report.usage.unwrap().context_tokens() == Some(3)));
            assert!(
                output.next().await.is_none(),
                "the stalled stream terminates once"
            );
        }
    }

    #[test]
    fn provider_failure_keeps_retryability() {
        let mut delivery = ReplyDelivery::new(None);
        let event = delivery
            .accept(ProtocolEvent::Failed(StreamStop::Provider(
                ProviderFailure {
                    message: "bounded".into(),
                    retryable: true,
                },
            )))
            .unwrap();
        assert!(matches!(
            event,
            LlmEvent::Finished {
                outcome: ReplyOutcome::Interrupted {
                    reason: ReplyStop::Stream(StreamStop::Provider(ProviderFailure {
                        retryable: true,
                        ..
                    }))
                },
                ..
            }
        ));
        assert!(delivery.accept(done()).is_none());
    }
}

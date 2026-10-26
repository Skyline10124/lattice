// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 惰性消费协议增量；一旦累积器结束，停止消费并只交付一次 Finished。

use crate::LlmStream;
use crate::reply::{ProtocolEvent, ReplyAccumulator, ReplyProtocolError, StreamStop, Update};
use crate::types::{AttemptReport, LlmEvent};
use futures::{Stream, StreamExt, stream};
use lattice_conversation::ProviderOrigin;
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
    pub(crate) fn new(origin: Option<ProviderOrigin>) -> Self {
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
                    self.report.usage = Some(usage);
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
    origin: Option<ProviderOrigin>,
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
            prompt_tokens: 1,
            completion_tokens: 2,
            total_tokens: 3,
        };
        let last = lattice_conversation::TokenUsage {
            prompt_tokens: 1,
            completion_tokens: 4,
            total_tokens: 5,
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
                    prompt_tokens: 1,
                    completion_tokens: 2,
                    total_tokens: 3,
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
            }) if actual == stop && report.usage.unwrap().total_tokens == 3));
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

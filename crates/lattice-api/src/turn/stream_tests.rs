// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0
use super::{observer::ObserverSink, test_support::*, types::*};
use lattice_llm::testing::ScriptEvent;
use lattice_session::store::{NoticeLevel, PublicTurnOutcome, StepEnd, StepProgress, StopCause};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

#[tokio::test]
async fn stream_turn_error_after_partial_output_ends_attempt_without_a_reply() {
    let fixture = setup_with_events(
        vec![
            ScriptEvent::Text {
                content: "partial output".into(),
            },
            ScriptEvent::Reasoning {
                content: "thinking".into(),
            },
            ScriptEvent::Text {
                content: " before error".into(),
            },
            ScriptEvent::Failure {
                failure: lattice_llm::ProviderFailure {
                    message: "connection reset".into(),
                    retryable: false,
                },
            },
        ],
        &[],
    );
    let (detail, events) = fixture.run(fixture.request.clone()).await;
    let deltas: String = events
        .iter()
        .filter_map(|event| match event {
            TurnEvent::TokenDelta { content, .. } => Some(content.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(deltas, "partial output before error");
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, TurnEvent::LlmRetry { .. }))
    );
    assert!(
        matches!(&events.last().unwrap(), TurnEvent::TurnFailed { summary, .. } if summary.notice.as_ref().unwrap().text.contains("connection reset"))
    );
    assert_eq!(detail.summary.outcome, PublicTurnOutcome::Failed);
    let read = fixture.store.begin_read().unwrap();
    let node = read
        .node(lattice_session::store::NodeRef {
            session_id: detail.session_id,
            node: lattice_session::store::NodeId(2),
        })
        .unwrap();
    let lattice_session::store::Node::Step {
        reply, attempts, ..
    } = node
    else {
        panic!("step")
    };
    assert!(reply.is_none(), "失败的半截输出不能变成完整回复");
    assert!(matches!(
        &attempts[0].progress,
        lattice_session::store::AttemptProgress::Ended { .. }
    ));
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
}

struct TimedLlm {
    inner: lattice_agent::testing::FakeStreamingLlmClient,
    starts: Mutex<Vec<tokio::time::Instant>>,
}
#[async_trait::async_trait]
impl lattice_agent::LlmClient for TimedLlm {
    async fn stream_chat(
        &self,
        request: lattice_llm::ChatRequest,
        control: lattice_llm::StreamControl,
    ) -> Result<lattice_llm::LlmStream, Box<dyn std::error::Error + Send + Sync>> {
        self.starts
            .lock()
            .unwrap()
            .push(tokio::time::Instant::now());
        self.inner.stream_chat(request, control).await
    }
}
fn opening_failure() -> lattice_llm::LlmError {
    lattice_llm::LlmError::Transport {
        message: "retryable opening failure".into(),
        retryable: true,
    }
}
fn timed(failures: usize) -> Arc<TimedLlm> {
    let mut builder = lattice_agent::testing::FakeStreamingLlmClient::builder();
    for _ in 0..failures {
        builder = builder.push_llm_error(opening_failure());
    }
    Arc::new(TimedLlm {
        inner: builder.push_events(happy_events()).build(),
        starts: Mutex::default(),
    })
}

#[tokio::test(start_paused = true)]
async fn stream_turn_retry_uses_profile_backoff_and_cap() {
    let llm = timed(3);
    let fixture = setup_fixture(llm.clone(), &[]);
    let mut request = fixture.request.clone();
    request.loaded_profile.as_mut().unwrap().profile.llm.retry = lattice_profile::LlmRetrySection {
        max_retries: 3,
        initial_delay_ms: 40,
        max_delay_ms: 60,
    };
    let (detail, events) = fixture.run(request).await;
    let delays: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            TurnEvent::LlmRetry {
                key,
                delay_ms,
                notice,
            } => {
                assert_eq!(notice.level, NoticeLevel::Warn);
                assert!(notice.text.contains("retryable opening failure"));
                Some((key.attempt_ordinal, *delay_ms))
            }
            _ => None,
        })
        .collect();
    assert_eq!(delays, vec![(0, 40), (1, 60), (2, 60)]);
    assert_eq!(detail.steps[0].attempts.len(), 4);
    let persisted: Vec<_> = detail.steps[0]
        .attempts
        .iter()
        .filter_map(|attempt| attempt.retry_delay_ms.map(|delay| (attempt.attempt, delay)))
        .collect();
    assert_eq!(persisted, delays);
    let node = fixture
        .store
        .begin_read()
        .unwrap()
        .node(lattice_session::store::NodeRef {
            session_id: detail.session_id,
            node: detail.steps[0].node,
        })
        .unwrap();
    let notices = node.retry_notices().unwrap();
    for (notice, event) in notices.iter().zip(
        events
            .iter()
            .filter(|e| matches!(e, TurnEvent::LlmRetry { .. })),
    ) {
        let TurnEvent::LlmRetry {
            key,
            delay_ms,
            notice: live_notice,
        } = event
        else {
            unreachable!()
        };
        assert_eq!(notice.attempt, key.attempt_ordinal);
        assert_eq!(&notice.delay_ms, delay_ms);
        assert_eq!(&notice.notice, live_notice);
    }
    assert_eq!(notices.len(), delays.len());
    {
        let starts = llm.starts.lock().unwrap();
        assert_eq!(starts.len(), 4);
        for (pair, delay) in starts.windows(2).zip([40, 60, 60]) {
            assert_eq!(pair[1] - pair[0], Duration::from_millis(delay));
        }
    }
    assert_eq!(detail.summary.outcome, PublicTurnOutcome::Completed);

    // 释放原库句柄后由后台重开，核验 IPC 分页与客户端组装也保留同一重试提示。
    let layout = fixture.layout.clone();
    let TestFixture {
        api,
        store,
        _state,
        _project,
        ..
    } = fixture;
    drop((api, store));
    let serving = tokio::spawn(crate::local::serve(layout.clone()));
    let client = crate::local::LocalClient::new(layout);
    tokio::time::timeout(Duration::from_secs(10), async {
        while client.status().await.is_err() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let mut view = client.watch(detail.session_id).await.unwrap();
    let Some(crate::local::ViewUpdate::Head(head)) = view.next().await.unwrap() else {
        panic!("reopened head")
    };
    assert!(!head.owner);
    view.next().await.unwrap().unwrap();
    let Some(crate::local::ViewUpdate::Entry { entry }) = view.next().await.unwrap() else {
        panic!("reopened entry")
    };
    let lattice_session::store::ConversationEntry::Step {
        retries, summary, ..
    } = *entry
    else {
        panic!("reopened step")
    };
    assert_eq!(retries, notices);
    assert_eq!(summary, Some(detail.summary));
    drop(view);
    client.stop().await.unwrap();
    serving.await.unwrap().unwrap();
}
#[tokio::test(start_paused = true)]
async fn stream_turn_final_provider_failure_only_emits_terminal_notice() {
    for retryable in [false, true] {
        let count = if retryable { 3 } else { 1 };
        let mut builder = lattice_agent::testing::FakeStreamingLlmClient::builder();
        for attempt in 0..count {
            builder = builder.push_llm_error(lattice_llm::LlmError::Transport {
                message: format!("provider failure {attempt}"),
                retryable,
            });
        }
        let fixture = setup_fixture(Arc::new(builder.build()), &[]);
        let mut request = fixture.request.clone();
        request.loaded_profile.as_mut().unwrap().profile.llm.retry =
            lattice_profile::LlmRetrySection {
                max_retries: 2,
                initial_delay_ms: 1,
                max_delay_ms: 1,
            };
        let (detail, events) = fixture.run(request).await;
        let attempts = &detail.steps[0].attempts;
        assert_eq!(attempts.len(), count);
        let retries: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                TurnEvent::LlmRetry { key, notice, .. } => Some((key, notice)),
                _ => None,
            })
            .collect();
        assert_eq!(retries.len(), count - 1);
        assert_eq!(attempts.last().unwrap().retry_delay_ms, None);
        for (ordinal, (key, notice)) in retries.iter().enumerate() {
            assert_eq!(key.attempt_ordinal as usize, ordinal);
            assert_eq!(notice.level, NoticeLevel::Warn);
            let lattice_session::store::AttemptEndDetail::Stopped { diagnostic, .. } =
                &attempts[ordinal].end
            else {
                panic!("stopped attempt")
            };
            assert_eq!(Some(&notice.text), diagnostic.as_ref());
        }
        assert!(matches!(
            &events[events.len() - 2],
            TurnEvent::ModelStarted { key, .. } if key.attempt_ordinal as usize == count - 1
        ));
        let TurnEvent::TurnFailed { summary, .. } = events.last().unwrap() else {
            panic!("terminal failure must follow the final request")
        };
        assert_eq!(summary, &detail.summary);
        let notice = summary.notice.as_ref().unwrap();
        assert_eq!(notice.level, NoticeLevel::Error);
        let lattice_session::store::AttemptEndDetail::Stopped { diagnostic, .. } =
            &attempts.last().unwrap().end
        else {
            panic!("final stopped attempt")
        };
        assert_eq!(Some(&notice.text), diagnostic.as_ref());
        assert!(
            notice
                .text
                .contains(&format!("provider failure {}", count - 1))
        );
    }
}

async fn wait_retry(observer: &ObserverSink) {
    let mut changed = observer.subscribe();
    loop {
        if observer.events().iter().any(|event| {
            matches!(
                event,
                TurnEvent::LlmRetry {
                    delay_ms: 10_000,
                    ..
                }
            )
        }) {
            break;
        }
        changed.changed().await.unwrap();
    }
}
#[tokio::test(start_paused = true)]
async fn stream_turn_retry_cancellation_closes_without_another_request() {
    let llm = timed(1);
    let fixture = setup_fixture(llm.clone(), &[]);
    let cancel = Arc::new(AtomicBool::new(false));
    let mut request = fixture.request.clone();
    request.cancel_flag = Some(cancel.clone());
    request.loaded_profile.as_mut().unwrap().profile.llm.retry = lattice_profile::LlmRetrySection {
        max_retries: 1,
        initial_delay_ms: 10_000,
        max_delay_ms: 10_000,
    };
    let (execution, observer) = fixture.start(request);
    execution.acceptance.await.unwrap().unwrap();
    wait_retry(&observer).await;
    tokio::time::advance(Duration::from_millis(100)).await;
    assert_eq!(
        llm.starts.lock().unwrap().len(),
        1,
        "retry must still be waiting"
    );
    cancel.store(true, Ordering::Release);
    let TurnCompletion::Closed(detail) =
        tokio::time::timeout(Duration::from_secs(1), execution.completion)
            .await
            .expect("cancel must interrupt backoff")
            .unwrap()
            .unwrap();
    assert_eq!(llm.starts.lock().unwrap().len(), 1);
    assert_eq!(detail.summary.outcome, PublicTurnOutcome::Cancelled);
}
#[tokio::test(start_paused = true)]
async fn stream_turn_retry_respects_explicit_and_default_deadlines() {
    for (timeout, seconds, count) in [(Some(1), 1, 1), (None, 300, 5)] {
        let llm = timed(10);
        let fixture = setup_fixture(llm.clone(), &[]);
        let mut request = fixture.request.clone();
        let config = &mut request.loaded_profile.as_mut().unwrap().profile.llm;
        config.timeout_secs = timeout;
        config.retry = lattice_profile::LlmRetrySection {
            max_retries: 10,
            initial_delay_ms: 60_000,
            max_delay_ms: 60_000,
        };
        let (detail, events) =
            tokio::time::timeout(Duration::from_secs(seconds + 1), fixture.run(request))
                .await
                .expect("deadline must interrupt backoff");
        let starts = llm.starts.lock().unwrap();
        assert_eq!(starts.len(), count);
        assert!(
            starts
                .iter()
                .all(|at| *at < starts[0] + Duration::from_secs(seconds))
        );
        let delays: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                TurnEvent::LlmRetry { delay_ms, .. } => Some(*delay_ms),
                _ => None,
            })
            .collect();
        assert_eq!(
            delays,
            vec![seconds.min(60) * 1_000; count],
            "plan must be capped by remaining Turn budget"
        );
        assert_eq!(detail.summary.outcome, PublicTurnOutcome::Failed);
        assert!(matches!(
            &detail.steps[0].progress,
            StepProgress::Closed {
                end: StepEnd::Stop {
                    cause: StopCause::Deadline
                }
            }
        ));
    }
}

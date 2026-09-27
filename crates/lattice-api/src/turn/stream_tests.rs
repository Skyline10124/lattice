// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0
use super::{observer::ObserverSink, test_support::*, types::*};
use lattice_llm::LlmEvent;
use lattice_session::store::{PublicTurnOutcome, StepDetailState, StepEnd, StopCause};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

#[tokio::test]
async fn stream_turn_error_after_partial_closes_without_persisting_partial() {
    let fixture = setup_with_events(
        vec![
            LlmEvent::Token {
                content: "partial output".into(),
            },
            LlmEvent::Token {
                content: " before error".into(),
            },
            LlmEvent::Error {
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
        matches!(&events.last().unwrap(), TurnEvent::TurnFailed { summary, .. } if summary.notice.as_ref().unwrap().text.contains("connection reset"))
    );
    assert_eq!(detail.summary.unwrap().outcome, PublicTurnOutcome::Failed);
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
    ) -> Result<lattice_llm::LlmStream, Box<dyn std::error::Error + Send + Sync>> {
        self.starts
            .lock()
            .unwrap()
            .push(tokio::time::Instant::now());
        self.inner.stream_chat(request).await
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
            TurnEvent::LlmRequestError {
                key,
                will_retry,
                delay_ms,
                ..
            } => Some((key.attempt_ordinal, *will_retry, *delay_ms)),
            _ => None,
        })
        .collect();
    assert_eq!(
        delays,
        vec![
            (0, true, Some(40)),
            (1, true, Some(60)),
            (2, true, Some(60))
        ]
    );
    assert_eq!(detail.steps[0].attempts.len(), 4);
    let starts = llm.starts.lock().unwrap();
    assert_eq!(starts.len(), 4);
    for (pair, delay) in starts.windows(2).zip([40, 60, 60]) {
        assert_eq!(pair[1] - pair[0], Duration::from_millis(delay));
    }
    assert_eq!(
        detail.summary.unwrap().outcome,
        PublicTurnOutcome::Completed
    );
}
async fn wait_retry(observer: &ObserverSink) {
    let mut changed = observer.subscribe();
    loop {
        if let Some(events) = observer.events_after(0)
            && events.iter().any(|event| {
                matches!(
                    event.event,
                    TurnEvent::LlmRequestError {
                        will_retry: true,
                        delay_ms: Some(10_000),
                        ..
                    }
                )
            })
        {
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
            .unwrap()
    else {
        panic!("closed")
    };
    assert_eq!(llm.starts.lock().unwrap().len(), 1);
    assert_eq!(
        detail.summary.as_ref().unwrap().outcome,
        PublicTurnOutcome::Cancelled
    );
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
                TurnEvent::LlmRequestError { delay_ms, .. } => Some(*delay_ms),
                _ => None,
            })
            .collect();
        assert_eq!(
            delays,
            vec![Some(seconds.min(60) * 1_000); count],
            "plan must be capped by remaining Turn budget"
        );
        assert_eq!(
            detail.summary.as_ref().unwrap().outcome,
            PublicTurnOutcome::Failed
        );
        assert!(matches!(
            &detail.steps[0].state,
            StepDetailState::Closed {
                end: StepEnd::Stop {
                    cause: StopCause::Deadline,
                    ..
                },
                ..
            }
        ));
    }
}

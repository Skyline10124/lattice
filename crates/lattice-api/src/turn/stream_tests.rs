// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0
use futures::StreamExt;
use lattice_llm::LlmEvent;

use super::execute::stream_turn;
use super::test_support::*;
use super::types::TurnEvent;
#[tokio::test]
async fn stream_turn_error_after_partial_commits_error() {
    let events = vec![
        LlmEvent::Token {
            content: "partial output".to_string(),
        },
        LlmEvent::Token {
            content: " before error".to_string(),
        },
        LlmEvent::Error {
            message: "LLM error: connection reset".to_string(),
        },
    ];
    let fixture = setup_with_events(events, &[]);

    let events: Vec<TurnEvent> = stream_turn(
        fixture.session_api,
        with_test_llm(
            make_request("hello"),
            fixture.llm.clone(),
            &fixture.enabled_tools,
            &fixture.project_root,
        ),
    )
    .await
    .unwrap()
    .collect()
    .await;

    let failed = events
        .iter()
        .find(|e| matches!(e, TurnEvent::TurnFailed { .. }))
        .expect("should have TurnFailed event");
    if let TurnEvent::TurnFailed { reason } = failed {
        assert!(reason.contains("connection reset"));
    }

    let committed = events
        .iter()
        .find(|e| matches!(e, TurnEvent::TurnCommitted { .. }))
        .expect("should have TurnCommitted event");
    if let TurnEvent::TurnCommitted {
        outcome,
        session_id,
        ..
    } = committed
    {
        assert!(matches!(
            outcome,
            lattice_session::TurnOutcome::Interrupted { .. }
        ));
        let transcript = fixture.store.as_ref().head_transcript(session_id).unwrap();
        // ADR 0005 §7 (#139-B Commit 4): partial assistant output from an
        // interrupted attempt is diagnostic state, never committed
        // conversation. The turn still commits with the typed error reason.
        assert!(
            !transcript
                .messages
                .iter()
                .any(|m| m.role == lattice_conversation::MessageRole::Assistant),
            "interrupted partial output must not be committed"
        );
    }
}

struct TimedLlm {
    inner: lattice_agent::testing::FakeStreamingLlmClient,
    starts: std::sync::Mutex<Vec<tokio::time::Instant>>,
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

#[tokio::test(start_paused = true)]
async fn stream_turn_retry_uses_profile_backoff_and_cap() {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    let llm = Arc::new(TimedLlm {
        inner: lattice_agent::testing::FakeStreamingLlmClient::builder()
            .push_llm_error(opening_failure())
            .push_llm_error(opening_failure())
            .push_llm_error(opening_failure())
            .push_events(happy_events())
            .build(),
        starts: Mutex::new(Vec::new()),
    });
    let fixture = setup_fixture(llm.clone(), &[]);
    let mut request = make_request("retry with configured backoff");
    request.loaded_profile.as_mut().unwrap().profile.llm.retry = lattice_profile::LlmRetrySection {
        max_retries: 3,
        initial_delay_ms: 40,
        max_delay_ms: 60,
    };
    let events: Vec<_> = stream_turn(
        fixture.session_api,
        with_test_llm(request, llm.clone(), &[], &fixture.project_root),
    )
    .await
    .unwrap()
    .collect()
    .await;

    let delays: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            TurnEvent::LlmRequestError {
                attempt,
                max_attempts,
                will_retry,
                delay_ms,
                ..
            } => Some((*attempt, *max_attempts, *will_retry, *delay_ms)),
            _ => None,
        })
        .collect();
    assert_eq!(
        delays,
        vec![
            (1, 4, true, Some(40)),
            (2, 4, true, Some(60)),
            (3, 4, true, Some(60))
        ]
    );
    let starts = llm.starts.lock().unwrap();
    assert_eq!(starts.len(), 4);
    for (pair, delay_ms) in starts.windows(2).zip([40, 60, 60]) {
        assert_eq!(pair[1] - pair[0], Duration::from_millis(delay_ms));
    }
    assert!(events.iter().any(|event| matches!(
        event,
        TurnEvent::TurnCommitted {
            outcome: lattice_session::TurnOutcome::Completed { .. },
            ..
        }
    )));
}

#[tokio::test(start_paused = true)]
async fn stream_turn_retry_cancellation_closes_without_another_request() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    let llm = Arc::new(
        lattice_agent::testing::FakeStreamingLlmClient::builder()
            .push_llm_error(opening_failure())
            .push_events(happy_events())
            .build(),
    );
    let fixture = setup_fixture(llm.clone(), &[]);
    let cancel = Arc::new(AtomicBool::new(false));
    let mut request = make_request("cancel during retry backoff");
    request.cancel_flag = Some(cancel.clone());
    request.loaded_profile.as_mut().unwrap().profile.llm.retry = lattice_profile::LlmRetrySection {
        max_retries: 1,
        initial_delay_ms: 10_000,
        max_delay_ms: 10_000,
    };
    let mut stream = stream_turn(
        fixture.session_api,
        with_test_llm(request, llm.clone(), &[], &fixture.project_root),
    )
    .await
    .unwrap();
    loop {
        if let TurnEvent::LlmRequestError {
            will_retry,
            delay_ms,
            ..
        } = stream.next().await.expect("retry event before stream ends")
        {
            assert!(will_retry);
            assert_eq!(delay_ms, Some(10_000));
            break;
        }
    }
    tokio::time::advance(Duration::from_millis(100)).await;
    assert_eq!(llm.call_count(), 1, "the retry must still be waiting");
    cancel.store(true, Ordering::Relaxed);
    let events = tokio::time::timeout(Duration::from_secs(1), stream.collect::<Vec<_>>())
        .await
        .expect("cancellation must close without waiting out the 10-second backoff");
    assert_eq!(
        llm.call_count(),
        1,
        "cancellation must prevent a second request"
    );
    assert!(events.iter().any(|event| matches!(
        event,
        TurnEvent::TurnCommitted {
            outcome: lattice_session::TurnOutcome::Cancelled {
                reason: lattice_session::pending::TurnCancellation::UserCancelled,
            },
            ..
        }
    )));
}

#[tokio::test(start_paused = true)]
async fn stream_turn_retry_respects_explicit_and_default_deadlines() {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    for (timeout_secs, expected_seconds, expected_requests) in [(Some(1), 1, 1), (None, 300, 5)] {
        let mut builder = lattice_agent::testing::FakeStreamingLlmClient::builder();
        for _ in 0..10 {
            builder = builder.push_llm_error(opening_failure());
        }
        let llm = Arc::new(TimedLlm {
            inner: builder.push_events(happy_events()).build(),
            starts: Mutex::new(Vec::new()),
        });
        let fixture = setup_fixture(llm.clone(), &[]);
        let mut request = make_request("deadline during retry backoff");
        let config = &mut request.loaded_profile.as_mut().unwrap().profile.llm;
        config.timeout_secs = timeout_secs;
        config.retry = lattice_profile::LlmRetrySection {
            max_retries: 10,
            initial_delay_ms: 60_000,
            max_delay_ms: 60_000,
        };
        let stream = stream_turn(
            fixture.session_api,
            with_test_llm(request, llm.clone(), &[], &fixture.project_root),
        )
        .await
        .unwrap();
        let events = tokio::time::timeout(
            Duration::from_secs(expected_seconds + 1),
            stream.collect::<Vec<_>>(),
        )
        .await
        .expect("deadline must end the wait, not merely reject a request after the backoff");
        let starts = llm.starts.lock().unwrap();
        assert_eq!(starts.len(), expected_requests);
        assert!(
            starts
                .iter()
                .all(|at| *at < starts[0] + Duration::from_secs(expected_seconds))
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
            vec![Some(expected_seconds.min(60) * 1_000); expected_requests],
            "the plan must be capped by the remaining Turn budget"
        );
        assert!(events.iter().any(|event| matches!(
            event,
            TurnEvent::TurnCommitted {
                outcome: lattice_session::TurnOutcome::Interrupted {
                    reason: lattice_session::pending::TurnInterruption::ModelAttempt(
                        lattice_conversation::AttemptInterruption::Timeout,
                    ),
                },
                ..
            }
        )));
    }
}

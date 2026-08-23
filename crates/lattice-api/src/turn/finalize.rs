// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

// Private #139-B module until the Commit 6 production switch wires it into
// the coordinator's terminal publication path.
#![allow(dead_code)]

//! The fixed terminal-finalization budget (ADR 0005 §12, #139-B Commit 5).
//!
//! `FinalizeBudget` is a private, fixed policy — neither profile
//! configuration nor public API: a monotonic 60-second envelope anchored
//! immediately after the executor returns. It covers repository-lock
//! acquisition wait, exact uncertain-draft revalidation, terminal/evidence
//! publication attempts, and retry delays. The first attempt is immediate;
//! retriable durability failures back off exponentially from 25 ms capped at
//! 1 second; no new attempt starts after the deadline; corruption, topology,
//! binding, identity, material and digest mismatches never retry.
//!
//! The one immutable terminal candidate — identity, real execution outcome,
//! material, digests, and typed evidence — is frozen by the caller before
//! this executor runs; every retry publishes byte-identical facts.

use std::future::Future;
use std::time::Duration;

use lancet_audit::invocation_store::InvocationStoreError;
use tokio::time::Instant;

/// The fixed budget envelope: 60 seconds, monotonic (ADR 0005 §12).
pub(crate) const FINALIZE_BUDGET: Duration = Duration::from_secs(60);

/// First retry backoff (ADR 0005 §12): 25 ms, exponential.
const BACKOFF_INITIAL: Duration = Duration::from_millis(25);

/// Backoff ceiling (ADR 0005 §12): 1 second.
const BACKOFF_MAX: Duration = Duration::from_secs(1);

/// The caller-visible typed failure when terminal/evidence publication
/// cannot complete inside the budget (#139-B Commit 5; promoted to the
/// public API surface with the Commit 6 switch).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum FinalizeFailure<E> {
    /// The budget expired (or the next attempt could not start before the
    /// deadline) while a retriable durability failure was outstanding. The
    /// invocation remains `DispatchStarted`; the journal stays at its
    /// existing stage; nothing later is published.
    #[error("terminal evidence durability failed: budget expired after {attempts} attempt(s)")]
    EvidenceDurabilityFailed {
        /// Publication attempts actually started.
        attempts: u32,
    },
    /// A non-retriable failure (corruption, topology, binding, identity,
    /// material or digest mismatch). Reported immediately; never retried.
    #[error("terminal publication failed closed: {0}")]
    Fatal(#[source] E),
}

/// A monotonic budget anchored at executor return.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FinalizeBudget {
    deadline: Instant,
}

impl FinalizeBudget {
    /// Anchor the budget at `anchor` — the moment the Tool executor
    /// returned. Repository-lock wait charged against the budget happens
    /// after this point by construction.
    pub(crate) fn anchored_at(anchor: Instant) -> Self {
        Self {
            deadline: anchor + FINALIZE_BUDGET,
        }
    }

    /// Whether one more attempt may start at `now`. The check runs before
    /// every attempt; an attempt is never started after the deadline.
    pub(crate) fn allows_attempt(&self, now: Instant) -> bool {
        now < self.deadline
    }

    /// Remaining envelope at `now` (zero once expired).
    pub(crate) fn remaining(&self, now: Instant) -> Duration {
        self.deadline.saturating_duration_since(now)
    }
}

/// Deterministic exponential backoff: 0 for the first attempt, then 25 ms
/// doubling, capped at 1 second.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FinalizeBackoff {
    exponent: u32,
}

impl Default for FinalizeBackoff {
    fn default() -> Self {
        Self { exponent: 0 }
    }
}

impl FinalizeBackoff {
    /// Delay before the attempt with this ordinal (1-based): attempt 1 is
    /// immediate; attempt N waits `25ms * 2^(N-2)` capped at 1 s.
    pub(crate) fn delay_before(&mut self, attempt: u32) -> Duration {
        if attempt <= 1 {
            return Duration::ZERO;
        }
        let shifts = (attempt - 2).min(16);
        let delay = BACKOFF_INITIAL.saturating_mul(1u32 << shifts);
        delay.min(BACKOFF_MAX)
    }
}

/// Injectable time for the finalize executor: production uses tokio's
/// monotonic clock and real sleeps; tests step a fake clock, which is how
/// the tests assert the budget begins at executor return.
pub(crate) trait FinalizeClock: Send + Sync {
    fn now(&self) -> Instant;
    fn sleep(&self, delay: Duration) -> impl Future<Output = ()> + Send;
}

/// Production clock.
pub(crate) struct TokioFinalizeClock;

impl FinalizeClock for TokioFinalizeClock {
    fn now(&self) -> Instant {
        Instant::now()
    }

    async fn sleep(&self, delay: Duration) {
        tokio::time::sleep(delay).await;
    }
}

/// Outcome of one publication attempt as seen by [`finalize_under_budget`]:
/// the attempt's typed error, already classified for retryability.
pub(crate) enum AttemptOutcome {
    /// Published durably.
    Published,
    /// The attempt failed; `retryable` mirrors
    /// [`InvocationStoreError::is_durability_retryable`].
    Failed { error: InvocationStoreError },
}

/// Publish one frozen terminal candidate under the fixed budget.
///
/// `attempt` performs one publication attempt (repository-lock acquisition,
/// exact revalidation, and the append itself); it must publish the same
/// byte-identical candidate on every call. The first attempt runs
/// immediately; retriable durability failures back off per
/// [`FinalizeBackoff`]; the deadline is checked before every attempt.
pub(crate) async fn finalize_under_budget<C, F, Fut>(
    clock: &C,
    budget: FinalizeBudget,
    mut attempt: F,
) -> Result<(), FinalizeFailure<InvocationStoreError>>
where
    C: FinalizeClock + ?Sized,
    F: FnMut() -> Fut,
    Fut: Future<Output = AttemptOutcome>,
{
    let mut backoff = FinalizeBackoff::default();
    let mut attempt_ordinal: u32 = 0;
    loop {
        let now = clock.now();
        if !budget.allows_attempt(now) {
            return Err(FinalizeFailure::EvidenceDurabilityFailed {
                attempts: attempt_ordinal,
            });
        }
        let delay = backoff.delay_before(attempt_ordinal.saturating_add(1));
        if !delay.is_zero() {
            clock.sleep(delay).await;
            // The sleep itself is inside the budget: re-check before the
            // attempt starts.
            if !budget.allows_attempt(clock.now()) {
                return Err(FinalizeFailure::EvidenceDurabilityFailed {
                    attempts: attempt_ordinal,
                });
            }
        }
        attempt_ordinal = attempt_ordinal.saturating_add(1);
        match attempt().await {
            AttemptOutcome::Published => return Ok(()),
            AttemptOutcome::Failed { error } => {
                if !error.is_durability_retryable() {
                    return Err(FinalizeFailure::Fatal(error));
                }
                // Loop: the next iteration checks the deadline, then backs off.
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::sync::atomic::{AtomicU64, Ordering};

    use lancet_audit::invocation_store::InvocationStore;
    use lattice_session::{ProjectStoreKey, StateLayout};

    /// Stepping fake clock: `now()` returns the accumulated virtual time;
    /// `sleep(d)` advances it without real waiting.
    struct FakeClock {
        elapsed_ms: AtomicU64,
    }

    impl FakeClock {
        fn new() -> Self {
            Self {
                elapsed_ms: AtomicU64::new(0),
            }
        }

        fn advance_ms(&self, ms: u64) {
            self.elapsed_ms.fetch_add(ms, Ordering::SeqCst);
        }
    }

    impl FinalizeClock for FakeClock {
        fn now(&self) -> Instant {
            Instant::now() + Duration::from_millis(self.elapsed_ms.load(Ordering::SeqCst))
        }

        async fn sleep(&self, delay: Duration) {
            self.advance_ms(delay.as_millis() as u64);
        }
    }

    fn durability_error() -> InvocationStoreError {
        InvocationStoreError::Store(make_io_error())
    }

    fn make_io_error() -> lattice_session::StoreError {
        lattice_session::StoreError::Io {
            operation: "testing",
            path: std::path::PathBuf::from("/tmp/x"),
            source: std::io::Error::other("injected"),
        }
    }

    fn corruption_error() -> InvocationStoreError {
        InvocationStoreError::Corruption {
            detail: "identity mismatch".to_string(),
        }
    }

    #[test]
    fn budget_starts_at_anchor_and_allows_the_exact_boundary() {
        let anchor = Instant::now();
        let budget = FinalizeBudget::anchored_at(anchor);
        assert!(budget.allows_attempt(anchor));
        assert!(budget.allows_attempt(anchor + FINALIZE_BUDGET - Duration::from_millis(1)));
        assert!(!budget.allows_attempt(anchor + FINALIZE_BUDGET));
    }

    #[test]
    fn backoff_schedule_is_25ms_exponential_capped_at_1s() {
        let mut backoff = FinalizeBackoff::default();
        assert_eq!(backoff.delay_before(1), Duration::ZERO);
        assert_eq!(backoff.delay_before(2), Duration::from_millis(25));
        assert_eq!(backoff.delay_before(3), Duration::from_millis(50));
        assert_eq!(backoff.delay_before(4), Duration::from_millis(100));
        assert_eq!(backoff.delay_before(7), Duration::from_millis(800));
        assert_eq!(backoff.delay_before(8), Duration::from_millis(1000));
        assert_eq!(backoff.delay_before(30), Duration::from_millis(1000));
    }

    async fn run_finalizer(
        clock: &FakeClock,
        failures_before_success: usize,
        error: impl Fn() -> InvocationStoreError + Send + Sync,
        payload: usize,
        attempts: &std::sync::Mutex<Vec<usize>>,
    ) -> Result<(), FinalizeFailure<InvocationStoreError>> {
        let budget = FinalizeBudget::anchored_at(clock.now());
        let mut seen = 0usize;
        let attempts = &attempts;
        let error = &error;
        let payload = &payload;
        let failures = failures_before_success;
        finalize_under_budget(clock, budget, || {
            // State mutates synchronously at closure-call time; the future
            // carries the precomputed copy (an async-move block that mutated
            // the capture would copy and lose writes).
            seen += 1;
            attempts.lock().unwrap().push(*payload);
            let outcome = if seen > failures {
                AttemptOutcome::Published
            } else {
                AttemptOutcome::Failed { error: error() }
            };
            async move { outcome }
        })
        .await
    }

    #[tokio::test]
    async fn first_attempt_is_immediate_and_success_publishes() {
        let clock = FakeClock::new();
        let attempts = std::sync::Mutex::new(Vec::new());
        run_finalizer(&clock, 0, durability_error, 42, &attempts)
            .await
            .expect("publishes");
        // The clock did not move: the first attempt is immediate.
        assert_eq!(clock.elapsed_ms.load(Ordering::SeqCst), 0);
        assert_eq!(*attempts.lock().unwrap(), vec![42]);
    }

    #[tokio::test]
    async fn retryable_failures_back_off_and_publish_identical_candidates() {
        let clock = FakeClock::new();
        let attempts = std::sync::Mutex::new(Vec::new());
        run_finalizer(&clock, 3, durability_error, 7, &attempts)
            .await
            .expect("publishes on the fourth attempt");
        assert_eq!(*attempts.lock().unwrap(), vec![7, 7, 7, 7]);
        // 25 + 50 + 100 ms of scheduled backoff before the final attempt.
        assert_eq!(clock.elapsed_ms.load(Ordering::SeqCst), 175);
    }

    #[tokio::test]
    async fn corruption_never_retries() {
        let clock = FakeClock::new();
        let attempts = std::sync::Mutex::new(Vec::new());
        let failure = run_finalizer(&clock, 5, corruption_error, 1, &attempts)
            .await
            .expect_err("fatal");
        assert!(matches!(
            failure,
            FinalizeFailure::Fatal(InvocationStoreError::Corruption { .. })
        ));
        assert_eq!(attempts.lock().unwrap().len(), 1);
        assert_eq!(clock.elapsed_ms.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn no_attempt_starts_after_the_deadline() {
        let clock = FakeClock::new();
        // Anchor at executor return, then consume the whole budget with
        // repository-lock wait: lock acquisition is inside the budget, so
        // the first publication attempt may not start.
        let budget = FinalizeBudget::anchored_at(clock.now());
        clock.advance_ms(FINALIZE_BUDGET.as_millis() as u64);
        let attempts = std::sync::Mutex::new(Vec::new());
        let failure = finalize_under_budget(&clock, budget, || async {
            attempts.lock().unwrap().push(1);
            AttemptOutcome::Published
        })
        .await
        .expect_err("budget expired");
        assert!(matches!(
            failure,
            FinalizeFailure::EvidenceDurabilityFailed { attempts: 0 }
        ));
        assert!(attempts.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_backoff_sleep_that_crosses_the_deadline_stops_before_attempting() {
        let clock = FakeClock::new();
        let attempts = std::sync::Mutex::new(Vec::new());
        // Anchor, run the first (immediate) attempt at t=59.980s, fail it
        // retryably; the 25 ms backoff then crosses the deadline, so no
        // second attempt starts.
        let budget = FinalizeBudget::anchored_at(clock.now());
        clock.advance_ms(FINALIZE_BUDGET.as_millis() as u64 - 20);
        let failure = finalize_under_budget(&clock, budget, || async {
            attempts.lock().unwrap().push(1);
            AttemptOutcome::Failed {
                error: durability_error(),
            }
        })
        .await
        .expect_err("budget expires inside the backoff");
        assert!(matches!(
            failure,
            FinalizeFailure::EvidenceDurabilityFailed { attempts: 1 }
        ));
        assert_eq!(attempts.lock().unwrap().len(), 1);
        // The failed 25 ms sleep still ran inside the budget.
        assert_eq!(
            clock.elapsed_ms.load(Ordering::SeqCst),
            (FINALIZE_BUDGET.as_millis() - 20 + 25) as u64
        );
    }

    // ─── Retryability classification over the real store error taxonomy ────

    #[test]
    fn store_error_retryability_split() {
        let retryable = [
            InvocationStoreError::Store(make_io_error()),
            InvocationStoreError::Store(lattice_session::StoreError::Durability {
                operation: "fsync",
                path: std::path::PathBuf::from("/tmp/x"),
                source: std::io::Error::other("injected"),
            }),
            InvocationStoreError::ConcurrentRead,
        ];
        for error in retryable {
            assert!(error.is_durability_retryable(), "{error:?}");
        }
        let fatal = [
            corruption_error(),
            InvocationStoreError::NotFound {
                id: "00000000-0000-4000-8000-000000000001".parse().unwrap(),
            },
            InvocationStoreError::InvalidTransition {
                id: "00000000-0000-4000-8000-000000000001".parse().unwrap(),
                expected: "Reserved".to_string(),
                actual: "Terminal".to_string(),
            },
        ];
        for error in fatal {
            assert!(!error.is_durability_retryable(), "{error:?}");
        }
    }

    // ─── End-to-end against a real Git store with a failing shim ──────────

    /// Real-store integration: the first attempt fails with an injected
    /// durability error, the second publishes the frozen terminal onto the
    /// real ledger, and the budget observes both attempts.
    #[test]
    fn real_store_publication_retries_within_budget() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        runtime.block_on(async {
            let tmp = tempfile::tempdir().unwrap();
            let layout = StateLayout::in_state_root(tmp.path());
            let key = ProjectStoreKey::derive(tmp.path()).unwrap();
            let store = InvocationStore::create(&layout, &key).unwrap();

            let turn_id = lancet_protocol::TurnId::parse("3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c")
                .expect("turn id");
            let plan_key = lancet_protocol::PlanKey {
                turn_id,
                model_step_ordinal: 0,
            };
            let invocation =
                lancet_protocol::InvocationId::parse("00000000-0000-4000-8000-000000000001")
                    .unwrap();
            let binding = lancet_audit::invocation_store::ReservationBinding {
                invocation_id: invocation,
                identity: lancet_audit::invocation_store::BindingIdentity {
                    session_id: "3f8b2c1a7d4e4f5a8b9c0d1e2f3a4b5c".parse().unwrap(),
                    turn_id,
                    model_step_ordinal: 0,
                    part_ordinal: 0,
                    method: "test.echo".to_string(),
                    descriptor_identity: "d".repeat(64),
                    arguments_digest: [0u8; 32],
                },
            };
            store.reserve_batch(plan_key, &[binding]).unwrap();
            store.dispatch_started(invocation).unwrap();

            // One immutable terminal candidate: the exact value every retry
            // publishes, byte-identically.
            let candidate = (
                invocation,
                lancet_audit::invocation_store::TerminalOutcome::Succeeded,
                lancet_audit::invocation_store::TerminalMaterial::Available {
                    digest: [7u8; 32],
                    byte_length: 9,
                },
                terminal_evidence(),
            );

            let clock = FakeClock::new();
            let budget = FinalizeBudget::anchored_at(clock.now());
            let injected_failures = std::sync::atomic::AtomicUsize::new(1);
            let attempts = std::sync::atomic::AtomicUsize::new(0);

            finalize_under_budget(&clock, budget, || async {
                attempts.fetch_add(1, Ordering::SeqCst);
                if injected_failures.fetch_sub(1, Ordering::AcqRel) > 0 {
                    return AttemptOutcome::Failed {
                        error: durability_error(),
                    };
                }
                let (invocation, outcome, material, evidence) = &candidate;
                let result = store.publish_terminal(
                    *invocation,
                    outcome.clone(),
                    material.clone(),
                    evidence.clone(),
                );
                match result {
                    Ok(()) => AttemptOutcome::Published,
                    Err(error) => AttemptOutcome::Failed { error },
                }
            })
            .await
            .expect("publishes within budget");

            assert_eq!(attempts.load(Ordering::SeqCst), 2);
            // The 25 ms backoff ran inside the budget.
            assert_eq!(clock.elapsed_ms.load(Ordering::SeqCst), 25);
            let records = store.query_invocations(&[invocation]).unwrap();
            assert!(matches!(
                records.get(&invocation).unwrap().state,
                lancet_audit::invocation_store::InvocationState::Terminal { .. }
            ));
        });
    }

    fn terminal_evidence() -> lancet_audit::invocation_store::TerminalEvidence {
        use lancet_protocol::{
            ADMISSION_EVIDENCE_VERSION, AdmissionEvidence, DENIED_OPERATIONS_RECORD_VERSION,
            DeniedOperationsRecord, OBSERVED_EFFECTS_RECORD_VERSION, ObservedEffectsRecord,
        };
        lancet_audit::invocation_store::TerminalEvidence {
            admission: AdmissionEvidence {
                version: ADMISSION_EVIDENCE_VERSION,
                tool: "test.echo".to_string(),
                approved_capabilities: Vec::new(),
                denied_capabilities: Vec::new(),
                boundary_excluded: Vec::new(),
                max_boundary: lancet_protocol::Trust::In,
            },
            effects: ObservedEffectsRecord {
                version: OBSERVED_EFFECTS_RECORD_VERSION,
                effects: Vec::new(),
            },
            denied: DeniedOperationsRecord {
                version: DENIED_OPERATIONS_RECORD_VERSION,
                denied: Vec::new(),
            },
        }
    }
}

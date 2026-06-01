//! Observed-effect ledger: an Arc-backed handle that records semantic side effects
//! observed during CEP method dispatch.
//!
//! The ledger mirrors the [`CallDurableMetadata`] Arc-handle pattern: it is cloned
//! into [`HandlerContext`] before `registry.dispatch` and snapshotted from the outer
//! handle after dispatch returns (since `ctx` is moved into the handler).
//!
//! Mutex poison recovery: a handler that panics while holding the ledger mutex
//! poisons it. Rather than propagating the poison via `.expect()` (which would crash
//! every subsequent handler in the turn — a denial-of-service vector), we recover
//! the poisoned guard and emit a `tracing::warn!`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use lancet_catalog::method::RequiredCapability;
use lancet_protocol::envelope::ImpactKind;

use super::region::AuthorizedEffectRegion;

/// Source of an observed effect — determines whether the effect counts toward
/// the agent-visible effect contract.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum EffectSource {
    /// Effect produced by the handler's backend IO (file read/write, network fetch,
    /// command execution). Counts toward the effect contract check.
    HandlerBackend,

    /// Internal snapshot or probe used for effect-region analysis (e.g. pre/post
    /// workspace diff). Excluded from the agent-visible effect contract.
    EffectProbe,

    /// Internal probe performed during guard evaluation. Excluded from the
    /// agent-visible effect contract.
    GuardProbe,

    /// Effect produced by the envelope verify command. Recorded to audit but must
    /// not charge `RunCommand`/`CommandExecution` to the mutation method that
    /// triggered the verify — verify is an envelope-internal constraint.
    Verify,
}

/// Runtime scope of an observed effect — carries concrete values (paths, hosts)
/// rather than the static [`EffectScopeKind`] classes.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ObservedScope {
    /// A concrete file path was affected.
    Path(PathBuf),
    /// A root/directory region was affected (e.g. project root for observations).
    Root(String),
    /// An external host was contacted.
    Host(String),
    /// Session state was mutated (e.g. todo_write).
    SessionState,
}

/// A single observed semantic effect recorded during method dispatch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservedEffect {
    /// The impact kind of this effect (e.g. WorkspaceObservation, WorkspaceMutation).
    pub kind: ImpactKind,
    /// The concrete runtime scope of the effect.
    pub scope: ObservedScope,
    /// The capability required to perform this effect.
    pub required_capability: RequiredCapability,
    /// The source that produced this effect (handler backend, probe, or verify).
    pub source: EffectSource,
}

/// Arc-backed handle for accumulating observed effects during a single CEP dispatch.
///
/// Mirrors `CallDurableMetadata` exactly: `Clone + Debug + Default`, with `record()`
/// and `snapshot()` accessors. The Arc interior is shared across clones, so recording
/// through one clone is visible when reading through another.
#[derive(Clone, Debug, Default)]
pub struct EffectLedger(Arc<Mutex<Vec<ObservedEffect>>>);

impl EffectLedger {
    /// Create a new empty ledger.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record an observed effect into the ledger.
    pub fn record(&self, effect: ObservedEffect) {
        let mut guard = self.lock_recover();
        guard.push(effect);
    }

    /// Take a snapshot of all observed effects recorded so far.
    pub fn snapshot(&self) -> Vec<ObservedEffect> {
        let guard = self.lock_recover();
        guard.clone()
    }

    /// Recover the mutex guard, handling poisoned mutexes gracefully.
    ///
    /// A handler that panics while holding the ledger mutex poisons it. Rather than
    /// crashing every subsequent handler (denial-of-service), we recover the guard
    /// from the poisoned mutex and log a warning. The ledger remains functional.
    fn lock_recover(&self) -> MutexGuard<'_, Vec<ObservedEffect>> {
        match self.0.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                tracing::warn!(
                    "effect ledger mutex poisoned by a panicking handler; recovering guard"
                );
                poisoned.into_inner()
            }
        }
    }
}

/// Arc-backed capture slot for the per-call authorized effect region.
///
/// Mirrors the `EffectLedger` Arc-handle pattern: cloned into `HandlerContext`
/// before dispatch, populated by the handler during policy evaluation, and
/// read from the outer handle after dispatch returns.
///
/// The `None` default is load-bearing:
/// - `None` after `dispatch_result.is_err()` → legitimate skip (handler failed
///   before reaching policy evaluation)
/// - `None` after `dispatch_result.is_ok()` → internal invariant breach
///   (handler completed without setting its authorized region — fail closed)
#[derive(Clone, Debug, Default)]
pub struct AuthorizedRegionSlot(Arc<Mutex<Option<AuthorizedEffectRegion>>>);

impl AuthorizedRegionSlot {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&self, region: AuthorizedEffectRegion) {
        let mut guard = self.lock_recover();
        *guard = Some(region);
    }

    pub fn get(&self) -> Option<AuthorizedEffectRegion> {
        let guard = self.lock_recover();
        guard.clone()
    }

    fn lock_recover(&self) -> MutexGuard<'_, Option<AuthorizedEffectRegion>> {
        match self.0.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                tracing::warn!(
                    "authorized region slot mutex poisoned by a panicking handler; recovering guard"
                );
                poisoned.into_inner()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_effect(kind: ImpactKind, source: EffectSource) -> ObservedEffect {
        ObservedEffect {
            kind,
            scope: ObservedScope::Path(PathBuf::from("/tmp/test.txt")),
            required_capability: RequiredCapability::ReadPath,
            source,
        }
    }

    #[test]
    fn fresh_ledger_has_empty_snapshot() {
        let ledger = EffectLedger::new();
        assert!(ledger.snapshot().is_empty());
    }

    #[test]
    fn record_then_snapshot_returns_effect() {
        let ledger = EffectLedger::new();
        let effect = sample_effect(
            ImpactKind::WorkspaceObservation,
            EffectSource::HandlerBackend,
        );
        ledger.record(effect.clone());
        let snapshot = ledger.snapshot();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0], effect);
    }

    #[test]
    fn shared_arc_record_via_clone_a_read_via_clone_b() {
        let ledger_a = EffectLedger::new();
        let ledger_b = ledger_a.clone();

        let effect = sample_effect(ImpactKind::WorkspaceMutation, EffectSource::HandlerBackend);
        ledger_a.record(effect.clone());

        // Read via clone B — should see the effect recorded through clone A
        let snapshot = ledger_b.snapshot();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0], effect);
    }

    #[test]
    fn effect_source_has_all_four_variants() {
        // Verify all source variants exist and are distinct.
        let sources = [
            EffectSource::HandlerBackend,
            EffectSource::EffectProbe,
            EffectSource::GuardProbe,
            EffectSource::Verify,
        ];
        for (i, a) in sources.iter().enumerate() {
            for (j, b) in sources.iter().enumerate() {
                if i == j {
                    assert_eq!(a, b);
                } else {
                    assert_ne!(a, b);
                }
            }
        }
    }

    #[test]
    fn observed_scope_variants() {
        let path = ObservedScope::Path(PathBuf::from("/foo"));
        let root = ObservedScope::Root("project".to_string());
        let host = ObservedScope::Host("example.com".to_string());
        let session = ObservedScope::SessionState;

        // All variants are distinct
        assert_ne!(path, root);
        assert_ne!(path, host);
        assert_ne!(path, session);
        assert_ne!(root, host);
        assert_ne!(root, session);
        assert_ne!(host, session);
    }

    #[test]
    fn multiple_effects_accumulate_in_order() {
        let ledger = EffectLedger::new();
        let e1 = sample_effect(
            ImpactKind::WorkspaceObservation,
            EffectSource::HandlerBackend,
        );
        let e2 = sample_effect(ImpactKind::WorkspaceMutation, EffectSource::HandlerBackend);
        let e3 = sample_effect(ImpactKind::CommandExecution, EffectSource::Verify);

        ledger.record(e1.clone());
        ledger.record(e2.clone());
        ledger.record(e3.clone());

        let snapshot = ledger.snapshot();
        assert_eq!(snapshot, vec![e1, e2, e3]);
    }

    #[test]
    fn mutex_poison_recovery() {
        let ledger = EffectLedger::new();

        // Poison the mutex by panicking while holding the lock
        let ledger_clone = ledger.clone();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = ledger_clone.0.lock().unwrap();
            panic!("intentional panic to poison mutex");
        }));

        // The mutex is now poisoned — record and snapshot should still work
        // thanks to lock_recover()
        let effect = sample_effect(
            ImpactKind::WorkspaceObservation,
            EffectSource::HandlerBackend,
        );
        ledger.record(effect.clone());
        let snapshot = ledger.snapshot();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0], effect);
    }
}

// ── AuthorizedRegionSlot tests ──────────────────────────────────────────

#[cfg(test)]
mod region_slot_tests {
    use super::*;
    use crate::effect::region::AuthorizedEffectRegion;

    #[test]
    fn fresh_slot_returns_none() {
        let slot = AuthorizedRegionSlot::new();
        assert!(slot.get().is_none());
    }

    #[test]
    fn set_then_get_returns_region() {
        let slot = AuthorizedRegionSlot::new();
        let region = AuthorizedEffectRegion::empty();
        slot.set(region.clone());
        let retrieved = slot.get();
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap(), region);
    }

    #[test]
    fn shared_arc_set_via_clone_a_read_via_clone_b() {
        let slot_a = AuthorizedRegionSlot::new();
        let slot_b = slot_a.clone();

        let region = AuthorizedEffectRegion::empty();
        slot_a.set(region.clone());

        // Read via clone B — should see the region set through clone A
        let retrieved = slot_b.get();
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap(), region);
    }

    #[test]
    fn mutex_poison_recovery_on_slot() {
        let slot = AuthorizedRegionSlot::new();

        // Poison the mutex
        let slot_clone = slot.clone();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = slot_clone.0.lock().unwrap();
            panic!("intentional panic to poison slot mutex");
        }));

        // set and get should still work via lock_recover
        let region = AuthorizedEffectRegion::empty();
        slot.set(region.clone());
        let retrieved = slot.get();
        assert!(retrieved.is_some());
    }
}

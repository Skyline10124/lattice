//! Reusable effect-region contract test harness (EFF-04, D-23).
//!
//! Provides [`effect_contract_harness`], a generic async function that dispatches
//! a CEP method through the [`MethodRegistry`] and captures the resulting
//! [`ObservedEffect`] ledger entries for assertion.
//!
//! The sync→async bridge uses a module-level [`OnceLock<Runtime>`] so that
//! `proptest!` bodies (which are synchronous) can drive async dispatch via
//! `rt().block_on(async { … })`. Do NOT combine with `#[tokio::test]` — the
//! two macros conflict during expansion.
//!
//! This module is `pub` so Phase 27 DOG-03 can reuse the harness scaffolding
//! without duplication (D-25).

use std::sync::{Arc, OnceLock};

use lancet_protocol::envelope::BaseExecutionEnvelope;
use lancet_protocol::policy::{PolicyHook, SourcedCapability};
use lancet_protocol::{CatalogMethod, CepError};
use serde::Serialize;
use serde_json::Value;
use tokio::runtime::Runtime;
use tokio::sync::RwLock;

use crate::audit::CepAuditContext;
use crate::call_durable_metadata::CallDurableMetadata;
use crate::effect::ledger::{AuthorizedRegionSlot, EffectLedger, EffectSource, ObservedEffect};
use crate::effect::region::AuthorizedEffectRegion;
use crate::envelope_store::ActiveEnvelopeStore;
use crate::policy::default::{DefaultPolicyHook, PolicyConfig};
use crate::policy::network::NetworkPolicyConfig;
use crate::policy::{Phase21PolicyConfig, Phase21TurnContext};
use crate::registry::{Cancellation, EventSink, HandlerContext, MethodRegistry};

/// Module-level Tokio runtime for bridging sync `proptest!` bodies to async dispatch.
///
/// Built once on first use via [`OnceLock`]. Safe to call from plain `#[test]`
/// functions (no outer runtime context). Do NOT call from within `#[tokio::test]`
/// or another `block_on` — that panics on reentrancy.
pub fn rt() -> &'static Runtime {
    static RT: OnceLock<Runtime> = OnceLock::new();
    RT.get_or_init(|| Runtime::new().expect("tokio runtime"))
}

/// Result of a single harness dispatch — the dispatch outcome plus the observed
/// effects captured in the ledger.
#[derive(Debug)]
pub struct HarnessOutcome {
    /// The raw dispatch result (Ok with JSON value, or Err with CepError).
    pub dispatch_result: Result<Value, CepError>,
    /// All observed effects recorded in the ledger during dispatch.
    pub observed_effects: Vec<ObservedEffect>,
}

/// Dispatch a catalog method through the registry and capture the effect ledger.
///
/// This is the core reusable harness for EFF-04 property-based testing (D-23).
/// It is generic over `M: CatalogMethod` — the type parameter drives which
/// `M::Params` type the caller must supply.
///
/// # What it does
///
/// 1. Clones the ledger Arc handle from `ctx` before dispatch (the ctx is moved
///    into `registry.dispatch`).
/// 2. Sets the `authority` on `ctx.authorized_region` so the effect checker
///    (Plan 06) can compare observed ⊆ authorized.
/// 3. Serializes `params` to `serde_json::Value` and dispatches by name
///    (`M::NAME`) — NOT `dispatch::<M>`, which does not exist on the registry.
/// 4. Reads the ledger snapshot after dispatch returns.
///
/// # Assertion helpers
///
/// Use [`assert_positive`] and [`assert_negative_no_backend_effects`] on the returned
/// [`HarnessOutcome`] to check the two main property classes (D-24a, D-24b).
pub async fn effect_contract_harness<M>(
    registry: &MethodRegistry,
    params: M::Params,
    ctx: HandlerContext,
    authority: AuthorizedEffectRegion,
) -> HarnessOutcome
where
    M: CatalogMethod,
    M::Params: Serialize,
{
    // Clone the ledger Arc handle before ctx is moved into dispatch.
    let ledger = ctx.effect_ledger.clone();

    // Set the authorized region so the effect checker can compare.
    ctx.authorized_region.set(authority);

    // Serialize params to JSON Value for by-name dispatch.
    let params_value =
        serde_json::to_value(&params).expect("effect_contract_harness: params serialization");

    // Dispatch by name (NOT dispatch::<M> — the registry uses by-name dispatch).
    let dispatch_result = registry.dispatch(M::NAME, params_value, ctx).await;

    // Read the ledger snapshot after dispatch.
    let observed_effects = ledger.snapshot();

    HarnessOutcome {
        dispatch_result,
        observed_effects,
    }
}

/// Positive assertion (D-24a): every `HandlerBackend` effect in the ledger
/// must fall within the authorized region.
///
/// Call this after dispatching with legal params + matching authority.
/// Probes (`EffectProbe`, `GuardProbe`) and `Verify` effects are excluded
/// per D-11 source filtering.
///
/// # Panics
///
/// Panics if any charged (HandlerBackend) effect is not contained in `authority`.
pub fn assert_positive(outcome: &HarnessOutcome, authority: &AuthorizedEffectRegion) {
    for effect in &outcome.observed_effects {
        if matches!(effect.source, EffectSource::HandlerBackend) {
            assert!(
                authority.contains(effect),
                "positive contract violated: {:?}@{:?} not in authorized region",
                effect.kind,
                effect.scope
            );
        }
    }
}

/// Negative assertion (D-24b): on a preflight-denied (Err) dispatch result,
/// no `HandlerBackend` effects must have leaked into the ledger.
///
/// For post-backend overreach (dispatch returned Ok but the effect checker
/// would produce an EffectViolation), use [`assert_effect_violation`] instead.
///
/// # Panics
///
/// Panics if dispatch returned Err but HandlerBackend effects exist in the ledger.
pub fn assert_negative_no_backend_effects(outcome: &HarnessOutcome) {
    if outcome.dispatch_result.is_err() {
        let backend_effects: Vec<&ObservedEffect> = outcome
            .observed_effects
            .iter()
            .filter(|e| matches!(e.source, EffectSource::HandlerBackend))
            .collect();
        assert!(
            backend_effects.is_empty(),
            "preflight-rejected call left {} backend effect(s): {:?}",
            backend_effects.len(),
            backend_effects
        );
    }
}

/// Post-backend overreach assertion: the dispatch result must be an
/// `EffectViolation` (-32050).
///
/// Use this for the D-17 command.run post-backend overreach case where the
/// handler completed successfully but the effect checker caught the overreach.
///
/// # Panics
///
/// Panics if the dispatch result is not `Err(EffectViolation)`.
pub fn assert_effect_violation(outcome: &HarnessOutcome) {
    match &outcome.dispatch_result {
        Err(err) => {
            assert_eq!(
                err.code,
                lancet_protocol::CepErrorCode::EffectViolation,
                "expected EffectViolation, got {:?}: {}",
                err.code,
                err.message
            );
        }
        Ok(_) => {
            panic!("expected EffectViolation but dispatch succeeded");
        }
    }
}

/// Build a [`HandlerContext`] for integration tests with proper policy config,
/// capabilities, and all internal fields initialized.
///
/// Returns the context and the event receiver (for draining notifications).
/// Build a test context, returning `(ctx, event_rx, canonical_root)`.
///
/// The canonical root is the `fs::canonicalize`d form of `root` (matches what
/// `defaults_for_project_root` stores in `Phase21PolicyConfig::project_root`).
/// Callers should use this canonical root when building `AuthorizedEffectRegion`
/// paths so that region containment checks match the paths handlers record.
pub fn build_test_context(
    root: &str,
    caps: Vec<SourcedCapability>,
    network: NetworkPolicyConfig,
) -> (
    HandlerContext,
    tokio::sync::mpsc::Receiver<lancet_protocol::ToolEvent>,
    String,
) {
    let (event_tx, event_rx) = tokio::sync::mpsc::channel(64);
    let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);

    let root_path = std::path::Path::new(root);
    let policy_cfg = PolicyConfig::defaults_for_project_root(root_path.to_path_buf());
    let canonical_root = policy_cfg.project_root.to_string_lossy().to_string();
    let default_hook = Arc::new(DefaultPolicyHook::new(policy_cfg));
    let mut phase21_policy = Phase21PolicyConfig::defaults_for_project_root(root_path);
    phase21_policy.network = network;
    let phase21 = Arc::new(Phase21TurnContext::from_policy(phase21_policy));

    let mut base =
        BaseExecutionEnvelope::for_workspace_root(&canonical_root, "effect-contract-test");
    base.inner.capabilities = caps.iter().map(|sc| sc.capability.clone()).collect();

    let ctx = HandlerContext {
        call_id: 1,
        events: EventSink::new(event_tx, 1),
        cancellation: Cancellation::new(cancel_rx),
        base_envelope: base,
        phase21,
        active_envelope: Arc::new(ActiveEnvelopeStore::new()),
        turn_id: "test-turn".into(),
        durable_metadata: CallDurableMetadata::default(),
        effect_ledger: EffectLedger::new(),
        authorized_region: AuthorizedRegionSlot::new(),
        policy_hook: default_hook.clone() as Arc<dyn PolicyHook>,
        default_policy_hook: Some(default_hook),
        audit_context: Arc::new(RwLock::new(CepAuditContext {
            turn_id: "test-turn".into(),
            session_id: "test-session".into(),
        })),
        invariant_index: Arc::new(crate::invariants::InvariantCardIndex::new(vec![])),
        cep_mode: lancet_protocol::envelope::CepMode::Off,
    };
    (ctx, event_rx, canonical_root)
}

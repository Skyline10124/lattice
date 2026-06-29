// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Reusable effect-region contract test harness.
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
//! This module is crate-private test support so effect-contract tests can reuse
//! registry and ledger scaffolding without exposing it as public API.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};

use lancet_catalog::CatalogMethod;
use lancet_protocol::CepError;
use lancet_protocol::policy::PolicyHook;
use serde::Serialize;
use serde_json::Value;
use tokio::runtime::Runtime;
use tokio::sync::RwLock;

use crate::audit::CepAuditContext;
use crate::execution_backend::{
    DirectoryEntry, ExecutionBackend, ExecutionBackendError, ExecutionRouting, FindRequest,
    GrepMatch, GrepRequest, PathFacts, PathIntent, UpperContext, WorkspacePath,
};
use crate::server::call_durable_metadata::CallDurableMetadata;
use crate::server::registry::{Cancellation, EventSink, HandlerContext, MethodRegistry};
use lancet_authority::effect::ledger::{
    AuthorizedRegionSlot, EffectLedger, EffectSource, ObservedEffect,
};
use lancet_authority::effect::region::AuthorizedEffectRegion;
use lancet_authority::policy::default::{DefaultPolicyHook, PolicyConfig};
use lancet_authority::policy::network::NetworkPolicyConfig;
use lancet_authority::policy::{CepPolicyConfig, CepTurnContext};

/// Minimal host-filesystem backend for effect contract tests.
struct SupportFsBackend {
    root: std::path::PathBuf,
    pre_exec_snapshot: Arc<std::sync::Mutex<Option<std::collections::HashSet<String>>>>,
}

impl SupportFsBackend {
    fn new(root: impl Into<std::path::PathBuf>) -> Self {
        let raw = root.into();
        Self {
            root: std::fs::canonicalize(&raw).unwrap_or(raw),
            pre_exec_snapshot: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    fn snapshot_files(root: &std::path::Path) -> std::collections::HashSet<String> {
        let mut files = std::collections::HashSet::new();
        if let Ok(entries) = std::fs::read_dir(root) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    if let Ok(rel) = path.strip_prefix(root) {
                        files.insert(rel.to_string_lossy().to_string());
                    }
                } else if path.is_dir()
                    && let Ok(rel) = path.strip_prefix(root)
                {
                    let prefix = rel.to_string_lossy().to_string();
                    if let Ok(sub) = std::fs::read_dir(&path) {
                        for se in sub.flatten() {
                            let sp = se.path();
                            if sp.is_file() {
                                let srel = sp.strip_prefix(root).unwrap_or(&sp);
                                files.insert(srel.to_string_lossy().to_string());
                            }
                        }
                    }
                    let _ = prefix;
                }
            }
        }
        files
    }
}

impl ExecutionBackend for SupportFsBackend {
    fn backend_name(&self) -> &str {
        "support-fs"
    }

    fn resolve_path(
        &self,
        path: &WorkspacePath,
        _intent: PathIntent,
    ) -> Pin<Box<dyn Future<Output = Result<PathFacts, ExecutionBackendError>> + Send + '_>>
    {
        let full = if path.is_project_root() {
            self.root.clone()
        } else {
            self.root.join(path.as_str())
        };
        let path_str = path.as_str().to_string();
        let exists = full.exists();
        let kind = if full.is_dir() {
            crate::execution_backend::PathKind::Directory
        } else {
            crate::execution_backend::PathKind::File
        };
        let canonical_under_workspace = if full.is_symlink() {
            full.canonicalize()
                .map(|c| c.starts_with(&self.root))
                .unwrap_or(false)
        } else {
            true
        };
        Box::pin(async move {
            Ok(PathFacts {
                normalized_path: path_str,
                exists,
                kind: Some(kind),
                parent_exists: true,
                parent_kind: Some(crate::execution_backend::PathKind::Directory),
                byte_size: None,
                canonical_under_workspace,
            })
        })
    }

    fn read_file(
        &self,
        path: &WorkspacePath,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, ExecutionBackendError>> + Send + '_>>
    {
        let full = if path.is_project_root() {
            self.root.clone()
        } else {
            self.root.join(path.as_str())
        };
        Box::pin(async move {
            std::fs::read(&full).map_err(|e| ExecutionBackendError::Unavailable {
                message: e.to_string(),
            })
        })
    }

    fn write_file(
        &self,
        path: &WorkspacePath,
        content: &[u8],
    ) -> Pin<Box<dyn Future<Output = Result<(), ExecutionBackendError>> + Send + '_>>
    {
        let full = if path.is_project_root() {
            self.root.clone()
        } else {
            self.root.join(path.as_str())
        };
        let content = content.to_vec();
        Box::pin(async move {
            std::fs::write(&full, &content).map_err(|e| ExecutionBackendError::Unavailable {
                message: e.to_string(),
            })
        })
    }

    fn remove_file(
        &self,
        path: &WorkspacePath,
    ) -> Pin<Box<dyn Future<Output = Result<(), ExecutionBackendError>> + Send + '_>>
    {
        let full = if path.is_project_root() {
            self.root.clone()
        } else {
            self.root.join(path.as_str())
        };
        Box::pin(async move {
            std::fs::remove_file(&full).map_err(|e| ExecutionBackendError::Unavailable {
                message: e.to_string(),
            })
        })
    }

    fn list_dir(
        &self,
        _path: &WorkspacePath,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<DirectoryEntry>, ExecutionBackendError>> + Send + '_>>
    {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn grep(
        &self,
        _request: &GrepRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<GrepMatch>, ExecutionBackendError>> + Send + '_>>
    {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn find_files(
        &self,
        _request: &FindRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<WorkspacePath>, ExecutionBackendError>> + Send + '_>>
    {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn changed_paths(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<WorkspacePath>, ExecutionBackendError>> + Send + '_>>
    {
        let pre = self.pre_exec_snapshot.lock().unwrap().take();
        let post = Self::snapshot_files(&self.root);
        Box::pin(async move {
            let changed: Vec<WorkspacePath> = match pre {
                Some(pre_set) => {
                    post.iter()
                        .filter(|p| !pre_set.contains(*p))
                        .filter_map(|p| WorkspacePath::parse(p).ok())
                        .collect()
                }
                None => Vec::new(),
            };
            Ok(changed)
        })
    }
}

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
/// This is the core reusable harness for property-based effect checks. It is
/// generic over `M: CatalogMethod`; the type parameter drives which `M::Params`
/// type the caller must supply.
///
/// # What it does
///
/// 1. Clones the ledger Arc handle from `ctx` before dispatch (the ctx is moved
///    into `registry.dispatch`).
/// 2. Sets the `authority` on `ctx.authorized_region` so the effect checker can
///    compare observed effects against the authorized region.
/// 3. Serializes `params` to `serde_json::Value` and dispatches by name
///    (`M::NAME`) — NOT `dispatch::<M>`, which does not exist on the registry.
/// 4. Reads the ledger snapshot after dispatch returns.
///
/// # Assertion helpers
///
/// Use [`assert_positive`] and [`assert_negative_no_backend_effects`] on the returned
/// [`HarnessOutcome`] to check the two main property classes.
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

/// Positive assertion: every `HandlerBackend` effect in the ledger must fall
/// within the authorized region.
///
/// Call this after dispatching with legal params + matching authority.
/// Probes (`EffectProbe`, `GuardProbe`) and `Verify` effects are excluded by
/// source filtering.
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

/// Negative assertion: on a preflight-denied (Err) dispatch result,
/// no `HandlerBackend` effects must have leaked into the ledger.
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

/// Build a [`HandlerContext`] for integration tests with proper policy config,
/// capabilities, and all internal fields initialized.
///
/// Returns the context and the event receiver (for draining notifications).
/// Build a test context, returning `(ctx, event_rx, canonical_root)`.
///
/// The canonical root is the `fs::canonicalize`d form of `root` (matches what
/// `defaults_for_project_root` stores in `CepPolicyConfig::project_root`).
/// Callers should use this canonical root when building `AuthorizedEffectRegion`
/// paths so that region containment checks match the paths handlers record.
pub fn build_test_context(
    root: &str,
    _caps: Vec<()>,
    network: NetworkPolicyConfig,
) -> (
    HandlerContext,
    tokio::sync::mpsc::Receiver<lancet_protocol::ToolEvent>,
    String,
) {
    let (event_tx, event_rx) = tokio::sync::mpsc::channel(64);
    let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);

    let root_path = std::path::Path::new(root);
    let mut policy_cfg = PolicyConfig::defaults_for_project_root(root_path.to_path_buf());
    // Blacklist semantics: empty deny list → all workspace paths allowed
    policy_cfg.workspace.denied_write_patterns = Vec::new();
    let canonical_root = policy_cfg.project_root.to_string_lossy().to_string();
    let default_hook = Arc::new(DefaultPolicyHook::new(policy_cfg));
    let mut cep_policy = CepPolicyConfig::defaults_for_project_root(root_path);
    cep_policy.network = network;
    let cep_context = Arc::new(CepTurnContext::from_policy(cep_policy));

    let ctx = HandlerContext {
        call_id: 1,
        events: EventSink::new(event_tx, 1),
        cancellation: Cancellation::new(cancel_rx),
        cep_context,
        turn_id: "test-turn".into(),
        durable_metadata: CallDurableMetadata::default(),
        effect_ledger: EffectLedger::new(),
        authorized_region: AuthorizedRegionSlot::new(),
        policy_hook: default_hook.clone() as Arc<dyn PolicyHook>,
        default_policy_hook: Some(default_hook),
        audit_context: Arc::new(RwLock::new(CepAuditContext {
            turn_id: "test-turn".into(),
            session_id: "test-session".into(),
            ..Default::default()
        })),
        audit: Arc::new(crate::audit::CepAuditStore::in_memory().expect("audit store")),
        cep_mode: lancet_protocol::policy::CepMode::Off,

        execution: ExecutionRouting {
            backend: Arc::new(SupportFsBackend::new(root)),
            upper: UpperContext {
                execution_backend: "support-fs".into(),
                upper_workspace_id: "test-upper".into(),
            },
        },
        routing_invocation: crate::execution_backend::RoutingInvocationState::default(),
    };
    (ctx, event_rx, canonical_root)
}

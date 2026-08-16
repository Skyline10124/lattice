// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Host-side Policy-Visible Workspace observation for the `workspace-observe`
//! Tool Host imports (GitHub #30).
//!
//! This module is the whole filesystem authority behind `observe.workspace`.
//! The Component that exports the method owns typed mode dispatch and result
//! shaping and nothing else: it holds no path, no preopened directory, no file
//! descriptor, and no handle to a workspace object. Every fact it can return
//! came out of one of the four functions here, which is what makes "the guest
//! cannot widen its own visibility" a structural property rather than a promise.
//!
//! # What the host decides, per operation
//!
//! Each of `read` / `list` / `grep` / `find` runs the same fixed sequence, and
//! the sequence is the reason all four modes cannot disagree:
//!
//! 1. **Path parse** — [`WorkspacePath::parse`] rejects `..`, absolute paths,
//!    and NUL bytes lexically, before any syscall.
//! 2. **Cell authorization** — the operation's host-bound Cell and its concrete
//!    path target go through [`authorize_operation`], the same seam
//!    `host-control`'s `emit-event` uses.
//! 3. **Safe resolution** — [`LocalWorkspace::resolve_facts_blocking`] walks the
//!    path one segment at a time with `openat`/`AT_SYMLINK_NOFOLLOW`, so a
//!    symlink pointing outside the root fails as `RootEscape` instead of being
//!    followed. System Path Exclusion is applied inside that walk.
//! 4. **Guard L1** — `validate_path_facts` validates the *observed* facts (not
//!    the requested string) against the empty effect region.
//! 5. **Visibility** — one [`VisibilityEngine`], built from the turn's
//!    `ReadVisibilityPolicy`, decides the root and every entry/match/path the
//!    walk produces. This is the single decision the issue's fourth criterion
//!    asks for: the same engine instance, the same policy, and the same
//!    [`HiddenReason`] vocabulary for all four modes.
//! 6. **Bounds** — the per-operation ceilings in [`ObservationPolicyConfig`] clamp the
//!    caller's requested limit, and the turn-scoped observation budget is
//!    charged with the bytes actually returned.
//! 7. **Evidence** — the operation's Cell, the concrete resolved path, the
//!    truncation flag, and the policy decision are recorded through the call's
//!    [`CellEvidenceLedger`]; a refusal records a `DeniedOperation` and the host
//!    facility is never reached.
//!
//! # Hidden paths never leak
//!
//! A hidden path is reported to the guest exactly as a missing one
//! ([`ObservationError::NotFound`]), and the *reason* goes only to evidence.
//! Directory entries, grep matches, and find results are filtered by the same
//! engine before they are handed over, so no mode can surface a name the read
//! path would have refused. The guest never learns why a path is absent, so it
//! cannot use the error shape to probe the boundary.
//!
//! # Truncation is host-computed
//!
//! Every bounded result's truncation state is computed from the real limit
//! comparison and recorded in evidence. For `read`, the host also renders and
//! charges the final content, including any truncation marker; for `list` /
//! `grep` / `find`, the guest receives and echoes the host-computed flag. In
//! either case, the recorded [`crate::evidence::CellEvidenceLedger`] entry is
//! the audit trail's answer, not the guest's document.
//!
//! # Synchronous by construction
//!
//! The Tool Host linker is non-async (`build_linker` in [`crate::runtime`] uses
//! the sync bindgen and the guest is invoked with async support disabled), so
//! these functions are synchronous and call the Local Workspace's blocking
//! primitives directly. Those primitives are the *same* resolver the async
//! native wrappers drive through `spawn_blocking`; there is no second
//! implementation of path safety.

use std::sync::Arc;

use lancet_authority::effect::region::AuthorizedEffectRegion;
use lancet_authority::guard::validate_path_facts;
use lancet_authority::{
    HiddenReason, ObservationPolicyConfig, ReadVisibility, ReadVisibilityPolicy,
};
use lancet_protocol::cell::{Cell, ChannelKind, CtxOp, ExtImpact};
use lancet_protocol::error::ValidationErrorData;
use lancet_protocol::evidence::ObservedTarget;
use lancet_protocol::{CepError, CepErrorCode, CepErrorData, Retriability};

use crate::admission::{AdmittedTool, authorize_operation, resolve_scope};
use crate::bindings;
use crate::evidence::CellEvidenceLedger;
use crate::runtime::StoreState;
use crate::workspace::{
    BoundedFileRead, DirectoryEntry, FindRequest, GrepRequest, LocalWorkspace, LocalWorkspaceError,
    PathIntent, PathKind, VisibilityEngine, VisibilityEvidence, WorkspacePath, content_hash,
    join_rel,
};

/// The turn-scoped observation budget, as an opaque charge sink.
///
/// The tracker itself lives on `CepTurnContext` in `lancet-authority` and is
/// shared by every call in the Turn, so the host takes a closure rather than
/// owning a counter: a per-call copy would silently reset the per-turn ceiling.
/// A refusal carries the real numbers so the error can name them.
pub type BudgetCharge = Arc<dyn Fn(usize) -> Result<(), BudgetRefusal> + Send + Sync>;

/// A turn budget refusal, carrying the numbers the caller's error reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BudgetRefusal {
    /// Stable name of the exhausted limit, e.g.
    /// `observation.max_turn_observation_bytes`.
    pub limit: &'static str,
    pub requested: usize,
    pub remaining: usize,
}

/// What went wrong in one observation operation.
///
/// Deliberately coarse on the agent-facing side and precise on the evidence
/// side: [`Self::NotFound`] covers a genuinely missing path *and* a path hidden
/// by policy, because distinguishing them for the guest is exactly how a
/// hidden-path probe would work. Why a path was hidden reaches evidence through
/// [`ObservationError::audit_label`], never through the guest-visible text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ObservationError {
    /// The path does not exist, is the wrong kind, or is not visible under
    /// policy. One shape for all three, by design.
    NotFound {
        /// The path as the caller wrote it — echoed so the guest can say which
        /// request failed. Never a resolved host path.
        requested: String,
        /// Present when policy hid the path. Evidence-only.
        hidden: Option<HiddenReason>,
    },
    /// The path was rejected before any syscall (`..`, absolute, NUL), or its
    /// resolution escaped the root.
    ///
    /// `reason` is the structured cause; the agent-facing sentence is derived
    /// from it in [`ObservationError::guest_message`], because those sentences are
    /// a product contract the model has been reading since the native handler.
    PathRejected {
        requested: String,
        reason: PathRejection,
    },
    /// A per-operation ceiling would be exceeded by this single request.
    LimitExceeded {
        limit: &'static str,
        requested: usize,
        allowed: usize,
    },
    /// The Turn's observation budget is exhausted.
    BudgetExhausted(BudgetRefusal),
    /// The operation's Cell was not admitted, or its target was refused.
    Denied { requested: String, reason: String },
    /// The caller's own input was malformed (an invalid regex, say).
    InvalidRequest { field: &'static str, reason: String },
    /// The filesystem failed in a way none of the above describes.
    Io { requested: String, reason: String },
}

impl ObservationError {
    /// The policy reason a path was hidden, for evidence only.
    pub fn hidden_reason(&self) -> Option<HiddenReason> {
        match self {
            Self::NotFound { hidden, .. } => *hidden,
            _ => None,
        }
    }

    /// The stable audit label for this refusal.
    ///
    /// These are the strings the native path already wrote into its rejection
    /// records, so operator queries keep working across the migration: hidden
    /// paths keep [`HiddenReason::as_audit_label`], containment failures keep
    /// `path_guard`.
    pub fn audit_label(&self) -> &'static str {
        match self {
            Self::NotFound {
                hidden: Some(reason),
                ..
            } => reason.as_audit_label(),
            Self::NotFound { hidden: None, .. } => "not_found",
            Self::PathRejected { .. } => "path_guard",
            Self::LimitExceeded { .. } => "observation_limit",
            Self::BudgetExhausted(_) => "observation_budget",
            Self::Denied { .. } => "cell_denied",
            Self::InvalidRequest { .. } => "invalid_request",
            Self::Io { .. } => "io_error",
        }
    }

    /// Guest-facing message. Carries the *requested* path only — never a
    /// resolved absolute path, and never the reason a path was hidden.
    pub fn guest_message(&self) -> String {
        match self {
            Self::NotFound { requested, .. } => format!("not_found: '{requested}'"),
            Self::PathRejected { requested, reason } => match reason {
                PathRejection::ParentTraversal => {
                    format!("path contains '..' traversal: '{requested}'")
                }
                PathRejection::NotRelative => format!("path must be relative: '{requested}'"),
                PathRejection::OutsideRoot => {
                    format!("path escapes the project root: '{requested}'")
                }
                PathRejection::Malformed => format!("path is not usable: '{requested}'"),
                PathRejection::GuardDenied => format!("path denied by guard: '{requested}'"),
            },
            Self::LimitExceeded {
                limit,
                requested,
                allowed,
            } => format!("limit {limit} exceeded: requested {requested}, allowed {allowed}"),
            Self::BudgetExhausted(refusal) => format!(
                "observation budget exhausted: {} (requested {}, remaining {})",
                refusal.limit, refusal.requested, refusal.remaining
            ),
            Self::Denied { requested, reason } => {
                format!("operation denied for '{requested}': {reason}")
            }
            Self::InvalidRequest { field, reason } => format!("invalid {field}: {reason}"),
            Self::Io { requested, reason } => format!("io error for '{requested}': {reason}"),
        }
    }
}

/// Why a path was refused before it could be observed.
///
/// Structured rather than a free string so the agent-facing sentence is generated
/// in one place and the audit label cannot drift from it. Each variant maps to a
/// sentence the model already knows from the native handler.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathRejection {
    /// The path text contains a `..` component.
    ParentTraversal,
    /// The path is absolute, or otherwise not project-relative.
    NotRelative,
    /// Resolution left the project root — typically through a symlink.
    OutsideRoot,
    /// Empty, NUL-bearing, non-UTF-8, or the wrong kind for the intent.
    Malformed,
    /// Guard L1 refused the observed path facts.
    GuardDenied,
}

/// One `read` result: the uniquely rendered visible window plus its hash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadOutcome {
    /// Final LLM-visible content. Rendering is host-owned because this exact
    /// byte length is charged to the Turn budget and persisted in evidence.
    pub content: String,
    /// Canonical `blake3:<64 hex>` of the bytes read. Host-computed, because
    /// this is the value `mutate.edit` later verifies — a guest must not be able
    /// to invent it.
    pub content_hash: String,
}

/// One `list` result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListOutcome {
    /// Visible entries only, sorted by name. Hidden entries are absent, not
    /// marked.
    pub entries: Vec<DirectoryEntry>,
    /// True when the host dropped entries to satisfy the effective limit.
    pub truncated: bool,
}

/// One grep match, as the host observed it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MatchLine {
    /// Project-relative path. Always inside the root and always visible.
    pub path: String,
    pub line: u64,
    pub text: String,
}

/// One `grep` result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrepOutcome {
    pub matches: Vec<MatchLine>,
    pub truncated: bool,
}

/// One `find` result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FindOutcome {
    pub paths: Vec<String>,
    pub truncated: bool,
}

/// Call-scoped host authority for workspace observation.
///
/// Holds the concrete workspace, the resolved visibility policy, the
/// per-operation limits, the turn budget sink, and the workspace scope name —
/// everything the guest is not allowed to hold. Constructed by the transport
/// once per server run and shared across the Turn's calls; the per-call facts
/// (admission, evidence, turn id) arrive separately as
/// [`ObservationCallContext`].
pub struct WorkspaceObservationHost {
    workspace: Arc<LocalWorkspace>,
    visibility_policy: ReadVisibilityPolicy,
    observation: ObservationPolicyConfig,
    charge: BudgetCharge,
    /// The descriptor scope the observation cell roots in. Host-resolved from
    /// the admitted Tool's descriptor; a guest never names a scope.
    scope_name: String,
    /// The first refusal this call produced, as a typed [`CepError`].
    ///
    /// A guest can only return a string, so without this the runtime would wrap
    /// the host's decision into a generic `ExecutionFailed` prefixed with
    /// `tool <name> guest error:` — losing the error *code* that distinguishes a
    /// path-containment refusal from a crash, and putting the host's sentence
    /// inside the guest's frame. The runtime reads this instead
    /// (`runtime::host_refusal`).
    ///
    /// First, not last: a guest that swallows a refusal and calls again must not
    /// be able to overwrite the reason its call was refused.
    refusal: std::sync::Mutex<Option<CepError>>,
}

impl WorkspaceObservationHost {
    /// Assemble the host authority.
    ///
    /// `scope_name` must be the workspace-channel scope the calling Tool
    /// declared. The caller resolves it from the descriptor, so an undeclared
    /// scope fails closed inside `resolve_scope` (trust `ex`) rather than here.
    pub fn new(
        workspace: Arc<LocalWorkspace>,
        visibility_policy: ReadVisibilityPolicy,
        observation: ObservationPolicyConfig,
        charge: BudgetCharge,
        scope_name: impl Into<String>,
    ) -> Self {
        Self {
            workspace,
            visibility_policy,
            observation,
            charge,
            scope_name: scope_name.into(),
            refusal: std::sync::Mutex::new(None),
        }
    }

    /// Take the host's structured refusal for this call, if any.
    ///
    /// Consuming: the runtime asks once, when the guest reported a logical error.
    pub fn take_refusal(&self) -> Option<CepError> {
        self.refusal.lock().unwrap().take()
    }

    /// Remember the first refusal, so the runtime can return it verbatim.
    fn remember_refusal(&self, err: &ObservationError) {
        let mut slot = self.refusal.lock().unwrap();
        if slot.is_none() {
            *slot = Some(observation_cep_error(err));
        }
    }

    /// The observation Cell: `alloc:read:<workspace scope>`.
    ///
    /// Fixed by the host, not by the guest. `ExtImpact::Read` is what makes an
    /// observation distinguishable from a mutation in the effect ledger, and
    /// `CtxOp::Alloc` is what makes the returned context measurable.
    fn cell(&self) -> Cell {
        Cell {
            ctx_op: CtxOp::Alloc,
            ext_impact: ExtImpact::Read,
            scope: self.scope_name.clone(),
        }
    }

    /// The host-resolved scope identity for evidence.
    fn scope(&self, ctx: &ObservationCallContext<'_>) -> lancet_protocol::evidence::ObservedScope {
        resolve_scope(
            &ctx.admitted.descriptor,
            &self.scope_name,
            ChannelKind::Workspace,
        )
    }

    /// A fresh visibility engine for one operation.
    ///
    /// Built per operation from current Local state rather than cached, so a
    /// `.gitignore` or git-tracking change made during the Turn takes effect on
    /// the next call. That freshness is what makes the "external changes are
    /// visible to later calls" criterion hold for the visibility decision, not
    /// only for file content.
    fn visibility(&self) -> VisibilityEngine {
        VisibilityEngine::load(self.workspace.root(), self.visibility_policy.clone())
    }

    /// Authorize one operation against the admitted capability set.
    ///
    /// The same seam `host-control`'s `emit-event` goes through, with a real
    /// path target: the Cell must be covered by an admitted capability and the
    /// target must pass host-owned validation. A denial records a
    /// `DeniedOperation` and the caller never reaches the filesystem.
    fn authorize(
        &self,
        ctx: &ObservationCallContext<'_>,
        resolved: &str,
        requested: &str,
    ) -> Result<(), ObservationError> {
        let cell = self.cell();
        let target = ObservedTarget::Path {
            path: resolved.to_string(),
        };
        if let Err(denial) = authorize_operation(ctx.admitted, &cell, &target, ctx.turn_id) {
            let err = ObservationError::Denied {
                requested: requested.to_string(),
                reason: denial.to_string(),
            };
            ctx.evidence
                .record_denied(cell, self.scope(ctx), target, denial.to_string());
            self.remember_refusal(&err);
            return Err(err);
        }
        Ok(())
    }

    /// Record a refusal that happened instead of an effect.
    ///
    /// Every refusal reaches evidence, including the ones whose guest-facing
    /// shape is indistinguishable from "missing": the audit trail is where the
    /// policy decision stays legible.
    fn record_refusal(
        &self,
        ctx: &ObservationCallContext<'_>,
        resolved: &str,
        err: &ObservationError,
    ) {
        self.remember_refusal(err);
        ctx.evidence.record_denied(
            self.cell(),
            self.scope(ctx),
            ObservedTarget::Path {
                path: resolved.to_string(),
            },
            err.audit_label().to_string(),
        );
    }

    /// Record one observation effect against a concrete path.
    fn record_effect(
        &self,
        ctx: &ObservationCallContext<'_>,
        resolved: &str,
        bytes_returned: usize,
        truncated: bool,
        visibility: VisibilityEvidence,
    ) {
        ctx.evidence.record_observation_effect(
            self.cell(),
            self.scope(ctx),
            ObservedTarget::Path {
                path: resolved.to_string(),
            },
            bytes_returned as u64,
            truncated,
            visibility.into_protocol(),
        );
    }

    /// Charge the Turn budget with the bytes this operation returns, recording
    /// the refusal if it is exhausted.
    fn charge(
        &self,
        ctx: &ObservationCallContext<'_>,
        resolved: &str,
        bytes: usize,
    ) -> Result<(), ObservationError> {
        match (self.charge)(bytes) {
            Ok(()) => Ok(()),
            Err(refusal) => {
                let err = ObservationError::BudgetExhausted(refusal);
                self.record_refusal(ctx, resolved, &err);
                Err(err)
            }
        }
    }

    /// Parse, authorize, resolve, Guard-L1-validate and kind-check one path.
    ///
    /// The single funnel all four modes enter through. Returning a
    /// [`WorkspacePath`] is what stops any caller from reaching the filesystem
    /// with a string that skipped a step.
    /// Take the per-project `WorkspaceOperationLock` shared for one
    /// observation (ADR 0005 §5).
    ///
    /// Observation never mutates, so a shared hold is enough: it excludes a
    /// concurrent mutation or Rewind while several observations proceed
    /// together. The guard lives for the calling observation only, and no
    /// repository write lock is taken under it.
    fn lock_observation(
        &self,
        ctx: &ObservationCallContext<'_>,
        requested: &str,
    ) -> Result<Option<lattice_store::FlockGuard>, ObservationError> {
        self.workspace
            .lock_operation(
                lattice_store::LockMode::Shared,
                lattice_store::Blocking::Block,
            )
            .map_err(|err| {
                let e = ObservationError::Io {
                    requested: requested.to_string(),
                    reason: err.to_string(),
                };
                self.record_refusal(ctx, requested, &e);
                e
            })
    }

    fn open(
        &self,
        ctx: &ObservationCallContext<'_>,
        requested: &str,
        intent: PathIntent,
        expect: PathKind,
    ) -> Result<WorkspacePath, ObservationError> {
        let ws_path = match WorkspacePath::parse(requested) {
            Ok(path) => path,
            Err(err) => {
                // Nothing resolved, so the evidence target is the requested
                // text: it is the only concrete thing the host observed.
                let e = ObservationError::PathRejected {
                    requested: requested.to_string(),
                    reason: classify_path_error(&err),
                };
                self.record_refusal(ctx, requested, &e);
                return Err(e);
            }
        };
        if expect == PathKind::File
            && let Err(err) = ws_path.require_file_path()
        {
            let e = ObservationError::PathRejected {
                requested: requested.to_string(),
                reason: classify_path_error(&err),
            };
            self.record_refusal(ctx, ws_path.as_str(), &e);
            return Err(e);
        }

        self.authorize(ctx, ws_path.as_str(), requested)?;

        let facts = match self.workspace.resolve_facts_blocking(&ws_path, intent) {
            Ok(facts) => facts,
            Err(err) => {
                let e = observation_io_error(requested, err);
                self.record_refusal(ctx, ws_path.as_str(), &e);
                return Err(e);
            }
        };
        if let Err(err) = validate_path_facts(
            ws_path.as_str(),
            &facts.to_guard(),
            &AuthorizedEffectRegion::empty(),
        ) {
            // Guard L1 refused the *observed* facts. Its message describes host
            // structure, so the agent-facing sentence is derived from the
            // classification instead; the message reaches evidence, not the guest.
            let _ = &err;
            let e = ObservationError::PathRejected {
                requested: requested.to_string(),
                reason: PathRejection::GuardDenied,
            };
            self.record_refusal(ctx, ws_path.as_str(), &e);
            return Err(e);
        }
        if !facts.exists || facts.kind != Some(expect) {
            let e = ObservationError::NotFound {
                requested: requested.to_string(),
                hidden: None,
            };
            self.record_refusal(ctx, ws_path.as_str(), &e);
            return Err(e);
        }
        Ok(ws_path)
    }

    /// Read one file's visible content.
    pub fn read(
        &self,
        ctx: &ObservationCallContext<'_>,
        requested: &str,
        offset: Option<u64>,
        limit: Option<u64>,
        line_numbers: Option<bool>,
    ) -> Result<ReadOutcome, ObservationError> {
        let _operation_guard = self.lock_observation(ctx, requested)?;
        let ws_path = self.open(ctx, requested, PathIntent::Read, PathKind::File)?;

        // The read mode's visibility decision: a hidden file is refused as
        // missing and the reason goes only to evidence.
        let mut visibility = self.visibility();
        if let ReadVisibility::Hidden(reason) = visibility.decide(ws_path.as_str(), false) {
            let e = ObservationError::NotFound {
                requested: requested.to_string(),
                hidden: Some(reason),
            };
            self.record_refusal(ctx, ws_path.as_str(), &e);
            return Err(e);
        }
        let visibility_evidence = visibility.evidence();

        let bytes = match self
            .workspace
            .read_file_bounded_blocking(&ws_path, self.observation.max_single_read_bytes)
        {
            Ok(BoundedFileRead::Complete(bytes)) => bytes,
            Ok(BoundedFileRead::LimitExceeded { observed, allowed }) => {
                let e = ObservationError::LimitExceeded {
                    limit: "observation.max_single_read_bytes",
                    requested: observed,
                    allowed,
                };
                self.record_refusal(ctx, ws_path.as_str(), &e);
                return Err(e);
            }
            Err(err) => {
                let e = observation_io_error(requested, err);
                self.record_refusal(ctx, ws_path.as_str(), &e);
                return Err(e);
            }
        };
        let content = match String::from_utf8(bytes) {
            Ok(content) => content,
            Err(err) => {
                let e = ObservationError::Io {
                    requested: requested.to_string(),
                    reason: err.to_string(),
                };
                self.record_refusal(ctx, ws_path.as_str(), &e);
                return Err(e);
            }
        };

        let (rendered, truncated) = render_file_slice(
            &content,
            offset.unwrap_or(1).max(1),
            limit.unwrap_or(2000).max(1),
            line_numbers.unwrap_or(true),
        );
        self.charge(ctx, ws_path.as_str(), rendered.len())?;
        self.record_effect(
            ctx,
            ws_path.as_str(),
            rendered.len(),
            truncated,
            visibility_evidence,
        );

        Ok(ReadOutcome {
            content_hash: content_hash(content.as_bytes()),
            content: rendered,
        })
    }

    /// List one directory's visible entries.
    pub fn list(
        &self,
        ctx: &ObservationCallContext<'_>,
        requested: &str,
        limit: Option<u64>,
    ) -> Result<ListOutcome, ObservationError> {
        let requested = if requested.is_empty() { "." } else { requested };
        let _operation_guard = self.lock_observation(ctx, requested)?;
        let ws_path = self.open(
            ctx,
            requested,
            PathIntent::ListDirectory,
            PathKind::Directory,
        )?;
        let mut visibility = self.enter_root(ctx, &ws_path, requested)?;

        let effective = clamp_limit(limit, self.observation.max_list_entries);
        let raw = match self.workspace.list_dir_blocking(&ws_path) {
            Ok(entries) => entries,
            Err(err) => {
                let e = observation_io_error(requested, err);
                self.record_refusal(ctx, ws_path.as_str(), &e);
                return Err(e);
            }
        };

        // Filtered by the same engine that judged the root, so a hidden child
        // cannot appear in a listing the read mode would have refused.
        let mut entries: Vec<DirectoryEntry> = raw
            .into_iter()
            .filter(|entry| {
                let child = join_rel(ws_path.as_str(), &entry.name);
                visibility.is_visible_entry(&child, entry.kind == PathKind::Directory)
            })
            .collect();
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        let truncated = entries.len() > effective;
        entries.truncate(effective);

        let bytes = entries.iter().map(|e| e.name.len() + 16).sum::<usize>();
        self.charge(ctx, ws_path.as_str(), bytes)?;
        self.record_effect(
            ctx,
            ws_path.as_str(),
            bytes,
            truncated,
            visibility.evidence(),
        );

        Ok(ListOutcome { entries, truncated })
    }

    /// Grep visible files beneath one root.
    pub fn grep(
        &self,
        ctx: &ObservationCallContext<'_>,
        pattern: &str,
        requested: Option<&str>,
        max_results: Option<u64>,
    ) -> Result<GrepOutcome, ObservationError> {
        let requested = default_root(requested);
        // A malformed regex is the caller's error and costs no traversal, so it
        // is refused before the path is even resolved.
        if let Err(err) = regex::Regex::new(pattern) {
            let err = ObservationError::InvalidRequest {
                field: "pattern",
                reason: err.to_string(),
            };
            self.record_refusal(ctx, requested, &err);
            return Err(err);
        }
        let _operation_guard = self.lock_observation(ctx, requested)?;
        let ws_path = self.open(
            ctx,
            requested,
            PathIntent::ListDirectory,
            PathKind::Directory,
        )?;
        let visibility = self.enter_root(ctx, &ws_path, requested)?;

        let effective = clamp_limit(max_results, self.observation.max_grep_matches);
        let request = GrepRequest {
            pattern: pattern.to_string(),
            path: ws_path.clone(),
            glob: None,
        };
        // The walk carries the engine, so hidden files produce no match and are
        // not counted toward truncation.
        let probe_limit = effective.saturating_add(1);
        let raw = match self
            .workspace
            .grep_blocking(&request, probe_limit, visibility)
        {
            Ok((matches, evidence)) => (matches, evidence),
            Err(err) => {
                let e = observation_io_error(requested, err);
                self.record_refusal(ctx, ws_path.as_str(), &e);
                return Err(e);
            }
        };

        let (raw, visibility_evidence) = raw;
        let truncated = raw.len() > effective;
        let matches: Vec<MatchLine> = raw
            .into_iter()
            .take(effective)
            .map(|m| MatchLine {
                path: m.path.as_str().to_string(),
                line: m.line_number,
                text: m.line,
            })
            .collect();

        let bytes = matches
            .iter()
            .map(|m| m.path.len() + m.text.len() + 16)
            .sum::<usize>();
        self.charge(ctx, ws_path.as_str(), bytes)?;
        self.record_effect(ctx, ws_path.as_str(), bytes, truncated, visibility_evidence);

        Ok(GrepOutcome { matches, truncated })
    }

    /// Find visible paths matching a glob beneath one root.
    pub fn find(
        &self,
        ctx: &ObservationCallContext<'_>,
        pattern: &str,
        requested: Option<&str>,
        limit: Option<u64>,
    ) -> Result<FindOutcome, ObservationError> {
        let requested = default_root(requested);
        let _operation_guard = self.lock_observation(ctx, requested)?;
        let ws_path = self.open(
            ctx,
            requested,
            PathIntent::ListDirectory,
            PathKind::Directory,
        )?;
        let visibility = self.enter_root(ctx, &ws_path, requested)?;

        let effective = clamp_limit(limit, self.observation.max_find_paths);
        let request = FindRequest {
            pattern: pattern.to_string(),
            path: ws_path.clone(),
        };
        let probe_limit = effective.saturating_add(1);
        let raw = match self
            .workspace
            .find_files_blocking(&request, probe_limit, visibility)
        {
            Ok((paths, evidence)) => (paths, evidence),
            Err(err) => {
                let e = observation_io_error(requested, err);
                self.record_refusal(ctx, ws_path.as_str(), &e);
                return Err(e);
            }
        };

        let (raw, visibility_evidence) = raw;
        let truncated = raw.len() > effective;
        let paths: Vec<String> = raw
            .into_iter()
            .take(effective)
            .map(|p| p.as_str().to_string())
            .collect();

        let bytes = paths.iter().map(|p| p.len()).sum::<usize>();
        self.charge(ctx, ws_path.as_str(), bytes)?;
        self.record_effect(ctx, ws_path.as_str(), bytes, truncated, visibility_evidence);

        Ok(FindOutcome { paths, truncated })
    }

    /// Assess a traversal root and return the engine that decided it.
    ///
    /// Shared by `list`/`grep`/`find` so a hidden root is refused identically in
    /// all three, and so the *same engine instance* that judged the root goes on
    /// to judge its entries. Handing the engine back is what prevents a second,
    /// differently-configured decision from forming inside one operation.
    fn enter_root(
        &self,
        ctx: &ObservationCallContext<'_>,
        ws_path: &WorkspacePath,
        requested: &str,
    ) -> Result<VisibilityEngine, ObservationError> {
        let mut visibility = self.visibility();
        let assessment = visibility.assess(ws_path.as_str(), true);
        if let Some(reason) = assessment.hidden_reason()
            && !assessment.should_descend()
        {
            let e = ObservationError::NotFound {
                requested: requested.to_string(),
                hidden: Some(reason),
            };
            self.record_refusal(ctx, ws_path.as_str(), &e);
            return Err(e);
        }
        Ok(visibility)
    }
}

/// The call-scoped facts an observation operation authorizes against.
///
/// Borrowed rather than owned so the host authority can be shared across the
/// Turn while admission and evidence stay per call.
pub struct ObservationCallContext<'a> {
    pub admitted: &'a AdmittedTool,
    pub evidence: &'a CellEvidenceLedger,
    pub turn_id: &'a str,
}

/// `.` is the workspace root, and an empty string means the same thing.
fn default_root(requested: Option<&str>) -> &str {
    match requested {
        None | Some("") => ".",
        Some(path) => path,
    }
}

/// Render the caller's requested line window after the host has applied the
/// full-file allocation ceiling. This is the unique renderer because its exact
/// output is what the Turn observation budget authorizes.
fn render_file_slice(content: &str, offset: u64, limit: u64, line_numbers: bool) -> (String, bool) {
    let total = content.lines().count();
    let start = usize::try_from(offset.saturating_sub(1)).unwrap_or(usize::MAX);
    if start >= total {
        return (
            format!("[No content: requested offset {offset} but file has {total} lines]"),
            false,
        );
    }

    let take = usize::try_from(limit).unwrap_or(usize::MAX);
    let mut rendered = Vec::new();
    for (index, line) in content.lines().enumerate().skip(start).take(take) {
        if line_numbers {
            rendered.push(format!("{:>6}\t{}", index + 1, line));
        } else {
            rendered.push(line.to_string());
        }
    }
    let returned = rendered.len();
    let truncated = start.saturating_add(returned) < total;
    if truncated {
        rendered.push(format!(
            "[TRUNCATED: showing lines {}-{} of {}; call read_file with offset {} to continue]",
            offset,
            start + returned,
            total,
            start + returned + 1
        ));
    }
    (rendered.join("\n"), truncated)
}

/// Clamp a caller's requested limit into the policy ceiling.
///
/// `None` means "the default", which is the ceiling itself. `Some(0)` is not a
/// way to disable bounding, so it clamps up to 1.
fn clamp_limit(requested: Option<u64>, ceiling: usize) -> usize {
    match requested {
        None => ceiling,
        Some(value) => usize::try_from(value).unwrap_or(ceiling).clamp(1, ceiling),
    }
}

/// Map a Local Workspace failure onto the guest-facing shape.
///
/// Every containment failure collapses into `PathRejected` with a *fixed* reason
/// string, and every absence into `NotFound`. The workspace's own error text is
/// deliberately discarded for these cases, and the match is exhaustive rather
/// than ending in a catch-all so a new variant cannot join the leaking path by
/// default: `SymlinkRejected` carries the offending component as an **absolute
/// host path**, and `Path`/`RootEscape` describe host structure. Forwarding any
/// of them would hand the guest the project's real location on disk — the thing
/// the visibility rules exist to keep out of its reach — through an error message
/// instead of through a read.
///
/// The `Io` arm forwards `std::io::Error` text only, which describes the
/// operation ("is a directory", "permission denied") and not the layout.
fn observation_io_error(requested: &str, err: LocalWorkspaceError) -> ObservationError {
    match err {
        LocalWorkspaceError::NotFound { .. } => ObservationError::NotFound {
            requested: requested.to_string(),
            hidden: None,
        },
        LocalWorkspaceError::RootEscape | LocalWorkspaceError::SymlinkRejected { .. } => {
            ObservationError::PathRejected {
                requested: requested.to_string(),
                reason: PathRejection::OutsideRoot,
            }
        }
        LocalWorkspaceError::Path(err) => ObservationError::PathRejected {
            requested: requested.to_string(),
            reason: classify_path_error(&err),
        },
        // Not reachable from an observation, which never creates anything; mapped
        // rather than caught so the match stays exhaustive.
        LocalWorkspaceError::TargetExists { .. } => ObservationError::PathRejected {
            requested: requested.to_string(),
            reason: PathRejection::Malformed,
        },
        LocalWorkspaceError::Io(io) => ObservationError::Io {
            requested: requested.to_string(),
            reason: io.to_string(),
        },
    }
}

// ---------------------------------------------------------------------------
// The `workspace-observe` Tool Host imports
// ---------------------------------------------------------------------------

/// Implement the `workspace-observe` interface for `StoreState`.
///
/// Each function does the same three things: assemble the call-scoped context,
/// delegate to the authority above, and translate the outcome into the WIT
/// result. No decision is made here — this is the boundary, not the policy.
///
/// **Fail-closed when unauthorized.** A call whose context carries no
/// [`WorkspaceObservationHost`] refuses every operation with `denied`. That is
/// the state a Tool reaches by linking the interface without the transport
/// granting workspace authority, and refusing is the only safe reading: there is
/// no default workspace, no default policy, and no ambient root to fall back to.
impl bindings::lattice::tool_host::workspace_observe::Host for StoreState {
    fn read_file(
        &mut self,
        path: String,
        offset: Option<u64>,
        limit: Option<u64>,
        line_numbers: Option<bool>,
    ) -> Result<bindings::lattice::tool_host::workspace_observe::ReadResult, WitObserveError> {
        let (host, ctx) = self.observation()?;
        host.read(&ctx, &path, offset, limit, line_numbers)
            .map(
                |outcome| bindings::lattice::tool_host::workspace_observe::ReadResult {
                    content: outcome.content,
                    content_hash: outcome.content_hash,
                },
            )
            .map_err(wit_error)
    }

    fn list_dir(
        &mut self,
        path: String,
        limit: Option<u64>,
    ) -> Result<bindings::lattice::tool_host::workspace_observe::ListResult, WitObserveError> {
        let (host, ctx) = self.observation()?;
        host.list(&ctx, &path, limit)
            .map(
                |outcome| bindings::lattice::tool_host::workspace_observe::ListResult {
                    entries: outcome
                        .entries
                        .into_iter()
                        .map(
                            |entry| bindings::lattice::tool_host::workspace_observe::DirEntry {
                                name: entry.name,
                                kind: wit_entry_kind(entry.kind),
                            },
                        )
                        .collect(),
                    truncated: outcome.truncated,
                },
            )
            .map_err(wit_error)
    }

    fn grep(
        &mut self,
        pattern: String,
        path: Option<String>,
        max_results: Option<u64>,
    ) -> Result<bindings::lattice::tool_host::workspace_observe::GrepResult, WitObserveError> {
        let (host, ctx) = self.observation()?;
        host.grep(&ctx, &pattern, path.as_deref(), max_results)
            .map(
                |outcome| bindings::lattice::tool_host::workspace_observe::GrepResult {
                    matches: outcome
                        .matches
                        .into_iter()
                        .map(
                            |m| bindings::lattice::tool_host::workspace_observe::MatchLine {
                                path: m.path,
                                line: m.line,
                                text: m.text,
                            },
                        )
                        .collect(),
                    truncated: outcome.truncated,
                },
            )
            .map_err(wit_error)
    }

    fn find_files(
        &mut self,
        pattern: String,
        path: Option<String>,
        limit: Option<u64>,
    ) -> Result<bindings::lattice::tool_host::workspace_observe::FindResult, WitObserveError> {
        let (host, ctx) = self.observation()?;
        host.find(&ctx, &pattern, path.as_deref(), limit)
            .map(
                |outcome| bindings::lattice::tool_host::workspace_observe::FindResult {
                    paths: outcome.paths,
                    truncated: outcome.truncated,
                },
            )
            .map_err(wit_error)
    }
}

/// The generated WIT error type for the observation interface.
type WitObserveError = bindings::lattice::tool_host::workspace_observe::ObserveError;

impl StoreState {
    /// The call's workspace authority and context, or a `denied` refusal.
    ///
    /// Borrowing the authority out of the store is what lets one shared
    /// per-turn host serve per-call admission and evidence: the `Arc` is cloned,
    /// the call facts are borrowed.
    fn observation(
        &self,
    ) -> Result<(Arc<WorkspaceObservationHost>, ObservationCallContext<'_>), WitObserveError> {
        let host = self.call.workspace.as_ref().cloned().ok_or_else(|| {
            WitObserveError::Denied(String::from(
                "this call has no workspace observation authority",
            ))
        })?;
        Ok((
            host,
            ObservationCallContext {
                admitted: &self.call.admitted,
                evidence: &self.call.evidence,
                turn_id: &self.call.turn_id,
            },
        ))
    }
}

/// Map a host refusal onto the WIT variant, carrying only the guest-facing
/// message.
///
/// The variant is coarse where the host's decision is coarse: a hidden path and a
/// missing one both arrive as `not-found`, because the guest must not be able to
/// tell them apart. The policy reason is already in evidence by the time this
/// runs.
fn wit_error(err: ObservationError) -> WitObserveError {
    let message = err.guest_message();
    match err {
        ObservationError::NotFound { .. } => WitObserveError::NotFound(message),
        ObservationError::PathRejected { .. } => WitObserveError::PathRejected(message),
        ObservationError::LimitExceeded { .. } => WitObserveError::LimitExceeded(message),
        ObservationError::BudgetExhausted(_) => WitObserveError::BudgetExhausted(message),
        ObservationError::Denied { .. } => WitObserveError::Denied(message),
        ObservationError::InvalidRequest { .. } => WitObserveError::InvalidRequest(message),
        ObservationError::Io { .. } => WitObserveError::IoError(message),
    }
}

/// Map an observed path kind onto the WIT enum.
///
/// `PathKind` has no `Other`, because the workspace's own `file_type_kind`
/// resolves everything it can name; the WIT enum keeps the variant so a future
/// kind is expressible without a breaking change.
fn wit_entry_kind(kind: PathKind) -> bindings::lattice::tool_host::workspace_observe::EntryKind {
    use bindings::lattice::tool_host::workspace_observe::EntryKind;
    match kind {
        PathKind::File => EntryKind::File,
        PathKind::Directory => EntryKind::Directory,
        PathKind::Symlink => EntryKind::Symlink,
    }
}

/// Classify a workspace path error into the agent-facing rejection reason.
///
/// The mapping is deliberately coarse: the model needs to know whether to fix a
/// `..`, make the path relative, or stop reaching outside the project. The finer
/// internal distinctions (NUL byte, non-UTF-8, retired sandbox alias) all mean
/// "this path is not usable", and spelling them out would describe host internals
/// to a caller that cannot act on them.
fn classify_path_error(err: &crate::workspace::WorkspacePathError) -> PathRejection {
    use crate::workspace::WorkspacePathError as E;
    match err {
        E::ParentTraversal => PathRejection::ParentTraversal,
        E::Absolute | E::SandboxRootAlias => PathRejection::NotRelative,
        E::Empty | E::NulByte | E::InvalidUtf8 | E::ProjectRootNotAllowedForFileIntent => {
            PathRejection::Malformed
        }
    }
}

/// Structured CEP error for an observation refusal.
///
/// The guest receives [`ObservationError::guest_message`]; this is the mapping
/// the host uses when a refusal must leave the Component boundary as a typed CEP
/// error. The `reason` field carries the audit label, not content, because this
/// value can reach a durable audit row.
pub fn observation_cep_error(err: &ObservationError) -> CepError {
    let (code, retry) = match err {
        ObservationError::NotFound { .. } | ObservationError::InvalidRequest { .. } => {
            (CepErrorCode::TypeError, Retriability::Permanent)
        }
        ObservationError::PathRejected { .. } => {
            (CepErrorCode::PathContainment, Retriability::Permanent)
        }
        ObservationError::LimitExceeded { .. } | ObservationError::BudgetExhausted(_) => {
            (CepErrorCode::Reserved, Retriability::RequiresReplan)
        }
        ObservationError::Denied { .. } => (CepErrorCode::EffectViolation, Retriability::Permanent),
        ObservationError::Io { .. } => (CepErrorCode::ExecutionFailed, Retriability::Permanent),
    };
    CepError::new(
        code,
        err.guest_message(),
        CepErrorData::Validation(ValidationErrorData {
            field: "path".into(),
            reason: err.audit_label().into(),
        }),
        retry,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::BTreeSet;
    use std::fs;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use lancet_protocol::cell::{ArtifactIdentity, Capability, ScopeDecl, ToolDescriptor, Trust};
    use lancet_protocol::evidence::CapabilityAdmissionPolicy;

    use crate::admission::admit_tool;
    use crate::history::WorkspaceHistory;

    /// A descriptor declaring one workspace-channel scope and the observation
    /// capability, i.e. what `tools/observe-workspace/manifest.toml` lowers to.
    fn observe_descriptor() -> Arc<ToolDescriptor> {
        Arc::new(ToolDescriptor {
            name: "observe.workspace".into(),
            version: "0.1.0".into(),
            description: "observation fixture".into(),
            params_schema: serde_json::Value::Null,
            result_schema: serde_json::Value::Null,
            scopes: vec![ScopeDecl {
                name: "workspace".into(),
                trust: Trust::In,
                channel: ChannelKind::Workspace,
            }],
            capabilities: vec![Capability {
                name: "workspace.observation".into(),
                cell: Cell {
                    ctx_op: CtxOp::Alloc,
                    ext_impact: ExtImpact::Read,
                    scope: "workspace".into(),
                },
            }],
            artifact: ArtifactIdentity {
                sha256: "0".repeat(64),
                toolchain: "test".into(),
            },
            abi_version: 1,
        })
    }

    fn admitted() -> AdmittedTool {
        admit_tool(
            observe_descriptor(),
            &CapabilityAdmissionPolicy {
                enabled_tools: BTreeSet::from(["observe.workspace".to_string()]),
                denied_capabilities: BTreeSet::new(),
                max_boundary: Trust::In,
            },
        )
        .expect("the observation capability is admitted")
    }

    /// A host over `root` with the given visibility policy and limits, plus an
    /// unbounded budget. Returns the host and a fresh ledger.
    fn host_over(
        root: &Path,
        policy: ReadVisibilityPolicy,
        limits: ObservationPolicyConfig,
    ) -> (WorkspaceObservationHost, CellEvidenceLedger) {
        let workspace = Arc::new(LocalWorkspace::new(root));
        let charge: BudgetCharge = Arc::new(|_bytes| Ok(()));
        (
            WorkspaceObservationHost::new(workspace, policy, limits, charge, "workspace"),
            CellEvidenceLedger::new(),
        )
    }

    fn default_host(root: &Path) -> (WorkspaceObservationHost, CellEvidenceLedger) {
        host_over(
            root,
            ReadVisibilityPolicy::default(),
            ObservationPolicyConfig::defaults(),
        )
    }

    /// Run one operation with a real admitted tool and this ledger.
    fn ctx<'a>(
        tool: &'a AdmittedTool,
        evidence: &'a CellEvidenceLedger,
    ) -> ObservationCallContext<'a> {
        ObservationCallContext {
            admitted: tool,
            evidence,
            turn_id: "turn-1",
        }
    }

    /// A real git project with one visible file, one gitignored file, and a
    /// nested directory.
    ///
    /// Genuinely `git init`-ed, not just a `.git` directory: the visibility
    /// engine reads ignore rules through `gix`, and a fake `.git` would make
    /// every path report `NotIgnored` — which would let an ignore test pass
    /// without any ignore rule ever being consulted.
    fn seeded_project() -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().expect("temp root");
        let root = dir.path();
        gix::init(root).expect("git init");
        fs::write(root.join("visible.rs"), "fn main() {}\nlet x = 1;\n").expect("seed");
        fs::write(root.join(".gitignore"), "secret.txt\n").expect("seed");
        fs::write(root.join("secret.txt"), "hidden content\n").expect("seed");
        fs::create_dir(root.join("src")).expect("seed");
        fs::write(root.join("src/lib.rs"), "pub fn f() {}\n").expect("seed");
        dir
    }

    /// The four modes share one visibility decision, so a path the read mode
    /// refuses cannot appear in a listing, a match, or a find result. This is the
    /// issue's fourth criterion stated as one test: the *same* hidden file is
    /// invisible four ways.
    #[test]
    fn all_four_modes_share_one_visibility_decision() {
        let dir = seeded_project();
        // `.git/config` is under a System Path Exclusion; the exclusions are not
        // profile-configurable, so the default policy is enough to hide it.
        let (host, evidence) = default_host(dir.path());
        let tool = admitted();
        let c = ctx(&tool, &evidence);

        // read: refused as missing, with the policy reason in evidence only.
        let err = host
            .read(&c, ".git/config", None, None, None)
            .expect_err("system path is hidden");
        assert!(matches!(err, ObservationError::NotFound { .. }));
        assert!(
            !err.guest_message().contains("system"),
            "the guest must not learn WHY a path is hidden: {}",
            err.guest_message()
        );

        // list: the excluded directory is absent from the root listing.
        let listed = host.list(&c, ".", None).expect("root lists");
        assert!(
            !listed.entries.iter().any(|e| e.name == ".git"),
            "{:?}",
            listed.entries
        );

        // grep: no match comes out of it. The pattern `core` appears in the
        // excluded `.git/config`, so a match would be a real leak.
        let matched = host.grep(&c, "core", Some("."), None).expect("grep runs");
        assert!(
            !matched
                .matches
                .iter()
                .any(|m| m.path.split('/').any(|segment| segment == ".git")),
            "{:?}",
            matched.matches
        );

        // find: no path comes out of it. Matched per segment, not by substring:
        // `.gitignore` is a legitimately visible file whose *name* starts with
        // `.git`, and a substring test would call it a leak.
        let found = host.find(&c, "*", Some("."), None).expect("find runs");
        assert!(
            !found
                .paths
                .iter()
                .any(|p| p.split('/').any(|segment| segment == ".git")),
            "{:?}",
            found.paths
        );
        // The excluded subtree really was there to be found, so the absence above
        // is a decision rather than an empty walk.
        assert!(
            dir.path().join(".git/config").is_file(),
            "the fixture must contain a system-excluded file"
        );
        let (_, effects, _) = evidence.snapshot();
        assert_eq!(effects.effects.len(), 3);
        assert!(
            effects
                .effects
                .iter()
                .all(|effect| effect.visibility.is_some()),
            "every successful workspace observation persists its visibility decision"
        );
    }

    /// A gitignored file is hidden by default and included when policy says so —
    /// and the inclusion applies to every mode, not just the one that configured
    /// it.
    #[test]
    fn ignored_paths_are_hidden_by_default_and_included_on_request() {
        let dir = seeded_project();
        let tool = admitted();

        let (default, ev) = default_host(dir.path());
        let c = ctx(&tool, &ev);
        assert!(matches!(
            default.read(&c, "secret.txt", None, None, None),
            Err(ObservationError::NotFound { .. })
        ));

        let including = ReadVisibilityPolicy::from_patterns(&["secret.txt".to_string()], &[])
            .expect("valid pattern");
        let (host, ev2) = host_over(dir.path(), including, ObservationPolicyConfig::defaults());
        let c2 = ctx(&tool, &ev2);
        let read = host
            .read(&c2, "secret.txt", None, None, None)
            .expect("explicitly included");
        assert!(read.content.contains("hidden content"));
        // And the same decision surfaces it to find, not only to read.
        let found = host.find(&c2, "secret.txt", Some("."), None).expect("find");
        assert_eq!(found.paths, vec!["secret.txt".to_string()]);
        let (_, effects, _) = ev2.snapshot();
        assert!(
            effects.effects.iter().all(|effect| {
                effect
                    .visibility
                    .is_some_and(|visibility| visibility.included_ignored)
            }),
            "explicit ignored-path inclusion is durable effect evidence"
        );
    }

    /// A denied-read path is refused even when it is tracked and otherwise
    /// perfectly visible, and its absence is indistinguishable from a missing
    /// file.
    #[test]
    fn denied_reads_are_refused_and_recorded_with_their_reason() {
        let dir = seeded_project();
        let policy = ReadVisibilityPolicy::from_patterns(&[], &["visible.rs".to_string()])
            .expect("valid pattern");
        let (host, evidence) = host_over(dir.path(), policy, ObservationPolicyConfig::defaults());
        let tool = admitted();
        let c = ctx(&tool, &evidence);

        let err = host
            .read(&c, "visible.rs", None, None, None)
            .expect_err("denied read");
        assert_eq!(err.hidden_reason(), Some(HiddenReason::ReadDenied));
        assert_eq!(err.audit_label(), "read_denied");

        // The refusal is evidence, not silence: the audit trail carries the
        // reason and the concrete path even though the guest was told "missing".
        let (_, effects, denied) = evidence.snapshot();
        assert!(effects.effects.is_empty(), "{:?}", effects.effects);
        assert_eq!(denied.denied.len(), 1);
        assert_eq!(denied.denied[0].reason, "read_denied");
        assert_eq!(
            denied.denied[0].target,
            ObservedTarget::Path {
                path: "visible.rs".into()
            }
        );
    }

    /// A symlink escaping the root fails as a rejected path, not as a followed
    /// link. The resolver refuses per segment, so the escape never happens.
    #[test]
    fn symlink_escaping_the_root_is_refused() {
        let outside = tempfile::TempDir::new().expect("outside root");
        fs::write(outside.path().join("target.txt"), "outside\n").expect("seed");
        let dir = seeded_project();
        std::os::unix::fs::symlink(
            outside.path().join("target.txt"),
            dir.path().join("escape.txt"),
        )
        .expect("symlink");

        let (host, evidence) = default_host(dir.path());
        let tool = admitted();
        let err = host
            .read(&ctx(&tool, &evidence), "escape.txt", None, None, None)
            .expect_err("a symlink out of the root is refused");
        assert!(
            matches!(err, ObservationError::PathRejected { .. }),
            "{err:?}"
        );
        assert_eq!(err.audit_label(), "path_guard");
        // The message describes the boundary and nothing else. The workspace's own
        // `SymlinkRejected` carries the offending component as an ABSOLUTE host
        // path, so forwarding its text would hand the guest the project's real
        // location on disk through an error instead of through a read.
        let message = err.guest_message();
        for leak in [
            outside.path().to_str().expect("utf-8 temp path"),
            dir.path().to_str().expect("utf-8 temp path"),
        ] {
            assert!(
                !message.contains(leak),
                "refusal must not leak the host path {leak}: {message}"
            );
        }
        assert!(!message.contains("/tmp"), "{message}");
    }

    /// A `..` segment is refused before any syscall, so traversal cannot even be
    /// attempted.
    #[test]
    fn parent_traversal_is_refused_lexically() {
        let dir = seeded_project();
        let (host, evidence) = default_host(dir.path());
        let tool = admitted();
        let err = host
            .read(&ctx(&tool, &evidence), "../outside.txt", None, None, None)
            .expect_err("traversal refused");
        assert!(
            matches!(err, ObservationError::PathRejected { .. }),
            "{err:?}"
        );
    }

    /// A single read over the ceiling is refused rather than truncated: the
    /// caller asked for a whole file and cannot be given a silent prefix.
    #[test]
    fn a_read_over_the_single_read_ceiling_is_refused() {
        let dir = seeded_project();
        fs::write(dir.path().join("big.txt"), "x".repeat(4096)).expect("seed");
        let limits = ObservationPolicyConfig {
            max_single_read_bytes: 16,
            ..ObservationPolicyConfig::defaults()
        };
        let (host, evidence) = host_over(dir.path(), ReadVisibilityPolicy::default(), limits);
        let tool = admitted();
        let err = host
            .read(&ctx(&tool, &evidence), "big.txt", None, None, None)
            .expect_err("over the ceiling");
        match err {
            ObservationError::LimitExceeded {
                limit,
                requested,
                allowed,
            } => {
                assert_eq!(limit, "observation.max_single_read_bytes");
                assert_eq!(requested, 4096);
                assert_eq!(allowed, 16);
            }
            other => panic!("expected a limit refusal, got {other:?}"),
        }
    }

    /// The caller's limit is clamped into policy, and the truncation verdict the
    /// host records is the real comparison — not the caller's request.
    #[test]
    fn list_truncation_is_host_computed_and_recorded() {
        let dir = tempfile::TempDir::new().expect("temp root");
        for i in 0..5 {
            fs::write(dir.path().join(format!("f{i}.txt")), "x\n").expect("seed");
        }
        let limits = ObservationPolicyConfig {
            max_list_entries: 2,
            ..ObservationPolicyConfig::defaults()
        };
        let (host, evidence) = host_over(dir.path(), ReadVisibilityPolicy::default(), limits);
        let tool = admitted();

        // The caller asks for 50; policy allows 2.
        let listed = host
            .list(&ctx(&tool, &evidence), ".", Some(50))
            .expect("list runs");
        assert_eq!(listed.entries.len(), 2);
        assert!(listed.truncated);

        let (_, effects, _) = evidence.snapshot();
        assert_eq!(effects.effects.len(), 1);
        assert_eq!(effects.effects[0].truncated, Some(true));
        assert_eq!(effects.effects[0].cell.reference(), "alloc:read:workspace");
        assert_eq!(effects.effects[0].scope.channel, ChannelKind::Workspace);
        assert!(effects.effects[0].context_measure.is_some());
    }

    /// A complete result records `Some(false)`, not absence: "bounded and nothing
    /// cut" is a different claim from "no bound applied".
    #[test]
    fn an_untruncated_result_records_an_explicit_false() {
        let dir = seeded_project();
        let (host, evidence) = default_host(dir.path());
        let tool = admitted();
        let listed = host
            .list(&ctx(&tool, &evidence), "src", None)
            .expect("list runs");
        assert!(!listed.truncated);
        let (_, effects, _) = evidence.snapshot();
        assert_eq!(effects.effects[0].truncated, Some(false));
    }

    /// Read budgeting follows the final rendered window, not the full file the
    /// host had to inspect for its hash and single-read ceiling.
    #[test]
    fn read_window_is_host_bounded_charged_and_recorded() {
        let dir = tempfile::TempDir::new().expect("temp root");
        fs::write(dir.path().join("lines.txt"), "one\ntwo\nthree\n").expect("seed");
        let charged = Arc::new(AtomicUsize::new(0));
        let charged_for_call = Arc::clone(&charged);
        let charge: BudgetCharge = Arc::new(move |bytes| {
            charged_for_call.store(bytes, Ordering::SeqCst);
            Ok(())
        });
        let host = WorkspaceObservationHost::new(
            Arc::new(LocalWorkspace::new(dir.path())),
            ReadVisibilityPolicy::default(),
            ObservationPolicyConfig::defaults(),
            charge,
            "workspace",
        );
        let evidence = CellEvidenceLedger::new();
        let tool = admitted();

        let read = host
            .read(
                &ctx(&tool, &evidence),
                "lines.txt",
                Some(2),
                Some(1),
                Some(false),
            )
            .expect("window reads");
        assert!(read.content.starts_with("two\n[TRUNCATED:"));
        assert_eq!(charged.load(Ordering::SeqCst), read.content.len());
        let (_, effects, _) = evidence.snapshot();
        assert_eq!(
            effects.effects[0].context_measure,
            Some(read.content.len() as u64)
        );
        assert_eq!(effects.effects[0].truncated, Some(true));
    }

    #[test]
    fn empty_and_numbered_read_windows_charge_exact_rendered_bytes() {
        let dir = tempfile::TempDir::new().expect("temp root");
        fs::write(dir.path().join("lines.txt"), "one\ntwo\n").expect("seed");
        let charged = Arc::new(AtomicUsize::new(0));
        let charged_for_call = Arc::clone(&charged);
        let host = WorkspaceObservationHost::new(
            Arc::new(LocalWorkspace::new(dir.path())),
            ReadVisibilityPolicy::default(),
            ObservationPolicyConfig::defaults(),
            Arc::new(move |bytes| {
                charged_for_call.store(bytes, Ordering::SeqCst);
                Ok(())
            }),
            "workspace",
        );
        let evidence = CellEvidenceLedger::new();
        let tool = admitted();

        let numbered = host
            .read(
                &ctx(&tool, &evidence),
                "lines.txt",
                Some(1),
                Some(1),
                Some(true),
            )
            .expect("numbered window");
        assert!(numbered.content.starts_with("     1\tone\n[TRUNCATED:"));
        assert_eq!(charged.load(Ordering::SeqCst), numbered.content.len());

        let empty = host
            .read(
                &ctx(&tool, &evidence),
                "lines.txt",
                Some(99),
                Some(1),
                Some(false),
            )
            .expect("empty window message");
        assert!(empty.content.starts_with("[No content:"));
        assert_eq!(charged.load(Ordering::SeqCst), empty.content.len());
        let (_, effects, _) = evidence.snapshot();
        assert_eq!(
            effects.effects[1].context_measure,
            Some(empty.content.len() as u64)
        );
    }

    /// The Turn budget is charged with what the operation returned, and
    /// exhaustion refuses the operation and records the refusal.
    #[test]
    fn budget_exhaustion_refuses_the_operation_and_is_recorded() {
        let dir = seeded_project();
        let workspace = Arc::new(LocalWorkspace::new(dir.path()));
        let charge: BudgetCharge = Arc::new(|bytes| {
            Err(BudgetRefusal {
                limit: "observation.max_turn_observation_bytes",
                requested: bytes,
                remaining: 0,
            })
        });
        let host = WorkspaceObservationHost::new(
            workspace,
            ReadVisibilityPolicy::default(),
            ObservationPolicyConfig::defaults(),
            charge,
            "workspace",
        );
        let evidence = CellEvidenceLedger::new();
        let tool = admitted();

        let err = host
            .read(&ctx(&tool, &evidence), "visible.rs", None, None, None)
            .expect_err("budget exhausted");
        assert!(
            matches!(err, ObservationError::BudgetExhausted(_)),
            "{err:?}"
        );
        let (_, effects, denied) = evidence.snapshot();
        assert!(
            effects.effects.is_empty(),
            "an operation refused on budget records no effect: {:?}",
            effects.effects
        );
        assert_eq!(denied.denied[0].reason, "observation_budget");
    }

    /// Observation reads current Local state: a file created after the host was
    /// built is visible to the next call, and a deleted one stops being visible.
    /// No snapshot, no cache.
    #[test]
    fn later_calls_see_external_changes_made_during_the_turn() {
        let dir = seeded_project();
        let (host, evidence) = default_host(dir.path());
        let tool = admitted();
        let c = ctx(&tool, &evidence);

        assert!(matches!(
            host.read(&c, "late.txt", None, None, None),
            Err(ObservationError::NotFound { .. })
        ));

        // An external writer (the user, another process) creates the file.
        fs::write(dir.path().join("late.txt"), "created mid-turn\n").expect("external create");
        let read = host
            .read(&c, "late.txt", None, None, None)
            .expect("now visible");
        assert!(read.content.contains("created mid-turn"));

        // An external edit is visible too, with a hash that follows the content.
        let first_hash = read.content_hash.clone();
        fs::write(dir.path().join("late.txt"), "edited mid-turn\n").expect("external edit");
        let again = host
            .read(&c, "late.txt", None, None, None)
            .expect("still visible");
        assert!(again.content.contains("edited mid-turn"));
        assert_ne!(again.content_hash, first_hash);

        // And a deletion is observed as a deletion.
        fs::remove_file(dir.path().join("late.txt")).expect("external delete");
        assert!(matches!(
            host.read(&c, "late.txt", None, None, None),
            Err(ObservationError::NotFound { .. })
        ));
    }

    /// The content hash is the host's, in the canonical form `mutate.edit`
    /// verifies.
    #[test]
    fn read_returns_the_host_computed_content_hash() {
        let dir = seeded_project();
        let (host, evidence) = default_host(dir.path());
        let tool = admitted();
        let read = host
            .read(&ctx(&tool, &evidence), "visible.rs", None, None, None)
            .expect("read runs");
        assert_eq!(
            read.content_hash,
            crate::workspace::content_hash(
                &fs::read(dir.path().join("visible.rs")).expect("fixture bytes")
            )
        );
        assert!(read.content_hash.starts_with("blake3:"));
    }

    /// An operation whose cell is not admitted never reaches the filesystem, and
    /// the refusal is a `DeniedOperation` rather than an effect.
    #[test]
    fn an_unadmitted_cell_is_refused_before_the_filesystem() {
        let dir = seeded_project();
        // Admit the Tool, then deny the one capability it has: the tool is
        // enabled but every operation's cell is uncovered.
        let denied_tool = admit_tool(
            observe_descriptor(),
            &CapabilityAdmissionPolicy {
                enabled_tools: BTreeSet::from(["observe.workspace".to_string()]),
                denied_capabilities: BTreeSet::from(["workspace.observation".to_string()]),
                max_boundary: Trust::In,
            },
        );
        // With its only capability denied there is nothing left to admit, which
        // is itself the fail-closed answer.
        assert!(
            denied_tool.is_err(),
            "a tool whose only capability is denied must not admit"
        );

        // The same refusal one layer in: a tool admitted for a *different* cell
        // cannot use it to observe.
        let session_only = Arc::new(ToolDescriptor {
            scopes: vec![ScopeDecl {
                name: "workspace".into(),
                trust: Trust::In,
                channel: ChannelKind::Workspace,
            }],
            capabilities: vec![Capability {
                name: "workspace.mutation".into(),
                cell: Cell {
                    ctx_op: CtxOp::Mutate,
                    ext_impact: ExtImpact::Write,
                    scope: "workspace".into(),
                },
            }],
            ..(*observe_descriptor()).clone()
        });
        let tool = admit_tool(
            session_only,
            &CapabilityAdmissionPolicy {
                enabled_tools: BTreeSet::from(["observe.workspace".to_string()]),
                denied_capabilities: BTreeSet::new(),
                max_boundary: Trust::In,
            },
        )
        .expect("the mutation capability admits");

        let (host, evidence) = default_host(dir.path());
        let err = host
            .read(&ctx(&tool, &evidence), "visible.rs", None, None, None)
            .expect_err("the observation cell is not admitted");
        assert!(matches!(err, ObservationError::Denied { .. }), "{err:?}");
        let (_, effects, denied) = evidence.snapshot();
        assert!(effects.effects.is_empty());
        assert_eq!(denied.denied.len(), 1);
        assert!(
            denied.denied[0].reason.contains("not covered"),
            "{}",
            denied.denied[0].reason
        );
        let refusal = host
            .take_refusal()
            .expect("an authorization denial remains a structured CEP refusal");
        assert_eq!(refusal.code, CepErrorCode::EffectViolation);
    }

    /// A grep over a subtree records the root it observed, so the effect is
    /// attributable to a concrete path rather than to the call.
    #[test]
    fn grep_records_its_concrete_root_and_finds_visible_matches() {
        let dir = seeded_project();
        let (host, evidence) = default_host(dir.path());
        let tool = admitted();
        let found = host
            .grep(&ctx(&tool, &evidence), "pub fn", Some("src"), None)
            .expect("grep runs");
        assert_eq!(found.matches.len(), 1);
        assert_eq!(found.matches[0].path, "src/lib.rs");

        let (_, effects, _) = evidence.snapshot();
        assert_eq!(
            effects.effects[0].target,
            ObservedTarget::Path { path: "src".into() }
        );
    }

    /// A malformed regex is the caller's error and costs no traversal.
    #[test]
    fn an_invalid_regex_is_refused_as_the_callers_error() {
        let dir = seeded_project();
        let (host, evidence) = default_host(dir.path());
        let tool = admitted();
        let err = host
            .grep(&ctx(&tool, &evidence), "(unclosed", Some("."), None)
            .expect_err("invalid regex");
        match err {
            ObservationError::InvalidRequest { field, .. } => assert_eq!(field, "pattern"),
            other => panic!("expected an invalid-request refusal, got {other:?}"),
        }
        let (_, effects, denied) = evidence.snapshot();
        assert!(effects.effects.is_empty());
        assert_eq!(denied.denied.len(), 1);
        assert_eq!(denied.denied[0].reason, "invalid_request");
        assert_eq!(
            host.take_refusal().expect("structured refusal").code,
            CepErrorCode::TypeError
        );
    }

    /// An exact-size result is complete. The host probes one item beyond the
    /// effective limit, so `truncated` means data was actually dropped rather
    /// than merely that the result filled its capacity.
    #[test]
    fn exact_limits_are_not_reported_as_truncated() {
        let dir = tempfile::TempDir::new().expect("temp root");
        fs::write(dir.path().join("only.txt"), "needle\n").expect("seed");
        let limits = ObservationPolicyConfig {
            max_grep_matches: 1,
            max_find_paths: 1,
            ..ObservationPolicyConfig::defaults()
        };
        let (host, evidence) = host_over(dir.path(), ReadVisibilityPolicy::default(), limits);
        let tool = admitted();
        let c = ctx(&tool, &evidence);

        let grep = host.grep(&c, "needle", Some("."), Some(1)).expect("grep");
        assert_eq!(grep.matches.len(), 1);
        assert!(!grep.truncated);

        let find = host.find(&c, "only.txt", Some("."), Some(1)).expect("find");
        assert_eq!(find.paths, vec!["only.txt"]);
        assert!(!find.truncated);
    }

    /// `None` means "the policy default", and a zero limit is not a way to turn
    /// bounding off.
    #[test]
    fn limit_clamping_never_exceeds_policy_and_never_reaches_zero() {
        assert_eq!(clamp_limit(None, 10), 10);
        assert_eq!(clamp_limit(Some(0), 10), 1);
        assert_eq!(clamp_limit(Some(3), 10), 3);
        assert_eq!(clamp_limit(Some(999), 10), 10);
        assert_eq!(clamp_limit(Some(u64::MAX), 10), 10);
    }

    /// A history-less workspace is a test convenience only; observation never
    /// records mutation history, so this documents that the observation path does
    /// not depend on it.
    #[test]
    fn observation_needs_no_mutation_history() {
        let dir = seeded_project();
        let workspace = Arc::new(LocalWorkspace::with_history(
            dir.path(),
            Arc::new(WorkspaceHistory::disabled()),
        ));
        let charge: BudgetCharge = Arc::new(|_| Ok(()));
        let host = WorkspaceObservationHost::new(
            workspace,
            ReadVisibilityPolicy::default(),
            ObservationPolicyConfig::defaults(),
            charge,
            "workspace",
        );
        let evidence = CellEvidenceLedger::new();
        let tool = admitted();
        assert!(
            host.read(&ctx(&tool, &evidence), "visible.rs", None, None, None,)
                .is_ok()
        );
    }
}

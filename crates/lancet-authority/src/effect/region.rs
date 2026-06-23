// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Effect-region decision layer: EffectiveEffect (per-invocation materialized effect)
//! and AuthorizedEffectRegion (post-policy authorized region).
//!
//! This is the middle and outer layer of the three-layer effect model:
//! - `EffectDecl` (static ceiling, protocol-owned)
//! - `EffectiveEffect` (per-invocation, materialized from EffectDecl + params)
//! - `AuthorizedEffectRegion` (post policy + envelope)
//!
//! The final runtime check is `observed ⊆ authorized_effect_region`, NOT
//! `observed ⊆ static EffectDecl`.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use lancet_protocol::effect_types::{EffectKindSet, ImpactKind};

/// Normalize a path pattern for region containment, aligning with Guard L1 semantics.
///
/// Strips `/**` suffix (Guard L1 `DirectoryGlob`) so that `"src/**"` becomes `"src"`,
/// then resolves relative patterns against `project_root`.
/// This ensures `Path::starts_with` containment checks work correctly.
pub fn normalize_region_path(pattern: &str, project_root: &Path) -> PathBuf {
    let base = pattern.strip_suffix("/**").unwrap_or(pattern);
    if Path::new(base).is_relative() {
        project_root.join(base)
    } else {
        PathBuf::from(base)
    }
}

/// Normalize a WritePath pattern while preserving Guard L1's exact-file vs
/// directory-prefix semantics.
pub fn normalize_write_region_path(pattern: &str, project_root: &Path) -> Option<(PathBuf, bool)> {
    match normalize_write_path_pattern(pattern).ok()? {
        WritePathPattern::ExactFile(path) => Some((project_root.join(path), false)),
        WritePathPattern::DirectoryGlob(path) => Some((project_root.join(path), true)),
    }
}
use lancet_protocol::CepError;

use super::ledger::{ObservedEffect, ObservedScope};
use crate::guard::{WritePathPattern, normalize_write_path_pattern};
use crate::policy::command::EffectiveImpact;

/// Per-invocation materialized effect — the middle layer of the effect model.
///
/// For most methods this is derived from the static `EffectDecl`. For
/// `exec.command_run` it is computed from the existing `EffectiveImpact`
/// classification (single source — no parallel classifier).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectiveEffect {
    /// The set of impact kinds this invocation materializes.
    pub kinds: BTreeSet<ImpactKind>,
}

impl EffectiveEffect {
    /// Materialize an effective effect from a static `EffectKindSet`.
    ///
    /// Used for fixed-effect methods (observe.workspace, mutate.create, etc.)
    /// where the effective kinds are the same as the declared ceiling.
    pub fn from_effect_kind_set(kinds: &EffectKindSet) -> Self {
        Self {
            kinds: BTreeSet::from(*kinds),
        }
    }

    /// Derive exec.command_run's effective effect from the existing `EffectiveImpact`
    /// classification.
    ///
    /// - `WorkspaceMutation` → Ok({CommandExecution, WorkspaceMutation})
    /// - `Diagnostic` → Ok({CommandExecution})
    /// - `HardDenied` → Err(internal error) — HardDenied is rejected pre-execution
    ///   and must never reach this function; if it does, it is an invariant breach
    ///   (review concern 6).
    ///
    /// The declared impact selects which kinds materialize; it never grants authority.
    pub fn from_command_impact(impact: &EffectiveImpact) -> Result<Self, CepError> {
        match impact {
            EffectiveImpact::WorkspaceMutation => {
                let mut kinds = BTreeSet::new();
                kinds.insert(ImpactKind::CommandExecution);
                kinds.insert(ImpactKind::WorkspaceMutation);
                Ok(Self { kinds })
            }
            EffectiveImpact::Diagnostic => {
                let mut kinds = BTreeSet::new();
                kinds.insert(ImpactKind::CommandExecution);
                Ok(Self { kinds })
            }
            EffectiveImpact::HardDenied => {
                // Review concern 6: HardDenied is rejected pre-execution and must never
                // reach effect materialization. If it does, it is an invariant breach,
                // not an absence of effect — return an internal error, never a silent
                // empty set.
                Err(CepError::internal_error(
                    "HardDenied must not reach effect materialization",
                ))
            }
        }
    }
}

/// Post-policy authorized effect region — the outer layer of the effect model.
///
/// Assembled from the matched capability and the policy configuration.
/// The final check is `observed ⊆ authorized_effect_region`.
///
/// An empty region contains nothing — any HandlerBackend effect checked
/// against it produces an EffectViolation (fail-closed, review concern 5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthorizedEffectRegion {
    /// Authorized impact kinds.
    pub kinds: BTreeSet<ImpactKind>,
    /// Authorized read paths (from ReadPath capability pattern).
    pub read_paths: Vec<PathBuf>,
    /// Authorized observation roots (from Inspect policy roots).
    pub observation_roots: Vec<PathBuf>,
    /// Authorized exact write paths (from exact-file WritePath capability patterns).
    pub write_paths: Vec<PathBuf>,
    /// Authorized write path prefixes (from directory-glob WritePath capability patterns).
    pub write_path_prefixes: Vec<PathBuf>,
    /// Authorized external hosts (from FetchUrl capability pattern).
    pub hosts: Vec<String>,
    /// Whether session state mutation is authorized.
    pub session_state_allowed: bool,
    /// Whether command execution is authorized.
    pub command_allowed: bool,
}

impl AuthorizedEffectRegion {
    /// Create an EMPTY authorized region — the fail-closed default.
    ///
    /// An empty region contains nothing. Any HandlerBackend effect checked
    /// against it via `contains()` returns false, so overreach → EffectViolation
    /// (review concern 5 — None matched-capability fails closed).
    pub fn empty() -> Self {
        Self {
            kinds: BTreeSet::new(),
            read_paths: Vec::new(),
            observation_roots: Vec::new(),
            write_paths: Vec::new(),
            write_path_prefixes: Vec::new(),
            hosts: Vec::new(),
            session_state_allowed: false,
            command_allowed: false,
        }
    }

    /// Union another region into this one (kinds ∪ kinds, paths ∪ paths, etc.).
    pub fn union(&mut self, other: &AuthorizedEffectRegion) {
        self.kinds.extend(other.kinds.iter().cloned());
        self.read_paths.extend(other.read_paths.iter().cloned());
        self.observation_roots
            .extend(other.observation_roots.iter().cloned());
        self.write_paths.extend(other.write_paths.iter().cloned());
        self.write_path_prefixes
            .extend(other.write_path_prefixes.iter().cloned());
        self.hosts.extend(other.hosts.iter().cloned());
        self.session_state_allowed |= other.session_state_allowed;
        self.command_allowed |= other.command_allowed;
    }

    /// Check whether an observed effect falls within this authorized region.
    ///
    /// Returns true iff the observed kind is in `kinds` AND the observed
    /// scope falls within the corresponding authorized region.
    /// An empty region contains nothing.
    pub fn contains(&self, observed: &ObservedEffect) -> bool {
        // Kind must be authorized
        if !self.kinds.contains(&observed.kind) {
            return false;
        }

        // Scope must be within the authorized region for this kind
        match (&observed.kind, &observed.scope) {
            (ImpactKind::WorkspaceObservation, ObservedScope::Path(path)) => self
                .read_paths
                .iter()
                .any(|rp| relative_path_within_prefix(path, rp)),
            (ImpactKind::WorkspaceObservation, ObservedScope::Root(root)) => {
                let root_path = Path::new(root);
                self.observation_roots
                    .iter()
                    .any(|r| relative_observation_root_within(root_path, r))
            }
            (ImpactKind::WorkspaceMutation, ObservedScope::Path(path)) => {
                self.write_paths.iter().any(|wp| path == wp)
                    || self
                        .write_path_prefixes
                        .iter()
                        .any(|wp| relative_path_within_prefix(path, wp))
            }
            (ImpactKind::WorkspaceMutation, ObservedScope::Lines { path, .. }) => {
                self.write_paths.iter().any(|wp| path == wp)
                    || self
                        .write_path_prefixes
                        .iter()
                        .any(|wp| relative_path_within_prefix(path, wp))
            }
            (ImpactKind::CommandExecution, ObservedScope::Root(_)) => {
                // Command execution authorized at the project root level
                self.command_allowed
            }
            (ImpactKind::ExternalObservation, ObservedScope::Host(host)) => {
                // Check hosts (FetchUrl capability)
                self.hosts.iter().any(|h| h == host)
            }
            (ImpactKind::SessionStateMutation, ObservedScope::SessionState) => {
                self.session_state_allowed
            }
            (ImpactKind::AuditObservation, ObservedScope::SessionState) => {
                self.session_state_allowed
            }
            // Mismatched kind+scope combinations are not authorized
            _ => false,
        }
    }
}

pub(crate) fn is_relative_workspace_root(path: &Path) -> bool {
    path == Path::new(".")
}

fn relative_path_has_parent_traversal(path: &Path) -> bool {
    path.components().any(|c| matches!(c, Component::ParentDir))
}

pub(crate) fn relative_path_within_prefix(observed: &Path, prefix: &Path) -> bool {
    if is_relative_workspace_root(prefix) {
        return observed.is_relative() && !relative_path_has_parent_traversal(observed);
    }
    if observed.is_relative() && prefix.is_relative() {
        return observed == prefix || observed.starts_with(prefix);
    }
    observed.starts_with(prefix)
}

fn relative_observation_root_within(observed: &Path, authorized_root: &Path) -> bool {
    if observed.is_absolute() && authorized_root.is_absolute() {
        return observed.starts_with(authorized_root);
    }
    if is_relative_workspace_root(authorized_root) {
        return observed.is_relative()
            && !observed.components().any(|c| {
                matches!(
                    c,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            });
    }
    if observed.is_relative() && authorized_root.is_relative() {
        return observed.starts_with(authorized_root);
    }
    false
}

// Cross-type conversion (review concern 10) is in lancet-protocol:
// `impl From<EffectKindSet> for BTreeSet<ImpactKind>`

#[cfg(test)]
mod tests {
    use super::*;
    use lancet_catalog::method::RequiredCapability;
    use std::path::PathBuf;

    // ── EffectiveEffect::from_command_impact tests ─────────────────────

    #[test]
    fn from_command_impact_workspace_mutation_yields_both_kinds() {
        let effect =
            EffectiveEffect::from_command_impact(&EffectiveImpact::WorkspaceMutation).unwrap();
        assert!(effect.kinds.contains(&ImpactKind::CommandExecution));
        assert!(effect.kinds.contains(&ImpactKind::WorkspaceMutation));
        assert_eq!(effect.kinds.len(), 2);
    }

    #[test]
    fn from_command_impact_diagnostic_yields_command_execution_only() {
        // Diagnostic command execution does not require WritePath.
        let effect = EffectiveEffect::from_command_impact(&EffectiveImpact::Diagnostic).unwrap();
        assert!(effect.kinds.contains(&ImpactKind::CommandExecution));
        assert!(!effect.kinds.contains(&ImpactKind::WorkspaceMutation));
        assert_eq!(effect.kinds.len(), 1);
    }

    #[test]
    fn from_command_impact_hard_denied_returns_error() {
        // Review concern 6: HardDenied → Err, never a silent empty set
        let result = EffectiveEffect::from_command_impact(&EffectiveImpact::HardDenied);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.message
                .contains("HardDenied must not reach effect materialization")
        );
    }

    // ── AuthorizedEffectRegion::empty tests ────────────────────────────

    #[test]
    fn empty_region_contains_nothing() {
        let region = AuthorizedEffectRegion::empty();
        assert!(region.kinds.is_empty());

        // Any HandlerBackend effect should NOT be contained
        let effect = ObservedEffect {
            kind: ImpactKind::WorkspaceObservation,
            scope: ObservedScope::Path(PathBuf::from("/project/src/main.rs")),
            required_capability: RequiredCapability::ReadPath,
            source: crate::effect::ledger::EffectSource::HandlerBackend,
        };
        assert!(!region.contains(&effect));
    }

    // ── EffectKindSet → BTreeSet conversion tests ──────────────────────

    #[test]
    fn effect_kind_set_to_btreeset_conversion() {
        let kinds = EffectKindSet::from_static(&[
            ImpactKind::WorkspaceObservation,
            ImpactKind::WorkspaceMutation,
        ]);
        let set: BTreeSet<ImpactKind> = BTreeSet::from(kinds);
        assert_eq!(set.len(), 2);
        assert!(set.contains(&ImpactKind::WorkspaceObservation));
        assert!(set.contains(&ImpactKind::WorkspaceMutation));
    }

    #[test]
    fn effect_kind_set_to_btreeset_empty() {
        let kinds = EffectKindSet::from_static(&[]);
        let set: BTreeSet<ImpactKind> = BTreeSet::from(kinds);
        assert!(set.is_empty());
    }

    #[test]
    fn effect_kind_set_to_btreeset_single() {
        let kinds = EffectKindSet::from_static(&[ImpactKind::CommandExecution]);
        let set: BTreeSet<ImpactKind> = BTreeSet::from(kinds);
        assert_eq!(set.len(), 1);
        assert!(set.contains(&ImpactKind::CommandExecution));
    }

    // ── EffectiveEffect::from_effect_kind_set tests ────────────────────

    #[test]
    fn from_effect_kind_set_materializes_all_kinds() {
        let kinds = EffectKindSet::from_static(&[
            ImpactKind::CommandExecution,
            ImpactKind::WorkspaceMutation,
        ]);
        let effect = EffectiveEffect::from_effect_kind_set(&kinds);
        assert_eq!(effect.kinds.len(), 2);
        assert!(effect.kinds.contains(&ImpactKind::CommandExecution));
        assert!(effect.kinds.contains(&ImpactKind::WorkspaceMutation));
    }

    // ── No parallel EffectiveImpact enum ───────────────────────────────

    #[test]
    fn effective_impact_still_exists_in_policy_command() {
        // Verify we derive from EffectiveImpact, not duplicate it
        let _ = EffectiveImpact::Diagnostic;
        let _ = EffectiveImpact::WorkspaceMutation;
        let _ = EffectiveImpact::HardDenied;
    }
}

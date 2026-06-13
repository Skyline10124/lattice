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
use std::path::{Path, PathBuf};

use lancet_protocol::envelope::{EffectKindSet, ImpactKind};

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
use lancet_protocol::policy::SourcedCapability;

use super::ledger::{ObservedEffect, ObservedScope};
use crate::guard::{WritePathPattern, normalize_write_path_pattern};
use crate::policy::CepPolicyConfig;
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

    /// Build the authorized region from a matched capability and policy config.
    ///
    /// Two-source rule:
    /// - `ReadPath` → read region from the capability's path pattern
    /// - `Inspect` → read region from `policy` observation roots; Inspect has
    ///   no path pattern.
    /// - `WritePath` → write region from the capability's path pattern
    /// - `RunCommand` → command execution authorized
    /// - `FetchUrl` → host authorized
    /// - `MutateSessionState` → session state authorized
    ///
    /// On `None` matched capability: returns `Self::empty()` — fails closed
    /// (review concern 5 — "no authority granted" yields an empty region,
    /// NEVER a skipped check or an over-broad region).
    ///
    /// If a method's matched set ever carries BOTH ReadPath and Inspect (a
    /// hypothetical future dual-capability method), the regions are UNIONED
    /// (capability path pattern ∪ observation policy roots) rather than
    /// picking one — precedence is union.
    pub fn from_matched_capability(
        matched: Option<&SourcedCapability>,
        policy: &CepPolicyConfig,
    ) -> Self {
        let Some(sc) = matched else {
            // None → empty region (fail-closed, review concern 5)
            return Self::empty();
        };

        use lancet_protocol::envelope::Capability;
        match &sc.capability {
            Capability::ReadPath { path_pattern } => {
                let mut kinds = BTreeSet::new();
                kinds.insert(ImpactKind::WorkspaceObservation);
                Self {
                    kinds,
                    read_paths: vec![normalize_region_path(path_pattern, &policy.project_root)],
                    observation_roots: Vec::new(),
                    write_paths: Vec::new(),
                    write_path_prefixes: Vec::new(),
                    hosts: Vec::new(),
                    session_state_allowed: false,
                    command_allowed: false,
                }
            }
            Capability::Inspect => {
                // Inspect has NO path pattern (capability.rs:329).
                // Authorized observation region comes from policy roots
                // Inspect has no path pattern; policy roots supply its region.
                let mut kinds = BTreeSet::new();
                kinds.insert(ImpactKind::WorkspaceObservation);
                Self {
                    kinds,
                    read_paths: Vec::new(),
                    observation_roots: policy.observation.allowed_roots.clone(),
                    write_paths: Vec::new(),
                    write_path_prefixes: Vec::new(),
                    hosts: Vec::new(),
                    session_state_allowed: false,
                    command_allowed: false,
                }
            }
            Capability::WritePath { path_pattern } => {
                let mut kinds = BTreeSet::new();
                kinds.insert(ImpactKind::WorkspaceMutation);
                let (write_paths, write_path_prefixes) =
                    match normalize_write_region_path(path_pattern, &policy.project_root) {
                        Some((path, false)) => (vec![path], Vec::new()),
                        Some((path, true)) => (Vec::new(), vec![path]),
                        None => (Vec::new(), Vec::new()),
                    };
                Self {
                    kinds,
                    read_paths: Vec::new(),
                    observation_roots: Vec::new(),
                    write_paths,
                    write_path_prefixes,
                    hosts: Vec::new(),
                    session_state_allowed: false,
                    command_allowed: false,
                }
            }
            Capability::RunCommand { .. } => {
                let mut kinds = BTreeSet::new();
                kinds.insert(ImpactKind::CommandExecution);
                Self {
                    kinds,
                    read_paths: Vec::new(),
                    observation_roots: Vec::new(),
                    write_paths: Vec::new(),
                    write_path_prefixes: Vec::new(),
                    hosts: Vec::new(),
                    session_state_allowed: false,
                    command_allowed: true,
                }
            }
            Capability::FetchUrl { host } => {
                let mut kinds = BTreeSet::new();
                kinds.insert(ImpactKind::ExternalObservation);
                Self {
                    kinds,
                    read_paths: Vec::new(),
                    observation_roots: Vec::new(),
                    write_paths: Vec::new(),
                    write_path_prefixes: Vec::new(),
                    hosts: vec![host.clone()],
                    session_state_allowed: false,
                    command_allowed: false,
                }
            }
            Capability::MutateSessionState => {
                let mut kinds = BTreeSet::new();
                kinds.insert(ImpactKind::SessionStateMutation);
                Self {
                    kinds,
                    read_paths: Vec::new(),
                    observation_roots: Vec::new(),
                    write_paths: Vec::new(),
                    write_path_prefixes: Vec::new(),
                    hosts: Vec::new(),
                    session_state_allowed: true,
                    command_allowed: false,
                }
            }
            Capability::ReadAudit => {
                let mut kinds = BTreeSet::new();
                kinds.insert(ImpactKind::AuditObservation);
                Self {
                    kinds,
                    read_paths: Vec::new(),
                    observation_roots: Vec::new(),
                    write_paths: Vec::new(),
                    write_path_prefixes: Vec::new(),
                    hosts: Vec::new(),
                    session_state_allowed: true,
                    command_allowed: false,
                }
            }
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
            (ImpactKind::WorkspaceObservation, ObservedScope::Path(path)) => {
                // Check read_paths (ReadPath capability)
                self.read_paths.iter().any(|rp| path.starts_with(rp))
            }
            (ImpactKind::WorkspaceObservation, ObservedScope::Root(root)) => {
                // Path-component-aware check (not string prefix — "/project-evil" must NOT match "/project")
                let root_path = std::path::Path::new(root);
                self.observation_roots
                    .iter()
                    .any(|r| root_path.starts_with(r))
            }
            (ImpactKind::WorkspaceMutation, ObservedScope::Path(path)) => {
                // Exact-file WritePath grants authorize only the exact path. Directory-glob
                // WritePath grants authorize descendants under the normalized prefix.
                self.write_paths.iter().any(|wp| path == wp)
                    || self
                        .write_path_prefixes
                        .iter()
                        .any(|wp| path.starts_with(wp))
            }
            (ImpactKind::WorkspaceMutation, ObservedScope::Lines { path, .. }) => {
                self.write_paths.iter().any(|wp| path == wp)
                    || self
                        .write_path_prefixes
                        .iter()
                        .any(|wp| path.starts_with(wp))
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

// Cross-type conversion (review concern 10) is in lancet-protocol:
// `impl From<EffectKindSet> for BTreeSet<ImpactKind>`

#[cfg(test)]
mod tests {
    use super::*;
    use lancet_catalog::method::RequiredCapability;
    use lancet_protocol::envelope::Capability;
    use lancet_protocol::policy::{CapabilitySource, SourcedCapability};
    use std::path::PathBuf;

    fn sourced_read_cap(pattern: &str) -> SourcedCapability {
        SourcedCapability {
            capability: Capability::ReadPath {
                path_pattern: pattern.to_string(),
            },
            source: CapabilitySource::Base,
        }
    }

    fn sourced_inspect_cap() -> SourcedCapability {
        SourcedCapability {
            capability: Capability::Inspect,
            source: CapabilitySource::Base,
        }
    }

    fn sourced_write_cap(pattern: &str) -> SourcedCapability {
        SourcedCapability {
            capability: Capability::WritePath {
                path_pattern: pattern.to_string(),
            },
            source: CapabilitySource::Base,
        }
    }

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

    // ── AuthorizedEffectRegion::from_matched_capability tests ──────────

    #[test]
    fn none_matched_capability_yields_empty_region() {
        // Review concern 5: None → empty region (fail-closed)
        let policy = crate::effect::shared_test_policy();
        let region = AuthorizedEffectRegion::from_matched_capability(None, &policy);
        assert_eq!(region, AuthorizedEffectRegion::empty());
    }

    #[test]
    fn read_path_capability_sets_read_region() {
        let policy = crate::effect::shared_test_policy();
        let cap = sourced_read_cap("/project/src");
        let region = AuthorizedEffectRegion::from_matched_capability(Some(&cap), &policy);

        assert!(region.kinds.contains(&ImpactKind::WorkspaceObservation));
        assert!(!region.kinds.contains(&ImpactKind::WorkspaceMutation));
        assert_eq!(region.read_paths, vec![PathBuf::from("/project/src")]);
    }

    #[test]
    fn inspect_capability_sets_observation_roots_from_policy() {
        // Inspect has NO path pattern — region comes from policy roots
        let policy = crate::effect::shared_test_policy();
        let cap = sourced_inspect_cap();
        let region = AuthorizedEffectRegion::from_matched_capability(Some(&cap), &policy);

        assert!(region.kinds.contains(&ImpactKind::WorkspaceObservation));
        assert!(!region.kinds.contains(&ImpactKind::WorkspaceMutation));
        assert_eq!(region.observation_roots, vec![PathBuf::from("/project")]);
    }

    #[test]
    fn observation_only_region_rejects_workspace_mutation() {
        let policy = crate::effect::shared_test_policy();
        let cap = sourced_inspect_cap();
        let region = AuthorizedEffectRegion::from_matched_capability(Some(&cap), &policy);

        let mutation = ObservedEffect {
            kind: ImpactKind::WorkspaceMutation,
            scope: ObservedScope::Path(PathBuf::from("/project/src/main.rs")),
            required_capability: RequiredCapability::WritePath,
            source: crate::effect::ledger::EffectSource::HandlerBackend,
        };
        assert!(!region.contains(&mutation));
    }

    #[test]
    fn read_path_region_contains_in_scope_path() {
        let policy = crate::effect::shared_test_policy();
        let cap = sourced_read_cap("/project/src");
        let region = AuthorizedEffectRegion::from_matched_capability(Some(&cap), &policy);

        let effect = ObservedEffect {
            kind: ImpactKind::WorkspaceObservation,
            scope: ObservedScope::Path(PathBuf::from("/project/src/main.rs")),
            required_capability: RequiredCapability::ReadPath,
            source: crate::effect::ledger::EffectSource::HandlerBackend,
        };
        assert!(region.contains(&effect));
    }

    #[test]
    fn read_path_directory_glob_region_allows_descendant() {
        let policy = crate::effect::shared_test_policy();
        let cap = sourced_read_cap("src/**");
        let region = AuthorizedEffectRegion::from_matched_capability(Some(&cap), &policy);

        let effect = ObservedEffect {
            kind: ImpactKind::WorkspaceObservation,
            scope: ObservedScope::Path(PathBuf::from("/project/src/main.rs")),
            required_capability: RequiredCapability::ReadPath,
            source: crate::effect::ledger::EffectSource::HandlerBackend,
        };
        assert!(region.contains(&effect));
    }

    #[test]
    fn read_path_region_rejects_out_of_scope_path() {
        let policy = crate::effect::shared_test_policy();
        let cap = sourced_read_cap("/project/src");
        let region = AuthorizedEffectRegion::from_matched_capability(Some(&cap), &policy);

        let effect = ObservedEffect {
            kind: ImpactKind::WorkspaceObservation,
            scope: ObservedScope::Path(PathBuf::from("/etc/passwd")),
            required_capability: RequiredCapability::ReadPath,
            source: crate::effect::ledger::EffectSource::HandlerBackend,
        };
        assert!(!region.contains(&effect));
    }

    #[test]
    fn inspect_region_contains_observation_at_policy_root() {
        let policy = crate::effect::shared_test_policy();
        let cap = sourced_inspect_cap();
        let region = AuthorizedEffectRegion::from_matched_capability(Some(&cap), &policy);

        let effect = ObservedEffect {
            kind: ImpactKind::WorkspaceObservation,
            scope: ObservedScope::Root("/project".to_string()),
            required_capability: RequiredCapability::Inspect,
            source: crate::effect::ledger::EffectSource::HandlerBackend,
        };
        assert!(region.contains(&effect));
    }

    #[test]
    fn inspect_region_rejects_prefix_collision() {
        let policy = crate::effect::shared_test_policy();
        let cap = sourced_inspect_cap();
        let region = AuthorizedEffectRegion::from_matched_capability(Some(&cap), &policy);

        // "/project-evil" must NOT pass just because "/project" is authorized
        let effect = ObservedEffect {
            kind: ImpactKind::WorkspaceObservation,
            scope: ObservedScope::Root("/project-evil".to_string()),
            required_capability: RequiredCapability::Inspect,
            source: crate::effect::ledger::EffectSource::HandlerBackend,
        };
        assert!(!region.contains(&effect));
    }

    #[test]
    fn inspect_region_allows_subdirectory() {
        let policy = crate::effect::shared_test_policy();
        let cap = sourced_inspect_cap();
        let region = AuthorizedEffectRegion::from_matched_capability(Some(&cap), &policy);

        let effect = ObservedEffect {
            kind: ImpactKind::WorkspaceObservation,
            scope: ObservedScope::Root("/project/src".to_string()),
            required_capability: RequiredCapability::Inspect,
            source: crate::effect::ledger::EffectSource::HandlerBackend,
        };
        assert!(region.contains(&effect));
    }

    #[test]
    fn write_path_exact_file_region_rejects_descendant() {
        let policy = crate::effect::shared_test_policy();
        let cap = sourced_write_cap("src/main.rs");
        let region = AuthorizedEffectRegion::from_matched_capability(Some(&cap), &policy);

        let effect = ObservedEffect {
            kind: ImpactKind::WorkspaceMutation,
            scope: ObservedScope::Path(PathBuf::from("/project/src/main.rs/generated.txt")),
            required_capability: RequiredCapability::WritePath,
            source: crate::effect::ledger::EffectSource::HandlerBackend,
        };
        assert!(!region.contains(&effect));
    }

    #[test]
    fn write_path_directory_glob_region_allows_descendant() {
        let policy = crate::effect::shared_test_policy();
        let cap = sourced_write_cap("src/**");
        let region = AuthorizedEffectRegion::from_matched_capability(Some(&cap), &policy);

        let effect = ObservedEffect {
            kind: ImpactKind::WorkspaceMutation,
            scope: ObservedScope::Path(PathBuf::from("/project/src/generated.txt")),
            required_capability: RequiredCapability::WritePath,
            source: crate::effect::ledger::EffectSource::HandlerBackend,
        };
        assert!(region.contains(&effect));
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

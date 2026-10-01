// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Cell capability admission — the only authorization authority for
//! Component Tools (#28 B2/B4, C2/C3).
//!
//! Two seams, both pure (no I/O, no wasmtime, no audit):
//!
//! - [`admit_tool`] answers *whether a Tool is callable and with which
//!   capabilities*, from the operator/profile policy. It runs per call,
//!   before the runtime is entered, and its [`AdmissionEvidence`] is the
//!   call's "why" — a denial carries it too, so a refused call keeps its
//!   evidence.
//! - [`authorize_operation`] answers whether one Tool Host operation's cell
//!   and concrete target are covered by that admitted set. A failure rejects
//!   only the operation; the host facility is never invoked (B3).
//!
//! Admission is unconditional; no warn/off mode can soften these stages.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use lancet_protocol::cell::{Capability, Cell, ChannelKind, ToolDescriptor, Trust};
use lancet_protocol::evidence::{
    ADMISSION_EVIDENCE_VERSION, AdmissionEvidence, CapabilityAdmissionPolicy, ObservedScope,
    ObservedTarget,
};

/// A Tool that passed admission: the descriptor it was admitted from, the
/// surviving capabilities by name, and the evidence explaining the outcome.
///
/// The descriptor rides along so the per-operation seam can resolve cells and
/// scopes from the call state alone.
#[derive(Debug, Clone)]
pub struct AdmittedTool {
    pub descriptor: Arc<ToolDescriptor>,
    pub admitted: BTreeMap<String, Capability>,
    pub evidence: AdmissionEvidence,
}

impl AdmittedTool {
    /// True if some admitted capability names exactly this cell. Admission is
    /// per capability, enforcement per cell: aliases collapse here, and an
    /// alias never becomes an effect (B5).
    pub fn covers(&self, cell: &Cell) -> bool {
        self.admitted.values().any(|c| &c.cell == cell)
    }
}

/// Which admission stage refused the Tool.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AdmissionDenialReason {
    /// Stage 1: capability grant derives from the profile's enabled tools;
    /// a tool outside that set has no capability at all.
    #[error("tool {0} is not enabled")]
    ToolNotEnabled(String),
    /// Stage 3 tail: the deny list and the boundary predicate left nothing,
    /// so the Tool is not callable (B2).
    #[error("tool {0} has no admitted capability")]
    NoAdmittedCapabilities(String),
}

/// An admission refusal. It carries the (empty or partial) evidence so the
/// denied call's audit record still states why it was refused — a denial must
/// never finalize with absent metadata.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{reason}")]
pub struct AdmissionDenial {
    pub reason: AdmissionDenialReason,
    pub evidence: AdmissionEvidence,
}

/// Startup validation of an operator policy against the composed catalog.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PolicyValidationError {
    /// A deny entry that names no composed capability is a typo silently
    /// keeping the grant it meant to remove, so it is a hard startup error
    /// (C2) — including on a binary whose component catalog is empty, where
    /// any deny entry therefore fails startup.
    #[error("profile denies capability names no composed Tool declares: {0:?}")]
    UnknownDeniedCapabilities(Vec<String>),
}

/// Refuse a policy whose deny list names capabilities the composed catalog
/// does not declare (C2). Runs once, where catalog and policy first coexist
/// (runtime construction), never per call.
pub fn validate_policy_against_catalog<'a>(
    catalog_capabilities: impl IntoIterator<Item = &'a str>,
    policy: &CapabilityAdmissionPolicy,
) -> Result<(), PolicyValidationError> {
    let known: BTreeSet<&str> = catalog_capabilities.into_iter().collect();
    let unknown: Vec<String> = policy
        .denied_capabilities
        .iter()
        .filter(|name| !known.contains(name.as_str()))
        .cloned()
        .collect();
    if unknown.is_empty() {
        Ok(())
    } else {
        Err(PolicyValidationError::UnknownDeniedCapabilities(unknown))
    }
}

/// Admit a Tool under an operator policy. The stage order is fixed (C3):
///
/// 1. enabled: the tool must be in `policy.enabled_tools`, and its declared
///    capabilities are the starting set — there is no independent allow-list.
/// 2. deny/subtract: capabilities whose *name* is denied are removed.
/// 3. `max_boundary`: with `in`, every capability whose scope trust resolves
///    to `ex` is removed — an undeclared scope resolves to `ex` (B4).
///
/// The per-operation concrete target check happens at call time
/// ([`authorize_operation`]), not here.
// Both variants carry the same evidence-sized payload, so boxing the denial
// would buy no stack space — the "large Err" heuristic does not apply.
#[allow(clippy::result_large_err)]
pub fn admit_tool(
    descriptor: Arc<ToolDescriptor>,
    policy: &CapabilityAdmissionPolicy,
) -> Result<AdmittedTool, AdmissionDenial> {
    if !policy.enabled_tools.contains(&descriptor.name) {
        return Err(AdmissionDenial {
            reason: AdmissionDenialReason::ToolNotEnabled(descriptor.name.clone()),
            evidence: AdmissionEvidence {
                version: ADMISSION_EVIDENCE_VERSION,
                tool: descriptor.name.clone(),
                approved_capabilities: Vec::new(),
                denied_capabilities: Vec::new(),
                boundary_excluded: Vec::new(),
                max_boundary: policy.max_boundary,
            },
        });
    }

    let mut admitted: BTreeMap<String, Capability> = BTreeMap::new();
    let mut denied: BTreeSet<String> = BTreeSet::new();
    let mut boundary_excluded: BTreeSet<String> = BTreeSet::new();

    for capability in &descriptor.capabilities {
        if policy.denied_capabilities.contains(&capability.name) {
            denied.insert(capability.name.clone());
            continue;
        }
        if policy.max_boundary == Trust::In
            && descriptor.trust_of(&capability.cell.scope) == Trust::Ex
        {
            boundary_excluded.insert(capability.name.clone());
            continue;
        }
        admitted.insert(capability.name.clone(), capability.clone());
    }

    // Every list is sorted and deduplicated by construction, so persisted
    // evidence is stable across runs.
    let evidence = AdmissionEvidence {
        version: ADMISSION_EVIDENCE_VERSION,
        tool: descriptor.name.clone(),
        approved_capabilities: admitted.keys().cloned().collect(),
        denied_capabilities: denied.into_iter().collect(),
        boundary_excluded: boundary_excluded.into_iter().collect(),
        max_boundary: policy.max_boundary,
    };

    if admitted.is_empty() {
        return Err(AdmissionDenial {
            reason: AdmissionDenialReason::NoAdmittedCapabilities(descriptor.name.clone()),
            evidence,
        });
    }

    Ok(AdmittedTool {
        descriptor,
        admitted,
        evidence,
    })
}

/// Why one Tool Host operation was refused. The `Display` text is what the
/// recorded `DeniedOperation` carries as its reason.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OperationDenial {
    #[error("cell {0} is not covered by an admitted capability")]
    CellNotAdmitted(String),
    /// No channel can legitimately produce a host target yet, and `none`
    /// carries nothing to validate — both fail closed. `path` left this set in
    /// #30, when the workspace channel arrived to produce it.
    #[error("target kind {0} is not authorizable")]
    UnsupportedTarget(&'static str),
    #[error("session target {actual} is not the call's turn {expected}")]
    TurnMismatch { expected: String, actual: String },
    /// A path target whose cell does not root in a workspace-channel scope.
    #[error("path target requires a workspace-channel scope, but {0} is not one")]
    PathOutsideWorkspaceChannel(String),
    /// A host target whose cell does not root in a network-channel scope.
    #[error("host target requires a network-channel scope, but {0} is not one")]
    HostOutsideNetworkChannel(String),
    #[error("host target is empty or malformed")]
    HostNotNormalized,
    /// A path target that is not in the project-relative comparison form the
    /// host produces. Structural, not a containment check: containment is
    /// enforced by the workspace's per-segment resolution, and this refuses a
    /// *shape* that resolution could never have emitted.
    #[error("path target {0} is not a project-relative resolved path")]
    PathNotProjectRelative(String),
}

/// Authorize one operation: its host-bound cell must be covered by an
/// admitted capability, and its concrete target must pass host-owned scope
/// validation. Guests never name either — both are host-resolved.
///
/// # Path targets
///
/// #30 opened `ObservedTarget::Path` for the workspace channel (it was refused
/// outright while no channel could produce one). Two conditions hold, and
/// neither is a substitute for filesystem containment:
///
/// 1. The cell's scope must be declared by the Tool on the **workspace**
///    channel. A Tool cannot use a session- or network-channel grant to touch a
///    path, so a capability audited as "session" can never turn into a file
///    read.
/// 2. The path must be in the project-relative comparison form the host's
///    resolver emits: not absolute, no `..` component, no NUL. This refuses
///    shapes the resolver could not have produced, which is what makes a
///    fabricated target detectable here rather than deep in an IO call.
///
/// Containment itself — that the path is inside the root after every symlink,
/// that System Path Exclusion applied, that policy makes it visible — is the
/// concrete workspace's job, enforced per segment during resolution. This
/// function authorizes the *grant*; the resolver enforces the *boundary*. Both
/// run, in that order, for every operation.
pub fn authorize_operation(
    admitted: &AdmittedTool,
    cell: &Cell,
    target: &ObservedTarget,
    turn_id: &str,
) -> Result<(), OperationDenial> {
    if !admitted.covers(cell) {
        return Err(OperationDenial::CellNotAdmitted(cell.reference()));
    }
    match target {
        ObservedTarget::Session { turn_id: actual } if actual == turn_id => Ok(()),
        ObservedTarget::Session { turn_id: actual } => Err(OperationDenial::TurnMismatch {
            expected: turn_id.to_owned(),
            actual: actual.clone(),
        }),
        ObservedTarget::Path { path } => {
            let declared = admitted
                .descriptor
                .scopes
                .iter()
                .find(|scope| scope.name == cell.scope);
            match declared {
                Some(scope) if scope.channel == ChannelKind::Workspace => {}
                // An undeclared scope has no channel, so it cannot be the
                // workspace one: fail closed rather than defaulting.
                _ => {
                    return Err(OperationDenial::PathOutsideWorkspaceChannel(
                        cell.scope.clone(),
                    ));
                }
            }
            if !is_project_relative(path) {
                return Err(OperationDenial::PathNotProjectRelative(path.clone()));
            }
            Ok(())
        }
        ObservedTarget::Host { host } => {
            let declared = admitted
                .descriptor
                .scopes
                .iter()
                .find(|scope| scope.name == cell.scope);
            if !matches!(declared, Some(scope) if scope.channel == ChannelKind::Network) {
                return Err(OperationDenial::HostOutsideNetworkChannel(
                    cell.scope.clone(),
                ));
            }
            let normalized =
                url::Host::parse(host).is_ok_and(|parsed| parsed.to_string() == host.as_str());
            if !normalized {
                return Err(OperationDenial::HostNotNormalized);
            }
            Ok(())
        }
        ObservedTarget::None => Err(OperationDenial::UnsupportedTarget("none")),
    }
}

/// Whether a path target has the shape the host's resolver emits.
///
/// `.` is the root itself and is legal. Anything absolute, containing a `..`
/// component, or carrying a NUL is a shape the resolver never produces.
fn is_project_relative(path: &str) -> bool {
    if path.is_empty() || path.contains('\0') || path.starts_with('/') {
        return false;
    }
    !std::path::Path::new(path)
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
}

/// Host-owned scope resolution for evidence: trust comes from the descriptor
/// (undeclared → `ex`, B4) and never from the guest. A declared scope's own
/// channel wins; `channel` is the fallback for a scope the descriptor does
/// not declare.
pub fn resolve_scope(
    descriptor: &ToolDescriptor,
    scope: &str,
    channel: ChannelKind,
) -> ObservedScope {
    let declared = descriptor.scopes.iter().find(|s| s.name == scope);
    ObservedScope {
        scope: scope.to_owned(),
        trust: declared.map(|s| s.trust).unwrap_or_default(),
        channel: declared.map(|s| s.channel).unwrap_or(channel),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use lancet_protocol::cell::{ArtifactIdentity, CtxOp, ExtImpact, ScopeDecl};

    fn cell(ctx_op: CtxOp, scope: &str) -> Cell {
        Cell {
            ctx_op,
            ext_impact: ExtImpact::None,
            scope: scope.into(),
        }
    }

    /// A multi-scope descriptor: two `in` scopes on different channels, an
    /// `ex` network scope, and room for undeclared-scope capabilities (B4's
    /// fail-closed case). Two admissible scopes make the scope axis of
    /// coverage observable.
    fn descriptor(capabilities: Vec<Capability>) -> Arc<ToolDescriptor> {
        Arc::new(ToolDescriptor {
            name: "noop.echo".into(),
            version: "0.1.0".into(),
            description: "admission fixture".into(),
            params_schema: serde_json::Value::Null,
            result_schema: serde_json::Value::Null,
            scopes: vec![
                ScopeDecl {
                    name: "session".into(),
                    trust: Trust::In,
                    channel: ChannelKind::Session,
                },
                ScopeDecl {
                    name: "workspace".into(),
                    trust: Trust::In,
                    channel: ChannelKind::Workspace,
                },
                ScopeDecl {
                    name: "net".into(),
                    trust: Trust::Ex,
                    channel: ChannelKind::Network,
                },
            ],
            capabilities,
            artifact: ArtifactIdentity {
                sha256: "0".repeat(64),
                toolchain: "test".into(),
            },
            abi_version: 1,
        })
    }

    fn capability(name: &str, ctx_op: CtxOp, scope: &str) -> Capability {
        Capability {
            name: name.into(),
            cell: cell(ctx_op, scope),
        }
    }

    fn policy(deny: &[&str], max_boundary: Trust) -> CapabilityAdmissionPolicy {
        CapabilityAdmissionPolicy {
            enabled_tools: BTreeSet::from(["noop.echo".to_string()]),
            denied_capabilities: deny.iter().map(|s| (*s).to_string()).collect(),
            max_boundary,
        }
    }

    fn admitted_session_tool() -> AdmittedTool {
        admit_tool(
            descriptor(vec![capability("session.events", CtxOp::Mutate, "session")]),
            &policy(&[], Trust::In),
        )
        .expect("session capability is admitted")
    }

    /// Two capability names over one cell: both are admitted (admission is
    /// per capability) and both appear in the evidence, while coverage is
    /// decided per cell.
    #[test]
    fn capability_alias_admits_both_names_for_one_cell() {
        let admitted = admit_tool(
            descriptor(vec![
                capability("session.result", CtxOp::Alloc, "session"),
                capability("session.echo", CtxOp::Alloc, "session"),
            ]),
            &policy(&[], Trust::In),
        )
        .expect("aliases admit");

        assert_eq!(
            admitted.evidence.approved_capabilities,
            vec!["session.echo".to_string(), "session.result".to_string()]
        );
        assert!(admitted.covers(&cell(CtxOp::Alloc, "session")));
        // The alias set collapses to one cell; nothing else is covered — on
        // any of the three axes.
        assert!(!admitted.covers(&cell(CtxOp::Mutate, "session")));
        assert!(!admitted.covers(&cell(CtxOp::Alloc, "workspace")));
        assert!(!admitted.covers(&Cell {
            ctx_op: CtxOp::Alloc,
            ext_impact: ExtImpact::Read,
            scope: "session".into(),
        }));
    }

    /// Coverage is exact on all three axes, in both directions: a capability
    /// on one scope never covers the same `ctx_op`/`ext_impact` on another,
    /// and a differing `ext_impact` is a different cell.
    #[test]
    fn coverage_never_crosses_scope_or_ext_impact() {
        let admitted = admit_tool(
            descriptor(vec![
                capability("session.result", CtxOp::Alloc, "session"),
                capability("workspace.events", CtxOp::Mutate, "workspace"),
            ]),
            &policy(&[], Trust::In),
        )
        .expect("both in-scopes admit");

        assert!(admitted.covers(&cell(CtxOp::Alloc, "session")));
        assert!(admitted.covers(&cell(CtxOp::Mutate, "workspace")));
        // Each admitted cell's ctx_op on the *other* admitted scope.
        assert!(!admitted.covers(&cell(CtxOp::Alloc, "workspace")));
        assert!(!admitted.covers(&cell(CtxOp::Mutate, "session")));
        // Same ctx_op and scope, external impact — still a different cell.
        for ext_impact in [ExtImpact::Read, ExtImpact::Write] {
            assert!(
                !admitted.covers(&Cell {
                    ctx_op: CtxOp::Alloc,
                    ext_impact,
                    scope: "session".into(),
                }),
                "{ext_impact:?} must not be covered by an ext_impact=none capability"
            );
        }
    }

    /// Stage 2 subtracts by name: the surviving capability still admits its
    /// own cell, and the removed name is recorded as denied, not approved.
    #[test]
    fn profile_deny_subtracts_one_capability() {
        let admitted = admit_tool(
            descriptor(vec![
                capability("session.result", CtxOp::Alloc, "session"),
                capability("session.events", CtxOp::Mutate, "session"),
            ]),
            &policy(&["session.events"], Trust::In),
        )
        .expect("partial admission still admits the tool");

        assert_eq!(
            admitted.evidence.approved_capabilities,
            vec!["session.result".to_string()]
        );
        assert_eq!(
            admitted.evidence.denied_capabilities,
            vec!["session.events".to_string()]
        );
        assert!(admitted.evidence.boundary_excluded.is_empty());
        assert!(admitted.covers(&cell(CtxOp::Alloc, "session")));
        assert!(!admitted.covers(&cell(CtxOp::Mutate, "session")));
    }

    #[test]
    fn admission_evidence_tracks_policy_stages() {
        for (
            name,
            capabilities,
            deny,
            max_boundary,
            approved,
            denied,
            excluded,
            evidence_boundary,
        ) in [
            // Stage 3 excludes declared ex and undeclared scopes alike.
            (
                "boundary_excludes_ex_and_undeclared_scopes",
                vec![
                    capability("session.result", CtxOp::Alloc, "session"),
                    capability("net.fetch", CtxOp::Alloc, "net"),
                    capability("ghost.read", CtxOp::Alloc, "undeclared"),
                ],
                vec![],
                Trust::In,
                Some(vec!["session.result"]),
                Some(vec![]),
                vec!["ghost.read", "net.fetch"],
                Some(Trust::In),
            ),
            // Deny precedes boundary filtering: a denied ex capability is not boundary-excluded.
            (
                "deny_precedes_the_boundary_predicate_in_the_evidence",
                vec![
                    capability("session.result", CtxOp::Alloc, "session"),
                    capability("net.fetch", CtxOp::Alloc, "net"),
                ],
                vec!["net.fetch"],
                Trust::In,
                None,
                Some(vec!["net.fetch"]),
                vec![],
                None,
            ),
            // Ex admits even undeclared scopes; observed trust remains ex.
            (
                "boundary_ex_admits_external_capabilities",
                vec![
                    capability("net.fetch", CtxOp::Alloc, "net"),
                    capability("ghost.read", CtxOp::Alloc, "undeclared"),
                ],
                vec![],
                Trust::Ex,
                Some(vec!["ghost.read", "net.fetch"]),
                None,
                vec![],
                None,
            ),
            // Evidence is sorted independently of declaration order, keeping audit bytes stable.
            (
                "evidence_lists_are_sorted",
                vec![
                    capability("session.zeta", CtxOp::Alloc, "session"),
                    capability("session.alpha", CtxOp::Mutate, "session"),
                    capability("session.deny-b", CtxOp::Alloc, "session"),
                    capability("session.deny-a", CtxOp::Alloc, "session"),
                    capability("net.zeta", CtxOp::Alloc, "net"),
                    capability("net.alpha", CtxOp::Alloc, "net"),
                ],
                vec!["session.deny-b", "session.deny-a"],
                Trust::In,
                Some(vec!["session.alpha", "session.zeta"]),
                Some(vec!["session.deny-a", "session.deny-b"]),
                vec!["net.alpha", "net.zeta"],
                None,
            ),
        ] {
            let admitted = admit_tool(descriptor(capabilities), &policy(&deny, max_boundary))
                .unwrap_or_else(|error| panic!("{name}: {error:?}"));
            for (actual, expected) in [
                (&admitted.evidence.approved_capabilities, approved),
                (&admitted.evidence.denied_capabilities, denied),
                (&admitted.evidence.boundary_excluded, Some(excluded)),
            ] {
                if let Some(expected) = expected {
                    assert_eq!(*actual, expected, "{name}");
                }
            }
            if let Some(expected) = evidence_boundary {
                assert_eq!(admitted.evidence.max_boundary, expected, "{name}");
            }
        }
    }

    /// An empty remainder fails the tool closed, and the denial keeps the
    /// evidence that explains which stage removed what.
    #[test]
    fn empty_remainder_denies_the_tool_with_evidence() {
        let denial = admit_tool(
            descriptor(vec![
                capability("session.events", CtxOp::Mutate, "session"),
                capability("net.fetch", CtxOp::Alloc, "net"),
            ]),
            &policy(&["session.events"], Trust::In),
        )
        .expect_err("nothing survives both stages");

        assert_eq!(
            denial.reason,
            AdmissionDenialReason::NoAdmittedCapabilities("noop.echo".into())
        );
        assert!(denial.evidence.approved_capabilities.is_empty());
        assert_eq!(
            denial.evidence.denied_capabilities,
            vec!["session.events".to_string()]
        );
        assert_eq!(
            denial.evidence.boundary_excluded,
            vec!["net.fetch".to_string()]
        );
    }

    /// Stage 1 denial: no capability is even considered, so the evidence
    /// carries an empty approved set — but still names the tool and boundary.
    #[test]
    fn tool_not_enabled_denies_with_empty_approved_set() {
        let denial = admit_tool(
            descriptor(vec![capability("session.result", CtxOp::Alloc, "session")]),
            &CapabilityAdmissionPolicy::default(),
        )
        .expect_err("the fail-closed default enables no tool");

        assert_eq!(
            denial.reason,
            AdmissionDenialReason::ToolNotEnabled("noop.echo".into())
        );
        assert_eq!(denial.evidence.tool, "noop.echo");
        assert_eq!(denial.evidence.version, ADMISSION_EVIDENCE_VERSION);
        assert!(denial.evidence.approved_capabilities.is_empty());
        assert!(denial.evidence.denied_capabilities.is_empty());
        assert!(denial.evidence.boundary_excluded.is_empty());
        assert_eq!(denial.evidence.max_boundary, Trust::In);
    }

    #[test]
    fn policy_validation_rejects_unknown_denied_capabilities() {
        for (name, catalog, deny, expected) in [
            // C2 reports every unknown name at startup.
            (
                "unknown_deny_names_are_a_policy_error/unknown",
                vec!["session.events", "session.result"],
                vec!["session.events", "session.typo", "net.typo"],
                Err(PolicyValidationError::UnknownDeniedCapabilities(vec![
                    "net.typo".into(),
                    "session.typo".into(),
                ])),
            ),
            // Every name known is accepted.
            (
                "unknown_deny_names_are_a_policy_error/all_known",
                vec![
                    "session.events",
                    "session.result",
                    "session.typo",
                    "net.typo",
                ],
                vec!["session.events", "session.typo", "net.typo"],
                Ok(()),
            ),
            // An empty catalog rejects any deny entry, as the profile schema requires.
            (
                "empty_catalog_rejects_any_deny_entry/nonempty_deny",
                vec![],
                vec!["session.events"],
                Err(PolicyValidationError::UnknownDeniedCapabilities(vec![
                    "session.events".into(),
                ])),
            ),
            (
                "empty_catalog_rejects_any_deny_entry/empty_deny",
                vec![],
                vec![],
                Ok(()),
            ),
        ] {
            assert_eq!(
                validate_policy_against_catalog(catalog, &policy(&deny, Trust::In)),
                expected,
                "{name}"
            );
        }
    }

    #[test]
    fn authorize_operation_rejects_uncovered_cells_and_foreign_turns() {
        let admitted = admitted_session_tool();
        for (name, operation_cell, target, expected) in [
            // Uncovered cells are refused before reaching any host facility.
            (
                "authorize_operation_denies_an_uncovered_cell",
                cell(CtxOp::Alloc, "session"),
                ObservedTarget::Session {
                    turn_id: "turn-1".into(),
                },
                OperationDenial::CellNotAdmitted("alloc:none:session".into()),
            ),
            // Changing either scope or external impact escapes the exact admitted cell.
            (
                "authorize_operation_denies_a_cell_differing_only_by_scope_or_ext_impact/scope",
                cell(CtxOp::Mutate, "workspace"),
                ObservedTarget::Session {
                    turn_id: "turn-1".into(),
                },
                OperationDenial::CellNotAdmitted("mutate:none:workspace".into()),
            ),
            (
                "authorize_operation_denies_a_cell_differing_only_by_scope_or_ext_impact/ext_impact",
                Cell {
                    ctx_op: CtxOp::Mutate,
                    ext_impact: ExtImpact::Write,
                    scope: "session".into(),
                },
                ObservedTarget::Session {
                    turn_id: "turn-1".into(),
                },
                OperationDenial::CellNotAdmitted("mutate:write:session".into()),
            ),
            // An admitted cell cannot authorize a different turn.
            (
                "authorize_operation_denies_a_foreign_turn_id",
                cell(CtxOp::Mutate, "session"),
                ObservedTarget::Session {
                    turn_id: "other-turn".into(),
                },
                OperationDenial::TurnMismatch {
                    expected: "turn-1".into(),
                    actual: "other-turn".into(),
                },
            ),
            // Cell coverage precedes path checks; an uncovered cell must not reveal path validity.
            (
                "path_target_still_requires_an_admitted_cell",
                cell(CtxOp::Alloc, "workspace"),
                ObservedTarget::Path {
                    path: "src/lib.rs".into(),
                },
                OperationDenial::CellNotAdmitted("alloc:none:workspace".into()),
            ),
        ] {
            assert_eq!(
                authorize_operation(&admitted, &operation_cell, &target, "turn-1"),
                Err(expected),
                "{name}"
            );
        }
    }

    /// Host targets require an admitted network-channel cell; `none` remains unsupported.
    #[test]
    fn authorize_operation_gates_host_targets_by_network_channel() {
        let admitted = admit_tool(
            descriptor(vec![capability("network.fetch", CtxOp::Alloc, "net")]),
            &policy(&[], Trust::Ex),
        )
        .expect("network capability admits");
        let target = ObservedTarget::Host {
            host: "example.com".into(),
        };
        assert_eq!(
            authorize_operation(&admitted, &cell(CtxOp::Alloc, "net"), &target, "turn-1"),
            Ok(())
        );
        assert_eq!(
            authorize_operation(
                &admitted,
                &cell(CtxOp::Alloc, "net"),
                &ObservedTarget::Host {
                    host: String::new()
                },
                "turn-1",
            ),
            Err(OperationDenial::HostNotNormalized)
        );
        for host in [
            "EXAMPLE.com",
            "example.com/path",
            "user@example.com",
            "example.com:443",
        ] {
            assert_eq!(
                authorize_operation(
                    &admitted,
                    &cell(CtxOp::Alloc, "net"),
                    &ObservedTarget::Host { host: host.into() },
                    "turn-1",
                ),
                Err(OperationDenial::HostNotNormalized),
                "{host}",
            );
        }
        assert_eq!(
            authorize_operation(
                &admitted,
                &cell(CtxOp::Alloc, "net"),
                &ObservedTarget::Host {
                    host: "[::1]".into(),
                },
                "turn-1",
            ),
            Ok(())
        );

        let session_tool = admitted_session_tool();
        assert_eq!(
            authorize_operation(
                &session_tool,
                &cell(CtxOp::Mutate, "session"),
                &target,
                "turn-1",
            ),
            Err(OperationDenial::HostOutsideNetworkChannel("session".into()))
        );
        assert_eq!(
            authorize_operation(
                &session_tool,
                &cell(CtxOp::Mutate, "session"),
                &ObservedTarget::None,
                "turn-1",
            ),
            Err(OperationDenial::UnsupportedTarget("none"))
        );
    }

    /// #30: a path target is authorizable only through a workspace-channel
    /// scope. A session-channel grant reaching a file would mean a capability
    /// audited as "session" could read the project.
    #[test]
    fn path_target_requires_a_workspace_channel_scope() {
        let admitted = admit_tool(
            descriptor(vec![
                capability("session.events", CtxOp::Mutate, "session"),
                capability("workspace.observation", CtxOp::Alloc, "workspace"),
            ]),
            &policy(&[], Trust::In),
        )
        .expect("both capabilities admit");
        let target = ObservedTarget::Path {
            path: "src/lib.rs".into(),
        };

        // The workspace-channel cell authorizes.
        assert_eq!(
            authorize_operation(
                &admitted,
                &cell(CtxOp::Alloc, "workspace"),
                &target,
                "turn-1"
            ),
            Ok(())
        );
        // The session-channel cell does not, even though it is equally admitted.
        assert_eq!(
            authorize_operation(
                &admitted,
                &cell(CtxOp::Mutate, "session"),
                &target,
                "turn-1"
            ),
            Err(OperationDenial::PathOutsideWorkspaceChannel(
                "session".into()
            ))
        );
    }

    /// An undeclared scope has no channel, so it cannot be the workspace one.
    ///
    /// Two independent refusals stand between such a cell and a path, and this
    /// pins both. Admission removes it first — an undeclared scope resolves to
    /// `ex` trust, which a `max_boundary = in` policy excludes — so the tool with
    /// only that capability never admits at all. Reaching the authorizer with the
    /// cell anyway (a laxer boundary, or a host bug) still fails: the scope is not
    /// on the workspace channel.
    #[test]
    fn path_target_with_an_undeclared_scope_fails_closed() {
        // Stage one: the boundary excludes the undeclared-scope capability, so
        // there is nothing left to admit.
        let denial = admit_tool(
            descriptor(vec![capability("ghost", CtxOp::Alloc, "undeclared")]),
            &policy(&[], Trust::In),
        )
        .expect_err("an undeclared scope is ex-trust and the boundary excludes it");
        assert_eq!(denial.evidence.boundary_excluded, vec!["ghost".to_string()]);

        // Stage two: with a boundary that admits it, the authorizer still refuses
        // the path target on the channel check.
        let admitted = admit_tool(
            descriptor(vec![capability("ghost", CtxOp::Alloc, "undeclared")]),
            &policy(&[], Trust::Ex),
        )
        .expect("an ex boundary admits the capability");
        assert_eq!(
            authorize_operation(
                &admitted,
                &cell(CtxOp::Alloc, "undeclared"),
                &ObservedTarget::Path {
                    path: "src/lib.rs".into()
                },
                "turn-1"
            ),
            Err(OperationDenial::PathOutsideWorkspaceChannel(
                "undeclared".into()
            ))
        );
    }

    /// A path target must have the shape the host's resolver emits. This is not
    /// containment — the workspace enforces that per segment — but a fabricated
    /// target is detectable here rather than deep in an IO call.
    #[test]
    fn path_target_rejects_shapes_the_resolver_never_emits() {
        let admitted = admit_tool(
            descriptor(vec![capability(
                "workspace.observation",
                CtxOp::Alloc,
                "workspace",
            )]),
            &policy(&[], Trust::In),
        )
        .expect("workspace capability admits");
        let operation_cell = cell(CtxOp::Alloc, "workspace");

        for bad in ["/etc/passwd", "../outside", "src/../../etc", "", "a\0b"] {
            assert_eq!(
                authorize_operation(
                    &admitted,
                    &operation_cell,
                    &ObservedTarget::Path {
                        path: bad.to_string()
                    },
                    "turn-1"
                ),
                Err(OperationDenial::PathNotProjectRelative(bad.to_string())),
                "{bad:?} is not a shape the resolver emits"
            );
        }
        // `.` is the root itself, and a nested relative path is the ordinary case.
        for good in [".", "src/lib.rs", "a/b/c.txt"] {
            assert_eq!(
                authorize_operation(
                    &admitted,
                    &operation_cell,
                    &ObservedTarget::Path {
                        path: good.to_string()
                    },
                    "turn-1"
                ),
                Ok(()),
                "{good:?} is a legal resolved path"
            );
        }
    }

    #[test]
    fn resolve_scope_is_fail_closed_for_undeclared_scopes() {
        let descriptor = descriptor(vec![]);
        let session = resolve_scope(&descriptor, "session", ChannelKind::Session);
        assert_eq!(session.trust, Trust::In);
        assert_eq!(session.channel, ChannelKind::Session);

        // Undeclared: trust falls back to `ex`, and the host's own channel
        // stands in for the missing declaration.
        let ghost = resolve_scope(&descriptor, "ghost", ChannelKind::Session);
        assert_eq!(ghost.trust, Trust::Ex);
        assert_eq!(ghost.channel, ChannelKind::Session);
    }
}

// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Effect checker — decides whether observed effects are within the authorized region.
//!
//! The check is `observed ⊆ authorized_effect_region`, NOT
//! `observed ⊆ static EffectDecl`. The checker is a pure decision module
//! (structural analog: guard_l1.rs) — no I/O, no mutation, no cep_mode logic.
//! Callers apply enforce/warn/off gating to the verdict.

use std::collections::BTreeSet;

use lancet_protocol::effect_types::{EffectDecl, ImpactKind};
use lancet_protocol::error::{CepErrorData, EffectErrorData};
use lancet_protocol::{CepError, CepErrorCode, Retriability};

use super::ledger::{EffectSource, ObservedEffect};
use super::region::AuthorizedEffectRegion;

/// Check observed effects against a pre-built authorized region.
///
/// This is the core checking function — it returns the raw verdict
/// (Ok or EffectViolation) without cep_mode gating. Callers apply
/// enforce/warn/off decisions externally.
///
/// Source filtering:
/// - `HandlerBackend` — charged against the contract
/// - `EffectProbe` / `GuardProbe` — skipped entirely
/// - `Verify` — audited but not charged (envelope-internal constraint)
pub fn check_effects_against_region(
    method: &str,
    observed: &[ObservedEffect],
    authorized: &AuthorizedEffectRegion,
    effect_decl: &EffectDecl,
) -> Result<(), CepError> {
    check_effects_against_region_inner(method, observed, authorized, effect_decl)
}

/// Hard-fail if any charged observed effect kind is globally denied.
///
/// This is intentionally independent of region membership and `CepMode`: an
/// effect-kind deny is a kill switch for runtime-observed handler effects, not
/// a warn/off-tunable region policy.
pub fn check_denied_effect_kinds(
    method: &str,
    observed: &[ObservedEffect],
    effect_decl: &EffectDecl,
    denied_effect_kinds: &BTreeSet<ImpactKind>,
) -> Result<(), CepError> {
    if denied_effect_kinds.is_empty() {
        return Ok(());
    }

    let mut observed_denied_kinds: Vec<ImpactKind> = Vec::new();
    let mut violations: Vec<String> = Vec::new();

    for effect in observed {
        if effect.source != EffectSource::HandlerBackend {
            continue;
        }
        if denied_effect_kinds.contains(&effect.kind) {
            observed_denied_kinds.push(effect.kind.clone());
            violations.push(format!(
                "{:?}@{:?} denied by effect-kind policy",
                effect.kind, effect.scope
            ));
        }
    }

    if violations.is_empty() {
        return Ok(());
    }

    Err(effect_violation_error(
        method,
        effect_decl,
        Vec::new(),
        observed_denied_kinds,
        violations,
        "observed effect denied by effect-kind policy",
    ))
}

pub fn check_effects_against_region_with_denies(
    method: &str,
    observed: &[ObservedEffect],
    authorized: &AuthorizedEffectRegion,
    effect_decl: &EffectDecl,
    denied_effect_kinds: &BTreeSet<ImpactKind>,
) -> Result<(), CepError> {
    check_denied_effect_kinds(method, observed, effect_decl, denied_effect_kinds)?;
    check_effects_against_region_inner(method, observed, authorized, effect_decl)
}

fn check_effects_against_region_inner(
    method: &str,
    observed: &[ObservedEffect],
    authorized: &AuthorizedEffectRegion,
    effect_decl: &EffectDecl,
) -> Result<(), CepError> {
    let mut violations: Vec<String> = Vec::new();
    let mut charged_kinds: Vec<ImpactKind> = Vec::new();

    for effect in observed {
        match effect.source {
            EffectSource::HandlerBackend => {
                charged_kinds.push(effect.kind.clone());
                if !authorized.contains(effect) {
                    violations.push(format!(
                        "{:?}@{:?} exceeds authorized region",
                        effect.kind, effect.scope
                    ));
                }
            }
            EffectSource::EffectProbe | EffectSource::GuardProbe => {}
            EffectSource::Verify => {}
        }
    }

    if violations.is_empty() {
        return Ok(());
    }

    let authorized_kinds: Vec<ImpactKind> = authorized.kinds.iter().cloned().collect();

    Err(effect_violation_error(
        method,
        effect_decl,
        authorized_kinds,
        charged_kinds,
        violations,
        "observed effect exceeds authorized region",
    ))
}

fn effect_violation_error(
    method: &str,
    effect_decl: &EffectDecl,
    authorized_kinds: Vec<ImpactKind>,
    observed_kinds: Vec<ImpactKind>,
    violations: Vec<String>,
    reason: &str,
) -> CepError {
    CepError::new(
        CepErrorCode::EffectViolation,
        format!(
            "{}: {} ({} violation{})",
            method,
            reason,
            violations.len(),
            if violations.len() == 1 { "" } else { "s" }
        ),
        CepErrorData::Effect(Box::new(EffectErrorData {
            method: method.to_string(),
            declared_effect: effect_decl.kinds.0.to_vec(),
            authorized_effect: authorized_kinds,
            observed_effects: observed_kinds,
            violations,
        })),
        Retriability::Permanent,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect::ledger::{EffectSource, ObservedScope};
    use crate::effect::region::AuthorizedEffectRegion;
    use lancet_catalog::method::RequiredCapability;
    use lancet_protocol::effect_types::{EffectKindSet, EffectScopeKind, FailureSemantics, OverlayBehavior};
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    fn observation_decl() -> EffectDecl {
        EffectDecl {
            kinds: EffectKindSet::from_static(&[ImpactKind::WorkspaceObservation]),
            scope: EffectScopeKind::ObservationRoot,
            capabilities: &[RequiredCapability::Inspect],
            overlay: OverlayBehavior::ReadWorkspace,
            failure: FailureSemantics::NoBackendEffectOnFailure,
        }
    }

    fn inspect_region() -> AuthorizedEffectRegion {
        let mut kinds = BTreeSet::new();
        kinds.insert(ImpactKind::WorkspaceObservation);
        AuthorizedEffectRegion {
            kinds,
            read_paths: Vec::new(),
            observation_roots: vec![PathBuf::from("/project")],
            write_paths: Vec::new(),
            write_path_prefixes: Vec::new(),
            hosts: Vec::new(),
            session_state_allowed: false,
            command_allowed: false,
        }
    }

    fn read_region(pattern: &str) -> AuthorizedEffectRegion {
        let mut kinds = BTreeSet::new();
        kinds.insert(ImpactKind::WorkspaceObservation);
        AuthorizedEffectRegion {
            kinds,
            read_paths: vec![PathBuf::from(pattern)],
            observation_roots: Vec::new(),
            write_paths: Vec::new(),
            write_path_prefixes: Vec::new(),
            hosts: Vec::new(),
            session_state_allowed: false,
            command_allowed: false,
        }
    }

    fn empty_denied() -> BTreeSet<ImpactKind> {
        BTreeSet::new()
    }

    #[test]
    fn observation_within_region_returns_ok() {
        let observed = vec![ObservedEffect {
            kind: ImpactKind::WorkspaceObservation,
            scope: ObservedScope::Root("/project".to_string()),
            required_capability: RequiredCapability::Inspect,
            source: EffectSource::HandlerBackend,
        }];

        let result = check_effects_against_region_with_denies(
            "observe.workspace",
            &observed,
            &inspect_region(),
            &observation_decl(),
            &empty_denied(),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn denied_effect_kind_rejects_authorized_handler_effect() {
        let mut denied = empty_denied();
        denied.insert(ImpactKind::WorkspaceObservation);
        let observed = vec![ObservedEffect {
            kind: ImpactKind::WorkspaceObservation,
            scope: ObservedScope::Root("/project".to_string()),
            required_capability: RequiredCapability::Inspect,
            source: EffectSource::HandlerBackend,
        }];

        let result = check_effects_against_region_with_denies(
            "observe.workspace",
            &observed,
            &inspect_region(),
            &observation_decl(),
            &denied,
        );

        let err = result.expect_err("denied effect kind must hard fail");
        assert_eq!(err.code, CepErrorCode::EffectViolation);
        if let CepErrorData::Effect(data) = &err.data {
            assert_eq!(
                data.observed_effects,
                vec![ImpactKind::WorkspaceObservation]
            );
            assert!(
                data.violations
                    .iter()
                    .any(|violation| violation.contains("denied by effect-kind policy"))
            );
        } else {
            panic!("expected Effect error data");
        }
    }

    #[test]
    fn mutation_in_observation_method_returns_effect_violation() {
        let observed = vec![ObservedEffect {
            kind: ImpactKind::WorkspaceMutation,
            scope: ObservedScope::Path(PathBuf::from("/project/src/main.rs")),
            required_capability: RequiredCapability::WritePath,
            source: EffectSource::HandlerBackend,
        }];

        let result = check_effects_against_region_with_denies(
            "observe.workspace",
            &observed,
            &inspect_region(),
            &observation_decl(),
            &empty_denied(),
        );
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.code, CepErrorCode::EffectViolation);
        assert_eq!(err.retriability, Retriability::Permanent);

        if let CepErrorData::Effect(data) = &err.data {
            assert_eq!(data.method, "observe.workspace");
            assert!(!data.violations.is_empty());
        } else {
            panic!("expected Effect error data");
        }
    }

    #[test]
    fn probe_and_guard_effects_are_not_charged() {
        let mut denied = empty_denied();
        denied.insert(ImpactKind::WorkspaceMutation);
        denied.insert(ImpactKind::WorkspaceObservation);
        let observed = vec![
            ObservedEffect {
                kind: ImpactKind::WorkspaceMutation,
                scope: ObservedScope::Path(PathBuf::from("/project/tmp")),
                required_capability: RequiredCapability::WritePath,
                source: EffectSource::EffectProbe,
            },
            ObservedEffect {
                kind: ImpactKind::WorkspaceObservation,
                scope: ObservedScope::Path(PathBuf::from("/etc/shadow")),
                required_capability: RequiredCapability::ReadPath,
                source: EffectSource::GuardProbe,
            },
        ];

        let result = check_effects_against_region_with_denies(
            "observe.workspace",
            &observed,
            &inspect_region(),
            &observation_decl(),
            &denied,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn verify_effect_is_not_charged() {
        let mut denied = empty_denied();
        denied.insert(ImpactKind::CommandExecution);
        let observed = vec![ObservedEffect {
            kind: ImpactKind::CommandExecution,
            scope: ObservedScope::Root("/project".to_string()),
            required_capability: RequiredCapability::RunCommand,
            source: EffectSource::Verify,
        }];

        let result = check_effects_against_region_with_denies(
            "observe.workspace",
            &observed,
            &inspect_region(),
            &observation_decl(),
            &denied,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn empty_region_with_any_handler_effect_returns_violation() {
        let observed = vec![ObservedEffect {
            kind: ImpactKind::WorkspaceObservation,
            scope: ObservedScope::Path(PathBuf::from("/project/src/main.rs")),
            required_capability: RequiredCapability::ReadPath,
            source: EffectSource::HandlerBackend,
        }];

        let result = check_effects_against_region_with_denies(
            "observe.workspace",
            &observed,
            &AuthorizedEffectRegion::empty(),
            &observation_decl(),
            &empty_denied(),
        );
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.code, CepErrorCode::EffectViolation);
    }

    #[test]
    fn read_within_readpath_region_returns_ok() {
        let decl = EffectDecl {
            kinds: EffectKindSet::from_static(&[ImpactKind::WorkspaceObservation]),
            scope: EffectScopeKind::PathRead,
            capabilities: &[RequiredCapability::ReadPath],
            overlay: OverlayBehavior::ReadWorkspace,
            failure: FailureSemantics::NoBackendEffectOnFailure,
        };
        let observed = vec![ObservedEffect {
            kind: ImpactKind::WorkspaceObservation,
            scope: ObservedScope::Path(PathBuf::from("/project/src/lib.rs")),
            required_capability: RequiredCapability::ReadPath,
            source: EffectSource::HandlerBackend,
        }];

        let result = check_effects_against_region_with_denies(
            "observe.workspace",
            &observed,
            &read_region("/project/src"),
            &decl,
            &empty_denied(),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn read_outside_readpath_region_returns_violation() {
        let decl = EffectDecl {
            kinds: EffectKindSet::from_static(&[ImpactKind::WorkspaceObservation]),
            scope: EffectScopeKind::PathRead,
            capabilities: &[RequiredCapability::ReadPath],
            overlay: OverlayBehavior::ReadWorkspace,
            failure: FailureSemantics::NoBackendEffectOnFailure,
        };
        let observed = vec![ObservedEffect {
            kind: ImpactKind::WorkspaceObservation,
            scope: ObservedScope::Path(PathBuf::from("/etc/passwd")),
            required_capability: RequiredCapability::ReadPath,
            source: EffectSource::HandlerBackend,
        }];

        let result = check_effects_against_region_with_denies(
            "observe.workspace",
            &observed,
            &read_region("/project/src"),
            &decl,
            &empty_denied(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn mixed_charged_and_uncharged_only_charged_matters() {
        let observed = vec![
            ObservedEffect {
                kind: ImpactKind::WorkspaceMutation,
                scope: ObservedScope::Path(PathBuf::from("/project/tmp")),
                required_capability: RequiredCapability::WritePath,
                source: EffectSource::EffectProbe,
            },
            ObservedEffect {
                kind: ImpactKind::WorkspaceObservation,
                scope: ObservedScope::Root("/project".to_string()),
                required_capability: RequiredCapability::Inspect,
                source: EffectSource::HandlerBackend,
            },
        ];

        let result = check_effects_against_region_with_denies(
            "observe.workspace",
            &observed,
            &inspect_region(),
            &observation_decl(),
            &empty_denied(),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn violation_populates_all_effect_error_data_fields() {
        let observed = vec![ObservedEffect {
            kind: ImpactKind::WorkspaceMutation,
            scope: ObservedScope::Path(PathBuf::from("/project/src/main.rs")),
            required_capability: RequiredCapability::WritePath,
            source: EffectSource::HandlerBackend,
        }];

        let result = check_effects_against_region_with_denies(
            "observe.workspace",
            &observed,
            &inspect_region(),
            &observation_decl(),
            &empty_denied(),
        );
        let err = result.unwrap_err();

        if let CepErrorData::Effect(data) = &err.data {
            assert_eq!(data.method, "observe.workspace");
            assert!(
                data.declared_effect
                    .contains(&ImpactKind::WorkspaceObservation)
            );
            assert!(!data.observed_effects.is_empty());
            assert!(!data.violations.is_empty());
        } else {
            panic!("expected Effect error data, got {:?}", err.data);
        }
    }
}

//! Effect checker — decides whether observed effects are within the authorized region.
//!
//! The check is `observed ⊆ authorized_effect_region`, NOT
//! `observed ⊆ static EffectDecl`. The checker is a pure decision module
//! (structural analog: guard_l1.rs) — no I/O, no mutation, no cep_mode logic.
//! Callers apply enforce/warn/off gating to the verdict.

use lancet_protocol::envelope::{EffectDecl, ImpactKind};
use lancet_protocol::error::{CepErrorData, EffectErrorData};
use lancet_protocol::policy::SourcedCapability;
use lancet_protocol::{CepError, CepErrorCode, Retriability};

use super::ledger::{EffectSource, ObservedEffect};
use super::region::AuthorizedEffectRegion;
use crate::policy::CepPolicyConfig;

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

    Err(CepError::new(
        CepErrorCode::EffectViolation,
        format!(
            "{}: observed effect exceeds authorized region ({} violation{})",
            method,
            violations.len(),
            if violations.len() == 1 { "" } else { "s" }
        ),
        CepErrorData::Effect(Box::new(EffectErrorData {
            method: method.to_string(),
            declared_effect: effect_decl.kinds.0.to_vec(),
            authorized_effect: authorized_kinds,
            observed_effects: charged_kinds,
            violations,
        })),
        Retriability::Permanent,
    ))
}

/// Check whether observed effects are within the authorized effect region,
/// computing the region from the matched capability and policy config.
///
/// Convenience wrapper around [`check_effects_against_region`] for callers
/// that hold a matched capability rather than a pre-built region.
pub fn check_effects(
    method: &str,
    observed: &[ObservedEffect],
    matched_cap: Option<&SourcedCapability>,
    policy: &CepPolicyConfig,
    effect_decl: &EffectDecl,
) -> Result<(), CepError> {
    let authorized = AuthorizedEffectRegion::from_matched_capability(matched_cap, policy);
    check_effects_against_region(method, observed, &authorized, effect_decl)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect::ledger::{EffectSource, ObservedScope};
    use lancet_catalog::method::RequiredCapability;
    use lancet_protocol::envelope::{Capability, EffectKindSet, EffectScopeKind};
    use lancet_protocol::policy::{CapabilitySource, SourcedCapability};
    use std::path::PathBuf;

    fn observation_decl() -> EffectDecl {
        EffectDecl {
            kinds: EffectKindSet::from_static(&[ImpactKind::WorkspaceObservation]),
            scope: EffectScopeKind::ObservationRoot,
            capabilities: &[RequiredCapability::Inspect],
        }
    }

    fn inspect_cap() -> SourcedCapability {
        SourcedCapability {
            capability: Capability::Inspect,
            source: CapabilitySource::Base,
        }
    }

    fn read_cap(pattern: &str) -> SourcedCapability {
        SourcedCapability {
            capability: Capability::ReadPath {
                path_pattern: pattern.to_string(),
            },
            source: CapabilitySource::Base,
        }
    }

    #[test]
    fn observation_within_region_returns_ok() {
        let policy = crate::effect::shared_test_policy();
        let cap = inspect_cap();
        let observed = vec![ObservedEffect {
            kind: ImpactKind::WorkspaceObservation,
            scope: ObservedScope::Root("/project".to_string()),
            required_capability: RequiredCapability::Inspect,
            source: EffectSource::HandlerBackend,
        }];

        let result = check_effects(
            "workspace.grep",
            &observed,
            Some(&cap),
            &policy,
            &observation_decl(),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn mutation_in_observation_method_returns_effect_violation() {
        let policy = crate::effect::shared_test_policy();
        let cap = inspect_cap();
        let observed = vec![ObservedEffect {
            kind: ImpactKind::WorkspaceMutation,
            scope: ObservedScope::Path(PathBuf::from("/project/src/main.rs")),
            required_capability: RequiredCapability::WritePath,
            source: EffectSource::HandlerBackend,
        }];

        let result = check_effects(
            "workspace.grep",
            &observed,
            Some(&cap),
            &policy,
            &observation_decl(),
        );
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.code, CepErrorCode::EffectViolation);
        assert_eq!(err.retriability, Retriability::Permanent);

        if let CepErrorData::Effect(data) = &err.data {
            assert_eq!(data.method, "workspace.grep");
            assert!(!data.violations.is_empty());
        } else {
            panic!("expected Effect error data");
        }
    }

    #[test]
    fn probe_and_guard_effects_are_not_charged() {
        let policy = crate::effect::shared_test_policy();
        let cap = inspect_cap();
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

        let result = check_effects(
            "workspace.grep",
            &observed,
            Some(&cap),
            &policy,
            &observation_decl(),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn verify_effect_is_not_charged() {
        let policy = crate::effect::shared_test_policy();
        let cap = inspect_cap();
        let observed = vec![ObservedEffect {
            kind: ImpactKind::CommandExecution,
            scope: ObservedScope::Root("/project".to_string()),
            required_capability: RequiredCapability::RunCommand,
            source: EffectSource::Verify,
        }];

        let result = check_effects(
            "workspace.grep",
            &observed,
            Some(&cap),
            &policy,
            &observation_decl(),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn empty_region_with_any_handler_effect_returns_violation() {
        let policy = crate::effect::shared_test_policy();
        let observed = vec![ObservedEffect {
            kind: ImpactKind::WorkspaceObservation,
            scope: ObservedScope::Path(PathBuf::from("/project/src/main.rs")),
            required_capability: RequiredCapability::ReadPath,
            source: EffectSource::HandlerBackend,
        }];

        let result = check_effects(
            "workspace.read_file",
            &observed,
            None,
            &policy,
            &observation_decl(),
        );
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.code, CepErrorCode::EffectViolation);
    }

    #[test]
    fn read_within_readpath_region_returns_ok() {
        let policy = crate::effect::shared_test_policy();
        let cap = read_cap("/project/src");
        let decl = EffectDecl {
            kinds: EffectKindSet::from_static(&[ImpactKind::WorkspaceObservation]),
            scope: EffectScopeKind::PathRead,
            capabilities: &[RequiredCapability::ReadPath],
        };
        let observed = vec![ObservedEffect {
            kind: ImpactKind::WorkspaceObservation,
            scope: ObservedScope::Path(PathBuf::from("/project/src/lib.rs")),
            required_capability: RequiredCapability::ReadPath,
            source: EffectSource::HandlerBackend,
        }];

        let result = check_effects("workspace.read_file", &observed, Some(&cap), &policy, &decl);
        assert!(result.is_ok());
    }

    #[test]
    fn read_outside_readpath_region_returns_violation() {
        let policy = crate::effect::shared_test_policy();
        let cap = read_cap("/project/src");
        let decl = EffectDecl {
            kinds: EffectKindSet::from_static(&[ImpactKind::WorkspaceObservation]),
            scope: EffectScopeKind::PathRead,
            capabilities: &[RequiredCapability::ReadPath],
        };
        let observed = vec![ObservedEffect {
            kind: ImpactKind::WorkspaceObservation,
            scope: ObservedScope::Path(PathBuf::from("/etc/passwd")),
            required_capability: RequiredCapability::ReadPath,
            source: EffectSource::HandlerBackend,
        }];

        let result = check_effects("workspace.read_file", &observed, Some(&cap), &policy, &decl);
        assert!(result.is_err());
    }

    #[test]
    fn mixed_charged_and_uncharged_only_charged_matters() {
        let policy = crate::effect::shared_test_policy();
        let cap = inspect_cap();
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

        let result = check_effects(
            "workspace.grep",
            &observed,
            Some(&cap),
            &policy,
            &observation_decl(),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn violation_populates_all_effect_error_data_fields() {
        let policy = crate::effect::shared_test_policy();
        let cap = inspect_cap();
        let observed = vec![ObservedEffect {
            kind: ImpactKind::WorkspaceMutation,
            scope: ObservedScope::Path(PathBuf::from("/project/src/main.rs")),
            required_capability: RequiredCapability::WritePath,
            source: EffectSource::HandlerBackend,
        }];

        let result = check_effects(
            "workspace.grep",
            &observed,
            Some(&cap),
            &policy,
            &observation_decl(),
        );
        let err = result.unwrap_err();

        if let CepErrorData::Effect(data) = &err.data {
            assert_eq!(data.method, "workspace.grep");
            assert!(data
                .declared_effect
                .contains(&ImpactKind::WorkspaceObservation));
            assert!(!data.observed_effects.is_empty());
            assert!(!data.violations.is_empty());
        } else {
            panic!("expected Effect error data, got {:?}", err.data);
        }
    }
}

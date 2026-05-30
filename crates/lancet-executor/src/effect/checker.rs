//! Effect checker — decides whether observed effects are within the authorized region.
//!
//! The check is `observed ⊆ authorized_effect_region` (D-03), NOT
//! `observed ⊆ static EffectDecl`. The checker is a pure decision module
//! (structural analog: guard.rs) — no I/O, no mutation, no cep_mode logic.
//! Plan 06 applies enforce/warn gating on the verdict.

use lancet_protocol::envelope::{EffectDecl, ImpactKind};
use lancet_protocol::error::{CepErrorData, EffectErrorData};
use lancet_protocol::policy::SourcedCapability;
use lancet_protocol::{CepError, CepErrorCode, Retriability};

use super::ledger::{EffectSource, ObservedEffect};
use super::region::AuthorizedEffectRegion;
use crate::policy::Phase21PolicyConfig;

/// Check whether observed effects are within the authorized effect region.
///
/// Returns `Ok(())` when `observed ⊆ authorized`, or `Err(CepError)` with
/// `EffectViolation` (-32050) and full `EffectErrorData` when any charged
/// observed effect exceeds the authorized region.
///
/// Source filtering per D-11:
/// - `HandlerBackend` — charged against the contract
/// - `EffectProbe` / `GuardProbe` — skipped entirely
/// - `Verify` — audited but not charged (envelope-internal constraint)
///
/// Does NOT perform cep_mode enforce/warn decision — returns the raw verdict.
/// Does NOT mutate the ledger.
pub fn check_effects(
    method: &str,
    observed: &[ObservedEffect],
    matched_cap: Option<&SourcedCapability>,
    policy: &Phase21PolicyConfig,
    effect_decl: &EffectDecl,
) -> Result<(), CepError> {
    let authorized = AuthorizedEffectRegion::from_matched_capability(matched_cap, policy);

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

    let authorized_kinds: Vec<ImpactKind> = authorized.kinds.into_iter().collect();

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
            declared_effect: effect_decl.kinds,
            authorized_effect: authorized_kinds,
            observed_effects: charged_kinds,
            violations,
        })),
        Retriability::Permanent,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect::ledger::{EffectSource, ObservedScope};
    use crate::policy::command::CommandPolicy;
    use crate::policy::network::NetworkPolicyConfig;
    use crate::policy::observation::ObservationPolicyConfig;
    use crate::policy::BudgetLimits;
    use lancet_protocol::envelope::{Capability, EffectKindSet, EffectScopeKind};
    use lancet_protocol::method::RequiredCapability;
    use lancet_protocol::policy::{CapabilitySource, SourcedCapability};
    use std::path::PathBuf;

    fn test_policy() -> Phase21PolicyConfig {
        Phase21PolicyConfig {
            project_root: PathBuf::from("/project"),
            observation: ObservationPolicyConfig {
                allowed_roots: vec![PathBuf::from("/project")],
                max_single_read_bytes: 10_000_000,
                max_turn_observation_bytes: 10_000_000,
                max_grep_matches: 1000,
                max_list_entries: 1000,
                max_find_paths: 1000,
                max_todo_ops: 100,
            },
            network: NetworkPolicyConfig::safe_defaults(),
            budget: BudgetLimits {
                max_single_read_bytes: 10_000_000,
                max_turn_observation_bytes: 10_000_000,
                max_grep_matches: 1000,
                max_list_entries: 1000,
                max_find_paths: 1000,
                max_todo_ops: 100,
            },
            command: CommandPolicy::defaults(),
        }
    }

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
        let policy = test_policy();
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
        let policy = test_policy();
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
        let policy = test_policy();
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
        let policy = test_policy();
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
        let policy = test_policy();
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
        let policy = test_policy();
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
        let policy = test_policy();
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
        let policy = test_policy();
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
        let policy = test_policy();
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

// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::helpers::is_incomplete_dispatch;
use lancet_protocol::{CepError, CepErrorCode, CepErrorData, Retriability, policy::CepMode};
use serde_json::Value;

pub(super) fn apply_effect_check(
    method: &str,
    dispatch_result: Result<Value, CepError>,
    observed_effects: &[lancet_authority::effect::ledger::ObservedEffect],
    authorized_region_slot: &lancet_authority::effect::ledger::AuthorizedRegionSlot,
    cep_mode: CepMode,
    denied_effect_kinds: &std::collections::BTreeSet<lancet_protocol::effect_types::ImpactKind>,
) -> Result<Value, CepError> {
    if is_incomplete_dispatch(&dispatch_result) {
        return dispatch_result;
    }
    let Ok(dispatch_value) = dispatch_result else {
        return dispatch_result;
    };
    let Some(effect_decl) = lancet_catalog::catalog()
        .iter()
        .find(|descriptor| descriptor.name == method)
        .map(|descriptor| &descriptor.effect)
    else {
        return Ok(dispatch_value);
    };
    lancet_authority::effect::checker::check_denied_effect_kinds(
        method,
        observed_effects,
        effect_decl,
        denied_effect_kinds,
    )?;
    if cep_mode == CepMode::Off {
        return Ok(dispatch_value);
    }
    match authorized_region_slot.get() {
        Some(ref authorized) => {
            match lancet_authority::effect::checker::check_effects_against_region(
                method,
                observed_effects,
                authorized,
                effect_decl,
            ) {
                Ok(()) => Ok(dispatch_value),
                Err(violation_err) => match cep_mode {
                    CepMode::Enforce => Err(violation_err),
                    CepMode::Warn => Ok(dispatch_value),
                    CepMode::Off => unreachable!("Off early-returned above"),
                },
            }
        }
        None => match cep_mode {
            CepMode::Enforce => Err(CepError::new(
                CepErrorCode::EffectViolation,
                format!("authorized region missing after successful dispatch for {method}"),
                CepErrorData::Effect(Box::new(lancet_protocol::error::EffectErrorData {
                    method: method.to_string(),
                    declared_effect: effect_decl.kinds.0.to_vec(),
                    authorized_effect: vec![],
                    observed_effects: observed_effects
                        .iter()
                        .map(|effect| effect.kind.clone())
                        .collect(),
                    violations: vec![
                        "handler completed without setting authorized region".to_string(),
                    ],
                    cell_ref: None,
                    capability: None,
                })),
                Retriability::Permanent,
            )),
            CepMode::Warn => {
                tracing::warn!(
                    method = %method,
                    "authorized region missing after successful dispatch; warn mode"
                );
                Ok(dispatch_value)
            }
            CepMode::Off => unreachable!("Off early-returned above"),
        },
    }
}

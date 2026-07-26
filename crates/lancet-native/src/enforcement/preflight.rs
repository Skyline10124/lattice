// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Compact preflight and policy evidence metadata for native handlers.

use lancet_authority::effect::region::AuthorizedEffectRegion;
use lancet_authority::guard::PolicyEvaluation;

use lancet_protocol::policy::{PolicyDecision, PolicyInput, PolicyReasonCode};
use serde_json::{Value, json};
use std::fmt::Debug;
use std::path::Path;

/// Build durable policy metadata shared by all native handlers.
pub(crate) fn policy_metadata(
    method: &str,
    evaluation: &PolicyEvaluation,
    authorized_region: Option<&AuthorizedEffectRegion>,
) -> Value {
    let evidence = evaluation.evidence.as_ref();

    let mut metadata = json!({
        "cep_policy": {
            "method": method,
            "decision": policy_decision_label(&evaluation.decision),
            "budget_remaining": evidence.and_then(|e| e.budget_remaining),
            "network_host": evidence.and_then(|e| e.network_host.as_deref()),
            "resolved_paths": evidence.map(|e| e.resolved_paths.clone()).unwrap_or_default(),
            "declared_effect": declared_effect_metadata(method),
            "authorized_region": authorized_region.map(authorized_region_metadata),
        }
    });

    if let Some((reason_code, message, action_required)) =
        policy_decision_detail(&evaluation.decision)
    {
        metadata["cep_policy"]["reason_code"] = json!(reason_code);
        metadata["cep_policy"]["message"] = json!(message);
        metadata["cep_policy"]["action_required"] = json!(action_required);
    }

    metadata
}

/// Build approval-ready metadata for a human-review policy decision.
pub(crate) fn approval_request_metadata(
    method: &str,
    _input: &PolicyInput,
    evaluation: &PolicyEvaluation,
) -> Option<Value> {
    let PolicyDecision::HumanReview {
        reason_code,
        message,
    } = &evaluation.decision
    else {
        return None;
    };

    let capability = required_capability_for_method(method);

    Some(json!({
        "method": method,
        "status": "required",
        "reason_code": debug_label(reason_code),
        "message": message,
        "required_capability": capability,
        "retry_hint": "obtain_human_approval_or_replan",
    }))
}

/// Build method preflight metadata for policy denials that happen before a handler
/// can run its own scope preflight.
pub(crate) fn policy_denial_preflight_metadata(
    method: &str,
    input: &PolicyInput,
    evaluation: &PolicyEvaluation,
) -> Option<Value> {
    let PolicyDecision::Denied { reason_code, .. } = &evaluation.decision else {
        return None;
    };
    if !matches!(
        reason_code,
        PolicyReasonCode::PathOutsideScope | PolicyReasonCode::ReplanRequired
    ) {
        return None;
    }
    if !matches!(method, "mutate.create" | "mutate.edit") {
        return None;
    }

    let paths = mutation_target_paths(&input.params);
    Some(json!({
        "cep_preflight": {
            "method": method,
            "kind": "workspace_mutation",
            "effective_effect": ["workspace_mutation"],
            "changed_paths": paths,
            "write_patterns": [],
            "out_of_scope_paths": paths,
            "local_revision": null,
            "replan_reason": "out_of_scope_paths",
        },
        "changed_paths": paths,
        "out_of_scope_paths": paths,
        "replan_reason": "out_of_scope_paths",
        "policy_enforced": true,
    }))
}

/// Build mutation-specific preflight metadata after scope checks.
///
/// Method-local evidence only: the guarded target paths, the Local Revision
/// observed at the Mutation Precondition check (when relevant), and the
/// replan reason. No routing invocation flags.
pub(crate) fn mutation_preflight_metadata(input: MutationPreflight<'_>) -> Value {
    json!({
        "cep_preflight": {
            "method": input.method,
            "kind": "workspace_mutation",
            "effective_effect": ["workspace_mutation"],
            "changed_paths": input.changed_paths,
            "write_patterns": input.write_patterns,
            "out_of_scope_paths": input.out_of_scope_paths,
            "local_revision": input.local_revision,
            "replan_reason": input.replan_reason,
        },
        "changed_paths": input.changed_paths,
        "out_of_scope_paths": input.out_of_scope_paths,
        "local_revision": input.local_revision,
        "replan_reason": input.replan_reason,
        "policy_enforced": true,
    })
}

pub(crate) struct MutationPreflight<'a> {
    pub(crate) method: &'a str,
    pub(crate) changed_paths: &'a [String],
    pub(crate) write_patterns: &'a [String],
    pub(crate) out_of_scope_paths: &'a [String],
    /// Local Revision observed at the Mutation Precondition check (the
    /// current content hash of the target), present when the precondition
    /// failed on a stale expected hash.
    pub(crate) local_revision: Option<&'a str>,
    pub(crate) replan_reason: Option<&'a str>,
}

pub(crate) fn authorized_region_metadata(region: &AuthorizedEffectRegion) -> Value {
    json!({
        "kinds": region
            .kinds
            .iter()
            .map(crate::audit::impact_kind_label)
            .collect::<Vec<_>>(),
        "read_paths": path_list(&region.read_paths),
        "observation_roots": path_list(&region.observation_roots),
        "denied_write_paths": path_list(&region.denied_write_paths),
        "denied_write_path_prefixes": path_list(&region.denied_write_path_prefixes),
        "hosts": region.hosts,
        "session_state_allowed": region.session_state_allowed,
        "command_allowed": region.command_allowed,
    })
}

fn declared_effect_metadata(method: &str) -> Option<Value> {
    lancet_catalog::catalog()
        .iter()
        .find(|descriptor| descriptor.name == method)
        .map(|descriptor| {
            json!({
                "impact": crate::audit::impact_kind_label(&descriptor.impact),
                "kinds": descriptor
                    .effect
                    .kinds
                    .0
                    .iter()
                    .map(crate::audit::impact_kind_label)
                    .collect::<Vec<_>>(),
                "scope": debug_label(&descriptor.effect.scope),
                "capabilities": descriptor
                    .effect
                    .capabilities
                    .iter()
                    .map(|capability| crate::audit::required_capability_label(*capability))
                    .collect::<Vec<_>>(),
                "overlay": debug_label(&descriptor.effect.overlay),
                "failure": debug_label(&descriptor.effect.failure),
            })
        })
}

fn required_capability_for_method(method: &str) -> Option<&'static str> {
    lancet_catalog::catalog()
        .iter()
        .find(|descriptor| descriptor.name == method)
        .map(|descriptor| crate::audit::required_capability_label(descriptor.capability))
}

fn mutation_target_paths(params: &Value) -> Vec<String> {
    if let Some(path) = params.get("path").and_then(Value::as_str) {
        return vec![path.to_string()];
    }

    params
        .get("operations")
        .and_then(Value::as_array)
        .map(|operations| {
            operations
                .iter()
                .filter_map(|operation| operation.get("path").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn policy_decision_label(decision: &PolicyDecision) -> &'static str {
    match decision {
        PolicyDecision::Allowed => "allowed",
        PolicyDecision::Denied { .. } => "denied",
        PolicyDecision::HumanReview { .. } => "human_review",
    }
}

fn policy_decision_detail(decision: &PolicyDecision) -> Option<(String, String, Option<String>)> {
    match decision {
        PolicyDecision::Allowed => None,
        PolicyDecision::Denied {
            reason_code,
            message,
            action_required,
        } => Some((
            debug_label(reason_code),
            message.clone(),
            action_required.as_ref().map(debug_label),
        )),
        PolicyDecision::HumanReview {
            reason_code,
            message,
        } => Some((
            debug_label(reason_code),
            message.clone(),
            Some("ask_human".into()),
        )),
    }
}

fn path_list(paths: &[std::path::PathBuf]) -> Vec<String> {
    paths
        .iter()
        .map(|path| path.to_string_lossy().to_string())
        .collect()
}

fn debug_label(value: &impl Debug) -> String {
    camel_to_snake(&format!("{value:?}"))
}

fn camel_to_snake(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for (index, ch) in value.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if index > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

#[allow(dead_code)]
fn _assert_path_ref(_path: &Path) {}

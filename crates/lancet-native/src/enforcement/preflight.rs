// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Compact preflight and policy evidence metadata for native handlers.

use lancet_authority::effect::region::AuthorizedEffectRegion;
use lancet_authority::guard::PolicyEvaluation;
use lancet_authority::policy::command::EffectiveImpact;
use lancet_protocol::effect_types::ImpactKind;

use lancet_protocol::policy::Capability;
use lancet_protocol::policy::{
    CapabilitySource, PolicyAuthorityInput, PolicyDecision, PolicyInput, PolicyReasonCode,
    SourcedCapability,
};
use serde_json::{Value, json};
use std::fmt::Debug;
use std::path::Path;

fn declared_envelope_id(input: &PolicyInput) -> Option<String> {
    match &input.authority {
        PolicyAuthorityInput::Envelope { envelope_snapshot } => {
            envelope_snapshot.declared_envelope_id.clone()
        }
        PolicyAuthorityInput::Direct => None,
    }
}

/// Build durable policy metadata shared by all native handlers.
pub(crate) fn policy_metadata(
    method: &str,
    evaluation: &PolicyEvaluation,
    authorized_region: Option<&AuthorizedEffectRegion>,
) -> Value {
    let evidence = evaluation.evidence.as_ref();
    let capability_match = evidence.and_then(|e| e.capability_match.as_ref());

    let mut metadata = json!({
        "cep_policy": {
            "method": method,
            "decision": policy_decision_label(&evaluation.decision),
            "capability_match": capability_match.is_some(),
            "capability_match_detail": capability_match.map(capability_match_metadata),
            "budget_remaining": evidence.and_then(|e| e.budget_remaining),
            "network_host": evidence.and_then(|e| e.network_host.as_deref()),
            "resolved_paths": evidence.map(|e| e.resolved_paths.clone()).unwrap_or_default(),
            "command_classification": evidence
                .and_then(|e| e.command_classification.as_ref())
                .map(effective_impact_label),
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
    input: &PolicyInput,
    evaluation: &PolicyEvaluation,
) -> Option<Value> {
    let PolicyDecision::HumanReview {
        reason_code,
        message,
    } = &evaluation.decision
    else {
        return None;
    };

    let capability = evaluation
        .evidence
        .as_ref()
        .and_then(|e| e.capability_match.as_ref())
        .map(|m| crate::audit::required_capability_label(m.required))
        .or_else(|| required_capability_for_method(method));

    Some(json!({
        "method": method,
        "status": "required",
        "reason_code": debug_label(reason_code),
        "message": message,
        "required_capability": capability,
        "active_envelope_id": declared_envelope_id(input),
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
    if !matches!(method, "mutate.create" | "mutate.edit" | "mutate.batch") {
        return None;
    }

    let paths = mutation_target_paths(&input.params);
    Some(json!({
        "cep_preflight": {
            "method": method,
            "kind": "workspace_mutation",
            "effective_effect": ["workspace_mutation"],
            "requires_active_envelope": true,
            "active_envelope_id": declared_envelope_id(input),
            "changed_paths": paths,
            "write_patterns": [],
            "out_of_scope_paths": paths,
            "expected_hash_status": null,
            "verify_configured": null,
            "replan_reason": "out_of_scope_paths",
            "backend_invoked": false,
        },
        "changed_paths": paths,
        "out_of_scope_paths": paths,
        "replan_reason": "out_of_scope_paths",
        "backend_invoked": false,
        "policy_enforced": true,
    }))
}

/// Build command-specific preflight metadata after effective classification.
pub(crate) fn command_preflight_metadata(input: CommandPreflight<'_>) -> Value {
    json!({
        "cep_preflight": {
            "method": input.method,
            "kind": "command",
            "declared_impact": input.declared_impact.map(crate::audit::impact_kind_label),
            "effective_impact": effective_impact_label(input.effective_impact),
            "effective_effect": command_effective_effect_labels(input.effective_impact),
            "argv0": input.argv.first().map(String::as_str),
            "argv_len": input.argv.len(),
            "requires_active_envelope": matches!(
                input.effective_impact,
                EffectiveImpact::WorkspaceMutation
            ),
            "active_envelope_id": input.envelope_id,
            "run_command_capability_present": input.run_command_capability_present,
            "argv_allowlist_matched": input.argv_allowlist_matched,
            "write_region": input.authorized_region.map(authorized_region_metadata),
            "backend_invoked": input.backend_invoked,
        },
        "declared_impact": input.declared_impact.map(crate::audit::impact_kind_label),
        "classification": effective_impact_label(input.effective_impact),
        "effective_effect": command_effective_effect_labels(input.effective_impact),
        "envelope_id": input.envelope_id,
        "backend_invoked": input.backend_invoked,
        "policy_enforced": true,
    })
}

pub(crate) struct CommandPreflight<'a> {
    pub(crate) method: &'a str,
    pub(crate) argv: &'a [String],
    pub(crate) declared_impact: Option<&'a ImpactKind>,
    pub(crate) effective_impact: &'a EffectiveImpact,
    pub(crate) envelope_id: Option<&'a str>,
    pub(crate) run_command_capability_present: bool,
    pub(crate) argv_allowlist_matched: bool,
    pub(crate) authorized_region: Option<&'a AuthorizedEffectRegion>,
    pub(crate) backend_invoked: bool,
}

/// Build mutation-specific preflight metadata after envelope and scope checks.
pub(crate) fn mutation_preflight_metadata(input: MutationPreflight<'_>) -> Value {
    json!({
        "cep_preflight": {
            "method": input.method,
            "kind": "workspace_mutation",
            "effective_effect": ["workspace_mutation"],
            "requires_active_envelope": true,
            "active_envelope_id": input.envelope_id,
            "changed_paths": input.changed_paths,
            "write_patterns": input.write_patterns,
            "out_of_scope_paths": input.out_of_scope_paths,
            "expected_hash_status": input.expected_hash_status,
            "verify_configured": input.verify_configured,
            "replan_reason": input.replan_reason,
            "backend_invoked": input.backend_invoked,
        },
        "changed_paths": input.changed_paths,
        "out_of_scope_paths": input.out_of_scope_paths,
        "expected_hash_status": input.expected_hash_status,
        "verify_configured": input.verify_configured,
        "replan_reason": input.replan_reason,
        "envelope_id": input.envelope_id,
        "backend_invoked": input.backend_invoked,
        "policy_enforced": true,
    })
}

pub(crate) struct MutationPreflight<'a> {
    pub(crate) method: &'a str,
    pub(crate) envelope_id: &'a str,
    pub(crate) changed_paths: &'a [String],
    pub(crate) write_patterns: &'a [String],
    pub(crate) out_of_scope_paths: &'a [String],
    pub(crate) expected_hash_status: Option<&'a str>,
    pub(crate) verify_configured: bool,
    pub(crate) replan_reason: Option<&'a str>,
    pub(crate) backend_invoked: bool,
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
        "write_paths": path_list(&region.write_paths),
        "write_path_prefixes": path_list(&region.write_path_prefixes),
        "hosts": region.hosts,
        "session_state_allowed": region.session_state_allowed,
        "command_allowed": region.command_allowed,
    })
}

fn capability_match_metadata(
    capability_match: &lancet_authority::policy::capability::CapabilityMatch,
) -> Value {
    json!({
        "required": crate::audit::required_capability_label(capability_match.required),
        "verdict": debug_label(&capability_match.verdict),
        "source_allowed": capability_match.source_allowed,
        "matched": capability_match
            .matched
            .as_ref()
            .map(sourced_capability_metadata),
        "failure_reason": capability_match
            .failure_reason
            .as_ref()
            .map(debug_label),
        "action_required": capability_match
            .action_required
            .as_ref()
            .map(debug_label),
    })
}

fn sourced_capability_metadata(capability: &SourcedCapability) -> Value {
    json!({
        "kind": capability_kind_label(&capability.capability),
        "source": capability_source_label(&capability.source),
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

fn command_effective_effect_labels(effective_impact: &EffectiveImpact) -> Vec<&'static str> {
    match effective_impact {
        EffectiveImpact::Diagnostic => vec!["command_execution"],
        EffectiveImpact::WorkspaceMutation => vec!["command_execution", "workspace_mutation"],
        EffectiveImpact::HardDenied => Vec::new(),
    }
}

fn effective_impact_label(effective_impact: &EffectiveImpact) -> &'static str {
    match effective_impact {
        EffectiveImpact::Diagnostic => "diagnostic",
        EffectiveImpact::WorkspaceMutation => "workspace_mutation",
        EffectiveImpact::HardDenied => "hard_denied",
    }
}

fn capability_kind_label(capability: &Capability) -> &'static str {
    match capability {
        Capability::ReadPath { .. } => "read_path",
        Capability::WritePath { .. } => "write_path",
        Capability::RunCommand { .. } => "run_command",
        Capability::FetchUrl { .. } => "fetch_url",
        Capability::MutateSessionState => "mutate_session_state",
        Capability::Inspect => "inspect",
        Capability::ReadAudit => "read_audit",
    }
}

fn capability_source_label(source: &CapabilitySource) -> &'static str {
    match source {
        CapabilitySource::Base => "base",
        CapabilitySource::Declared { .. } => "declared",
        CapabilitySource::Both { .. } => "both",
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

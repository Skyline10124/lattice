// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Native policy projections that remain observable outside the policy engine.

use lancet_authority::guard::PolicyEvaluation;
use lancet_catalog::RequiredCapability;
use lancet_protocol::policy::{PolicyDecision, PolicyInput};
use serde_json::{Value, json};

/// Build the content-free operator approval request emitted for HumanReview.
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

    Some(json!({
        "method": method,
        "status": "required",
        "reason_code": camel_to_snake(&format!("{reason_code:?}")),
        "message": message,
        "required_capability": required_capability_for_method(method),
        "retry_hint": "obtain_human_approval_or_replan",
    }))
}

fn required_capability_for_method(method: &str) -> Option<&'static str> {
    lancet_catalog::catalog()
        .iter()
        .find(|descriptor| descriptor.name == method)
        .map(|descriptor| required_capability_label(descriptor.capability))
}

fn required_capability_label(capability: RequiredCapability) -> &'static str {
    match capability {
        RequiredCapability::Inspect => "inspect",
        RequiredCapability::ReadPath => "read_path",
        RequiredCapability::WritePath => "write_path",
        RequiredCapability::RunCommand => "run_command",
        RequiredCapability::FetchUrl => "fetch_url",
        RequiredCapability::MutateSessionState => "mutate_session_state",
        RequiredCapability::ReadAudit => "read_audit",
    }
}

fn camel_to_snake(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for (index, ch) in value.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if index > 0 {
                output.push('_');
            }
            output.push(ch.to_ascii_lowercase());
        } else {
            output.push(ch);
        }
    }
    output
}

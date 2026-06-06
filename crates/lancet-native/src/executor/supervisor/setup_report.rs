// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Setup report encoding for supervised command handoff.

use std::path::PathBuf;

use lancet_authority::{
    LandlockApplied, MinimalMountViewApplied, MountNamespaceApplied, NetnsApplied,
};
use serde_json::Value;

use crate::enforcement::isolation::{
    AppliedBoundaryEvidence, BoundaryAttemptOutcome, CommandIsolationAttempt,
    LandlockApplicationStatus, MinimalMountViewStatus, SupervisorSetupStatus,
};

pub(crate) fn encode_setup_attempt_report(attempt: &CommandIsolationAttempt) -> String {
    let outcome = &attempt.outcome;
    let report = serde_json::json!({
        "supervisor_backend": outcome.supervisor_backend,
        "project_view_path": outcome.project_view_path.as_ref().map(|path| path.display().to_string()),
        "cwd_view_path": outcome.cwd_view_path.as_ref().map(|path| path.display().to_string()),
        "privilege_strategy": outcome.privilege_strategy,
        "userns_status": outcome.userns_status,
        "setgroups_status": outcome.setgroups_status,
        "uid_map_status": outcome.uid_map_status,
        "gid_map_status": outcome.gid_map_status,
        "read_only_bind_scope": outcome.read_only_bind_scope,
        "network_namespace_status": outcome.network_namespace_status.label(),
        "mount_namespace_status": outcome.mount_namespace_status.label(),
        "minimal_mount_view_status": outcome.minimal_mount_view_status.label(),
        "landlock_status": outcome.landlock_status.label(),
        "landlock_abi_version": outcome.landlock_abi_version,
        "landlock_rights_mask": outcome.landlock_rights_mask,
        "landlock_ruleset_status": outcome.landlock_ruleset_status,
        "landlock_required": outcome.landlock_required,
        "fd_cleanup_status": outcome.fd_cleanup_status,
        "fd_cleanup_method": outcome.fd_cleanup_method,
        "env_policy": outcome.env_policy,
        "setup_failure_stage": outcome.setup_failure_stage,
        "setup_failure_reason": outcome.setup_failure_reason,
        "failed_closed": outcome.failed_closed,
    });
    format!("{report}\n")
}

pub(crate) fn decode_setup_attempt_report(bytes: &[u8]) -> Option<CommandIsolationAttempt> {
    let value: Value = serde_json::from_slice(bytes).ok()?;
    let mut outcome = BoundaryAttemptOutcome {
        supervisor_backend: string_field(&value, "supervisor_backend"),
        project_view_path: string_field(&value, "project_view_path").map(PathBuf::from),
        cwd_view_path: string_field(&value, "cwd_view_path").map(PathBuf::from),
        privilege_strategy: string_field(&value, "privilege_strategy"),
        userns_status: string_field(&value, "userns_status"),
        setgroups_status: string_field(&value, "setgroups_status"),
        uid_map_status: string_field(&value, "uid_map_status"),
        gid_map_status: string_field(&value, "gid_map_status"),
        read_only_bind_scope: string_field(&value, "read_only_bind_scope"),
        network_namespace_status: setup_status_field(&value, "network_namespace_status"),
        mount_namespace_status: setup_status_field(&value, "mount_namespace_status"),
        minimal_mount_view_status: mount_view_status_field(&value, "minimal_mount_view_status"),
        landlock_status: landlock_status_field(&value, "landlock_status"),
        landlock_abi_version: u32_field(&value, "landlock_abi_version"),
        landlock_rights_mask: value.get("landlock_rights_mask").and_then(Value::as_u64),
        landlock_ruleset_status: string_field(&value, "landlock_ruleset_status"),
        landlock_required: value.get("landlock_required").and_then(Value::as_bool),
        fd_cleanup_status: string_field(&value, "fd_cleanup_status"),
        fd_cleanup_method: string_field(&value, "fd_cleanup_method"),
        env_policy: string_field(&value, "env_policy"),
        setup_failure_stage: string_field(&value, "setup_failure_stage"),
        setup_failure_reason: string_field(&value, "setup_failure_reason"),
        failed_closed: value
            .get("failed_closed")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        ..BoundaryAttemptOutcome::default()
    };
    add_evidence_for_statuses(&mut outcome);
    Some(CommandIsolationAttempt::new(outcome))
}

fn add_evidence_for_statuses(outcome: &mut BoundaryAttemptOutcome) {
    if outcome.network_namespace_status == SupervisorSetupStatus::Applied {
        outcome
            .evidence
            .push(AppliedBoundaryEvidence::NetworkNamespace(
                NetnsApplied::from_supervisor_netns_success(),
            ));
    }
    if outcome.mount_namespace_status == SupervisorSetupStatus::Applied {
        outcome
            .evidence
            .push(AppliedBoundaryEvidence::MountNamespace(
                MountNamespaceApplied::from_supervisor_mount_namespace_success(),
            ));
    }
    if outcome.minimal_mount_view_status == MinimalMountViewStatus::Applied {
        outcome
            .evidence
            .push(AppliedBoundaryEvidence::MinimalMountView(
                MinimalMountViewApplied::from_supervisor_minimal_mount_view_success(),
            ));
    }
    if outcome.landlock_status == LandlockApplicationStatus::Applied {
        outcome.evidence.push(AppliedBoundaryEvidence::Landlock(
            LandlockApplied::from_supervisor_landlock_success(),
        ));
    }
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

fn u32_field(value: &Value, key: &str) -> Option<u32> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
}

fn setup_status_field(value: &Value, key: &str) -> SupervisorSetupStatus {
    match value.get(key).and_then(Value::as_str) {
        Some("applied") => SupervisorSetupStatus::Applied,
        Some("not_applied") => SupervisorSetupStatus::NotApplied,
        Some("failed_closed") => SupervisorSetupStatus::FailedClosed,
        Some("unsupported") => SupervisorSetupStatus::Unsupported,
        _ => SupervisorSetupStatus::NotAttempted,
    }
}

fn mount_view_status_field(value: &Value, key: &str) -> MinimalMountViewStatus {
    match value.get(key).and_then(Value::as_str) {
        Some("applied") => MinimalMountViewStatus::Applied,
        Some("not_applied") => MinimalMountViewStatus::NotApplied,
        Some("failed_closed") => MinimalMountViewStatus::FailedClosed,
        _ => MinimalMountViewStatus::NotAttempted,
    }
}

fn landlock_status_field(value: &Value, key: &str) -> LandlockApplicationStatus {
    match value.get(key).and_then(Value::as_str) {
        Some("unsupported") => LandlockApplicationStatus::Unsupported,
        Some("applied") => LandlockApplicationStatus::Applied,
        _ => LandlockApplicationStatus::NotApplied,
    }
}

// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Crate-private command isolation supervisor seam.
//!
//! Wave 0 intentionally defines deterministic planning, ordering, and fake
//! failure mapping only. Later Phase 29 plans wire these stages to Linux
//! primitives; no live namespace, mount, fd, or Landlock syscalls happen here.

#![allow(dead_code)]

pub(crate) mod artifact;
pub(crate) mod env;
pub(crate) mod fd;
pub(crate) mod landlock;
pub(crate) mod linux;
#[cfg(test)]
mod linux_canary_tests;
pub(crate) mod overlay;
pub(crate) mod plan;
pub(crate) mod snapshot;
pub(crate) mod syscall;

use std::collections::BTreeSet;

use landlock::LandlockApplicationReport;

pub(crate) use plan::{default_executable_view_paths, SupervisorPlan};

/// Ordered setup stages for the command supervisor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum SupervisorStage {
    PrivilegeCheck,
    UserNamespace,
    SetgroupsDeny,
    UidMap,
    GidMap,
    NetworkNamespace,
    AssertNoVethOrDefaultRoute,
    MountNamespace,
    MountPropagationPrivate,
    OverlayUpperWork,
    OverlayMount,
    TmpfsRoot,
    ProjectBind,
    ReadOnlyToolchainBind,
    ReadOnlyRemount,
    ReadOnlyBindScopeDecision,
    ScratchTmpfs,
    MinimalDevices,
    OldRootCleanup,
    SavedFdOldRootCheck,
    CwdMapping,
    EnvCleanup,
    FdCleanup,
    Landlock,
    SetupReportPipe,
    OverlayCleanup,
    ExecHandoff,
}

impl SupervisorStage {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            SupervisorStage::PrivilegeCheck => "privilege_check",
            SupervisorStage::UserNamespace => "user_namespace",
            SupervisorStage::SetgroupsDeny => "setgroups_deny",
            SupervisorStage::UidMap => "uid_map",
            SupervisorStage::GidMap => "gid_map",
            SupervisorStage::NetworkNamespace => "network_namespace",
            SupervisorStage::AssertNoVethOrDefaultRoute => "assert_no_veth_or_default_route",
            SupervisorStage::MountNamespace => "mount_namespace",
            SupervisorStage::MountPropagationPrivate => "mount_propagation_private",
            SupervisorStage::OverlayUpperWork => "overlay_upper_work",
            SupervisorStage::OverlayMount => "overlay_mount",
            SupervisorStage::TmpfsRoot => "tmpfs_root",
            SupervisorStage::ProjectBind => "project_bind",
            SupervisorStage::ReadOnlyToolchainBind => "read_only_toolchain_bind",
            SupervisorStage::ReadOnlyRemount => "read_only_remount",
            SupervisorStage::ReadOnlyBindScopeDecision => "read_only_bind_scope_decision",
            SupervisorStage::ScratchTmpfs => "scratch_tmpfs",
            SupervisorStage::MinimalDevices => "minimal_devices",
            SupervisorStage::OldRootCleanup => "old_root_cleanup",
            SupervisorStage::SavedFdOldRootCheck => "saved_fd_old_root_check",
            SupervisorStage::CwdMapping => "cwd_mapping",
            SupervisorStage::EnvCleanup => "env_cleanup",
            SupervisorStage::FdCleanup => "fd_cleanup",
            SupervisorStage::Landlock => "landlock",
            SupervisorStage::SetupReportPipe => "setup_report_pipe",
            SupervisorStage::OverlayCleanup => "overlay_cleanup",
            SupervisorStage::ExecHandoff => "exec_handoff",
        }
    }

    pub(crate) fn from_label(label: &str) -> Option<Self> {
        Some(match label {
            "privilege_check" => SupervisorStage::PrivilegeCheck,
            "user_namespace" => SupervisorStage::UserNamespace,
            "setgroups_deny" => SupervisorStage::SetgroupsDeny,
            "uid_map" => SupervisorStage::UidMap,
            "gid_map" => SupervisorStage::GidMap,
            "network_namespace" => SupervisorStage::NetworkNamespace,
            "assert_no_veth_or_default_route" => SupervisorStage::AssertNoVethOrDefaultRoute,
            "mount_namespace" => SupervisorStage::MountNamespace,
            "mount_propagation_private" => SupervisorStage::MountPropagationPrivate,
            "overlay_upper_work" => SupervisorStage::OverlayUpperWork,
            "overlay_mount" => SupervisorStage::OverlayMount,
            "tmpfs_root" => SupervisorStage::TmpfsRoot,
            "project_bind" => SupervisorStage::ProjectBind,
            "read_only_toolchain_bind" => SupervisorStage::ReadOnlyToolchainBind,
            "read_only_remount" => SupervisorStage::ReadOnlyRemount,
            "read_only_bind_scope_decision" => SupervisorStage::ReadOnlyBindScopeDecision,
            "scratch_tmpfs" => SupervisorStage::ScratchTmpfs,
            "minimal_devices" => SupervisorStage::MinimalDevices,
            "old_root_cleanup" => SupervisorStage::OldRootCleanup,
            "saved_fd_old_root_check" => SupervisorStage::SavedFdOldRootCheck,
            "cwd_mapping" => SupervisorStage::CwdMapping,
            "env_cleanup" => SupervisorStage::EnvCleanup,
            "fd_cleanup" => SupervisorStage::FdCleanup,
            "landlock" => SupervisorStage::Landlock,
            "setup_report_pipe" => SupervisorStage::SetupReportPipe,
            "overlay_cleanup" => SupervisorStage::OverlayCleanup,
            "exec_handoff" => SupervisorStage::ExecHandoff,
            _ => return None,
        })
    }
}

/// A deterministic supervisor attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SupervisorAttempt {
    pub(crate) plan: SupervisorPlan,
    pub(crate) outcome: SupervisorOutcome,
}

/// Final result of applying the supervisor setup plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SupervisorOutcome {
    Ready(SupervisorSetupReport),
    FailedClosed(SupervisorSetupReport),
}

impl SupervisorOutcome {
    pub(crate) fn report(&self) -> &SupervisorSetupReport {
        match self {
            SupervisorOutcome::Ready(report) | SupervisorOutcome::FailedClosed(report) => report,
        }
    }

    pub(crate) fn failed_closed(&self) -> bool {
        matches!(self, SupervisorOutcome::FailedClosed(_))
    }
}

/// Structured setup report produced even when setup fails before exec.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SupervisorSetupReport {
    pub(crate) backend: String,
    pub(crate) applied_stages: BTreeSet<SupervisorStage>,
    pub(crate) failed_stage: Option<SupervisorStage>,
    pub(crate) error: Option<SupervisorSetupError>,
    pub(crate) landlock: Option<LandlockApplicationReport>,
}

impl SupervisorSetupReport {
    fn new(backend: impl Into<String>) -> Self {
        Self {
            backend: backend.into(),
            applied_stages: BTreeSet::new(),
            failed_stage: None,
            error: None,
            landlock: None,
        }
    }

    pub(crate) fn stage_applied(&self, stage: SupervisorStage) -> bool {
        self.applied_stages.contains(&stage)
    }
}

/// Setup failure for a specific supervisor stage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SupervisorSetupError {
    pub(crate) stage: SupervisorStage,
    pub(crate) reason: String,
}

impl SupervisorSetupError {
    pub(crate) fn new(stage: SupervisorStage, reason: impl Into<String>) -> Self {
        Self {
            stage,
            reason: reason.into(),
        }
    }
}

/// Fakeable backend seam. Production implementations should bind this to
/// native Linux primitive setup; tests use it to record order and inject errors.
pub(crate) trait SupervisorBackend {
    fn backend_label(&self) -> &'static str;

    fn landlock_report(&self) -> Option<LandlockApplicationReport> {
        None
    }

    fn apply_stage(
        &mut self,
        stage: SupervisorStage,
        plan: &SupervisorPlan,
    ) -> Result<(), SupervisorSetupError>;
}

pub(crate) fn run_supervisor_attempt<B: SupervisorBackend>(
    plan: SupervisorPlan,
    backend: &mut B,
) -> SupervisorAttempt {
    let mut report = SupervisorSetupReport::new(backend.backend_label());
    for stage in plan.stages() {
        match backend.apply_stage(stage, &plan) {
            Ok(()) => {
                report.applied_stages.insert(stage);
                if stage == SupervisorStage::Landlock {
                    report.landlock = backend.landlock_report();
                }
            }
            Err(error) => {
                if stage == SupervisorStage::Landlock {
                    report.landlock = backend.landlock_report();
                }
                report.failed_stage = Some(stage);
                report.error = Some(error);
                return SupervisorAttempt {
                    plan,
                    outcome: SupervisorOutcome::FailedClosed(report),
                };
            }
        }
    }

    SupervisorAttempt {
        plan,
        outcome: SupervisorOutcome::Ready(report),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[derive(Default)]
    struct FakeSupervisorBackend {
        calls: Vec<SupervisorStage>,
        fail_at: Option<SupervisorStage>,
    }

    impl FakeSupervisorBackend {
        fn failing_at(stage: SupervisorStage) -> Self {
            Self {
                calls: Vec::new(),
                fail_at: Some(stage),
            }
        }
    }

    impl SupervisorBackend for FakeSupervisorBackend {
        fn backend_label(&self) -> &'static str {
            "fake-supervisor"
        }

        fn apply_stage(
            &mut self,
            stage: SupervisorStage,
            _plan: &SupervisorPlan,
        ) -> Result<(), SupervisorSetupError> {
            self.calls.push(stage);
            if self.fail_at == Some(stage) {
                Err(SupervisorSetupError::new(
                    stage,
                    format!("injected {} failure", stage.label()),
                ))
            } else {
                Ok(())
            }
        }
    }

    fn full_plan() -> SupervisorPlan {
        SupervisorPlan::wave0_full_fake(PathBuf::from("/project"), PathBuf::from("/project/src"))
    }

    #[test]
    fn isolation_supervisor_overlay_stage_labels_round_trip() {
        for stage in [
            SupervisorStage::OverlayUpperWork,
            SupervisorStage::OverlayMount,
            SupervisorStage::OverlayCleanup,
        ] {
            assert_eq!(SupervisorStage::from_label(stage.label()), Some(stage));
        }
    }

    #[test]
    fn isolation_supervisor_fake_records_setup_order_without_privileges() {
        let plan = full_plan();
        let mut backend = FakeSupervisorBackend::default();
        let attempt = run_supervisor_attempt(plan, &mut backend);

        assert_eq!(
            backend.calls,
            vec![
                SupervisorStage::PrivilegeCheck,
                SupervisorStage::UserNamespace,
                SupervisorStage::SetgroupsDeny,
                SupervisorStage::UidMap,
                SupervisorStage::GidMap,
                SupervisorStage::NetworkNamespace,
                SupervisorStage::AssertNoVethOrDefaultRoute,
                SupervisorStage::MountNamespace,
                SupervisorStage::MountPropagationPrivate,
                SupervisorStage::TmpfsRoot,
                SupervisorStage::ProjectBind,
                SupervisorStage::ReadOnlyBindScopeDecision,
                SupervisorStage::ScratchTmpfs,
                SupervisorStage::MinimalDevices,
                SupervisorStage::OldRootCleanup,
                SupervisorStage::SavedFdOldRootCheck,
                SupervisorStage::CwdMapping,
                SupervisorStage::EnvCleanup,
                SupervisorStage::FdCleanup,
                SupervisorStage::Landlock,
                SupervisorStage::SetupReportPipe,
                SupervisorStage::ExecHandoff,
            ]
        );
        assert!(!attempt.outcome.failed_closed());
        let report = attempt.outcome.report();
        assert_eq!(report.backend, "fake-supervisor");
        assert!(report.stage_applied(SupervisorStage::NetworkNamespace));
        assert!(report.stage_applied(SupervisorStage::ExecHandoff));
    }

    #[test]
    fn isolation_supervisor_fake_maps_every_stage_failure_to_failed_closed() {
        for stage in [
            SupervisorStage::PrivilegeCheck,
            SupervisorStage::UserNamespace,
            SupervisorStage::SetgroupsDeny,
            SupervisorStage::UidMap,
            SupervisorStage::GidMap,
            SupervisorStage::NetworkNamespace,
            SupervisorStage::AssertNoVethOrDefaultRoute,
            SupervisorStage::MountNamespace,
            SupervisorStage::MountPropagationPrivate,
            SupervisorStage::TmpfsRoot,
            SupervisorStage::ProjectBind,
            SupervisorStage::ReadOnlyBindScopeDecision,
            SupervisorStage::ScratchTmpfs,
            SupervisorStage::MinimalDevices,
            SupervisorStage::OldRootCleanup,
            SupervisorStage::SavedFdOldRootCheck,
            SupervisorStage::CwdMapping,
            SupervisorStage::EnvCleanup,
            SupervisorStage::FdCleanup,
            SupervisorStage::Landlock,
            SupervisorStage::SetupReportPipe,
            SupervisorStage::ExecHandoff,
        ] {
            let mut backend = FakeSupervisorBackend::failing_at(stage);
            let attempt = run_supervisor_attempt(full_plan(), &mut backend);
            let report = attempt.outcome.report();

            assert!(attempt.outcome.failed_closed(), "{}", stage.label());
            assert_eq!(report.failed_stage, Some(stage));
            assert_eq!(report.error.as_ref().map(|error| error.stage), Some(stage));
            assert!(backend.calls.contains(&stage));
            assert!(!report.stage_applied(stage));
        }
    }

    #[test]
    fn isolation_supervisor_fake_stops_after_injected_failure() {
        let mut backend = FakeSupervisorBackend::failing_at(SupervisorStage::ProjectBind);
        let attempt = run_supervisor_attempt(full_plan(), &mut backend);

        assert!(attempt.outcome.failed_closed());
        assert_eq!(
            backend.calls,
            vec![
                SupervisorStage::PrivilegeCheck,
                SupervisorStage::UserNamespace,
                SupervisorStage::SetgroupsDeny,
                SupervisorStage::UidMap,
                SupervisorStage::GidMap,
                SupervisorStage::NetworkNamespace,
                SupervisorStage::AssertNoVethOrDefaultRoute,
                SupervisorStage::MountNamespace,
                SupervisorStage::MountPropagationPrivate,
                SupervisorStage::TmpfsRoot,
                SupervisorStage::ProjectBind,
            ]
        );
        let report = attempt.outcome.report();
        assert!(report.stage_applied(SupervisorStage::NetworkNamespace));
        assert!(report.stage_applied(SupervisorStage::MountNamespace));
        assert!(!report.stage_applied(SupervisorStage::ProjectBind));
        assert!(!report.stage_applied(SupervisorStage::ExecHandoff));
    }
}

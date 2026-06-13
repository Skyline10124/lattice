// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Linux supervisor setup backend built on the fakeable syscall seam.

use lancet_authority::{
    LandlockApplied, MinimalMountViewApplied, MountNamespaceApplied, NetnsApplied,
};

use crate::enforcement::isolation::{
    AppliedBoundaryEvidence, BoundaryAttemptOutcome, CommandIsolationAttempt,
    LandlockApplicationStatus, MinimalMountViewStatus, SupervisorSetupStatus,
};

use super::landlock::{LandlockApplicationReport, LandlockRefinementPlan};
use super::overlay::mount_overlay;
use super::plan::{MountKind, PrivilegeStrategy, SupervisorPlan};
use super::syscall::{SupervisorSyscalls, SyscallError};
use super::{
    SupervisorAttempt, SupervisorBackend, SupervisorSetupError, SupervisorStage,
    run_supervisor_attempt,
};

pub(crate) struct LinuxSupervisorBackend<S> {
    syscalls: S,
    landlock_report: Option<LandlockApplicationReport>,
}

impl<S> LinuxSupervisorBackend<S> {
    pub(crate) fn new(syscalls: S) -> Self {
        Self {
            syscalls,
            landlock_report: None,
        }
    }
}

impl<S: SupervisorSyscalls> SupervisorBackend for LinuxSupervisorBackend<S> {
    fn backend_label(&self) -> &'static str {
        if cfg!(target_os = "linux") {
            "native_linux"
        } else {
            "unsupported_platform"
        }
    }

    fn landlock_report(&self) -> Option<LandlockApplicationReport> {
        self.landlock_report.clone()
    }

    fn apply_stage(
        &mut self,
        stage: SupervisorStage,
        plan: &SupervisorPlan,
    ) -> Result<(), SupervisorSetupError> {
        apply_linux_stage(&mut self.syscalls, &mut self.landlock_report, stage, plan).map_err(
            |err| SupervisorSetupError::new(stage, format!("{}: {}", err.operation, err.reason)),
        )
    }
}

pub(crate) fn run_linux_supervisor_attempt<S: SupervisorSyscalls>(
    plan: SupervisorPlan,
    syscalls: S,
) -> SupervisorAttempt {
    let mut backend = LinuxSupervisorBackend::new(syscalls);
    run_supervisor_attempt(plan, &mut backend)
}

pub(crate) fn boundary_attempt_from_supervisor(
    attempt: &SupervisorAttempt,
) -> CommandIsolationAttempt {
    let report = attempt.outcome.report();
    let plan = &attempt.plan;
    let mut outcome = BoundaryAttemptOutcome {
        supervisor_backend: Some(report.backend.clone()),
        project_view_path: Some(plan.project_view_path.clone()),
        cwd_view_path: Some(plan.cwd_view_path.clone()),
        privilege_strategy: Some(plan.privilege_strategy.label().to_string()),
        read_only_bind_scope: Some(plan.read_only_bind_scope.label().to_string()),
        env_policy: env_policy_label(report.stage_applied(SupervisorStage::EnvCleanup)),
        fd_cleanup_status: stage_applied_label(report.stage_applied(SupervisorStage::FdCleanup)),
        fd_cleanup_method: Some("supervisor_fd_cleanup".to_string()),
        failed_closed: attempt.outcome.failed_closed(),
        setup_failure_stage: report.failed_stage.map(|stage| stage.label().to_string()),
        setup_failure_reason: report.error.as_ref().map(|error| error.reason.clone()),
        ..BoundaryAttemptOutcome::default()
    };

    outcome.userns_status = optional_stage_status(
        plan.privilege_strategy == PrivilegeStrategy::UnprivilegedUserNamespace,
        report,
        SupervisorStage::UserNamespace,
    );
    outcome.setgroups_status = optional_stage_status(
        plan.privilege_strategy == PrivilegeStrategy::UnprivilegedUserNamespace,
        report,
        SupervisorStage::SetgroupsDeny,
    );
    outcome.uid_map_status = optional_stage_status(
        plan.privilege_strategy == PrivilegeStrategy::UnprivilegedUserNamespace,
        report,
        SupervisorStage::UidMap,
    );
    outcome.gid_map_status = optional_stage_status(
        plan.privilege_strategy == PrivilegeStrategy::UnprivilegedUserNamespace,
        report,
        SupervisorStage::GidMap,
    );

    if report.stage_applied(SupervisorStage::NetworkNamespace) {
        outcome.network_namespace_status = SupervisorSetupStatus::Applied;
        outcome
            .evidence
            .push(AppliedBoundaryEvidence::NetworkNamespace(
                NetnsApplied::from_supervisor_netns_success(),
            ));
    } else if failed_at_or_before(report.failed_stage, SupervisorStage::NetworkNamespace) {
        outcome.network_namespace_status = SupervisorSetupStatus::FailedClosed;
    }

    if report.stage_applied(SupervisorStage::MountNamespace) {
        outcome.mount_namespace_status = SupervisorSetupStatus::Applied;
        outcome
            .evidence
            .push(AppliedBoundaryEvidence::MountNamespace(
                MountNamespaceApplied::from_supervisor_mount_namespace_success(),
            ));
    } else if failed_at_or_before(report.failed_stage, SupervisorStage::MountNamespace) {
        outcome.mount_namespace_status = SupervisorSetupStatus::FailedClosed;
    }

    if mount_view_applied(report) {
        outcome.minimal_mount_view_status = MinimalMountViewStatus::Applied;
        outcome
            .evidence
            .push(AppliedBoundaryEvidence::MinimalMountView(
                MinimalMountViewApplied::from_supervisor_minimal_mount_view_success(),
            ));
    } else if report.failed_stage.is_some() && plan.mount_view {
        outcome.minimal_mount_view_status = MinimalMountViewStatus::FailedClosed;
    }

    if let Some(landlock) = &report.landlock {
        outcome.landlock_status = landlock.status;
        outcome.landlock_abi_version = landlock.abi_version;
        outcome.landlock_rights_mask = landlock.rights_mask;
        outcome.landlock_ruleset_status = Some(landlock.ruleset_status.clone());
        outcome.landlock_required = Some(landlock.required);
        if landlock.status == LandlockApplicationStatus::Applied {
            outcome.evidence.push(AppliedBoundaryEvidence::Landlock(
                LandlockApplied::from_supervisor_landlock_success(),
            ));
        }
    } else {
        outcome.landlock_required = Some(plan.landlock_required);
        if !plan.landlock {
            outcome.landlock_ruleset_status = Some("disabled".to_string());
        }
    }

    CommandIsolationAttempt::new(outcome)
}

fn apply_linux_stage<S: SupervisorSyscalls>(
    syscalls: &mut S,
    landlock_report: &mut Option<LandlockApplicationReport>,
    stage: SupervisorStage,
    plan: &SupervisorPlan,
) -> Result<(), SyscallError> {
    match stage {
        SupervisorStage::PrivilegeCheck => match plan.privilege_strategy {
            PrivilegeStrategy::DirectCapSysAdmin => {
                if syscalls.has_cap_sys_admin()? {
                    Ok(())
                } else {
                    Err(SyscallError::new(
                        "cap_sys_admin",
                        "direct privileged setup requested without CAP_SYS_ADMIN",
                    ))
                }
            }
            PrivilegeStrategy::UnprivilegedUserNamespace => {
                syscalls.check_unprivileged_user_namespace()
            }
        },
        SupervisorStage::UserNamespace => syscalls.unshare_user_namespace(),
        SupervisorStage::SetgroupsDeny => syscalls.write_setgroups_deny(),
        SupervisorStage::UidMap => syscalls.write_uid_map_current(),
        SupervisorStage::GidMap => syscalls.write_gid_map_current(),
        SupervisorStage::PidNamespace => syscalls.enter_pid_namespace_child(),
        SupervisorStage::NetworkNamespace => syscalls.unshare_network_namespace(),
        SupervisorStage::AssertNoVethOrDefaultRoute => syscalls.assert_no_veth_or_default_route(),
        SupervisorStage::MountNamespace => syscalls.unshare_mount_namespace(),
        SupervisorStage::MountPropagationPrivate => syscalls.make_mounts_private(),
        SupervisorStage::OverlayUpperWork => {
            let Some(overlay_plan) = plan.overlay_plan.as_ref() else {
                return Err(SyscallError::new(
                    "overlay_upper_work",
                    "overlay stage requested without overlay plan",
                ));
            };
            super::overlay::prepare_overlay_dirs(overlay_plan)
        }
        SupervisorStage::OverlayMount => {
            let Some(overlay_plan) = plan.overlay_plan.as_ref() else {
                return Err(SyscallError::new(
                    "overlay_mount",
                    "overlay stage requested without overlay plan",
                ));
            };
            mount_overlay(overlay_plan, syscalls).map(|_| ())
        }
        SupervisorStage::TmpfsRoot => syscalls.mount_tmpfs_root(),
        SupervisorStage::ProjectBind => {
            for mount in plan
                .mounts
                .iter()
                .filter(|mount| mount.kind == MountKind::ProjectRoot)
            {
                let source = plan.project_mount_source();
                syscalls.bind_mount(source, &mount.target)?;
            }
            Ok(())
        }
        SupervisorStage::ReadOnlyToolchainBind => {
            for mount in plan
                .mounts
                .iter()
                .filter(|mount| mount.kind == MountKind::ReadOnlyToolchain)
            {
                let source = mount.source.as_deref().ok_or_else(|| {
                    SyscallError::new("read_only_toolchain_bind", "toolchain bind missing source")
                })?;
                syscalls.bind_mount(source, &mount.target)?;
            }
            Ok(())
        }
        SupervisorStage::ReadOnlyRemount => {
            for mount in plan
                .mounts
                .iter()
                .filter(|mount| mount.kind == MountKind::ReadOnlyToolchain && mount.read_only)
            {
                syscalls.remount_read_only(&mount.target)?;
            }
            Ok(())
        }
        SupervisorStage::ReadOnlyBindScopeDecision => {
            syscalls.record_read_only_bind_scope(plan.read_only_bind_scope)
        }
        SupervisorStage::ScratchTmpfs => {
            for mount in plan
                .mounts
                .iter()
                .filter(|mount| mount.kind == MountKind::ScratchTmpfs)
            {
                syscalls.mount_scratch_tmpfs(&mount.target)?;
            }
            Ok(())
        }
        SupervisorStage::ProcFs => {
            for mount in plan
                .mounts
                .iter()
                .filter(|mount| mount.kind == MountKind::ProcFs)
            {
                syscalls.mount_procfs(&mount.target)?;
            }
            Ok(())
        }
        SupervisorStage::MinimalDevices => {
            for mount in plan
                .mounts
                .iter()
                .filter(|mount| mount.kind == MountKind::MinimalDevice)
            {
                let source = mount.source.as_deref().ok_or_else(|| {
                    SyscallError::new("minimal_device", "device bind missing source")
                })?;
                syscalls.mount_minimal_device(source, &mount.target)?;
            }
            Ok(())
        }
        SupervisorStage::OldRootCleanup => syscalls.detach_old_root(),
        SupervisorStage::SavedFdOldRootCheck => syscalls.assert_old_root_unreachable(),
        SupervisorStage::CwdMapping => syscalls.chdir(&plan.cwd_view_path),
        SupervisorStage::EnvCleanup => syscalls.cleanup_env(),
        SupervisorStage::FdCleanup => syscalls.cleanup_fds(),
        SupervisorStage::Landlock => apply_landlock_stage(syscalls, landlock_report, plan),
        SupervisorStage::SetupReportPipe => syscalls.setup_report_pipe_ready(),
        SupervisorStage::OverlayCleanup => Ok(()),
        SupervisorStage::ExecHandoff => syscalls.exec_handoff_ready(),
    }
}

fn apply_landlock_stage<S: SupervisorSyscalls>(
    syscalls: &mut S,
    landlock_report: &mut Option<LandlockApplicationReport>,
    plan: &SupervisorPlan,
) -> Result<(), SyscallError> {
    let abi = syscalls.probe_landlock_abi()?;
    let Some(abi_version) = abi.abi_version.filter(|_| abi.supported) else {
        *landlock_report = Some(LandlockApplicationReport::unsupported(
            plan.landlock_required,
            abi.reason,
        ));
        return if plan.landlock_fail_closed {
            Err(SyscallError::new("landlock_abi_probe", "unsupported"))
        } else {
            Ok(())
        };
    };

    let refinement = LandlockRefinementPlan::from_supervisor_plan(
        plan,
        abi_version,
        plan.landlock_required,
        plan.landlock_fail_closed,
    );
    match syscalls.apply_landlock(&refinement) {
        Ok(report) => {
            *landlock_report = Some(report);
            Ok(())
        }
        Err(err) => {
            *landlock_report = Some(LandlockApplicationReport::not_applied(
                plan.landlock_required,
                Some(refinement.abi_version),
                Some(refinement.handled_access_fs),
                format!("{}: {}", err.operation, err.reason),
            ));
            if refinement.fail_closed {
                Err(err)
            } else {
                Ok(())
            }
        }
    }
}

fn stage_applied_label(applied: bool) -> Option<String> {
    Some(if applied { "applied" } else { "failed" }.to_string())
}

fn env_policy_label(applied: bool) -> Option<String> {
    Some(if applied { "allowlist" } else { "failed" }.to_string())
}

fn optional_stage_status(
    requested: bool,
    report: &super::SupervisorSetupReport,
    stage: SupervisorStage,
) -> Option<String> {
    if !requested {
        Some("not_attempted".to_string())
    } else if report.stage_applied(stage) {
        Some("applied".to_string())
    } else if failed_at_or_before(report.failed_stage, stage) {
        Some("failed_closed".to_string())
    } else {
        Some("not_attempted".to_string())
    }
}

fn failed_at_or_before(failed: Option<SupervisorStage>, stage: SupervisorStage) -> bool {
    failed.is_some_and(|failed| failed <= stage)
}

fn mount_view_applied(report: &super::SupervisorSetupReport) -> bool {
    [
        SupervisorStage::TmpfsRoot,
        SupervisorStage::ProjectBind,
        SupervisorStage::ReadOnlyBindScopeDecision,
        SupervisorStage::ScratchTmpfs,
        SupervisorStage::ProcFs,
        SupervisorStage::MinimalDevices,
        SupervisorStage::OldRootCleanup,
        SupervisorStage::SavedFdOldRootCheck,
    ]
    .into_iter()
    .all(|stage| report.stage_applied(stage))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enforcement::isolation::CommandIsolationRuntimeConfig;
    use crate::executor::supervisor::landlock::{
        LandlockAbiProbe, LandlockApplicationReport, LandlockRefinementPlan,
    };
    use crate::executor::supervisor::plan::SupervisorPlan;
    use crate::executor::supervisor::syscall::{SupervisorSyscalls, SyscallError};
    use lancet_protocol::{BoundaryDimension, BoundaryPrimitive, EffectiveBoundaryStatus};
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};
    use tempfile::TempDir;

    #[derive(Default)]
    struct FakeSyscalls {
        cap_sys_admin: bool,
        landlock_abi: Option<u32>,
        fail_operations: BTreeSet<&'static str>,
        calls: Vec<String>,
    }

    impl FakeSyscalls {
        fn direct() -> Self {
            Self {
                cap_sys_admin: true,
                landlock_abi: Some(3),
                ..Self::default()
            }
        }

        fn with_landlock_abi(mut self, abi: Option<u32>) -> Self {
            self.landlock_abi = abi;
            self
        }

        fn fail(mut self, operation: &'static str) -> Self {
            self.fail_operations.insert(operation);
            self
        }

        fn record(&mut self, operation: &'static str) -> Result<(), SyscallError> {
            self.calls.push(operation.to_string());
            if self.fail_operations.contains(operation) {
                Err(SyscallError::new(operation, "injected failure"))
            } else {
                Ok(())
            }
        }
    }

    impl SupervisorSyscalls for FakeSyscalls {
        fn has_cap_sys_admin(&mut self) -> Result<bool, SyscallError> {
            self.calls.push("has_cap_sys_admin".to_string());
            Ok(self.cap_sys_admin)
        }

        fn check_unprivileged_user_namespace(&mut self) -> Result<(), SyscallError> {
            self.record("check_unprivileged_user_namespace")
        }

        fn unshare_user_namespace(&mut self) -> Result<(), SyscallError> {
            self.record("unshare_user_namespace")
        }

        fn write_setgroups_deny(&mut self) -> Result<(), SyscallError> {
            self.record("write_setgroups_deny")
        }

        fn write_uid_map_current(&mut self) -> Result<(), SyscallError> {
            self.record("write_uid_map_current")
        }

        fn write_gid_map_current(&mut self) -> Result<(), SyscallError> {
            self.record("write_gid_map_current")
        }

        fn enter_pid_namespace_child(&mut self) -> Result<(), SyscallError> {
            self.record("enter_pid_namespace_child")
        }

        fn unshare_network_namespace(&mut self) -> Result<(), SyscallError> {
            self.record("unshare_network_namespace")
        }

        fn assert_no_veth_or_default_route(&mut self) -> Result<(), SyscallError> {
            self.record("assert_no_veth_or_default_route")
        }

        fn unshare_mount_namespace(&mut self) -> Result<(), SyscallError> {
            self.record("unshare_mount_namespace")
        }

        fn make_mounts_private(&mut self) -> Result<(), SyscallError> {
            self.record("make_mounts_private")
        }

        fn mount_tmpfs_root(&mut self) -> Result<(), SyscallError> {
            self.record("mount_tmpfs_root")
        }

        fn bind_mount(&mut self, source: &Path, target: &Path) -> Result<(), SyscallError> {
            self.calls.push(format!(
                "bind_mount:{}->{}",
                source.display(),
                target.display()
            ));
            let operation = if target == Path::new("/project") {
                "project_bind"
            } else {
                "read_only_toolchain_bind"
            };
            if self.fail_operations.contains(operation) {
                Err(SyscallError::new(operation, "injected failure"))
            } else {
                Ok(())
            }
        }

        fn remount_read_only(&mut self, target: &Path) -> Result<(), SyscallError> {
            self.calls
                .push(format!("remount_read_only:{}", target.display()));
            if self.fail_operations.contains("remount_read_only") {
                Err(SyscallError::new("remount_read_only", "injected failure"))
            } else {
                Ok(())
            }
        }

        fn record_read_only_bind_scope(
            &mut self,
            _scope: super::super::plan::ReadOnlyBindScope,
        ) -> Result<(), SyscallError> {
            self.record("record_read_only_bind_scope")
        }

        fn mount_scratch_tmpfs(&mut self, target: &Path) -> Result<(), SyscallError> {
            self.calls
                .push(format!("mount_scratch_tmpfs:{}", target.display()));
            if self.fail_operations.contains("scratch_tmpfs") {
                Err(SyscallError::new("scratch_tmpfs", "injected failure"))
            } else {
                Ok(())
            }
        }

        fn mount_procfs(&mut self, target: &Path) -> Result<(), SyscallError> {
            self.calls
                .push(format!("mount_procfs:{}", target.display()));
            if self.fail_operations.contains("procfs") {
                Err(SyscallError::new("procfs", "injected failure"))
            } else {
                Ok(())
            }
        }

        fn mount_minimal_device(
            &mut self,
            source: &Path,
            target: &Path,
        ) -> Result<(), SyscallError> {
            self.calls.push(format!(
                "mount_minimal_device:{}->{}",
                source.display(),
                target.display()
            ));
            if self.fail_operations.contains("minimal_devices") {
                Err(SyscallError::new("minimal_devices", "injected failure"))
            } else {
                Ok(())
            }
        }

        fn detach_old_root(&mut self) -> Result<(), SyscallError> {
            self.record("detach_old_root")
        }

        fn assert_old_root_unreachable(&mut self) -> Result<(), SyscallError> {
            self.record("assert_old_root_unreachable")
        }

        fn chdir(&mut self, target: &Path) -> Result<(), SyscallError> {
            self.calls.push(format!("chdir:{}", target.display()));
            if self.fail_operations.contains("chdir") {
                Err(SyscallError::new("chdir", "injected failure"))
            } else {
                Ok(())
            }
        }

        fn cleanup_env(&mut self) -> Result<(), SyscallError> {
            self.record("cleanup_env")
        }

        fn cleanup_fds(&mut self) -> Result<(), SyscallError> {
            self.record("cleanup_fds")
        }

        fn probe_landlock_abi(&mut self) -> Result<LandlockAbiProbe, SyscallError> {
            self.calls.push("landlock_abi_probe".to_string());
            Ok(match self.landlock_abi {
                Some(version) => LandlockAbiProbe::supported(version),
                None => LandlockAbiProbe::unsupported("unsupported"),
            })
        }

        fn apply_landlock(
            &mut self,
            plan: &LandlockRefinementPlan,
        ) -> Result<LandlockApplicationReport, SyscallError> {
            self.calls.push(format!(
                "landlock_create_ruleset:0x{:x}",
                plan.handled_access_fs
            ));
            self.calls.push("landlock_ruleset_fd_cloexec".to_string());
            for rule in &plan.rules {
                self.calls
                    .push(format!("landlock_add_rule:{}", rule.path.display()));
                self.calls.push("landlock_path_fd_cloexec".to_string());
                self.calls.push("landlock_path_fd_closed".to_string());
            }
            self.calls.push("prctl_PR_SET_NO_NEW_PRIVS".to_string());
            self.calls.push("landlock_restrict_self".to_string());
            self.calls.push("landlock_ruleset_fd_closed".to_string());
            if self.fail_operations.contains("landlock_restrict_self") {
                Err(SyscallError::new(
                    "landlock_restrict_self",
                    "injected failure",
                ))
            } else {
                Ok(LandlockApplicationReport::applied(
                    plan.abi_version,
                    plan.handled_access_fs,
                    plan.required,
                ))
            }
        }

        fn setup_report_pipe_ready(&mut self) -> Result<(), SyscallError> {
            self.record("setup_report_pipe_ready")
        }

        fn exec_handoff_ready(&mut self) -> Result<(), SyscallError> {
            self.record("exec_handoff_ready")
        }

        fn mount_overlay(
            &mut self,
            lower: &Path,
            upper: &Path,
            work: &Path,
            merged: &Path,
        ) -> Result<(), SyscallError> {
            self.calls.push(format!(
                "mount_overlay:{}->{}:{}:{}",
                lower.display(),
                merged.display(),
                upper.display(),
                work.display()
            ));
            if self.fail_operations.contains("mount_overlay") {
                Err(SyscallError::new("mount_overlay", "injected failure"))
            } else {
                Ok(())
            }
        }

        fn umount_overlay(&mut self, merged: &Path) -> Result<(), SyscallError> {
            self.calls
                .push(format!("umount_overlay:{}", merged.display()));
            if self.fail_operations.contains("umount_overlay") {
                Err(SyscallError::new("umount_overlay", "injected failure"))
            } else {
                Ok(())
            }
        }

        fn remove_overlay_tree(&mut self, path: &Path) -> Result<(), SyscallError> {
            self.calls
                .push(format!("remove_overlay_tree:{}", path.display()));
            if self.fail_operations.contains("remove_overlay_tree") {
                Err(SyscallError::new("remove_overlay_tree", "injected failure"))
            } else {
                Ok(())
            }
        }
    }

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().expect("parent")).unwrap();
        std::fs::write(path, b"#!/bin/sh\n").unwrap();
    }

    fn plan_with_toolchain(strategy: PrivilegeStrategy) -> SupervisorPlan {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("project");
        let toolchain = temp.path().join("toolchain");
        touch(&root.join("bin/run"));
        touch(&toolchain.join("bin/cargo"));
        let config = CommandIsolationRuntimeConfig {
            mode: crate::enforcement::isolation::CommandIsolationMode::Enforce,
            optional_boundaries: vec![BoundaryDimension::Landlock],
            read_only_toolchain_paths: vec![toolchain],
            scratch_paths: vec![PathBuf::from("/scratch/build")],
            ..CommandIsolationRuntimeConfig::default()
        };
        SupervisorPlan::build(&root, None, &["run".to_string()], &config)
            .unwrap()
            .with_privilege_strategy(strategy)
    }

    fn require_landlock(mut plan: SupervisorPlan) -> SupervisorPlan {
        plan.landlock_required = true;
        plan.landlock_fail_closed = true;
        plan
    }

    #[test]
    fn network_direct_setup_occurs_before_exec_and_gets_netns_credit() {
        let plan = plan_with_toolchain(PrivilegeStrategy::DirectCapSysAdmin);
        let mut backend = LinuxSupervisorBackend::new(FakeSyscalls::direct());
        let attempt = run_supervisor_attempt(plan, &mut backend);
        let boundary = boundary_attempt_from_supervisor(&attempt);

        assert!(!attempt.outcome.failed_closed());
        let calls = &backend.syscalls.calls;
        let net = calls
            .iter()
            .position(|call| call == "unshare_network_namespace")
            .unwrap();
        let exec = calls
            .iter()
            .position(|call| call == "exec_handoff_ready")
            .unwrap();
        assert!(net < exec);
        assert!(
            calls
                .iter()
                .any(|call| call == "assert_no_veth_or_default_route")
        );
        assert!(!calls.iter().any(|call| call.contains("create_veth")));
        assert!(!calls.iter().any(|call| call.contains("add_default_route")));
        assert!(
            !calls
                .iter()
                .any(|call| call.contains("join_host_namespace"))
        );
        assert_eq!(
            boundary.outcome.network_namespace_status,
            SupervisorSetupStatus::Applied
        );
        assert!(
            boundary.outcome.evidence.iter().any(|evidence| {
                matches!(evidence, AppliedBoundaryEvidence::NetworkNamespace(_))
            })
        );
    }

    #[test]
    fn user_namespace_bootstrap_orders_setgroups_before_uid_and_gid_maps() {
        let plan = plan_with_toolchain(PrivilegeStrategy::UnprivilegedUserNamespace);
        let mut backend = LinuxSupervisorBackend::new(FakeSyscalls::default());
        let attempt = run_supervisor_attempt(plan, &mut backend);
        let calls = &backend.syscalls.calls;

        assert!(!attempt.outcome.failed_closed());
        assert!(
            calls
                .iter()
                .position(|call| call == "check_unprivileged_user_namespace")
                < calls
                    .iter()
                    .position(|call| call == "unshare_user_namespace")
        );
        assert!(
            calls
                .iter()
                .position(|call| call == "unshare_user_namespace")
                < calls.iter().position(|call| call == "write_setgroups_deny")
        );
        assert!(
            calls.iter().position(|call| call == "write_setgroups_deny")
                < calls
                    .iter()
                    .position(|call| call == "write_uid_map_current")
        );
        assert!(
            calls
                .iter()
                .position(|call| call == "write_uid_map_current")
                < calls
                    .iter()
                    .position(|call| call == "write_gid_map_current")
        );
        assert!(
            calls
                .iter()
                .position(|call| call == "write_gid_map_current")
                < calls
                    .iter()
                    .position(|call| call == "unshare_network_namespace")
        );
    }

    #[test]
    fn unprivileged_user_namespace_preflight_failure_stops_before_userns() {
        let plan = plan_with_toolchain(PrivilegeStrategy::UnprivilegedUserNamespace);
        let mut backend = LinuxSupervisorBackend::new(
            FakeSyscalls::default().fail("check_unprivileged_user_namespace"),
        );
        let attempt = run_supervisor_attempt(plan, &mut backend);
        let boundary = boundary_attempt_from_supervisor(&attempt);

        assert!(attempt.outcome.failed_closed());
        assert_eq!(
            attempt.outcome.report().failed_stage,
            Some(SupervisorStage::PrivilegeCheck)
        );
        assert!(
            !backend
                .syscalls
                .calls
                .iter()
                .any(|call| call == "unshare_user_namespace")
        );
        assert_eq!(
            boundary.outcome.userns_status.as_deref(),
            Some("failed_closed")
        );
        assert!(
            boundary
                .outcome
                .setup_failure_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("check_unprivileged_user_namespace"))
        );
    }

    #[test]
    fn user_namespace_mapping_failure_reports_stage_and_never_execs() {
        let plan = plan_with_toolchain(PrivilegeStrategy::UnprivilegedUserNamespace);
        let mut backend =
            LinuxSupervisorBackend::new(FakeSyscalls::default().fail("write_gid_map_current"));
        let attempt = run_supervisor_attempt(plan, &mut backend);
        let boundary = boundary_attempt_from_supervisor(&attempt);

        assert!(attempt.outcome.failed_closed());
        assert!(
            !backend
                .syscalls
                .calls
                .iter()
                .any(|call| call == "exec_handoff_ready")
        );
        assert_eq!(
            boundary.outcome.gid_map_status.as_deref(),
            Some("failed_closed")
        );
        assert_eq!(
            boundary.outcome.setup_failure_stage.as_deref(),
            Some("gid_map")
        );
    }

    #[test]
    fn filesystem_credit_requires_mount_view_applied_not_mount_namespace_only() {
        let plan = plan_with_toolchain(PrivilegeStrategy::UnprivilegedUserNamespace);
        let mut backend = LinuxSupervisorBackend::new(FakeSyscalls::default().fail("project_bind"));
        let attempt = run_supervisor_attempt(plan, &mut backend);
        let boundary = boundary_attempt_from_supervisor(&attempt);
        let config = CommandIsolationRuntimeConfig {
            mode: crate::enforcement::isolation::CommandIsolationMode::Enforce,
            required_boundaries: vec![BoundaryDimension::Filesystem],
            ..CommandIsolationRuntimeConfig::default()
        };
        let preflight = crate::enforcement::isolation::evaluate_command_boundary_attempt(
            &config,
            Some(&boundary),
        );
        let filesystem = preflight
            .effective_boundary
            .status_for(BoundaryDimension::Filesystem)
            .unwrap();

        assert_eq!(
            boundary.outcome.mount_namespace_status,
            SupervisorSetupStatus::Applied
        );
        assert_eq!(
            boundary.outcome.minimal_mount_view_status,
            MinimalMountViewStatus::FailedClosed
        );
        assert_eq!(filesystem.status, EffectiveBoundaryStatus::FailedClosed);
        assert!(
            !filesystem
                .source_primitives
                .contains(&BoundaryPrimitive::MountNamespace)
        );
    }

    #[test]
    fn old_root_and_fd_cleanup_stages_precede_exec_handoff() {
        let plan = plan_with_toolchain(PrivilegeStrategy::UnprivilegedUserNamespace);
        let mut backend = LinuxSupervisorBackend::new(FakeSyscalls::default());
        let attempt = run_supervisor_attempt(plan, &mut backend);
        let calls = &backend.syscalls.calls;

        assert!(!attempt.outcome.failed_closed());
        for required in [
            "detach_old_root",
            "assert_old_root_unreachable",
            "cleanup_fds",
        ] {
            assert!(
                calls.iter().position(|call| call == required)
                    < calls.iter().position(|call| call == "exec_handoff_ready"),
                "{required} did not precede exec"
            );
        }
    }

    #[test]
    fn isolation_supervisor_failure_matrix_maps_each_ordered_stage() {
        for (stage, operation) in [
            (SupervisorStage::UserNamespace, "unshare_user_namespace"),
            (SupervisorStage::SetgroupsDeny, "write_setgroups_deny"),
            (SupervisorStage::UidMap, "write_uid_map_current"),
            (SupervisorStage::GidMap, "write_gid_map_current"),
            (SupervisorStage::PidNamespace, "enter_pid_namespace_child"),
            (
                SupervisorStage::NetworkNamespace,
                "unshare_network_namespace",
            ),
            (
                SupervisorStage::AssertNoVethOrDefaultRoute,
                "assert_no_veth_or_default_route",
            ),
            (SupervisorStage::MountNamespace, "unshare_mount_namespace"),
            (
                SupervisorStage::MountPropagationPrivate,
                "make_mounts_private",
            ),
            (SupervisorStage::TmpfsRoot, "mount_tmpfs_root"),
            (SupervisorStage::ProjectBind, "project_bind"),
            (
                SupervisorStage::ReadOnlyToolchainBind,
                "read_only_toolchain_bind",
            ),
            (SupervisorStage::ReadOnlyRemount, "remount_read_only"),
            (
                SupervisorStage::ReadOnlyBindScopeDecision,
                "record_read_only_bind_scope",
            ),
            (SupervisorStage::ScratchTmpfs, "scratch_tmpfs"),
            (SupervisorStage::ProcFs, "procfs"),
            (SupervisorStage::MinimalDevices, "minimal_devices"),
            (SupervisorStage::OldRootCleanup, "detach_old_root"),
            (
                SupervisorStage::SavedFdOldRootCheck,
                "assert_old_root_unreachable",
            ),
            (SupervisorStage::CwdMapping, "chdir"),
            (SupervisorStage::EnvCleanup, "cleanup_env"),
            (SupervisorStage::FdCleanup, "cleanup_fds"),
            (SupervisorStage::SetupReportPipe, "setup_report_pipe_ready"),
            (SupervisorStage::ExecHandoff, "exec_handoff_ready"),
        ] {
            let plan = plan_with_toolchain(PrivilegeStrategy::UnprivilegedUserNamespace);
            let mut backend = LinuxSupervisorBackend::new(FakeSyscalls::default().fail(operation));
            let attempt = run_supervisor_attempt(plan, &mut backend);
            let boundary = boundary_attempt_from_supervisor(&attempt);

            assert!(attempt.outcome.failed_closed(), "{}", stage.label());
            assert_eq!(
                attempt.outcome.report().failed_stage,
                Some(stage),
                "{}",
                stage.label()
            );
            assert_eq!(
                boundary.outcome.setup_failure_stage.as_deref(),
                Some(stage.label()),
                "{}",
                stage.label()
            );
            if stage != SupervisorStage::ExecHandoff {
                assert!(
                    !backend
                        .syscalls
                        .calls
                        .iter()
                        .any(|call| call == "exec_handoff_ready"),
                    "{} reached exec after failure",
                    stage.label()
                );
            }
        }
    }

    #[test]
    fn landlock_runs_after_mount_view_and_before_exec_handoff() {
        let plan = plan_with_toolchain(PrivilegeStrategy::DirectCapSysAdmin);
        let mut backend = LinuxSupervisorBackend::new(FakeSyscalls::direct());
        let attempt = run_supervisor_attempt(plan, &mut backend);
        let boundary = boundary_attempt_from_supervisor(&attempt);
        let calls = &backend.syscalls.calls;

        assert!(!attempt.outcome.failed_closed());
        let old_root = calls
            .iter()
            .position(|call| call == "assert_old_root_unreachable")
            .unwrap();
        let cwd = calls
            .iter()
            .position(|call| call.starts_with("chdir:"))
            .unwrap();
        let restrict = calls
            .iter()
            .position(|call| call == "landlock_restrict_self")
            .unwrap();
        let exec = calls
            .iter()
            .position(|call| call == "exec_handoff_ready")
            .unwrap();
        assert!(old_root < restrict);
        assert!(cwd < restrict);
        assert!(restrict < exec);
        assert_eq!(
            boundary.outcome.landlock_status,
            LandlockApplicationStatus::Applied
        );
        assert_eq!(boundary.outcome.landlock_abi_version, Some(3));
        assert!(boundary.outcome.landlock_rights_mask.is_some());
        assert!(
            boundary
                .outcome
                .evidence
                .iter()
                .any(|evidence| matches!(evidence, AppliedBoundaryEvidence::Landlock(_)))
        );

        let config = CommandIsolationRuntimeConfig {
            mode: crate::enforcement::isolation::CommandIsolationMode::Enforce,
            required_boundaries: vec![BoundaryDimension::Landlock],
            ..CommandIsolationRuntimeConfig::default()
        };
        let preflight = crate::enforcement::isolation::evaluate_command_boundary_attempt(
            &config,
            Some(&boundary),
        );
        let landlock = preflight
            .effective_boundary
            .status_for(BoundaryDimension::Landlock)
            .unwrap();
        assert_eq!(landlock.status, EffectiveBoundaryStatus::PhysicallyBounded);
        assert_eq!(
            landlock.source_primitives,
            vec![BoundaryPrimitive::Landlock]
        );
    }

    #[test]
    fn required_landlock_enforce_fails_closed_on_unsupported_or_not_applied() {
        for syscalls in [
            FakeSyscalls::direct().with_landlock_abi(None),
            FakeSyscalls::direct().fail("landlock_restrict_self"),
        ] {
            let plan = require_landlock(plan_with_toolchain(PrivilegeStrategy::DirectCapSysAdmin));
            let mut backend = LinuxSupervisorBackend::new(syscalls);
            let attempt = run_supervisor_attempt(plan, &mut backend);
            let boundary = boundary_attempt_from_supervisor(&attempt);

            assert!(attempt.outcome.failed_closed());
            assert_eq!(
                attempt.outcome.report().failed_stage,
                Some(SupervisorStage::Landlock)
            );
            assert!(matches!(
                boundary.outcome.landlock_status,
                LandlockApplicationStatus::Unsupported | LandlockApplicationStatus::NotApplied
            ));
            assert_eq!(boundary.outcome.landlock_required, Some(true));
            assert!(
                !backend
                    .syscalls
                    .calls
                    .iter()
                    .any(|call| call == "exec_handoff_ready")
            );
        }
    }

    #[test]
    fn optional_landlock_failure_keeps_enforce_setup_honest_and_continues() {
        let plan = plan_with_toolchain(PrivilegeStrategy::DirectCapSysAdmin);
        let mut backend =
            LinuxSupervisorBackend::new(FakeSyscalls::direct().fail("landlock_restrict_self"));
        let attempt = run_supervisor_attempt(plan, &mut backend);
        let boundary = boundary_attempt_from_supervisor(&attempt);

        assert!(!attempt.outcome.failed_closed());
        assert_eq!(
            boundary.outcome.landlock_status,
            LandlockApplicationStatus::NotApplied
        );
        assert_eq!(boundary.outcome.landlock_required, Some(false));
        assert!(
            backend
                .syscalls
                .calls
                .iter()
                .any(|call| call == "exec_handoff_ready")
        );
        assert!(
            !boundary
                .outcome
                .evidence
                .iter()
                .any(|evidence| matches!(evidence, AppliedBoundaryEvidence::Landlock(_)))
        );
    }

    #[test]
    fn landlock_temporary_fds_are_cloexec_and_closed_before_exec() {
        let plan = plan_with_toolchain(PrivilegeStrategy::DirectCapSysAdmin);
        let mut backend = LinuxSupervisorBackend::new(FakeSyscalls::direct());
        let attempt = run_supervisor_attempt(plan, &mut backend);
        let calls = &backend.syscalls.calls;

        assert!(!attempt.outcome.failed_closed());
        let exec = calls
            .iter()
            .position(|call| call == "exec_handoff_ready")
            .unwrap();
        for required in [
            "landlock_ruleset_fd_cloexec",
            "landlock_path_fd_cloexec",
            "landlock_path_fd_closed",
            "landlock_ruleset_fd_closed",
        ] {
            let position = calls.iter().position(|call| call == required).unwrap();
            assert!(position < exec, "{required} did not occur before exec");
        }
    }

    #[test]
    fn landlock_off_reports_not_applied_not_unsupported() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("project");
        touch(&root.join("bin/run"));
        let config = CommandIsolationRuntimeConfig {
            mode: crate::enforcement::isolation::CommandIsolationMode::Enforce,
            optional_boundaries: vec![BoundaryDimension::Landlock],
            landlock: crate::enforcement::isolation::CommandIsolationLandlockBehavior::Off,
            ..CommandIsolationRuntimeConfig::default()
        };
        let plan = SupervisorPlan::build(&root, None, &["run".to_string()], &config)
            .unwrap()
            .with_privilege_strategy(PrivilegeStrategy::DirectCapSysAdmin);
        let mut backend = LinuxSupervisorBackend::new(FakeSyscalls::direct());
        let attempt = run_supervisor_attempt(plan, &mut backend);
        let boundary = boundary_attempt_from_supervisor(&attempt);

        assert!(
            !attempt
                .outcome
                .report()
                .stage_applied(SupervisorStage::Landlock)
        );
        assert_eq!(
            boundary.outcome.landlock_status,
            LandlockApplicationStatus::NotApplied
        );
        assert_eq!(
            boundary.outcome.landlock_ruleset_status.as_deref(),
            Some("disabled")
        );
        assert!(
            !backend
                .syscalls
                .calls
                .iter()
                .any(|call| call == "landlock_abi_probe")
        );
    }

    #[test]
    fn isolation_supervisor_direct_privilege_failure_stops_before_netns() {
        let plan = plan_with_toolchain(PrivilegeStrategy::DirectCapSysAdmin);
        let mut backend = LinuxSupervisorBackend::new(FakeSyscalls::default());
        let attempt = run_supervisor_attempt(plan, &mut backend);
        let boundary = boundary_attempt_from_supervisor(&attempt);

        assert!(attempt.outcome.failed_closed());
        assert_eq!(
            attempt.outcome.report().failed_stage,
            Some(SupervisorStage::PrivilegeCheck)
        );
        assert!(
            !backend
                .syscalls
                .calls
                .iter()
                .any(|call| call == "unshare_network_namespace")
        );
        assert_eq!(
            boundary.outcome.privilege_strategy.as_deref(),
            Some("direct_cap_sys_admin")
        );
    }

    #[test]
    fn command_boundary_network_failure_never_gets_credit_and_audit_fallback_is_explicit() {
        let plan = plan_with_toolchain(PrivilegeStrategy::UnprivilegedUserNamespace);
        let mut backend =
            LinuxSupervisorBackend::new(FakeSyscalls::default().fail("unshare_network_namespace"));
        let attempt = run_supervisor_attempt(plan, &mut backend);
        let boundary = boundary_attempt_from_supervisor(&attempt);
        let host_probe =
            lancet_protocol::HostProbeReport::from_entries([lancet_protocol::HostProbeEntry::new(
                BoundaryPrimitive::NetworkNamespace,
                lancet_protocol::HostProbeStatus::Available,
                "host can create netns but setup failed",
            )]);

        let enforce = CommandIsolationRuntimeConfig {
            mode: crate::enforcement::isolation::CommandIsolationMode::Enforce,
            required_boundaries: vec![BoundaryDimension::Network],
            host_probe: Some(host_probe.clone()),
            ..CommandIsolationRuntimeConfig::default()
        };
        let audit = CommandIsolationRuntimeConfig {
            mode: crate::enforcement::isolation::CommandIsolationMode::Audit,
            optional_boundaries: vec![BoundaryDimension::Network],
            unsupported_host:
                crate::enforcement::isolation::CommandIsolationUnsupportedHostBehavior::AllowUnbounded,
            host_probe: Some(host_probe),
            ..CommandIsolationRuntimeConfig::default()
        };

        let enforce_preflight = crate::enforcement::isolation::evaluate_command_boundary_attempt(
            &enforce,
            Some(&boundary),
        );
        let audit_preflight = crate::enforcement::isolation::evaluate_command_boundary_attempt(
            &audit,
            Some(&boundary),
        );
        let enforce_network = enforce_preflight
            .effective_boundary
            .status_for(BoundaryDimension::Network)
            .unwrap();
        let audit_network = audit_preflight
            .effective_boundary
            .status_for(BoundaryDimension::Network)
            .unwrap();

        assert_eq!(
            enforce_network.status,
            EffectiveBoundaryStatus::FailedClosed
        );
        assert!(enforce_network.source_primitives.is_empty());
        assert!(enforce_preflight.should_fail_closed());
        assert_eq!(audit_network.status, EffectiveBoundaryStatus::Unbounded);
        assert!(audit_network.source_primitives.is_empty());
        assert!(!audit_preflight.should_fail_closed());
    }

    #[test]
    fn isolation_supervisor_no_helper_exec_or_external_wrapper_in_setup_calls() {
        let plan = plan_with_toolchain(PrivilegeStrategy::UnprivilegedUserNamespace);
        let mut backend = LinuxSupervisorBackend::new(FakeSyscalls::default());
        let attempt = run_supervisor_attempt(plan, &mut backend);

        assert!(!attempt.outcome.failed_closed());
        let calls = backend.syscalls.calls.join("\n");
        for forbidden in ["helper", "shell", "docker", "bubblewrap", "bwrap", "nsjail"] {
            assert!(
                !calls.contains(forbidden),
                "forbidden setup wrapper appeared: {forbidden}"
            );
        }
    }
}

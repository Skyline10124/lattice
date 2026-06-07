// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Overlay session lifecycle contracts for command execution.

use std::path::{Path, PathBuf};

use super::artifact::ArtifactManifest;
use super::syscall::{SupervisorSyscalls, SyscallError};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OverlayMode {
    Diagnostic,
    Artifact,
    CandidateVerify,
}

impl OverlayMode {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            OverlayMode::Diagnostic => "diagnostic",
            OverlayMode::Artifact => "artifact",
            OverlayMode::CandidateVerify => "candidate_verify",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OverlayStatus {
    NotRequested,
    Applied,
    NotApplied,
    SetupFailed,
}

impl OverlayStatus {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            OverlayStatus::NotRequested => "not_requested",
            OverlayStatus::Applied => "applied",
            OverlayStatus::NotApplied => "not_applied",
            OverlayStatus::SetupFailed => "setup_failed",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OverlayDisposition {
    Discarded,
    Committed,
    Denied,
}

impl OverlayDisposition {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            OverlayDisposition::Discarded => "discarded",
            OverlayDisposition::Committed => "committed",
            OverlayDisposition::Denied => "denied",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OverlaySessionPaths {
    pub(crate) lower: PathBuf,
    pub(crate) upper: PathBuf,
    pub(crate) work: PathBuf,
    pub(crate) merged: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OverlayPlan {
    pub(crate) mode: OverlayMode,
    pub(crate) paths: OverlaySessionPaths,
    pub(crate) session_root: PathBuf,
}

impl OverlayPlan {
    pub(crate) fn deterministic(
        project_root: &Path,
        mode: OverlayMode,
        session_id: &str,
    ) -> Result<Self, OverlayPlanError> {
        let lower = std::fs::canonicalize(project_root).map_err(|err| OverlayPlanError {
            field: "project_root",
            reason: format!("cannot canonicalize project root: {err}"),
        })?;
        let parent = lower.parent().ok_or_else(|| OverlayPlanError {
            field: "project_root",
            reason: "project root has no parent directory".to_string(),
        })?;
        let session_root = parent.join(".lattice-overlays").join(format!(
            "{}-{}",
            mode.label(),
            sanitize_session_id(session_id)
        ));
        let paths = OverlaySessionPaths {
            lower,
            upper: session_root.join("upper"),
            work: session_root.join("work"),
            merged: session_root.join("merged"),
        };
        let plan = Self {
            mode,
            paths,
            session_root,
        };
        plan.validate()?;
        Ok(plan)
    }

    pub(crate) fn validate(&self) -> Result<(), OverlayPlanError> {
        for (field, path) in [
            ("upper", &self.paths.upper),
            ("work", &self.paths.work),
            ("merged", &self.paths.merged),
        ] {
            if path == &self.paths.lower || path.starts_with(&self.paths.lower) {
                return Err(OverlayPlanError {
                    field,
                    reason: "overlay path must be outside the lower project tree".to_string(),
                });
            }
            if path.parent().is_none() {
                return Err(OverlayPlanError {
                    field,
                    reason: "overlay path must be parent-visible".to_string(),
                });
            }
        }
        if self.paths.upper.parent() != self.paths.work.parent() {
            return Err(OverlayPlanError {
                field: "work",
                reason: "workdir must share a parent filesystem with upperdir".to_string(),
            });
        }
        Ok(())
    }

    pub(crate) fn project_mount_source(&self) -> &Path {
        &self.paths.merged
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OverlayPlanError {
    pub(crate) field: &'static str,
    pub(crate) reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OverlaySetupReport {
    pub(crate) mode: OverlayMode,
    pub(crate) status: OverlayStatus,
    pub(crate) paths: OverlaySessionPaths,
    pub(crate) error: Option<String>,
}

impl OverlaySetupReport {
    pub(crate) fn applied(plan: &OverlayPlan) -> Self {
        Self {
            mode: plan.mode,
            status: OverlayStatus::Applied,
            paths: plan.paths.clone(),
            error: None,
        }
    }

    pub(crate) fn setup_failed(plan: &OverlayPlan, error: impl Into<String>) -> Self {
        Self {
            mode: plan.mode,
            status: OverlayStatus::SetupFailed,
            paths: plan.paths.clone(),
            error: Some(error.into()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CleanupOutcome {
    Succeeded,
    Failed,
}

impl CleanupOutcome {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            CleanupOutcome::Succeeded => "cleanup_succeeded",
            CleanupOutcome::Failed => "cleanup_failed",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CleanupFailureKind {
    ChildCleanup,
    UnmountMerged,
    RemoveMerged,
    RemoveUpper,
    RemoveWork,
    RemoveSessionRoot,
}

impl CleanupFailureKind {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            CleanupFailureKind::ChildCleanup => "child_cleanup",
            CleanupFailureKind::UnmountMerged => "unmount_merged",
            CleanupFailureKind::RemoveMerged => "remove_merged",
            CleanupFailureKind::RemoveUpper => "remove_upper",
            CleanupFailureKind::RemoveWork => "remove_work",
            CleanupFailureKind::RemoveSessionRoot => "remove_session_root",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OverlayCleanupFailure {
    pub(crate) failure_kind: CleanupFailureKind,
    pub(crate) path_label: &'static str,
    pub(crate) reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OverlayCleanupReport {
    pub(crate) mode: OverlayMode,
    pub(crate) disposition: OverlayDisposition,
    pub(crate) outcome: CleanupOutcome,
    pub(crate) failure_kind: Option<CleanupFailureKind>,
    pub(crate) path_label: Option<&'static str>,
    pub(crate) reason: Option<String>,
    pub(crate) cleanup_failures: Vec<OverlayCleanupFailure>,
    pub(crate) child_cleanup_succeeded: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CandidateArtifactView {
    pub(crate) root: PathBuf,
    pub(crate) artifact_paths: OverlaySessionPaths,
    pub(crate) manifest: ArtifactManifest,
}

impl CandidateArtifactView {
    pub(crate) fn manifest_paths(&self) -> Vec<String> {
        self.manifest.paths()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct VerifyDiscardPlan {
    pub(crate) overlay: OverlayPlan,
    pub(crate) candidate_root: PathBuf,
    pub(crate) manifest: ArtifactManifest,
}

impl VerifyDiscardPlan {
    pub(crate) fn deterministic(
        candidate: &CandidateArtifactView,
        session_id: &str,
    ) -> Result<Self, OverlayPlanError> {
        let mut overlay =
            OverlayPlan::deterministic(&candidate.root, OverlayMode::CandidateVerify, session_id)?;
        overlay.paths.lower = candidate.root.clone();
        overlay.validate()?;
        Ok(Self {
            overlay,
            candidate_root: candidate.root.clone(),
            manifest: candidate.manifest.clone(),
        })
    }

    pub(crate) fn merged_path(&self) -> &Path {
        &self.overlay.paths.merged
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VerifyLayerStatus {
    NotRun,
    Applied,
    Discarded,
    CleanupFailed,
}

impl VerifyLayerStatus {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            VerifyLayerStatus::NotRun => "not_run",
            VerifyLayerStatus::Applied => "applied",
            VerifyLayerStatus::Discarded => "discarded",
            VerifyLayerStatus::CleanupFailed => "cleanup_failed",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct VerifyDiscardReport {
    pub(crate) verify_layer: VerifyLayerStatus,
    pub(crate) cleanup_outcome: Option<CleanupOutcome>,
    pub(crate) failure_kind: Option<CleanupFailureKind>,
    pub(crate) path_label: Option<&'static str>,
    pub(crate) reason: Option<String>,
    pub(crate) child_cleanup_succeeded: bool,
    pub(crate) contaminated_paths: Vec<String>,
}

impl VerifyDiscardReport {
    pub(crate) fn not_run() -> Self {
        Self {
            verify_layer: VerifyLayerStatus::NotRun,
            cleanup_outcome: None,
            failure_kind: None,
            path_label: None,
            reason: None,
            child_cleanup_succeeded: true,
            contaminated_paths: Vec::new(),
        }
    }

    pub(crate) fn applied() -> Self {
        Self {
            verify_layer: VerifyLayerStatus::Applied,
            cleanup_outcome: None,
            failure_kind: None,
            path_label: None,
            reason: None,
            child_cleanup_succeeded: true,
            contaminated_paths: Vec::new(),
        }
    }
}

pub(crate) fn prepare_candidate_artifact_view(
    artifact_plan: &OverlayPlan,
    accepted_manifest: &ArtifactManifest,
) -> CandidateArtifactView {
    CandidateArtifactView {
        root: artifact_plan.paths.merged.clone(),
        artifact_paths: artifact_plan.paths.clone(),
        manifest: accepted_manifest.clone(),
    }
}

pub(crate) fn cleanup_verify_discard_layer<S: SupervisorSyscalls>(
    plan: &VerifyDiscardPlan,
    child_cleanup_succeeded: bool,
    syscalls: &mut S,
) -> VerifyDiscardReport {
    let contaminated_paths = verify_contaminated_paths(&plan.overlay.paths.upper);
    let cleanup = cleanup_overlay(
        &plan.overlay,
        OverlayDisposition::Discarded,
        child_cleanup_succeeded,
        syscalls,
    );
    let verify_layer = match cleanup.outcome {
        CleanupOutcome::Succeeded => VerifyLayerStatus::Discarded,
        CleanupOutcome::Failed => VerifyLayerStatus::CleanupFailed,
    };
    VerifyDiscardReport {
        verify_layer,
        cleanup_outcome: Some(cleanup.outcome),
        failure_kind: cleanup.failure_kind,
        path_label: cleanup.path_label,
        reason: cleanup.reason,
        child_cleanup_succeeded: cleanup.child_cleanup_succeeded,
        contaminated_paths,
    }
}

fn verify_contaminated_paths(upper: &Path) -> Vec<String> {
    let mut paths = Vec::new();
    collect_contaminated_paths(upper, upper, &mut paths);
    paths.sort();
    paths
}

fn collect_contaminated_paths(base: &Path, current: &Path, paths: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(current) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if let Ok(relative) = path.strip_prefix(base) {
            paths.push(relative.to_string_lossy().replace('\\', "/"));
        }
        if entry.file_type().is_ok_and(|file_type| file_type.is_dir()) {
            collect_contaminated_paths(base, &path, paths);
        }
    }
}

pub(crate) fn prepare_overlay_dirs(plan: &OverlayPlan) -> Result<(), SyscallError> {
    for (operation, path) in [
        ("mkdir(overlay upper)", &plan.paths.upper),
        ("mkdir(overlay work)", &plan.paths.work),
        ("mkdir(overlay merged)", &plan.paths.merged),
    ] {
        std::fs::create_dir_all(path)
            .map_err(|err| SyscallError::new(operation, err.to_string()))?;
    }
    Ok(())
}

pub(crate) fn mount_overlay<S: SupervisorSyscalls>(
    plan: &OverlayPlan,
    syscalls: &mut S,
) -> Result<OverlaySetupReport, SyscallError> {
    prepare_overlay_dirs(plan)?;
    match syscalls.mount_overlay(
        &plan.paths.lower,
        &plan.paths.upper,
        &plan.paths.work,
        &plan.paths.merged,
    ) {
        Ok(()) => Ok(OverlaySetupReport::applied(plan)),
        Err(err) => {
            let _ = OverlaySetupReport::setup_failed(plan, err.to_string());
            Err(err)
        }
    }
}

pub(crate) fn cleanup_overlay<S: SupervisorSyscalls>(
    plan: &OverlayPlan,
    disposition: OverlayDisposition,
    child_cleanup_succeeded: bool,
    syscalls: &mut S,
) -> OverlayCleanupReport {
    let mut failures = Vec::new();
    if !child_cleanup_succeeded {
        failures.push(OverlayCleanupFailure {
            failure_kind: CleanupFailureKind::ChildCleanup,
            path_label: "child",
            reason: "child kill/reap cleanup failed".to_string(),
        });
    }

    if let Err(err) = syscalls.umount_overlay(&plan.paths.merged) {
        if is_not_mounted_overlay_unmount(&err) {
            return cleanup_overlay_dirs(
                plan,
                disposition,
                child_cleanup_succeeded,
                syscalls,
                failures,
            );
        }
        failures.push(OverlayCleanupFailure {
            failure_kind: CleanupFailureKind::UnmountMerged,
            path_label: "merged",
            reason: err.to_string(),
        });
    }

    cleanup_overlay_dirs(
        plan,
        disposition,
        child_cleanup_succeeded,
        syscalls,
        failures,
    )
}

fn cleanup_overlay_dirs<S: SupervisorSyscalls>(
    plan: &OverlayPlan,
    disposition: OverlayDisposition,
    child_cleanup_succeeded: bool,
    syscalls: &mut S,
    mut failures: Vec<OverlayCleanupFailure>,
) -> OverlayCleanupReport {
    for (kind, label, path) in [
        (
            CleanupFailureKind::RemoveMerged,
            "merged",
            &plan.paths.merged,
        ),
        (CleanupFailureKind::RemoveUpper, "upper", &plan.paths.upper),
        (CleanupFailureKind::RemoveWork, "work", &plan.paths.work),
        (
            CleanupFailureKind::RemoveSessionRoot,
            "session_root",
            &plan.session_root,
        ),
    ] {
        if let Err(err) = syscalls.remove_overlay_tree(path) {
            failures.push(OverlayCleanupFailure {
                failure_kind: kind,
                path_label: label,
                reason: err.to_string(),
            });
        }
    }

    if let Some(first_failure) = failures.first() {
        OverlayCleanupReport {
            mode: plan.mode,
            disposition,
            outcome: CleanupOutcome::Failed,
            failure_kind: Some(first_failure.failure_kind),
            path_label: Some(first_failure.path_label),
            reason: Some(first_failure.reason.clone()),
            cleanup_failures: failures,
            child_cleanup_succeeded,
        }
    } else {
        OverlayCleanupReport {
            mode: plan.mode,
            disposition,
            outcome: CleanupOutcome::Succeeded,
            failure_kind: None,
            path_label: None,
            reason: None,
            cleanup_failures: Vec::new(),
            child_cleanup_succeeded,
        }
    }
}

fn is_not_mounted_overlay_unmount(err: &SyscallError) -> bool {
    if err.operation != "umount2(overlay merged)" {
        return false;
    }
    #[cfg(target_os = "linux")]
    {
        matches!(err.errno, Some(libc::EINVAL | libc::ENOENT))
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

fn sanitize_session_id(session_id: &str) -> String {
    let sanitized = session_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();
    if sanitized.is_empty() {
        "session".to_string()
    } else {
        sanitized
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::supervisor::artifact::{
        ArtifactEntry, ArtifactManifest, ArtifactOperation, RegularArtifactEntry,
    };
    use std::collections::BTreeSet;
    use tempfile::TempDir;

    #[derive(Default)]
    struct FakeOverlaySyscalls {
        calls: Vec<String>,
        fail: BTreeSet<&'static str>,
        unmount_errno: Option<i32>,
    }

    impl FakeOverlaySyscalls {
        fn fail(mut self, operation: &'static str) -> Self {
            self.fail.insert(operation);
            self
        }

        fn fail_unmount_errno(mut self, errno: i32) -> Self {
            self.unmount_errno = Some(errno);
            self
        }

        fn maybe_fail(&self, operation: &'static str) -> Result<(), SyscallError> {
            if self.fail.contains(operation) {
                Err(SyscallError::new(operation, "injected failure"))
            } else {
                Ok(())
            }
        }
    }

    impl SupervisorSyscalls for FakeOverlaySyscalls {
        fn has_cap_sys_admin(&mut self) -> Result<bool, SyscallError> {
            Ok(true)
        }
        fn check_unprivileged_user_namespace(&mut self) -> Result<(), SyscallError> {
            Ok(())
        }
        fn unshare_user_namespace(&mut self) -> Result<(), SyscallError> {
            Ok(())
        }
        fn write_setgroups_deny(&mut self) -> Result<(), SyscallError> {
            Ok(())
        }
        fn write_uid_map_current(&mut self) -> Result<(), SyscallError> {
            Ok(())
        }
        fn write_gid_map_current(&mut self) -> Result<(), SyscallError> {
            Ok(())
        }
        fn enter_pid_namespace_child(&mut self) -> Result<(), SyscallError> {
            Ok(())
        }
        fn unshare_network_namespace(&mut self) -> Result<(), SyscallError> {
            Ok(())
        }
        fn assert_no_veth_or_default_route(&mut self) -> Result<(), SyscallError> {
            Ok(())
        }
        fn unshare_mount_namespace(&mut self) -> Result<(), SyscallError> {
            Ok(())
        }
        fn make_mounts_private(&mut self) -> Result<(), SyscallError> {
            Ok(())
        }
        fn mount_tmpfs_root(&mut self) -> Result<(), SyscallError> {
            Ok(())
        }
        fn bind_mount(&mut self, _source: &Path, _target: &Path) -> Result<(), SyscallError> {
            Ok(())
        }
        fn remount_read_only(&mut self, _target: &Path) -> Result<(), SyscallError> {
            Ok(())
        }
        fn record_read_only_bind_scope(
            &mut self,
            _scope: super::super::plan::ReadOnlyBindScope,
        ) -> Result<(), SyscallError> {
            Ok(())
        }
        fn mount_scratch_tmpfs(&mut self, _target: &Path) -> Result<(), SyscallError> {
            Ok(())
        }
        fn mount_procfs(&mut self, _target: &Path) -> Result<(), SyscallError> {
            Ok(())
        }
        fn mount_minimal_device(
            &mut self,
            _source: &Path,
            _target: &Path,
        ) -> Result<(), SyscallError> {
            Ok(())
        }
        fn detach_old_root(&mut self) -> Result<(), SyscallError> {
            Ok(())
        }
        fn assert_old_root_unreachable(&mut self) -> Result<(), SyscallError> {
            Ok(())
        }
        fn chdir(&mut self, _target: &Path) -> Result<(), SyscallError> {
            Ok(())
        }
        fn cleanup_env(&mut self) -> Result<(), SyscallError> {
            Ok(())
        }
        fn cleanup_fds(&mut self) -> Result<(), SyscallError> {
            Ok(())
        }
        fn probe_landlock_abi(
            &mut self,
        ) -> Result<super::super::landlock::LandlockAbiProbe, SyscallError> {
            Ok(super::super::landlock::LandlockAbiProbe::unsupported(
                "not tested",
            ))
        }
        fn apply_landlock(
            &mut self,
            _plan: &super::super::landlock::LandlockRefinementPlan,
        ) -> Result<super::super::landlock::LandlockApplicationReport, SyscallError> {
            Err(SyscallError::new("landlock", "not tested"))
        }
        fn setup_report_pipe_ready(&mut self) -> Result<(), SyscallError> {
            Ok(())
        }
        fn exec_handoff_ready(&mut self) -> Result<(), SyscallError> {
            Ok(())
        }
        fn mount_overlay(
            &mut self,
            lower: &Path,
            upper: &Path,
            work: &Path,
            merged: &Path,
        ) -> Result<(), SyscallError> {
            self.calls.push(format!(
                "mount_overlay:{}:{}:{}:{}",
                lower.display(),
                upper.display(),
                work.display(),
                merged.display()
            ));
            self.maybe_fail("mount_overlay")
        }
        fn umount_overlay(&mut self, merged: &Path) -> Result<(), SyscallError> {
            self.calls
                .push(format!("umount_overlay:{}", merged.display()));
            if let Some(errno) = self.unmount_errno {
                return Err(SyscallError::with_errno(
                    "umount2(overlay merged)",
                    "not mounted",
                    errno,
                ));
            }
            self.maybe_fail("umount_overlay")
        }
        fn remove_overlay_tree(&mut self, path: &Path) -> Result<(), SyscallError> {
            self.calls
                .push(format!("remove_overlay_tree:{}", path.display()));
            if path.ends_with("upper") {
                self.maybe_fail("remove_upper")
            } else if path.ends_with("work") {
                self.maybe_fail("remove_work")
            } else if path.ends_with("merged") {
                self.maybe_fail("remove_merged")
            } else {
                self.maybe_fail("remove_session_root")
            }
        }
    }

    fn overlay_plan(mode: OverlayMode) -> (TempDir, OverlayPlan) {
        let temp = TempDir::new().unwrap();
        let project = temp.path().join("project");
        std::fs::create_dir(&project).unwrap();
        let plan = OverlayPlan::deterministic(&project, mode, "turn-1").unwrap();
        (temp, plan)
    }

    fn accepted_manifest() -> ArtifactManifest {
        ArtifactManifest {
            entries: vec![ArtifactEntry::Regular(RegularArtifactEntry {
                path: "src/generated.txt".to_string(),
                operation: ArtifactOperation::Create,
                content_hash: "blake3:candidate".to_string(),
                size_bytes: 9,
                mode: 0o644,
                lower_hash: None,
            })],
        }
    }

    #[test]
    fn overlay_plan_paths_are_deterministic_parent_visible_and_outside_lower() {
        for mode in [
            OverlayMode::Diagnostic,
            OverlayMode::Artifact,
            OverlayMode::CandidateVerify,
        ] {
            let (_temp, plan) = overlay_plan(mode);

            assert_eq!(plan.mode, mode);
            assert!(plan
                .session_root
                .ends_with(format!("{}-turn-1", mode.label())));
            for path in [&plan.paths.upper, &plan.paths.work, &plan.paths.merged] {
                assert!(!path.starts_with(&plan.paths.lower), "{}", path.display());
                assert!(path.parent().is_some(), "{}", path.display());
            }
            assert_eq!(plan.paths.upper.parent(), plan.paths.work.parent());
            assert_eq!(plan.project_mount_source(), plan.paths.merged);
        }
    }

    #[test]
    fn overlay_plan_sanitizes_session_ids() {
        let temp = TempDir::new().unwrap();
        let project = temp.path().join("project");
        std::fs::create_dir(&project).unwrap();
        let plan =
            OverlayPlan::deterministic(&project, OverlayMode::Diagnostic, "turn/one").unwrap();

        assert!(plan.session_root.ends_with("diagnostic-turn-one"));
    }

    #[test]
    fn overlay_plan_mount_receives_lower_upper_work_merged_in_order() {
        let (_temp, plan) = overlay_plan(OverlayMode::Artifact);
        let mut syscalls = FakeOverlaySyscalls::default();

        let report = mount_overlay(&plan, &mut syscalls).unwrap();

        assert_eq!(report.status, OverlayStatus::Applied);
        assert_eq!(
            syscalls.calls,
            vec![format!(
                "mount_overlay:{}:{}:{}:{}",
                plan.paths.lower.display(),
                plan.paths.upper.display(),
                plan.paths.work.display(),
                plan.paths.merged.display()
            )]
        );
    }

    #[test]
    fn overlay_cleanup_records_unmount_failure() {
        let (_temp, plan) = overlay_plan(OverlayMode::Diagnostic);
        let mut syscalls = FakeOverlaySyscalls::default().fail("umount_overlay");

        let report = cleanup_overlay(&plan, OverlayDisposition::Discarded, true, &mut syscalls);

        assert_eq!(report.outcome, CleanupOutcome::Failed);
        assert_eq!(report.outcome.label(), "cleanup_failed");
        assert_eq!(report.failure_kind, Some(CleanupFailureKind::UnmountMerged));
        assert_eq!(report.path_label, Some("merged"));
        assert_eq!(report.cleanup_failures.len(), 1);
        assert_eq!(
            report.cleanup_failures[0].failure_kind,
            CleanupFailureKind::UnmountMerged
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn overlay_cleanup_ignores_parent_namespace_not_mounted_unmount() {
        let (_temp, plan) = overlay_plan(OverlayMode::Artifact);
        let mut syscalls = FakeOverlaySyscalls::default().fail_unmount_errno(libc::EINVAL);

        let report = cleanup_overlay(&plan, OverlayDisposition::Committed, true, &mut syscalls);

        assert_eq!(report.outcome, CleanupOutcome::Succeeded);
        assert_eq!(report.failure_kind, None);
        assert_eq!(
            syscalls
                .calls
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec![
                format!("umount_overlay:{}", plan.paths.merged.display()),
                format!("remove_overlay_tree:{}", plan.paths.merged.display()),
                format!("remove_overlay_tree:{}", plan.paths.upper.display()),
                format!("remove_overlay_tree:{}", plan.paths.work.display()),
                format!("remove_overlay_tree:{}", plan.session_root.display()),
            ]
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn overlay_cleanup_live_parent_namespace_non_mount_removes_workdir_tree() {
        let (_temp, plan) = overlay_plan(OverlayMode::Artifact);
        std::fs::create_dir_all(&plan.paths.upper).unwrap();
        std::fs::create_dir_all(&plan.paths.merged).unwrap();
        let inner_work = plan.paths.work.join("work");
        std::fs::create_dir_all(&inner_work).unwrap();

        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&inner_work).unwrap().permissions();
        permissions.set_mode(0o0);
        std::fs::set_permissions(&inner_work, permissions).unwrap();

        let mut syscalls = crate::executor::supervisor::syscall::LiveSupervisorSyscalls::default();
        let report = cleanup_overlay(&plan, OverlayDisposition::Committed, true, &mut syscalls);

        assert_eq!(report.outcome, CleanupOutcome::Succeeded);
        assert!(!plan.session_root.exists());
    }

    #[test]
    fn overlay_cleanup_preserves_first_failure_and_records_all_failures() {
        let (_temp, plan) = overlay_plan(OverlayMode::Artifact);
        let mut syscalls = FakeOverlaySyscalls::default()
            .fail("umount_overlay")
            .fail("remove_upper");

        let report = cleanup_overlay(&plan, OverlayDisposition::Denied, false, &mut syscalls);

        assert_eq!(report.outcome, CleanupOutcome::Failed);
        assert_eq!(report.failure_kind, Some(CleanupFailureKind::ChildCleanup));
        assert_eq!(report.path_label, Some("child"));
        assert_eq!(report.cleanup_failures.len(), 3);
        assert_eq!(
            report
                .cleanup_failures
                .iter()
                .map(|failure| failure.failure_kind)
                .collect::<Vec<_>>(),
            vec![
                CleanupFailureKind::ChildCleanup,
                CleanupFailureKind::UnmountMerged,
                CleanupFailureKind::RemoveUpper,
            ]
        );
    }

    #[test]
    fn overlay_cleanup_records_directory_removal_failure_separately() {
        let (_temp, plan) = overlay_plan(OverlayMode::Artifact);
        let mut syscalls = FakeOverlaySyscalls::default().fail("remove_upper");

        let report = cleanup_overlay(&plan, OverlayDisposition::Denied, true, &mut syscalls);

        assert_eq!(report.outcome, CleanupOutcome::Failed);
        assert_eq!(report.failure_kind, Some(CleanupFailureKind::RemoveUpper));
        assert_eq!(report.path_label, Some("upper"));
    }

    #[test]
    fn overlay_cleanup_records_child_kill_reap_failure() {
        let (_temp, plan) = overlay_plan(OverlayMode::Diagnostic);
        let mut syscalls = FakeOverlaySyscalls::default();

        let report = cleanup_overlay(&plan, OverlayDisposition::Discarded, false, &mut syscalls);

        assert_eq!(report.outcome, CleanupOutcome::Failed);
        assert_eq!(report.failure_kind, Some(CleanupFailureKind::ChildCleanup));
        assert_eq!(report.path_label, Some("child"));
        assert!(!report.child_cleanup_succeeded);
    }

    #[test]
    fn verify_discard_layer_paths_are_distinct_from_artifact_layer_and_project_root() {
        let (_temp, artifact_plan) = overlay_plan(OverlayMode::Artifact);
        std::fs::create_dir_all(&artifact_plan.paths.merged).unwrap();
        let manifest = accepted_manifest();
        let candidate = prepare_candidate_artifact_view(&artifact_plan, &manifest);

        let verify_plan = VerifyDiscardPlan::deterministic(&candidate, "verify-1").unwrap();

        assert_eq!(candidate.root, artifact_plan.paths.merged);
        assert_eq!(verify_plan.overlay.mode, OverlayMode::CandidateVerify);
        assert_eq!(verify_plan.candidate_root, candidate.root);
        for verify_path in [
            &verify_plan.overlay.paths.upper,
            &verify_plan.overlay.paths.work,
            &verify_plan.overlay.paths.merged,
        ] {
            assert_ne!(verify_path, &artifact_plan.paths.upper);
            assert_ne!(verify_path, &artifact_plan.paths.work);
            assert_ne!(verify_path, &artifact_plan.paths.merged);
            assert_ne!(verify_path, &artifact_plan.paths.lower);
            assert!(!verify_path.starts_with(&artifact_plan.paths.lower));
        }
        assert_eq!(
            verify_plan.overlay.project_mount_source(),
            verify_plan.merged_path()
        );
    }

    #[test]
    fn cleanup_verify_discard_layer_discards_writes_for_all_verify_terminal_paths() {
        let (_temp, artifact_plan) = overlay_plan(OverlayMode::Artifact);
        std::fs::create_dir_all(&artifact_plan.paths.merged).unwrap();
        let candidate = prepare_candidate_artifact_view(&artifact_plan, &accepted_manifest());
        let terminal_paths = ["pass", "fail", "timeout", "cancellation"];

        for terminal_path in terminal_paths {
            let verify_plan = VerifyDiscardPlan::deterministic(&candidate, terminal_path).unwrap();
            let mut syscalls = FakeOverlaySyscalls::default();

            let report = cleanup_verify_discard_layer(&verify_plan, true, &mut syscalls);

            assert_eq!(
                report.verify_layer,
                VerifyLayerStatus::Discarded,
                "{terminal_path}"
            );
            assert_eq!(report.verify_layer.label(), "discarded");
            assert_eq!(report.cleanup_outcome, Some(CleanupOutcome::Succeeded));
            assert!(report.failure_kind.is_none());
        }
    }

    #[test]
    fn cleanup_verify_discard_layer_reports_cleanup_failure_as_commit_blocking_metadata() {
        let (_temp, artifact_plan) = overlay_plan(OverlayMode::Artifact);
        std::fs::create_dir_all(&artifact_plan.paths.merged).unwrap();
        let candidate = prepare_candidate_artifact_view(&artifact_plan, &accepted_manifest());
        let verify_plan = VerifyDiscardPlan::deterministic(&candidate, "verify-fail").unwrap();
        let mut syscalls = FakeOverlaySyscalls::default().fail("remove_upper");

        let report = cleanup_verify_discard_layer(&verify_plan, true, &mut syscalls);

        assert_eq!(report.verify_layer, VerifyLayerStatus::CleanupFailed);
        assert_eq!(report.verify_layer.label(), "cleanup_failed");
        assert_eq!(report.cleanup_outcome, Some(CleanupOutcome::Failed));
        assert_eq!(report.failure_kind, Some(CleanupFailureKind::RemoveUpper));
        assert_eq!(report.path_label, Some("upper"));
        assert!(report.reason.as_deref().unwrap_or_default().len() < 128);
    }

    #[test]
    fn verify_discard_layer_setup_and_cleanup_do_not_mutate_accepted_manifest() {
        let (_temp, artifact_plan) = overlay_plan(OverlayMode::Artifact);
        std::fs::create_dir_all(&artifact_plan.paths.merged).unwrap();
        let manifest = accepted_manifest();
        let manifest_before = manifest.clone();
        let candidate = prepare_candidate_artifact_view(&artifact_plan, &manifest);
        let verify_plan = VerifyDiscardPlan::deterministic(&candidate, "verify-immutable").unwrap();
        let mut syscalls = FakeOverlaySyscalls::default();

        let _report = cleanup_verify_discard_layer(&verify_plan, true, &mut syscalls);

        assert_eq!(manifest, manifest_before);
        assert_eq!(candidate.manifest, manifest_before);
        assert_eq!(verify_plan.manifest, manifest_before);
        assert_eq!(candidate.manifest_paths(), vec!["src/generated.txt"]);
    }

    #[test]
    fn verify_discard_layer_metadata_is_compact_and_content_free() {
        let not_run = VerifyDiscardReport::not_run();
        let applied = VerifyDiscardReport::applied();

        assert_eq!(not_run.verify_layer.label(), "not_run");
        assert_eq!(applied.verify_layer.label(), "applied");
        assert!(not_run.reason.is_none());
        assert!(applied.reason.is_none());

        let (_temp, artifact_plan) = overlay_plan(OverlayMode::Artifact);
        std::fs::create_dir_all(&artifact_plan.paths.merged).unwrap();
        let candidate = prepare_candidate_artifact_view(&artifact_plan, &accepted_manifest());
        let verify_plan = VerifyDiscardPlan::deterministic(&candidate, "verify-compact").unwrap();
        let mut syscalls = FakeOverlaySyscalls::default().fail("umount_overlay");

        let report = cleanup_verify_discard_layer(&verify_plan, true, &mut syscalls);

        assert_eq!(report.verify_layer.label(), "cleanup_failed");
        assert_eq!(report.path_label, Some("merged"));
        assert!(report.reason.as_deref().unwrap_or("").contains("failure"));
        assert!(!format!("{report:?}").contains("blake3:candidate"));
        assert!(!format!("{report:?}").contains("src/generated.txt"));
    }
}

//! Deterministic parent-built supervisor plan for isolated commands.

use std::path::{Path, PathBuf};

use lancet_protocol::error::{CepErrorData, ValidationErrorData};
use lancet_protocol::{CepError, CepErrorCode, Retriability};

use crate::enforcement::isolation::CommandIsolationRuntimeConfig;
use crate::executor::shell::resolve_command_cwd;
use crate::executor::supervisor::landlock::{
    landlock_fail_closed, landlock_required, landlock_stage_enabled,
};
use crate::executor::supervisor::overlay::OverlayPlan;

pub(crate) const PROJECT_VIEW_PATH: &str = "/project";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PrivilegeStrategy {
    DirectCapSysAdmin,
    UnprivilegedUserNamespace,
}

impl PrivilegeStrategy {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            PrivilegeStrategy::DirectCapSysAdmin => "direct_cap_sys_admin",
            PrivilegeStrategy::UnprivilegedUserNamespace => "unprivileged_user_namespace",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReadOnlyBindScope {
    TopLevelOnly,
    Recursive,
}

impl ReadOnlyBindScope {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            ReadOnlyBindScope::TopLevelOnly => "top_level",
            ReadOnlyBindScope::Recursive => "recursive",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MountKind {
    ProjectRoot,
    ReadOnlyToolchain,
    ScratchTmpfs,
    MinimalDevice,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MountPlan {
    pub(crate) kind: MountKind,
    pub(crate) source: Option<PathBuf>,
    pub(crate) target: PathBuf,
    pub(crate) read_only: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SupervisorPlanDiagnostic {
    pub(crate) key: String,
    pub(crate) value: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ExecutableResolution {
    pub(crate) argv0: String,
    pub(crate) host_path: PathBuf,
    pub(crate) view_path: PathBuf,
    pub(crate) source: ExecutableSource,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExecutableSource {
    Project,
    ReadOnlyToolchain,
}

/// Deterministic parent-built setup plan for one command attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SupervisorPlan {
    pub(crate) privilege_strategy: PrivilegeStrategy,
    pub(crate) network_namespace: bool,
    pub(crate) mount_namespace: bool,
    pub(crate) mount_view: bool,
    pub(crate) cwd_mapping: bool,
    pub(crate) env_cleanup: bool,
    pub(crate) fd_cleanup: bool,
    pub(crate) landlock: bool,
    pub(crate) landlock_required: bool,
    pub(crate) landlock_fail_closed: bool,
    pub(crate) setup_report_pipe: bool,
    pub(crate) exec_handoff: bool,
    pub(crate) project_host_root: PathBuf,
    pub(crate) project_view_path: PathBuf,
    pub(crate) cwd_host_path: PathBuf,
    pub(crate) cwd_view_path: PathBuf,
    pub(crate) mounts: Vec<MountPlan>,
    pub(crate) executable_view_paths: Vec<PathBuf>,
    pub(crate) generated_path_entries: Vec<PathBuf>,
    pub(crate) tmpfs_paths: Vec<PathBuf>,
    pub(crate) read_only_bind_scope: ReadOnlyBindScope,
    pub(crate) executable: ExecutableResolution,
    pub(crate) diagnostics: Vec<SupervisorPlanDiagnostic>,
    pub(crate) overlay_plan: Option<OverlayPlan>,
}

impl SupervisorPlan {
    pub(crate) fn build(
        project_root: &Path,
        cwd: Option<&str>,
        argv: &[String],
        config: &CommandIsolationRuntimeConfig,
    ) -> Result<Self, CepError> {
        if argv.is_empty() || argv[0].is_empty() {
            return Err(plan_error("argv[0]", "must name an executable"));
        }

        let project_host_root = std::fs::canonicalize(project_root).map_err(|err| {
            plan_error(
                "project_root",
                format!("cannot canonicalize project root: {err}"),
            )
        })?;
        let cwd_host_path = resolve_command_cwd(&project_host_root.to_string_lossy(), cwd)?;
        let project_view_path = PathBuf::from(PROJECT_VIEW_PATH);
        let cwd_view_path = map_project_path_to_view(&project_host_root, &cwd_host_path)?;

        let mut mounts = vec![MountPlan {
            kind: MountKind::ProjectRoot,
            source: Some(project_host_root.clone()),
            target: project_view_path.clone(),
            read_only: false,
        }];
        let mut diagnostics = Vec::new();
        let mut toolchain_sources = Vec::new();
        let mut generated_path_entries =
            vec![project_view_path.clone(), project_view_path.join("bin")];

        for (index, source) in config.read_only_toolchain_paths.iter().enumerate() {
            match std::fs::canonicalize(source) {
                Ok(canonical) if canonical.is_dir() || canonical.is_file() => {
                    let target = PathBuf::from(format!("/toolchain/ro{index}"));
                    mounts.push(MountPlan {
                        kind: MountKind::ReadOnlyToolchain,
                        source: Some(canonical.clone()),
                        target: target.clone(),
                        read_only: true,
                    });
                    generated_path_entries.push(target.clone());
                    if canonical.join("bin").is_dir() {
                        generated_path_entries.push(target.join("bin"));
                    }
                    toolchain_sources.push((canonical, target));
                }
                Ok(_) => diagnostics.push(SupervisorPlanDiagnostic {
                    key: format!("read_only_toolchain_paths.{index}"),
                    value: "not_mountable".to_string(),
                }),
                Err(err) => diagnostics.push(SupervisorPlanDiagnostic {
                    key: format!("read_only_toolchain_paths.{index}"),
                    value: format!("missing: {err}"),
                }),
            }
        }

        mounts.push(MountPlan {
            kind: MountKind::ScratchTmpfs,
            source: None,
            target: PathBuf::from("/tmp"),
            read_only: false,
        });
        for (index, scratch) in config.scratch_paths.iter().enumerate() {
            mounts.push(MountPlan {
                kind: MountKind::ScratchTmpfs,
                source: None,
                target: scratch_view_target(index, scratch),
                read_only: false,
            });
        }

        for device in ["/dev/null", "/dev/zero", "/dev/urandom"] {
            mounts.push(MountPlan {
                kind: MountKind::MinimalDevice,
                source: Some(PathBuf::from(device)),
                target: PathBuf::from(device),
                read_only: true,
            });
        }

        let executable = resolve_executable(
            &argv[0],
            &project_host_root,
            &cwd_host_path,
            &toolchain_sources,
        )?;
        let executable_view_paths = generated_path_entries.clone();
        let tmpfs_paths = mounts
            .iter()
            .filter(|mount| mount.kind == MountKind::ScratchTmpfs)
            .map(|mount| mount.target.clone())
            .collect();

        Ok(Self {
            privilege_strategy: PrivilegeStrategy::UnprivilegedUserNamespace,
            network_namespace: true,
            mount_namespace: true,
            mount_view: true,
            cwd_mapping: true,
            env_cleanup: true,
            fd_cleanup: true,
            landlock: landlock_stage_enabled(config),
            landlock_required: landlock_required(config),
            landlock_fail_closed: landlock_fail_closed(config),
            setup_report_pipe: true,
            exec_handoff: true,
            project_host_root,
            project_view_path,
            cwd_host_path,
            cwd_view_path,
            mounts,
            executable_view_paths,
            generated_path_entries,
            tmpfs_paths,
            read_only_bind_scope: ReadOnlyBindScope::TopLevelOnly,
            executable,
            diagnostics,
            overlay_plan: None,
        })
    }

    pub(crate) fn wave0_full_fake(project_view_path: PathBuf, cwd_view_path: PathBuf) -> Self {
        let executable = ExecutableResolution {
            argv0: "fake".to_string(),
            host_path: PathBuf::from("/fake/project/fake"),
            view_path: project_view_path.join("fake"),
            source: ExecutableSource::Project,
        };
        Self {
            privilege_strategy: PrivilegeStrategy::UnprivilegedUserNamespace,
            network_namespace: true,
            mount_namespace: true,
            mount_view: true,
            cwd_mapping: true,
            env_cleanup: true,
            fd_cleanup: true,
            landlock: true,
            landlock_required: true,
            landlock_fail_closed: true,
            setup_report_pipe: true,
            exec_handoff: true,
            project_host_root: PathBuf::from("/fake/project"),
            project_view_path,
            cwd_host_path: PathBuf::from("/fake/project"),
            cwd_view_path,
            mounts: Vec::new(),
            executable_view_paths: default_executable_view_paths(),
            generated_path_entries: default_executable_view_paths(),
            tmpfs_paths: vec![PathBuf::from("/tmp")],
            read_only_bind_scope: ReadOnlyBindScope::TopLevelOnly,
            executable,
            diagnostics: Vec::new(),
            overlay_plan: None,
        }
    }

    pub(crate) fn env_fd_hygiene(
        project_view_path: PathBuf,
        cwd_view_path: PathBuf,
        executable_view_paths: Vec<PathBuf>,
        tmpfs_paths: Vec<PathBuf>,
    ) -> Self {
        let executable = ExecutableResolution {
            argv0: "fake".to_string(),
            host_path: PathBuf::from("/fake/project/fake"),
            view_path: project_view_path.join("fake"),
            source: ExecutableSource::Project,
        };
        Self {
            privilege_strategy: PrivilegeStrategy::UnprivilegedUserNamespace,
            network_namespace: false,
            mount_namespace: false,
            mount_view: false,
            cwd_mapping: false,
            env_cleanup: true,
            fd_cleanup: true,
            landlock: false,
            landlock_required: false,
            landlock_fail_closed: false,
            setup_report_pipe: true,
            exec_handoff: true,
            project_host_root: PathBuf::from("/fake/project"),
            project_view_path,
            cwd_host_path: PathBuf::from("/fake/project"),
            cwd_view_path,
            mounts: Vec::new(),
            generated_path_entries: executable_view_paths.clone(),
            executable_view_paths,
            tmpfs_paths,
            read_only_bind_scope: ReadOnlyBindScope::TopLevelOnly,
            executable,
            diagnostics: Vec::new(),
            overlay_plan: None,
        }
    }

    pub(crate) fn with_privilege_strategy(mut self, strategy: PrivilegeStrategy) -> Self {
        self.privilege_strategy = strategy;
        self
    }

    pub(crate) fn with_overlay_plan(mut self, overlay_plan: OverlayPlan) -> Self {
        self.overlay_plan = Some(overlay_plan);
        self
    }

    pub(crate) fn project_mount_source(&self) -> &Path {
        self.overlay_plan
            .as_ref()
            .map(OverlayPlan::project_mount_source)
            .or_else(|| {
                self.mounts
                    .iter()
                    .find(|mount| mount.kind == MountKind::ProjectRoot)
                    .and_then(|mount| mount.source.as_deref())
            })
            .unwrap_or(&self.project_host_root)
    }

    pub(crate) fn read_only_toolchain_mounts(&self) -> impl Iterator<Item = &MountPlan> {
        self.mounts
            .iter()
            .filter(|mount| mount.kind == MountKind::ReadOnlyToolchain)
    }

    pub(crate) fn stages(&self) -> Vec<super::SupervisorStage> {
        use super::SupervisorStage;

        let mut stages = Vec::new();
        stages.push(SupervisorStage::PrivilegeCheck);
        if self.privilege_strategy == PrivilegeStrategy::UnprivilegedUserNamespace {
            stages.push(SupervisorStage::UserNamespace);
            stages.push(SupervisorStage::SetgroupsDeny);
            stages.push(SupervisorStage::UidMap);
            stages.push(SupervisorStage::GidMap);
        }
        if self.network_namespace {
            stages.push(SupervisorStage::NetworkNamespace);
            stages.push(SupervisorStage::AssertNoVethOrDefaultRoute);
        }
        if self.mount_namespace {
            stages.push(SupervisorStage::MountNamespace);
            stages.push(SupervisorStage::MountPropagationPrivate);
        }
        if self.overlay_plan.is_some() {
            stages.push(SupervisorStage::OverlayUpperWork);
            stages.push(SupervisorStage::OverlayMount);
        }
        if self.mount_view {
            stages.push(SupervisorStage::TmpfsRoot);
            stages.push(SupervisorStage::ProjectBind);
            if self.read_only_toolchain_mounts().next().is_some() {
                stages.push(SupervisorStage::ReadOnlyToolchainBind);
                stages.push(SupervisorStage::ReadOnlyRemount);
            }
            stages.push(SupervisorStage::ReadOnlyBindScopeDecision);
            if !self.tmpfs_paths.is_empty() {
                stages.push(SupervisorStage::ScratchTmpfs);
            }
            stages.push(SupervisorStage::MinimalDevices);
            stages.push(SupervisorStage::OldRootCleanup);
            stages.push(SupervisorStage::SavedFdOldRootCheck);
        }
        if self.cwd_mapping {
            stages.push(SupervisorStage::CwdMapping);
        }
        if self.env_cleanup {
            stages.push(SupervisorStage::EnvCleanup);
        }
        if self.fd_cleanup {
            stages.push(SupervisorStage::FdCleanup);
        }
        if self.landlock {
            stages.push(SupervisorStage::Landlock);
        }
        if self.setup_report_pipe {
            stages.push(SupervisorStage::SetupReportPipe);
        }
        if self.exec_handoff {
            stages.push(SupervisorStage::ExecHandoff);
        }
        stages
    }
}

pub(crate) fn default_executable_view_paths() -> Vec<PathBuf> {
    ["/usr/bin", "/bin", "/usr/sbin", "/sbin"]
        .into_iter()
        .map(PathBuf::from)
        .collect()
}

fn scratch_view_target(index: usize, scratch: &Path) -> PathBuf {
    if scratch.is_absolute() && scratch != Path::new("/") {
        scratch.to_path_buf()
    } else {
        PathBuf::from(format!("/scratch/{index}"))
    }
}

fn resolve_executable(
    argv0: &str,
    project_root: &Path,
    cwd: &Path,
    toolchains: &[(PathBuf, PathBuf)],
) -> Result<ExecutableResolution, CepError> {
    if argv0.contains('\0') {
        return Err(plan_error("argv[0]", "contains NUL byte"));
    }

    let candidates = executable_candidates(argv0, project_root, cwd, toolchains);
    for (host_candidate, source_hint) in candidates {
        if !host_candidate.is_file() {
            continue;
        }
        let canonical = std::fs::canonicalize(&host_candidate).map_err(|err| {
            plan_error("argv[0]", format!("cannot canonicalize executable: {err}"))
        })?;
        if canonical == project_root || canonical.starts_with(project_root) {
            return Ok(ExecutableResolution {
                argv0: argv0.to_string(),
                host_path: canonical.clone(),
                view_path: map_project_path_to_view(project_root, &canonical)?,
                source: ExecutableSource::Project,
            });
        }
        for (source, target) in toolchains {
            if canonical == *source || canonical.starts_with(source) {
                return Ok(ExecutableResolution {
                    argv0: argv0.to_string(),
                    host_path: canonical.clone(),
                    view_path: map_toolchain_path_to_view(source, target, &canonical)?,
                    source: ExecutableSource::ReadOnlyToolchain,
                });
            }
        }
        if source_hint == ExecutableSource::ReadOnlyToolchain {
            return Err(plan_error(
                "argv[0]",
                "toolchain executable resolved outside mounted toolchain source",
            ));
        }
    }

    Err(plan_error(
        "argv[0]",
        "executable is not reachable through /project or configured read-only toolchain mounts",
    ))
}

fn executable_candidates(
    argv0: &str,
    project_root: &Path,
    cwd: &Path,
    toolchains: &[(PathBuf, PathBuf)],
) -> Vec<(PathBuf, ExecutableSource)> {
    let path = Path::new(argv0);
    if path.is_absolute() {
        return vec![(path.to_path_buf(), ExecutableSource::Project)];
    }
    if argv0.contains('/') {
        return vec![(cwd.join(path), ExecutableSource::Project)];
    }

    let mut candidates = vec![
        (cwd.join(argv0), ExecutableSource::Project),
        (project_root.join(argv0), ExecutableSource::Project),
        (
            project_root.join("bin").join(argv0),
            ExecutableSource::Project,
        ),
    ];
    for (source, _target) in toolchains {
        candidates.push((source.join(argv0), ExecutableSource::ReadOnlyToolchain));
        candidates.push((
            source.join("bin").join(argv0),
            ExecutableSource::ReadOnlyToolchain,
        ));
    }
    candidates
}

fn map_project_path_to_view(project_root: &Path, host_path: &Path) -> Result<PathBuf, CepError> {
    let relative = host_path.strip_prefix(project_root).map_err(|_| {
        plan_error(
            "cwd",
            "host path is outside canonical project root and cannot map to /project",
        )
    })?;
    Ok(if relative.as_os_str().is_empty() {
        PathBuf::from(PROJECT_VIEW_PATH)
    } else {
        PathBuf::from(PROJECT_VIEW_PATH).join(relative)
    })
}

fn map_toolchain_path_to_view(
    source: &Path,
    target: &Path,
    host_path: &Path,
) -> Result<PathBuf, CepError> {
    let relative = host_path.strip_prefix(source).map_err(|_| {
        plan_error(
            "argv[0]",
            "host path is outside configured read-only toolchain source",
        )
    })?;
    Ok(if relative.as_os_str().is_empty() {
        target.to_path_buf()
    } else {
        target.join(relative)
    })
}

fn plan_error(field: impl Into<String>, reason: impl Into<String>) -> CepError {
    let field = field.into();
    let reason = reason.into();
    CepError::new(
        CepErrorCode::ProjectRootViolation,
        format!("invalid supervisor plan input for {field}: {reason}"),
        CepErrorData::Validation(ValidationErrorData { field, reason }),
        Retriability::Permanent,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enforcement::isolation::CommandIsolationRuntimeConfig;
    use crate::executor::supervisor::overlay::{OverlayMode, OverlayPlan};
    use crate::executor::supervisor::SupervisorStage;
    use tempfile::TempDir;

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().expect("parent")).unwrap();
        std::fs::write(path, b"#!/bin/sh\n").unwrap();
    }

    fn argv(name: &str) -> Vec<String> {
        vec![name.to_string()]
    }

    #[test]
    fn mount_plan_maps_project_root_and_cwd_to_stable_project_view() {
        let temp = TempDir::new().unwrap();
        std::fs::create_dir(temp.path().join("crates")).unwrap();
        touch(&temp.path().join("crates/tools/run"));

        let plan = SupervisorPlan::build(
            temp.path(),
            Some("crates"),
            &argv("tools/run"),
            &CommandIsolationRuntimeConfig::default(),
        )
        .unwrap();

        assert_eq!(plan.project_view_path, Path::new("/project"));
        assert_eq!(plan.cwd_view_path, Path::new("/project/crates"));
        assert_eq!(
            plan.executable.view_path,
            Path::new("/project/crates/tools/run")
        );
        assert!(plan.mounts.iter().any(|mount| {
            mount.kind == MountKind::ProjectRoot
                && mount.target == Path::new("/project")
                && !mount.read_only
        }));
    }

    #[test]
    fn mount_plan_rejects_parent_traversal_cwd() {
        let temp = TempDir::new().unwrap();
        touch(&temp.path().join("run"));

        let err = SupervisorPlan::build(
            temp.path(),
            Some("../outside"),
            &argv("run"),
            &CommandIsolationRuntimeConfig::default(),
        )
        .unwrap_err();

        assert_eq!(err.code, CepErrorCode::ProjectRootViolation);
        assert!(err.message.contains("path traversal"));
    }

    #[test]
    fn mount_plan_resolves_project_local_bare_executable_through_project_path() {
        let temp = TempDir::new().unwrap();
        touch(&temp.path().join("bin/tool"));

        let plan = SupervisorPlan::build(
            temp.path(),
            None,
            &argv("tool"),
            &CommandIsolationRuntimeConfig::default(),
        )
        .unwrap();

        assert_eq!(plan.executable.source, ExecutableSource::Project);
        assert_eq!(plan.executable.view_path, Path::new("/project/bin/tool"));
        assert!(plan
            .generated_path_entries
            .iter()
            .any(|entry| entry == Path::new("/project/bin")));
    }

    #[test]
    fn mount_plan_resolves_configured_toolchain_executable_through_generated_path() {
        let temp = TempDir::new().unwrap();
        touch(&temp.path().join("project/run"));
        let toolchain = temp.path().join("rust");
        touch(&toolchain.join("bin/cargo"));
        let config = CommandIsolationRuntimeConfig {
            read_only_toolchain_paths: vec![toolchain],
            ..CommandIsolationRuntimeConfig::default()
        };

        let plan =
            SupervisorPlan::build(&temp.path().join("project"), None, &argv("cargo"), &config)
                .unwrap();

        assert_eq!(plan.executable.source, ExecutableSource::ReadOnlyToolchain);
        assert_eq!(
            plan.executable.view_path,
            Path::new("/toolchain/ro0/bin/cargo")
        );
        assert!(plan
            .generated_path_entries
            .iter()
            .any(|entry| entry == Path::new("/toolchain/ro0/bin")));
        assert!(plan
            .read_only_toolchain_mounts()
            .all(|mount| mount.read_only));
    }

    #[test]
    fn mount_plan_reports_missing_toolchain_without_widening_mounts() {
        let temp = TempDir::new().unwrap();
        touch(&temp.path().join("bin/run"));
        let missing = temp.path().join("missing-toolchain");
        let config = CommandIsolationRuntimeConfig {
            read_only_toolchain_paths: vec![missing],
            ..CommandIsolationRuntimeConfig::default()
        };

        let plan = SupervisorPlan::build(temp.path(), None, &argv("run"), &config).unwrap();

        assert_eq!(plan.read_only_toolchain_mounts().count(), 0);
        assert!(plan
            .diagnostics
            .iter()
            .any(|diag| diag.key == "read_only_toolchain_paths.0"
                && diag.value.starts_with("missing:")));
    }

    #[test]
    fn mount_plan_fails_closed_for_unmounted_host_executable() {
        let temp = TempDir::new().unwrap();
        let host_tool = temp.path().join("host/bin/tool");
        touch(&host_tool);
        let project = temp.path().join("project");
        std::fs::create_dir(&project).unwrap();

        let err = SupervisorPlan::build(
            &project,
            None,
            &argv(&host_tool.display().to_string()),
            &CommandIsolationRuntimeConfig::default(),
        )
        .unwrap_err();

        assert_eq!(err.code, CepErrorCode::ProjectRootViolation);
        assert!(err.message.contains("executable is not reachable"));
    }

    #[test]
    fn mount_plan_omits_dangerous_host_paths_by_default() {
        let temp = TempDir::new().unwrap();
        touch(&temp.path().join("bin/run"));
        let plan = SupervisorPlan::build(
            temp.path(),
            None,
            &argv("run"),
            &CommandIsolationRuntimeConfig::default(),
        )
        .unwrap();
        let rendered = plan
            .mounts
            .iter()
            .map(|mount| mount.target.display().to_string())
            .collect::<Vec<_>>()
            .join(":");

        for forbidden in [
            "/home/",
            "/run",
            "docker.sock",
            "SSH_AUTH_SOCK",
            ".ssh",
            ".config",
            "/proc",
            "/tmp/host",
        ] {
            assert!(
                !rendered.contains(forbidden),
                "{forbidden} appeared in plan"
            );
        }
        assert!(!plan
            .mounts
            .iter()
            .any(|mount| mount.target == Path::new("/dev")
                && mount.source == Some(PathBuf::from("/dev"))));
    }

    #[test]
    fn read_only_bind_scope_is_top_level_until_recursive_enforcement_exists() {
        let temp = TempDir::new().unwrap();
        touch(&temp.path().join("bin/run"));

        let plan = SupervisorPlan::build(
            temp.path(),
            None,
            &argv("run"),
            &CommandIsolationRuntimeConfig::default(),
        )
        .unwrap();

        assert_eq!(plan.read_only_bind_scope, ReadOnlyBindScope::TopLevelOnly);
        assert_eq!(plan.read_only_bind_scope.label(), "top_level");
    }

    #[test]
    fn scratch_paths_are_tmpfs_mounts_not_host_binds() {
        let temp = TempDir::new().unwrap();
        touch(&temp.path().join("bin/run"));
        let config = CommandIsolationRuntimeConfig {
            scratch_paths: vec![PathBuf::from("/scratch/build")],
            ..CommandIsolationRuntimeConfig::default()
        };

        let plan = SupervisorPlan::build(temp.path(), None, &argv("run"), &config).unwrap();

        let scratch: Vec<_> = plan
            .mounts
            .iter()
            .filter(|mount| mount.kind == MountKind::ScratchTmpfs)
            .collect();
        assert!(scratch
            .iter()
            .any(|mount| mount.target == Path::new("/tmp")));
        assert!(scratch
            .iter()
            .any(|mount| mount.target == Path::new("/scratch/build")));
        assert!(scratch.iter().all(|mount| mount.source.is_none()));
    }

    #[test]
    fn overlay_plan_adds_overlay_setup_before_project_bind() {
        let temp = TempDir::new().unwrap();
        touch(&temp.path().join("bin/run"));
        let overlay =
            OverlayPlan::deterministic(temp.path(), OverlayMode::Diagnostic, "turn-1").unwrap();
        let plan = SupervisorPlan::build(
            temp.path(),
            None,
            &argv("run"),
            &CommandIsolationRuntimeConfig::default(),
        )
        .unwrap()
        .with_overlay_plan(overlay.clone());
        let stages = plan.stages();

        assert_eq!(plan.project_mount_source(), overlay.paths.merged);
        assert!(
            stages
                .iter()
                .position(|stage| *stage == SupervisorStage::OverlayUpperWork)
                < stages
                    .iter()
                    .position(|stage| *stage == SupervisorStage::ProjectBind)
        );
        assert!(
            stages
                .iter()
                .position(|stage| *stage == SupervisorStage::OverlayMount)
                < stages
                    .iter()
                    .position(|stage| *stage == SupervisorStage::ProjectBind)
        );
    }
}

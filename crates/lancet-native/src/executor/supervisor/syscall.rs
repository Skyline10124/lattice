// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Fakeable syscall surface for the Linux command supervisor.

#[cfg(unix)]
use std::os::fd::RawFd;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use super::fd::{cleanup_inherited_fds, FdCleanupStatus, LiveFdCleanupBackend};
use super::landlock::{
    LandlockAbiProbe, LandlockApplicationReport, LandlockRefinementPlan, LANDLOCK_RULE_PATH_BENEATH,
};
use super::plan::ReadOnlyBindScope;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SyscallError {
    pub(crate) operation: &'static str,
    pub(crate) reason: String,
    pub(crate) errno: Option<i32>,
}

impl SyscallError {
    pub(crate) fn new(operation: &'static str, reason: impl Into<String>) -> Self {
        Self {
            operation,
            reason: reason.into(),
            errno: None,
        }
    }

    pub(crate) fn with_errno(
        operation: &'static str,
        reason: impl Into<String>,
        errno: i32,
    ) -> Self {
        Self {
            operation,
            reason: reason.into(),
            errno: Some(errno),
        }
    }
}

impl std::fmt::Display for SyscallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} failed: {}", self.operation, self.reason)
    }
}

pub(crate) trait SupervisorSyscalls {
    fn has_cap_sys_admin(&mut self) -> Result<bool, SyscallError>;
    fn check_unprivileged_user_namespace(&mut self) -> Result<(), SyscallError>;
    fn unshare_user_namespace(&mut self) -> Result<(), SyscallError>;
    fn write_setgroups_deny(&mut self) -> Result<(), SyscallError>;
    fn write_uid_map_current(&mut self) -> Result<(), SyscallError>;
    fn write_gid_map_current(&mut self) -> Result<(), SyscallError>;
    fn enter_pid_namespace_child(&mut self) -> Result<(), SyscallError>;
    fn unshare_network_namespace(&mut self) -> Result<(), SyscallError>;
    fn assert_no_veth_or_default_route(&mut self) -> Result<(), SyscallError>;
    fn unshare_mount_namespace(&mut self) -> Result<(), SyscallError>;
    fn make_mounts_private(&mut self) -> Result<(), SyscallError>;
    fn mount_tmpfs_root(&mut self) -> Result<(), SyscallError>;
    fn bind_mount(&mut self, source: &Path, target: &Path) -> Result<(), SyscallError>;
    fn remount_read_only(&mut self, target: &Path) -> Result<(), SyscallError>;
    fn record_read_only_bind_scope(&mut self, scope: ReadOnlyBindScope)
        -> Result<(), SyscallError>;
    fn mount_scratch_tmpfs(&mut self, target: &Path) -> Result<(), SyscallError>;
    fn mount_procfs(&mut self, target: &Path) -> Result<(), SyscallError>;
    fn mount_minimal_device(&mut self, source: &Path, target: &Path) -> Result<(), SyscallError>;
    fn detach_old_root(&mut self) -> Result<(), SyscallError>;
    fn assert_old_root_unreachable(&mut self) -> Result<(), SyscallError>;
    fn chdir(&mut self, target: &Path) -> Result<(), SyscallError>;
    fn cleanup_env(&mut self) -> Result<(), SyscallError>;
    fn cleanup_fds(&mut self) -> Result<(), SyscallError>;
    fn probe_landlock_abi(&mut self) -> Result<LandlockAbiProbe, SyscallError>;
    fn apply_landlock(
        &mut self,
        plan: &LandlockRefinementPlan,
    ) -> Result<LandlockApplicationReport, SyscallError>;
    fn setup_report_pipe_ready(&mut self) -> Result<(), SyscallError>;
    fn exec_handoff_ready(&mut self) -> Result<(), SyscallError>;
    fn mount_overlay(
        &mut self,
        lower: &Path,
        upper: &Path,
        work: &Path,
        merged: &Path,
    ) -> Result<(), SyscallError>;
    fn umount_overlay(&mut self, merged: &Path) -> Result<(), SyscallError>;
    fn remove_overlay_tree(&mut self, path: &Path) -> Result<(), SyscallError>;
}

#[derive(Debug, Default)]
pub(crate) struct LiveSupervisorSyscalls {
    tmpfs_root: Option<PathBuf>,
    preserve_fds: Vec<std::os::fd::RawFd>,
    userns_handshake: Option<UsernsChildHandshake>,
}

impl LiveSupervisorSyscalls {
    pub(crate) fn with_preserve_fds(preserve_fds: Vec<std::os::fd::RawFd>) -> Self {
        Self {
            tmpfs_root: None,
            preserve_fds,
            userns_handshake: None,
        }
    }

    #[cfg(unix)]
    pub(crate) fn with_preserve_fds_and_userns_handshake(
        preserve_fds: Vec<RawFd>,
        userns_handshake: UsernsChildHandshake,
    ) -> Self {
        Self {
            tmpfs_root: None,
            preserve_fds,
            userns_handshake: Some(userns_handshake),
        }
    }

    fn rooted_target(&self, target: &Path) -> PathBuf {
        let Some(root) = &self.tmpfs_root else {
            return target.to_path_buf();
        };
        if target.is_absolute() {
            match target.strip_prefix("/") {
                Ok(relative) if !relative.as_os_str().is_empty() => root.join(relative),
                _ => root.clone(),
            }
        } else {
            root.join(target)
        }
    }
}

#[cfg(unix)]
#[derive(Debug)]
pub(crate) struct UsernsChildHandshake {
    request_write: RawFd,
    response_read: RawFd,
}

#[cfg(unix)]
impl UsernsChildHandshake {
    pub(crate) fn new(request_write: RawFd, response_read: RawFd) -> Self {
        Self {
            request_write,
            response_read,
        }
    }

    fn request(&mut self, stage: UsernsMapStage) -> Result<(), SyscallError> {
        write_all_fd(self.request_write, &[stage.request_byte()]).map_err(|err| {
            SyscallError::new(
                stage.operation(),
                format!("userns map request failed: {err}"),
            )
        })?;
        let mut response = [0u8; 1];
        read_exact_fd(self.response_read, &mut response).map_err(|err| {
            SyscallError::new(
                stage.operation(),
                format!("userns map response failed: {err}"),
            )
        })?;
        match response[0] {
            b'1' => {
                if stage == UsernsMapStage::GidMap {
                    self.close();
                }
                Ok(())
            }
            _ => Err(SyscallError::new(
                stage.operation(),
                "parent userns map write failed",
            )),
        }
    }

    fn close(&mut self) {
        if self.request_write >= 0 {
            unsafe {
                libc::close(self.request_write);
            }
            self.request_write = -1;
        }
        if self.response_read >= 0 {
            unsafe {
                libc::close(self.response_read);
            }
            self.response_read = -1;
        }
    }
}

#[cfg(unix)]
impl Drop for UsernsChildHandshake {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UsernsMapStage {
    UserNamespaceReady,
    SetgroupsDeny,
    UidMap,
    GidMap,
}

#[cfg(unix)]
impl UsernsMapStage {
    pub(crate) fn from_request_byte(byte: u8) -> Option<Self> {
        Some(match byte {
            b'U' => UsernsMapStage::UserNamespaceReady,
            b'S' => UsernsMapStage::SetgroupsDeny,
            b'u' => UsernsMapStage::UidMap,
            b'g' => UsernsMapStage::GidMap,
            _ => return None,
        })
    }

    const fn request_byte(self) -> u8 {
        match self {
            UsernsMapStage::UserNamespaceReady => b'U',
            UsernsMapStage::SetgroupsDeny => b'S',
            UsernsMapStage::UidMap => b'u',
            UsernsMapStage::GidMap => b'g',
        }
    }

    const fn operation(self) -> &'static str {
        match self {
            UsernsMapStage::UserNamespaceReady => "userns_parent_ready",
            UsernsMapStage::SetgroupsDeny => "write(/proc/child/setgroups)",
            UsernsMapStage::UidMap => "write(/proc/child/uid_map)",
            UsernsMapStage::GidMap => "write(/proc/child/gid_map)",
        }
    }
}

impl Drop for LiveSupervisorSyscalls {
    fn drop(&mut self) {
        if let Some(root) = self.tmpfs_root.take() {
            #[cfg(target_os = "linux")]
            let _ = live_umount2(&root, libc::MNT_DETACH, "umount2(tmpfs_root)");
            let _ = std::fs::remove_dir_all(root);
        }
    }
}

impl SupervisorSyscalls for LiveSupervisorSyscalls {
    fn has_cap_sys_admin(&mut self) -> Result<bool, SyscallError> {
        Ok(live_has_cap_sys_admin())
    }

    fn check_unprivileged_user_namespace(&mut self) -> Result<(), SyscallError> {
        live_check_unprivileged_user_namespace()
    }

    fn unshare_user_namespace(&mut self) -> Result<(), SyscallError> {
        live_unshare(libc::CLONE_NEWUSER, "unshare(CLONE_NEWUSER)")?;
        #[cfg(unix)]
        if let Some(handshake) = self.userns_handshake.as_mut() {
            handshake.request(UsernsMapStage::UserNamespaceReady)?;
        }
        Ok(())
    }

    fn write_setgroups_deny(&mut self) -> Result<(), SyscallError> {
        #[cfg(unix)]
        if let Some(handshake) = self.userns_handshake.as_mut() {
            return handshake.request(UsernsMapStage::SetgroupsDeny);
        }
        std::fs::write("/proc/self/setgroups", b"deny\n")
            .map_err(|err| SyscallError::new("write(/proc/self/setgroups)", err.to_string()))
    }

    fn write_uid_map_current(&mut self) -> Result<(), SyscallError> {
        #[cfg(unix)]
        if let Some(handshake) = self.userns_handshake.as_mut() {
            return handshake.request(UsernsMapStage::UidMap);
        }
        let uid = unsafe { libc::getuid() };
        std::fs::write("/proc/self/uid_map", format!("0 {uid} 1\n"))
            .map_err(|err| SyscallError::new("write(/proc/self/uid_map)", err.to_string()))
    }

    fn write_gid_map_current(&mut self) -> Result<(), SyscallError> {
        #[cfg(unix)]
        if let Some(handshake) = self.userns_handshake.as_mut() {
            return handshake.request(UsernsMapStage::GidMap);
        }
        let gid = unsafe { libc::getgid() };
        std::fs::write("/proc/self/gid_map", format!("0 {gid} 1\n"))
            .map_err(|err| SyscallError::new("write(/proc/self/gid_map)", err.to_string()))
    }

    fn enter_pid_namespace_child(&mut self) -> Result<(), SyscallError> {
        live_enter_pid_namespace_child(&self.preserve_fds)
    }

    fn unshare_network_namespace(&mut self) -> Result<(), SyscallError> {
        live_unshare(libc::CLONE_NEWNET, "unshare(CLONE_NEWNET)")
    }

    fn assert_no_veth_or_default_route(&mut self) -> Result<(), SyscallError> {
        assert_no_veth_or_default_route()
    }

    fn unshare_mount_namespace(&mut self) -> Result<(), SyscallError> {
        live_unshare(libc::CLONE_NEWNS, "unshare(CLONE_NEWNS)")
    }

    fn make_mounts_private(&mut self) -> Result<(), SyscallError> {
        live_mount(
            None,
            Path::new("/"),
            None,
            libc::MS_REC | libc::MS_PRIVATE,
            None,
            "mount(MS_REC|MS_PRIVATE)",
        )
    }

    fn mount_tmpfs_root(&mut self) -> Result<(), SyscallError> {
        let root = tmpfs_root_path();
        std::fs::create_dir_all(&root)
            .map_err(|err| SyscallError::new("mkdir(tmpfs_root)", err.to_string()))?;
        live_mount(
            Some(Path::new("tmpfs")),
            &root,
            Some("tmpfs"),
            0,
            Some("mode=755"),
            "mount(tmpfs root)",
        )?;
        self.tmpfs_root = Some(root);
        Ok(())
    }

    fn bind_mount(&mut self, source: &Path, target: &Path) -> Result<(), SyscallError> {
        let target = self.rooted_target(target);
        prepare_mount_target(source, &target, MountTargetKind::MirrorSource)?;
        live_mount(
            Some(source),
            &target,
            None,
            libc::MS_BIND | libc::MS_REC,
            None,
            "mount(MS_BIND)",
        )
    }

    fn remount_read_only(&mut self, target: &Path) -> Result<(), SyscallError> {
        let target = self.rooted_target(target);
        live_mount(
            None,
            &target,
            None,
            libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY,
            None,
            "mount(MS_REMOUNT|MS_RDONLY)",
        )
    }

    fn record_read_only_bind_scope(
        &mut self,
        _scope: ReadOnlyBindScope,
    ) -> Result<(), SyscallError> {
        Ok(())
    }

    fn mount_scratch_tmpfs(&mut self, target: &Path) -> Result<(), SyscallError> {
        let target = self.rooted_target(target);
        prepare_mount_target(Path::new("."), &target, MountTargetKind::Directory)?;
        live_mount(
            Some(Path::new("tmpfs")),
            &target,
            Some("tmpfs"),
            0,
            Some("mode=700"),
            "mount(tmpfs scratch)",
        )
    }

    fn mount_procfs(&mut self, target: &Path) -> Result<(), SyscallError> {
        let target = self.rooted_target(target);
        prepare_mount_target(Path::new("."), &target, MountTargetKind::Directory)?;
        live_mount(
            Some(Path::new("proc")),
            &target,
            Some("proc"),
            libc::MS_NOSUID | libc::MS_NOEXEC | libc::MS_NODEV | libc::MS_RDONLY,
            None,
            "mount(procfs)",
        )
    }

    fn mount_minimal_device(&mut self, source: &Path, target: &Path) -> Result<(), SyscallError> {
        let target = self.rooted_target(target);
        prepare_mount_target(source, &target, MountTargetKind::File)?;
        live_mount(
            Some(source),
            &target,
            None,
            libc::MS_BIND,
            None,
            "mount(minimal device)",
        )
    }

    fn detach_old_root(&mut self) -> Result<(), SyscallError> {
        let root = self
            .tmpfs_root
            .clone()
            .ok_or_else(|| SyscallError::new("pivot_root", "tmpfs root missing"))?;
        let old_root = root.join(".oldroot");
        std::fs::create_dir_all(&old_root)
            .map_err(|err| SyscallError::new("mkdir(.oldroot)", err.to_string()))?;
        live_pivot_root(&root, &old_root)?;
        std::env::set_current_dir("/")
            .map_err(|err| SyscallError::new("chdir(/)", err.to_string()))?;
        live_umount2(
            Path::new("/.oldroot"),
            libc::MNT_DETACH,
            "umount2(.oldroot)",
        )?;
        std::fs::remove_dir("/.oldroot")
            .map_err(|err| SyscallError::new("rmdir(.oldroot)", err.to_string()))?;
        self.tmpfs_root = None;
        Ok(())
    }

    fn assert_old_root_unreachable(&mut self) -> Result<(), SyscallError> {
        if Path::new("/.oldroot").exists() {
            Err(SyscallError::new(
                "assert_old_root_unreachable",
                "/.oldroot still exists after old-root cleanup",
            ))
        } else {
            Ok(())
        }
    }

    fn chdir(&mut self, target: &Path) -> Result<(), SyscallError> {
        std::env::set_current_dir(target).map_err(|err| SyscallError::new("chdir", err.to_string()))
    }

    fn cleanup_env(&mut self) -> Result<(), SyscallError> {
        // The parent-side Command builder already used env_clear plus the
        // supervisor-generated allowlist. This stage records that the child is
        // past all setup code that might inspect inherited process env.
        Ok(())
    }

    fn cleanup_fds(&mut self) -> Result<(), SyscallError> {
        let mut backend = LiveFdCleanupBackend;
        let report = cleanup_inherited_fds(&mut backend, &self.preserve_fds);
        if report.status == FdCleanupStatus::Applied {
            Ok(())
        } else {
            Err(SyscallError::new(
                "fd_cleanup",
                report
                    .error
                    .unwrap_or_else(|| "fd cleanup failed".to_string()),
            ))
        }
    }

    fn probe_landlock_abi(&mut self) -> Result<LandlockAbiProbe, SyscallError> {
        Ok(super::landlock::probe_landlock_abi())
    }

    fn apply_landlock(
        &mut self,
        plan: &LandlockRefinementPlan,
    ) -> Result<LandlockApplicationReport, SyscallError> {
        live_apply_landlock(plan)
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
        std::fs::create_dir_all(upper)
            .map_err(|err| SyscallError::new("mkdir(overlay upper)", err.to_string()))?;
        std::fs::create_dir_all(work)
            .map_err(|err| SyscallError::new("mkdir(overlay work)", err.to_string()))?;
        std::fs::create_dir_all(merged)
            .map_err(|err| SyscallError::new("mkdir(overlay merged)", err.to_string()))?;
        live_mount(
            Some(Path::new("overlay")),
            merged,
            Some("overlay"),
            0,
            Some(&format!(
                "lowerdir={},upperdir={},workdir={}",
                lower.display(),
                upper.display(),
                work.display()
            )),
            "mount(overlay)",
        )
    }

    fn umount_overlay(&mut self, merged: &Path) -> Result<(), SyscallError> {
        #[cfg(target_os = "linux")]
        {
            if !current_namespace_has_mountpoint(merged).unwrap_or(true) {
                return Ok(());
            }
            live_umount2(merged, libc::MNT_DETACH, "umount2(overlay merged)")
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = merged;
            Err(SyscallError::new(
                "umount2(overlay merged)",
                "unsupported_platform",
            ))
        }
    }

    fn remove_overlay_tree(&mut self, path: &Path) -> Result<(), SyscallError> {
        make_overlay_tree_removable(path)?;
        match std::fs::remove_dir_all(path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(SyscallError::new(
                "remove_dir_all(overlay)",
                err.to_string(),
            )),
        }
    }
}

#[cfg(target_os = "linux")]
fn current_namespace_has_mountpoint(target: &Path) -> Result<bool, SyscallError> {
    let target = target.to_string_lossy();
    let mountinfo = std::fs::read_to_string("/proc/self/mountinfo")
        .map_err(|err| SyscallError::new("read(/proc/self/mountinfo)", err.to_string()))?;
    Ok(mountinfo_contains_mountpoint(&mountinfo, &target))
}

#[cfg(target_os = "linux")]
fn mountinfo_contains_mountpoint(mountinfo: &str, target: &str) -> bool {
    mountinfo.lines().any(|line| {
        line.split_ascii_whitespace()
            .nth(4)
            .is_some_and(|mountpoint| mountpoint == target)
    })
}

#[cfg(unix)]
fn make_overlay_tree_removable(path: &Path) -> Result<(), SyscallError> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(SyscallError::new(
                "metadata(overlay cleanup)",
                err.to_string(),
            ))
        }
    };
    if !metadata.file_type().is_dir() {
        return Ok(());
    }

    let mut permissions = metadata.permissions();
    permissions.set_mode(permissions.mode() | 0o700);
    std::fs::set_permissions(path, permissions)
        .map_err(|err| SyscallError::new("chmod(overlay cleanup)", err.to_string()))?;

    let entries = std::fs::read_dir(path)
        .map_err(|err| SyscallError::new("read_dir(overlay cleanup)", err.to_string()))?;
    for entry in entries {
        let entry =
            entry.map_err(|err| SyscallError::new("read_dir(overlay cleanup)", err.to_string()))?;
        let file_type = entry
            .file_type()
            .map_err(|err| SyscallError::new("metadata(overlay cleanup)", err.to_string()))?;
        if file_type.is_dir() {
            make_overlay_tree_removable(&entry.path())?;
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn make_overlay_tree_removable(_path: &Path) -> Result<(), SyscallError> {
    Ok(())
}

#[cfg(target_os = "linux")]
#[repr(C)]
struct LandlockRulesetAttr {
    handled_access_fs: u64,
}

#[cfg(target_os = "linux")]
#[repr(C)]
struct LandlockPathBeneathAttr {
    allowed_access: u64,
    parent_fd: i32,
}

#[cfg(target_os = "linux")]
fn live_apply_landlock(
    plan: &LandlockRefinementPlan,
) -> Result<LandlockApplicationReport, SyscallError> {
    use std::ffi::CString;
    use std::os::fd::RawFd;
    use std::os::unix::ffi::OsStrExt;

    fn close_fd(fd: RawFd) {
        unsafe {
            libc::close(fd);
        }
    }

    fn set_cloexec(fd: RawFd, operation: &'static str) -> Result<(), SyscallError> {
        let rc = unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
        if rc == 0 {
            Ok(())
        } else {
            Err(SyscallError::new(
                operation,
                std::io::Error::last_os_error().to_string(),
            ))
        }
    }

    let attr = LandlockRulesetAttr {
        handled_access_fs: plan.handled_access_fs,
    };
    let ruleset_fd = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            &attr as *const LandlockRulesetAttr,
            std::mem::size_of::<LandlockRulesetAttr>(),
            0,
        ) as RawFd
    };
    if ruleset_fd < 0 {
        return Err(SyscallError::new(
            "landlock_create_ruleset",
            std::io::Error::last_os_error().to_string(),
        ));
    }
    if let Err(err) = set_cloexec(ruleset_fd, "fcntl(landlock ruleset FD_CLOEXEC)") {
        close_fd(ruleset_fd);
        return Err(err);
    }

    for rule in &plan.rules {
        let path = CString::new(rule.path.as_os_str().as_bytes())
            .map_err(|err| SyscallError::new("landlock_path_cstring", err.to_string()))?;
        let path_fd = unsafe { libc::open(path.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
        if path_fd < 0 {
            close_fd(ruleset_fd);
            return Err(SyscallError::new(
                "open(landlock path)",
                std::io::Error::last_os_error().to_string(),
            ));
        }
        if let Err(err) = set_cloexec(path_fd, "fcntl(landlock path FD_CLOEXEC)") {
            close_fd(path_fd);
            close_fd(ruleset_fd);
            return Err(err);
        }

        let path_attr = LandlockPathBeneathAttr {
            allowed_access: rule.allowed_access_fs,
            parent_fd: path_fd,
        };
        let rc = unsafe {
            libc::syscall(
                libc::SYS_landlock_add_rule,
                ruleset_fd,
                LANDLOCK_RULE_PATH_BENEATH,
                &path_attr as *const LandlockPathBeneathAttr,
                0,
            )
        };
        close_fd(path_fd);
        if rc != 0 {
            close_fd(ruleset_fd);
            return Err(SyscallError::new(
                "landlock_add_rule",
                std::io::Error::last_os_error().to_string(),
            ));
        }
    }

    let prctl_rc = unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
    if prctl_rc != 0 {
        close_fd(ruleset_fd);
        return Err(SyscallError::new(
            "prctl(PR_SET_NO_NEW_PRIVS)",
            std::io::Error::last_os_error().to_string(),
        ));
    }

    let restrict_rc = unsafe { libc::syscall(libc::SYS_landlock_restrict_self, ruleset_fd, 0) };
    close_fd(ruleset_fd);
    if restrict_rc == 0 {
        Ok(LandlockApplicationReport::applied(
            plan.abi_version,
            plan.handled_access_fs,
            plan.required,
        ))
    } else {
        Err(SyscallError::new(
            "landlock_restrict_self",
            std::io::Error::last_os_error().to_string(),
        ))
    }
}

#[cfg(not(target_os = "linux"))]
fn live_apply_landlock(
    plan: &LandlockRefinementPlan,
) -> Result<LandlockApplicationReport, SyscallError> {
    let _ = plan;
    Err(SyscallError::new(
        "landlock_restrict_self",
        "unsupported_platform",
    ))
}

#[cfg(target_os = "linux")]
fn live_has_cap_sys_admin() -> bool {
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return false;
    };
    let Some(line) = status.lines().find(|line| line.starts_with("CapEff:")) else {
        return false;
    };
    let Some(hex) = line.split_whitespace().nth(1) else {
        return false;
    };
    let Ok(bits) = u64::from_str_radix(hex, 16) else {
        return false;
    };
    const CAP_SYS_ADMIN_BIT: u64 = 21;
    bits & (1u64 << CAP_SYS_ADMIN_BIT) != 0
}

#[derive(Clone, Copy, Debug)]
enum MountTargetKind {
    Directory,
    File,
    MirrorSource,
}

fn tmpfs_root_path() -> PathBuf {
    let pid = unsafe { libc::getpid() };
    PathBuf::from(format!("/tmp/lattice-supervisor-root-{pid}"))
}

fn prepare_mount_target(
    source: &Path,
    target: &Path,
    kind: MountTargetKind,
) -> Result<(), SyscallError> {
    let target_kind = match kind {
        MountTargetKind::MirrorSource if source.is_file() => MountTargetKind::File,
        MountTargetKind::MirrorSource => MountTargetKind::Directory,
        explicit => explicit,
    };
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| SyscallError::new("mkdir(mount parent)", err.to_string()))?;
    }
    match target_kind {
        MountTargetKind::Directory => std::fs::create_dir_all(target)
            .map_err(|err| SyscallError::new("mkdir(mount target)", err.to_string())),
        MountTargetKind::File => {
            if target.exists() {
                Ok(())
            } else {
                std::fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(target)
                    .map(|_| ())
                    .map_err(|err| SyscallError::new("create(mount target)", err.to_string()))
            }
        }
        MountTargetKind::MirrorSource => unreachable!("mirror source resolved above"),
    }
}

#[cfg(target_os = "linux")]
fn live_enter_pid_namespace_child(parent_close_fds: &[RawFd]) -> Result<(), SyscallError> {
    live_unshare(libc::CLONE_NEWPID, "unshare(CLONE_NEWPID)")?;
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(SyscallError::new(
            "fork(pid namespace child)",
            std::io::Error::last_os_error().to_string(),
        ));
    }
    if pid == 0 {
        return Ok(());
    }

    for fd in parent_close_fds {
        if *fd >= 0 {
            unsafe {
                libc::close(*fd);
            }
        }
    }

    let code = match waitpid_status(pid) {
        Ok(status) => wait_status_exit_code(status),
        Err(_) => 127,
    };
    unsafe {
        libc::_exit(code);
    }
}

#[cfg(not(target_os = "linux"))]
fn live_enter_pid_namespace_child() -> Result<(), SyscallError> {
    Err(SyscallError::new(
        "unshare(CLONE_NEWPID)",
        "unsupported_platform",
    ))
}

#[cfg(target_os = "linux")]
fn waitpid_status(pid: libc::pid_t) -> Result<libc::c_int, SyscallError> {
    let mut status = 0;
    loop {
        let rc = unsafe { libc::waitpid(pid, &mut status, 0) };
        if rc == pid {
            return Ok(status);
        }
        if rc < 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(SyscallError::new(
                "waitpid(pid namespace child)",
                err.to_string(),
            ));
        }
    }
}

#[cfg(target_os = "linux")]
fn wait_status_exit_code(status: libc::c_int) -> libc::c_int {
    if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else if libc::WIFSIGNALED(status) {
        128 + libc::WTERMSIG(status)
    } else {
        127
    }
}

#[cfg(target_os = "linux")]
fn live_pivot_root(new_root: &Path, put_old: &Path) -> Result<(), SyscallError> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let new_root = CString::new(new_root.as_os_str().as_bytes())
        .map_err(|err| SyscallError::new("pivot_root", err.to_string()))?;
    let put_old = CString::new(put_old.as_os_str().as_bytes())
        .map_err(|err| SyscallError::new("pivot_root", err.to_string()))?;
    let rc = unsafe { libc::syscall(libc::SYS_pivot_root, new_root.as_ptr(), put_old.as_ptr()) };
    if rc == 0 {
        Ok(())
    } else {
        Err(SyscallError::new(
            "pivot_root",
            std::io::Error::last_os_error().to_string(),
        ))
    }
}

#[cfg(not(target_os = "linux"))]
fn live_pivot_root(_new_root: &Path, _put_old: &Path) -> Result<(), SyscallError> {
    Err(SyscallError::new("pivot_root", "unsupported_platform"))
}

#[cfg(target_os = "linux")]
fn live_umount2(
    target: &Path,
    flags: libc::c_int,
    operation: &'static str,
) -> Result<(), SyscallError> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let target = CString::new(target.as_os_str().as_bytes())
        .map_err(|err| SyscallError::new(operation, err.to_string()))?;
    let rc = unsafe { libc::umount2(target.as_ptr(), flags) };
    if rc == 0 {
        Ok(())
    } else {
        let err = std::io::Error::last_os_error();
        match err.raw_os_error() {
            Some(errno) => Err(SyscallError::with_errno(operation, err.to_string(), errno)),
            None => Err(SyscallError::new(operation, err.to_string())),
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn live_umount2(
    _target: &Path,
    _flags: libc::c_int,
    operation: &'static str,
) -> Result<(), SyscallError> {
    Err(SyscallError::new(operation, "unsupported_platform"))
}

#[cfg(target_os = "linux")]
fn assert_no_veth_or_default_route() -> Result<(), SyscallError> {
    let devices = std::fs::read_to_string("/proc/net/dev")
        .map_err(|err| SyscallError::new("read(/proc/net/dev)", err.to_string()))?;
    assert_proc_net_dev_has_only_loopback(&devices)?;
    let routes = std::fs::read_to_string("/proc/net/route")
        .map_err(|err| SyscallError::new("read(/proc/net/route)", err.to_string()))?;
    assert_proc_net_route_has_no_default_route(&routes)?;

    Ok(())
}

pub(super) fn assert_proc_net_dev_has_only_loopback(contents: &str) -> Result<(), SyscallError> {
    let interfaces = proc_net_dev_interfaces(contents);
    if !interfaces.iter().any(|name| name == "lo") {
        return Err(SyscallError::new(
            "assert_no_veth_or_default_route",
            "missing loopback interface in command namespace",
        ));
    }

    let unexpected = interfaces
        .iter()
        .filter(|name| name.as_str() != "lo")
        .cloned()
        .collect::<Vec<_>>();
    if !unexpected.is_empty() {
        return Err(SyscallError::new(
            "assert_no_veth_or_default_route",
            format!(
                "unexpected non-loopback interface(s) in command namespace: {}",
                unexpected.join(", ")
            ),
        ));
    }

    Ok(())
}

pub(super) fn assert_proc_net_route_has_no_default_route(
    contents: &str,
) -> Result<(), SyscallError> {
    for (iface, destination) in proc_net_route_entries(contents) {
        if destination == "00000000" {
            return Err(SyscallError::new(
                "assert_no_veth_or_default_route",
                format!("unexpected default route via {iface} in command namespace"),
            ));
        }
    }

    Ok(())
}

fn proc_net_dev_interfaces(contents: &str) -> Vec<String> {
    contents
        .lines()
        .skip(2)
        .filter_map(|line| {
            line.split_once(':')
                .map(|(name, _)| name.trim().to_string())
        })
        .filter(|name| !name.is_empty())
        .collect()
}

fn proc_net_route_entries(contents: &str) -> Vec<(String, String)> {
    contents
        .lines()
        .skip(1)
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let iface = fields.next()?;
            let destination = fields.next()?;
            Some((iface.to_string(), destination.to_string()))
        })
        .collect()
}

#[cfg(not(target_os = "linux"))]
fn assert_no_veth_or_default_route() -> Result<(), SyscallError> {
    Err(SyscallError::new(
        "assert_no_veth_or_default_route",
        "unsupported_platform",
    ))
}

#[cfg(not(target_os = "linux"))]
fn live_has_cap_sys_admin() -> bool {
    false
}

#[cfg(target_os = "linux")]
fn live_check_unprivileged_user_namespace() -> Result<(), SyscallError> {
    if let Some(contents) = read_optional_sysctl(
        Path::new("/proc/sys/kernel/unprivileged_userns_clone"),
        "read(/proc/sys/kernel/unprivileged_userns_clone)",
    )? {
        assert_sysctl_enabled(
            "kernel.unprivileged_userns_clone",
            "/proc/sys/kernel/unprivileged_userns_clone",
            &contents,
        )?;
    }

    if let Some(contents) = read_optional_sysctl(
        Path::new("/proc/sys/user/max_user_namespaces"),
        "read(/proc/sys/user/max_user_namespaces)",
    )? {
        assert_sysctl_positive(
            "user.max_user_namespaces",
            "/proc/sys/user/max_user_namespaces",
            &contents,
        )?;
    }

    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn live_check_unprivileged_user_namespace() -> Result<(), SyscallError> {
    Err(SyscallError::new(
        "check_unprivileged_user_namespace",
        "unsupported_platform",
    ))
}

#[cfg(target_os = "linux")]
fn read_optional_sysctl(
    path: &Path,
    operation: &'static str,
) -> Result<Option<String>, SyscallError> {
    match std::fs::read_to_string(path) {
        Ok(contents) => Ok(Some(contents)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(SyscallError::new(operation, err.to_string())),
    }
}

fn assert_sysctl_enabled(name: &str, path: &str, contents: &str) -> Result<(), SyscallError> {
    let value = parse_sysctl_u64(name, contents)?;
    if value == 0 {
        return Err(SyscallError::new(
            "check_unprivileged_user_namespace",
            format!("{name} is disabled at {path}"),
        ));
    }

    Ok(())
}

fn assert_sysctl_positive(name: &str, path: &str, contents: &str) -> Result<(), SyscallError> {
    let value = parse_sysctl_u64(name, contents)?;
    if value == 0 {
        return Err(SyscallError::new(
            "check_unprivileged_user_namespace",
            format!("{name} is zero at {path}"),
        ));
    }

    Ok(())
}

fn parse_sysctl_u64(name: &str, contents: &str) -> Result<u64, SyscallError> {
    let Some(token) = contents.split_whitespace().next() else {
        return Err(SyscallError::new(
            "check_unprivileged_user_namespace",
            format!("{name} is empty"),
        ));
    };

    token.parse::<u64>().map_err(|err| {
        SyscallError::new(
            "check_unprivileged_user_namespace",
            format!("{name} has invalid value {token:?}: {err}"),
        )
    })
}

#[cfg(unix)]
fn write_all_fd(fd: RawFd, mut bytes: &[u8]) -> std::io::Result<()> {
    while !bytes.is_empty() {
        let written =
            unsafe { libc::write(fd, bytes.as_ptr().cast::<libc::c_void>(), bytes.len()) };
        if written < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if written == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "short write to userns handshake pipe",
            ));
        }
        bytes = &bytes[written as usize..];
    }
    Ok(())
}

#[cfg(unix)]
fn read_exact_fd(fd: RawFd, mut bytes: &mut [u8]) -> std::io::Result<()> {
    while !bytes.is_empty() {
        let read =
            unsafe { libc::read(fd, bytes.as_mut_ptr().cast::<libc::c_void>(), bytes.len()) };
        if read < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "userns handshake pipe closed",
            ));
        }
        let (_, rest) = bytes.split_at_mut(read as usize);
        bytes = rest;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn live_unshare(flags: libc::c_int, operation: &'static str) -> Result<(), SyscallError> {
    let rc = unsafe { libc::unshare(flags) };
    if rc == 0 {
        Ok(())
    } else {
        Err(SyscallError::new(
            operation,
            std::io::Error::last_os_error().to_string(),
        ))
    }
}

#[cfg(not(target_os = "linux"))]
fn live_unshare(_flags: libc::c_int, operation: &'static str) -> Result<(), SyscallError> {
    Err(SyscallError::new(operation, "unsupported_platform"))
}

#[cfg(target_os = "linux")]
fn live_mount(
    source: Option<&Path>,
    target: &Path,
    fstype: Option<&str>,
    flags: libc::c_ulong,
    data: Option<&str>,
    operation: &'static str,
) -> Result<(), SyscallError> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let source = source
        .map(|path| CString::new(path.as_os_str().as_bytes()))
        .transpose()
        .map_err(|err| SyscallError::new(operation, err.to_string()))?;
    let target = CString::new(target.as_os_str().as_bytes())
        .map_err(|err| SyscallError::new(operation, err.to_string()))?;
    let fstype = fstype
        .map(CString::new)
        .transpose()
        .map_err(|err| SyscallError::new(operation, err.to_string()))?;
    let data = data
        .map(CString::new)
        .transpose()
        .map_err(|err| SyscallError::new(operation, err.to_string()))?;

    let rc = unsafe {
        libc::mount(
            source
                .as_ref()
                .map(|value| value.as_ptr())
                .unwrap_or(std::ptr::null()),
            target.as_ptr(),
            fstype
                .as_ref()
                .map(|value| value.as_ptr())
                .unwrap_or(std::ptr::null()),
            flags,
            data.as_ref()
                .map(|value| value.as_ptr().cast())
                .unwrap_or(std::ptr::null()),
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(SyscallError::new(
            operation,
            std::io::Error::last_os_error().to_string(),
        ))
    }
}

#[cfg(not(target_os = "linux"))]
fn live_mount(
    _source: Option<&Path>,
    _target: &Path,
    _fstype: Option<&str>,
    _flags: libc::c_ulong,
    _data: Option<&str>,
    operation: &'static str,
) -> Result<(), SyscallError> {
    Err(SyscallError::new(operation, "unsupported_platform"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROC_NET_DEV_LOOPBACK_ONLY: &str = r#"Inter-|   Receive                                                |  Transmit
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
    lo:     648       8    0    0    0     0          0         0      648       8    0    0    0     0       0          0
"#;

    const PROC_NET_DEV_WITH_HOST_LINKS: &str = r#"Inter-|   Receive                                                |  Transmit
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
    lo:     648       8    0    0    0     0          0         0      648       8    0    0    0     0       0          0
  eth0:    1024      12    0    0    0     0          0         0     2048      16    0    0    0     0       0          0
veth123:   4096      20    0    0    0     0          0         0     8192      22    0    0    0     0       0          0
"#;

    const PROC_NET_ROUTE_NO_DEFAULT: &str =
        "Iface\tDestination\tGateway\tFlags\tRefCnt\tUse\tMetric\tMask\tMTU\tWindow\tIRTT\n";

    const PROC_NET_ROUTE_WITH_DEFAULT: &str =
        "Iface\tDestination\tGateway\tFlags\tRefCnt\tUse\tMetric\tMask\tMTU\tWindow\tIRTT\n\
eth0\t00000000\t010012AC\t0003\t0\t0\t0\t00000000\t0\t0\t0\n";

    #[test]
    fn live_supervisor_roots_absolute_targets_under_tmpfs_root() {
        let syscalls = LiveSupervisorSyscalls {
            tmpfs_root: Some(PathBuf::from("/tmp/lattice-supervisor-root-test")),
            preserve_fds: vec![9],
            userns_handshake: None,
        };

        assert_eq!(
            syscalls.rooted_target(Path::new("/project/src")),
            Path::new("/tmp/lattice-supervisor-root-test/project/src")
        );
        assert_eq!(
            syscalls.rooted_target(Path::new("/")),
            Path::new("/tmp/lattice-supervisor-root-test")
        );
        assert_eq!(
            syscalls.rooted_target(Path::new("relative")),
            Path::new("/tmp/lattice-supervisor-root-test/relative")
        );
        assert_eq!(syscalls.preserve_fds, vec![9]);
    }

    #[cfg(unix)]
    #[test]
    fn live_overlay_cleanup_removes_unsearchable_workdir_children() {
        let temp = tempfile::TempDir::new().unwrap();
        let work = temp.path().join("work");
        let inner = work.join("work");
        std::fs::create_dir_all(&inner).unwrap();

        let mut permissions = std::fs::metadata(&inner).unwrap().permissions();
        permissions.set_mode(0);
        std::fs::set_permissions(&inner, permissions).unwrap();

        let mut syscalls = LiveSupervisorSyscalls::default();
        syscalls
            .remove_overlay_tree(&work)
            .expect("overlay work tree cleanup should restore permissions before removal");

        assert!(!work.exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn mountinfo_match_requires_exact_mountpoint_field() {
        let mountinfo = "\
24 21 0:20 / / rw,relatime - ext4 /dev/root rw
25 24 0:42 / /tmp/lattice-overlay rw,relatime - overlay overlay rw
";

        assert!(mountinfo_contains_mountpoint(
            mountinfo,
            "/tmp/lattice-overlay"
        ));
        assert!(!mountinfo_contains_mountpoint(mountinfo, "/tmp/lattice"));
    }

    #[test]
    fn proc_net_dev_accepts_fresh_netns_loopback_only() {
        assert!(assert_proc_net_dev_has_only_loopback(PROC_NET_DEV_LOOPBACK_ONLY).is_ok());
    }

    #[test]
    fn proc_net_dev_rejects_any_non_loopback_interface() {
        let err = assert_proc_net_dev_has_only_loopback(PROC_NET_DEV_WITH_HOST_LINKS)
            .expect_err("host interfaces should fail no-egress topology assertion");

        assert_eq!(err.operation, "assert_no_veth_or_default_route");
        assert!(err.reason.contains("eth0"));
        assert!(err.reason.contains("veth123"));
    }

    #[test]
    fn proc_net_dev_requires_loopback_interface() {
        let err = assert_proc_net_dev_has_only_loopback("").expect_err("missing lo should fail");

        assert_eq!(err.operation, "assert_no_veth_or_default_route");
        assert!(err.reason.contains("missing loopback interface"));
    }

    #[test]
    fn proc_net_route_accepts_no_default_route() {
        assert!(assert_proc_net_route_has_no_default_route(PROC_NET_ROUTE_NO_DEFAULT).is_ok());
    }

    #[test]
    fn proc_net_route_rejects_default_route() {
        let err = assert_proc_net_route_has_no_default_route(PROC_NET_ROUTE_WITH_DEFAULT)
            .expect_err("default route should fail no-egress topology assertion");

        assert_eq!(err.operation, "assert_no_veth_or_default_route");
        assert!(err.reason.contains("eth0"));
        assert!(err.reason.contains("default route"));
    }

    #[test]
    fn unprivileged_userns_sysctls_accept_enabled_values() {
        assert!(assert_sysctl_enabled(
            "kernel.unprivileged_userns_clone",
            "/proc/sys/kernel/unprivileged_userns_clone",
            "1\n"
        )
        .is_ok());
        assert!(assert_sysctl_positive(
            "user.max_user_namespaces",
            "/proc/sys/user/max_user_namespaces",
            "126726\n"
        )
        .is_ok());
    }

    #[test]
    fn unprivileged_userns_sysctls_reject_disabled_values() {
        let disabled = assert_sysctl_enabled(
            "kernel.unprivileged_userns_clone",
            "/proc/sys/kernel/unprivileged_userns_clone",
            "0\n",
        )
        .expect_err("disabled unprivileged userns should fail preflight");
        assert_eq!(disabled.operation, "check_unprivileged_user_namespace");
        assert!(disabled.reason.contains("disabled"));

        let zero_quota = assert_sysctl_positive(
            "user.max_user_namespaces",
            "/proc/sys/user/max_user_namespaces",
            "0\n",
        )
        .expect_err("zero user namespace quota should fail preflight");
        assert_eq!(zero_quota.operation, "check_unprivileged_user_namespace");
        assert!(zero_quota.reason.contains("zero"));
    }

    #[test]
    fn unprivileged_userns_sysctls_reject_invalid_values() {
        let err = assert_sysctl_positive(
            "user.max_user_namespaces",
            "/proc/sys/user/max_user_namespaces",
            "not-a-number\n",
        )
        .expect_err("invalid sysctl value should fail preflight");

        assert_eq!(err.operation, "check_unprivileged_user_namespace");
        assert!(err.reason.contains("invalid value"));
    }
}

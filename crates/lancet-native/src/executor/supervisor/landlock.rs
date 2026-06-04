//! Landlock filesystem refinement planning and application helpers.
//!
//! Landlock is a refinement over the already-constructed command mount view.
//! It is never a substitute for the minimal mount view and never contributes
//! network boundary credit.

use std::path::PathBuf;

use lancet_protocol::BoundaryDimension;

use crate::enforcement::isolation::{
    CommandIsolationLandlockBehavior, CommandIsolationMode, CommandIsolationRuntimeConfig,
    LandlockApplicationStatus,
};

use super::plan::{MountKind, SupervisorPlan};

pub(crate) const LANDLOCK_CREATE_RULESET_VERSION: libc::c_uint = 1;
pub(crate) const LANDLOCK_RULE_PATH_BENEATH: libc::c_uint = 1;

pub(crate) const ACCESS_FS_EXECUTE: u64 = 1 << 0;
pub(crate) const ACCESS_FS_WRITE_FILE: u64 = 1 << 1;
pub(crate) const ACCESS_FS_READ_FILE: u64 = 1 << 2;
pub(crate) const ACCESS_FS_READ_DIR: u64 = 1 << 3;
pub(crate) const ACCESS_FS_REMOVE_DIR: u64 = 1 << 4;
pub(crate) const ACCESS_FS_REMOVE_FILE: u64 = 1 << 5;
pub(crate) const ACCESS_FS_MAKE_CHAR: u64 = 1 << 6;
pub(crate) const ACCESS_FS_MAKE_DIR: u64 = 1 << 7;
pub(crate) const ACCESS_FS_MAKE_REG: u64 = 1 << 8;
pub(crate) const ACCESS_FS_MAKE_SOCK: u64 = 1 << 9;
pub(crate) const ACCESS_FS_MAKE_FIFO: u64 = 1 << 10;
pub(crate) const ACCESS_FS_MAKE_BLOCK: u64 = 1 << 11;
pub(crate) const ACCESS_FS_MAKE_SYM: u64 = 1 << 12;
pub(crate) const ACCESS_FS_REFER: u64 = 1 << 13;
pub(crate) const ACCESS_FS_TRUNCATE: u64 = 1 << 14;
pub(crate) const ACCESS_FS_IOCTL_DEV: u64 = 1 << 15;

const ABI_1_FS_RIGHTS: u64 = ACCESS_FS_EXECUTE
    | ACCESS_FS_WRITE_FILE
    | ACCESS_FS_READ_FILE
    | ACCESS_FS_READ_DIR
    | ACCESS_FS_REMOVE_DIR
    | ACCESS_FS_REMOVE_FILE
    | ACCESS_FS_MAKE_CHAR
    | ACCESS_FS_MAKE_DIR
    | ACCESS_FS_MAKE_REG
    | ACCESS_FS_MAKE_SOCK
    | ACCESS_FS_MAKE_FIFO
    | ACCESS_FS_MAKE_BLOCK
    | ACCESS_FS_MAKE_SYM;
const ABI_2_FS_RIGHTS: u64 = ABI_1_FS_RIGHTS | ACCESS_FS_REFER;
const ABI_3_FS_RIGHTS: u64 = ABI_2_FS_RIGHTS | ACCESS_FS_TRUNCATE;
const ABI_5_FS_RIGHTS: u64 = ABI_3_FS_RIGHTS | ACCESS_FS_IOCTL_DEV;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LandlockAbiProbe {
    pub(crate) abi_version: Option<u32>,
    pub(crate) supported: bool,
    pub(crate) reason: String,
}

impl LandlockAbiProbe {
    pub(crate) fn supported(abi_version: u32) -> Self {
        Self {
            abi_version: Some(abi_version),
            supported: true,
            reason: format!("landlock ABI v{abi_version}"),
        }
    }

    pub(crate) fn unsupported(reason: impl Into<String>) -> Self {
        Self {
            abi_version: None,
            supported: false,
            reason: reason.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LandlockRightsMask {
    pub(crate) abi_version: u32,
    pub(crate) handled_access_fs: u64,
}

impl LandlockRightsMask {
    pub(crate) fn for_abi(abi_version: u32) -> Self {
        let handled_access_fs = match abi_version {
            0 => 0,
            1 => ABI_1_FS_RIGHTS,
            2 => ABI_2_FS_RIGHTS,
            3 | 4 => ABI_3_FS_RIGHTS,
            _ => ABI_5_FS_RIGHTS,
        };
        Self {
            abi_version,
            handled_access_fs,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LandlockRuleAccess {
    ReadOnly,
    ReadWrite,
    Device,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LandlockPathRulePlan {
    pub(crate) path: PathBuf,
    pub(crate) access: LandlockRuleAccess,
    pub(crate) allowed_access_fs: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LandlockRefinementPlan {
    pub(crate) abi_version: u32,
    pub(crate) required: bool,
    pub(crate) fail_closed: bool,
    pub(crate) handled_access_fs: u64,
    pub(crate) rules: Vec<LandlockPathRulePlan>,
}

impl LandlockRefinementPlan {
    pub(crate) fn from_supervisor_plan(
        plan: &SupervisorPlan,
        abi_version: u32,
        required: bool,
        fail_closed: bool,
    ) -> Self {
        let rights = LandlockRightsMask::for_abi(abi_version);
        let mut rules = Vec::new();

        rules.push(LandlockPathRulePlan {
            path: plan.project_view_path.clone(),
            access: LandlockRuleAccess::ReadWrite,
            allowed_access_fs: read_write_access_for(rights.handled_access_fs),
        });

        for mount in plan.read_only_toolchain_mounts() {
            rules.push(LandlockPathRulePlan {
                path: mount.target.clone(),
                access: LandlockRuleAccess::ReadOnly,
                allowed_access_fs: read_only_access_for(rights.handled_access_fs),
            });
        }

        for path in &plan.tmpfs_paths {
            rules.push(LandlockPathRulePlan {
                path: path.clone(),
                access: LandlockRuleAccess::ReadWrite,
                allowed_access_fs: read_write_access_for(rights.handled_access_fs),
            });
        }

        for mount in plan
            .mounts
            .iter()
            .filter(|mount| mount.kind == MountKind::MinimalDevice)
        {
            rules.push(LandlockPathRulePlan {
                path: mount.target.clone(),
                access: LandlockRuleAccess::Device,
                allowed_access_fs: device_access_for(rights.handled_access_fs),
            });
        }

        Self {
            abi_version,
            required,
            fail_closed,
            handled_access_fs: rights.handled_access_fs,
            rules,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LandlockApplicationReport {
    pub(crate) status: LandlockApplicationStatus,
    pub(crate) abi_version: Option<u32>,
    pub(crate) rights_mask: Option<u64>,
    pub(crate) ruleset_status: String,
    pub(crate) required: bool,
}

impl LandlockApplicationReport {
    pub(crate) fn unsupported(required: bool, reason: impl Into<String>) -> Self {
        Self {
            status: LandlockApplicationStatus::Unsupported,
            abi_version: None,
            rights_mask: None,
            ruleset_status: reason.into(),
            required,
        }
    }

    pub(crate) fn not_applied(
        required: bool,
        abi_version: Option<u32>,
        rights_mask: Option<u64>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            status: LandlockApplicationStatus::NotApplied,
            abi_version,
            rights_mask,
            ruleset_status: reason.into(),
            required,
        }
    }

    pub(crate) fn applied(abi_version: u32, rights_mask: u64, required: bool) -> Self {
        Self {
            status: LandlockApplicationStatus::Applied,
            abi_version: Some(abi_version),
            rights_mask: Some(rights_mask),
            ruleset_status: "applied".to_string(),
            required,
        }
    }
}

pub(crate) fn landlock_required(config: &CommandIsolationRuntimeConfig) -> bool {
    config
        .required_boundaries
        .contains(&BoundaryDimension::Landlock)
}

pub(crate) fn landlock_fail_closed(config: &CommandIsolationRuntimeConfig) -> bool {
    config.mode == CommandIsolationMode::Enforce && landlock_required(config)
}

pub(crate) fn landlock_stage_enabled(config: &CommandIsolationRuntimeConfig) -> bool {
    config.mode != CommandIsolationMode::Off
        && config.landlock == CommandIsolationLandlockBehavior::Auto
}

pub(crate) fn read_only_access_for(mask: u64) -> u64 {
    mask & (ACCESS_FS_EXECUTE | ACCESS_FS_READ_FILE | ACCESS_FS_READ_DIR | ACCESS_FS_REFER)
}

pub(crate) fn read_write_access_for(mask: u64) -> u64 {
    mask
}

pub(crate) fn device_access_for(mask: u64) -> u64 {
    mask & (ACCESS_FS_READ_FILE | ACCESS_FS_WRITE_FILE | ACCESS_FS_IOCTL_DEV)
}

#[cfg(target_os = "linux")]
pub(crate) fn probe_landlock_abi() -> LandlockAbiProbe {
    let abi = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<libc::c_void>(),
            0usize,
            LANDLOCK_CREATE_RULESET_VERSION,
        )
    };
    if abi > 0 {
        LandlockAbiProbe::supported(abi as u32)
    } else {
        LandlockAbiProbe::unsupported(format!(
            "landlock ABI probe failed: {}",
            std::io::Error::last_os_error()
        ))
    }
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn probe_landlock_abi() -> LandlockAbiProbe {
    LandlockAbiProbe::unsupported("unsupported_platform")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enforcement::isolation::{CommandIsolationMode, CommandIsolationRuntimeConfig};
    use crate::executor::supervisor::plan::SupervisorPlan;
    use std::path::{Path, PathBuf};
    use tempfile::TempDir;

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().expect("parent")).unwrap();
        std::fs::write(path, b"#!/bin/sh\n").unwrap();
    }

    #[test]
    fn landlock_rights_masks_are_abi_specific() {
        let abi1 = LandlockRightsMask::for_abi(1);
        let abi2 = LandlockRightsMask::for_abi(2);
        let abi3 = LandlockRightsMask::for_abi(3);
        let abi4 = LandlockRightsMask::for_abi(4);
        let abi5 = LandlockRightsMask::for_abi(5);

        assert_eq!(abi1.handled_access_fs & ACCESS_FS_REFER, 0);
        assert_eq!(abi1.handled_access_fs & ACCESS_FS_TRUNCATE, 0);
        assert_eq!(abi1.handled_access_fs & ACCESS_FS_IOCTL_DEV, 0);
        assert_ne!(abi2.handled_access_fs & ACCESS_FS_REFER, 0);
        assert_eq!(abi2.handled_access_fs & ACCESS_FS_TRUNCATE, 0);
        assert_ne!(abi3.handled_access_fs & ACCESS_FS_TRUNCATE, 0);
        assert_eq!(abi3.handled_access_fs & ACCESS_FS_IOCTL_DEV, 0);
        assert_eq!(abi4.handled_access_fs, abi3.handled_access_fs);
        assert_ne!(abi5.handled_access_fs & ACCESS_FS_IOCTL_DEV, 0);
    }

    #[test]
    fn landlock_rule_access_is_masked_by_supported_abi_rights() {
        let abi1 = LandlockRightsMask::for_abi(1).handled_access_fs;
        let abi5 = LandlockRightsMask::for_abi(5).handled_access_fs;

        assert_eq!(read_only_access_for(abi1) & ACCESS_FS_TRUNCATE, 0);
        assert_eq!(read_write_access_for(abi1) & ACCESS_FS_TRUNCATE, 0);
        assert_eq!(device_access_for(abi1) & ACCESS_FS_IOCTL_DEV, 0);
        assert_ne!(read_write_access_for(abi5) & ACCESS_FS_IOCTL_DEV, 0);
        assert_ne!(device_access_for(abi5) & ACCESS_FS_IOCTL_DEV, 0);
    }

    #[test]
    fn landlock_refinement_rules_use_in_view_mount_paths() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("project");
        let toolchain = temp.path().join("toolchain");
        touch(&root.join("bin/run"));
        touch(&toolchain.join("bin/cargo"));
        let config = CommandIsolationRuntimeConfig {
            mode: CommandIsolationMode::Enforce,
            optional_boundaries: vec![BoundaryDimension::Landlock],
            read_only_toolchain_paths: vec![toolchain],
            scratch_paths: vec![PathBuf::from("/scratch/build")],
            ..CommandIsolationRuntimeConfig::default()
        };
        let plan = SupervisorPlan::build(&root, None, &["run".to_string()], &config).unwrap();
        let refinement = LandlockRefinementPlan::from_supervisor_plan(&plan, 3, false, false);

        let rule_paths: Vec<_> = refinement.rules.iter().map(|rule| &rule.path).collect();
        assert!(rule_paths.contains(&&PathBuf::from("/project")));
        assert!(rule_paths.contains(&&PathBuf::from("/toolchain/ro0")));
        assert!(rule_paths.contains(&&PathBuf::from("/tmp")));
        assert!(rule_paths.contains(&&PathBuf::from("/scratch/build")));
        assert!(rule_paths.contains(&&PathBuf::from("/dev/null")));
        assert!(!rule_paths
            .iter()
            .any(|path| path.starts_with(temp.path()) || path.starts_with("/home")));

        let toolchain_rule = refinement
            .rules
            .iter()
            .find(|rule| rule.path == Path::new("/toolchain/ro0"))
            .unwrap();
        assert_eq!(toolchain_rule.access, LandlockRuleAccess::ReadOnly);
        assert_eq!(toolchain_rule.allowed_access_fs & ACCESS_FS_WRITE_FILE, 0);
        assert_ne!(toolchain_rule.allowed_access_fs & ACCESS_FS_READ_FILE, 0);
    }
}

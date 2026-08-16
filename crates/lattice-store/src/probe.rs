// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Capability probes against an actual publication location (ADR 0005 §18).
//!
//! Probes run against every real parent directory a store is published in —
//! never once per host or inferred from a filesystem name. A missing
//! primitive fails closed. The maintained Arch/Btrfs environment is the full
//! correctness/fault/performance gate; another filesystem that passes the same
//! probes runs as `Unverified` without creating another maintenance promise.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::StoreError;

/// One probe check outcome.
#[derive(Debug, Clone)]
pub struct ProbeCheck {
    /// Which capability was exercised.
    pub name: &'static str,
    /// Whether the capability is present.
    pub ok: bool,
    /// Human-readable detail (error text on failure).
    pub detail: String,
}

/// The full probe report for one location.
#[derive(Debug, Clone)]
pub struct ProbeReport {
    /// The probed directory.
    pub location: PathBuf,
    /// Mount source and filesystem type from `/proc/self/mounts`, when known.
    pub mount: Option<MountInfo>,
    /// Every check that ran, in execution order.
    pub checks: Vec<ProbeCheck>,
}

/// Mount-table information for a location.
#[derive(Debug, Clone)]
pub struct MountInfo {
    /// Mount source (device or virtual filesystem name).
    pub source: String,
    /// Filesystem type (e.g. `btrfs`, `ext4`).
    pub fstype: String,
}

impl ProbeReport {
    /// Whether every check passed.
    pub fn all_ok(&self) -> bool {
        self.checks.iter().all(|c| c.ok)
    }
}

/// Run every probe against `location` and fail closed on the first missing
/// primitive. Returns the partial report through the error's display text on
/// failure; use [`probe_location_report`] for an always-complete report.
pub fn probe_location(location: &Path) -> Result<ProbeReport, StoreError> {
    let report = probe_location_report(location);
    for check in &report.checks {
        if !check.ok {
            return Err(StoreError::MissingPrimitive {
                primitive: check.name,
                detail: format!("at '{}': {}", location.display(), check.detail),
            });
        }
    }
    Ok(report)
}

/// Run every probe against `location`, collecting all outcomes (a failing
/// check does not stop later checks).
pub fn probe_location_report(location: &Path) -> ProbeReport {
    let mut checks = Vec::new();
    let mount = mount_info(location);

    let mut record = |name: &'static str, f: &mut dyn FnMut() -> Result<(), String>| {
        checks.push(match f() {
            Ok(()) => ProbeCheck {
                name,
                ok: true,
                detail: "ok".to_string(),
            },
            Err(detail) => ProbeCheck {
                name,
                ok: false,
                detail,
            },
        });
    };
    // One scratch sibling per probe run, removed afterwards.
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let scratch = location.join(format!(
        ".lattice-probe-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    if let Err(e) = std::fs::create_dir_all(location) {
        record("same-mount sibling publication", &mut || {
            Err(format!("create location: {e}"))
        });
        return ProbeReport {
            location: location.to_path_buf(),
            mount,
            checks,
        };
    }

    let result = std::fs::create_dir(&scratch);
    match result {
        Ok(()) => record("same-mount sibling publication", &mut || Ok(())),
        Err(e) => {
            record("same-mount sibling publication", &mut || {
                Err(format!("create sibling: {e}"))
            });
            return ProbeReport {
                location: location.to_path_buf(),
                mount,
                checks,
            };
        }
    }

    // File + directory fsync.
    record("file fsync", &mut || {
        let file = scratch.join("probe-file");
        std::fs::write(&file, b"probe").map_err(|e| format!("write: {e}"))?;
        crate::durability::fsync_file(&file).map_err(|e| e.to_string())
    });
    record("directory fsync", &mut || {
        crate::durability::fsync_dir(&scratch).map_err(|e| e.to_string())
    });

    // renameat2(RENAME_NOREPLACE): positive and negative case.
    record("renameat2(RENAME_NOREPLACE)", &mut || {
        let a = scratch.join("rn-a");
        let b = scratch.join("rn-b");
        std::fs::create_dir(&a).map_err(|e| format!("mkdir a: {e}"))?;
        std::fs::create_dir(&b).map_err(|e| format!("mkdir b: {e}"))?;
        match rename_noreplace_probe(&a, &b) {
            Ok(()) => return Err("renameat2 onto an existing target succeeded".to_string()),
            Err(e) if e.raw_os_error() == Some(libc::EEXIST) => {}
            Err(e) => return Err(format!("renameat2 negative case: unexpected error: {e}")),
        }
        let c = scratch.join("rn-c");
        rename_noreplace_probe(&a, &c).map_err(|e| format!("fresh rename failed: {e}"))?;
        Ok(())
    });

    // Local flock: same-file second exclusive acquisition must conflict.
    record("local flock", &mut || {
        use std::os::unix::io::AsRawFd;
        let path = scratch.join("probe.lock");
        let f1 = std::fs::File::create(&path).map_err(|e| format!("create: {e}"))?;
        if unsafe { libc::flock(f1.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(format!(
                "first LOCK_EX: {}",
                std::io::Error::last_os_error()
            ));
        }
        let f2 = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|e| format!("reopen: {e}"))?;
        let second = unsafe { libc::flock(f2.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if second == 0 {
            unsafe { libc::flock(f2.as_raw_fd(), libc::LOCK_UN) };
            return Err("second LOCK_EX on the same file succeeded".to_string());
        }
        unsafe {
            libc::flock(f1.as_raw_fd(), libc::LOCK_UN);
        }
        Ok(())
    });

    // Minimal repository create, publish, reopen, and identity validation.
    record("minimal repository round-trip", &mut || {
        let staging = scratch.join("mini-staging.git");
        let published = scratch.join("mini.git");
        let repo = gix::init_bare(&staging).map_err(|e| format!("init: {e}"))?;
        let blob = repo
            .write_blob(b"probe")
            .map(|id| id.detach())
            .map_err(|e| format!("write blob: {e}"))?;
        repo.reference(
            "refs/lattice/probe",
            blob,
            gix::refs::transaction::PreviousValue::MustNotExist,
            "probe",
        )
        .map_err(|e| format!("write ref: {e}"))?;
        crate::durability::fsync_tree(&staging).map_err(|e| e.to_string())?;
        rename_noreplace_probe(&staging, &published).map_err(|e| format!("publish: {e}"))?;
        crate::durability::fsync_dir(&scratch).map_err(|e| e.to_string())?;
        let reopened = gix::open_opts(&published, gix::open::Options::isolated())
            .map_err(|e| format!("reopen: {e}"))?;
        let reference = reopened
            .try_find_reference("refs/lattice/probe")
            .map_err(|e| format!("read ref: {e}"))?
            .ok_or("ref missing after reopen")?;
        let target = reference
            .target()
            .try_id()
            .ok_or("ref has no oid target")?
            .to_owned();
        if target != blob {
            return Err("ref target changed across publish".to_string());
        }
        Ok(())
    });

    let _ = std::fs::remove_dir_all(&scratch);

    ProbeReport {
        location: location.to_path_buf(),
        mount,
        checks,
    }
}

/// Probe-local renameat2(RENAME_NOREPLACE) so probes report the primitive
/// directly rather than through the create path.
fn rename_noreplace_probe(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let from_c = std::ffi::CString::new(from.as_os_str().as_bytes()).expect("no NUL");
    let to_c = std::ffi::CString::new(to.as_os_str().as_bytes()).expect("no NUL");
    // SAFETY: both CStrings outlive the call.
    let rc = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            from_c.as_ptr(),
            libc::AT_FDCWD,
            to_c.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Best-effort mount-table lookup: longest mount-point prefix of the
/// canonical location in `/proc/self/mounts`.
pub fn mount_info(location: &Path) -> Option<MountInfo> {
    let canonical = std::fs::canonicalize(location).unwrap_or_else(|_| location.to_path_buf());
    let mounts = std::fs::read_to_string("/proc/self/mounts").ok()?;
    let mut best: Option<(usize, MountInfo)> = None;
    for line in mounts.lines() {
        let mut fields = line.split_whitespace();
        let (Some(source), Some(target), Some(fstype), ..) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        // Mount-table escapes: spaces are \040 etc.
        let target = target.replace("\\040", " ");
        if canonical.starts_with(&target) {
            let len = target.len();
            if best.as_ref().is_none_or(|(best_len, _)| len > *best_len) {
                best = Some((
                    len,
                    MountInfo {
                        source: source.to_string(),
                        fstype: fstype.to_string(),
                    },
                ));
            }
        }
    }
    best.map(|(_, info)| info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probes_pass_on_a_temporary_smoke_location() {
        let dir = tempfile::tempdir().unwrap();
        let report = probe_location(dir.path()).unwrap();
        assert!(report.all_ok());
        assert!(report.checks.len() >= 5);
        for check in &report.checks {
            assert!(check.ok, "{}: {}", check.name, check.detail);
        }
    }

    #[test]
    fn mount_info_reports_this_filesystem() {
        let dir = tempfile::tempdir().unwrap();
        let info = mount_info(dir.path()).expect("mount info");
        assert!(!info.fstype.is_empty());
    }
}

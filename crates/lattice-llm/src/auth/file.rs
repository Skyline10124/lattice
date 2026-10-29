// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::{
    fs::{self, File},
    io::{Read, Write},
    os::{
        fd::AsFd,
        unix::fs::{DirBuilderExt, MetadataExt},
    },
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

use rustix::fs::{AtFlags, Mode, OFlags, openat, renameat, unlinkat};
use zeroize::Zeroizing;

use super::{AuthError, MAX_FILE_BYTES, format};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

pub(super) struct Directory(File);

fn effective_uid() -> u32 {
    rustix::process::geteuid().as_raw()
}

fn check_metadata(meta: &fs::Metadata, directory: bool, owner: u32) -> Result<(), AuthError> {
    if meta.uid() != owner {
        return Err(AuthError::InvalidFile(
            "file and directory must belong to the current user",
        ));
    }
    if directory {
        if !meta.is_dir() {
            return Err(AuthError::InvalidFile(
                "configuration directory must be a directory",
            ));
        }
        if meta.mode() & 0o077 != 0 {
            return Err(AuthError::InvalidFile(
                "configuration directory permissions are too broad; group and other permissions must be absent; permissions were not changed",
            ));
        }
        return Ok(());
    }
    if !meta.is_file() {
        return Err(AuthError::InvalidFile(
            "credential file must be a regular file",
        ));
    }
    if meta.mode() & 0o077 != 0 {
        return Err(AuthError::InvalidFile(
            "credential file must not grant group or other permissions; permissions were not changed",
        ));
    }
    if meta.len() > MAX_FILE_BYTES as u64 {
        return Err(AuthError::InvalidFile("file exceeds 1 MiB"));
    }
    Ok(())
}

// 目录和凭据文件共用打开与元数据检查，属主要求不会散落在两个读取入口。
fn open_checked(
    dirfd: impl AsFd,
    path: impl rustix::path::Arg,
    directory: bool,
    owner: u32,
) -> Result<Option<File>, AuthError> {
    let flags = OFlags::RDONLY
        | OFlags::NOFOLLOW
        | OFlags::CLOEXEC
        | OFlags::NONBLOCK
        | if directory {
            OFlags::DIRECTORY
        } else {
            OFlags::empty()
        };
    let descriptor = match openat(dirfd, path, flags, Mode::empty()) {
        Ok(descriptor) => descriptor,
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(rustix::io::Errno::LOOP | rustix::io::Errno::NOTDIR) if directory => {
            return Err(AuthError::InvalidFile(
                "configuration directory must be a directory, not a symbolic link",
            ));
        }
        Err(rustix::io::Errno::LOOP) => {
            return Err(AuthError::InvalidFile(
                "credential file must not be a symbolic link",
            ));
        }
        Err(error) => return Err(AuthError::io("open credential file or directory", error)),
    };
    let file = File::from(descriptor);
    check_metadata(
        &file
            .metadata()
            .map_err(|error| AuthError::io("inspect credential file or directory", error))?,
        directory,
        owner,
    )?;
    Ok(Some(file))
}

impl Directory {
    pub(super) fn open(path: &Path, create: bool) -> Result<Option<Self>, AuthError> {
        if create {
            match fs::DirBuilder::new().mode(0o700).create(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(AuthError::io("create configuration directory", error)),
            }
        }
        open_checked(rustix::fs::CWD, path, true, effective_uid()).map(|file| file.map(Self))
    }

    fn current(&self) -> Result<Option<File>, AuthError> {
        // 相对已检查的目录句柄打开，拒绝链接；NONBLOCK 避免特殊文件让后台挂住。
        open_checked(&self.0, "auth.json", false, effective_uid())
    }

    pub(super) fn read(&self) -> Result<format::Document, AuthError> {
        let Some(file) = self.current()? else {
            return Ok(format::Document::default());
        };
        let mut bytes = Zeroizing::new(Vec::with_capacity(MAX_FILE_BYTES + 2));
        file.take((MAX_FILE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|error| AuthError::io("read credential file", error))?;
        if bytes.len() > MAX_FILE_BYTES {
            return Err(AuthError::InvalidFile("file exceeds 1 MiB"));
        }
        format::parse(&bytes)
    }

    pub(super) fn write(&self, document: &format::Document) -> Result<(), AuthError> {
        let mut bytes = Zeroizing::new(Vec::new());
        serde_json::to_writer(&mut *bytes, document)
            .map_err(|_| AuthError::Internal("cannot encode credentials"))?;
        if bytes.len() > MAX_FILE_BYTES {
            return Err(AuthError::InvalidFile("file exceeds 1 MiB"));
        }
        let mut temporary = self.temporary()?;
        let mut file = &temporary.file;
        file.write_all(&bytes)
            .map_err(|error| AuthError::io("write temporary credential file", error))?;
        file.sync_all()
            .map_err(|error| AuthError::io("sync temporary credential file", error))?;
        // 发布前再次检查现有目标；不通过替换绕过权限或链接拒绝。
        self.current()?;
        renameat(&self.0, &temporary.name, &self.0, "auth.json")
            .map_err(|error| AuthError::io("replace credential file", error))?;
        temporary.published = true;
        self.0.sync_all().map_err(|error| {
            AuthError::io(
                "sync directory after replacement (file may already have changed)",
                error,
            )
        })
    }

    fn temporary(&self) -> Result<Temporary<'_>, AuthError> {
        loop {
            let name = format!(
                ".auth.{}.{}.tmp",
                std::process::id(),
                NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
            );
            match openat(
                &self.0,
                &name,
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::RUSR | Mode::WUSR,
            ) {
                Ok(descriptor) => {
                    let temporary = Temporary {
                        directory: &self.0,
                        file: File::from(descriptor),
                        name,
                        published: false,
                    };
                    check_metadata(
                        &temporary.file.metadata().map_err(|error| {
                            AuthError::io("inspect temporary credential file", error)
                        })?,
                        false,
                        effective_uid(),
                    )?;
                    return Ok(temporary);
                }
                Err(rustix::io::Errno::EXIST) => {}
                Err(error) => return Err(AuthError::io("create temporary credential file", error)),
            }
        }
    }
}

struct Temporary<'a> {
    directory: &'a File,
    file: File,
    name: String,
    published: bool,
}

impl Drop for Temporary<'_> {
    fn drop(&mut self) {
        if !self.published {
            let _ = unlinkat(self.directory, &self.name, AtFlags::empty());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn ownership_requires_the_effective_user_for_both_files_and_directories() {
        let current = effective_uid();
        let root = tempfile::tempdir().unwrap();
        // 临时目录的权限取决于 umask，这里固定为 0700，只考察属主检查。
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let path = root.path().join("auth.json");
        fs::write(&path, b"{}").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        for (path, directory) in [(root.path(), true), (path.as_path(), false)] {
            assert!(
                open_checked(rustix::fs::CWD, path, directory, current)
                    .unwrap()
                    .is_some()
            );
            assert!(matches!(
                open_checked(rustix::fs::CWD, path, directory, current.wrapping_add(1)),
                Err(AuthError::InvalidFile(
                    "file and directory must belong to the current user"
                ))
            ));
        }
    }

    #[test]
    fn unpublished_temporary_is_private_and_removed_on_failure() {
        let root = tempfile::tempdir().unwrap();
        // 同上，固定目录权限，避免结果随 umask 变化。
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let directory = Directory::open(root.path(), false).unwrap().unwrap();
        let temporary = directory.temporary().unwrap();
        let name = temporary.name.clone();
        assert_eq!(temporary.file.metadata().unwrap().mode() & 0o777, 0o600);
        assert!(root.path().join(&name).exists());
        drop(temporary);
        assert!(!root.path().join(&name).exists());
    }
}

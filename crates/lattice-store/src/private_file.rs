// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 私有目录与单分量私有文件的属主/权限核验、限量读取及同目录临时文件原子替换。

use std::{
    ffi::OsStr,
    fmt,
    fs::{self, File},
    io::{Read, Write},
    os::{
        fd::AsFd,
        unix::{
            ffi::OsStrExt,
            fs::{DirBuilderExt, MetadataExt},
        },
    },
    path::{Component, Path},
    sync::atomic::{AtomicU64, Ordering},
};

use rustix::fs::{AtFlags, Mode, OFlags, mkdirat, openat, renameat, unlinkat};
use zeroize::{Zeroize, Zeroizing};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

/// 私有目录或文件各阶段的底层 I/O 操作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrivateFileOperation {
    /// 创建私有目录。
    CreateDirectory,
    /// 打开私有目录。
    OpenDirectory,
    /// 读取私有目录元数据。
    InspectDirectory,
    /// 打开私有文件。
    OpenFile,
    /// 读取私有文件元数据。
    InspectFile,
    /// 读取私有文件内容。
    ReadFile,
    /// 创建同目录临时文件。
    CreateTemporaryFile,
    /// 读取临时文件元数据。
    InspectTemporaryFile,
    /// 写入临时文件内容。
    WriteTemporaryFile,
    /// 同步临时文件数据与元数据。
    SyncTemporaryFile,
    /// 原子重命名替换目标文件。
    ReplaceFile,
}

impl fmt::Display for PrivateFileOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let description = match self {
            Self::CreateDirectory => "创建私有目录",
            Self::OpenDirectory => "打开私有目录",
            Self::InspectDirectory => "读取私有目录元数据",
            Self::OpenFile => "打开私有文件",
            Self::InspectFile => "读取私有文件元数据",
            Self::ReadFile => "读取私有文件内容",
            Self::CreateTemporaryFile => "创建临时私有文件",
            Self::InspectTemporaryFile => "读取临时私有文件元数据",
            Self::WriteTemporaryFile => "写入临时私有文件",
            Self::SyncTemporaryFile => "同步临时私有文件",
            Self::ReplaceFile => "原子替换私有文件",
        };
        f.write_str(description)
    }
}

/// 私有目录与文件操作的结构化错误。
#[derive(Debug, thiserror::Error)]
pub enum PrivateFileError {
    /// 相对名称不是单个普通路径分量或包含非法字节。
    #[error("名称必须是单个普通路径分量，不能包含路径分隔符或空字节")]
    InvalidName,

    /// 目录或文件不属于当前有效用户。
    #[error("私有文件或目录必须属于当前有效用户")]
    WrongOwner,

    /// 目录目标不是目录。
    #[error("私有目录路径必须是目录")]
    NotADirectory,

    /// 目录目标是符号链接。
    #[error("私有目录不能是符号链接")]
    DirectorySymlink,

    /// 目录向同组或其他用户开放了权限。
    #[error("私有目录权限过宽，同组与其他用户权限必须为空（未自动修改权限）")]
    BroadDirectoryPermissions,

    /// 目录在创建后打开前并发消失。
    #[error("私有目录在打开前消失")]
    DirectoryDisappeared,

    /// 文件目标不是普通文件（如目录或 FIFO）。
    #[error("私有文件必须是普通文件")]
    NotARegularFile,

    /// 文件目标是符号链接。
    #[error("私有文件不能是符号链接")]
    FileSymlink,

    /// 文件向同组或其他用户开放了权限。
    #[error("私有文件权限过宽，不能向同组或其他用户授予权限（未自动修改权限）")]
    BroadFilePermissions,

    /// 文件字节数超出调用方指定的上限。
    #[error("私有文件超出大小上限（上限 {max_bytes} 字节）")]
    FileTooLarge {
        /// 允许的最大字节数。
        max_bytes: usize,
    },

    /// 原子重命名替换已成功，但目录同步失败；新文件可能已经生效。
    #[error("替换私有文件后同步目录失败（新文件可能已生效）：{source}")]
    DirectorySyncAfterReplace {
        /// 底层操作系统错误。
        #[source]
        source: std::io::Error,
    },

    /// 指定阶段的底层 I/O 失败。
    #[error("{operation}失败：{source}")]
    Io {
        /// 失败所处的操作阶段。
        operation: PrivateFileOperation,
        /// 底层操作系统错误。
        #[source]
        source: std::io::Error,
    },
}

/// 已通过当前有效用户属主与私有权限（`mode & 0o077 == 0`）核验的目录句柄。
#[derive(Debug)]
pub struct PrivateDirectory {
    dir: File,
}

fn effective_uid() -> u32 {
    rustix::process::geteuid().as_raw()
}

fn validate_single_component(name: &OsStr) -> Result<(), PrivateFileError> {
    let bytes = name.as_bytes();
    if bytes.is_empty()
        || bytes.contains(&0)
        || bytes.contains(&b'/')
        || bytes == b"."
        || bytes == b".."
    {
        return Err(PrivateFileError::InvalidName);
    }
    let mut components = Path::new(name).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(_)), None) => Ok(()),
        _ => Err(PrivateFileError::InvalidName),
    }
}

fn check_directory_metadata(meta: &fs::Metadata, owner: u32) -> Result<(), PrivateFileError> {
    if meta.uid() != owner {
        return Err(PrivateFileError::WrongOwner);
    }
    if !meta.is_dir() {
        return Err(PrivateFileError::NotADirectory);
    }
    if meta.mode() & 0o077 != 0 {
        return Err(PrivateFileError::BroadDirectoryPermissions);
    }
    Ok(())
}

fn check_regular_file_metadata(
    meta: &fs::Metadata,
    owner: u32,
    max_bytes: Option<usize>,
) -> Result<(), PrivateFileError> {
    if meta.uid() != owner {
        return Err(PrivateFileError::WrongOwner);
    }
    if !meta.is_file() {
        return Err(PrivateFileError::NotARegularFile);
    }
    if meta.mode() & 0o077 != 0 {
        return Err(PrivateFileError::BroadFilePermissions);
    }
    if let Some(max_bytes) = max_bytes
        && meta.len() > u64::try_from(max_bytes).unwrap_or(u64::MAX)
    {
        return Err(PrivateFileError::FileTooLarge { max_bytes });
    }
    Ok(())
}

fn open_directory_at(
    dirfd: impl AsFd,
    path: &Path,
    owner: u32,
) -> Result<Option<File>, PrivateFileError> {
    let flags =
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK | OFlags::DIRECTORY;
    let descriptor = match openat(&dirfd, path, flags, Mode::empty()) {
        Ok(descriptor) => descriptor,
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(rustix::io::Errno::LOOP) => return Err(PrivateFileError::DirectorySymlink),
        Err(rustix::io::Errno::NOTDIR) => {
            // Linux 上 O_NOFOLLOW | O_DIRECTORY 遇到符号链接也会返回 ENOTDIR；
            // 在拒绝打开后的冷路径用不跟随链接的 statat 精确区分符号链接与非目录对象。
            if let Ok(stat) = rustix::fs::statat(&dirfd, path, AtFlags::SYMLINK_NOFOLLOW)
                && rustix::fs::FileType::from_raw_mode(stat.st_mode).is_symlink()
            {
                return Err(PrivateFileError::DirectorySymlink);
            }
            return Err(PrivateFileError::NotADirectory);
        }
        Err(err) => {
            return Err(PrivateFileError::Io {
                operation: PrivateFileOperation::OpenDirectory,
                source: std::io::Error::from(err),
            });
        }
    };
    let file = File::from(descriptor);
    let meta = file.metadata().map_err(|source| PrivateFileError::Io {
        operation: PrivateFileOperation::InspectDirectory,
        source,
    })?;
    check_directory_metadata(&meta, owner)?;
    Ok(Some(file))
}

// 相对已核验的目录句柄打开目标文件：NOFOLLOW 拒绝符号链接，NONBLOCK 避免命名管道阻塞进程。
fn open_regular_file(
    dirfd: impl AsFd,
    name: &OsStr,
    owner: u32,
    max_bytes: Option<usize>,
) -> Result<Option<(File, fs::Metadata)>, PrivateFileError> {
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
    let descriptor = match openat(dirfd, name, flags, Mode::empty()) {
        Ok(descriptor) => descriptor,
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(rustix::io::Errno::LOOP) => return Err(PrivateFileError::FileSymlink),
        Err(err) => {
            return Err(PrivateFileError::Io {
                operation: PrivateFileOperation::OpenFile,
                source: std::io::Error::from(err),
            });
        }
    };
    let file = File::from(descriptor);
    let meta = file.metadata().map_err(|source| PrivateFileError::Io {
        operation: PrivateFileOperation::InspectFile,
        source,
    })?;
    check_regular_file_metadata(&meta, owner, max_bytes)?;
    Ok(Some((file, meta)))
}

// 读取私有字节流并避免标准库 Vec::read_to_end 在扩容搬迁时遗留未清零的旧堆内存：
// 初始缓冲多预留 1 字节供 EOF 探测一次读完；若文件在元数据检查后并发增长，先拷贝至新 Zeroizing
// 缓冲再释放并清零旧缓冲。
fn read_zeroizing(
    mut reader: impl Read,
    expected_len: u64,
    max_bytes: Option<usize>,
) -> Result<Zeroizing<Vec<u8>>, PrivateFileError> {
    let cap_limit = max_bytes.and_then(|limit| limit.checked_add(1));
    let mut initial_len = usize::try_from(expected_len)
        .unwrap_or(usize::MAX)
        .saturating_add(1);
    if let Some(limit) = cap_limit {
        initial_len = initial_len.min(limit);
    }
    let mut buf = Zeroizing::new(vec![0u8; initial_len]);
    let mut filled = 0usize;
    loop {
        if filled == buf.len() {
            if let Some(max_bytes) = max_bytes
                && filled > max_bytes
            {
                return Err(PrivateFileError::FileTooLarge { max_bytes });
            }
            let grown_len = buf.len().saturating_mul(2).max(64);
            let next_len = match cap_limit {
                Some(limit) => grown_len.min(limit),
                None => grown_len,
            };
            if next_len == buf.len() {
                let mut probe = Zeroizing::new([0u8; 1]);
                match reader.read(&mut *probe) {
                    Ok(0) => break,
                    Ok(_) => {
                        return Err(PrivateFileError::FileTooLarge {
                            max_bytes: max_bytes.unwrap_or(usize::MAX),
                        });
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(source) => {
                        return Err(PrivateFileError::Io {
                            operation: PrivateFileOperation::ReadFile,
                            source,
                        });
                    }
                }
            }
            let mut next = Zeroizing::new(vec![0u8; next_len]);
            next[..filled].copy_from_slice(&buf[..filled]);
            buf = next;
        }
        match reader.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(read_len) => {
                filled += read_len;
                if let Some(max_bytes) = max_bytes
                    && filled > max_bytes
                {
                    return Err(PrivateFileError::FileTooLarge { max_bytes });
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(source) => {
                return Err(PrivateFileError::Io {
                    operation: PrivateFileOperation::ReadFile,
                    source,
                });
            }
        }
    }
    buf[filled..].zeroize();
    buf.truncate(filled);
    Ok(buf)
}

impl PrivateDirectory {
    /// 打开并核验已存在的私有目录；目录不存在时返回 `Ok(None)`。
    pub fn open(path: &Path) -> Result<Option<Self>, PrivateFileError> {
        open_directory_at(rustix::fs::CWD, path, effective_uid())
            .map(|file| file.map(|dir| Self { dir }))
    }

    /// 以 `0700` 创建（若已存在则忽略）并严格核验私有目录。
    pub fn open_or_create(path: &Path) -> Result<Self, PrivateFileError> {
        match fs::DirBuilder::new().mode(0o700).create(path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(source) => {
                return Err(PrivateFileError::Io {
                    operation: PrivateFileOperation::CreateDirectory,
                    source,
                });
            }
        }
        Self::open(path)?.ok_or(PrivateFileError::DirectoryDisappeared)
    }

    /// 相对当前目录句柄打开（或以 `0700` 创建后打开）单分量私有子目录。
    pub fn child(&self, name: &OsStr, create: bool) -> Result<Option<Self>, PrivateFileError> {
        validate_single_component(name)?;
        if create {
            match mkdirat(&self.dir, name, Mode::RUSR | Mode::WUSR | Mode::XUSR) {
                Ok(()) | Err(rustix::io::Errno::EXIST) => {}
                Err(err) => {
                    return Err(PrivateFileError::Io {
                        operation: PrivateFileOperation::CreateDirectory,
                        source: std::io::Error::from(err),
                    });
                }
            }
        }
        let opened =
            open_directory_at(&self.dir, Path::new(name), effective_uid())?.map(|dir| Self { dir });
        if create && opened.is_none() {
            return Err(PrivateFileError::DirectoryDisappeared);
        }
        Ok(opened)
    }

    /// 相对当前目录句柄严格核验并读取单分量私有文件；不存在时返回 `Ok(None)`。
    pub fn read(
        &self,
        name: &OsStr,
        max_bytes: Option<usize>,
    ) -> Result<Option<Zeroizing<Vec<u8>>>, PrivateFileError> {
        validate_single_component(name)?;
        let Some((file, meta)) = open_regular_file(&self.dir, name, effective_uid(), max_bytes)?
        else {
            return Ok(None);
        };
        read_zeroizing(&file, meta.len(), max_bytes).map(Some)
    }

    /// 通过同目录 `0600` 临时文件、`fsync`、目标复核、`renameat` 与目录同步原子替换单分量文件。
    pub fn replace(&self, name: &OsStr, bytes: &[u8]) -> Result<(), PrivateFileError> {
        self.replace_with_hooks(name, bytes, &NEXT_TEMP, File::sync_all, || Ok(()), |()| {})
    }

    /// 带发布门闩的原子替换：在重命名前执行 `before_publish`，重命名成功后执行 `published`
    /// 并立即释放门闩，最后执行目录同步。
    pub fn replace_guarded<G, E: From<PrivateFileError>>(
        &self,
        name: &OsStr,
        bytes: &[u8],
        before_publish: impl FnOnce() -> Result<G, E>,
        published: impl FnOnce(&mut G),
    ) -> Result<(), E> {
        self.replace_with_hooks(
            name,
            bytes,
            &NEXT_TEMP,
            File::sync_all,
            before_publish,
            published,
        )
    }

    fn replace_with_hooks<G, E: From<PrivateFileError>>(
        &self,
        name: &OsStr,
        bytes: &[u8],
        next_temp: &AtomicU64,
        sync_directory: impl FnOnce(&File) -> std::io::Result<()>,
        before_publish: impl FnOnce() -> Result<G, E>,
        published: impl FnOnce(&mut G),
    ) -> Result<(), E> {
        validate_single_component(name)?;
        let mut temporary = self.create_temporary(name, next_temp)?;
        (&temporary.file)
            .write_all(bytes)
            .map_err(|source| PrivateFileError::Io {
                operation: PrivateFileOperation::WriteTemporaryFile,
                source,
            })?;
        temporary
            .file
            .sync_all()
            .map_err(|source| PrivateFileError::Io {
                operation: PrivateFileOperation::SyncTemporaryFile,
                source,
            })?;
        // 发布前再次核验现有目标：不通过 renameat 绕过坏权限或覆盖符号链接与特殊文件。
        let existing = open_regular_file(&self.dir, name, effective_uid(), None)?;
        drop(existing);

        let mut guard = before_publish()?;
        renameat(&self.dir, &temporary.name, &self.dir, name).map_err(|err| {
            PrivateFileError::Io {
                operation: PrivateFileOperation::ReplaceFile,
                source: std::io::Error::from(err),
            }
        })?;
        temporary.published = true;
        published(&mut guard);
        // 发布事实已确定，目录同步前先行释放门闩，避免文件同步耗时阻塞取消与发布协调。
        drop(guard);
        sync_directory(&self.dir)
            .map_err(|source| PrivateFileError::DirectorySyncAfterReplace { source })
            .map_err(E::from)
    }

    fn create_temporary(
        &self,
        excluded_name: &OsStr,
        next_temp: &AtomicU64,
    ) -> Result<Temporary<'_>, PrivateFileError> {
        loop {
            let name = format!(
                ".private.{}.{}.tmp",
                std::process::id(),
                next_temp.fetch_add(1, Ordering::Relaxed)
            );
            // 排除目标文件名本身：若尚不存在的目标恰好与候选临时名相同，直接使用该名称会在发布门闩放行前暴露正文。
            if OsStr::new(&name) == excluded_name {
                continue;
            }
            match openat(
                &self.dir,
                &name,
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::RUSR | Mode::WUSR,
            ) {
                Ok(descriptor) => {
                    let temporary = Temporary {
                        directory: &self.dir,
                        file: File::from(descriptor),
                        name,
                        published: false,
                    };
                    let meta =
                        temporary
                            .file
                            .metadata()
                            .map_err(|source| PrivateFileError::Io {
                                operation: PrivateFileOperation::InspectTemporaryFile,
                                source,
                            })?;
                    check_regular_file_metadata(&meta, effective_uid(), None)?;
                    return Ok(temporary);
                }
                Err(rustix::io::Errno::EXIST) => {}
                Err(err) => {
                    return Err(PrivateFileError::Io {
                        operation: PrivateFileOperation::CreateTemporaryFile,
                        source: std::io::Error::from(err),
                    });
                }
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
    use std::{
        ffi::OsString,
        os::unix::{
            ffi::OsStringExt,
            fs::{PermissionsExt, symlink},
        },
        sync::Mutex,
        sync::atomic::AtomicBool,
    };

    #[derive(Debug)]
    enum GuardedError {
        Conflict,
        Storage(PrivateFileError),
    }

    impl From<PrivateFileError> for GuardedError {
        fn from(error: PrivateFileError) -> Self {
            Self::Storage(error)
        }
    }

    #[test]
    fn guarded_replace_orders_hooks_and_releases_guard_before_directory_sync() {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let directory = PrivateDirectory::open(root.path()).unwrap().unwrap();
        let target = OsStr::new("secret.bin");
        directory.replace(target, b"initial").unwrap();

        // before_publish 拒绝时不得发布新内容，且临时文件必须自动清理。
        let rejected: Result<(), GuardedError> = directory.replace_guarded(
            target,
            b"rejected",
            || Err::<(), _>(GuardedError::Conflict),
            |()| panic!("被拒绝的替换不能触发 published 回调"),
        );
        assert!(
            matches!(rejected, Err(GuardedError::Conflict)),
            "预期返回业务门闩冲突错误，实际：{rejected:?}"
        );
        assert_eq!(
            directory
                .read(target, None)
                .unwrap()
                .as_deref()
                .map(Vec::as_slice),
            Some(b"initial".as_slice()),
            "被拒绝后原文件内容必须保持不变"
        );
        assert_eq!(
            fs::read_dir(root.path()).unwrap().count(),
            1,
            "未发布的临时文件必须在返回前清理"
        );

        let publication = Mutex::new(());
        let published_flag = AtomicBool::new(false);
        let synced = directory.replace_with_hooks(
            target,
            b"published-payload",
            &NEXT_TEMP,
            |_| {
                assert!(
                    published_flag.load(Ordering::SeqCst),
                    "目录同步必须在 published 回调之后执行"
                );
                assert!(
                    publication.try_lock().is_ok(),
                    "目录同步前必须已释放发布门闩"
                );
                Err(std::io::Error::other("injected directory sync failure"))
            },
            || Ok(publication.lock().unwrap()),
            |_| {
                assert!(
                    publication.try_lock().is_err(),
                    "published 回调执行期间必须仍持有发布门闩"
                );
                assert_eq!(
                    directory
                        .read(target, None)
                        .unwrap()
                        .as_deref()
                        .map(Vec::as_slice),
                    Some(b"published-payload".as_slice()),
                    "published 回调时新文件必须已可见"
                );
                published_flag.store(true, Ordering::SeqCst);
            },
        );
        assert!(
            matches!(
                synced,
                Err(GuardedError::Storage(
                    PrivateFileError::DirectorySyncAfterReplace { .. }
                ))
            ),
            "目录同步失败应映射为发布后同步失败变体，实际：{synced:?}"
        );
        assert_eq!(
            directory
                .read(target, None)
                .unwrap()
                .as_deref()
                .map(Vec::as_slice),
            Some(b"published-payload".as_slice()),
            "目录同步失败后不得回滚已发布的新文件"
        );
        assert_eq!(
            fs::metadata(root.path().join("secret.bin")).unwrap().mode() & 0o777,
            0o600,
            "替换后的文件权限必须为 0600"
        );
    }

    #[test]
    fn ownership_requires_effective_user_for_files_and_directories() {
        let current = effective_uid();
        let root = tempfile::tempdir().unwrap();
        // 临时目录权限受宿主 umask 影响，先固定为 0700，确保测试只考察属主检查。
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let file_path = root.path().join("item.bin");
        fs::write(&file_path, b"payload").unwrap();
        fs::set_permissions(&file_path, fs::Permissions::from_mode(0o600)).unwrap();

        let dir_fd = open_directory_at(rustix::fs::CWD, root.path(), current)
            .unwrap()
            .expect("当前有效用户应能打开自有目录");
        assert!(
            matches!(
                open_directory_at(rustix::fs::CWD, root.path(), current.wrapping_add(1)),
                Err(PrivateFileError::WrongOwner)
            ),
            "目录属主不匹配当前有效用户时必须拒绝"
        );

        assert!(
            open_regular_file(&dir_fd, OsStr::new("item.bin"), current, None)
                .unwrap()
                .is_some(),
            "当前有效用户应能打开自有私有文件"
        );
        assert!(
            matches!(
                open_regular_file(
                    &dir_fd,
                    OsStr::new("item.bin"),
                    current.wrapping_add(1),
                    None,
                ),
                Err(PrivateFileError::WrongOwner)
            ),
            "文件属主不匹配当前有效用户时必须拒绝"
        );
    }

    #[test]
    fn unpublished_temporary_is_private_and_removed_on_drop() {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let directory = PrivateDirectory::open(root.path()).unwrap().unwrap();

        let temporary = directory
            .create_temporary(OsStr::new("unused"), &NEXT_TEMP)
            .unwrap();
        let temp_path = root.path().join(&temporary.name);
        assert_eq!(
            temporary.file.metadata().unwrap().mode() & 0o777,
            0o600,
            "临时文件必须以 0600 创建"
        );
        assert!(temp_path.exists(), "Drop 前临时文件应存在");
        drop(temporary);
        assert!(!temp_path.exists(), "未发布的临时文件在 Drop 后必须删除");
    }

    #[test]
    fn temporary_name_never_collides_with_target_name() {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let directory = PrivateDirectory::open(root.path()).unwrap().unwrap();

        let pid = std::process::id();
        let counter = AtomicU64::new(0);
        let colliding_name = format!(".private.{pid}.0.tmp");
        let target = OsStr::new(&colliding_name);

        let rejected: Result<(), GuardedError> = directory.replace_with_hooks(
            target,
            b"unapproved-payload",
            &counter,
            File::sync_all,
            || {
                assert!(
                    directory.read(target, None).unwrap().is_none(),
                    "发布门闩放行前目标文件不得可读"
                );
                assert!(
                    !root.path().join(&colliding_name).exists(),
                    "临时文件不得直接创建在尚不存在的目标路径上"
                );
                assert!(
                    root.path().join(format!(".private.{pid}.1.tmp")).exists(),
                    "候选临时名与目标重合时应顺延分配下一个序号"
                );
                Err(GuardedError::Conflict)
            },
            |()| panic!("门闩拒绝时不得执行 published 回调"),
        );
        assert!(
            matches!(rejected, Err(GuardedError::Conflict)),
            "预期返回业务门闩冲突错误，实际：{rejected:?}"
        );
        assert_eq!(
            counter.load(Ordering::Relaxed),
            2,
            "跳过重合序号 0 后应实际分配序号 1"
        );
        assert!(
            directory.read(target, None).unwrap().is_none(),
            "门闩拒绝后目标路径必须仍不存在"
        );
        assert_eq!(
            fs::read_dir(root.path()).unwrap().count(),
            0,
            "门闩拒绝后目录内不得残留任何文件"
        );

        counter.store(0, Ordering::Relaxed);
        directory
            .replace_with_hooks::<(), PrivateFileError>(
                target,
                b"approved-payload",
                &counter,
                File::sync_all,
                || {
                    assert!(
                        directory.read(target, None).unwrap().is_none(),
                        "重命名前目标路径必须不可见"
                    );
                    Ok(())
                },
                |()| {
                    assert_eq!(
                        directory
                            .read(target, None)
                            .unwrap()
                            .as_deref()
                            .map(Vec::as_slice),
                        Some(b"approved-payload".as_slice()),
                        "published 回调期间目标文件必须已原子可见"
                    );
                },
            )
            .unwrap();
        assert_eq!(
            directory
                .read(target, None)
                .unwrap()
                .as_deref()
                .map(Vec::as_slice),
            Some(b"approved-payload".as_slice()),
            "发布完成后目标文件内容必须可读"
        );
        assert_eq!(
            fs::read_dir(root.path()).unwrap().count(),
            1,
            "发布完成后目录内应仅保留目标文件"
        );

        let public_root = tempfile::tempdir().unwrap();
        fs::set_permissions(public_root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let public_dir = PrivateDirectory::open(public_root.path()).unwrap().unwrap();
        let predicted = format!(".private.{pid}.{}.tmp", NEXT_TEMP.load(Ordering::Relaxed));
        let public_target = OsStr::new(&predicted);
        let public_rejected: Result<(), GuardedError> = public_dir.replace_guarded(
            public_target,
            b"public-unapproved",
            || {
                assert!(
                    public_dir.read(public_target, None).unwrap().is_none(),
                    "replace_guarded 放行前目标路径不得可读"
                );
                Err(GuardedError::Conflict)
            },
            |()| panic!("门闩拒绝时不得执行 published 回调"),
        );
        assert!(matches!(public_rejected, Err(GuardedError::Conflict)));
        assert!(public_dir.read(public_target, None).unwrap().is_none());
    }

    #[test]
    fn directory_permissions_symlinks_and_parent_symlinks_are_enforced_without_repair() {
        let workspace = tempfile::tempdir().unwrap();
        let missing = workspace.path().join("absent");
        assert!(
            PrivateDirectory::open(&missing).unwrap().is_none(),
            "不存在的目录调用 open 应返回 Ok(None)"
        );

        let created_path = workspace.path().join("private_dir");
        let created = PrivateDirectory::open_or_create(&created_path).unwrap();
        created
            .replace(OsStr::new("data"), b"secret-bytes")
            .unwrap();
        assert_eq!(
            fs::metadata(&created_path).unwrap().mode() & 0o777,
            0o700,
            "open_or_create 新建目录权限必须为 0700"
        );

        // 0500 只读目录应被接受且不修改权限。
        fs::set_permissions(&created_path, fs::Permissions::from_mode(0o500)).unwrap();
        let readonly_dir = PrivateDirectory::open(&created_path).unwrap().unwrap();
        assert_eq!(
            readonly_dir
                .read(OsStr::new("data"), None)
                .unwrap()
                .as_deref()
                .map(Vec::as_slice),
            Some(b"secret-bytes".as_slice())
        );
        assert_eq!(
            fs::metadata(&created_path).unwrap().mode() & 0o777,
            0o500,
            "打开 0500 目录不得修改其权限"
        );
        let create_temp_err = readonly_dir
            .replace(OsStr::new("data"), b"updated")
            .unwrap_err();
        assert!(
            matches!(
                create_temp_err,
                PrivateFileError::Io {
                    operation: PrivateFileOperation::CreateTemporaryFile,
                    ..
                }
            ),
            "只读目录写入应报创建临时文件 I/O 失败，实际：{create_temp_err:?}"
        );

        // 0755 权限过宽目录必须拒绝且不自动修权。
        fs::set_permissions(&created_path, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(
            PrivateDirectory::open(&created_path),
            Err(PrivateFileError::BroadDirectoryPermissions)
        ));
        assert!(matches!(
            PrivateDirectory::open_or_create(&created_path),
            Err(PrivateFileError::BroadDirectoryPermissions)
        ));
        assert_eq!(
            fs::metadata(&created_path).unwrap().mode() & 0o777,
            0o755,
            "拒绝过宽目录权限时不得自动 chmod"
        );
        fs::set_permissions(&created_path, fs::Permissions::from_mode(0o700)).unwrap();

        // 目标目录本身为符号链接时必须拒绝。
        let dir_link = workspace.path().join("dir_link");
        symlink(&created_path, &dir_link).unwrap();
        assert!(matches!(
            PrivateDirectory::open(&dir_link),
            Err(PrivateFileError::DirectorySymlink)
        ));
        assert!(matches!(
            PrivateDirectory::open_or_create(&dir_link),
            Err(PrivateFileError::DirectorySymlink)
        ));

        // 目标路径为普通文件时必须返回 NotADirectory。
        let regular_file = workspace.path().join("plain_file");
        fs::write(&regular_file, b"not a dir").unwrap();
        fs::set_permissions(&regular_file, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(matches!(
            PrivateDirectory::open(&regular_file),
            Err(PrivateFileError::NotADirectory)
        ));
        assert!(matches!(
            PrivateDirectory::open_or_create(&regular_file),
            Err(PrivateFileError::NotADirectory)
        ));

        // 上级父路径是符号链接（如符号链接 $HOME），但最终目标目录是真实 0700 目录时必须允许。
        let real_home = workspace.path().join("real_home");
        fs::create_dir(&real_home).unwrap();
        let linked_home = workspace.path().join("linked_home");
        symlink(&real_home, &linked_home).unwrap();
        let nested_target = linked_home.join(".lattice");
        let via_parent_link = PrivateDirectory::open_or_create(&nested_target).unwrap();
        via_parent_link.replace(OsStr::new("token"), b"ok").unwrap();
        assert_eq!(
            PrivateDirectory::open(&nested_target)
                .unwrap()
                .unwrap()
                .read(OsStr::new("token"), None)
                .unwrap()
                .as_deref()
                .map(Vec::as_slice),
            Some(b"ok".as_slice())
        );
        assert_eq!(
            fs::metadata(real_home.join(".lattice")).unwrap().mode() & 0o777,
            0o700
        );
    }

    #[test]
    fn child_directory_creation_and_validation() {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let parent = PrivateDirectory::open(root.path()).unwrap().unwrap();

        assert!(
            parent.child(OsStr::new("im"), false).unwrap().is_none(),
            "子目录不存在且 create=false 时应返回 Ok(None)"
        );

        let im = parent
            .child(OsStr::new("im"), true)
            .unwrap()
            .expect("create=true 时应创建并返回子目录句柄");
        let im_path = root.path().join("im");
        assert_eq!(
            fs::metadata(&im_path).unwrap().mode() & 0o777,
            0o700,
            "新建子目录权限必须为 0700"
        );
        im.replace(OsStr::new("ledger"), b"entry-1").unwrap();
        assert_eq!(
            parent
                .child(OsStr::new("im"), false)
                .unwrap()
                .unwrap()
                .read(OsStr::new("ledger"), None)
                .unwrap()
                .as_deref()
                .map(Vec::as_slice),
            Some(b"entry-1".as_slice())
        );

        // 子目录权限过宽拒绝且不自动修复。
        fs::set_permissions(&im_path, fs::Permissions::from_mode(0o750)).unwrap();
        assert!(matches!(
            parent.child(OsStr::new("im"), false),
            Err(PrivateFileError::BroadDirectoryPermissions)
        ));
        assert!(matches!(
            parent.child(OsStr::new("im"), true),
            Err(PrivateFileError::BroadDirectoryPermissions)
        ));
        assert_eq!(
            fs::metadata(&im_path).unwrap().mode() & 0o777,
            0o750,
            "子目录权限过宽时不得自动修改权限"
        );
        fs::set_permissions(&im_path, fs::Permissions::from_mode(0o700)).unwrap();

        // 子目录为符号链接或普通文件时拒绝。
        let sub_link = root.path().join("im_link");
        symlink("im", &sub_link).unwrap();
        assert!(matches!(
            parent.child(OsStr::new("im_link"), false),
            Err(PrivateFileError::DirectorySymlink)
        ));
        assert!(matches!(
            parent.child(OsStr::new("im_link"), true),
            Err(PrivateFileError::DirectorySymlink)
        ));

        parent.replace(OsStr::new("plain"), b"file").unwrap();
        assert!(matches!(
            parent.child(OsStr::new("plain"), false),
            Err(PrivateFileError::NotADirectory)
        ));
        assert!(matches!(
            parent.child(OsStr::new("plain"), true),
            Err(PrivateFileError::NotADirectory)
        ));
    }

    #[test]
    fn file_permissions_symlinks_and_special_files_are_rejected_without_repair() {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let directory = PrivateDirectory::open(root.path()).unwrap().unwrap();
        let name = OsStr::new("credential");
        let path = root.path().join("credential");

        assert!(
            directory.read(name, None).unwrap().is_none(),
            "不存在的文件读取应返回 Ok(None)"
        );

        directory.replace(name, b"v1").unwrap();
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);

        // 0400 只读文件读取允许且不改权限。
        fs::set_permissions(&path, fs::Permissions::from_mode(0o400)).unwrap();
        assert_eq!(
            directory
                .read(name, None)
                .unwrap()
                .as_deref()
                .map(Vec::as_slice),
            Some(b"v1".as_slice())
        );
        assert_eq!(
            fs::metadata(&path).unwrap().mode() & 0o777,
            0o400,
            "读取 0400 文件不得改变权限"
        );

        // 0644 过宽权限：读取与替换均拒绝，且不改权限、不覆盖内容。
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            directory.read(name, None),
            Err(PrivateFileError::BroadFilePermissions)
        ));
        assert!(matches!(
            directory.replace(name, b"overwrite"),
            Err(PrivateFileError::BroadFilePermissions)
        ));
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o644);
        assert_eq!(fs::read(&path).unwrap(), b"v1");

        // 符号链接（含指向有效私有文件与悬空符号链接）：读取与替换均拒绝，不跟随、不覆盖。
        let real_copy = root.path().join("real_copy");
        fs::rename(&path, &real_copy).unwrap();
        fs::set_permissions(&real_copy, fs::Permissions::from_mode(0o600)).unwrap();
        symlink("real_copy", &path).unwrap();
        assert!(matches!(
            directory.read(name, None),
            Err(PrivateFileError::FileSymlink)
        ));
        assert!(matches!(
            directory.replace(name, b"overwrite"),
            Err(PrivateFileError::FileSymlink)
        ));
        assert!(
            fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read(&real_copy).unwrap(), b"v1");
        fs::remove_file(&path).unwrap();

        symlink("missing_target", &path).unwrap();
        assert!(matches!(
            directory.read(name, None),
            Err(PrivateFileError::FileSymlink)
        ));
        assert!(matches!(
            directory.replace(name, b"overwrite"),
            Err(PrivateFileError::FileSymlink)
        ));
        fs::remove_file(&path).unwrap();

        // 目标位置被子目录占用：读取与替换均拒绝且不覆盖目录。
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(matches!(
            directory.read(name, None),
            Err(PrivateFileError::NotARegularFile)
        ));
        assert!(matches!(
            directory.replace(name, b"overwrite"),
            Err(PrivateFileError::NotARegularFile)
        ));
        assert!(path.is_dir());
        fs::remove_dir(&path).unwrap();

        // 目标位置为 FIFO 管道：NONBLOCK 打开不阻塞，读取与替换均立即以 NotARegularFile 拒绝。
        rustix::fs::mknodat(
            rustix::fs::CWD,
            &path,
            rustix::fs::FileType::Fifo,
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
            0,
        )
        .unwrap();
        assert!(matches!(
            directory.read(name, None),
            Err(PrivateFileError::NotARegularFile)
        ));
        assert!(matches!(
            directory.replace(name, b"overwrite"),
            Err(PrivateFileError::NotARegularFile)
        ));
        fs::remove_file(&path).unwrap();

        // 恢复正常私有文件后读写与替换均恢复正常，且目录中无残留临时文件。
        fs::rename(&real_copy, &path).unwrap();
        assert_eq!(
            directory
                .read(name, None)
                .unwrap()
                .as_deref()
                .map(Vec::as_slice),
            Some(b"v1".as_slice())
        );
        directory.replace(name, b"v2").unwrap();
        assert_eq!(
            directory
                .read(name, None)
                .unwrap()
                .as_deref()
                .map(Vec::as_slice),
            Some(b"v2".as_slice())
        );
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[test]
    fn single_component_names_and_max_bytes_limit() {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let directory = PrivateDirectory::open(root.path()).unwrap().unwrap();

        let invalid_names = [
            OsString::from(""),
            OsString::from("."),
            OsString::from(".."),
            OsString::from("a/b"),
            OsString::from("/root"),
            OsString::from("trailing/"),
            OsString::from("sub/."),
            OsString::from_vec(b"nul\0byte".to_vec()),
        ];
        for bad in &invalid_names {
            assert!(
                matches!(
                    directory.child(bad, false),
                    Err(PrivateFileError::InvalidName)
                ),
                "child(false) 应拒绝非法名称 {bad:?}"
            );
            assert!(
                matches!(
                    directory.child(bad, true),
                    Err(PrivateFileError::InvalidName)
                ),
                "child(true) 应拒绝非法名称 {bad:?}"
            );
            assert!(
                matches!(
                    directory.read(bad, None),
                    Err(PrivateFileError::InvalidName)
                ),
                "read 应拒绝非法名称 {bad:?}"
            );
            assert!(
                matches!(
                    directory.replace(bad, b"x"),
                    Err(PrivateFileError::InvalidName)
                ),
                "replace 应拒绝非法名称 {bad:?}"
            );
            let guarded: Result<(), PrivateFileError> =
                directory.replace_guarded(bad, b"x", || Ok(()), |()| {});
            assert!(
                matches!(guarded, Err(PrivateFileError::InvalidName)),
                "replace_guarded 应拒绝非法名称 {bad:?}"
            );
        }

        let name = OsStr::new("bounded.bin");
        directory.replace(name, b"12345").unwrap();
        assert_eq!(
            directory
                .read(name, Some(5))
                .unwrap()
                .as_deref()
                .map(Vec::as_slice),
            Some(b"12345".as_slice()),
            "恰好等于 max_bytes 的文件应允许读取"
        );
        assert!(
            matches!(
                directory.read(name, Some(4)),
                Err(PrivateFileError::FileTooLarge { max_bytes: 4 })
            ),
            "超过 max_bytes 的文件必须返回 FileTooLarge"
        );
        assert_eq!(
            directory
                .read(name, None)
                .unwrap()
                .as_deref()
                .map(Vec::as_slice),
            Some(b"12345".as_slice()),
            "未设置 max_bytes 时应完整读取"
        );

        // 文件在元数据检查后并发增长时，安全扩容读取仍应完整返回或按上限拒绝。
        let grown = read_zeroizing(&b"0123456789abcdef"[..], 2, Some(16)).unwrap();
        assert_eq!(grown.as_slice(), b"0123456789abcdef");
        assert!(matches!(
            read_zeroizing(&b"0123456789abcdef"[..], 2, Some(15)),
            Err(PrivateFileError::FileTooLarge { max_bytes: 15 })
        ));
        let grown_unbounded = read_zeroizing(&b"concurrent-growth-payload"[..], 0, None).unwrap();
        assert_eq!(grown_unbounded.as_slice(), b"concurrent-growth-payload");
    }
}

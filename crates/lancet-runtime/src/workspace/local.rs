// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Concrete Local Workspace implementation.
//!
//! See [`super`] for the module-level contract. The single concrete type
//! [`LocalWorkspace`] owns secure path resolution, bounded reads, content
//! hashing, no-replace create, atomic edit installation, and filesystem
//! error normalization. It is constructed from a canonical project root
//! and never re-canonicalizes after construction.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
#[cfg(any(test, feature = "test-fault-injection"))]
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::history::{WorkspaceHistory, WorkspaceHistoryError};
use regex::Regex;
use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, ResolveFlags};
use rustix::io::Errno;

use crate::workspace::content_hash;

use super::types::{
    CreatedFile, DirectoryEntry, FindRequest, GrepMatch, GrepRequest, InstalledFile,
    LocalWorkspaceError, PathFacts, PathIntent, PathKind, WorkspacePath,
};
use super::visibility::{VisibilityEngine, VisibilityEvidence};

#[cfg(any(test, feature = "test-fault-injection"))]
type ReplaceBeforeCheck = Arc<Mutex<Option<(WorkspacePath, Vec<u8>)>>>;

// #233：在检查/打开边界上确定性地替换文件系统对象。保持模块私有且线程局部，
// 使并行测试互不影响。
#[cfg(test)]
type BeforeOpenHook = Box<dyn FnMut(&Path)>;
#[cfg(test)]
thread_local! {
    static BEFORE_OPEN: std::cell::RefCell<Option<BeforeOpenHook>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
struct BeforeOpenGuard(std::marker::PhantomData<std::rc::Rc<()>>);

#[cfg(test)]
impl Drop for BeforeOpenGuard {
    fn drop(&mut self) {
        BEFORE_OPEN.with(|hook| *hook.borrow_mut() = None);
    }
}

#[cfg(test)]
fn set_before_open(hook: impl FnMut(&Path) + 'static) -> BeforeOpenGuard {
    BEFORE_OPEN.with(|slot| {
        assert!(
            slot.borrow().is_none(),
            "before-open hook already installed"
        );
        *slot.borrow_mut() = Some(Box::new(hook));
    });
    BeforeOpenGuard(std::marker::PhantomData)
}

#[inline]
fn before_open(_rel: &Path) {
    #[cfg(test)]
    BEFORE_OPEN.with(|slot| {
        if let Some(hook) = slot.borrow_mut().as_mut() {
            hook(_rel);
        }
    });
}

/// The single concrete Local Workspace (ADR 0003 decision 3).
///
/// Internal deep module: secure path resolution, bounded read, atomic
/// installation, mutation history, and error normalization all live behind
/// this type. All Tool Host observation/mutation paths use it directly; there
/// is no workspace-backend trait or caller-supplied execution seam.
pub struct LocalWorkspace {
    /// Canonical project root. Captured at construction time; never mutates.
    root: PathBuf,
    /// Private durable history of successfully installed Atomic Local
    /// Mutations (GitHub #63). Lives in its own bare `gix` repository outside
    /// this workspace and outside the project `.git`; recording never touches
    /// workspace content.
    history: Arc<WorkspaceHistory>,
    /// Serializes LATTICE-owned replacement checks and installs. It does not
    /// claim to lock non-cooperating external writers (ADR 0003 decision 9),
    /// but keeps the final revision check and rename in one host operation.
    mutation_lock: Arc<Mutex<()>>,
    /// Serializes one complete LATTICE mutation transaction — installation,
    /// private-history append, and evidence publication — across every Host
    /// instance constructed over this workspace. This is separate from
    /// `mutation_lock` because conditional replacement acquires that narrower
    /// lock while the outer transaction gate is already held.
    mutation_sequence_gate: Arc<Mutex<()>>,
    /// One-shot install-failure flag for the narrow fault-injection seam
    /// (`fail_next_install`). Not present in production builds.
    #[cfg(any(test, feature = "test-fault-injection"))]
    fail_install: Arc<AtomicBool>,
    /// One-shot post-install parent-fsync failure for tests. This fires only
    /// after the target name has been atomically installed.
    #[cfg(any(test, feature = "test-fault-injection"))]
    fail_parent_fsync: Arc<AtomicBool>,
    /// One-shot external-writer simulation consumed immediately before a
    /// conditional replace performs its final Local Revision check.
    #[cfg(any(test, feature = "test-fault-injection"))]
    replace_before_check: ReplaceBeforeCheck,
}

/// Result of a read whose allocation is capped by the caller.
pub enum BoundedFileRead {
    Complete(Vec<u8>),
    LimitExceeded { observed: usize, allowed: usize },
}

/// Successful conditional replacement: exact pre-install bytes plus the
/// atomic install receipt used for history and evidence.
#[derive(Debug)]
pub(crate) struct RevisionMatchedInstall {
    pub before: Vec<u8>,
    pub installed: InstalledFile,
}

/// An atomic replacement receipt plus a possible failure to confirm the
/// installed directory entry's durability. The target is already changed
/// whenever `durability_error` is present.
struct AtomicInstallReceipt {
    installed: InstalledFile,
    durability_error: Option<LocalWorkspaceError>,
}

/// Failure vocabulary for the host-only conditional replacement primitive.
#[derive(Debug)]
pub(crate) enum ConditionalReplaceError {
    Workspace(LocalWorkspaceError),
    StaleRevision {
        observed: String,
    },
    Cancelled,
    InstalledButNotDurable {
        replacement: RevisionMatchedInstall,
        error: LocalWorkspaceError,
    },
}

#[derive(Debug)]
pub(crate) enum ConditionalCreateError {
    Workspace(LocalWorkspaceError),
    Cancelled,
    InstalledButNotDurable {
        created: CreatedFile,
        error: LocalWorkspaceError,
    },
}

impl From<LocalWorkspaceError> for ConditionalCreateError {
    fn from(error: LocalWorkspaceError) -> Self {
        Self::Workspace(error)
    }
}

impl From<std::io::Error> for ConditionalCreateError {
    fn from(error: std::io::Error) -> Self {
        Self::Workspace(LocalWorkspaceError::Io(error))
    }
}

impl LocalWorkspace {
    /// Construct from a project root with mutation history recording off.
    ///
    /// Test-only: the production runtime always attaches the Turn's private
    /// history through [`LocalWorkspace::with_history`], so an installation
    /// path that records nothing cannot exist in a real turn. Gated on the
    /// The feature keeps the constructor absent from production binaries.
    #[cfg(any(test, feature = "test-utils"))]
    pub fn new(root: &Path) -> Self {
        Self::with_history(root, Arc::new(WorkspaceHistory::disabled()))
    }

    /// Construct from a project root, canonicalizing it once, with a private
    /// mutation history attached.
    ///
    /// Falls back to the raw path if canonicalize fails (mirrors
    /// `CepPolicyConfig::defaults_for_project_root` tolerance).
    pub fn with_history(root: &Path, history: Arc<WorkspaceHistory>) -> Self {
        let canonical = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        Self {
            root: canonical,
            history,
            mutation_lock: Arc::new(Mutex::new(())),
            mutation_sequence_gate: Arc::new(Mutex::new(())),
            #[cfg(any(test, feature = "test-fault-injection"))]
            fail_install: Arc::new(AtomicBool::new(false)),
            #[cfg(any(test, feature = "test-fault-injection"))]
            fail_parent_fsync: Arc::new(AtomicBool::new(false)),
            #[cfg(any(test, feature = "test-fault-injection"))]
            replace_before_check: Arc::new(Mutex::new(None)),
        }
    }

    /// Canonical project root this workspace operates on.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Acquire the per-project `WorkspaceOperationLock` for this workspace
    /// (ADR 0005 §5).
    ///
    /// The lock belongs to the project binding the private history carries,
    /// so it is reached through the history handle. Observation takes it
    /// shared; a mutation takes it exclusively and holds it from the
    /// precondition read through install and history publication, which is
    /// strictly wider than the repository write lock the history append takes
    /// underneath. `Ok(None)` means recording is disabled and there is no
    /// project binding to serialize against.
    pub fn lock_operation(
        &self,
        mode: lattice_store::LockMode,
        blocking: lattice_store::Blocking,
    ) -> Result<Option<lattice_store::FlockGuard>, WorkspaceHistoryError> {
        self.history.lock_workspace_operation(mode, blocking)
    }

    /// Clone the shareable state for a `spawn_blocking` closure.
    fn blocking_clone(&self) -> Self {
        Self {
            root: self.root.clone(),
            history: Arc::clone(&self.history),
            mutation_lock: Arc::clone(&self.mutation_lock),
            mutation_sequence_gate: Arc::clone(&self.mutation_sequence_gate),
            #[cfg(any(test, feature = "test-fault-injection"))]
            fail_install: Arc::clone(&self.fail_install),
            #[cfg(any(test, feature = "test-fault-injection"))]
            fail_parent_fsync: Arc::clone(&self.fail_parent_fsync),
            #[cfg(any(test, feature = "test-fault-injection"))]
            replace_before_check: Arc::clone(&self.replace_before_check),
        }
    }

    /// Shared gate for one full Host mutation transaction.
    pub(crate) fn mutation_sequence_gate(&self) -> Arc<Mutex<()>> {
        Arc::clone(&self.mutation_sequence_gate)
    }

    /// 以目录句柄打开规范化后的项目根。每个操作只打开一次项目根，并通过
    /// [`open_beneath`] 在其下解析该操作的全部路径。项目根在构造时已规范化，
    /// 因此这里若因 `O_NOFOLLOW` 失败，说明项目根本身被篡改；按普通 I/O 错误
    /// 失败关闭。
    fn open_root(&self) -> Result<OwnedFd, LocalWorkspaceError> {
        rustix::fs::open(
            &self.root,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|e| LocalWorkspaceError::Io(e.into()))
    }

    /// 打开变更目标的父目录。写入路径（create/install/replace）只能经由返回的
    /// fd 触及父目录，调用方随后通过它以单段叶名操作。这里用 `O_RDONLY` 而非
    /// `O_PATH` 打开，因为安装后的条目要通过对此 fd 的 `fsync` 落盘。
    ///
    /// 带 `create_missing` 时，每个缺失的前缀用 `mkdirat` 在上一层已打开的目录
    /// 下创建（容忍 `EEXIST`：与并发创建竞争失败，和目录原本就存在无法区分），
    /// 然后从项目根重新打开，因此用到的每一层在打开时本身都位于根内。`mkdirat`
    /// 不带解析约束：若上一层恰在它之前被移出项目根，空目录会建在根外，随后从
    /// 项目根的重新打开会失败。
    fn open_parent_dir(
        &self,
        path: &WorkspacePath,
        create_missing: bool,
    ) -> Result<OwnedFd, LocalWorkspaceError> {
        let (parent_rel, _) = split_parent_leaf(path);
        before_open(Path::new(parent_rel));
        let root = self.open_root()?;
        if parent_rel == "." {
            return Ok(root);
        }
        let requested = path.as_str();
        if !create_missing {
            return open_beneath(&root, parent_rel, dir_flags(), Mode::empty())
                .map_err(|e| resolve_error(e, requested));
        }
        let mut level: Option<OwnedFd> = None;
        let mut end = 0;
        for name in parent_rel.split('/') {
            end += if end == 0 { name.len() } else { name.len() + 1 };
            let prefix = &parent_rel[..end];
            let fd = match open_beneath(&root, prefix, dir_flags(), Mode::empty()) {
                Ok(fd) => fd,
                Err(Errno::NOENT) => {
                    let under = level.as_ref().unwrap_or(&root);
                    if let Err(e) = rustix::fs::mkdirat(under, name, Mode::from_raw_mode(0o755))
                        && e != Errno::EXIST
                    {
                        return Err(resolve_error(e, requested));
                    }
                    open_beneath(&root, prefix, dir_flags(), Mode::empty())
                        .map_err(|e| resolve_error(e, requested))?
                }
                Err(e) => return Err(resolve_error(e, requested)),
            };
            level = Some(fd);
        }
        Ok(level.unwrap_or(root))
    }

    /// 解析 `path` 并返回观察到的事实。
    ///
    /// 所有观察入口和同步 Tool Host import 都经由此函数。它与之后的每次打开一样
    /// 通过 [`open_beneath`] 解析，因此路径安全只有一份实现：路径中任一处的符号
    /// 链接都会被拒绝，事实取自对实际到达对象的 `fstat`。目标不存在时报告
    /// `exists = false`，父目录的事实按同样方式解析。
    ///
    /// 意图作为操作词汇传给 guard 与前置条件层；解析不依赖它。
    pub fn resolve_facts_blocking(
        &self,
        path: &WorkspacePath,
        _intent: PathIntent,
    ) -> Result<PathFacts, LocalWorkspaceError> {
        let root = self.open_root()?;
        let facts = |exists, stat: Option<&rustix::fs::Stat>, parent: Option<PathKind>| PathFacts {
            normalized_path: path.as_str().to_string(),
            exists,
            kind: stat.map(stat_kind),
            parent_exists: parent.is_some(),
            parent_kind: parent,
            byte_size: stat
                .filter(|st| FileType::from_raw_mode(st.st_mode) == FileType::RegularFile)
                .map(|st| st.st_size as u64),
            // 只要解析成功，根内约束就成立：带 `RESOLVE_BENEATH` 的 `openat2` 只
            // 返回项目根下的对象，缺失的叶节点则由其经过词法检查的路径保证位于
            // 根内。
            canonical_under_workspace: true,
        };
        let stat_of =
            |fd: &OwnedFd| rustix::fs::fstat(fd).map_err(|e| LocalWorkspaceError::Io(e.into()));
        if path.is_project_root() {
            let stat = stat_of(&root)?;
            return Ok(facts(true, Some(&stat), Some(PathKind::Directory)));
        }
        let (parent_rel, _) = split_parent_leaf(path);
        let parent_kind = open_beneath(&root, parent_rel, OFlags::PATH, Mode::empty())
            .ok()
            .and_then(|fd| rustix::fs::fstat(&fd).ok())
            .map(|st| stat_kind(&st));
        match open_beneath(&root, path.as_str(), OFlags::PATH, Mode::empty()) {
            Ok(fd) => {
                let stat = stat_of(&fd)?;
                Ok(facts(true, Some(&stat), parent_kind))
            }
            Err(Errno::NOENT) => Ok(facts(false, None, parent_kind)),
            Err(e) => Err(resolve_error(e, path.as_str())),
        }
    }

    /// 打开普通文件用于读取。以非阻塞方式打开，使 FIFO 或设备无法让它挂起；
    /// 凡不是普通文件的对象，都依据已打开 fd 自身的 `fstat` 拒绝。
    fn open_regular_file(
        &self,
        path: &WorkspacePath,
    ) -> Result<(fs::File, u64), LocalWorkspaceError> {
        path.require_file_path()?;
        let root = self.open_root()?;
        before_open(Path::new(path.as_str()));
        let fd = open_beneath(&root, path.as_str(), file_read_flags(), Mode::empty())
            .map_err(|e| resolve_error(e, path.as_str()))?;
        let stat = rustix::fs::fstat(&fd).map_err(|e| LocalWorkspaceError::Io(e.into()))?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
            return Err(LocalWorkspaceError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "not a regular file",
            )));
        }
        Ok((fs::File::from(fd), stat.st_size as u64))
    }

    /// 打开显式请求的目录（list 目标或 grep/find 的根），并连同项目根 fd 一起
    /// 返回，供递归遍历从根解析子项。请求路径中任一处的符号链接都会被拒绝。
    fn open_dir(&self, path: &WorkspacePath) -> Result<(OwnedFd, OwnedFd), LocalWorkspaceError> {
        let root = self.open_root()?;
        before_open(Path::new(path.as_str()));
        let dir = open_beneath(&root, path.as_str(), dir_flags(), Mode::empty())
            .map_err(|e| resolve_error(e, path.as_str()))?;
        Ok((root, dir))
    }

    /// 读取工作区内的文件并返回其字节。同步形式。
    pub fn read_file_blocking(&self, path: &WorkspacePath) -> Result<Vec<u8>, LocalWorkspaceError> {
        let (mut file, size) = self.open_regular_file(path)?;
        let mut bytes = Vec::with_capacity(size as usize);
        file.read_to_end(&mut bytes)?;
        Ok(bytes)
    }

    /// 读取普通文件，分配的内存绝不超过 `allowed + 1` 字节。读取前先依据已打开
    /// fd 的大小拒绝已经超限的文件；有上限的读取则是对打开后仍在增长的文件的
    /// 事后检查。
    pub fn read_file_bounded_blocking(
        &self,
        path: &WorkspacePath,
        allowed: usize,
    ) -> Result<BoundedFileRead, LocalWorkspaceError> {
        let (file, size) = self.open_regular_file(path)?;
        if size as usize > allowed {
            return Ok(BoundedFileRead::LimitExceeded {
                observed: size as usize,
                allowed,
            });
        }
        let probe = allowed.saturating_add(1) as u64;
        let mut bytes = Vec::with_capacity(allowed.min(64 * 1024));
        file.take(probe).read_to_end(&mut bytes)?;
        if bytes.len() > allowed {
            Ok(BoundedFileRead::LimitExceeded {
                observed: bytes.len(),
                allowed,
            })
        } else {
            Ok(BoundedFileRead::Complete(bytes))
        }
    }

    /// Read a file under the workspace, returning its bytes.
    pub async fn read_file(&self, path: &WorkspacePath) -> Result<Vec<u8>, LocalWorkspaceError> {
        path.require_file_path()?;
        let ws = self.blocking_clone();
        let path = path.clone();
        tokio::task::spawn_blocking(move || ws.read_file_blocking(&path))
            .await
            .map_err(|e| LocalWorkspaceError::Io(std::io::Error::other(e)))?
    }

    /// List directory entries directly beneath `path` (non-recursive).
    /// Synchronous form.
    pub fn list_dir_blocking(
        &self,
        path: &WorkspacePath,
    ) -> Result<Vec<DirectoryEntry>, LocalWorkspaceError> {
        let (_, fd) = self.open_dir(path)?;
        let mut dir = Dir::new(fd).map_err(|e| LocalWorkspaceError::Io(e.into()))?;
        let mut entries = Vec::new();
        while let Some(entry) = next_entry(&mut dir) {
            let entry = entry?;
            // `d_type`（文件系统不报告时退回不跟随链接的 `statat`）从不跟随
            // 符号链接，因此符号链接（包括悬空链接）列为 `Symlink`，既不会让
            // 整个列举出错，也不会伪装成其目标。
            let ft = match entry.file_type() {
                FileType::Unknown => entry_stat(&dir, &entry)?.0,
                ft => ft,
            };
            entries.push(DirectoryEntry {
                name: entry.file_name().to_string_lossy().into_owned(),
                kind: kind_of(ft),
            });
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }

    /// List directory entries directly beneath `path` (non-recursive).
    pub async fn list_dir(
        &self,
        path: &WorkspacePath,
    ) -> Result<Vec<DirectoryEntry>, LocalWorkspaceError> {
        let ws = self.blocking_clone();
        let path = path.clone();
        tokio::task::spawn_blocking(move || ws.list_dir_blocking(&path))
            .await
            .map_err(|e| LocalWorkspaceError::Io(std::io::Error::other(e)))?
    }

    /// Grep a directory subtree, returning up to `max_results` matches.
    ///
    /// Hidden paths (Policy-Visible Workspace) produce no matches and are not
    /// counted toward truncation.
    ///
    /// Byte-budget truncation semantics: files larger than
    /// [`GREP_MAX_FILE_BYTES`] are silently skipped (exactly like unreadable
    /// files), scanning stops once [`GREP_MAX_TOTAL_BYTES`] of content has
    /// been read across the walk, and lines longer than
    /// [`GREP_MAX_LINE_BYTES`] are consumed against the budget but never
    /// matched. Callers may therefore see fewer matches than exist on disk
    /// even when the result is not `max_results`-truncated.
    pub fn grep_blocking(
        &self,
        req: &GrepRequest,
        max_results: usize,
        visibility: VisibilityEngine,
    ) -> Result<(Vec<GrepMatch>, VisibilityEvidence), LocalWorkspaceError> {
        self.grep_blocking_cancellable(req, max_results, visibility, || false)
    }

    pub(crate) fn grep_blocking_cancellable(
        &self,
        req: &GrepRequest,
        max_results: usize,
        mut visibility: VisibilityEngine,
        is_cancelled: impl Fn() -> bool,
    ) -> Result<(Vec<GrepMatch>, VisibilityEvidence), LocalWorkspaceError> {
        if is_cancelled() {
            return Err(LocalWorkspaceError::Cancelled);
        }
        let (root, dir) = self.open_dir(&req.path)?;
        let re = Regex::new(&req.pattern)
            .map_err(std::io::Error::other)
            .map_err(LocalWorkspaceError::Io)?;
        let mut out = Vec::new();
        let mut budget = GrepBudget::default();
        grep_walk(
            &root,
            dir,
            req.path.as_str().as_ref(),
            &re,
            req.glob.as_deref(),
            max_results,
            &mut budget,
            &mut visibility,
            &mut out,
            &is_cancelled,
        )?;
        Ok((out, visibility.evidence()))
    }

    /// Grep a directory subtree, returning up to `max_results` matches.
    pub async fn grep(
        &self,
        req: &GrepRequest,
        max_results: usize,
        visibility: VisibilityEngine,
    ) -> Result<(Vec<GrepMatch>, VisibilityEvidence), LocalWorkspaceError> {
        let ws = self.blocking_clone();
        let req = req.clone();
        tokio::task::spawn_blocking(move || ws.grep_blocking(&req, max_results, visibility))
            .await
            .map_err(|e| LocalWorkspaceError::Io(std::io::Error::other(e)))?
    }

    /// Find files matching a glob beneath a directory subtree. Synchronous form.
    ///
    /// Hidden paths are omitted from results and truncation counts.
    pub fn find_files_blocking(
        &self,
        req: &FindRequest,
        max_results: usize,
        visibility: VisibilityEngine,
    ) -> Result<(Vec<WorkspacePath>, VisibilityEvidence), LocalWorkspaceError> {
        self.find_files_blocking_cancellable(req, max_results, visibility, || false)
    }

    pub(crate) fn find_files_blocking_cancellable(
        &self,
        req: &FindRequest,
        max_results: usize,
        mut visibility: VisibilityEngine,
        is_cancelled: impl Fn() -> bool,
    ) -> Result<(Vec<WorkspacePath>, VisibilityEvidence), LocalWorkspaceError> {
        if is_cancelled() {
            return Err(LocalWorkspaceError::Cancelled);
        }
        let (root, dir) = self.open_dir(&req.path)?;
        let mut out = Vec::new();
        find_walk(
            &root,
            dir,
            req.path.as_str().as_ref(),
            &req.pattern,
            max_results,
            &mut visibility,
            &mut out,
            &is_cancelled,
        )?;
        Ok((out, visibility.evidence()))
    }

    /// Find files matching a glob beneath a directory subtree.
    ///
    /// Hidden paths are omitted from results and truncation counts.
    pub async fn find_files(
        &self,
        req: &FindRequest,
        max_results: usize,
        visibility: VisibilityEngine,
    ) -> Result<(Vec<WorkspacePath>, VisibilityEvidence), LocalWorkspaceError> {
        let ws = self.blocking_clone();
        let req = req.clone();
        tokio::task::spawn_blocking(move || ws.find_files_blocking(&req, max_results, visibility))
            .await
            .map_err(|e| LocalWorkspaceError::Io(std::io::Error::other(e)))?
    }

    /// Non-cancellable no-replace create; maps installation errors to workspace errors.
    pub fn create_excl_sync(
        &self,
        path: &WorkspacePath,
        content: &[u8],
        create_parents: bool,
    ) -> Result<CreatedFile, LocalWorkspaceError> {
        match self.create_excl_sync_cancellable(path, content, create_parents, || false) {
            Ok(created) => Ok(created),
            Err(ConditionalCreateError::Workspace(err)) => Err(err),
            Err(ConditionalCreateError::Cancelled) => Err(LocalWorkspaceError::Io(
                std::io::Error::other("unexpected cancellation in non-cancellable create"),
            )),
            Err(ConditionalCreateError::InstalledButNotDurable { error, .. }) => Err(error),
        }
    }

    /// Atomic no-replace create with a call-scoped cancellation probe checked
    /// before parent creation, before preparing the temporary inode, and
    /// immediately before the no-replace link installs the final name.
    pub(crate) fn create_excl_sync_cancellable(
        &self,
        path: &WorkspacePath,
        content: &[u8],
        create_parents: bool,
        is_cancelled: impl Fn() -> bool,
    ) -> Result<CreatedFile, ConditionalCreateError> {
        path.require_file_path()
            .map_err(LocalWorkspaceError::from)?;
        let facts = self.resolve_facts_blocking(path, PathIntent::Create)?;
        #[cfg(any(test, feature = "test-fault-injection"))]
        if self.fail_install.swap(false, Ordering::SeqCst) {
            return Err(
                LocalWorkspaceError::Io(std::io::Error::other("injected install failure")).into(),
            );
        }
        if facts.exists {
            return Err(LocalWorkspaceError::TargetExists {
                normalized: path.as_str().to_string(),
            }
            .into());
        }
        if !create_parents && !facts.parent_exists {
            return Err(LocalWorkspaceError::NotFound {
                normalized: path.as_str().to_string(),
            }
            .into());
        }
        let (_, leaf) = split_parent_leaf(path);
        if is_cancelled() {
            return Err(ConditionalCreateError::Cancelled);
        }
        let parent_fd = self.open_parent_dir(path, create_parents)?;
        if is_cancelled() {
            return Err(ConditionalCreateError::Cancelled);
        }
        let (tmp_name, tmp_fd) = claim_install_temp(&parent_fd)?;
        let mut tmp_file = fs::File::from(tmp_fd);
        let prepared = (|| -> Result<(), ConditionalCreateError> {
            tmp_file.write_all(content)?;
            tmp_file.sync_all()?;
            if is_cancelled() {
                return Err(ConditionalCreateError::Cancelled);
            }
            // POSIX linkat is an atomic no-replace name installation: a
            // concurrent target (including a symlink) yields EEXIST, while
            // the final name can never expose the partially written temp.
            rustix::fs::linkat(
                &parent_fd,
                tmp_name.as_str(),
                &parent_fd,
                leaf,
                AtFlags::empty(),
            )
            .map_err(|err| {
                if err == Errno::EXIST {
                    ConditionalCreateError::Workspace(LocalWorkspaceError::TargetExists {
                        normalized: path.as_str().to_string(),
                    })
                } else {
                    ConditionalCreateError::Workspace(LocalWorkspaceError::Io(
                        std::io::Error::other(format!("atomic no-replace persist failed: {err}")),
                    ))
                }
            })?;
            Ok(())
        })();
        if let Err(err) = prepared {
            let _ = rustix::fs::unlinkat(&parent_fd, tmp_name.as_str(), AtFlags::empty());
            return Err(err);
        }
        drop(tmp_file);
        // The final target is already complete. Temp-name cleanup is
        // best-effort because reporting a failure here would falsely claim
        // that the installed mutation did not happen.
        let _ = rustix::fs::unlinkat(&parent_fd, tmp_name.as_str(), AtFlags::empty());
        let created = CreatedFile {
            path: path.clone(),
            content_hash: content_hash(content),
            bytes_written: content.len() as u64,
        };
        if let Err(error) = self.sync_parent_dir(&parent_fd) {
            return Err(ConditionalCreateError::InstalledButNotDurable { created, error });
        }
        Ok(created)
    }

    /// 通过已打开的父目录 fd 执行不可取消的原子安装。
    /// 调用方必须已经检查变更前置条件。
    pub fn install_atomic_sync(
        &self,
        path: &WorkspacePath,
        content: &[u8],
    ) -> Result<InstalledFile, LocalWorkspaceError> {
        let _guard = self.mutation_lock.lock().unwrap_or_else(|e| e.into_inner());
        let receipt = self.install_atomic_sync_locked(path, content)?;
        match receipt.durability_error {
            Some(error) => Err(error),
            None => Ok(receipt.installed),
        }
    }

    /// Host-only conditional replacement. The caller supplies the call-scoped
    /// cancellation probe; the concrete workspace owns the final revision
    /// check and atomic install under one LATTICE mutation lock.
    pub(crate) fn replace_if_revision_sync(
        &self,
        path: &WorkspacePath,
        expected_revision: &str,
        content: &[u8],
        is_cancelled: impl Fn() -> bool,
    ) -> Result<RevisionMatchedInstall, ConditionalReplaceError> {
        let _guard = self.mutation_lock.lock().unwrap_or_else(|e| e.into_inner());
        if is_cancelled() {
            return Err(ConditionalReplaceError::Cancelled);
        }

        #[cfg(any(test, feature = "test-fault-injection"))]
        if let Some((race_path, race_content)) = self.replace_before_check.lock().unwrap().take() {
            debug_assert_eq!(&race_path, path);
            fs::write(self.root.join(race_path.as_str()), race_content)
                .map_err(LocalWorkspaceError::from)
                .map_err(ConditionalReplaceError::Workspace)?;
        }

        let before = self
            .read_file_blocking(path)
            .map_err(ConditionalReplaceError::Workspace)?;
        let observed = content_hash(&before);
        if observed != expected_revision {
            return Err(ConditionalReplaceError::StaleRevision { observed });
        }
        if is_cancelled() {
            return Err(ConditionalReplaceError::Cancelled);
        }
        let receipt = self
            .install_atomic_sync_locked(path, content)
            .map_err(ConditionalReplaceError::Workspace)?;
        let replacement = RevisionMatchedInstall {
            before,
            installed: receipt.installed,
        };
        if let Some(error) = receipt.durability_error {
            return Err(ConditionalReplaceError::InstalledButNotDurable { replacement, error });
        }
        Ok(replacement)
    }

    /// Atomic install implementation. The caller holds `mutation_lock`.
    fn install_atomic_sync_locked(
        &self,
        path: &WorkspacePath,
        content: &[u8],
    ) -> Result<AtomicInstallReceipt, LocalWorkspaceError> {
        path.require_file_path()?;
        let ws = self;
        // 事实本身不使用，但先解析可让静态拒绝（符号链接叶节点或父目录）
        // 先于任何父目录创建发生。
        ws.resolve_facts_blocking(path, PathIntent::Overwrite)?;
        #[cfg(any(test, feature = "test-fault-injection"))]
        if ws.fail_install.swap(false, Ordering::SeqCst) {
            return Err(LocalWorkspaceError::Io(std::io::Error::other(
                "injected install failure",
            )));
        }
        let (_, leaf) = split_parent_leaf(path);
        // `create_missing=true` 保留安装路径历来的 `create_dir_all` 行为。
        let parent_fd = ws.open_parent_dir(path, true)?;
        // Unique single-component temp name: pid disambiguates across
        // processes sharing a workspace, the sequence number across
        // concurrent installs into the same directory in this process.
        //
        // Neither is collision-proof on its own — a crashed process can
        // leave `.lattice-tmp-<pid>-<seq>` residue that a later process
        // recycling that pid walks into, and two PID namespaces over one
        // bind-mounted workspace can both start at the same pid. `O_EXCL`
        // detects that; retrying with a fresh sequence number resolves it
        // rather than surfacing a bare `EEXIST`.
        let (tmp_name, tmp_fd) = claim_install_temp(&parent_fd)?;
        let mut tmp_file = fs::File::from(tmp_fd);
        let installed = (|| -> Result<(), LocalWorkspaceError> {
            tmp_file.write_all(content)?;
            tmp_file.sync_all()?;
            // 两个名称都是相对同一父目录 fd 的单段名，rename 两侧都不可能
            // 经过符号链接。
            rustix::fs::renameat(&parent_fd, tmp_name.as_str(), &parent_fd, leaf).map_err(|e| {
                LocalWorkspaceError::Io(std::io::Error::other(format!(
                    "atomic persist failed: {e}"
                )))
            })?;
            Ok(())
        })();
        if let Err(err) = installed {
            // Best-effort temp hygiene on any failure after creation;
            // the rename consumed the temp name on success.
            let _ = rustix::fs::unlinkat(&parent_fd, tmp_name.as_str(), AtFlags::empty());
            return Err(err);
        }
        drop(tmp_file);
        // 通过父目录 fd 把 rename 持久记录到目录本身，而不是按路径重新打开
        // 目录。
        let installed = InstalledFile {
            path: path.clone(),
            content_hash: content_hash(content),
            bytes_written: content.len() as u64,
        };
        let durability_error = self.sync_parent_dir(&parent_fd).err();
        Ok(AtomicInstallReceipt {
            installed,
            durability_error,
        })
    }

    /// Flip a one-shot flag so the next create or replacement fails before
    /// any bytes are written. Narrow test-only
    /// fault injection proving the install failure boundary leaves the
    /// target unchanged — a single boolean, not a composable plugin seam.
    #[cfg(any(test, feature = "test-fault-injection"))]
    pub fn fail_next_install(&self) {
        self.fail_install.store(true, Ordering::SeqCst);
    }

    /// Fail the next parent-directory fsync after the target was installed.
    #[cfg(any(test, feature = "test-fault-injection"))]
    pub fn fail_next_parent_fsync(&self) {
        self.fail_parent_fsync.store(true, Ordering::SeqCst);
    }

    /// Simulate a non-cooperating writer changing `path` immediately before
    /// the next conditional replacement's final revision check.
    #[cfg(any(test, feature = "test-fault-injection"))]
    pub fn replace_target_before_next_revision_check(&self, path: WorkspacePath, content: Vec<u8>) {
        *self.replace_before_check.lock().unwrap() = Some((path, content));
    }

    fn sync_parent_dir(&self, parent_fd: &OwnedFd) -> Result<(), LocalWorkspaceError> {
        #[cfg(any(test, feature = "test-fault-injection"))]
        if self.fail_parent_fsync.swap(false, Ordering::SeqCst) {
            return Err(LocalWorkspaceError::Io(std::io::Error::other(
                "injected parent fsync failure after install",
            )));
        }
        rustix::fs::fsync(parent_fd).map_err(|err| LocalWorkspaceError::Io(err.into()))
    }
}

/// 把原始文件类型映射到 Local 的 `PathKind` 词汇。既不是符号链接也不是目录
/// 的一律视为 `File`；读取方在打开内容前还要求它是普通文件。
fn kind_of(ft: FileType) -> PathKind {
    match ft {
        FileType::Symlink => PathKind::Symlink,
        FileType::Directory => PathKind::Directory,
        _ => PathKind::File,
    }
}

fn stat_kind(stat: &rustix::fs::Stat) -> PathKind {
    kind_of(FileType::from_raw_mode(stat.st_mode))
}

/// Mode for files created by the write path (`0o644`, further narrowed by
/// the process umask — the same policy `OpenOptions` applied before).
const CREATE_FILE_MODE: Mode = Mode::from_raw_mode(0o644);

/// Monotonic per-process counter making install temp names unique when
/// concurrent installs target the same directory.
static INSTALL_TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// How many temp names an install may try before giving up. Only reached when
/// `O_EXCL` keeps hitting leftover `.lattice-tmp-*` residue from a crashed
/// process that shared this pid, or a second PID namespace over the same
/// workspace; a handful of fresh sequence numbers clears both.
const INSTALL_TMP_NAME_ATTEMPTS: usize = 8;

/// Claim a unique same-directory temporary file for atomic installation.
fn claim_install_temp(parent_fd: &OwnedFd) -> Result<(String, OwnedFd), LocalWorkspaceError> {
    let tmp_flags =
        OFlags::CREATE | OFlags::EXCL | OFlags::WRONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut last = None;
    for _ in 0..INSTALL_TMP_NAME_ATTEMPTS {
        let candidate = format!(
            ".lattice-tmp-{}-{}",
            std::process::id(),
            INSTALL_TMP_SEQ.fetch_add(1, Ordering::Relaxed)
        );
        match rustix::fs::openat(parent_fd, candidate.as_str(), tmp_flags, CREATE_FILE_MODE) {
            Ok(fd) => return Ok((candidate, fd)),
            Err(err) if err == Errno::EXIST => last = Some(err),
            Err(err) => return Err(LocalWorkspaceError::Io(err.into())),
        }
    }
    Err(LocalWorkspaceError::Io(std::io::Error::other(format!(
        "could not claim an install temp name after {INSTALL_TMP_NAME_ATTEMPTS} attempts: {}",
        last.map(|err| err.to_string()).unwrap_or_default()
    ))))
}

/// 这是本模块把工作区路径解析为文件系统对象的唯一途径（#233）。`rel` 相对于
/// 项目根（`"."` 即项目根本身），且已由 `WorkspacePath` 解析排除了 `..` 和
/// 前导 `/`。
///
/// `RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS` 让内核在这一次调用中拒绝任何符号
/// 链接路径段（`ELOOP`）和任何离开项目根的解析（`EXDEV`），因此返回的 fd 指向
/// 的对象在打开那一刻位于根下，且到达它的路径上没有符号链接。之后的一切操作
/// 都通过该 fd 进行。若该对象之后被其他进程移出项目根，fd 仍指向同一对象；这是
/// `docs/architecture/workspace.md` 中记录的已接受边界。
///
/// 没有回退：内核不支持 `openat2`（`ENOSYS`）或沙箱禁止它（`EPERM`）时，操作
/// 直接失败。
fn open_beneath(
    root: &OwnedFd,
    rel: impl rustix::path::Arg,
    oflags: OFlags,
    mode: Mode,
) -> rustix::io::Result<OwnedFd> {
    rustix::fs::openat2(
        root,
        rel,
        oflags | OFlags::CLOEXEC,
        mode,
        ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS,
    )
}

/// 打开目录以枚举或在其中操作时使用的标志。
fn dir_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY
}

/// 打开待读内容时使用的标志。非阻塞使打开时被换入的 FIFO 无法挂起打开操作；
/// 它对普通文件的读取没有影响。
fn file_read_flags() -> OFlags {
    OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOCTTY
}

/// 把请求 `requested` 的 [`open_beneath`] 失败映射为工作区错误。
///
/// `openat2` 不指出哪个路径段是符号链接，因此拒绝信息给出完整的根相对请求。
/// 它从不包含 Host 路径：`host_mutate` 会把这段 `Display` 文本转发给 guest
/// （#98）。`EAGAIN`（只在路径含 `..` 时才可能出现，而解析阶段已拒绝 `..`）
/// 不重试。
fn resolve_error(err: Errno, requested: &str) -> LocalWorkspaceError {
    match err {
        Errno::LOOP => LocalWorkspaceError::SymlinkRejected {
            component: PathBuf::from(requested),
        },
        Errno::XDEV => LocalWorkspaceError::RootEscape,
        Errno::NOENT => LocalWorkspaceError::NotFound {
            normalized: requested.to_string(),
        },
        Errno::NOSYS => LocalWorkspaceError::Io(std::io::Error::other(
            "openat2 unavailable: workspace access requires Linux 5.6 or later",
        )),
        other => LocalWorkspaceError::Io(other.into()),
    }
}

/// 返回下一个目录条目，跳过 `.` 和 `..`（`std::fs::read_dir` 会隐藏它们，
/// rustix 的 `Dir` 不会）。
fn next_entry(dir: &mut Dir) -> Option<Result<rustix::fs::DirEntry, LocalWorkspaceError>> {
    loop {
        match dir.next()? {
            Ok(entry) if matches!(entry.file_name().to_bytes(), b"." | b"..") => continue,
            Ok(entry) => return Some(Ok(entry)),
            Err(e) => return Some(Err(LocalWorkspaceError::Io(e.into()))),
        }
    }
}

/// 相对正在枚举的目录，以不跟随链接的方式取得单个条目的类型和大小。这里只做
/// 分类；任何要进入或读取的对象都会通过 [`open_beneath`] 从项目根重新打开。
fn entry_stat(
    dir: &Dir,
    entry: &rustix::fs::DirEntry,
) -> Result<(FileType, u64), LocalWorkspaceError> {
    let fd = dir.fd().map_err(|e| LocalWorkspaceError::Io(e.into()))?;
    let stat = rustix::fs::statat(fd, entry.file_name(), AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|e| LocalWorkspaceError::Io(e.into()))?;
    Ok((FileType::from_raw_mode(stat.st_mode), stat.st_size as u64))
}

/// 按完整的根相对路径，从项目根打开 grep/find 遍历中发现的子项。若该子项已
/// 不再是可不经符号链接到达的目录或文件（`ELOOP`、`ENOTDIR`、`ENOENT`），就
/// 跳过（返回 `None`），与枚举时遇到符号链接条目的处理一致。离开项目根
/// （`EXDEV`）及其他任何错误都会使遍历失败。
fn open_walk_child(
    root: &OwnedFd,
    rel: &OsStr,
    oflags: OFlags,
) -> Result<Option<OwnedFd>, LocalWorkspaceError> {
    match open_beneath(root, rel, oflags, Mode::empty()) {
        Ok(fd) => Ok(Some(fd)),
        Err(Errno::LOOP | Errno::NOTDIR | Errno::NOENT) => Ok(None),
        Err(e) => Err(resolve_error(e, &rel.to_string_lossy())),
    }
}

/// 被遍历目录 `current` 中 `name` 的根相对路径。
fn child_rel(current: &OsStr, name: &std::ffi::CStr) -> OsString {
    let name = OsStr::from_bytes(name.to_bytes());
    if current == "." {
        return name.to_os_string();
    }
    let mut rel = current.to_os_string();
    rel.push("/");
    rel.push(name);
    rel
}

/// 把工作区文件路径拆分为（相对项目根的父路径，项目根本身记为 `"."`；单段
/// 叶名）。
fn split_parent_leaf(path: &WorkspacePath) -> (&str, &str) {
    path.as_str()
        .rsplit_once('/')
        .unwrap_or((".", path.as_str()))
}

/// Per-file grep size cap. Files whose on-disk size exceeds this are skipped
/// outright — before being opened — exactly like unreadable files.
const GREP_MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;

/// Aggregate byte budget for one grep walk. Once this many content bytes
/// have been read across all scanned files, the walk stops descending and
/// scanning, bounding worst-case RSS/IO independently of `max_results`.
const GREP_MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024;

/// Longest line eligible for matching. Longer lines are consumed against the
/// budget but never matched or returned, so a single pathological line
/// cannot balloon a `GrepMatch` payload.
const GREP_MAX_LINE_BYTES: usize = 64 * 1024;

/// Byte budget threaded through one grep walk.
///
/// `per_file_bytes` is the fixed per-file cap; `remaining_total_bytes`
/// drains as file content is read. Both bound bytes *read*, not bytes
/// matched, so memory stays proportional to the budget regardless of file
/// or match sizes.
struct GrepBudget {
    /// Fixed cap on bytes read from any single file.
    per_file_bytes: u64,
    /// Aggregate bytes still available across the rest of the walk.
    remaining_total_bytes: u64,
}

impl GrepBudget {
    fn new(per_file_bytes: u64, total_bytes: u64) -> Self {
        Self {
            per_file_bytes,
            remaining_total_bytes: total_bytes,
        }
    }

    /// True once the aggregate budget has been fully consumed.
    fn exhausted(&self) -> bool {
        self.remaining_total_bytes == 0
    }

    /// Bytes the next file may read: the per-file cap clamped by whatever
    /// remains of the aggregate budget.
    fn file_allowance(&self) -> u64 {
        self.per_file_bytes.min(self.remaining_total_bytes)
    }

    /// Consume `bytes` from the aggregate budget (saturating).
    fn charge(&mut self, bytes: u64) {
        self.remaining_total_bytes = self.remaining_total_bytes.saturating_sub(bytes);
    }
}

impl Default for GrepBudget {
    fn default() -> Self {
        Self::new(GREP_MAX_FILE_BYTES, GREP_MAX_TOTAL_BYTES)
    }
}

// Recursive walk plumbing: every parameter is threaded unchanged through the
// recursion, so a bundling struct would only rename the argument list.
#[allow(clippy::too_many_arguments)]
fn grep_walk(
    root: &OwnedFd,
    dir: OwnedFd,
    current: &OsStr,
    re: &Regex,
    glob: Option<&str>,
    max_results: usize,
    budget: &mut GrepBudget,
    visibility: &mut VisibilityEngine,
    out: &mut Vec<GrepMatch>,
    is_cancelled: &impl Fn() -> bool,
) -> Result<(), LocalWorkspaceError> {
    if is_cancelled() {
        return Err(LocalWorkspaceError::Cancelled);
    }
    if out.len() >= max_results || budget.exhausted() {
        return Ok(());
    }
    let mut dir = Dir::new(dir).map_err(|e| LocalWorkspaceError::Io(e.into()))?;
    while let Some(entry) = next_entry(&mut dir) {
        if is_cancelled() {
            return Err(LocalWorkspaceError::Cancelled);
        }
        if out.len() >= max_results || budget.exhausted() {
            break;
        }
        let entry = entry?;
        let (ft, size) = entry_stat(&dir, &entry)?;
        let child = child_rel(current, entry.file_name());
        let rel = child.to_string_lossy();
        if ft == FileType::Directory {
            if visibility.should_descend(&rel) {
                before_open(Path::new(&child));
                if let Some(fd) = open_walk_child(root, &child, dir_flags())? {
                    grep_walk(
                        root,
                        fd,
                        &child,
                        re,
                        glob,
                        max_results,
                        budget,
                        visibility,
                        out,
                        is_cancelled,
                    )?;
                }
            }
        } else if ft == FileType::RegularFile {
            if !visibility.is_visible_file(&rel) {
                continue;
            }
            if let Some(g) = glob {
                let name = entry.file_name().to_str().unwrap_or("");
                if !glob_match(g, name) {
                    continue;
                }
            }
            // Over-cap files are skipped before being opened, exactly like
            // unreadable files below.
            if size > budget.per_file_bytes {
                continue;
            }
            before_open(Path::new(&child));
            // 不可读的文件静默跳过，保持原有语义；枚举后被换成符号链接或被删除
            // 的文件也属此类。离开项目根仍会使遍历失败。
            let fd = match open_beneath(root, &child, file_read_flags(), Mode::empty()) {
                Ok(fd) => fd,
                Err(Errno::XDEV) => return Err(LocalWorkspaceError::RootEscape),
                Err(_) => continue,
            };
            // 分类之后才被换入的 FIFO 或设备，与静态存在的一样跳过。
            let regular = rustix::fs::fstat(&fd)
                .is_ok_and(|st| FileType::from_raw_mode(st.st_mode) == FileType::RegularFile);
            if !regular {
                continue;
            }
            grep_file(
                &rel,
                fs::File::from(fd),
                re,
                max_results,
                budget,
                out,
                is_cancelled,
            )?;
        }
    }
    Ok(())
}

/// Scan one file line-by-line under the walk budget.
///
/// The reader is hard-capped at the budget's current file allowance via
/// [`Read::take`], so a file can never cost more than `per_file_bytes` (or
/// the remaining aggregate budget, if smaller) regardless of its size on
/// disk. Every byte read is charged to the budget. Lines longer than
/// [`GREP_MAX_LINE_BYTES`] are consumed but never matched. An IO or
/// invalid-UTF-8 error mid-file stops the scan of that file while keeping
/// the matches already found in it — a deliberate improvement over the old
/// whole-file `read_to_string`, where a single invalid byte dropped every
/// match in the file.
fn grep_file(
    rel: &str,
    file: fs::File,
    re: &Regex,
    max_results: usize,
    budget: &mut GrepBudget,
    out: &mut Vec<GrepMatch>,
    is_cancelled: &impl Fn() -> bool,
) -> Result<(), LocalWorkspaceError> {
    let allowance = budget.file_allowance();
    let mut reader = BufReader::new(file.take(allowance));
    let mut buf = String::new();
    let mut consumed: u64 = 0;
    let mut line_number: u64 = 0;
    loop {
        if is_cancelled() {
            return Err(LocalWorkspaceError::Cancelled);
        }
        if out.len() >= max_results {
            break;
        }
        buf.clear();
        let read = match reader.read_line(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            // IO / invalid-UTF-8 mid-file: keep matches already found in
            // this file, stop scanning it.
            Err(_) => break,
        };
        consumed += read as u64;
        budget.charge(read as u64);
        line_number += 1;
        // A line without a trailing newline that ends exactly at the
        // allowance was truncated by the cap; matching half a line would
        // fabricate results, so stop instead.
        if !buf.ends_with('\n') && consumed >= allowance {
            break;
        }
        let mut line = buf.as_str();
        if let Some(stripped) = line.strip_suffix('\n') {
            line = stripped;
        }
        if let Some(stripped) = line.strip_suffix('\r') {
            line = stripped;
        }
        if line.len() > GREP_MAX_LINE_BYTES {
            continue;
        }
        if re.is_match(line) {
            let wp = WorkspacePath::parse(rel).map_err(LocalWorkspaceError::Path)?;
            out.push(GrepMatch {
                path: wp,
                line_number,
                line: line.to_string(),
            });
        }
    }
    Ok(())
}

// 递归遍历的参数传递，理由同 `grep_walk`。
#[allow(clippy::too_many_arguments)]
fn find_walk(
    root: &OwnedFd,
    dir: OwnedFd,
    current: &OsStr,
    pattern: &str,
    max_results: usize,
    visibility: &mut VisibilityEngine,
    out: &mut Vec<WorkspacePath>,
    is_cancelled: &impl Fn() -> bool,
) -> Result<(), LocalWorkspaceError> {
    if is_cancelled() {
        return Err(LocalWorkspaceError::Cancelled);
    }
    if out.len() >= max_results {
        return Ok(());
    }
    let mut dir = Dir::new(dir).map_err(|e| LocalWorkspaceError::Io(e.into()))?;
    while let Some(entry) = next_entry(&mut dir) {
        if is_cancelled() {
            return Err(LocalWorkspaceError::Cancelled);
        }
        if out.len() >= max_results {
            break;
        }
        let entry = entry?;
        let name = entry.file_name().to_string_lossy();
        let child = child_rel(current, entry.file_name());
        let rel = child.to_string_lossy();
        let is_dir = entry_stat(&dir, &entry)?.0 == FileType::Directory;
        let assessment = visibility.assess(&rel, is_dir);
        if glob_match(pattern, &name) && assessment.is_visible() {
            out.push(WorkspacePath::parse(&rel).map_err(LocalWorkspaceError::Path)?);
        }
        if is_dir && assessment.should_descend() {
            before_open(Path::new(&child));
            if let Some(fd) = open_walk_child(root, &child, dir_flags())? {
                find_walk(
                    root,
                    fd,
                    &child,
                    pattern,
                    max_results,
                    visibility,
                    out,
                    is_cancelled,
                )?;
            }
        }
    }
    Ok(())
}

/// Fnmatch-style glob matcher backing `find_files` patterns and grep's glob
/// file filter.
///
/// Supports `*` (any sequence, including empty), `?` (exactly one
/// character), and character classes `[abc]`, `[a-z]`, `[!...]`. An
/// unclosed `[` is treated as a literal `[`. Matches over Unicode scalar
/// values using the standard iterative two-pointer backtracking algorithm —
/// no recursion, no dependency, linear-space, worst-case O(pattern × name).
fn glob_match(pattern: &str, name: &str) -> bool {
    let pat: Vec<char> = pattern.chars().collect();
    let txt: Vec<char> = name.chars().collect();
    let mut p = 0usize;
    let mut t = 0usize;
    // Most recent `*` in the pattern and the text position just after the
    // span it currently absorbs; on mismatch we backtrack here and widen
    // the span by one character.
    let mut star: Option<usize> = None;
    let mut star_t = 0usize;
    while t < txt.len() {
        if p < pat.len() {
            match pat[p] {
                '*' => {
                    star = Some(p);
                    star_t = t;
                    p += 1;
                    continue;
                }
                '?' => {
                    p += 1;
                    t += 1;
                    continue;
                }
                '[' => {
                    if let Some((matched, next_p)) = match_char_class(&pat, p, txt[t]) {
                        if matched {
                            p = next_p;
                            t += 1;
                            continue;
                        }
                        // Well-formed class that does not match: fall
                        // through to backtracking.
                    } else if txt[t] == '[' {
                        // Unclosed class: literal '['.
                        p += 1;
                        t += 1;
                        continue;
                    }
                }
                c => {
                    if c == txt[t] {
                        p += 1;
                        t += 1;
                        continue;
                    }
                }
            }
        }
        // Mismatch (or pattern exhausted with text left): let the last `*`
        // absorb one more character, or fail if there is none.
        match star {
            Some(sp) => {
                star_t += 1;
                p = sp + 1;
                t = star_t;
            }
            None => return false,
        }
    }
    // Text consumed: the rest of the pattern must be all `*`.
    while p < pat.len() && pat[p] == '*' {
        p += 1;
    }
    p == pat.len()
}

/// Match `ch` against the character class opening at `pat[start]` (which
/// must be `[`).
///
/// Returns `None` when the class is never closed (the caller then treats
/// the `[` as a literal), otherwise `Some((matched, index past the closing
/// `]`))`. A leading `!` negates; a `]` immediately after `[` or `[!` is a
/// literal member (fnmatch convention); `a-z` ranges are inclusive.
fn match_char_class(pat: &[char], start: usize, ch: char) -> Option<(bool, usize)> {
    let mut i = start + 1;
    let negated = if i < pat.len() && pat[i] == '!' {
        i += 1;
        true
    } else {
        false
    };
    let mut matched = false;
    let mut first = true;
    while i < pat.len() {
        if pat[i] == ']' && !first {
            return Some((matched != negated, i + 1));
        }
        first = false;
        if i + 2 < pat.len() && pat[i + 1] == '-' && pat[i + 2] != ']' {
            if pat[i] <= ch && ch <= pat[i + 2] {
                matched = true;
            }
            i += 3;
        } else {
            if pat[i] == ch {
                matched = true;
            }
            i += 1;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use tempfile::TempDir;

    fn ws_at(dir: &TempDir) -> LocalWorkspace {
        LocalWorkspace::new(dir.path())
    }

    // #233：替换发生在公开操作内部、其路径/类型检查之后。调用前替换只会触发
    // 已有的静态拒绝。
    const RACE_CANARY: &str = "I233_EXTERNAL_CANARY_7b98\n";
    const RACE_NAME: &str = "I233_EXTERNAL_NAME_7b98.txt";

    fn with_swap<T>(trigger: &str, action: impl FnOnce() + 'static, op: impl FnOnce() -> T) -> T {
        let trigger = PathBuf::from(trigger);
        let fired = Arc::new(AtomicBool::new(false));
        let observed = Arc::clone(&fired);
        let mut action = Some(action);
        let _guard = set_before_open(move |rel| {
            if rel == trigger
                && let Some(action) = action.take()
            {
                observed.store(true, Ordering::Relaxed);
                action();
            }
        });
        let result = op();
        assert!(
            fired.load(Ordering::Relaxed),
            "swap insertion point was not reached"
        );
        result
    }

    #[derive(Clone, Copy)]
    enum Observation {
        Read,
        Bounded,
        List,
        Grep,
        Find,
    }

    fn observe(
        ws: &LocalWorkspace,
        mode: Observation,
        path: &str,
    ) -> Result<String, LocalWorkspaceError> {
        let path = WorkspacePath::parse(path).unwrap();
        let visibility = || VisibilityEngine::load(ws.root(), Default::default());
        match mode {
            Observation::Read => ws
                .read_file_blocking(&path)
                .map(|bytes| String::from_utf8(bytes).unwrap()),
            Observation::Bounded => {
                ws.read_file_bounded_blocking(&path, 4096)
                    .map(|read| match read {
                        BoundedFileRead::Complete(bytes) => String::from_utf8(bytes).unwrap(),
                        BoundedFileRead::LimitExceeded { .. } => {
                            panic!("fixture exceeds read limit")
                        }
                    })
            }
            Observation::List => ws
                .list_dir_blocking(&path)
                .map(|entries| format!("{entries:?}")),
            Observation::Grep => ws
                .grep_blocking(
                    &GrepRequest {
                        path,
                        pattern: "I233_EXTERNAL_CANARY".into(),
                        glob: None,
                    },
                    100,
                    visibility(),
                )
                .map(|(matches, _)| format!("{matches:?}")),
            Observation::Find => ws
                .find_files_blocking(
                    &FindRequest {
                        path,
                        pattern: "*".into(),
                    },
                    100,
                    visibility(),
                )
                .map(|(paths, _)| format!("{paths:?}")),
        }
    }

    fn assert_no_host_path(error: &LocalWorkspaceError, root: &Path) {
        let message = error.to_string();
        assert!(
            !message.contains(root.to_str().unwrap()),
            "refusal leaked workspace root: {message}"
        );
        assert!(
            !message.contains("/tmp"),
            "refusal leaked host path: {message}"
        );
    }

    fn assert_no_canary(result: &Result<String, LocalWorkspaceError>) {
        if let Ok(output) = result {
            assert!(
                !output.contains(RACE_CANARY.trim()) && !output.contains(RACE_NAME),
                "outside canary escaped: {output}"
            );
        }
    }

    fn symlink_swap_case(
        mode: Observation,
        request: &str,
        swapped: &str,
        file_leaf: bool,
        suffix: &str,
        explicit: bool,
    ) {
        let dir = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let ws = ws_at(&dir);
        let inside = dir.path().join(swapped);
        if matches!(mode, Observation::Read | Observation::Bounded) {
            let file = dir.path().join(request);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, b"inside\n").unwrap();
        } else {
            fs::create_dir_all(dir.path().join(request)).unwrap();
            if file_leaf {
                fs::write(&inside, b"inside\n").unwrap();
            } else {
                fs::create_dir_all(&inside).unwrap();
            }
        }
        let target = if file_leaf {
            outside.path().join(RACE_NAME)
        } else {
            outside.path().to_path_buf()
        };
        let canary_file = if file_leaf {
            target.clone()
        } else if matches!(mode, Observation::Read | Observation::Bounded) {
            target.join(suffix)
        } else {
            target.join(suffix).join(RACE_NAME)
        };
        fs::create_dir_all(canary_file.parent().unwrap()).unwrap();
        fs::write(&canary_file, RACE_CANARY).unwrap();
        let result = with_swap(
            if explicit { request } else { swapped },
            move || {
                if file_leaf {
                    fs::remove_file(&inside).unwrap();
                } else {
                    fs::remove_dir_all(&inside).unwrap();
                }
                symlink(target, inside).unwrap();
            },
            || observe(&ws, mode, request),
        );
        // 先检查外部对象完好，再断言返回内容。
        assert_eq!(
            fs::read_to_string(&canary_file).unwrap(),
            RACE_CANARY,
            "outside file changed"
        );
        assert_eq!(
            fs::read_dir(canary_file.parent().unwrap()).unwrap().count(),
            1,
            "outside directory changed"
        );
        assert_no_canary(&result);
        if explicit {
            let error = result.unwrap_err();
            assert_no_host_path(&error, ws.root());
            assert!(
                matches!(error, LocalWorkspaceError::SymlinkRejected { .. }),
                "expected SymlinkRejected, got {error:?}"
            );
        } else {
            result.expect("recursive swapped child must be skipped, not fail the operation");
        }
    }

    #[test]
    fn r1_read_rejects_leaf_symlink_swap() {
        symlink_swap_case(Observation::Read, "file", "file", true, "", true);
    }

    #[test]
    fn r1_read_bounded_rejects_leaf_symlink_swap() {
        symlink_swap_case(Observation::Bounded, "file", "file", true, "", true);
    }

    #[test]
    fn r2_read_rejects_first_directory_symlink_swap() {
        symlink_swap_case(Observation::Read, "a/file", "a", false, "file", true);
    }

    #[test]
    fn r3_read_rejects_middle_directory_symlink_swap() {
        symlink_swap_case(
            Observation::Read,
            "a/middle/file",
            "a/middle",
            false,
            "file",
            true,
        );
    }

    #[test]
    fn r4_list_rejects_requested_directory_symlink_swap() {
        symlink_swap_case(Observation::List, "target", "target", false, "", true);
    }

    #[test]
    fn r5_list_rejects_first_directory_symlink_swap() {
        symlink_swap_case(Observation::List, "a/target", "a", false, "target", true);
    }

    #[test]
    fn r6_grep_rejects_search_root_symlink_swap() {
        symlink_swap_case(Observation::Grep, "scan", "scan", false, "", true);
    }

    #[test]
    fn r7_grep_skips_recursive_directory_symlink_swap() {
        symlink_swap_case(Observation::Grep, "scan", "scan/child", false, "", false);
    }

    #[test]
    fn r8_grep_skips_recursive_file_symlink_swap() {
        symlink_swap_case(Observation::Grep, "scan", "scan/file", true, "", false);
    }

    #[test]
    fn r9_find_rejects_search_root_symlink_swap() {
        symlink_swap_case(Observation::Find, "scan", "scan", false, "", true);
    }

    #[test]
    fn r10_find_skips_recursive_directory_symlink_swap() {
        symlink_swap_case(Observation::Find, "scan", "scan/child", false, "", false);
    }

    fn moved_directory_case(rename_out: bool) {
        let dir = TempDir::new().unwrap();
        // 同一文件系统上的外部目录，供有意把对象 rename 出项目根时使用。
        let outside = TempDir::new_in(dir.path().parent().unwrap()).unwrap();
        let ws = ws_at(&dir);
        fs::create_dir_all(dir.path().join("a/middle")).unwrap();
        fs::write(dir.path().join("a/middle/file"), b"inside\n").unwrap();
        fs::write(outside.path().join(RACE_NAME), RACE_CANARY).unwrap();
        let middle = dir.path().join("a/middle");
        let moved = outside.path().join("moved");
        let moved_action = moved.clone();
        let result = with_swap(
            "a/middle/file",
            move || {
                if rename_out {
                    fs::rename(&middle, moved_action).unwrap();
                } else {
                    fs::remove_dir_all(&middle).unwrap();
                    fs::write(middle, b"not a directory").unwrap();
                }
            },
            || observe(&ws, Observation::Read, "a/middle/file"),
        );
        assert_eq!(
            fs::read_to_string(outside.path().join(RACE_NAME)).unwrap(),
            RACE_CANARY
        );
        assert_eq!(
            fs::read_dir(outside.path()).unwrap().count(),
            if rename_out { 2 } else { 1 }
        );
        if rename_out {
            assert_eq!(fs::read(moved.join("file")).unwrap(), b"inside\n");
        }
        assert_no_canary(&result);
        let error = result.unwrap_err();
        assert_no_host_path(&error, ws.root());
        if rename_out {
            assert!(
                matches!(error, LocalWorkspaceError::NotFound { .. }),
                "expected NotFound, got {error:?}"
            );
        } else {
            assert!(
                matches!(error, LocalWorkspaceError::Io(_)),
                "expected Io, got {error:?}"
            );
        }
    }

    #[test]
    fn r11_read_rejects_middle_directory_replaced_with_file() {
        moved_directory_case(false);
    }

    #[test]
    fn r12_read_reports_not_found_after_middle_directory_moves_out() {
        moved_directory_case(true);
    }

    fn make_fifo(path: &Path) {
        rustix::fs::mknodat(
            rustix::fs::CWD,
            path,
            rustix::fs::FileType::Fifo,
            Mode::RUSR | Mode::WUSR,
            0,
        )
        .unwrap();
    }

    fn fifo_case(recursive: bool, swap_fifo: bool) {
        use std::sync::mpsc;
        use std::time::Duration;

        let dir = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        fs::write(outside.path().join(RACE_NAME), RACE_CANARY).unwrap();
        let ws = ws_at(&dir);
        let fifo = dir.path().join("pipe");
        if swap_fifo {
            fs::write(&fifo, b"inside\n").unwrap();
        } else {
            make_fifo(&fifo);
        }
        let swap_path = fifo.clone();
        let (tx, rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let result = if swap_fifo {
                // grep 本来就会跳过静态 FIFO。在文件分类之后再替换，才能触发
                // 打开时真正的风险。
                with_swap(
                    "pipe",
                    move || {
                        fs::remove_file(&swap_path).unwrap();
                        make_fifo(&swap_path);
                    },
                    || observe(&ws, Observation::Grep, "."),
                )
            } else if recursive {
                observe(&ws, Observation::Grep, ".")
            } else {
                observe(&ws, Observation::Read, "pipe")
            };
            tx.send(result).unwrap();
        });
        let timely = rx.recv_timeout(Duration::from_secs(2));
        let timed_out = matches!(timely, Err(mpsc::RecvTimeoutError::Timeout));
        let result = if timed_out {
            // 以 `RDWR` 打开从不阻塞：短暂提供一个写端可唤醒旧的阻塞打开，
            // 关闭后 `read` 会读到 EOF。不使用 `process::exit`。
            let rescue = rustix::fs::open(
                &fifo,
                OFlags::RDWR | OFlags::NONBLOCK | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .unwrap();
            drop(rescue);
            rx.recv_timeout(Duration::from_secs(2))
        } else {
            timely
        };
        if worker.is_finished() || result.is_ok() {
            worker.join().expect("FIFO worker panicked");
        }
        assert_eq!(
            fs::read_to_string(outside.path().join(RACE_NAME)).unwrap(),
            RACE_CANARY
        );
        assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 1);
        let result = result.expect("FIFO worker did not finish after cleanup");
        assert_no_canary(&result);
        assert!(
            !timed_out,
            "FIFO observation hung for more than 2 seconds (worker unblocked for cleanup)"
        );
        if recursive {
            result.expect("grep must skip the FIFO");
        } else {
            let error = result.unwrap_err();
            assert_no_host_path(&error, dir.path());
            assert!(
                matches!(error, LocalWorkspaceError::Io(_)),
                "expected special-file refusal, got {error:?}"
            );
        }
    }

    #[test]
    fn r13_read_fifo_is_rejected_without_blocking() {
        fifo_case(false, false);
    }

    #[test]
    fn r14_grep_fifo_swap_is_skipped_without_blocking() {
        fifo_case(true, true);
    }

    #[test]
    fn r14_grep_static_fifo_is_skipped_without_blocking() {
        fifo_case(true, false);
    }

    #[test]
    fn r15_create_rejects_parent_symlink_swap() {
        let dir = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        fs::create_dir(dir.path().join("parent")).unwrap();
        fs::write(outside.path().join(RACE_NAME), RACE_CANARY).unwrap();
        let parent = dir.path().join("parent");
        let target = outside.path().to_path_buf();
        let ws = ws_at(&dir);
        let result = with_swap(
            "parent",
            move || {
                fs::remove_dir(&parent).unwrap();
                symlink(target, parent).unwrap();
            },
            || ws.create_excl_sync(&WorkspacePath::parse("parent/new").unwrap(), b"new", false),
        );
        assert_eq!(
            fs::read_to_string(outside.path().join(RACE_NAME)).unwrap(),
            RACE_CANARY
        );
        assert_eq!(
            fs::read_dir(outside.path()).unwrap().count(),
            1,
            "outside directory gained an entry"
        );
        assert!(!outside.path().join("new").exists());
        if let Ok(created) = &result {
            assert!(!format!("{created:?}").contains(RACE_CANARY.trim()));
        }
        let error = result.unwrap_err();
        assert_no_host_path(&error, ws.root());
        assert!(
            matches!(error, LocalWorkspaceError::SymlinkRejected { .. }),
            "got {error:?}"
        );
    }

    #[test]
    fn r16_all_static_symlink_refusals_hide_host_paths() {
        let dir = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        fs::write(outside.path().join(RACE_NAME), RACE_CANARY).unwrap();
        symlink(outside.path(), dir.path().join("link")).unwrap();
        let ws = ws_at(&dir);
        for (mode, path) in [
            (Observation::Read, "link/I233_EXTERNAL_NAME_7b98.txt"),
            (Observation::Bounded, "link/I233_EXTERNAL_NAME_7b98.txt"),
            (Observation::List, "link"),
            (Observation::Grep, "link"),
            (Observation::Find, "link"),
        ] {
            let result = observe(&ws, mode, path);
            assert_no_canary(&result);
            let error = result.unwrap_err();
            assert_no_host_path(&error, ws.root());
            assert!(matches!(error, LocalWorkspaceError::SymlinkRejected { .. }));
        }
        let error = ws
            .create_excl_sync(&WorkspacePath::parse("link/new").unwrap(), b"new", false)
            .unwrap_err();
        assert_no_host_path(&error, ws.root());
        assert!(matches!(error, LocalWorkspaceError::SymlinkRejected { .. }));
        assert_eq!(
            fs::read_to_string(outside.path().join(RACE_NAME)).unwrap(),
            RACE_CANARY
        );
        assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 1);
    }

    // ── ADR step 1: eight test classes ───────────────────────────────────

    // 1. Root escape: WorkspacePath::parse already rejects `..` and absolute
    //    paths, so the only escape vector is a symlink chain. This test pins
    //    the leaf-symlink rejection that closes that vector.
    #[tokio::test]
    async fn root_escape_via_symlink_rejected() {
        let dir = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        std::fs::write(outside.path().join("secret"), b"private").unwrap();
        let link = dir.path().join("escape");
        symlink(outside.path(), &link).unwrap();
        let ws = ws_at(&dir);
        let path = WorkspacePath::parse("escape/secret").unwrap();
        let err = ws.read_file(&path).await.unwrap_err();
        assert!(
            matches!(err, LocalWorkspaceError::SymlinkRejected { .. }),
            "got {err:?}"
        );
    }

    // 3. Symlink leaf rejection: a symlink at the target itself is rejected.
    #[tokio::test]
    async fn symlink_leaf_rejected() {
        let dir = TempDir::new().unwrap();
        let target = TempDir::new().unwrap();
        std::fs::write(target.path().join("file"), b"x").unwrap();
        let link = dir.path().join("leaf");
        symlink(target.path().join("file"), &link).unwrap();
        let ws = ws_at(&dir);
        let path = WorkspacePath::parse("leaf").unwrap();
        let err = ws.read_file(&path).await.unwrap_err();
        assert!(matches!(err, LocalWorkspaceError::SymlinkRejected { .. }));
    }

    // 5. Expected-hash conflict: the caller checks the precondition; this
    //    test pins that the installed content hash matches a known blake3
    //    literal computed independently, so the precondition comparison is
    //    sound.
    #[test]
    fn installed_content_hash_matches_independent_blake3() {
        let dir = TempDir::new().unwrap();
        let body = b"lattice content hash";
        let ws = ws_at(&dir);
        let path = WorkspacePath::parse("h.txt").unwrap();
        let created = ws
            .create_excl_sync_cancellable(&path, body.as_ref(), false, || false)
            .unwrap();
        let want = format!("blake3:{}", blake3::hash(body).to_hex());
        assert_eq!(created.content_hash, want);
    }

    // Conditional replacement: the complete content is observable after installation.
    #[test]
    fn conditional_replace_installs_complete_target() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("r.txt"), b"v1").unwrap();
        let ws = ws_at(&dir);
        let path = WorkspacePath::parse("r.txt").unwrap();
        let installed = ws
            .replace_if_revision_sync(&path, &content_hash(b"v1"), b"v2", || false)
            .unwrap()
            .installed;
        assert_eq!(installed.bytes_written, 2);
        let on_disk = std::fs::read(dir.path().join("r.txt")).unwrap();
        assert_eq!(on_disk, b"v2");
        let want = format!("blake3:{}", blake3::hash(b"v2").to_hex());
        assert_eq!(installed.content_hash, want);
    }

    // 8. 部分写入失败：向只读目录写入会失败，且磁盘上不留部分写入的目标
    //    （`O_EXCL` 临时文件创建本身就会失败，之后的任何失败也会通过父目录 fd
    //    `unlink` 临时名）。这是 ADR 第 1 步规定的真实故障近似；下方窄范围的
    //    故障注入接缝覆盖确定性边界。
    #[test]
    fn install_atomic_failure_leaves_no_partial_target() {
        let dir = TempDir::new().unwrap();
        let ro = dir.path().join("ro");
        std::fs::create_dir(&ro).unwrap();
        let mut perms = fs::metadata(&ro).unwrap().permissions();
        perms.set_readonly(true);
        fs::set_permissions(&ro, perms).unwrap();
        let ws = ws_at(&dir);
        let path = WorkspacePath::parse("ro/new.txt").unwrap();
        let err = {
            let _guard = ws.mutation_lock.lock().unwrap();
            ws.install_atomic_sync_locked(&path, b"x")
        };
        // Restore permissions so TempDir can clean up. Test-only restore
        // path: the TempDir parent is owner-only, so widening the child back
        // to writable here carries no production meaning.
        let mut perms = fs::metadata(&ro).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        let _ = fs::set_permissions(&ro, perms);
        assert!(err.is_err(), "expected install to fail in read-only dir");
        assert!(
            !dir.path().join("ro/new.txt").exists(),
            "no partial target may be left on disk"
        );
    }

    // 8b. Narrow fault injection (#38): a primed install failure leaves the
    //     existing target byte-identical, and the flag is one-shot — the
    //     next install succeeds. This proves the atomic-install failure
    //     boundary without a composable plugin seam.
    #[test]
    fn fail_next_install_leaves_target_unchanged_and_resets() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("f.txt"), b"v1").unwrap();
        let ws = ws_at(&dir);
        let path = WorkspacePath::parse("f.txt").unwrap();

        ws.fail_next_install();
        let err = ws
            .replace_if_revision_sync(&path, &content_hash(b"v1"), b"v2", || false)
            .unwrap_err();
        assert!(
            matches!(
                err,
                ConditionalReplaceError::Workspace(LocalWorkspaceError::Io(_))
            ),
            "got {err:?}"
        );
        let on_disk = std::fs::read(dir.path().join("f.txt")).unwrap();
        assert_eq!(on_disk, b"v1", "failed install must not touch the target");

        // One-shot: the flag was consumed by the failed install.
        ws.replace_if_revision_sync(&path, &content_hash(b"v1"), b"v2", || false)
            .unwrap();
        let on_disk = std::fs::read(dir.path().join("f.txt")).unwrap();
        assert_eq!(on_disk, b"v2");
    }

    #[test]
    fn conditional_replace_rechecks_cancellation_at_install_point() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("target.txt"), b"before\n").unwrap();
        let ws = ws_at(&dir);
        let path = WorkspacePath::parse("target.txt").unwrap();
        let probes = std::sync::atomic::AtomicUsize::new(0);

        let err = ws
            .replace_if_revision_sync(&path, &content_hash(b"before\n"), b"after\n", || {
                probes.fetch_add(1, Ordering::SeqCst) > 0
            })
            .expect_err("cancellation arriving after the first probe must abort install");
        assert!(matches!(err, ConditionalReplaceError::Cancelled));
        assert_eq!(
            std::fs::read(dir.path().join("target.txt")).unwrap(),
            b"before\n"
        );
        assert_eq!(probes.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn no_replace_create_cancelled_after_temp_sync_leaves_no_target_or_temp() {
        let dir = TempDir::new().unwrap();
        let ws = ws_at(&dir);
        let path = WorkspacePath::parse("cancelled.txt").unwrap();
        let probes = std::sync::atomic::AtomicUsize::new(0);

        let err = ws
            .create_excl_sync_cancellable(&path, b"content\n", false, || {
                probes.fetch_add(1, Ordering::SeqCst) > 1
            })
            .expect_err("cancellation after temp sync must abort before no-replace install");
        assert!(matches!(err, ConditionalCreateError::Cancelled));
        assert!(!dir.path().join("cancelled.txt").exists());
        assert_eq!(probes.load(Ordering::SeqCst), 3);
        assert!(
            fs::read_dir(dir.path()).unwrap().all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".lattice-tmp-")),
            "cancelled create must clean up its prepared temp file"
        );
    }

    // ── Supporting tests for observation operations ──────────────────────

    #[test]
    fn find_walk_observes_mid_traversal_cancellation() {
        let dir = TempDir::new().unwrap();
        for index in 0..32 {
            let child = dir.path().join(format!("d{index}"));
            std::fs::create_dir(&child).unwrap();
            std::fs::write(child.join("file.txt"), b"").unwrap();
        }
        let ws = ws_at(&dir);
        let request = FindRequest {
            pattern: "*".into(),
            path: WorkspacePath::project_root(),
        };
        let visibility = VisibilityEngine::load(dir.path(), Default::default());
        let probes = std::sync::atomic::AtomicUsize::new(0);
        let error = ws
            .find_files_blocking_cancellable(&request, 100, visibility, || {
                probes.fetch_add(1, Ordering::Relaxed) >= 3
            })
            .unwrap_err();
        assert!(matches!(error, LocalWorkspaceError::Cancelled));
    }

    // ── TOCTOU 安全的写入路径（父目录经 `open_beneath` 打开）─────────

    /// Names of `.lattice-tmp-*` residue directly beneath `dir`.
    fn tmp_residue(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(".lattice-tmp-"))
            .collect()
    }

    // 变更中的“事实→打开”竞态：父目录打开时会以 `RESOLVE_NO_SYMLINKS` 重新
    // 解析整条父路径，因此在取得事实之后才被换成符号链接的目录仍会被拒绝，
    // 外部目标不会被触及。
    #[test]
    fn open_parent_dir_rejects_post_resolve_symlink_swap() {
        let dir = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join("a")).unwrap();
        let ws = ws_at(&dir);
        // 此时解析 `a/x` 会成功：`a` 是真实目录。下面模拟解析后的替换。
        std::fs::remove_dir(dir.path().join("a")).unwrap();
        symlink(outside.path(), dir.path().join("a")).unwrap();
        let err = ws
            .open_parent_dir(&WorkspacePath::parse("a/x").unwrap(), false)
            .unwrap_err();
        assert!(
            matches!(err, LocalWorkspaceError::SymlinkRejected { .. }),
            "got {err:?}"
        );
        assert_eq!(
            std::fs::read_dir(outside.path()).unwrap().count(),
            0,
            "nothing may reach the outside directory"
        );
    }

    /// #98：`host_mutate` 把该错误的 `Display` 转发为 guest 可见的
    /// `PathRejected` 原因，因此报告的路径段必须是根相对的。`openat2` 不指出出错
    /// 的路径段，所以拒绝信息携带完整请求。同时覆盖事实解析与变更时的父目录
    /// 打开。
    #[test]
    fn symlink_rejection_reports_a_relative_component_not_the_host_path() {
        let dir = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join("nested")).unwrap();
        symlink(outside.path(), dir.path().join("nested/link")).unwrap();
        let ws = ws_at(&dir);
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let root_str = root.to_str().expect("utf-8 temp path");

        let resolve_err = ws
            .resolve_facts_blocking(
                &WorkspacePath::parse("nested/link/x").unwrap(),
                PathIntent::Read,
            )
            .unwrap_err();
        let LocalWorkspaceError::SymlinkRejected { component } = &resolve_err else {
            panic!("expected SymlinkRejected, got {resolve_err:?}");
        };
        assert_eq!(component, Path::new("nested/link/x"));

        std::fs::create_dir(dir.path().join("swap")).unwrap();
        let swapped = ws_at(&dir);
        std::fs::remove_dir(dir.path().join("swap")).unwrap();
        symlink(outside.path(), dir.path().join("swap")).unwrap();
        let walk_err = swapped
            .open_parent_dir(&WorkspacePath::parse("swap/new").unwrap(), false)
            .unwrap_err();
        let LocalWorkspaceError::SymlinkRejected { component } = &walk_err else {
            panic!("expected SymlinkRejected, got {walk_err:?}");
        };
        assert_eq!(component, Path::new("swap/new"));

        for err in [&resolve_err, &walk_err] {
            let message = err.to_string();
            assert!(
                !message.contains(root_str),
                "refusal leaked the workspace root: {message}"
            );
            assert!(
                !message.contains("/tmp"),
                "refusal leaked a host path: {message}"
            );
        }
    }

    // `create_missing` 同样不得穿过符号链接路径段执行 mkdir：打开已存在的符号
    // 链接前缀会先失败，任何 `mkdirat` 都没有机会执行。
    #[test]
    fn open_parent_dir_create_missing_rejects_symlink_component() {
        let dir = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let ws = ws_at(&dir);
        symlink(outside.path(), dir.path().join("a")).unwrap();
        let err = ws
            .open_parent_dir(&WorkspacePath::parse("a/b/x").unwrap(), true)
            .unwrap_err();
        assert!(
            matches!(err, LocalWorkspaceError::SymlinkRejected { .. }),
            "got {err:?}"
        );
        assert_eq!(
            std::fs::read_dir(outside.path()).unwrap().count(),
            0,
            "no directory may be created through the symlink"
        );
    }

    // 端到端防护：每个写入入口在动作前解析路径时都会拒绝经由符号链接的父目录；
    // 这里确认两种写操作被拒绝后，外部目标仍逐字节不变。
    #[test]
    fn write_paths_reject_symlink_parent_and_leave_outside_untouched() {
        let dir = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        std::fs::write(outside.path().join("x"), b"keep").unwrap();
        symlink(outside.path(), dir.path().join("link")).unwrap();
        let ws = ws_at(&dir);
        let path = WorkspacePath::parse("link/x").unwrap();

        let err = ws.create_excl_sync_cancellable(&path, b"evil".as_ref(), true, || false);
        assert!(
            matches!(
                err,
                Err(ConditionalCreateError::Workspace(
                    LocalWorkspaceError::SymlinkRejected { .. }
                ))
            ),
            "got {err:?}"
        );
        let err = ws.replace_if_revision_sync(&path, &content_hash(b"keep"), b"evil", || false);
        assert!(
            matches!(
                err,
                Err(ConditionalReplaceError::Workspace(
                    LocalWorkspaceError::SymlinkRejected { .. }
                ))
            ),
            "got {err:?}"
        );
        assert_eq!(
            std::fs::read(outside.path().join("x")).unwrap(),
            b"keep",
            "outside file must survive both rejected mutations"
        );
        assert_eq!(
            std::fs::read_dir(outside.path()).unwrap().count(),
            1,
            "outside directory must gain no entries"
        );
    }

    // Temp hygiene: neither a forced install failure nor a successful
    // install may leave `.lattice-tmp-*` residue in the parent directory.
    #[test]
    fn conditional_replace_leaves_no_temp_residue() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("t.txt"), b"v1").unwrap();
        let ws = ws_at(&dir);
        let path = WorkspacePath::parse("t.txt").unwrap();

        ws.fail_next_install();
        ws.replace_if_revision_sync(&path, &content_hash(b"v1"), b"v2", || false)
            .unwrap_err();
        assert_eq!(tmp_residue(dir.path()), Vec::<String>::new());

        ws.replace_if_revision_sync(&path, &content_hash(b"v1"), b"v2", || false)
            .unwrap();
        assert_eq!(tmp_residue(dir.path()), Vec::<String>::new());
        assert_eq!(std::fs::read(dir.path().join("t.txt")).unwrap(), b"v2");
    }

    // ── Grep byte-budget bounds ──────────────────────────────────────────

    fn needle_re() -> Regex {
        Regex::new("needle").unwrap()
    }

    // (a) A file above the per-file cap is skipped outright while a small
    //     file in the same walk still matches.
    #[test]
    fn grep_walk_skips_over_cap_file_but_scans_small_file() {
        let dir = TempDir::new().unwrap();
        // 100 bytes > per-file cap of 64: skipped before opening.
        let mut big = String::from("needle\n");
        big.push_str(&"x".repeat(93));
        std::fs::write(dir.path().join("big.txt"), &big).unwrap();
        std::fs::write(dir.path().join("small.txt"), "needle\n").unwrap();
        let mut budget = GrepBudget::new(64, 1024);
        let mut visibility = VisibilityEngine::load(dir.path(), Default::default());
        let mut out = Vec::new();
        grep_walk(
            &ws_at(&dir).open_root().unwrap(),
            ws_at(&dir).open_root().unwrap(),
            OsStr::new("."),
            &needle_re(),
            None,
            100,
            &mut budget,
            &mut visibility,
            &mut out,
            &|| false,
        )
        .unwrap();
        assert_eq!(out.len(), 1, "got {out:?}");
        assert_eq!(out[0].path.as_str(), "small.txt");
    }

    // (b) Total-budget exhaustion stops scanning subsequent files: an
    //     exhausted budget makes grep_walk a no-op, and a shared budget
    //     drained by one grep_file call leaves the next file only a
    //     truncated allowance that cannot produce a match.
    #[test]
    fn grep_budget_total_exhaustion_stops_scanning() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("m.txt"), "needle\n").unwrap();
        let mut budget = GrepBudget::new(64, 128);
        budget.charge(128);
        assert!(budget.exhausted());
        let mut visibility = VisibilityEngine::load(dir.path(), Default::default());
        let mut out = Vec::new();
        grep_walk(
            &ws_at(&dir).open_root().unwrap(),
            ws_at(&dir).open_root().unwrap(),
            OsStr::new("."),
            &needle_re(),
            None,
            100,
            &mut budget,
            &mut visibility,
            &mut out,
            &|| false,
        )
        .unwrap();
        assert!(out.is_empty(), "exhausted budget must scan nothing");

        // Deterministic two-file sequence: file one drains most of the
        // total, so file two's allowance is clamped below its first line.
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.txt");
        std::fs::write(&a, format!("needle {}\n", "a".repeat(53))).unwrap(); // 61 bytes
        std::fs::write(&b, "needle b\n").unwrap();
        let mut budget = GrepBudget::new(64, 64);
        let mut out = Vec::new();
        grep_file(
            "a.txt",
            fs::File::open(&a).unwrap(),
            &needle_re(),
            100,
            &mut budget,
            &mut out,
            &|| false,
        )
        .unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(budget.file_allowance(), 3, "61 of 64 total consumed");
        grep_file(
            "b.txt",
            fs::File::open(&b).unwrap(),
            &needle_re(),
            100,
            &mut budget,
            &mut out,
            &|| false,
        )
        .unwrap();
        assert_eq!(out.len(), 1, "truncated allowance must not match: {out:?}");
    }

    // (c) Matches beyond the per-file allowance are not returned while
    //     earlier ones in the same file are.
    #[test]
    fn grep_file_drops_matches_beyond_per_file_allowance() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("c.txt");
        // Line 1 (11 bytes) fits the 32-byte allowance; the filler line
        // crosses the cap, so the later matching line is never reached.
        let content = format!("needle one\n{}\nneedle two\n", "x".repeat(40));
        std::fs::write(&path, content).unwrap();
        let mut budget = GrepBudget::new(32, 1024);
        let mut out = Vec::new();
        grep_file(
            "c.txt",
            fs::File::open(&path).unwrap(),
            &needle_re(),
            100,
            &mut budget,
            &mut out,
            &|| false,
        )
        .unwrap();
        assert_eq!(out.len(), 1, "got {out:?}");
        assert_eq!(out[0].line_number, 1);
        assert_eq!(out[0].line, "needle one");
    }

    // Long-line guard through the public grep() path: a >64KiB line that
    // contains the pattern is consumed but never matched; the normal line
    // after it still matches with the correct line number.
    #[tokio::test]
    async fn grep_skips_over_long_lines_but_matches_normal_ones() {
        let dir = TempDir::new().unwrap();
        let long = format!("needle {}\nneedle short\n", "y".repeat(70 * 1024));
        std::fs::write(dir.path().join("l.txt"), long).unwrap();
        let ws = ws_at(&dir);
        let req = GrepRequest {
            pattern: "needle".into(),
            path: WorkspacePath::project_root(),
            glob: None,
        };
        let visibility = VisibilityEngine::load(dir.path(), Default::default());
        let (matches, _evidence) = ws.grep(&req, 100, visibility).await.unwrap();
        assert_eq!(matches.len(), 1, "got {matches:?}");
        assert_eq!(matches[0].line_number, 2);
        assert_eq!(matches[0].line, "needle short");
    }

    // Invalid UTF-8 mid-file: the match found before the bad bytes
    // survives and no error is surfaced (old whole-file read dropped the
    // entire file).
    #[tokio::test]
    async fn grep_keeps_matches_before_invalid_utf8() {
        let dir = TempDir::new().unwrap();
        let mut bytes = b"needle ok\n".to_vec();
        bytes.extend_from_slice(b"\xff\xfe broken\n");
        bytes.extend_from_slice(b"needle later\n");
        std::fs::write(dir.path().join("u.txt"), &bytes).unwrap();
        let ws = ws_at(&dir);
        let req = GrepRequest {
            pattern: "needle".into(),
            path: WorkspacePath::project_root(),
            glob: None,
        };
        let visibility = VisibilityEngine::load(dir.path(), Default::default());
        let (matches, _evidence) = ws.grep(&req, 100, visibility).await.unwrap();
        assert_eq!(matches.len(), 1, "got {matches:?}");
        assert_eq!(matches[0].line_number, 1);
        assert_eq!(matches[0].line, "needle ok");
    }

    // ── list_dir symlink reporting ───────────────────────────────────────

    // ── glob_match semantics ─────────────────────────────────────────────
}

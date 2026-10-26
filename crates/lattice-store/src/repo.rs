// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Typed create/open, object/commit mechanics, ref CAS/transactions, the
//! per-repository write lock, and atomic first-create publication (ADR 0005
//! §4).
//!
//! StoreRepo 提供工作区历史所需的项目 Git 机制；领域负责文件名、schema、
//! 引用与记录不变量。内核不暴露原始 Repository 或通用业务 put/get。
//!
//! ## First-create protocol
//!
//! 1. take the target-specific create lock in the parent directory;
//! 2. re-check the target: if it now exists, validate the winner, observe the
//!    same parent durability barrier, and return (loser path);
//! 3. run capability probes against the actual parent location — a missing
//!    primitive fails closed;
//! 4. initialize the bare repository in a same-parent sibling temporary
//!    directory, precreate the complete `objects/00`..=`objects/ff` fanout,
//!    write the `refs/lattice/meta` identity commit plus the
//!    domain's initial refs, and fsync the whole tree;
//! 5. publish with `renameat2(RENAME_NOREPLACE)`, fsync the parent, and
//!    reopen and validate the published identity (winner path).
//!
//! The create lock and the repository write lock never overlap: the create
//! lock is released before [`StoreRepo::open`] returns the repository.

use std::collections::BTreeMap;
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};

use gix::objs::tree::EntryKind;
use gix::refs::transaction::PreviousValue;

use crate::durability::{
    fsync_dir, fsync_publication, fsync_tree, precreate_loose_object_fanout,
    validate_loose_object_fanout,
};
use crate::lock::{Blocking, FlockGuard, acquire_flock_strict};
use crate::meta::{
    self, ExpectedMeta, META_REF, META_TREE_FILE, REPOSITORY_FORMAT_VERSION, StoreMeta, StoreScope,
};
use crate::{ProjectStoreKey, StateLayout, StoreError};

/// Result type for kernel operations.
pub type Result<T> = std::result::Result<T, StoreError>;

/// File name of the per-repository write lock inside the git directory.
pub const REPO_LOCK_FILE: &str = "lattice-repo.lock";

/// Ref namespace every kernel-managed ref lives under.
pub const LATTICE_REF_PREFIX: &str = "refs/lattice/";

const KERNEL_AUTHOR_NAME: &str = "LATTICE";
const KERNEL_AUTHOR_EMAIL: &str = "lattice@localhost";

/// One validated private repository.
///
/// The `gix::Repository` is cached behind a mutex (`Repository` is `Send`
/// but not `Sync`). Readers pin ref OIDs and read immutable objects without
/// the write lock; writers hold [`Self::write_lock`] across old-tip read,
/// object creation, CAS/transaction, and durability barriers.
pub struct StoreRepo {
    git_dir: PathBuf,
    repo: Mutex<gix::Repository>,
    meta: StoreMeta,
    /// Objects written through this handle since the last
    /// [`Self::fsync_pending`] barrier, so writers can fsync exactly what
    /// they created without threading every intermediate oid through their
    /// tree-building code.
    pending: Mutex<std::collections::BTreeSet<gix::ObjectId>>,
}

impl std::fmt::Debug for StoreRepo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoreRepo")
            .field("git_dir", &self.git_dir)
            .field("meta", &self.meta)
            .finish_non_exhaustive()
    }
}

/// A declarative tree edit: start from `base` (or the empty tree) and apply
/// upserts/removals, then write.
#[derive(Debug, Default)]
pub struct TreeSpec {
    base: Option<gix::ObjectId>,
    upserts: Vec<(String, EntryKind, gix::ObjectId)>,
    removals: Vec<String>,
}

impl TreeSpec {
    /// A tree edit starting from the empty tree.
    pub fn new() -> Self {
        Self::default()
    }

    /// A tree edit starting from an existing tree.
    pub fn from(base: gix::ObjectId) -> Self {
        Self {
            base: Some(base),
            ..Self::default()
        }
    }

    /// Insert or replace one entry.
    pub fn upsert(&mut self, name: &str, kind: EntryKind, oid: gix::ObjectId) -> &mut Self {
        self.upserts.push((name.to_string(), kind, oid));
        self
    }

    /// Remove one entry.
    pub fn remove(&mut self, name: &str) -> &mut Self {
        self.removals.push(name.to_string());
        self
    }
}

/// What a CAS expects the current ref value to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefExpectation {
    /// The ref must not exist.
    MustNotExist,
    /// The ref must exist with any value.
    MustExist,
    /// The ref must exist and point at this object.
    MustMatch(gix::ObjectId),
    /// No constraint.
    Any,
}

impl From<RefExpectation> for PreviousValue {
    fn from(expectation: RefExpectation) -> Self {
        match expectation {
            RefExpectation::MustNotExist => PreviousValue::MustNotExist,
            RefExpectation::MustExist => PreviousValue::MustExist,
            RefExpectation::MustMatch(oid) => {
                PreviousValue::MustExistAndMatch(gix::refs::Target::Object(oid))
            }
            RefExpectation::Any => PreviousValue::Any,
        }
    }
}

impl StoreRepo {
    // ─── Open / create ─────────────────────────────────────────────────

    /// Strictly open and validate an existing repository.
    ///
    /// An absent target is [`StoreError::NotInitialized`]; wrong kind, scope,
    /// version, or a missing/unreadable meta object is
    /// [`StoreError::Corruption`]. Open never initializes, repairs,
    /// quarantines, or substitutes anything.
    pub fn open(path: &Path, expected: &ExpectedMeta) -> Result<Self> {
        if !path.exists() {
            return Err(StoreError::NotInitialized {
                path: path.to_path_buf(),
            });
        }
        if !path.is_dir() {
            return Err(StoreError::Corruption {
                detail: format!("'{}' exists but is not a directory", path.display()),
            });
        }
        let repo = gix::open_opts(path, gix::open::Options::isolated()).map_err(|e| {
            StoreError::Corruption {
                detail: format!(
                    "'{}' is not an openable git repository: {e}",
                    path.display()
                ),
            }
        })?;
        if repo.workdir().is_some() {
            return Err(StoreError::Corruption {
                detail: format!("'{}' is not a bare repository", path.display()),
            });
        }
        validate_loose_object_fanout(path)?;
        let meta = read_meta(&repo, path)?;
        meta::check_expected(&meta, expected)?;
        Ok(Self {
            git_dir: path.to_path_buf(),
            repo: Mutex::new(repo),
            meta,
            pending: Mutex::new(std::collections::BTreeSet::new()),
        })
    }

    /// Strictly open the project store `name` bound to `key`. The recorded
    /// scope must match the directory key structurally.
    pub fn open_project(
        layout: &StateLayout,
        key: &ProjectStoreKey,
        name: &str,
        store_kind: &str,
        store_schema_version: u32,
    ) -> Result<Self> {
        Self::open(
            &layout.project_store(key, name),
            &ExpectedMeta {
                store_kind: store_kind.to_string(),
                store_schema_version,
                scope: StoreScope::Project { key: *key },
            },
        )
    }

    /// Create the repository at `path` with `meta`, or validate the existing
    /// one. `init` runs against the not-yet-published repository to write the
    /// domain's initial objects/refs; it runs at most once per process call
    /// and never on an existing repository.
    ///
    /// 产品仅在新 Session 首次提交时创建工作区历史；隔离测试使用自有目录。
    pub fn create(
        path: &Path,
        meta: StoreMeta,
        init: impl FnOnce(&StoreRepo) -> Result<()>,
    ) -> Result<Self> {
        let expected = ExpectedMeta {
            store_kind: meta.store_kind.clone(),
            store_schema_version: meta.store_schema_version,
            scope: meta.scope,
        };
        let parent = path.parent().ok_or_else(|| StoreError::InvalidInput {
            detail: format!("store path '{}' has no parent", path.display()),
        })?;
        let name = path.file_name().ok_or_else(|| StoreError::InvalidInput {
            detail: format!("store path '{}' has no file name", path.display()),
        })?;

        ensure_dirs_durable(parent)?;

        // 1. Target-specific create lock in the parent directory. This lock
        //    never overlaps the repository write lock: it is dropped before
        //    the validated repository is returned.
        let create_lock_path = parent.join(format!(".{}.create.lock", name.to_string_lossy()));
        let _create_lock = acquire_flock_strict(&create_lock_path, Blocking::Block)?;

        // 2. Loser path: a concurrent creator (or an earlier success) already
        //    published. The identity is re-validated against the expectation
        //    derived from the addressing, so a directory whose recorded scope
        //    does not match its address fails closed here too.
        if path.exists() {
            let repo = Self::open(path, &expected)?;
            fsync_dir(parent)?;
            return Ok(repo);
        }

        // 3. Capability probes against the actual publication parent. A
        //    missing primitive fails closed.
        crate::probe::probe_location(parent)?;

        // 4. Sibling temporary directory initialization. A stale temp from a
        //    dead creator is safe to remove: every creator of this target
        //    serializes on the create lock held above.
        for entry in std::fs::read_dir(parent).map_err(|source| StoreError::Io {
            operation: "scanning for stale create temporaries",
            path: parent.to_path_buf(),
            source,
        })? {
            let entry = entry.map_err(|source| StoreError::Io {
                operation: "reading directory entry",
                path: parent.to_path_buf(),
                source,
            })?;
            let entry_name = entry.file_name();
            let entry_name = entry_name.to_string_lossy();
            if entry_name.starts_with(&format!(".{}.tmp-", name.to_string_lossy())) {
                std::fs::remove_dir_all(entry.path()).map_err(|source| StoreError::Io {
                    operation: "removing stale create temporary",
                    path: entry.path(),
                    source,
                })?;
            }
        }

        static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);
        let temp = parent.join(format!(
            ".{}.tmp-{}-{}",
            name.to_string_lossy(),
            std::process::id(),
            TEMP_SEQ.fetch_add(1, Ordering::Relaxed)
        ));

        let create_result: Result<()> = (|| {
            let temp_repo = init_bare(&temp)?;
            precreate_loose_object_fanout(&temp)?;
            let meta_commit = write_meta_commit(&temp_repo, &meta)?;
            let this = StoreRepo {
                git_dir: temp.clone(),
                repo: Mutex::new(temp_repo),
                meta: meta.clone(),
                pending: Mutex::new(std::collections::BTreeSet::new()),
            };
            this.cas_ref(
                META_REF,
                meta_commit,
                RefExpectation::MustNotExist,
                "lattice store identity",
            )?;
            init(&this)?;
            fsync_tree(&temp)?;
            crate::fault::before_rename(path)?;
            rename_noreplace(&temp, path)?;
            crate::fault::parent_fsync(parent)?;
            crate::fault::on_repo_first_create(path);
            Ok(())
        })();

        if let Err(err) = create_result {
            if !path.exists() {
                // The rename never happened (or never could): only our temp
                // needs cleanup.
                let _ = std::fs::remove_dir_all(&temp);
            }
            return Err(err);
        }

        // 5. Winner path: reopen and validate the published identity.
        Self::open(path, &expected)
    }

    /// Create the project store `name` under the key's project directory. The
    /// recorded scope is forced to the directory key, so a project store can
    /// never record a scope that disagrees with its location.
    pub fn create_project(
        layout: &StateLayout,
        key: &ProjectStoreKey,
        name: &str,
        store_kind: &str,
        store_schema_version: u32,
        init: impl FnOnce(&StoreRepo) -> Result<()>,
    ) -> Result<Self> {
        Self::create(
            &layout.project_store(key, name),
            StoreMeta {
                store_kind: store_kind.to_string(),
                store_schema_version,
                scope: StoreScope::Project { key: *key },
            },
            init,
        )
    }

    // ─── Identity ──────────────────────────────────────────────────────

    /// The validated identity record.
    pub fn meta(&self) -> &StoreMeta {
        &self.meta
    }

    /// The git directory (repository path).
    pub fn git_dir(&self) -> &Path {
        &self.git_dir
    }

    // ─── Locking ───────────────────────────────────────────────────────

    /// The standard per-repository write lock: one exclusive strict `flock`
    /// inside the git directory, held across old-tip read, object creation,
    /// CAS/transaction, and durability barriers. Ordinary single-ref reads do
    /// not take it; domain strict-open validation may use it for a coherent
    /// cross-ref view.
    pub fn write_lock(&self) -> Result<FlockGuard> {
        acquire_flock_strict(&self.git_dir.join(REPO_LOCK_FILE), Blocking::Block)
    }

    // ─── Reads (lock-free: pin a ref OID, read immutable objects) ──────

    /// Resolve a ref to its object id, if it exists.
    pub fn ref_target(&self, ref_name: &str) -> Result<Option<gix::ObjectId>> {
        let repo = self.repo();
        match repo.try_find_reference(ref_name) {
            Ok(Some(r)) => Ok(r.target().try_id().map(|id| id.to_owned())),
            Ok(None) => Ok(None),
            Err(e) => Err(StoreError::Git(format!("find reference {ref_name}: {e}"))),
        }
    }

    /// Read the tree of a commit.
    pub fn commit_tree(&self, commit_oid: gix::ObjectId) -> Result<gix::ObjectId> {
        let repo = self.repo();
        let commit = repo
            .find_commit(commit_oid)
            .map_err(|e| StoreError::Git(format!("find commit {commit_oid}: {e}")))?;
        Ok(commit
            .tree()
            .map_err(|e| StoreError::Git(format!("read commit tree: {e}")))?
            .id()
            .detach())
    }

    /// Parent ids of a commit.
    pub fn commit_parents(&self, commit_oid: gix::ObjectId) -> Result<Vec<gix::ObjectId>> {
        let repo = self.repo();
        let commit = repo
            .find_commit(commit_oid)
            .map_err(|e| StoreError::Git(format!("find commit {commit_oid}: {e}")))?;
        Ok(commit.parent_ids().map(|id| id.detach()).collect())
    }

    /// Commit message, verbatim.
    pub fn commit_message(&self, commit_oid: gix::ObjectId) -> Result<String> {
        let repo = self.repo();
        let commit = repo
            .find_commit(commit_oid)
            .map_err(|e| StoreError::Git(format!("find commit {commit_oid}: {e}")))?;
        Ok(commit.message_raw_sloppy().to_string())
    }

    /// Read the blob at `path` inside a tree. `None` when the path is absent
    /// or is not a blob.
    pub fn read_tree_path(&self, tree_oid: gix::ObjectId, path: &str) -> Result<Option<Vec<u8>>> {
        let repo = self.repo();
        let tree = repo
            .find_tree(tree_oid)
            .map_err(|e| StoreError::Git(format!("find tree {tree_oid}: {e}")))?;
        let Some(entry) = tree
            .lookup_entry_by_path(path)
            .map_err(|e| StoreError::Git(format!("lookup '{path}' in tree: {e}")))?
        else {
            return Ok(None);
        };
        let object = entry
            .object()
            .map_err(|e| StoreError::Git(format!("load '{path}' from tree: {e}")))?;
        match object.try_into_blob() {
            Ok(blob) => Ok(Some(blob.data.clone())),
            // A tree (or anything else) at that path is not blob content.
            Err(_) => Ok(None),
        }
    }

    /// Every blob in a tree, keyed by its full path.
    pub fn tree_files(&self, tree_oid: gix::ObjectId) -> Result<BTreeMap<String, Vec<u8>>> {
        let repo = self.repo();
        let tree = repo
            .find_tree(tree_oid)
            .map_err(|e| StoreError::Git(format!("find tree {tree_oid}: {e}")))?;
        let mut out = BTreeMap::new();
        collect_tree_files(&tree, "", &mut out)?;
        Ok(out)
    }

    /// The empty tree id for this repository's object hash.
    pub fn empty_tree_oid(&self) -> gix::ObjectId {
        gix::ObjectId::empty_tree(self.repo().object_hash())
    }

    // ─── Writes (caller holds the write lock) ──────────────────────────

    /// Write a blob.
    pub fn write_blob(&self, bytes: &[u8]) -> Result<gix::ObjectId> {
        let oid = self
            .repo()
            .write_blob(bytes)
            .map(|id| id.detach())
            .map_err(|e| StoreError::Git(format!("write blob: {e}")))?;
        self.note_pending(oid);
        Ok(oid)
    }

    /// Apply a [`TreeSpec`] and write the resulting tree.
    pub fn write_tree(&self, spec: &TreeSpec) -> Result<gix::ObjectId> {
        let repo = self.repo();
        let base = match spec.base {
            Some(base) => base,
            None => gix::ObjectId::empty_tree(repo.object_hash()),
        };
        let mut editor = repo
            .edit_tree(base)
            .map_err(|e| StoreError::Git(format!("create tree editor: {e}")))?;
        for (name, kind, oid) in &spec.upserts {
            editor
                .upsert(name, *kind, *oid)
                .map_err(|e| StoreError::Git(format!("upsert {name}: {e}")))?;
        }
        for name in &spec.removals {
            editor
                .remove(name)
                .map_err(|e| StoreError::Git(format!("remove {name}: {e}")))?;
        }
        let oid = editor
            .write()
            .map(|id| id.detach())
            .map_err(|e| StoreError::Git(format!("write tree: {e}")))?;
        drop(repo);
        self.note_pending(oid);
        Ok(oid)
    }

    /// Write a commit at an explicit unix timestamp (deterministic callers
    /// supply their own; the kernel does not invent clocks).
    pub fn write_commit_at(
        &self,
        tree: gix::ObjectId,
        parents: &[gix::ObjectId],
        message: &str,
        timestamp: i64,
    ) -> Result<gix::ObjectId> {
        let sig = gix::actor::Signature {
            name: KERNEL_AUTHOR_NAME.into(),
            email: KERNEL_AUTHOR_EMAIL.into(),
            time: gix::date::Time::new(timestamp, 0),
        };
        let mut time_buf = gix::date::parse::TimeBuf::default();
        let sig_ref = sig.to_ref(&mut time_buf);
        let oid = {
            let repo = self.repo();
            repo.new_commit_as(sig_ref, sig_ref, message, tree, parents.iter().copied())
                .map_err(|e| StoreError::Git(format!("create commit: {e}")))?
                .id
        };
        self.note_pending(oid);
        Ok(oid)
    }

    /// CAS one ref to `new` under `expect`.
    ///
    /// gix semantics: `MustNotExist` with a byte-identical existing target is
    /// an idempotent success, not a conflict — the CAS loser that matters is
    /// the one with a *different* value, and that one is a typed
    /// [`StoreError::CasConflict`].
    pub fn cas_ref(
        &self,
        ref_name: &str,
        new: gix::ObjectId,
        expect: RefExpectation,
        log_message: &str,
    ) -> Result<()> {
        validate_ref_name(ref_name)?;
        // Objects and their fanout directories must be durable before a ref
        // can publish an oid that names them. The post-publication barrier is
        // deliberately separate: CAS remains the publication authority.
        // Callers must not add a second `fsync_pending` after success; this
        // method already fsyncs `ref_name`. Exact retries that skip CAS still
        // call `fsync_pending` themselves to re-attempt a failed ref barrier.
        self.fsync_pending(&[])?;
        self.repo()
            .reference(ref_name, new, expect.into(), log_message)
            .map(|_| ())
            .map_err(|e| classify_ref_error(ref_name, e))?;
        self.fsync_pending(&[ref_name])
    }

    // ─── Durability ────────────────────────────────────────────────────

    /// Publication barrier after a write sequence. Object publication fsyncs
    /// each new loose object and its exact fanout directory. Ref publication
    /// fsyncs the ref file and ancestor directories whose entries changed.
    /// Neither fsyncs top-level `objects/` in steady state.
    pub fn fsync_publication(&self, oids: &[gix::ObjectId], refs: &[&str]) -> Result<()> {
        fsync_publication(&self.git_dir, oids, refs)
    }

    /// Publication barrier over every object written through this handle
    /// since the last barrier, plus the given refs. Pending objects remain
    /// pending when the barrier fails so a caller can retry without publishing
    /// an oid whose first durability attempt failed.
    pub fn fsync_pending(&self, refs: &[&str]) -> Result<()> {
        let oids: Vec<gix::ObjectId> = {
            let pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            pending.iter().copied().collect()
        };
        fsync_publication(&self.git_dir, &oids, refs)?;
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        for oid in oids {
            pending.remove(&oid);
        }
        Ok(())
    }

    // ─── Internals ─────────────────────────────────────────────────────

    fn repo(&self) -> MutexGuard<'_, gix::Repository> {
        self.repo.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn note_pending(&self, oid: gix::ObjectId) {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(oid);
    }
}

/// Recursively collect every blob in `tree` into `out`, keyed by full path.
fn collect_tree_files(
    tree: &gix::Tree<'_>,
    prefix: &str,
    out: &mut BTreeMap<String, Vec<u8>>,
) -> Result<()> {
    for entry in tree.iter() {
        let entry = entry.map_err(|e| StoreError::Git(format!("iterate tree: {e}")))?;
        let name = entry.filename().to_string();
        let full = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        let object = entry
            .object()
            .map_err(|e| StoreError::Git(format!("load tree object: {e}")))?;
        match object.kind {
            gix::object::Kind::Tree => {
                let subtree = object
                    .try_into_tree()
                    .map_err(|e| StoreError::Git(format!("peel subtree: {e}")))?;
                collect_tree_files(&subtree, &full, out)?;
            }
            gix::object::Kind::Blob => {
                let blob = object
                    .try_into_blob()
                    .map_err(|e| StoreError::Git(format!("peel blob: {e}")))?;
                out.insert(full, blob.data.clone());
            }
            _ => {}
        }
    }
    Ok(())
}

/// Validate a ref name the kernel will write: syntactically legal and inside
/// the `refs/lattice/` namespace.
fn validate_ref_name(ref_name: &str) -> Result<()> {
    if !ref_name.starts_with(LATTICE_REF_PREFIX) {
        return Err(StoreError::InvalidInput {
            detail: format!("ref '{ref_name}' is outside the {LATTICE_REF_PREFIX} namespace"),
        });
    }
    let _: gix::refs::FullName = ref_name.try_into().map_err(|e| StoreError::InvalidInput {
        detail: format!("illegal ref name '{ref_name}': {e}"),
    })?;
    Ok(())
}

/// Map a gix ref-edit error to a typed CAS conflict by matching the
/// structured transaction-prepare variants (never the rendered message).
fn classify_ref_error(ref_name: &str, error: gix::reference::edit::Error) -> StoreError {
    use gix::refs::file::transaction::prepare::Error as Prepare;
    match &error {
        gix::reference::edit::Error::FileTransactionPrepare(
            Prepare::MustNotExist { .. }
            | Prepare::MustExist { .. }
            | Prepare::ReferenceOutOfDate { .. }
            | Prepare::DeleteReferenceMustExist { .. },
        ) => StoreError::CasConflict {
            ref_name: ref_name.to_string(),
            detail: error.to_string(),
        },
        // A ref-lock acquisition failure is contention on the ref itself,
        // not a lost precondition: the caller's expectation may still hold on
        // the other side of the lock, so this must not be retried as a CAS
        // race. It is a Git-mechanics failure, surfaced as such.
        gix::reference::edit::Error::FileTransactionPrepare(Prepare::LockAcquire { .. }) => {
            StoreError::Git(format!(
                "lock acquire while updating ref {ref_name}: {error}"
            ))
        }
        _ => StoreError::Git(format!("update ref {ref_name}: {error}")),
    }
}

/// Write the identity commit for a fresh repository (meta.json in a tree).
/// Repository initialization is intentionally side-effect free with respect to
/// process-global CWD. If the underlying Git library cannot resolve its
/// required environment, creation fails as a typed Git error; it never changes
/// another task's working directory or retries based on rendered debug text.
fn init_bare(temp: &Path) -> Result<gix::Repository> {
    gix::ThreadSafeRepository::init_opts(
        temp,
        gix::create::Kind::Bare,
        gix::create::Options::default(),
        gix::open::Options::isolated(),
    )
    .map(Into::into)
    .map_err(|e| StoreError::Git(format!("init bare repository in '{}': {e}", temp.display())))
}

fn write_meta_commit(repo: &gix::Repository, meta: &StoreMeta) -> Result<gix::ObjectId> {
    let bytes = meta::serialize_meta(meta)?;
    let blob = repo
        .write_blob(&bytes)
        .map(|id| id.detach())
        .map_err(|e| StoreError::Git(format!("write meta blob: {e}")))?;
    let mut editor = repo
        .edit_tree(gix::ObjectId::empty_tree(repo.object_hash()))
        .map_err(|e| StoreError::Git(format!("create meta tree editor: {e}")))?;
    editor
        .upsert(META_TREE_FILE, EntryKind::Blob, blob)
        .map_err(|e| StoreError::Git(format!("insert meta.json: {e}")))?;
    let tree = editor
        .write()
        .map(|id| id.detach())
        .map_err(|e| StoreError::Git(format!("write meta tree: {e}")))?;
    let sig = gix::actor::Signature {
        name: KERNEL_AUTHOR_NAME.into(),
        email: KERNEL_AUTHOR_EMAIL.into(),
        time: gix::date::Time::new(0, 0),
    };
    let mut time_buf = gix::date::parse::TimeBuf::default();
    let sig_ref = sig.to_ref(&mut time_buf);
    let commit = repo
        .new_commit_as(
            sig_ref,
            sig_ref,
            format!("lattice store identity (format v{REPOSITORY_FORMAT_VERSION})"),
            tree,
            std::iter::empty::<gix::ObjectId>(),
        )
        .map_err(|e| StoreError::Git(format!("create meta commit: {e}")))?;
    Ok(commit.id)
}

/// Read and parse the identity record of an open repository.
fn read_meta(repo: &gix::Repository, path: &Path) -> Result<StoreMeta> {
    let reference = repo
        .try_find_reference(META_REF)
        .map_err(|e| StoreError::Git(format!("find {META_REF}: {e}")))?
        .ok_or_else(|| StoreError::Corruption {
            detail: format!("'{}' has no {META_REF}", path.display()),
        })?;
    let oid = reference
        .target()
        .try_id()
        .ok_or_else(|| StoreError::Corruption {
            detail: format!("{META_REF} is symbolic"),
        })?
        .to_owned();
    let commit = repo.find_commit(oid).map_err(|_| StoreError::Corruption {
        detail: format!("{META_REF} target {oid} is not a commit"),
    })?;
    let tree = commit
        .tree()
        .map_err(|e| StoreError::Git(format!("read meta tree: {e}")))?;
    let entry = tree
        .lookup_entry_by_path(META_TREE_FILE)
        .map_err(|e| StoreError::Git(format!("lookup {META_TREE_FILE}: {e}")))?
        .ok_or_else(|| StoreError::Corruption {
            detail: format!("meta commit has no {META_TREE_FILE}"),
        })?;
    let object = entry
        .object()
        .map_err(|e| StoreError::Git(format!("load meta blob: {e}")))?;
    let blob = object
        .try_into_blob()
        .map_err(|e| StoreError::Git(format!("meta is not a blob: {e}")))?;
    meta::parse_meta(&blob.data)
}

/// `renameat2(RENAME_NOREPLACE)`: publish `from` at `to`, failing if `to`
/// exists. The primitive is required; its absence fails closed.
fn rename_noreplace(from: &Path, to: &Path) -> Result<()> {
    let from_c = CString::new(from.as_os_str().as_bytes()).expect("paths contain no NUL");
    let to_c = CString::new(to.as_os_str().as_bytes()).expect("paths contain no NUL");
    // SAFETY: both CStrings outlive the call; AT_FDCWD uses the process cwd
    // only for relative paths, and both paths are absolute by construction.
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
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    match err.raw_os_error() {
        Some(libc::EEXIST) => Err(StoreError::CasConflict {
            ref_name: "<first-create>".to_string(),
            detail: format!("'{}' already exists", to.display()),
        }),
        Some(code) if code == libc::ENOSYS || code == libc::EINVAL => {
            Err(StoreError::MissingPrimitive {
                primitive: "renameat2(RENAME_NOREPLACE)",
                detail: format!("errno {code}: {err}"),
            })
        }
        _ => Err(StoreError::Durability {
            operation: "renameat2(RENAME_NOREPLACE)",
            path: to.to_path_buf(),
            source: err,
        }),
    }
}

/// Create every missing directory component of `dir` and fsync the chain so
/// the layout directories themselves are durable before a store is published
/// inside them.
fn ensure_dirs_durable(dir: &Path) -> Result<()> {
    // Find the deepest existing ancestor.
    let mut missing = Vec::new();
    let mut cursor = dir;
    while !cursor.exists() {
        missing.push(cursor.to_path_buf());
        cursor = cursor.parent().ok_or_else(|| StoreError::Io {
            operation: "resolving existing ancestor",
            path: dir.to_path_buf(),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "no existing ancestor"),
        })?;
    }
    if missing.is_empty() {
        return Ok(());
    }
    std::fs::create_dir_all(dir).map_err(|source| StoreError::Io {
        operation: "creating layout directories",
        path: dir.to_path_buf(),
        source,
    })?;
    // fsync the first existing ancestor (its entry list changed) and every
    // newly created directory, shallow to deep.
    fsync_dir(cursor)?;
    for path in missing.iter().rev() {
        fsync_dir(path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fault;
    use crate::layout::StateLayout;

    fn temp_layout() -> (tempfile::TempDir, StateLayout) {
        let state = tempfile::tempdir().unwrap();
        let layout = StateLayout::in_state_root(state.path());
        (state, layout)
    }

    fn project_meta() -> StoreMeta {
        StoreMeta {
            store_kind: "workspace-history".into(),
            store_schema_version: 1,
            scope: StoreScope::Project {
                key: ProjectStoreKey::from_canonical_path_bytes(b"/ws"),
            },
        }
    }

    #[test]
    fn create_then_strict_open_roundtrip() {
        let (_state, layout) = temp_layout();
        let created = StoreRepo::create(
            &layout.lattice_root().join("store.git"),
            project_meta(),
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(created.meta().store_kind, "workspace-history");
        assert!(layout.lattice_root().join("store.git").exists());
        assert!(created.ref_target(META_REF).unwrap().is_some());

        let opened = StoreRepo::open(
            &layout.lattice_root().join("store.git"),
            &ExpectedMeta::from(&project_meta()),
        )
        .unwrap();
        assert_eq!(opened.meta(), created.meta());
    }

    #[test]
    fn create_is_idempotent_and_validates_existing() {
        let (_state, layout) = temp_layout();
        StoreRepo::create(
            &layout.lattice_root().join("store.git"),
            project_meta(),
            |_| Ok(()),
        )
        .unwrap();
        // Same identity: validate and reuse, never reinitialize.
        StoreRepo::create(
            &layout.lattice_root().join("store.git"),
            project_meta(),
            |_| panic!("init must not run on an existing store"),
        )
        .unwrap();
        // Wrong kind: typed corruption, not reinitialization.
        let err = StoreRepo::open(
            &layout.lattice_root().join("store.git"),
            &ExpectedMeta {
                store_kind: "other-kind".into(),
                ..ExpectedMeta::from(&project_meta())
            },
        )
        .unwrap_err();
        assert!(matches!(err, StoreError::Corruption { .. }));
    }

    #[test]
    fn project_scope_must_match_directory_key() {
        let (_state, layout) = temp_layout();
        let key = ProjectStoreKey::from_canonical_path_bytes(b"/ws");
        StoreRepo::create_project(
            &layout,
            &key,
            "workspace-history.git",
            "workspace-history",
            1,
            |_| Ok(()),
        )
        .unwrap();
        // Same store via a different key: the other project's directory does
        // not exist, so this is a typed miss, not corruption.
        let other = ProjectStoreKey::from_canonical_path_bytes(b"/other");
        let err = StoreRepo::open_project(
            &layout,
            &other,
            "workspace-history.git",
            "workspace-history",
            1,
        )
        .unwrap_err();
        assert!(matches!(err, StoreError::NotInitialized { .. }));

        // 同一仓库以错误的项目身份打开，必须拒绝。
        let real_path = layout.project_store(&key, "workspace-history.git");
        let err = StoreRepo::open(
            &real_path,
            &ExpectedMeta {
                store_kind: "workspace-history".into(),
                store_schema_version: 1,
                scope: StoreScope::Project { key: other },
            },
        )
        .unwrap_err();
        assert!(
            matches!(err, StoreError::Corruption { ref detail } if detail.contains("scope")),
            "a recorded project scope must fail closed under a different key: {err:?}"
        );

        // Scope forgery: hand-build a repo whose recorded key differs from
        // its directory key and require open to reject it.
        let forged = ProjectStoreKey::from_canonical_path_bytes(b"/forged");
        let path = layout.project_store(&key, "forged.git");
        StoreRepo::create(
            &path,
            StoreMeta {
                store_kind: "workspace-history".into(),
                store_schema_version: 1,
                scope: StoreScope::Project { key: forged },
            },
            |_| Ok(()),
        )
        .unwrap();
        let err = StoreRepo::open_project(&layout, &key, "forged.git", "workspace-history", 1)
            .unwrap_err();
        assert!(matches!(err, StoreError::Corruption { .. }));
    }

    #[test]
    fn concurrent_create_has_one_winner_and_valid_losers() {
        let (_state, layout) = temp_layout();
        let layout = std::sync::Arc::new(layout);
        let mut handles = Vec::new();
        for _ in 0..8 {
            let layout = std::sync::Arc::clone(&layout);
            handles.push(std::thread::spawn(move || {
                StoreRepo::create(
                    &layout.lattice_root().join("store.git"),
                    project_meta(),
                    |_| Ok(()),
                )
                .unwrap()
                .meta()
                .clone()
            }));
        }
        let metas: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert!(metas.iter().all(|m| *m == project_meta()));
        // Exactly one published repository, valid on strict open.
        StoreRepo::open(
            &layout.lattice_root().join("store.git"),
            &ExpectedMeta::from(&project_meta()),
        )
        .unwrap();
    }

    #[test]
    fn cas_conflict_is_typed() {
        let (_state, layout) = temp_layout();
        let repo = StoreRepo::create(
            &layout.lattice_root().join("store.git"),
            project_meta(),
            |_| Ok(()),
        )
        .unwrap();
        let blob = repo.write_blob(b"x").unwrap();
        repo.cas_ref(
            "refs/lattice/sessions/s/state",
            blob,
            RefExpectation::MustNotExist,
            "t",
        )
        .unwrap();
        // gix semantics: MustNotExist with a byte-identical existing target
        // is an idempotent success; a *different* value is the CAS conflict.
        repo.cas_ref(
            "refs/lattice/sessions/s/state",
            blob,
            RefExpectation::MustNotExist,
            "t",
        )
        .unwrap();
        let other = repo.write_blob(b"y").unwrap();
        let err = repo
            .cas_ref(
                "refs/lattice/sessions/s/state",
                other,
                RefExpectation::MustNotExist,
                "t",
            )
            .unwrap_err();
        assert!(
            matches!(err, StoreError::CasConflict { .. }),
            "MustNotExist with a different value must conflict, got: {err:?}"
        );
        // MustMatch against the wrong value conflicts too.
        let err = repo
            .cas_ref(
                "refs/lattice/sessions/s/state",
                other,
                RefExpectation::MustMatch(other),
                "t",
            )
            .unwrap_err();
        assert!(matches!(err, StoreError::CasConflict { .. }));
    }

    #[test]
    fn ref_cas_requires_object_barrier_before_publication() {
        let (_state, layout) = temp_layout();
        let repo = StoreRepo::create(
            &layout.lattice_root().join("store.git"),
            project_meta(),
            |_| Ok(()),
        )
        .unwrap();
        let blob = repo.write_blob(b"barrier-before-cas").unwrap();
        fault::fail_object_fsync(true);

        let err = repo
            .cas_ref(
                "refs/lattice/barrier/pre",
                blob,
                RefExpectation::MustNotExist,
                "barrier test",
            )
            .unwrap_err();
        assert!(matches!(err, StoreError::Durability { .. }), "got {err:?}");
        assert_eq!(repo.ref_target("refs/lattice/barrier/pre").unwrap(), None);

        repo.cas_ref(
            "refs/lattice/barrier/pre",
            blob,
            RefExpectation::MustNotExist,
            "barrier test retry",
        )
        .unwrap();
    }

    #[test]
    fn successful_cas_ref_runs_one_object_barrier_and_one_ref_barrier() {
        let (_state, layout) = temp_layout();
        let repo = StoreRepo::create(
            &layout.lattice_root().join("store.git"),
            project_meta(),
            |_| Ok(()),
        )
        .unwrap();
        let blob = repo.write_blob(b"barrier-counts").unwrap();
        fault::reset_barrier_counts();
        repo.cas_ref(
            "refs/lattice/barrier/counts",
            blob,
            RefExpectation::MustNotExist,
            "barrier count",
        )
        .unwrap();
        assert_eq!(
            fault::object_barrier_count(),
            1,
            "cas_ref fsyncs pending objects before the ref edit"
        );
        assert_eq!(
            fault::ref_barrier_count(),
            1,
            "cas_ref fsyncs the published ref; callers must not add a second ref barrier"
        );

        fault::reset_barrier_counts();
        repo.fsync_pending(&["refs/lattice/barrier/counts"])
            .unwrap();
        assert_eq!(
            fault::object_barrier_count(),
            0,
            "a follow-up fsync_pending after successful cas_ref has no pending objects"
        );
        assert_eq!(
            fault::ref_barrier_count(),
            1,
            "a follow-up fsync_pending is a redundant ref/dir barrier"
        );
    }

    #[test]
    fn fault_before_rename_leaves_no_target_and_cleans_temp() {
        let (_state, layout) = temp_layout();
        crate::fault::fail_before_rename(true);
        let err = StoreRepo::create(
            &layout.lattice_root().join("store.git"),
            project_meta(),
            |_| Ok(()),
        )
        .unwrap_err();
        assert!(matches!(err, StoreError::Durability { .. }));
        let target = layout.lattice_root().join("store.git");
        assert!(!target.exists());
        // No leftover temporaries.
        let leftovers: Vec<_> = std::fs::read_dir(layout.lattice_root())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(".store.git.tmp-")
            })
            .collect();
        assert!(leftovers.is_empty(), "leftover temps: {leftovers:?}");
        // Retry succeeds.
        StoreRepo::create(
            &layout.lattice_root().join("store.git"),
            project_meta(),
            |_| Ok(()),
        )
        .unwrap();
    }

    #[test]
    fn fault_at_parent_fsync_reports_durability_but_published() {
        let (_state, layout) = temp_layout();
        crate::fault::reset_repo_create_trace();
        crate::fault::fail_parent_fsync(true);
        let err = StoreRepo::create(
            &layout.lattice_root().join("store.git"),
            project_meta(),
            |_| Ok(()),
        )
        .unwrap_err();
        assert!(matches!(err, StoreError::Durability { .. }));
        assert!(
            crate::fault::created_repo_paths().is_empty(),
            "first-create tracing must not run before the parent fsync"
        );
        // The rename already published the repository; a later open validates
        // it. This is exactly why failure classification matters: the caller
        // must not retry blindly, but the store is not half-initialized.
        StoreRepo::open(
            &layout.lattice_root().join("store.git"),
            &ExpectedMeta::from(&project_meta()),
        )
        .unwrap();
    }

    #[test]
    fn orphan_temp_from_dead_creator_is_reclaimed() {
        let (_state, layout) = temp_layout();
        let parent = layout.lattice_root();
        std::fs::create_dir_all(parent).unwrap();
        let orphan = parent.join(".store.git.tmp-999999-0");
        std::fs::create_dir_all(&orphan).unwrap();
        std::fs::write(orphan.join("partial"), b"dead creator").unwrap();
        StoreRepo::create(
            &layout.lattice_root().join("store.git"),
            project_meta(),
            |_| Ok(()),
        )
        .unwrap();
        assert!(!orphan.exists());
        StoreRepo::open(
            &layout.lattice_root().join("store.git"),
            &ExpectedMeta::from(&project_meta()),
        )
        .unwrap();
    }

    fn assert_complete_loose_object_fanout(git_dir: &Path) {
        let objects = git_dir.join("objects");
        for prefix in 0u8..=255 {
            let dir = objects.join(format!("{prefix:02x}"));
            let meta = std::fs::symlink_metadata(&dir).unwrap_or_else(|e| {
                panic!(
                    "loose-object fanout {} missing or unreadable: {e}",
                    dir.display()
                )
            });
            assert!(
                meta.is_dir() && !meta.file_type().is_symlink(),
                "loose-object fanout {} must be a directory",
                dir.display()
            );
        }
    }

    #[test]
    fn first_create_constructs_all_256_fanout_directories() {
        let (_state, layout) = temp_layout();
        let repo = StoreRepo::create(
            &layout.lattice_root().join("store.git"),
            project_meta(),
            |_| Ok(()),
        )
        .unwrap();
        assert_complete_loose_object_fanout(repo.git_dir());
    }

    #[test]
    fn first_create_records_publication_and_reuse_does_not() {
        crate::fault::reset_repo_create_trace();
        let (_state, layout) = temp_layout();
        let path = layout.lattice_root().join("store.git");
        StoreRepo::create(&path, project_meta(), |_| Ok(())).unwrap();
        let created = crate::fault::created_repo_paths();
        assert_eq!(created, vec![path.clone()]);
        StoreRepo::create(&path, project_meta(), |_| {
            panic!("init must not run on an existing store")
        })
        .unwrap();
        assert_eq!(
            crate::fault::created_repo_paths(),
            vec![path],
            "reusing an existing store is not a first-create"
        );
        StoreRepo::open(
            &layout.lattice_root().join("store.git"),
            &ExpectedMeta::from(&project_meta()),
        )
        .unwrap();
        assert_eq!(crate::fault::created_repo_paths().len(), 1);
    }

    #[test]
    fn strict_open_rejects_invalid_fanout_without_repair() {
        for (name, prefix, replacement_bytes, expected_detail_contains) in [
            ("missing_fanout", "ff", None, Some("objects/ff")),
            (
                "malformed_fanout",
                "aa",
                Some(b"not a directory".as_slice()),
                None,
            ),
        ] {
            let (_state, layout) = temp_layout();
            let repo = StoreRepo::create(
                &layout.lattice_root().join("store.git"),
                project_meta(),
                |_| Ok(()),
            )
            .unwrap_or_else(|err| panic!("{name}: create: {err:?}"));
            let fanout = repo.git_dir().join("objects").join(prefix);
            if replacement_bytes.is_some() && fanout.is_dir()
                || replacement_bytes.is_none() && fanout.exists()
            {
                std::fs::remove_dir_all(&fanout)
                    .unwrap_or_else(|err| panic!("{name}: remove fanout: {err}"));
            }
            if let Some(bytes) = replacement_bytes {
                std::fs::write(&fanout, bytes)
                    .unwrap_or_else(|err| panic!("{name}: replace fanout: {err}"));
            } else {
                assert!(
                    !fanout.exists(),
                    "{name}: fanout must be absent before open"
                );
            }
            drop(repo);

            let err = StoreRepo::open(
                &layout.lattice_root().join("store.git"),
                &ExpectedMeta::from(&project_meta()),
            )
            .expect_err(&format!("{name}: invalid fanout must fail open"));
            if let Some(expected_detail) = expected_detail_contains {
                assert!(
                    matches!(err, StoreError::Corruption { ref detail } if detail.contains(expected_detail)),
                    "{name}: missing fanout is corruption, not a repair invitation: {err:?}"
                );
            } else {
                assert!(
                    matches!(err, StoreError::Corruption { .. }),
                    "{name}: a fanout file is corruption: {err:?}"
                );
            }
            if replacement_bytes.is_some() {
                let meta = std::fs::symlink_metadata(&fanout)
                    .unwrap_or_else(|err| panic!("{name}: fanout metadata: {err}"));
                assert!(
                    meta.is_file(),
                    "{name}: strict open must not replace a malformed fanout"
                );
            } else {
                assert!(
                    !fanout.exists(),
                    "{name}: strict open must not create a missing fanout directory"
                );
            }
        }
    }

    #[test]
    fn object_barrier_failures_prevent_ref_cas() {
        for (name, blob_bytes, ref_name, log_message, fail_directory) in [
            (
                "object_file",
                b"object-file-barrier".as_slice(),
                "refs/lattice/barrier/object-file",
                "object file barrier",
                false,
            ),
            (
                "fanout_directory",
                b"fanout-dir-barrier".as_slice(),
                "refs/lattice/barrier/fanout-dir",
                "fanout dir barrier",
                true,
            ),
        ] {
            let (_state, layout) = temp_layout();
            let repo = StoreRepo::create(
                &layout.lattice_root().join("store.git"),
                project_meta(),
                |_| Ok(()),
            )
            .unwrap_or_else(|err| panic!("{name}: create: {err:?}"));
            let blob = repo
                .write_blob(blob_bytes)
                .unwrap_or_else(|err| panic!("{name}: write blob: {err:?}"));
            let object = crate::durability::loose_object_path(repo.git_dir(), blob);
            let failure_path = if fail_directory {
                object
                    .parent()
                    .unwrap_or_else(|| panic!("{name}: object parent"))
                    .to_path_buf()
            } else {
                object
            };
            fault::reset_fsync_trace();
            fault::fail_fsync_path(&failure_path);

            let err = repo
                .cas_ref(ref_name, blob, RefExpectation::MustNotExist, log_message)
                .expect_err(&format!("{name}: object barrier must fail"));
            match err {
                StoreError::Durability { path, .. } => assert_eq!(path, failure_path, "{name}"),
                other => panic!("{name}: expected Durability on {failure_path:?}, got {other:?}"),
            }
            assert_eq!(
                repo.ref_target(ref_name)
                    .unwrap_or_else(|err| panic!("{name}: ref target: {err:?}")),
                None,
                "{name}"
            );
        }
    }

    #[test]
    fn cas_ref_does_not_sync_top_level_objects_directory() {
        let (_state, layout) = temp_layout();
        let repo = StoreRepo::create(
            &layout.lattice_root().join("store.git"),
            project_meta(),
            |_| Ok(()),
        )
        .unwrap();
        let blob = repo.write_blob(b"no-objects-dir-fsync").unwrap();
        let object = crate::durability::loose_object_path(repo.git_dir(), blob);
        let fanout = object.parent().unwrap().to_path_buf();
        let objects = repo.git_dir().join("objects");
        fault::reset_fsync_trace();
        repo.cas_ref(
            "refs/lattice/barrier/no-objects",
            blob,
            RefExpectation::MustNotExist,
            "no objects dir",
        )
        .unwrap();
        let synced = fault::synced_paths();
        assert!(
            synced.iter().any(|p| p == &object),
            "object file must be synced: {synced:?}"
        );
        assert!(
            synced.iter().any(|p| p == &fanout),
            "exact fanout directory must be synced: {synced:?}"
        );
        assert!(
            !synced.iter().any(|p| p == &objects),
            "steady-state cas_ref must not fsync objects/: {synced:?}"
        );
    }

    #[test]
    fn exact_retry_after_post_cas_ref_barrier_failure() {
        let (_state, layout) = temp_layout();
        let repo = StoreRepo::create(
            &layout.lattice_root().join("store.git"),
            project_meta(),
            |_| Ok(()),
        )
        .unwrap();
        let blob = repo.write_blob(b"exact-retry-bytes").unwrap();
        fault::fail_ref_fsync(true);
        let err = repo
            .cas_ref(
                "refs/lattice/barrier/exact-retry",
                blob,
                RefExpectation::MustNotExist,
                "exact retry",
            )
            .unwrap_err();
        assert!(matches!(err, StoreError::Durability { .. }), "got {err:?}");
        assert_eq!(
            repo.ref_target("refs/lattice/barrier/exact-retry").unwrap(),
            Some(blob)
        );
        repo.fsync_pending(&["refs/lattice/barrier/exact-retry"])
            .unwrap();
        assert_eq!(
            repo.ref_target("refs/lattice/barrier/exact-retry").unwrap(),
            Some(blob)
        );
    }
}

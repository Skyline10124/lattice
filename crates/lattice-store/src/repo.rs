// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Typed create/open, object/commit mechanics, ref CAS/transactions, the
//! per-repository write lock, and atomic first-create publication (ADR 0005
//! §4).
//!
//! A [`StoreRepo`] is one validated private bare repository. Domain wrappers
//! (`lattice-session`, `lancet-runtime`, later `lancet-audit`) own their
//! repository file names, store kinds, schema versions, refs, records, and
//! invariants; this module owns only domain-neutral mechanics. There is no
//! generic `put(key, value)`, no raw `gix::Repository` getter, no all-stores
//! handle, and no pluggable backend.
//!
//! ## First-create protocol
//!
//! 1. take the target-specific create lock in the parent directory;
//! 2. re-check the target: if it now exists, validate the winner, observe the
//!    same parent durability barrier, and return (loser path);
//! 3. run capability probes against the actual parent location — a missing
//!    primitive fails closed;
//! 4. initialize the bare repository in a same-parent sibling temporary
//!    directory, write the `refs/lattice/meta` identity commit plus the
//!    domain's initial refs, and fsync the whole tree;
//! 5. publish with `renameat2(RENAME_NOREPLACE)`, fsync the parent, and
//!    reopen and validate the published identity (winner path).
//!
//! The create lock and the repository write lock never overlap: the create
//! lock is released before [`StoreRepo::open`] returns the repository.

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};

use gix::objs::tree::EntryKind;
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};

use crate::durability::{fsync_dir, fsync_publication, fsync_tree};
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

/// Retention pin namespace: `refs/lattice/retained/<oid>` keeps an object
/// reachable independent of any domain ref (ADR 0005 §4 "retention
/// primitives").
pub const RETAINED_REF_PREFIX: &str = "refs/lattice/retained";

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

/// One edit inside a multi-ref transaction.
#[derive(Debug, Clone)]
pub struct RefTxnEdit {
    /// Full ref name (must be under `refs/lattice/`).
    pub name: String,
    /// What the ref must look like before the edit.
    pub expect: RefExpectation,
    /// The new target; `None` deletes the ref.
    pub new: Option<gix::ObjectId>,
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
        let meta = read_meta(&repo, path)?;
        meta::check_expected(&meta, expected)?;
        Ok(Self {
            git_dir: path.to_path_buf(),
            repo: Mutex::new(repo),
            meta,
            pending: Mutex::new(std::collections::BTreeSet::new()),
        })
    }

    /// Strictly open the global store `name` from the layout root.
    pub fn open_global(
        layout: &StateLayout,
        name: &str,
        store_kind: &str,
        store_schema_version: u32,
    ) -> Result<Self> {
        Self::open(
            &layout.lattice_root().join(name),
            &ExpectedMeta {
                store_kind: store_kind.to_string(),
                store_schema_version,
                scope: StoreScope::Global,
            },
        )
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
    /// This is the only initialization path, and it is restricted to the
    /// composition entry points the ADR names (`session new`, #139-A's
    /// isolated harness). Everything else strictly opens.
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
        let parent = path.parent().ok_or_else(|| StoreError::Corruption {
            detail: format!("store path '{}' has no parent", path.display()),
        })?;
        let name = path.file_name().ok_or_else(|| StoreError::Corruption {
            detail: format!("store path '{}' has no file name", path.display()),
        })?;

        ensure_dirs_durable(parent)?;

        // 1. Target-specific create lock in the parent directory. This lock
        //    never overlaps the repository write lock: it is dropped before
        //    the validated repository is returned.
        let create_lock_path = parent.join(format!(".{}.create.lock", name.to_string_lossy()));
        let _create_lock = acquire_flock_strict(&create_lock_path, Blocking::Block)?;

        // 2. Loser path: a concurrent creator (or an earlier success) already
        //    published. Validate the winner and observe the same parent
        //    durability barrier.
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
            let temp_repo = gix::init_bare(&temp).map_err(|e| {
                StoreError::Git(format!("init bare repository in '{}': {e}", temp.display()))
            })?;
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

    /// Create the global store `name` under the layout root.
    pub fn create_global(
        layout: &StateLayout,
        name: &str,
        meta: StoreMeta,
        init: impl FnOnce(&StoreRepo) -> Result<()>,
    ) -> Result<Self> {
        debug_assert_eq!(meta.scope, StoreScope::Global);
        Self::create(&layout.lattice_root().join(name), meta, init)
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
    /// CAS/transaction, and durability barriers. Readers do not take it.
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

    /// Enumerate `(name, target)` pairs under a ref prefix, sorted by name.
    pub fn enumerate_refs(&self, prefix: &str) -> Result<Vec<(String, gix::ObjectId)>> {
        let repo = self.repo();
        let platform = repo
            .references()
            .map_err(|e| StoreError::Git(format!("enumerate references: {e}")))?;
        let iter = platform
            .prefixed(prefix)
            .map_err(|e| StoreError::Git(format!("iterate references: {e}")))?;
        let mut out = Vec::new();
        for r in iter {
            let r = r.map_err(|e| StoreError::Git(format!("read reference: {e}")))?;
            let name = r.name().as_bstr().to_string();
            if let Some(oid) = r.target().try_id() {
                out.push((name, oid.to_owned()));
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(out)
    }

    /// Read a blob's bytes.
    pub fn read_blob(&self, oid: gix::ObjectId) -> Result<Vec<u8>> {
        let repo = self.repo();
        let object = repo
            .find_object(oid)
            .map_err(|e| StoreError::Git(format!("find object {oid}: {e}")))?;
        let blob = object
            .try_into_blob()
            .map_err(|e| StoreError::Git(format!("object {oid} is not a blob: {e}")))?;
        Ok(blob.data.clone())
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

    /// Read the blob at `path` inside a commit's tree.
    pub fn read_tree_blob(&self, commit_oid: gix::ObjectId, path: &str) -> Result<Vec<u8>> {
        let repo = self.repo();
        let commit = repo
            .find_commit(commit_oid)
            .map_err(|e| StoreError::Git(format!("find commit for tree read: {e}")))?;
        let tree = commit
            .tree()
            .map_err(|e| StoreError::Git(format!("read commit tree: {e}")))?;
        let entry = tree
            .lookup_entry_by_path(path)
            .map_err(|e| StoreError::Git(format!("lookup tree path {path}: {e}")))?
            .ok_or_else(|| StoreError::Git(format!("find tree path {path}: not found")))?;
        let object = entry
            .object()
            .map_err(|e| StoreError::Git(format!("load blob object for {path}: {e}")))?;
        let blob = object
            .try_into_blob()
            .map_err(|e| StoreError::Git(format!("peel {path} to blob: {e}")))?;
        Ok(blob.data.clone())
    }

    /// Newest-first rev-walk from `tips`, optionally hiding `hide`.
    pub fn rev_walk(
        &self,
        tips: &[gix::ObjectId],
        hide: &[gix::ObjectId],
    ) -> Result<Vec<gix::ObjectId>> {
        let repo = self.repo();
        let mut platform = repo.rev_walk(tips.iter().copied());
        if !hide.is_empty() {
            platform = platform.with_hidden(hide.to_vec());
        }
        let walk = platform
            .all()
            .map_err(|e| StoreError::Git(format!("create revwalk: {e}")))?;
        let mut ids = Vec::new();
        for item in walk {
            let info = item.map_err(|e| StoreError::Git(format!("revwalk error: {e}")))?;
            ids.push(info.id);
        }
        Ok(ids)
    }

    /// Merge base of two commits, if one exists.
    pub fn merge_base(&self, a: gix::ObjectId, b: gix::ObjectId) -> Result<Option<gix::ObjectId>> {
        let repo = self.repo();
        match repo.merge_base(a, b) {
            Ok(id) => Ok(Some(id.detach())),
            Err(_) => Ok(None),
        }
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
        self.repo()
            .reference(ref_name, new, expect.into(), log_message)
            .map(|_| ())
            .map_err(|e| classify_ref_error(ref_name, e))
    }

    /// Delete one ref under `expect`.
    pub fn delete_ref(&self, ref_name: &str, expect: RefExpectation) -> Result<()> {
        validate_ref_name(ref_name)?;
        let name: gix::refs::FullName = ref_name
            .try_into()
            .map_err(|e| StoreError::Git(format!("ref name {ref_name}: {e}")))?;
        self.repo()
            .edit_reference(RefEdit {
                change: Change::Delete {
                    expected: expect.into(),
                    log: RefLog::AndReference,
                },
                name,
                deref: false,
            })
            .map(|_| ())
            .map_err(|e| classify_ref_error(ref_name, e))
    }

    /// Apply a multi-ref transaction atomically within this repository.
    pub fn ref_transaction(&self, edits: &[RefTxnEdit]) -> Result<()> {
        let mut gix_edits = Vec::with_capacity(edits.len());
        for edit in edits {
            validate_ref_name(&edit.name)?;
            let name: gix::refs::FullName = edit
                .name
                .as_str()
                .try_into()
                .map_err(|e| StoreError::Git(format!("ref name {}: {e}", edit.name)))?;
            let change = match edit.new {
                Some(target) => Change::Update {
                    expected: edit.expect.into(),
                    log: LogChange {
                        mode: RefLog::AndReference,
                        ..Default::default()
                    },
                    new: gix::refs::Target::Object(target),
                },
                None => Change::Delete {
                    expected: edit.expect.into(),
                    log: RefLog::AndReference,
                },
            };
            gix_edits.push(RefEdit {
                change,
                name,
                deref: false,
            });
        }
        let repo = self.repo();
        repo.edit_references(gix_edits)
            .map(|_| ())
            .map_err(|e| classify_ref_error("<transaction>", e))
    }

    // ─── Retention primitives ──────────────────────────────────────────

    /// Pin `oid` under `refs/lattice/retained/<oid>` so it stays reachable
    /// independent of any domain ref. Idempotent.
    pub fn retain(&self, oid: gix::ObjectId) -> Result<()> {
        let ref_name = format!("{RETAINED_REF_PREFIX}/{oid}");
        match self.ref_target(&ref_name)? {
            Some(existing) if existing == oid => Ok(()),
            Some(_) => Err(StoreError::Corruption {
                detail: format!("retention pin for {oid} points elsewhere"),
            }),
            None => self.cas_ref(
                &ref_name,
                oid,
                RefExpectation::MustNotExist,
                "lattice retain",
            ),
        }
    }

    /// Release a retention pin. Idempotent.
    pub fn release(&self, oid: gix::ObjectId) -> Result<()> {
        let ref_name = format!("{RETAINED_REF_PREFIX}/{oid}");
        match self.ref_target(&ref_name)? {
            Some(existing) if existing == oid => {
                self.delete_ref(&ref_name, RefExpectation::MustMatch(oid))
            }
            Some(_) => Err(StoreError::Corruption {
                detail: format!("retention pin for {oid} points elsewhere"),
            }),
            None => Ok(()),
        }
    }

    // ─── Durability ────────────────────────────────────────────────────

    /// Publication barrier after a write sequence: fsync the given loose
    /// objects and refs plus their directories. Writers call this while still
    /// holding the write lock.
    pub fn fsync_publication(&self, oids: &[gix::ObjectId], refs: &[&str]) -> Result<()> {
        fsync_publication(&self.git_dir, oids, refs)
    }

    /// Publication barrier over every object written through this handle
    /// since the last barrier, plus the given refs. The pending set is
    /// drained whether or not the barrier succeeds.
    pub fn fsync_pending(&self, refs: &[&str]) -> Result<()> {
        let oids: Vec<gix::ObjectId> = {
            let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            std::mem::take(&mut *pending).into_iter().collect()
        };
        fsync_publication(&self.git_dir, &oids, refs)
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

/// Validate a ref name the kernel will write: syntactically legal and inside
/// the `refs/lattice/` namespace.
fn validate_ref_name(ref_name: &str) -> Result<()> {
    if !ref_name.starts_with(LATTICE_REF_PREFIX) {
        return Err(StoreError::Git(format!(
            "ref '{ref_name}' is outside the {LATTICE_REF_PREFIX} namespace"
        )));
    }
    let _: gix::refs::FullName = ref_name
        .try_into()
        .map_err(|e| StoreError::Git(format!("illegal ref name '{ref_name}': {e}")))?;
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
            | Prepare::DeleteReferenceMustExist { .. }
            | Prepare::LockAcquire { .. },
        ) => StoreError::CasConflict {
            ref_name: ref_name.to_string(),
            detail: error.to_string(),
        },
        _ => StoreError::Git(format!("update ref {ref_name}: {error}")),
    }
}

/// Write the identity commit for a fresh repository (meta.json in a tree).
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
    use crate::layout::StateLayout;

    fn temp_layout() -> (tempfile::TempDir, StateLayout) {
        let state = tempfile::tempdir().unwrap();
        let layout = StateLayout::in_state_root(state.path());
        (state, layout)
    }

    fn global_meta() -> StoreMeta {
        StoreMeta {
            store_kind: "session".into(),
            store_schema_version: 1,
            scope: StoreScope::Global,
        }
    }

    #[test]
    fn create_then_strict_open_roundtrip() {
        let (_state, layout) = temp_layout();
        let created =
            StoreRepo::create_global(&layout, "sessions.git", global_meta(), |_| Ok(())).unwrap();
        assert_eq!(created.meta().store_kind, "session");
        assert!(layout.lattice_root().join("sessions.git").exists());
        assert!(created.ref_target(META_REF).unwrap().is_some());

        let opened = StoreRepo::open_global(&layout, "sessions.git", "session", 1).unwrap();
        assert_eq!(opened.meta(), created.meta());
    }

    #[test]
    fn create_is_idempotent_and_validates_existing() {
        let (_state, layout) = temp_layout();
        StoreRepo::create_global(&layout, "sessions.git", global_meta(), |_| Ok(())).unwrap();
        // Same identity: validate and reuse, never reinitialize.
        StoreRepo::create_global(&layout, "sessions.git", global_meta(), |_| {
            panic!("init must not run on an existing store")
        })
        .unwrap();
        // Wrong kind: typed corruption, not reinitialization.
        let err = StoreRepo::open_global(&layout, "sessions.git", "other-kind", 1).unwrap_err();
        assert!(matches!(err, StoreError::Corruption { .. }));
    }

    #[test]
    fn strict_open_missing_is_not_initialized() {
        let (_state, layout) = temp_layout();
        let err = StoreRepo::open_global(&layout, "sessions.git", "session", 1).unwrap_err();
        assert!(matches!(err, StoreError::NotInitialized { .. }));
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
        // Same store via a different key: scope mismatch corruption.
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
                StoreRepo::create_global(&layout, "sessions.git", global_meta(), |_| Ok(()))
                    .unwrap()
                    .meta()
                    .clone()
            }));
        }
        let metas: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert!(metas.iter().all(|m| *m == global_meta()));
        // Exactly one published repository, valid on strict open.
        StoreRepo::open_global(&layout, "sessions.git", "session", 1).unwrap();
    }

    #[test]
    fn cas_conflict_is_typed() {
        let (_state, layout) = temp_layout();
        let repo =
            StoreRepo::create_global(&layout, "sessions.git", global_meta(), |_| Ok(())).unwrap();
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
    fn refs_outside_lattice_namespace_are_rejected() {
        let (_state, layout) = temp_layout();
        let repo =
            StoreRepo::create_global(&layout, "sessions.git", global_meta(), |_| Ok(())).unwrap();
        let blob = repo.write_blob(b"x").unwrap();
        let err = repo
            .cas_ref("refs/heads/main", blob, RefExpectation::MustNotExist, "t")
            .unwrap_err();
        assert!(matches!(err, StoreError::Git(_)));
    }

    #[test]
    fn retain_and_release_roundtrip() {
        let (_state, layout) = temp_layout();
        let repo =
            StoreRepo::create_global(&layout, "sessions.git", global_meta(), |_| Ok(())).unwrap();
        let blob = repo.write_blob(b"pin me").unwrap();
        repo.retain(blob).unwrap();
        assert!(
            repo.ref_target(&format!("{RETAINED_REF_PREFIX}/{blob}"))
                .unwrap()
                .is_some()
        );
        repo.retain(blob).unwrap(); // idempotent
        repo.release(blob).unwrap();
        assert!(
            repo.ref_target(&format!("{RETAINED_REF_PREFIX}/{blob}"))
                .unwrap()
                .is_none()
        );
        repo.release(blob).unwrap(); // idempotent
    }

    #[test]
    fn ref_transaction_is_atomic() {
        let (_state, layout) = temp_layout();
        let repo =
            StoreRepo::create_global(&layout, "sessions.git", global_meta(), |_| Ok(())).unwrap();
        let blob = repo.write_blob(b"x").unwrap();
        repo.ref_transaction(&[
            RefTxnEdit {
                name: "refs/lattice/a".into(),
                expect: RefExpectation::MustNotExist,
                new: Some(blob),
            },
            RefTxnEdit {
                name: "refs/lattice/b".into(),
                expect: RefExpectation::MustNotExist,
                new: Some(blob),
            },
        ])
        .unwrap();
        // A conflicting edit aborts the whole transaction: `a` must not move
        // (different value under MustNotExist) and `c` must not appear.
        let other = repo.write_blob(b"y").unwrap();
        let err = repo
            .ref_transaction(&[
                RefTxnEdit {
                    name: "refs/lattice/a".into(),
                    expect: RefExpectation::MustNotExist,
                    new: Some(other),
                },
                RefTxnEdit {
                    name: "refs/lattice/c".into(),
                    expect: RefExpectation::MustNotExist,
                    new: Some(blob),
                },
            ])
            .unwrap_err();
        assert!(matches!(err, StoreError::CasConflict { .. }));
        assert_eq!(repo.ref_target("refs/lattice/a").unwrap(), Some(blob));
        assert!(repo.ref_target("refs/lattice/c").unwrap().is_none());
    }

    #[test]
    fn fault_before_rename_leaves_no_target_and_cleans_temp() {
        let (_state, layout) = temp_layout();
        crate::fault::fail_before_rename(true);
        let err = StoreRepo::create_global(&layout, "sessions.git", global_meta(), |_| Ok(()))
            .unwrap_err();
        assert!(matches!(err, StoreError::Durability { .. }));
        let target = layout.lattice_root().join("sessions.git");
        assert!(!target.exists());
        // No leftover temporaries.
        let leftovers: Vec<_> = std::fs::read_dir(layout.lattice_root())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(".sessions.git.tmp-")
            })
            .collect();
        assert!(leftovers.is_empty(), "leftover temps: {leftovers:?}");
        // Retry succeeds.
        StoreRepo::create_global(&layout, "sessions.git", global_meta(), |_| Ok(())).unwrap();
    }

    #[test]
    fn fault_at_parent_fsync_reports_durability_but_published() {
        let (_state, layout) = temp_layout();
        crate::fault::fail_parent_fsync(true);
        let err = StoreRepo::create_global(&layout, "sessions.git", global_meta(), |_| Ok(()))
            .unwrap_err();
        assert!(matches!(err, StoreError::Durability { .. }));
        // The rename already published the repository; a later open validates
        // it. This is exactly why failure classification matters: the caller
        // must not retry blindly, but the store is not half-initialized.
        StoreRepo::open_global(&layout, "sessions.git", "session", 1).unwrap();
    }

    #[test]
    fn orphan_temp_from_dead_creator_is_reclaimed() {
        let (_state, layout) = temp_layout();
        let parent = layout.lattice_root();
        std::fs::create_dir_all(parent).unwrap();
        let orphan = parent.join(".sessions.git.tmp-999999-0");
        std::fs::create_dir_all(&orphan).unwrap();
        std::fs::write(orphan.join("partial"), b"dead creator").unwrap();
        StoreRepo::create_global(&layout, "sessions.git", global_meta(), |_| Ok(())).unwrap();
        assert!(!orphan.exists());
        StoreRepo::open_global(&layout, "sessions.git", "session", 1).unwrap();
    }

    #[test]
    fn tree_spec_edits_and_rev_walk() {
        let (_state, layout) = temp_layout();
        let repo =
            StoreRepo::create_global(&layout, "sessions.git", global_meta(), |_| Ok(())).unwrap();
        let blob = repo.write_blob(b"content").unwrap();
        let tree = repo
            .write_tree(TreeSpec::new().upsert("file.txt", EntryKind::Blob, blob))
            .unwrap();
        let c1 = repo.write_commit_at(tree, &[], "first", 1).unwrap();
        let tree2 = repo
            .write_tree(TreeSpec::from(tree).upsert("two.txt", EntryKind::Blob, blob))
            .unwrap();
        let c2 = repo.write_commit_at(tree2, &[c1], "second", 2).unwrap();
        assert_eq!(repo.commit_parents(c2).unwrap(), vec![c1]);
        assert_eq!(repo.commit_tree(c1).unwrap(), tree);
        assert_eq!(
            repo.read_tree_blob(c2, "two.txt").unwrap(),
            b"content".to_vec()
        );
        let walk = repo.rev_walk(&[c2], &[]).unwrap();
        // Exactly c2 and its parent c1; the meta commit is not an ancestor.
        assert_eq!(walk.len(), 2);
        assert!(walk.contains(&c2) && walk.contains(&c1));
    }
}

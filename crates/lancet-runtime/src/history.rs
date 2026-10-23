// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Private workspace mutation history (GitHub #63, migrated to the
//! `lattice-store` kernel in #143).
//!
//! Every Atomic Local Mutation that LATTICE **successfully installs** is
//! appended to a durable history that is independent of the Session History,
//! of the Local Workspace, and of the user's project `.git`.
//!
//! The history is a project-scoped store in the private-Git kernel: blobs for
//! file content, trees for the before/after state of the paths LATTICE has
//! touched, commits for ordering and parent relationships, and kernel refs for
//! reachability. There is no parallel custom blob format, receipt store, or
//! SQLite workspace graph, and this module owns no durability primitives of
//! its own — object writing, ref CAS, the repository write lock, retention,
//! and fsync barriers all belong to the kernel.
//!
//! ## What is and is not recorded
//!
//! The history records **only** paths LATTICE has successfully mutated. It is
//! not an execution workspace, a projection, a checkpoint, or a whole-project
//! snapshot: a path appears in the history tree the first time LATTICE touches
//! it and never before. Mutation Precondition failures, `ReplanRequired`,
//! policy/Guard/effect denials, and any pre-install error record nothing and
//! never move [`CURRENT_REF`].
//!
//! ## Layout
//!
//! ```text
//! $XDG_STATE_HOME/lattice/projects/<ProjectStoreKey>/workspace-history.git
//!   refs/lattice/meta                           -> kernel store identity
//!   refs/lattice/workspace-history/current      -> newest recorded mutation
//! ```
//!
//! `current` 仅随记录前进，所有已记录提交始终可达；会话退回不修改它。
//! 此库是私有状态，不触及用户项目的 refs。
//!
//! ## Before/after state
//!
//! For every mutation commit `N`, commit `N-1`'s tree is the *before* state
//! and commit `N`'s tree is the *after* state. The recorder enforces that
//! rather than assuming it: the Local Workspace is shared with the user, so
//! the history's idea of a path can be stale (the user edited the file in
//! their editor, a `git checkout` swapped branch contents, the file was
//! deleted outside LATTICE) and a path can be missing entirely the first time
//! `mutate__edit` touches a pre-existing file.
//!
//! Before appending, the recorder compares the parent tree's bytes at the path
//! against the before state the installer actually observed. When they differ
//! it writes a preceding **baseline** commit that makes the parent tree exact
//! — setting the path to the observed pre-mutation bytes, or *removing* it
//! when the real before state was absence (a `mutate__create` after an external
//! delete). A baseline commit is not a mutation commit: it is labelled
//! `lattice-history-kind: baseline`, and `current` still advances exactly once
//! per recorded mutation.
//!
//! ## Concurrency
//!
//! Recording holds the kernel's standard per-repository write
//! lock across the read-build-publish sequence, so a second LATTICE process —
//! or a second runtime in one process — cannot interleave. The compare-and-set
//! on `current` remains the fail-closed arbiter: a writer that somehow loses
//! re-reads the tip and appends on top of the winner, because the bytes it is
//! recording are already installed on disk and dropping them would lose real
//! history.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
#[cfg(any(test, feature = "test-fault-injection"))]
use std::sync::atomic::{AtomicBool, Ordering};

use gix::objs::tree::EntryKind;
use lancet_protocol::{InvocationId, TurnId};

use lattice_store::{
    Blocking, LockMode, ProjectStoreKey, RefExpectation, StateLayout, StoreError, StoreRepo,
    TreeSpec,
};

/// Ref holding the newest recorded mutation commit.
pub const CURRENT_REF: &str = "refs/lattice/workspace-history/current";

/// Directory name of the project-scoped history store.
pub const HISTORY_REPO_DIR: &str = "workspace-history.git";

/// Kernel store kind recorded in `refs/lattice/meta`.
pub const HISTORY_STORE_KIND: &str = "workspace-history";

/// Domain schema version of this store's contents.
pub const HISTORY_STORE_SCHEMA_VERSION: u32 = 3;

/// Lock file name of the per-project `WorkspaceOperationLock` (ADR 0005 §5).
pub const WORKSPACE_OPERATION_LOCK: &str = "workspace.lock";

/// Trailer key marking a commit as a mutation or a baseline.
const KIND_TRAILER: &str = "lattice-history-kind";
const KIND_MUTATION: &str = "mutation";
const KIND_BASELINE: &str = "baseline";

/// Failure recording into or reading from the private history.
#[derive(Debug, thiserror::Error)]
pub enum WorkspaceHistoryError {
    /// The private repository could not be opened, created, or written.
    #[error("private workspace history repository: {0}")]
    Repo(String),
    /// A published history commit violates the closed domain schema.
    #[error("workspace history commit '{commit_id}' is malformed: {reason}")]
    Corruption {
        /// The malformed commit.
        commit_id: String,
        /// The violated history invariant.
        reason: String,
    },
    /// The kernel error, taxonomy preserved (ADR 0005 §4 typed failures).
    ///
    /// `From<StoreError>` keeps `NotInitialized`/`Corruption`/`Busy`/
    /// `CasConflict`/`MissingPrimitive` matchable so a domain caller never
    /// has to sniff a Display string. Operation-lock conflicts are `Busy`;
    /// the kernel retains the older `LockUpgrade` enum variant for API
    /// compatibility, but this path never emits it.
    #[error("store kernel error: {0}")]
    Kernel(#[source] StoreError),
    /// A mutation path was not a workspace-relative file path.
    #[error("workspace history path '{path}' rejected: {reason}")]
    Path {
        /// The rejected path, as supplied.
        path: String,
        /// Why it was rejected.
        reason: &'static str,
    },
    /// A caller-supplied object id was not a valid hex object id.
    #[error("workspace history object id '{oid}' is not valid: {reason}")]
    ObjectId {
        /// The rejected object id.
        oid: String,
        /// Why it was rejected.
        reason: String,
    },
}

impl From<StoreError> for WorkspaceHistoryError {
    fn from(err: StoreError) -> Self {
        Self::Kernel(err)
    }
}

/// Where the private workspace history lives.
#[derive(Debug, Clone)]
pub enum WorkspaceHistoryBacking {
    /// No history recording.
    ///
    /// For unit tests and embedders that have no per-project state root.
    /// Production Turn runtimes always resolve [`Self::Project`].
    Disabled,
    /// Project-scoped store under a state layout, addressed by the Local
    /// Workspace root's [`ProjectStoreKey`].
    Project {
        /// The private-state layout the store lives under.
        layout: StateLayout,
        /// Storage address derived from the canonical workspace root.
        key: ProjectStoreKey,
    },
}

impl WorkspaceHistoryBacking {
    /// Project-scoped backing under a caller-supplied state root
    /// (tests/embedders).
    ///
    /// `workspace_root` is the full canonical Local Workspace root; the key is
    /// derived from its raw path bytes by the kernel (ADR 0005 §2).
    pub fn for_project_in(
        state_root: &Path,
        workspace_root: &Path,
    ) -> Result<Self, WorkspaceHistoryError> {
        Ok(Self::Project {
            layout: StateLayout::in_state_root(state_root),
            key: ProjectStoreKey::derive(workspace_root)?,
        })
    }

    /// Backing for an already-derived storage address.
    pub fn for_key(layout: StateLayout, key: ProjectStoreKey) -> Self {
        Self::Project { layout, key }
    }

    /// Path of the per-project `WorkspaceOperationLock`, when enabled.
    pub fn operation_lock_path(&self) -> Option<PathBuf> {
        match self {
            Self::Disabled => None,
            Self::Project { layout, key } => {
                Some(layout.project_lock(key, WORKSPACE_OPERATION_LOCK))
            }
        }
    }
}

/// One successfully installed Atomic Local Mutation, ready to record.
#[derive(Debug, Clone)]
pub struct MutationRecord {
    /// CEP method that installed the mutation (`mutate__create` / `mutate__edit`).
    pub method: String,
    /// Turn this mutation belongs to.
    pub turn_id: TurnId,
    /// CEP call id within the turn.
    pub invocation_id: InvocationId,
    /// Workspace-relative path of the mutated file.
    pub path: String,
    /// Pre-mutation bytes, when the mutation replaced existing content.
    ///
    /// `None` for `mutate__create`, whose before state is "absent".
    pub before: Option<Vec<u8>>,
    /// Bytes that were installed.
    pub after: Vec<u8>,
    /// Post-install Local Revision (content hash) of the target.
    pub content_hash: String,
    /// Unix seconds to stamp the history commit with.
    pub recorded_at: i64,
}

/// Result of recording one mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedMutation {
    /// The mutation commit `current` now points at.
    pub commit_id: String,
    /// The baseline commit written just before it, when one was needed.
    pub baseline_commit_id: Option<String>,
    /// The commit `current` pointed at before this mutation.
    pub parent_commit_id: Option<String>,
}

/// The LATTICE-mutated file state reachable from one history object id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistorySnapshot {
    /// The resolved commit.
    pub commit_id: String,
    /// Workspace-relative path -> file bytes, for every path LATTICE had
    /// mutated as of that commit.
    pub files: BTreeMap<String, Vec<u8>>,
}

/// The private, durable record of successfully installed Atomic Local
/// Mutations.
///
/// Deep module: store creation, object writing, ordering, ref movement, and
/// reachability all live behind this type. Callers hand it a
/// [`MutationRecord`] after a successful install and read back object ids.
pub struct WorkspaceHistory {
    /// `None` when recording is disabled.
    store: Option<StoreRepo>,
    /// Path of the per-project `WorkspaceOperationLock`. `None` when
    /// recording is disabled, which is also the only case with no project
    /// binding to lock against.
    operation_lock: Option<PathBuf>,
    /// One-shot record failure for tests of the post-install durability edge.
    #[cfg(any(test, feature = "test-fault-injection"))]
    fail_record: AtomicBool,
}

impl std::fmt::Debug for WorkspaceHistory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkspaceHistory")
            .field("path", &self.repo_path())
            .finish()
    }
}

impl WorkspaceHistory {
    /// Strictly open the history for `backing`.
    ///
    /// A missing or identity-invalid store fails closed (#143): opening never
    /// initializes. The store is first-created when a Turn creates a new
    /// Session (`lattice-api` `turn::execute`).
    pub fn open(backing: WorkspaceHistoryBacking) -> Result<Self, WorkspaceHistoryError> {
        match &backing {
            WorkspaceHistoryBacking::Disabled => Ok(Self::disabled()),
            WorkspaceHistoryBacking::Project { layout, key } => {
                let store = StoreRepo::open_project(
                    layout,
                    key,
                    HISTORY_REPO_DIR,
                    HISTORY_STORE_KIND,
                    HISTORY_STORE_SCHEMA_VERSION,
                )?;
                Ok(Self::from_store(store, backing.operation_lock_path()))
            }
        }
    }

    /// First-create (or validate) the history store for `backing`.
    ///
    /// The only initialization path, reserved for the composition entry point
    /// that creates a project's stores.
    pub fn create(backing: WorkspaceHistoryBacking) -> Result<Self, WorkspaceHistoryError> {
        match &backing {
            WorkspaceHistoryBacking::Disabled => Ok(Self::disabled()),
            WorkspaceHistoryBacking::Project { layout, key } => {
                let store = StoreRepo::create_project(
                    layout,
                    key,
                    HISTORY_REPO_DIR,
                    HISTORY_STORE_KIND,
                    HISTORY_STORE_SCHEMA_VERSION,
                    |_| Ok(()),
                )?;
                Ok(Self::from_store(store, backing.operation_lock_path()))
            }
        }
    }

    fn from_store(store: StoreRepo, operation_lock: Option<PathBuf>) -> Self {
        Self {
            store: Some(store),
            operation_lock,
            #[cfg(any(test, feature = "test-fault-injection"))]
            fail_record: AtomicBool::new(false),
        }
    }

    /// A history that records nothing.
    pub fn disabled() -> Self {
        Self {
            store: None,
            operation_lock: None,
            #[cfg(any(test, feature = "test-fault-injection"))]
            fail_record: AtomicBool::new(false),
        }
    }

    /// Path of the private store, when enabled.
    pub fn repo_path(&self) -> Option<&Path> {
        self.store.as_ref().map(|store| store.git_dir())
    }

    /// Acquire the per-project `WorkspaceOperationLock` (ADR 0005 §5).
    ///
    /// Observation takes it shared; mutations take it exclusively
    /// and hold it from the precondition read through install and history
    /// publication. It is strictly outside the repository write lock — a
    /// holder may take at most one repository write lock underneath it, and
    /// never the reverse — and the kernel's registry rejects an upgrade from
    /// a shared hold.
    ///
    /// Returns `Ok(None)` when recording is disabled: there is no project
    /// binding to serialize against.
    pub fn lock_workspace_operation(
        &self,
        mode: LockMode,
        blocking: Blocking,
    ) -> Result<Option<lattice_store::FlockGuard>, WorkspaceHistoryError> {
        let Some(path) = self.operation_lock.as_deref() else {
            return Ok(None);
        };
        Ok(Some(lattice_store::acquire_flock(path, mode, blocking)?))
    }

    /// The object id `current` points at, if any mutation has been recorded.
    pub fn current(&self) -> Result<Option<String>, WorkspaceHistoryError> {
        let Some(store) = self.store.as_ref() else {
            return Ok(None);
        };
        let tip = store.ref_target(CURRENT_REF)?;
        validate_history(store, tip)?;
        Ok(tip.map(|oid| oid.to_string()))
    }

    /// Record one successfully installed mutation.
    ///
    /// Returns `Ok(None)` when recording is disabled. Callers must only reach
    /// this after the atomic install succeeded: a recorded commit asserts the
    /// mutation is on disk.
    pub fn record(
        &self,
        record: &MutationRecord,
    ) -> Result<Option<RecordedMutation>, WorkspaceHistoryError> {
        let Some(store) = self.store.as_ref() else {
            return Ok(None);
        };
        #[cfg(any(test, feature = "test-fault-injection"))]
        if self.fail_record.swap(false, Ordering::SeqCst) {
            return Err(WorkspaceHistoryError::Repo(
                "injected history record failure".into(),
            ));
        }
        let path = normalize_history_path(&record.path)?;
        // The kernel's standard per-repository write lock covers the whole
        // read-build-publish sequence, across handles and processes; the
        // kernel releases it if the holder dies.
        let _write = store.write_lock()?;
        record_locked(store, &path, record).map(Some)
    }

    /// Fail the next enabled history append before any history object or ref
    /// changes. The Local Workspace install has already happened at this seam.
    #[cfg(any(test, feature = "test-fault-injection"))]
    pub fn fail_next_record(&self) {
        self.fail_record.store(true, Ordering::SeqCst);
    }

    /// Read back the LATTICE-mutated file state at `oid`.
    ///
    /// Returns `Ok(None)` when recording is disabled or the object is not a
    /// commit in this history.
    pub fn resolve(&self, oid: &str) -> Result<Option<HistorySnapshot>, WorkspaceHistoryError> {
        let Some(store) = self.store.as_ref() else {
            return Ok(None);
        };
        let object_id = parse_oid(oid)?;
        let Ok(tree) = store.commit_tree(object_id) else {
            return Ok(None);
        };
        Ok(Some(HistorySnapshot {
            commit_id: object_id.to_string(),
            files: store.tree_files(tree)?,
        }))
    }
}

/// 已校验的历史变更；仅供记录读取，不提供恢复规划。
#[derive(Debug, Clone, PartialEq, Eq)]
struct HistoryMutation {
    method: String,
    turn_id: TurnId,
    invocation_id: InvocationId,
    path: String,
    recorded_content_hash: String,
}

/// 固定 current 后核验完整不可变链；不持锁、不修改引用。
fn validate_history(
    store: &StoreRepo,
    tip: Option<gix::ObjectId>,
) -> Result<(), WorkspaceHistoryError> {
    let mut cursor = tip;
    let mut newer_was_baseline = false;
    while let Some(oid) = cursor {
        let parents = store.commit_parents(oid)?;
        if parents.len() > 1 {
            return Err(history_corruption(
                &oid,
                "history commits must have at most one parent",
            ));
        }
        let parent = parents.into_iter().next();
        let parsed = parse_history_commit(&oid, &store.commit_message(oid)?)?;
        let is_baseline = matches!(parsed, ParsedHistoryCommit::Baseline);
        if is_baseline && Some(oid) == tip {
            return Err(history_corruption(
                &oid,
                "history tip must be a mutation commit",
            ));
        }
        if newer_was_baseline && is_baseline {
            return Err(history_corruption(
                &oid,
                "baseline commits must be followed by exactly one mutation commit",
            ));
        }
        if let ParsedHistoryCommit::Mutation(step) = parsed {
            let parent_tree = parent.map(|p| commit_tree_id(store, p)).transpose()?;
            let before = tree_blob(store, parent_tree, &step.path)?;
            match (step.method.as_str(), before.is_some()) {
                ("mutate__create", true) => {
                    return Err(history_corruption(
                        &oid,
                        "mutate__create parent state must not contain the target path",
                    ));
                }
                ("mutate__edit", false) => {
                    return Err(history_corruption(
                        &oid,
                        "mutate__edit parent state must contain the target path",
                    ));
                }
                _ => {}
            }
            let after = tree_blob(store, Some(commit_tree_id(store, oid)?), &step.path)?
                .ok_or_else(|| {
                    history_corruption(&oid, "mutation tree must contain the target path")
                })?;
            if step.recorded_content_hash != crate::workspace::content_hash(&after) {
                return Err(history_corruption(
                    &oid,
                    "'lattice-content-hash' trailer does not match the mutation tree",
                ));
            }
        }
        newer_was_baseline = is_baseline;
        cursor = parent;
    }
    Ok(())
}

/// One recognized commit in the closed WorkspaceHistory chain.
enum ParsedHistoryCommit {
    /// Chain plumbing that captures external before-state drift.
    Baseline,
    /// One installed LATTICE mutation.
    Mutation(HistoryMutation),
}

fn history_corruption(oid: &gix::ObjectId, reason: impl Into<String>) -> WorkspaceHistoryError {
    WorkspaceHistoryError::Corruption {
        commit_id: oid.to_string(),
        reason: reason.into(),
    }
}

fn set_history_trailer(
    oid: &gix::ObjectId,
    key: &str,
    slot: &mut Option<String>,
    value: String,
) -> Result<(), WorkspaceHistoryError> {
    if slot.replace(value).is_some() {
        return Err(history_corruption(
            oid,
            format!("duplicate '{key}' trailer"),
        ));
    }
    Ok(())
}

fn required_history_trailer<'a>(
    oid: &gix::ObjectId,
    key: &'static str,
    value: &'a Option<String>,
) -> Result<&'a str, WorkspaceHistoryError> {
    value
        .as_deref()
        .ok_or_else(|| history_corruption(oid, format!("missing '{key}' trailer")))
}

fn is_canonical_content_hash(value: &str) -> bool {
    value.strip_prefix("blake3:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

/// Parse and validate one commit in the closed WorkspaceHistory schema.
fn parse_history_commit(
    oid: &gix::ObjectId,
    message: &str,
) -> Result<ParsedHistoryCommit, WorkspaceHistoryError> {
    let mut kind = None;
    let mut method = None;
    let mut turn_id = None;
    let mut invocation_id = None;
    let mut path = None;
    let mut content_hash = None;
    for line in message.lines() {
        let Some((raw_key, raw_value)) = line.split_once(':') else {
            continue;
        };
        let key = raw_key.trim();
        let (slot, value) = match key {
            KIND_TRAILER => (&mut kind, raw_value.trim().to_string()),
            "lattice-method" => (&mut method, raw_value.trim().to_string()),
            "lattice-turn-id" => (&mut turn_id, raw_value.trim().to_string()),
            "lattice-invocation-id" => (&mut invocation_id, raw_value.trim().to_string()),
            // The path must round-trip byte-exactly: a workspace-relative
            // path can legitimately begin or end with a space (`./ a.txt`
            // normalizes to ` a.txt`). Only the one separator space is removed.
            "lattice-path" => (
                &mut path,
                raw_value.strip_prefix(' ').unwrap_or(raw_value).to_string(),
            ),
            "lattice-content-hash" => (&mut content_hash, raw_value.trim().to_string()),
            _ => continue,
        };
        set_history_trailer(oid, key, slot, value)?;
    }

    match required_history_trailer(oid, KIND_TRAILER, &kind)? {
        KIND_BASELINE => return Ok(ParsedHistoryCommit::Baseline),
        KIND_MUTATION => {}
        other => {
            return Err(history_corruption(
                oid,
                format!("unknown '{KIND_TRAILER}' value '{other}'"),
            ));
        }
    }

    let method = required_history_trailer(oid, "lattice-method", &method)?.to_string();
    if !matches!(method.as_str(), "mutate__create" | "mutate__edit") {
        return Err(history_corruption(
            oid,
            format!("invalid 'lattice-method' trailer '{method}'"),
        ));
    }

    let turn_raw = required_history_trailer(oid, "lattice-turn-id", &turn_id)?;
    let turn_id = TurnId::parse(turn_raw).map_err(|err| {
        history_corruption(
            oid,
            format!("invalid 'lattice-turn-id' trailer '{turn_raw}': {err}"),
        )
    })?;

    let invocation_raw = required_history_trailer(oid, "lattice-invocation-id", &invocation_id)?;
    let invocation_id = invocation_raw.parse::<InvocationId>().map_err(|err| {
        history_corruption(
            oid,
            format!("invalid 'lattice-invocation-id' trailer '{invocation_raw}': {err}"),
        )
    })?;

    let path = required_history_trailer(oid, "lattice-path", &path)?.to_string();
    normalize_history_path(&path).map_err(|err| {
        history_corruption(
            oid,
            format!("invalid 'lattice-path' trailer '{path}': {err}"),
        )
    })?;

    let content_hash =
        required_history_trailer(oid, "lattice-content-hash", &content_hash)?.to_string();
    if !is_canonical_content_hash(&content_hash) {
        return Err(history_corruption(
            oid,
            format!("invalid 'lattice-content-hash' trailer '{content_hash}'"),
        ));
    }

    Ok(ParsedHistoryCommit::Mutation(HistoryMutation {
        method,
        turn_id,
        invocation_id,
        path,
        recorded_content_hash: content_hash,
    }))
}

// ─── Recording ──────────────────────────────────────────────────────────────

/// How many times a losing compare-and-set on `current` is retried.
///
/// The repository write lock already serializes LATTICE writers, so a loss
/// means a foreign writer touched the store. `current` only ever moves forward
/// by one commit per recorded mutation, so a loser just needs to rebuild on
/// the new tip; the bound exists so a pathological writer cannot spin forever.
const ADVANCE_RETRY_LIMIT: usize = 64;

fn record_locked(
    store: &StoreRepo,
    path: &str,
    record: &MutationRecord,
) -> Result<RecordedMutation, WorkspaceHistoryError> {
    // A loser must rebuild on the winner's tip rather than drop the mutation:
    // the bytes are already installed on disk, so failing to record would lose
    // history for a mutation that really happened.
    let mut last_conflict = None;
    for _ in 0..ADVANCE_RETRY_LIMIT {
        match try_record_once(store, path, record)? {
            RecordAttempt::Recorded(recorded) => return Ok(recorded),
            RecordAttempt::CurrentMoved(err) => last_conflict = Some(err),
        }
    }
    Err(WorkspaceHistoryError::Repo(format!(
        "workspace history current ref kept moving under {ADVANCE_RETRY_LIMIT} attempts: {}",
        last_conflict.unwrap_or_else(|| "unknown conflict".to_string())
    )))
}

/// Outcome of one attempt to append a mutation on top of the observed tip.
enum RecordAttempt {
    Recorded(RecordedMutation),
    /// Another writer advanced `current` between the read and the CAS.
    CurrentMoved(String),
}

fn try_record_once(
    store: &StoreRepo,
    path: &str,
    record: &MutationRecord,
) -> Result<RecordAttempt, WorkspaceHistoryError> {
    let parent = store.ref_target(CURRENT_REF)?;
    let parent_tree = match parent {
        Some(oid) => commit_tree_id_err(store, oid)?,
        None => store.empty_tree_oid(),
    };

    // Baseline: the mutation commit's parent tree must be the real before
    // state. That is only automatic while nothing outside LATTICE touches the
    // path — the user's editor, a `git checkout`, or an external delete can
    // leave the history holding LATTICE's stale after-state, and a first edit
    // of a pre-existing file has no entry at all. So compare the parent tree's
    // bytes against the before state the installer observed, and when they
    // differ record a baseline commit that makes the parent tree exact.
    // Absence is a state too: a create whose path is still present in the tree
    // (LATTICE created it, the user deleted it, LATTICE created it again)
    // baselines by *removing* the path.
    let recorded_before = store.read_tree_path(parent_tree, path)?;
    let needs_baseline = recorded_before.as_deref() != record.before.as_deref();
    let (effective_parent, effective_tree, baseline_commit_id) = if needs_baseline {
        let tree = match record.before.as_deref() {
            Some(before) => upsert_path(store, parent_tree, path, before)?,
            None => remove_path(store, parent_tree, path)?,
        };
        let message = baseline_message(record, path);
        let commit = write_commit(store, &message, tree, parent, record.recorded_at)?;
        (Some(commit), tree, Some(commit.to_string()))
    } else {
        (parent, parent_tree, None)
    };

    let tree = upsert_path(store, effective_tree, path, &record.after)?;
    let message = mutation_message(record, path);
    let commit = write_commit(store, &message, tree, effective_parent, record.recorded_at)?;

    // Everything appended here is an ancestor of the new `current`, so the
    // normal path needs no per-record refs: the CAS below is what publishes
    // it, and `cas_ref` already fsyncs the pending objects before publishing
    // and the ref after, so no second barrier follows a successful CAS.
    let expect = match parent {
        Some(oid) => RefExpectation::MustMatch(oid),
        None => RefExpectation::MustNotExist,
    };
    match store.cas_ref(
        CURRENT_REF,
        commit,
        expect,
        "lattice workspace history mutation",
    ) {
        Ok(()) => Ok(RecordAttempt::Recorded(RecordedMutation {
            commit_id: commit.to_string(),
            baseline_commit_id,
            parent_commit_id: parent.map(|oid| oid.to_string()),
        })),
        Err(StoreError::CasConflict { detail, .. }) => Ok(RecordAttempt::CurrentMoved(detail)),
        Err(err) => Err(err.into()),
    }
}

fn upsert_path(
    store: &StoreRepo,
    base_tree: gix::ObjectId,
    path: &str,
    content: &[u8],
) -> Result<gix::ObjectId, WorkspaceHistoryError> {
    let blob = store.write_blob(content)?;
    let mut spec = TreeSpec::from(base_tree);
    spec.upsert(path, EntryKind::Blob, blob);
    Ok(store.write_tree(&spec)?)
}

fn remove_path(
    store: &StoreRepo,
    base_tree: gix::ObjectId,
    path: &str,
) -> Result<gix::ObjectId, WorkspaceHistoryError> {
    let mut spec = TreeSpec::from(base_tree);
    spec.remove(path);
    Ok(store.write_tree(&spec)?)
}

fn write_commit(
    store: &StoreRepo,
    message: &str,
    tree: gix::ObjectId,
    parent: Option<gix::ObjectId>,
    recorded_at: i64,
) -> Result<gix::ObjectId, WorkspaceHistoryError> {
    let parents: Vec<gix::ObjectId> = parent.into_iter().collect();
    Ok(store.write_commit_at(tree, &parents, message, recorded_at)?)
}

// ─── Commit messages ────────────────────────────────────────────────────────

fn mutation_message(record: &MutationRecord, path: &str) -> String {
    format!(
        "{method} {path}\n\n\
         {KIND_TRAILER}: {KIND_MUTATION}\n\
         lattice-method: {method}\n\
         lattice-turn-id: {turn}\n\
         lattice-invocation-id: {invocation}\n\
         lattice-path: {path}\n\
         lattice-content-hash: {hash}\n",
        method = record.method,
        turn = record.turn_id,
        invocation = record.invocation_id,
        hash = record.content_hash,
    )
}

fn baseline_message(record: &MutationRecord, path: &str) -> String {
    format!(
        "baseline {path}\n\n\
         {KIND_TRAILER}: {KIND_BASELINE}\n\
         lattice-method: {method}\n\
         lattice-turn-id: {turn}\n\
         lattice-invocation-id: {invocation}\n\
         lattice-path: {path}\n",
        method = record.method,
        turn = record.turn_id,
        invocation = record.invocation_id,
    )
}

// ─── Store helpers ──────────────────────────────────────────────────────────

fn commit_tree_id_err(
    store: &StoreRepo,
    commit: gix::ObjectId,
) -> Result<gix::ObjectId, WorkspaceHistoryError> {
    Ok(store.commit_tree(commit)?)
}

fn commit_tree_id(
    store: &StoreRepo,
    commit: gix::ObjectId,
) -> Result<gix::ObjectId, WorkspaceHistoryError> {
    commit_tree_id_err(store, commit)
}

/// Bytes recorded at `path` in `tree`, or `None` when the tree or path is
/// absent.
fn tree_blob(
    store: &StoreRepo,
    tree: Option<gix::ObjectId>,
    path: &str,
) -> Result<Option<Vec<u8>>, WorkspaceHistoryError> {
    match tree {
        Some(oid) => Ok(store.read_tree_path(oid, path)?),
        None => Ok(None),
    }
}

fn parse_oid(hex: &str) -> Result<gix::ObjectId, WorkspaceHistoryError> {
    gix::ObjectId::from_hex(hex.as_bytes()).map_err(|e| WorkspaceHistoryError::ObjectId {
        oid: hex.to_string(),
        reason: e.to_string(),
    })
}

/// Validate a mutation path as a workspace-relative file path.
///
/// The caller's path already went through Local Workspace resolution; this is
/// a fail-closed second check so nothing can smuggle an absolute path, a
/// parent escape, or a `.git`-administration path into the private history.
fn normalize_history_path(path: &str) -> Result<String, WorkspaceHistoryError> {
    let reject = |reason: &'static str| WorkspaceHistoryError::Path {
        path: path.to_string(),
        reason,
    };
    if path.is_empty() {
        return Err(reject("path must not be empty"));
    }
    if path.starts_with('/') || path.contains('\\') {
        return Err(reject("path must be workspace-relative"));
    }
    if path.contains('\0') || path.contains('\n') {
        return Err(reject("path must not contain control characters"));
    }
    let mut components = Vec::new();
    for component in path.split('/') {
        match component {
            "" | "." => return Err(reject("path must be normalized")),
            ".." => return Err(reject("path must not escape the workspace")),
            ".git" => return Err(reject("project Git administration is never recorded")),
            other => components.push(other),
        }
    }
    Ok(components.join("/"))
}

// The `WorkspaceOperationLock` lives on the handle
// ([`WorkspaceHistory::lock_workspace_operation`]) so callers cannot address
// a project's lock without the project binding it belongs to.

#[cfg(test)]
mod tests {
    fn test_invocation_id(ordinal: u64) -> InvocationId {
        InvocationId::parse(&format!("00000000-0000-4000-8000-{ordinal:012x}"))
            .expect("invocation id")
    }

    fn test_turn_id(ordinal: u64) -> TurnId {
        TurnId::parse(&format!("10000000-0000-4000-8000-{ordinal:012x}")).expect("turn id")
    }

    use super::*;
    use gix::refs::transaction::PreviousValue;
    use tempfile::TempDir;

    /// A history store first-created under `dir` as the state root, bound to
    /// a fixed workspace root. Tests own both roots, so the derived
    /// `ProjectStoreKey` is stable within one test.
    fn history_at(dir: &TempDir) -> WorkspaceHistory {
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("create workspace root");
        WorkspaceHistory::create(
            WorkspaceHistoryBacking::for_project_in(&dir.path().join("state"), &workspace)
                .expect("derive backing"),
        )
        .expect("create history")
    }

    /// Strictly reopen the same store a `history_at` fixture created.
    fn reopen_history_at(dir: &TempDir) -> WorkspaceHistory {
        WorkspaceHistory::open(
            WorkspaceHistoryBacking::for_project_in(
                &dir.path().join("state"),
                &dir.path().join("workspace"),
            )
            .expect("derive backing"),
        )
        .expect("open history")
    }

    fn create(path: &str, after: &str) -> MutationRecord {
        MutationRecord {
            method: "mutate__create".into(),
            turn_id: test_turn_id(1),
            invocation_id: test_invocation_id(1),
            path: path.into(),
            before: None,
            after: after.as_bytes().to_vec(),
            content_hash: crate::workspace::content_hash(after.as_bytes()),
            recorded_at: 1_700_000_000,
        }
    }

    fn edit(path: &str, before: &str, after: &str) -> MutationRecord {
        MutationRecord {
            method: "mutate__edit".into(),
            turn_id: test_turn_id(1),
            invocation_id: test_invocation_id(2),
            path: path.into(),
            before: Some(before.as_bytes().to_vec()),
            after: after.as_bytes().to_vec(),
            content_hash: crate::workspace::content_hash(after.as_bytes()),
            recorded_at: 1_700_000_001,
        }
    }

    fn parse_corruption_reason(message: &str) -> String {
        let oid = parse_oid(&"f".repeat(40)).expect("object id");
        match parse_history_commit(&oid, message) {
            Err(WorkspaceHistoryError::Corruption { commit_id, reason }) => {
                assert_eq!(commit_id, oid.to_string());
                reason
            }
            Err(other) => panic!("expected history corruption, got {other:?}"),
            Ok(_) => panic!("expected malformed history commit to fail closed"),
        }
    }

    #[test]
    fn mutation_message_emits_canonical_turn_id_trailer() {
        let message = mutation_message(&create("a.txt", "x"), "a.txt");
        assert!(
            message
                .lines()
                .any(|line| line == "lattice-turn-id: 10000000-0000-4000-8000-000000000001")
        );
    }

    #[test]
    fn mutation_parser_rejects_invalid_required_trailers() {
        enum Edit<'a> {
            Remove,
            Duplicate,
            Replace(&'a str),
        }
        let canonical = mutation_message(&create("a.txt", "x"), "a.txt");
        let oid = parse_oid(&"f".repeat(40)).expect("object id");
        for (key, value) in [
            ("lattice-method", "fetch__web"),
            ("lattice-turn-id", "turn-1"),
            ("lattice-invocation-id", "invocation-1"),
            ("lattice-path", "../escape.txt"),
            ("lattice-content-hash", "sha256:not-a-local-revision"),
        ] {
            for (kind, edit, reason_kind, exact) in [
                ("missing", Edit::Remove, "missing", true),
                ("duplicate", Edit::Duplicate, "duplicate", true),
                ("malformed", Edit::Replace(value), "invalid", false),
            ] {
                let name = format!("mutation_parser_rejects_{kind}_required_trailers/{key}");
                let prefix = format!("{key}:");
                let message = match edit {
                    Edit::Remove => canonical
                        .lines()
                        .filter(|line| !line.starts_with(&prefix))
                        .collect::<Vec<_>>()
                        .join("\n"),
                    Edit::Duplicate => {
                        let line = canonical
                            .lines()
                            .find(|line| line.starts_with(&prefix))
                            .unwrap_or_else(|| panic!("{name}: canonical trailer missing"));
                        format!("{canonical}{line}\n")
                    }
                    Edit::Replace(value) => canonical
                        .lines()
                        .map(|line| {
                            if line.starts_with(&prefix) {
                                format!("{key}: {value}")
                            } else {
                                line.to_string()
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                };
                let reason = match parse_history_commit(&oid, &message) {
                    Err(WorkspaceHistoryError::Corruption { commit_id, reason }) => {
                        assert_eq!(commit_id, oid.to_string(), "{name}");
                        reason
                    }
                    Err(other) => panic!("{name}: expected history corruption, got {other:?}"),
                    Ok(_) => panic!("{name}: expected malformed history commit to fail closed"),
                };
                let expected = format!("{reason_kind} '{key}' trailer");
                if exact {
                    assert_eq!(reason, expected, "{name}");
                } else {
                    assert!(reason.starts_with(&expected), "{name}");
                }
            }
        }
    }

    #[test]
    fn history_parser_recognizes_baselines_and_rejects_unknown_kinds() {
        let record = edit("a.txt", "old", "new");
        let baseline = baseline_message(&record, "a.txt");
        let oid = parse_oid(&"f".repeat(40)).expect("object id");
        assert!(matches!(
            parse_history_commit(&oid, &baseline),
            Ok(ParsedHistoryCommit::Baseline)
        ));

        let unknown = mutation_message(&record, "a.txt").replace(
            "lattice-history-kind: mutation",
            "lattice-history-kind: future",
        );
        assert_eq!(
            parse_corruption_reason(&unknown),
            "unknown 'lattice-history-kind' value 'future'"
        );
    }

    #[test]
    fn ordered_mutations_chain_parents_in_execution_order() {
        let state = TempDir::new().unwrap();
        let history = history_at(&state);

        let first = history.record(&create("a.txt", "1")).unwrap().unwrap();
        let second = history.record(&create("b.txt", "2")).unwrap().unwrap();
        let third = history.record(&edit("a.txt", "1", "3")).unwrap().unwrap();

        assert_eq!(
            second.parent_commit_id.as_deref(),
            Some(first.commit_id.as_str())
        );
        assert_eq!(
            third.parent_commit_id.as_deref(),
            Some(second.commit_id.as_str())
        );
        // `a.txt` was already in the history, so no baseline was needed.
        assert_eq!(third.baseline_commit_id, None);

        let snapshot = history.resolve(&third.commit_id).unwrap().unwrap();
        assert_eq!(snapshot.files.get("a.txt").unwrap().as_slice(), b"3");
        assert_eq!(snapshot.files.get("b.txt").unwrap().as_slice(), b"2");
    }

    #[test]
    fn first_edit_of_an_untouched_file_records_its_before_state() {
        let state = TempDir::new().unwrap();
        let history = history_at(&state);

        let recorded = history
            .record(&edit("existing.txt", "old", "new"))
            .unwrap()
            .unwrap();
        let baseline = recorded
            .baseline_commit_id
            .clone()
            .expect("baseline commit");

        let before = history.resolve(&baseline).unwrap().unwrap();
        assert_eq!(before.files.get("existing.txt").unwrap().as_slice(), b"old");
        let after = history.resolve(&recorded.commit_id).unwrap().unwrap();
        assert_eq!(after.files.get("existing.txt").unwrap().as_slice(), b"new");
        // The baseline stays reachable from the published `current` ref: the
        // recorded commit is the tip and its parent is the baseline.
        assert_eq!(
            history.current().unwrap().as_deref(),
            Some(recorded.commit_id.as_str())
        );
        assert_eq!(
            history
                .store
                .as_ref()
                .unwrap()
                .commit_parents(parse_oid(&recorded.commit_id).unwrap())
                .unwrap(),
            vec![parse_oid(&baseline).unwrap()]
        );
    }

    /// 外部修改后的真实前态必须记录，不能沿用上次变更的陈旧后态。
    #[test]
    fn external_drift_between_mutations_still_records_the_real_before_state() {
        let state = TempDir::new().unwrap();
        let history = history_at(&state);

        history.record(&create("drift.txt", "v1")).unwrap().unwrap();
        // The user edits the file outside LATTICE; the next LATTICE edit sees
        // "v2-external" as its before state, not the recorded "v1".
        let recorded = history
            .record(&edit("drift.txt", "v2-external", "v3"))
            .unwrap()
            .unwrap();

        let baseline = recorded
            .baseline_commit_id
            .clone()
            .expect("drift must force a baseline commit");
        let before = history.resolve(&baseline).unwrap().unwrap();
        assert_eq!(
            before.files.get("drift.txt").unwrap().as_slice(),
            b"v2-external",
            "the parent tree of a mutation must be the state the installer observed"
        );
        let after = history.resolve(&recorded.commit_id).unwrap().unwrap();
        assert_eq!(after.files.get("drift.txt").unwrap().as_slice(), b"v3");
    }

    /// A create whose target was deleted outside LATTICE has "absent" as its
    /// before state, so the baseline removes the stale entry.
    #[test]
    fn create_after_an_external_delete_baselines_the_path_as_absent() {
        let state = TempDir::new().unwrap();
        let history = history_at(&state);

        history
            .record(&create("gone.txt", "first"))
            .unwrap()
            .unwrap();
        // Deleted outside LATTICE, then created again.
        let recorded = history
            .record(&create("gone.txt", "second"))
            .unwrap()
            .unwrap();

        let baseline = recorded
            .baseline_commit_id
            .clone()
            .expect("a stale entry must be baselined away");
        let before = history.resolve(&baseline).unwrap().unwrap();
        assert!(
            !before.files.contains_key("gone.txt"),
            "the before state of a create is absence: {:?}",
            before.files.keys().collect::<Vec<_>>()
        );
        let after = history.resolve(&recorded.commit_id).unwrap().unwrap();
        assert_eq!(after.files.get("gone.txt").unwrap().as_slice(), b"second");
    }

    /// Two independent handles over one private repository model two LATTICE
    /// processes on the same project. Every successfully installed mutation
    /// must end up in the chain: the loser of the compare-and-set rebuilds on
    /// the winner's tip instead of dropping bytes that are already on disk.
    #[test]
    fn concurrent_handles_record_every_mutation() {
        let state = TempDir::new().unwrap();
        let handles: Vec<WorkspaceHistory> = (0..4).map(|_| history_at(&state)).collect();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(handles.len()));
        const PER_HANDLE: usize = 12;

        std::thread::scope(|scope| {
            for (worker, history) in handles.iter().enumerate() {
                let barrier = std::sync::Arc::clone(&barrier);
                scope.spawn(move || {
                    barrier.wait();
                    for index in 0..PER_HANDLE {
                        history
                            .record(&create(&format!("w{worker}/f{index}.txt"), "x"))
                            .expect("recording must not fail under contention")
                            .expect("history is enabled");
                    }
                });
            }
        });

        let tip = handles[0].current().unwrap().expect("recorded");
        let snapshot = handles[0].resolve(&tip).unwrap().unwrap();
        let expected = handles.len() * PER_HANDLE;
        assert_eq!(
            snapshot.files.len(),
            expected,
            "every installed mutation must be in the tip tree, got {:?}",
            snapshot.files.keys().collect::<Vec<_>>()
        );
        for worker in 0..handles.len() {
            for index in 0..PER_HANDLE {
                assert!(
                    snapshot
                        .files
                        .contains_key(&format!("w{worker}/f{index}.txt")),
                    "w{worker}/f{index}.txt is missing from the history"
                );
            }
        }
    }

    #[test]
    fn prior_object_ids_stay_reachable_through_internal_refs() {
        let state = TempDir::new().unwrap();
        let history = history_at(&state);
        let first = history.record(&create("a.txt", "1")).unwrap().unwrap();
        let second = history.record(&create("b.txt", "2")).unwrap().unwrap();

        assert_eq!(
            history
                .store
                .as_ref()
                .unwrap()
                .commit_parents(parse_oid(&second.commit_id).unwrap())
                .unwrap(),
            vec![parse_oid(&first.commit_id).unwrap()]
        );
        assert_eq!(
            history.current().unwrap().as_deref(),
            Some(second.commit_id.as_str())
        );
        assert!(history.resolve(&first.commit_id).unwrap().is_some());
    }

    #[test]
    fn a_reopened_history_resolves_prior_object_ids() {
        let state = TempDir::new().unwrap();
        let recorded = {
            let history = history_at(&state);
            history.record(&create("a.txt", "1")).unwrap().unwrap()
        };
        // A fresh handle stands in for a process restart: strict open, the
        // production path, never a second create.
        let reopened = reopen_history_at(&state);
        assert_eq!(
            reopened.current().unwrap().as_deref(),
            Some(recorded.commit_id.as_str())
        );
        let snapshot = reopened.resolve(&recorded.commit_id).unwrap().unwrap();
        assert_eq!(snapshot.files.get("a.txt").unwrap().as_slice(), b"1");
        assert_eq!(
            reopened
                .store
                .as_ref()
                .unwrap()
                .ref_target(CURRENT_REF)
                .unwrap(),
            Some(parse_oid(&recorded.commit_id).unwrap())
        );
    }

    // ─── 历史记录真实性 ───────────────────────────────────────────────

    /// Open the private repository directly, bypassing the handle (and its
    /// locks) — the unit-test stand-in for an out-of-band actor.
    fn raw_repo(history: &WorkspaceHistory) -> gix::Repository {
        gix::open_opts(
            history.repo_path().expect("enabled"),
            gix::open::Options::isolated(),
        )
        .expect("open private repo")
    }

    fn assert_history_corruption(error: WorkspaceHistoryError, expected: &str) {
        match error {
            WorkspaceHistoryError::Corruption { reason, .. } => {
                assert!(
                    reason.contains(expected),
                    "expected corruption containing '{expected}', got '{reason}'"
                );
            }
            other => panic!("expected history corruption, got {other:?}"),
        }
    }

    #[test]
    fn current_rejects_a_forged_merge_commit() {
        let state = TempDir::new().unwrap();
        let history = history_at(&state);
        let first = history.record(&create("a.txt", "1")).unwrap().unwrap();
        let second = history.record(&create("b.txt", "2")).unwrap().unwrap();
        let repo = raw_repo(&history);
        let tree = repo
            .find_commit(parse_oid(&second.commit_id).unwrap())
            .unwrap()
            .tree_id()
            .unwrap()
            .detach();
        let signature = gix::actor::Signature {
            name: "LATTICE".into(),
            email: "lattice@localhost".into(),
            time: gix::date::Time::new(1_700_000_010, 0),
        };
        let mut time_buf = gix::date::parse::TimeBuf::default();
        let signature_ref = signature.to_ref(&mut time_buf);
        let merge = repo
            .new_commit_as(
                signature_ref,
                signature_ref,
                mutation_message(&create("c.txt", "3"), "c.txt"),
                tree,
                [
                    parse_oid(&second.commit_id).unwrap(),
                    parse_oid(&first.commit_id).unwrap(),
                ],
            )
            .unwrap()
            .id;
        repo.reference(CURRENT_REF, merge, PreviousValue::Any, "test forged merge")
            .unwrap();

        assert_history_corruption(history.current().unwrap_err(), "at most one parent");
    }

    #[test]
    fn current_rejects_baseline_tip() {
        let state = TempDir::new().unwrap();
        let history = history_at(&state);
        let recorded = history
            .record(&edit("existing.txt", "old", "new"))
            .unwrap()
            .unwrap();
        let baseline = recorded.baseline_commit_id.expect("baseline commit");

        let repo = raw_repo(&history);
        repo.reference(
            CURRENT_REF,
            parse_oid(&baseline).unwrap(),
            PreviousValue::Any,
            "test forged baseline tip",
        )
        .unwrap();
        assert_history_corruption(
            history.current().unwrap_err(),
            "history tip must be a mutation commit",
        );
    }

    #[test]
    fn current_rejects_forged_root_edit_and_mismatched_content_hash() {
        let state = TempDir::new().unwrap();
        let history = history_at(&state);
        let record = edit("existing.txt", "old", "new");
        let recorded = history.record(&record).unwrap().unwrap();
        let baseline = recorded.baseline_commit_id.expect("baseline commit");
        let repo = raw_repo(&history);
        let tree = repo
            .find_commit(parse_oid(&recorded.commit_id).unwrap())
            .unwrap()
            .tree_id()
            .unwrap()
            .detach();
        let signature = gix::actor::Signature {
            name: "LATTICE".into(),
            email: "lattice@localhost".into(),
            time: gix::date::Time::new(1_700_000_010, 0),
        };
        let mut time_buf = gix::date::parse::TimeBuf::default();
        let signature_ref = signature.to_ref(&mut time_buf);

        let root_edit = repo
            .new_commit_as(
                signature_ref,
                signature_ref,
                mutation_message(&record, "existing.txt"),
                tree,
                std::iter::empty::<gix::ObjectId>(),
            )
            .unwrap()
            .id;
        repo.reference(
            CURRENT_REF,
            root_edit,
            PreviousValue::Any,
            "test forged root edit",
        )
        .unwrap();
        assert_history_corruption(
            history.current().unwrap_err(),
            "mutate__edit parent state must contain the target path",
        );

        let wrong_hash = mutation_message(&record, "existing.txt")
            .replace(&record.content_hash, &format!("blake3:{}", "0".repeat(64)));
        let bad_hash = repo
            .new_commit_as(
                signature_ref,
                signature_ref,
                wrong_hash,
                tree,
                [parse_oid(&baseline).unwrap()],
            )
            .unwrap()
            .id;
        repo.reference(
            CURRENT_REF,
            bad_hash,
            PreviousValue::Any,
            "test forged content hash",
        )
        .unwrap();
        assert_history_corruption(
            history.current().unwrap_err(),
            "does not match the mutation tree",
        );
    }

    #[test]
    fn recorded_mutation_preserves_call_identity() {
        let state = TempDir::new().unwrap();
        let history = history_at(&state);
        let record = create("a.txt", "a1");
        let saved = history.record(&record).unwrap().unwrap();
        let store = history.store.as_ref().unwrap();
        let oid = parse_oid(&saved.commit_id).unwrap();
        let ParsedHistoryCommit::Mutation(parsed) =
            parse_history_commit(&oid, &store.commit_message(oid).unwrap()).unwrap()
        else {
            panic!("mutation");
        };
        assert_eq!(parsed.turn_id, test_turn_id(1));
        assert_eq!(parsed.invocation_id, test_invocation_id(1));
        assert_eq!(parsed.method, "mutate__create");
        assert_eq!(parsed.path, "a.txt");
        assert_eq!(parsed.recorded_content_hash, record.content_hash);
    }

    #[test]
    fn a_path_with_leading_whitespace_round_trips() {
        let state = TempDir::new().unwrap();
        let history = history_at(&state);
        let saved = history
            .record(&create(" spaced.txt", "s1"))
            .unwrap()
            .unwrap();
        assert_eq!(history.current().unwrap(), Some(saved.commit_id.clone()));
        let store = history.store.as_ref().unwrap();
        let oid = parse_oid(&saved.commit_id).unwrap();
        let ParsedHistoryCommit::Mutation(parsed) =
            parse_history_commit(&oid, &store.commit_message(oid).unwrap()).unwrap()
        else {
            panic!("mutation");
        };
        assert_eq!(parsed.path, " spaced.txt");
        assert_eq!(
            history.resolve(&saved.commit_id).unwrap().unwrap().files[" spaced.txt"],
            b"s1"
        );
    }

    /// 未知历史类型拒绝读取，不移动 current 或修复原始记录。
    #[test]
    fn a_forged_commit_without_a_history_kind_fails_closed() {
        let state = TempDir::new().unwrap();
        let history = history_at(&state);
        history.record(&create("a.txt", "a")).unwrap().unwrap();
        let hidden = history.record(&create("b.txt", "b")).unwrap().unwrap();

        // Rewrite the newest commit with its mutation trailer stripped, as a
        // stand-in for any commit the walker cannot recognise, and point
        // `current` at the rewritten commit.
        let repo = raw_repo(&history);
        let commit = repo
            .find_commit(parse_oid(&hidden.commit_id).unwrap())
            .unwrap();
        let tree = commit.tree_id().unwrap().detach();
        let parent = commit.parent_ids().next().map(|id| id.detach());
        let signature = gix::actor::Signature {
            name: "LATTICE".into(),
            email: "lattice@localhost".into(),
            time: gix::date::Time::new(1_700_000_010, 0),
        };
        let mut time_buf = gix::date::parse::TimeBuf::default();
        let signature_ref = signature.to_ref(&mut time_buf);
        let opaque = repo
            .new_commit_as(signature_ref, signature_ref, "opaque\n", tree, parent)
            .unwrap()
            .id;
        repo.reference(CURRENT_REF, opaque, PreviousValue::Any, "test rewrite")
            .unwrap();

        assert_history_corruption(
            history.current().unwrap_err(),
            "missing 'lattice-history-kind' trailer",
        );
        assert_eq!(
            repo.find_reference(CURRENT_REF)
                .unwrap()
                .target()
                .try_id()
                .unwrap()
                .to_string(),
            opaque.to_string(),
            "a refused read must not move the ref"
        );
    }

    #[test]
    fn escaping_paths_are_rejected_before_any_object_is_written() {
        let state = TempDir::new().unwrap();
        let history = history_at(&state);
        for bad in [
            "",
            "/abs.txt",
            "../escape.txt",
            "a//b.txt",
            "./a.txt",
            ".git/config",
        ] {
            assert!(
                matches!(
                    history.record(&create(bad, "x")),
                    Err(WorkspaceHistoryError::Path { .. })
                ),
                "path '{bad}' must be rejected"
            );
        }
        assert_eq!(
            history.current().unwrap(),
            None,
            "a rejected path must not move the current ref"
        );
    }

    // ─── Operation lock (ADR 0005 §5, #143) ─────────────────────────────

    /// The store is a kernel store with the domain's recorded identity, not a
    /// hand-rolled bare repository.
    #[test]
    fn history_is_a_kernel_store_with_the_domain_identity() {
        let state = TempDir::new().unwrap();
        let history = history_at(&state);
        let path = history.repo_path().expect("enabled").to_path_buf();
        assert!(path.ends_with(HISTORY_REPO_DIR));

        let repo = raw_repo(&history);
        let meta = repo
            .find_reference("refs/lattice/meta")
            .expect("kernel identity ref exists");
        assert!(meta.target().try_id().is_some());
    }

    /// Strict open: an absent store fails closed instead of initializing one.
    #[test]
    fn opening_an_absent_store_fails_closed() {
        let state = TempDir::new().unwrap();
        let workspace = state.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let backing =
            WorkspaceHistoryBacking::for_project_in(&state.path().join("state"), &workspace)
                .expect("derive backing");
        let WorkspaceHistoryBacking::Project { layout, key } = &backing else {
            panic!("enabled backing")
        };
        let history_path = layout.project_store(key, HISTORY_REPO_DIR);

        let err = WorkspaceHistory::open(backing).expect_err("absent store must fail");
        assert!(
            matches!(
                err,
                WorkspaceHistoryError::Kernel(StoreError::NotInitialized { .. })
            ),
            "expected typed NotInitialized, got {err:?}"
        );
        assert!(
            !history_path.exists(),
            "strict open must not create the store"
        );
    }

    /// The operation lock is the per-project lock under the state layout, and
    /// history recording may take the narrower repository write lock beneath it.
    #[test]
    fn operation_lock_is_project_scoped_and_allows_history_recording() {
        let state = TempDir::new().unwrap();
        let history = history_at(&state);

        let guard = history
            .lock_workspace_operation(LockMode::Exclusive, Blocking::Block)
            .expect("acquire")
            .expect("enabled history has a lock");
        assert!(
            guard.path().starts_with(state.path().join("state")),
            "lock must live under the state layout: {}",
            guard.path().display()
        );
        assert!(guard.path().ends_with(WORKSPACE_OPERATION_LOCK));
        // The repository write lock is a *different*, narrower lock taken
        // underneath — recording while the operation lock is held works.
        history.record(&create("a.txt", "1")).unwrap().unwrap();
        drop(guard);
    }

    /// An independent exclusive operation over a shared hold is busy, so an
    /// observation can never silently become a mutation.
    #[test]
    fn operation_lock_upgrade_is_rejected() {
        let state = TempDir::new().unwrap();
        let history = history_at(&state);

        let _shared = history
            .lock_workspace_operation(LockMode::Shared, Blocking::Block)
            .expect("shared acquire");
        let err = history
            .lock_workspace_operation(LockMode::Exclusive, Blocking::NonBlocking)
            .expect_err("independent exclusive acquire must be busy");
        assert!(
            matches!(err, WorkspaceHistoryError::Kernel(StoreError::Busy { .. })),
            "expected typed Busy, got {err:?}"
        );
    }
}

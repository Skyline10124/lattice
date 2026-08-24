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
//!   refs/lattice/retained/<oid>                 -> kernel retention pin
//! ```
//!
//! `current` moves forward on every recorded mutation. Normal recording writes
//! no per-record refs: everything it appends is an ancestor of `current`. An
//! explicit Rewind (GitHub #64) moves `current` *backwards*, so it retains the
//! abandoned tip through the kernel's retention primitive first — that is what
//! keeps prior object ids resolvable forever, with no time-based expiry,
//! compensation commit, or reflog dependency. The store is private state; the
//! user's project refs are never touched.
//!
//! ## Before/after state
//!
//! For every mutation commit `N`, commit `N-1`'s tree is the *before* state
//! and commit `N`'s tree is the *after* state. The recorder enforces that
//! rather than assuming it: the Local Workspace is shared with the user, so
//! the history's idea of a path can be stale (the user edited the file in
//! their editor, a `git checkout` swapped branch contents, the file was
//! deleted outside LATTICE) and a path can be missing entirely the first time
//! `mutate.edit` touches a pre-existing file.
//!
//! Before appending, the recorder compares the parent tree's bytes at the path
//! against the before state the installer actually observed. When they differ
//! it writes a preceding **baseline** commit that makes the parent tree exact
//! — setting the path to the observed pre-mutation bytes, or *removing* it
//! when the real before state was absence (a `mutate.create` after an external
//! delete). A baseline commit is not a mutation commit: it is labelled
//! `lattice-history-kind: baseline`, and `current` still advances exactly once
//! per recorded mutation.
//!
//! ## Concurrency
//!
//! Recording and rewinding hold the kernel's standard per-repository write
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
use lancet_protocol::InvocationId;

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
pub const HISTORY_STORE_SCHEMA_VERSION: u32 = 2;

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
    /// For unit-test servers and embedders that have no per-project state
    /// root. Production turn runtimes always resolve a [`Self::Project`].
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

    /// Project-scoped backing under the XDG state root (production).
    pub fn for_project(workspace_root: &Path) -> Result<Self, WorkspaceHistoryError> {
        Ok(Self::Project {
            layout: StateLayout::resolve()?,
            key: ProjectStoreKey::derive(workspace_root)?,
        })
    }

    /// Backing for an already-derived storage address.
    pub fn for_key(layout: StateLayout, key: ProjectStoreKey) -> Self {
        Self::Project { layout, key }
    }

    /// Path of the history store, when enabled.
    pub fn store_path(&self) -> Option<PathBuf> {
        match self {
            Self::Disabled => None,
            Self::Project { layout, key } => Some(layout.project_store(key, HISTORY_REPO_DIR)),
        }
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
    /// CEP method that installed the mutation (`mutate.create` / `mutate.edit`).
    pub method: String,
    /// Turn this mutation belongs to.
    pub turn_id: String,
    /// CEP call id within the turn.
    pub invocation_id: InvocationId,
    /// Workspace-relative path of the mutated file.
    pub path: String,
    /// Pre-mutation bytes, when the mutation replaced existing content.
    ///
    /// `None` for `mutate.create`, whose before state is "absent".
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
    /// initializes. The store is first-created by the `session new`
    /// composition path.
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

    /// Whether mutations are recorded.
    pub fn is_enabled(&self) -> bool {
        self.store.is_some()
    }

    /// Path of the private store, when enabled.
    pub fn repo_path(&self) -> Option<&Path> {
        self.store.as_ref().map(|store| store.git_dir())
    }

    /// Acquire the per-project `WorkspaceOperationLock` (ADR 0005 §5).
    ///
    /// Observation takes it shared; mutation and Rewind take it exclusively
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
        Ok(store.ref_target(CURRENT_REF)?.map(|oid| oid.to_string()))
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

    /// Whether `oid` is still reachable in this history.
    ///
    /// Reachable means an ancestor of `current` — the normal case, which needs
    /// no per-record refs — or pinned by the kernel's retention primitive,
    /// which is how a Rewind keeps the tip it abandoned resolvable. It never
    /// depends on `current` still pointing at or past the object.
    pub fn is_reachable(&self, oid: &str) -> Result<bool, WorkspaceHistoryError> {
        let Some(store) = self.store.as_ref() else {
            return Ok(false);
        };
        let object_id = parse_oid(oid)?;
        // Tips that keep history alive: the live `current`, plus every
        // retention pin a Rewind left on a tip it abandoned. An object is
        // reachable when it is one of those tips or an ancestor of one.
        let mut tips = store.retained_pins()?;
        if let Some(current) = store.ref_target(CURRENT_REF)? {
            tips.push(current);
        }
        if tips.contains(&object_id) {
            return Ok(true);
        }
        if tips.is_empty() {
            return Ok(false);
        }
        Ok(store.rev_walk(&tips, &[])?.contains(&object_id))
    }
}

// ─── Rewind (GitHub #64) ────────────────────────────────────────────────────

/// One recorded mutation that an explicit Rewind will reverse.
///
/// Steps are ordered newest-first — the reverse of real execution order —
/// because that is the order a Rewind conceptually unwinds them. All fields
/// come from the mutation commit's message trailers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewindStep {
    /// The mutation commit being reversed.
    pub commit_id: String,
    /// CEP method that installed the mutation.
    pub method: String,
    /// Turn the mutation belonged to.
    pub turn_id: String,
    /// CEP call id within the turn, when the trailer parses.
    pub invocation_id: InvocationId,
    /// Workspace-relative path the mutation touched.
    pub path: String,
    /// Post-install Local Revision recorded for the mutation.
    pub recorded_content_hash: Option<String>,
    /// When the mutation was recorded, in unix seconds.
    ///
    /// Written by the recorder from the installer's clock, so it orders
    /// mutations against Turn timestamps from the same machine.
    pub recorded_at: Option<i64>,
}

/// The final on-disk state one path must reach for a Rewind to complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewindPathRestore {
    /// Workspace-relative path.
    pub path: String,
    /// Bytes recorded for the path at the current tip, or `None` when the
    /// history records it as absent there.
    pub expected: Option<Vec<u8>>,
    /// Bytes the path must hold after the Rewind, or `None` to delete it.
    pub restore: Option<Vec<u8>>,
}

/// A complete Rewind: which mutations are reversed and what each path becomes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewindPlan {
    /// The history object id the Rewind starts from.
    pub from: Option<String>,
    /// The history object id the Rewind lands on.
    pub to: Option<String>,
    /// Reversed mutations, newest-first.
    pub steps: Vec<RewindStep>,
    /// Final per-path restores, sorted by path.
    pub restores: Vec<RewindPathRestore>,
}

impl RewindPlan {
    /// Whether the plan reverses nothing.
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty() && self.restores.is_empty()
    }
}

/// Failure planning or applying a Rewind.
#[derive(Debug, thiserror::Error)]
pub enum RewindError<E> {
    /// The private history could not be read or written.
    #[error(transparent)]
    History(#[from] WorkspaceHistoryError),
    /// Recording is disabled, so there is nothing to rewind.
    #[error("workspace history is disabled")]
    Disabled,
    /// The requested target is not on the recorded chain.
    #[error("rewind target '{target}' is not reachable from the current history tip")]
    TargetUnreachable {
        /// The requested target object id.
        target: String,
    },
    /// The plan does not cover every path whose recorded state differs.
    #[error("rewind plan does not cover changed paths: {}", paths.join(", "))]
    PlanIncomplete {
        /// Paths the plan failed to cover.
        paths: Vec<String>,
    },
    /// The apply closure refused the plan; no ref was moved.
    #[error("rewind aborted before any ref moved")]
    Apply(#[source] E),
    /// Every path was restored but the history ref did not move durably.
    ///
    /// This is a failure: callers must never report the Rewind as successful.
    #[error("workspace restored but the history ref move failed: {0}")]
    RefMove(String),
}

impl WorkspaceHistory {
    /// Plan — without applying — the Rewind from `current` back to `target`.
    ///
    /// Read-only inspection surface. The authoritative plan is rebuilt under
    /// the repository write lock inside [`WorkspaceHistory::rewind_with`].
    pub fn plan_rewind(
        &self,
        target: Option<&str>,
    ) -> Result<RewindPlan, RewindError<std::convert::Infallible>> {
        let Some(store) = self.store.as_ref() else {
            return Err(RewindError::Disabled);
        };
        plan_rewind_locked(store, target)
    }

    /// Rewind the history to `target`, letting `apply` restore the workspace.
    ///
    /// The whole sequence — plan, apply (preflight + restore), retention of
    /// the abandoned tip, and the compare-and-set that moves `current`
    /// backwards — runs under the kernel's repository write lock, so a
    /// concurrent *recorder* can never interleave. The lock covers history
    /// writes, not the Local Workspace: a Turn installing a mutation takes it
    /// only after its bytes are already on disk, so a concurrently running
    /// Turn can still write a path while a Rewind is between verifying and
    /// restoring it. `apply` returning `Err` aborts before any ref moves. The
    /// abandoned tip is retained first, so every reversed commit stays
    /// reachable afterwards.
    pub fn rewind_with<E>(
        &self,
        target: Option<&str>,
        apply: impl FnOnce(&RewindPlan) -> Result<(), E>,
    ) -> Result<RewindPlan, RewindError<E>> {
        let Some(store) = self.store.as_ref() else {
            return Err(RewindError::Disabled);
        };
        let _write = store.write_lock().map_err(WorkspaceHistoryError::from)?;
        let plan = match plan_rewind_locked(store, target) {
            Ok(plan) => plan,
            Err(err) => return Err(err.into_apply_never()),
        };

        apply(&plan).map_err(RewindError::Apply)?;

        if plan.from != plan.to {
            let expected = plan
                .from
                .as_deref()
                .map(parse_oid)
                .transpose()?
                .expect("a non-empty rewind always starts from a commit");
            // Retention before the ref moves: once `current` steps back, the
            // abandoned tip is only reachable through this pin.
            store
                .retain(expected)
                .map_err(WorkspaceHistoryError::from)?;
            move_current_backwards(store, expected, plan.to.as_deref())
                .map_err(RewindError::RefMove)?;
        }
        Ok(plan)
    }
}

impl RewindError<std::convert::Infallible> {
    /// Re-brand a plan-time error for any apply-error type.
    ///
    /// Planning never produces `Apply`, so this is a lossless conversion.
    fn into_apply_never<E>(self) -> RewindError<E> {
        match self {
            Self::History(e) => RewindError::History(e),
            Self::Disabled => RewindError::Disabled,
            Self::TargetUnreachable { target } => RewindError::TargetUnreachable { target },
            Self::PlanIncomplete { paths } => RewindError::PlanIncomplete { paths },
            Self::RefMove(msg) => RewindError::RefMove(msg),
            Self::Apply(never) => match never {},
        }
    }
}

/// Walk `current` back to `target` and build the reversal plan.
fn plan_rewind_locked(
    store: &StoreRepo,
    target: Option<&str>,
) -> Result<RewindPlan, RewindError<std::convert::Infallible>> {
    let target_oid = target.map(parse_oid).transpose()?;
    let from = store
        .ref_target(CURRENT_REF)
        .map_err(WorkspaceHistoryError::from)?;

    let mut steps = Vec::new();
    // Newest-first walk: a later (re-)insert for the same path comes from an
    // OLDER mutation, so the map naturally converges on the parent-tree state
    // of the oldest reversed mutation per path — the state the path had when
    // LATTICE first touched it after the target.
    let mut restores: BTreeMap<String, Option<Vec<u8>>> = BTreeMap::new();

    let mut cursor = from;
    loop {
        match (cursor, target_oid) {
            (None, None) => break,
            (None, Some(_)) => {
                return Err(RewindError::TargetUnreachable {
                    target: target.unwrap_or("<none>").to_string(),
                });
            }
            (Some(oid), Some(t)) if oid == t => break,
            (Some(oid), _) => {
                let message = store
                    .commit_message(oid)
                    .map_err(WorkspaceHistoryError::from)?;
                let parent = store
                    .commit_parents(oid)
                    .map_err(WorkspaceHistoryError::from)?
                    .into_iter()
                    .next();
                let parent_tree = match parent {
                    Some(parent_oid) => Some(commit_tree_id(store, parent_oid)?),
                    None => None,
                };
                let recorded_at = store
                    .commit_time(oid)
                    .map_err(WorkspaceHistoryError::from)?;
                if let Some(step) = parse_mutation_step(&oid, &message, recorded_at) {
                    let before = tree_blob(store, parent_tree, &step.path)?;
                    restores.insert(step.path.clone(), before);
                    steps.push(step);
                }
                cursor = parent;
            }
        }
    }

    // Preflight expectation per path: the state at `from` — the recorded
    // post-state of the newest reversed mutation of that path.
    let from_tree = match from {
        Some(oid) => Some(commit_tree_id(store, oid)?),
        None => None,
    };
    let mut path_restores = Vec::with_capacity(restores.len());
    for (path, restore) in restores {
        let expected = tree_blob(store, from_tree, &path)?;
        path_restores.push(RewindPathRestore {
            path,
            expected,
            restore,
        });
    }

    // Structural check, independent of how the steps were parsed: the plan
    // must cover every path whose recorded state differs between the target
    // and the tip. Anything else means a mutation commit was mis-read, and
    // proceeding would move `current` while leaving a mutated path both
    // unverified and unrestored — a Rewind reporting success over a path it
    // never touched. Fail closed instead.
    let target_tree = match target_oid {
        Some(oid) => Some(commit_tree_id(store, oid)?),
        None => None,
    };
    let planned: std::collections::BTreeSet<&str> =
        path_restores.iter().map(|r| r.path.as_str()).collect();
    let uncovered: Vec<String> = changed_paths(store, from_tree, target_tree)?
        .into_iter()
        .filter(|path| !planned.contains(path.as_str()))
        .collect();
    if !uncovered.is_empty() {
        return Err(RewindError::PlanIncomplete { paths: uncovered });
    }

    Ok(RewindPlan {
        from: from.map(|oid| oid.to_string()),
        to: target_oid.map(|oid| oid.to_string()),
        steps,
        restores: path_restores,
    })
}

/// Paths whose recorded content differs between two history trees.
fn changed_paths(
    store: &StoreRepo,
    from: Option<gix::ObjectId>,
    to: Option<gix::ObjectId>,
) -> Result<Vec<String>, WorkspaceHistoryError> {
    let from_files = tree_files(store, from)?;
    let to_files = tree_files(store, to)?;
    let mut changed = Vec::new();
    for (path, bytes) in &from_files {
        if to_files.get(path) != Some(bytes) {
            changed.push(path.clone());
        }
    }
    for path in to_files.keys() {
        if !from_files.contains_key(path) {
            changed.push(path.clone());
        }
    }
    Ok(changed)
}

/// Every recorded file in a tree, or an empty map for no tree.
fn tree_files(
    store: &StoreRepo,
    tree: Option<gix::ObjectId>,
) -> Result<BTreeMap<String, Vec<u8>>, WorkspaceHistoryError> {
    match tree {
        Some(oid) => Ok(store.tree_files(oid)?),
        None => Ok(BTreeMap::new()),
    }
}

/// Parse a history commit message into a rewind step.
///
/// Returns `None` for baseline commits (chain plumbing, never reversed on
/// their own) and for messages without the mutation trailer.
fn parse_mutation_step(
    oid: &gix::ObjectId,
    message: &str,
    recorded_at: Option<i64>,
) -> Option<RewindStep> {
    let mut kind = None;
    let mut method = None;
    let mut turn_id = None;
    let mut invocation_id = None;
    let mut path = None;
    let mut content_hash = None;
    for line in message.lines() {
        let Some((key, raw_value)) = line.split_once(':') else {
            continue;
        };
        let value = raw_value.trim();
        match key.trim() {
            KIND_TRAILER => kind = Some(value.to_string()),
            "lattice-method" => method = Some(value.to_string()),
            "lattice-turn-id" => turn_id = Some(value.to_string()),
            "lattice-invocation-id" => invocation_id = value.parse::<InvocationId>().ok(),
            // The path must round-trip byte-exactly: a workspace-relative
            // path can legitimately begin or end with a space (`./ a.txt`
            // normalizes to ` a.txt`), and trimming it here would plan a
            // restore for a path LATTICE never touched while leaving the real
            // one unverified. Only the single separator space is removed.
            "lattice-path" => {
                path = Some(raw_value.strip_prefix(' ').unwrap_or(raw_value).to_string())
            }
            "lattice-content-hash" => content_hash = Some(value.to_string()),
            _ => {}
        }
    }
    if kind.as_deref() != Some(KIND_MUTATION) {
        return None;
    }
    Some(RewindStep {
        commit_id: oid.to_string(),
        method: method.unwrap_or_default(),
        turn_id: turn_id.unwrap_or_default(),
        invocation_id: invocation_id?,
        path: path?,
        recorded_content_hash: content_hash,
        recorded_at,
    })
}

/// Move `current` backwards to `target`, or delete it for an empty target.
///
/// Guarded: the ref must still equal `expected`, which the repository write
/// lock makes certain in-process; the CAS constraint stays as the fail-closed
/// arbiter. Retention pins are untouched.
fn move_current_backwards(
    store: &StoreRepo,
    expected: gix::ObjectId,
    target: Option<&str>,
) -> Result<(), String> {
    let retained_ref = format!("{}/{}", lattice_store::RETAINED_REF_PREFIX, expected);
    match target {
        Some(target) => {
            let target_oid = parse_oid(target).map_err(|e| e.to_string())?;
            store
                .cas_ref(
                    CURRENT_REF,
                    target_oid,
                    RefExpectation::MustMatch(expected),
                    "lattice workspace history rewind",
                )
                .map_err(|e| e.to_string())?;
        }
        None => {
            store
                .delete_ref(CURRENT_REF, RefExpectation::MustMatch(expected))
                .map_err(|e| e.to_string())?;
        }
    }
    store
        // The retention pin was published before `current` moved. Include it
        // in the closing barrier as an explicit invariant even though the
        // kernel's retain CAS also applies its own publication barrier.
        .fsync_pending(&[CURRENT_REF, retained_ref.as_str()])
        .map_err(|e| e.to_string())
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
    // it, and the barrier makes objects and ref durable together.
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
        Ok(()) => {
            store.fsync_pending(&[CURRENT_REF])?;
            Ok(RecordAttempt::Recorded(RecordedMutation {
                commit_id: commit.to_string(),
                baseline_commit_id,
                parent_commit_id: parent.map(|oid| oid.to_string()),
            }))
        }
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
            method: "mutate.create".into(),
            turn_id: "turn-1".into(),
            invocation_id: test_invocation_id(1),
            path: path.into(),
            before: None,
            after: after.as_bytes().to_vec(),
            content_hash: format!("blake3:{}", blake3_hex(after.as_bytes())),
            recorded_at: 1_700_000_000,
        }
    }

    fn edit(path: &str, before: &str, after: &str) -> MutationRecord {
        MutationRecord {
            method: "mutate.edit".into(),
            turn_id: "turn-1".into(),
            invocation_id: test_invocation_id(2),
            path: path.into(),
            before: Some(before.as_bytes().to_vec()),
            after: after.as_bytes().to_vec(),
            content_hash: format!("blake3:{}", blake3_hex(after.as_bytes())),
            recorded_at: 1_700_000_001,
        }
    }

    // Tiny stand-in so the module's tests do not pull a hashing dependency in
    // just to build a plausible Local Revision string.
    fn blake3_hex(bytes: &[u8]) -> String {
        use std::hash::{DefaultHasher, Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        bytes.hash(&mut hasher);
        format!("{:016x}", hasher.finish())
    }

    #[test]
    fn disabled_history_records_nothing() {
        let history = WorkspaceHistory::disabled();
        assert!(!history.is_enabled());
        assert_eq!(history.current().unwrap(), None);
        assert_eq!(history.record(&create("a.txt", "x")).unwrap(), None);
    }

    #[test]
    fn repository_is_bare_and_outside_the_workspace() {
        let state = TempDir::new().unwrap();
        let history = history_at(&state);
        let path = history.repo_path().expect("enabled").to_path_buf();
        assert!(path.ends_with(HISTORY_REPO_DIR));
        assert!(path.join("HEAD").exists(), "bare repo lives at the path");
        assert!(
            !path.join(".git").exists(),
            "a bare repository has no nested .git"
        );
    }

    #[test]
    fn create_records_a_commit_and_moves_current() {
        let state = TempDir::new().unwrap();
        let history = history_at(&state);
        assert_eq!(history.current().unwrap(), None);

        let recorded = history
            .record(&create("src/a.txt", "one"))
            .unwrap()
            .unwrap();
        assert_eq!(recorded.parent_commit_id, None);
        assert_eq!(recorded.baseline_commit_id, None);
        assert_eq!(
            history.current().unwrap().as_deref(),
            Some(recorded.commit_id.as_str())
        );

        let snapshot = history.resolve(&recorded.commit_id).unwrap().unwrap();
        assert_eq!(
            snapshot.files.get("src/a.txt").map(|b| b.as_slice()),
            Some(b"one".as_ref())
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
        assert!(history.is_reachable(&baseline).unwrap());
    }

    /// The Local Workspace is shared with the user. When a path drifts between
    /// two LATTICE mutations, the parent tree holds LATTICE's stale
    /// after-state — the real before state must still be recorded, or a
    /// Rewind built on this history would overwrite the user's own edit.
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

    /// A mutation whose before state already matches the parent tree needs no
    /// baseline — the common case must stay one commit per mutation.
    #[test]
    fn matching_before_state_records_exactly_one_commit() {
        let state = TempDir::new().unwrap();
        let history = history_at(&state);
        history.record(&create("a.txt", "v1")).unwrap().unwrap();
        let recorded = history.record(&edit("a.txt", "v1", "v2")).unwrap().unwrap();
        assert_eq!(recorded.baseline_commit_id, None);
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

        assert!(history.is_reachable(&first.commit_id).unwrap());
        assert!(history.is_reachable(&second.commit_id).unwrap());
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
        assert!(reopened.is_reachable(&recorded.commit_id).unwrap());
    }

    #[test]
    fn unknown_object_id_resolves_to_none() {
        let state = TempDir::new().unwrap();
        let history = history_at(&state);
        let missing = "0".repeat(40);
        assert_eq!(history.resolve(&missing).unwrap(), None);
        assert!(!history.is_reachable(&missing).unwrap());
        assert!(matches!(
            history.resolve("not-an-oid"),
            Err(WorkspaceHistoryError::ObjectId { .. })
        ));
    }

    // ─── Rewind (#64) ───────────────────────────────────────────────────

    /// Open the private repository directly, bypassing the handle (and its
    /// locks) — the unit-test stand-in for an out-of-band actor.
    fn raw_repo(history: &WorkspaceHistory) -> gix::Repository {
        gix::open_opts(
            history.repo_path().expect("enabled"),
            gix::open::Options::isolated(),
        )
        .expect("open private repo")
    }

    #[test]
    fn rewind_plan_collects_only_mutations_after_target_in_reverse_order() {
        let state = TempDir::new().unwrap();
        let history = history_at(&state);
        let first = history.record(&create("a.txt", "a1")).unwrap().unwrap();
        let second = history.record(&create("b.txt", "b1")).unwrap().unwrap();
        let third = history.record(&edit("a.txt", "a1", "a2")).unwrap().unwrap();

        let plan = history.plan_rewind(Some(&first.commit_id)).unwrap();
        assert_eq!(plan.from.as_deref(), Some(third.commit_id.as_str()));
        assert_eq!(plan.to.as_deref(), Some(first.commit_id.as_str()));
        let ids: Vec<&str> = plan.steps.iter().map(|s| s.commit_id.as_str()).collect();
        assert_eq!(
            ids,
            vec![third.commit_id.as_str(), second.commit_id.as_str()],
            "steps are newest-first and exclude the target"
        );
        assert_eq!(plan.steps[0].method, "mutate.edit");
        assert_eq!(plan.steps[0].path, "a.txt");
        assert_eq!(plan.steps[0].turn_id, "turn-1");
        assert_eq!(plan.steps[0].invocation_id, test_invocation_id(2));
        assert!(
            plan.steps[0]
                .recorded_content_hash
                .as_deref()
                .is_some_and(|h| h.starts_with("blake3:")),
            "mutation steps carry the recorded post-state revision"
        );
    }

    #[test]
    fn rewind_restores_the_earliest_parent_state_per_path() {
        let state = TempDir::new().unwrap();
        let history = history_at(&state);
        let target = history.record(&create("f.txt", "v1")).unwrap().unwrap();
        history.record(&edit("f.txt", "v1", "v2")).unwrap().unwrap();
        history.record(&edit("f.txt", "v2", "v3")).unwrap().unwrap();

        let plan = history.plan_rewind(Some(&target.commit_id)).unwrap();
        assert_eq!(plan.restores.len(), 1);
        let restore = &plan.restores[0];
        assert_eq!(restore.path, "f.txt");
        assert_eq!(
            restore.expected.as_deref(),
            Some(b"v3".as_ref()),
            "preflight expects the newest recorded post-state"
        );
        assert_eq!(
            restore.restore.as_deref(),
            Some(b"v1".as_ref()),
            "restore is the parent state of the oldest reversed mutation"
        );
    }

    #[test]
    fn rewind_reverses_a_create_to_absence() {
        let state = TempDir::new().unwrap();
        let history = history_at(&state);
        let target = history.record(&create("kept.txt", "k")).unwrap().unwrap();
        history.record(&create("new.txt", "n")).unwrap().unwrap();

        let plan = history.plan_rewind(Some(&target.commit_id)).unwrap();
        assert_eq!(plan.restores.len(), 1);
        assert_eq!(plan.restores[0].path, "new.txt");
        assert_eq!(plan.restores[0].expected.as_deref(), Some(b"n".as_ref()));
        assert_eq!(
            plan.restores[0].restore, None,
            "reversing a create restores absence"
        );
    }

    /// External drift recorded by a baseline inside the reversed range must
    /// come back: the user's own pre-mutation content is the restore state,
    /// not LATTICE's older recorded state.
    #[test]
    fn rewind_restores_baselined_external_drift() {
        let state = TempDir::new().unwrap();
        let history = history_at(&state);
        let target = history.record(&create("d.txt", "v1")).unwrap().unwrap();
        // User edited d.txt outside LATTICE to "user-edit"; LATTICE then
        // edited it. The recorder wrote a baseline capturing "user-edit".
        let drifted = history
            .record(&edit("d.txt", "user-edit", "v2"))
            .unwrap()
            .unwrap();
        assert!(
            drifted.baseline_commit_id.is_some(),
            "drift forces baseline"
        );

        let plan = history.plan_rewind(Some(&target.commit_id)).unwrap();
        assert_eq!(plan.steps.len(), 1, "the baseline is not a step");
        assert_eq!(
            plan.restores[0].restore.as_deref(),
            Some(b"user-edit".as_ref()),
            "the baselined external content is what comes back"
        );
    }

    /// A create whose target the user had deleted outside LATTICE records a
    /// baseline that *removes* the path. Reversing that create must restore
    /// the user's deletion — not resurrect LATTICE's older content — so the
    /// external delete survives the Rewind.
    #[test]
    fn rewind_restores_absence_across_a_create_after_an_external_delete() {
        let state = TempDir::new().unwrap();
        let history = history_at(&state);
        let target = history.record(&create("anchor.txt", "a")).unwrap().unwrap();
        // LATTICE creates the file, the user deletes it, LATTICE creates it
        // again — the second create baselines the path away first.
        history
            .record(&create("cycle.txt", "first"))
            .unwrap()
            .unwrap();
        let again = history
            .record(&create("cycle.txt", "second"))
            .unwrap()
            .unwrap();
        assert!(
            again.baseline_commit_id.is_some(),
            "a create after an external delete must baseline the path away"
        );

        let plan = history.plan_rewind(Some(&target.commit_id)).unwrap();
        let restore = plan
            .restores
            .iter()
            .find(|r| r.path == "cycle.txt")
            .expect("the cycled path is planned");
        assert_eq!(
            restore.expected.as_deref(),
            Some(b"second".as_ref()),
            "preflight expects the newest recorded post-state"
        );
        assert_eq!(
            restore.restore, None,
            "reversing back past the first create restores absence"
        );
        assert_eq!(plan.steps.len(), 2, "both creates are reversed");
    }

    #[test]
    fn rewind_to_current_is_an_empty_plan_and_moves_nothing() {
        let state = TempDir::new().unwrap();
        let history = history_at(&state);
        let tip = history.record(&create("a.txt", "x")).unwrap().unwrap();

        let plan = history
            .rewind_with(Some(&tip.commit_id), |plan| {
                assert!(plan.steps.is_empty());
                assert!(plan.restores.is_empty());
                Ok::<(), std::convert::Infallible>(())
            })
            .unwrap();
        assert!(plan.is_empty());
        assert_eq!(
            history.current().unwrap().as_deref(),
            Some(tip.commit_id.as_str())
        );
    }

    #[test]
    fn rewind_to_none_unwinds_everything_and_deletes_current() {
        let state = TempDir::new().unwrap();
        let history = history_at(&state);
        let first = history.record(&create("a.txt", "1")).unwrap().unwrap();
        let second = history.record(&create("b.txt", "2")).unwrap().unwrap();

        let plan = history
            .rewind_with(None, |plan| {
                assert_eq!(plan.steps.len(), 2);
                assert!(plan.restores.iter().all(|r| r.restore.is_none()));
                Ok::<(), std::convert::Infallible>(())
            })
            .unwrap();
        assert_eq!(plan.to, None);
        assert_eq!(history.current().unwrap(), None, "current is deleted");
        // Reversed commits stay pinned and resolvable.
        assert!(history.is_reachable(&first.commit_id).unwrap());
        assert!(history.is_reachable(&second.commit_id).unwrap());
        assert!(history.resolve(&second.commit_id).unwrap().is_some());
    }

    #[test]
    fn rewind_rejects_targets_that_are_not_on_the_chain() {
        let state = TempDir::new().unwrap();
        let history = history_at(&state);
        history.record(&create("a.txt", "1")).unwrap().unwrap();

        let bogus = "1".repeat(40);
        let err = history.plan_rewind(Some(&bogus)).unwrap_err();
        assert!(
            matches!(err, RewindError::TargetUnreachable { .. }),
            "got {err:?}"
        );

        // An empty history cannot reach any concrete target either.
        let empty_state = TempDir::new().unwrap();
        let empty = history_at(&empty_state);
        let err = empty.plan_rewind(Some(&bogus)).unwrap_err();
        assert!(matches!(err, RewindError::TargetUnreachable { .. }));
    }

    /// After a rewind, the abandoned tip is pinned but no longer on the
    /// chain — a second rewind "forward" to it must be rejected, not
    /// silently replay.
    #[test]
    fn rewind_cannot_target_a_commit_abandoned_by_a_prior_rewind() {
        let state = TempDir::new().unwrap();
        let history = history_at(&state);
        let first = history.record(&create("a.txt", "1")).unwrap().unwrap();
        let second = history.record(&create("b.txt", "2")).unwrap().unwrap();
        history
            .rewind_with(Some(&first.commit_id), |_| {
                Ok::<(), std::convert::Infallible>(())
            })
            .unwrap();

        let err = history.plan_rewind(Some(&second.commit_id)).unwrap_err();
        assert!(
            matches!(err, RewindError::TargetUnreachable { .. }),
            "an off-chain (though pinned) commit is not a valid target: {err:?}"
        );
        assert!(history.is_reachable(&second.commit_id).unwrap());
    }

    #[test]
    fn rewind_apply_failure_aborts_before_any_ref_moves() {
        let state = TempDir::new().unwrap();
        let history = history_at(&state);
        let first = history.record(&create("a.txt", "1")).unwrap().unwrap();
        let tip = history.record(&create("b.txt", "2")).unwrap().unwrap();

        let err = history
            .rewind_with(Some(&first.commit_id), |_| Err("workspace conflict"))
            .unwrap_err();
        assert!(matches!(err, RewindError::Apply("workspace conflict")));
        assert_eq!(
            history.current().unwrap().as_deref(),
            Some(tip.commit_id.as_str()),
            "an aborted rewind must not move current"
        );
    }

    /// If the ref cannot be moved durably after the workspace was restored,
    /// the rewind must fail loudly — never report success.
    #[test]
    fn rewind_ref_move_failure_is_an_error_not_a_success() {
        let state = TempDir::new().unwrap();
        let history = history_at(&state);
        let first = history.record(&create("a.txt", "1")).unwrap().unwrap();
        history.record(&create("b.txt", "2")).unwrap().unwrap();

        let err = history
            .rewind_with(Some(&first.commit_id), |_| {
                // Sabotage: an out-of-band actor deletes `current` while the
                // apply phase runs (raw gix, so the advisory lock is not
                // taken). The trailing CAS must fail closed.
                let repo = raw_repo(&history);
                let name: gix::refs::FullName = CURRENT_REF.try_into().unwrap();
                repo.edit_reference(gix::refs::transaction::RefEdit {
                    change: gix::refs::transaction::Change::Delete {
                        expected: PreviousValue::Any,
                        log: gix::refs::transaction::RefLog::AndReference,
                    },
                    name,
                    deref: false,
                })
                .expect("sabotage delete");
                Ok::<(), std::convert::Infallible>(())
            })
            .unwrap_err();
        assert!(matches!(err, RewindError::RefMove(_)), "got {err:?}");
    }

    #[test]
    fn rewind_keeps_every_reversed_commit_reachable() {
        let state = TempDir::new().unwrap();
        let history = history_at(&state);
        let target = history.record(&create("a.txt", "1")).unwrap().unwrap();
        let reversed_a = history.record(&edit("a.txt", "1", "2")).unwrap().unwrap();
        let reversed_b = history.record(&create("c.txt", "3")).unwrap().unwrap();

        history
            .rewind_with(Some(&target.commit_id), |_| {
                Ok::<(), std::convert::Infallible>(())
            })
            .unwrap();

        assert_eq!(
            history.current().unwrap().as_deref(),
            Some(target.commit_id.as_str())
        );
        for oid in [&reversed_a.commit_id, &reversed_b.commit_id] {
            assert!(history.is_reachable(oid).unwrap(), "{oid} lost its pin");
            assert!(history.resolve(oid).unwrap().is_some());
        }
    }

    #[test]
    fn recording_after_a_rewind_appends_on_the_new_tip() {
        let state = TempDir::new().unwrap();
        let history = history_at(&state);
        let target = history.record(&create("a.txt", "v1")).unwrap().unwrap();
        history.record(&edit("a.txt", "v1", "v2")).unwrap().unwrap();
        history
            .rewind_with(Some(&target.commit_id), |_| {
                Ok::<(), std::convert::Infallible>(())
            })
            .unwrap();

        let next = history.record(&edit("a.txt", "v1", "v4")).unwrap().unwrap();
        assert_eq!(
            next.parent_commit_id.as_deref(),
            Some(target.commit_id.as_str()),
            "post-rewind recording chains on the rewound tip"
        );
        assert_eq!(
            next.baseline_commit_id, None,
            "the restored content matches the rewound tree, so no baseline"
        );
    }

    /// A workspace-relative path may legitimately begin with a space —
    /// `mutate.create` with `./ a.txt` normalizes to ` a.txt`. The path
    /// trailer must round-trip byte-exactly, or the plan would verify and
    /// restore a *different* path while leaving the real one untouched and
    /// still move `current`.
    #[test]
    fn a_path_with_leading_whitespace_round_trips_through_the_plan() {
        let state = TempDir::new().unwrap();
        let history = history_at(&state);
        let target = history.record(&create("anchor.txt", "a")).unwrap().unwrap();
        history
            .record(&create(" spaced.txt", "s1"))
            .unwrap()
            .unwrap();

        let plan = history.plan_rewind(Some(&target.commit_id)).unwrap();
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(
            plan.steps[0].path, " spaced.txt",
            "the recorded path must survive parsing unchanged"
        );
        assert_eq!(plan.restores.len(), 1);
        assert_eq!(plan.restores[0].path, " spaced.txt");
        assert_eq!(plan.restores[0].expected.as_deref(), Some(b"s1".as_ref()));
        assert_eq!(plan.restores[0].restore, None);
    }

    /// The plan must cover every path the reversed range changed. A commit
    /// whose mutation trailer cannot be read is not a licence to move
    /// `current` over an unverified path.
    #[test]
    fn a_plan_that_misses_a_changed_path_fails_closed() {
        let state = TempDir::new().unwrap();
        let history = history_at(&state);
        let target = history.record(&create("a.txt", "a")).unwrap().unwrap();
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

        let err = history.plan_rewind(Some(&target.commit_id)).unwrap_err();
        match err {
            RewindError::PlanIncomplete { paths } => assert_eq!(paths, vec!["b.txt".to_string()]),
            other => panic!("expected PlanIncomplete, got {other:?}"),
        }
        assert_eq!(
            history.current().unwrap().as_deref(),
            Some(opaque.to_string().as_str()),
            "a refused plan must not move the ref"
        );
    }

    #[test]
    fn rewind_on_disabled_history_is_rejected() {
        let history = WorkspaceHistory::disabled();
        let err = history
            .rewind_with(None, |_| Ok::<(), std::convert::Infallible>(()))
            .unwrap_err();
        assert!(matches!(err, RewindError::Disabled));
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
        let store_path = backing.store_path().expect("enabled backing");

        let err = WorkspaceHistory::open(backing).expect_err("absent store must fail");
        assert!(
            matches!(
                err,
                WorkspaceHistoryError::Kernel(StoreError::NotInitialized { .. })
            ),
            "expected typed NotInitialized, got {err:?}"
        );
        assert!(
            !store_path.exists(),
            "strict open must not create the store"
        );
    }

    /// The operation lock is the per-project lock under the state layout, and
    /// explicit guard passing is the only same-process re-entry path.
    #[test]
    fn operation_lock_is_project_scoped_and_reentrant() {
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

        // Re-entrant only through the explicit guard capability: a mutation
        // that records while already holding the operation lock must not
        // deadlock, and an independent acquire must not be mistaken for it.
        let nested = guard.reenter().expect("explicit re-entrant acquire");
        drop(nested);

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

    /// A disabled history has no project binding, so it has no lock to take.
    #[test]
    fn disabled_history_has_no_operation_lock() {
        let history = WorkspaceHistory::disabled();
        assert!(
            history
                .lock_workspace_operation(LockMode::Exclusive, Blocking::Block)
                .unwrap()
                .is_none()
        );
    }
}

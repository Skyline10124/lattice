// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Private workspace mutation history (GitHub #63).
//!
//! Every Atomic Local Mutation that LATTICE **successfully installs** is
//! appended to a durable history that is independent of the Session History,
//! of the Local Workspace, and of the user's project `.git`.
//!
//! The history is a private **bare** repository driven by `gix` — no Git
//! subprocess, no `git2`. LATTICE mutation state is expressed with ordinary
//! Git objects: blobs for file content, trees for the before/after state of
//! the paths LATTICE has touched, commits for ordering and parent
//! relationships, and internal refs for reachability. There is no parallel
//! custom blob format, receipt store, or SQLite workspace graph.
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
//! <state>/lattice/projects/<project-key>/workspace-history.git   (bare)
//!   refs/lattice/workspace-history/current      -> newest recorded mutation
//!   refs/lattice/workspace-history/records/<oid> -> one pin per recorded commit
//! ```
//!
//! `current` moves forward on every recorded mutation. The per-record pin refs
//! exist so prior object IDs stay reachable forever even after `current` moves
//! backwards — an explicit Rewind (GitHub #64) never has to rely on ancestry,
//! and nothing here uses time-based expiry, compensation commits, or reflog
//! retention. The repository is private state; the user's project refs are
//! never touched.
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
//! One handle serializes its own recording behind a mutex, but a second
//! LATTICE process — or a second runtime in one process — holds its own
//! handle. The compare-and-set on `current` is the real arbiter: a writer that
//! loses re-reads the tip and appends on top of the winner, because the bytes
//! it is recording are already installed on disk and dropping them would lose
//! real history.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use gix::objs::tree::EntryKind;
use gix::refs::transaction::PreviousValue;

/// Ref holding the newest recorded mutation commit.
pub const CURRENT_REF: &str = "refs/lattice/workspace-history/current";

/// Namespace holding one permanent pin ref per recorded commit.
pub const RECORD_REF_PREFIX: &str = "refs/lattice/workspace-history/records";

/// Directory name of the private bare history repository.
pub const HISTORY_REPO_DIR: &str = "workspace-history.git";

/// Cross-process recording lock inside the private repository.
const HISTORY_LOCK_FILE: &str = "lattice-history.lock";

const HISTORY_AUTHOR_NAME: &str = "LATTICE";
const HISTORY_AUTHOR_EMAIL: &str = "lattice@localhost";

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

/// Where the private workspace history lives.
#[derive(Debug, Clone)]
pub enum WorkspaceHistoryBacking {
    /// No history recording.
    ///
    /// For unit-test servers and embedders that have no per-project state
    /// root. Production turn runtimes always resolve a [`Self::Repo`] path.
    Disabled,
    /// Private bare repository at this path, created if absent.
    Repo(PathBuf),
}

impl WorkspaceHistoryBacking {
    /// Private history repository under a per-project state directory.
    ///
    /// `project_state_dir` is the same per-project directory that holds the
    /// CEP audit database, so the history lands outside both the Local
    /// Workspace and the user's project `.git`.
    pub fn under_project_state_dir(project_state_dir: impl AsRef<Path>) -> Self {
        Self::Repo(project_state_dir.as_ref().join(HISTORY_REPO_DIR))
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
    pub call_id: u64,
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
/// Deep module: repository creation, object writing, ordering, ref movement,
/// and reachability all live behind this type. Callers hand it a
/// [`MutationRecord`] after a successful install and read back object ids.
pub struct WorkspaceHistory {
    /// `None` when recording is disabled.
    ///
    /// `gix::Repository` is `Send` but not `Sync`, and the mutex additionally
    /// serializes recording so concurrent mutations chain in real execution
    /// order with true parent relationships.
    repo: Option<Mutex<gix::Repository>>,
    path: Option<PathBuf>,
    /// Advisory lock file serializing recording across handles and processes.
    lock_path: Option<PathBuf>,
}

impl std::fmt::Debug for WorkspaceHistory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkspaceHistory")
            .field("path", &self.path)
            .finish()
    }
}

impl WorkspaceHistory {
    /// Open (or create) the history for `backing`.
    pub fn open(backing: WorkspaceHistoryBacking) -> Result<Self, WorkspaceHistoryError> {
        match backing {
            WorkspaceHistoryBacking::Disabled => Ok(Self::disabled()),
            WorkspaceHistoryBacking::Repo(path) => {
                let repo = open_or_init_bare(&path)?;
                let lock_path = repo.path().join(HISTORY_LOCK_FILE);
                Ok(Self {
                    repo: Some(Mutex::new(repo)),
                    path: Some(path),
                    lock_path: Some(lock_path),
                })
            }
        }
    }

    /// A history that records nothing.
    pub fn disabled() -> Self {
        Self {
            repo: None,
            path: None,
            lock_path: None,
        }
    }

    /// Whether mutations are recorded.
    pub fn is_enabled(&self) -> bool {
        self.repo.is_some()
    }

    /// Path of the private bare repository, when enabled.
    pub fn repo_path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// The object id `current` points at, if any mutation has been recorded.
    pub fn current(&self) -> Result<Option<String>, WorkspaceHistoryError> {
        let Some(repo) = self.repo.as_ref() else {
            return Ok(None);
        };
        let repo = repo.lock().unwrap_or_else(|e| e.into_inner());
        Ok(ref_target(&repo, CURRENT_REF)?.map(|oid| oid.to_string()))
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
        let Some(repo) = self.repo.as_ref() else {
            return Ok(None);
        };
        let path = normalize_history_path(&record.path)?;
        let repo = repo.lock().unwrap_or_else(|e| e.into_inner());
        // The in-process mutex only covers this handle. A second LATTICE
        // runtime — in this process or another — has its own handle, so the
        // read-build-advance sequence is additionally serialized by an
        // advisory lock on a file inside the private repository. The kernel
        // releases it if the holder dies.
        let _guard = RecordingLock::acquire(self.lock_path.as_deref())?;
        record_locked(&repo, &path, record).map(Some)
    }

    /// Read back the LATTICE-mutated file state at `oid`.
    ///
    /// Returns `Ok(None)` when recording is disabled or the object is not a
    /// commit in this history.
    pub fn resolve(&self, oid: &str) -> Result<Option<HistorySnapshot>, WorkspaceHistoryError> {
        let Some(repo) = self.repo.as_ref() else {
            return Ok(None);
        };
        let object_id = parse_oid(oid)?;
        let repo = repo.lock().unwrap_or_else(|e| e.into_inner());
        let Ok(commit) = repo.find_commit(object_id) else {
            return Ok(None);
        };
        let tree = commit
            .tree()
            .map_err(|e| WorkspaceHistoryError::Repo(format!("read history tree: {e}")))?;
        let mut files = BTreeMap::new();
        collect_tree_files(&tree, "", &mut files)?;
        Ok(Some(HistorySnapshot {
            commit_id: object_id.to_string(),
            files,
        }))
    }

    /// Whether `oid` is still reachable through this history's internal refs.
    ///
    /// Reachability is what keeps prior object ids resolvable; it never
    /// depends on `current` still pointing at or past them.
    pub fn is_reachable(&self, oid: &str) -> Result<bool, WorkspaceHistoryError> {
        let Some(repo) = self.repo.as_ref() else {
            return Ok(false);
        };
        let object_id = parse_oid(oid)?;
        let repo = repo.lock().unwrap_or_else(|e| e.into_inner());
        Ok(ref_target(&repo, &record_ref_name(&object_id))? == Some(object_id))
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
    pub call_id: Option<u64>,
    /// Workspace-relative path the mutation touched.
    pub path: String,
    /// Post-install Local Revision recorded for the mutation.
    pub recorded_content_hash: Option<String>,
}

/// The final on-disk state one path must reach for a Rewind to complete.
///
/// `expected` is what the Local Workspace must currently hold (the recorded
/// post-state of the newest reversed mutation of the path); `restore` is what
/// the path becomes (the parent-tree state of the oldest reversed mutation of
/// the path). `None` means absence on both sides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewindPathRestore {
    /// Workspace-relative path.
    pub path: String,
    /// Bytes the path must currently hold, or `None` when it must be absent.
    pub expected: Option<Vec<u8>>,
    /// Bytes to restore, or `None` to delete the path.
    pub restore: Option<Vec<u8>>,
}

/// A planned Rewind: which mutation commits are reversed and what every
/// affected path must currently hold and will be restored to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewindPlan {
    /// The commit `current` pointed at when the plan was built.
    pub from: Option<String>,
    /// The commit `current` will point at afterwards (`None` deletes it).
    pub to: Option<String>,
    /// Reversed mutation commits, newest first (reverse execution order).
    pub steps: Vec<RewindStep>,
    /// Per-path preflight/restore states, one entry per distinct path.
    pub restores: Vec<RewindPathRestore>,
}

impl RewindPlan {
    /// Whether the plan changes nothing (target already is the identity).
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty() && self.from == self.to
    }
}

/// Failure of an explicit Rewind.
///
/// `E` is the caller's own abort reason, produced by the apply closure —
/// typically a structured same-path conflict report or a restore I/O error.
#[derive(Debug, thiserror::Error)]
pub enum RewindError<E> {
    /// Reading or writing the private history repository failed.
    #[error(transparent)]
    History(#[from] WorkspaceHistoryError),
    /// Recording is disabled, so there is no history to rewind.
    #[error("workspace history is disabled; nothing to rewind")]
    Disabled,
    /// The target identity is not on the chain reachable from `current`.
    #[error("rewind target '{target}' is not reachable from the current history identity")]
    TargetUnreachable {
        /// The requested target object id (hex), or "<none>" for empty.
        target: String,
    },
    /// The planned restores do not cover every path the range changed.
    ///
    /// A mutation commit could not be read as one, so proceeding would move
    /// `current` over a path that was never verified or restored.
    #[error("rewind plan does not cover changed path(s): {paths:?}")]
    PlanIncomplete {
        /// Paths that differ between the target and the tip but have no
        /// planned restore.
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
    /// the recording lock inside [`WorkspaceHistory::rewind_with`].
    pub fn plan_rewind(
        &self,
        target: Option<&str>,
    ) -> Result<RewindPlan, RewindError<std::convert::Infallible>> {
        let Some(repo) = self.repo.as_ref() else {
            return Err(RewindError::Disabled);
        };
        let repo = repo.lock().unwrap_or_else(|e| e.into_inner());
        plan_rewind_locked(&repo, target)
    }

    /// Rewind the history to `target`, letting `apply` restore the workspace.
    ///
    /// The whole sequence — plan, apply (preflight + restore), and the
    /// compare-and-set that moves `current` backwards — runs under the same
    /// advisory lock that serializes recording, so a concurrent mutation
    /// recorder can never interleave. `apply` returning `Err` aborts before
    /// any ref moves. Pin refs are never touched, so every reversed commit
    /// stays reachable afterwards.
    pub fn rewind_with<E>(
        &self,
        target: Option<&str>,
        apply: impl FnOnce(&RewindPlan) -> Result<(), E>,
    ) -> Result<RewindPlan, RewindError<E>> {
        let Some(repo) = self.repo.as_ref() else {
            return Err(RewindError::Disabled);
        };
        let repo = repo.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = RecordingLock::acquire(self.lock_path.as_deref())?;
        let plan = match plan_rewind_locked(&repo, target) {
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
            move_current_backwards(&repo, expected, plan.to.as_deref())
                .map_err(|e| RewindError::RefMove(e))?;
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
    repo: &gix::Repository,
    target: Option<&str>,
) -> Result<RewindPlan, RewindError<std::convert::Infallible>> {
    let target_oid = target.map(parse_oid).transpose()?;
    let from = ref_target(repo, CURRENT_REF)?;

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
                let commit = repo.find_commit(oid).map_err(|e| {
                    WorkspaceHistoryError::Repo(format!("find history commit {oid}: {e}"))
                })?;
                let message = commit.message_raw_sloppy().to_string();
                let parent = commit.parent_ids().next().map(|id| id.detach());
                let parent_tree = match parent {
                    Some(parent_oid) => commit_tree_id(repo, parent_oid)?,
                    None => gix::ObjectId::empty_tree(repo.object_hash()),
                };
                if let Some(step) = parse_mutation_step(&oid, &message) {
                    let before = tree_blob(repo, parent_tree, &step.path)?;
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
        Some(oid) => Some(commit_tree_id(repo, oid)?),
        None => None,
    };
    let mut path_restores = Vec::with_capacity(restores.len());
    for (path, restore) in restores {
        let expected = match from_tree {
            Some(tree) => tree_blob(repo, tree, &path)?,
            None => None,
        };
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
        Some(oid) => Some(commit_tree_id(repo, oid)?),
        None => None,
    };
    let planned: std::collections::BTreeSet<&str> =
        path_restores.iter().map(|r| r.path.as_str()).collect();
    let uncovered: Vec<String> = changed_paths(repo, from_tree, target_tree)?
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
    repo: &gix::Repository,
    from: Option<gix::ObjectId>,
    to: Option<gix::ObjectId>,
) -> Result<Vec<String>, WorkspaceHistoryError> {
    let from_files = tree_files(repo, from)?;
    let to_files = tree_files(repo, to)?;
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
    repo: &gix::Repository,
    tree: Option<gix::ObjectId>,
) -> Result<BTreeMap<String, Vec<u8>>, WorkspaceHistoryError> {
    let mut files = BTreeMap::new();
    if let Some(oid) = tree {
        let tree = repo
            .find_tree(oid)
            .map_err(|e| WorkspaceHistoryError::Repo(format!("find history tree: {e}")))?;
        collect_tree_files(&tree, "", &mut files)?;
    }
    Ok(files)
}

/// Parse a history commit message into a rewind step.
///
/// Returns `None` for baseline commits (chain plumbing, never reversed on
/// their own) and for messages without the mutation trailer.
fn parse_mutation_step(oid: &gix::ObjectId, message: &str) -> Option<RewindStep> {
    let mut kind = None;
    let mut method = None;
    let mut turn_id = None;
    let mut call_id = None;
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
            "lattice-call-id" => call_id = value.parse::<u64>().ok(),
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
        call_id,
        path: path?,
        recorded_content_hash: content_hash,
    })
}

/// Move `current` backwards to `target`, or delete it for an empty target.
///
/// Guarded: the ref must still equal `expected`, which the recording lock
/// makes certain in-process; the constraint stays as the cross-process
/// fail-closed arbiter. Pin refs are untouched.
fn move_current_backwards(
    repo: &gix::Repository,
    expected: gix::ObjectId,
    target: Option<&str>,
) -> Result<(), String> {
    let constraint = PreviousValue::MustExistAndMatch(gix::refs::Target::Object(expected));
    match target {
        Some(target) => {
            let target_oid = parse_oid(target).map_err(|e| e.to_string())?;
            repo.reference(
                CURRENT_REF,
                target_oid,
                constraint,
                "lattice workspace history rewind",
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
        }
        None => {
            let name: gix::refs::FullName = CURRENT_REF
                .try_into()
                .map_err(|e| format!("current ref name: {e}"))?;
            repo.edit_reference(gix::refs::transaction::RefEdit {
                change: gix::refs::transaction::Change::Delete {
                    expected: constraint,
                    log: gix::refs::transaction::RefLog::AndReference,
                },
                name,
                deref: false,
            })
            .map(|_| ())
            .map_err(|e| e.to_string())
        }
    }
}

// ─── Recording ──────────────────────────────────────────────────────────────

/// Advisory exclusive lock held for one record operation.
///
/// Serializes the read-build-advance sequence across every handle and process
/// that shares one private history. `flock` is the right primitive here: the
/// kernel releases it when the holder's file descriptor closes, including on
/// process death, so a crashed LATTICE cannot wedge the history behind a stale
/// lock file the way an `O_EXCL` marker would.
struct RecordingLock {
    #[cfg(unix)]
    _file: std::fs::File,
}

impl RecordingLock {
    fn acquire(path: Option<&Path>) -> Result<Self, WorkspaceHistoryError> {
        let _ = path;
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            let path = path.ok_or_else(|| {
                WorkspaceHistoryError::Repo("history lock path is unset".to_string())
            })?;
            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .map_err(|e| {
                    WorkspaceHistoryError::Repo(format!(
                        "open history lock {}: {e}",
                        path.display()
                    ))
                })?;
            // SAFETY: `file` owns a valid open descriptor for the duration of
            // the call, and `flock` only sets an advisory lock on it.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
                return Err(WorkspaceHistoryError::Repo(format!(
                    "lock history for recording: {}",
                    std::io::Error::last_os_error()
                )));
            }
            Ok(Self { _file: file })
        }
        #[cfg(not(unix))]
        {
            // No advisory lock available; the compare-and-set retry below is
            // the only serialization.
            Ok(Self {})
        }
    }
}

/// How many times a losing compare-and-set on `current` is retried.
///
/// `current` only ever moves forward by one commit per recorded mutation, so a
/// loser just needs to rebuild on the new tip. The bound exists so a pathological
/// writer cannot spin forever; it is far above any real contention.
const ADVANCE_RETRY_LIMIT: usize = 64;

fn record_locked(
    repo: &gix::Repository,
    path: &str,
    record: &MutationRecord,
) -> Result<RecordedMutation, WorkspaceHistoryError> {
    // The in-process mutex serializes this handle, but a second LATTICE
    // process (or a second runtime in one process) has its own handle and its
    // own lock — the ref compare-and-set is the only real arbiter. A loser
    // must rebuild on the winner's tip rather than drop the mutation: the
    // bytes are already installed on disk, so failing to record would lose
    // history for a mutation that really happened.
    let mut last_conflict = None;
    for _ in 0..ADVANCE_RETRY_LIMIT {
        match try_record_once(repo, path, record)? {
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
    repo: &gix::Repository,
    path: &str,
    record: &MutationRecord,
) -> Result<RecordAttempt, WorkspaceHistoryError> {
    let parent = ref_target(repo, CURRENT_REF)?;
    let parent_tree = match parent {
        Some(oid) => commit_tree_id(repo, oid)?,
        None => gix::ObjectId::empty_tree(repo.object_hash()),
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
    let recorded_before = tree_blob(repo, parent_tree, path)?;
    let needs_baseline = recorded_before.as_deref() != record.before.as_deref();
    let (effective_parent, effective_tree, baseline_commit_id) = if needs_baseline {
        let tree = match record.before.as_deref() {
            Some(before) => upsert_path(repo, parent_tree, path, before)?,
            None => remove_path(repo, parent_tree, path)?,
        };
        let message = baseline_message(record, path);
        let commit = write_commit(repo, &message, tree, parent, record.recorded_at)?;
        (Some(commit), tree, Some(commit.to_string()))
    } else {
        (parent, parent_tree, None)
    };

    let tree = upsert_path(repo, effective_tree, path, &record.after)?;
    let message = mutation_message(record, path);
    let commit = write_commit(repo, &message, tree, effective_parent, record.recorded_at)?;

    // Pin every recorded commit before moving `current`, so a crash between
    // the two leaves the objects reachable rather than dangling.
    if needs_baseline && let Some(baseline) = effective_parent {
        pin_record(repo, baseline)?;
    }
    pin_record(repo, commit)?;
    match advance_current(repo, parent, commit)? {
        AdvanceOutcome::Advanced => Ok(RecordAttempt::Recorded(RecordedMutation {
            commit_id: commit.to_string(),
            baseline_commit_id,
            parent_commit_id: parent.map(|oid| oid.to_string()),
        })),
        AdvanceOutcome::Conflict(err) => Ok(RecordAttempt::CurrentMoved(err)),
    }
}

fn upsert_path(
    repo: &gix::Repository,
    base_tree: gix::ObjectId,
    path: &str,
    content: &[u8],
) -> Result<gix::ObjectId, WorkspaceHistoryError> {
    let blob = repo
        .write_blob(content)
        .map(|id| id.detach())
        .map_err(|e| WorkspaceHistoryError::Repo(format!("write history blob: {e}")))?;
    let mut editor = repo
        .edit_tree(base_tree)
        .map_err(|e| WorkspaceHistoryError::Repo(format!("open history tree editor: {e}")))?;
    editor
        .upsert(path, EntryKind::Blob, blob)
        .map_err(|e| WorkspaceHistoryError::Repo(format!("upsert '{path}' into history: {e}")))?;
    editor
        .write()
        .map(|id| id.detach())
        .map_err(|e| WorkspaceHistoryError::Repo(format!("write history tree: {e}")))
}

fn write_commit(
    repo: &gix::Repository,
    message: &str,
    tree: gix::ObjectId,
    parent: Option<gix::ObjectId>,
    recorded_at: i64,
) -> Result<gix::ObjectId, WorkspaceHistoryError> {
    let signature = history_signature(recorded_at);
    let mut time_buf = gix::date::parse::TimeBuf::default();
    let signature_ref = signature.to_ref(&mut time_buf);
    let commit = repo
        .new_commit_as(signature_ref, signature_ref, message, tree, parent)
        .map_err(|e| WorkspaceHistoryError::Repo(format!("write history commit: {e}")))?;
    Ok(commit.id)
}

/// Pin a recorded commit so it stays reachable regardless of `current`.
///
/// Idempotent: re-recording an identical mutation resolves to the same object
/// id, and an already-present pin is not an error.
fn pin_record(repo: &gix::Repository, commit: gix::ObjectId) -> Result<(), WorkspaceHistoryError> {
    let name = record_ref_name(&commit);
    match repo.reference(
        name.as_str(),
        commit,
        PreviousValue::MustNotExist,
        "lattice workspace history record",
    ) {
        Ok(_) => Ok(()),
        Err(err) => {
            if ref_target(repo, &name)? == Some(commit) {
                Ok(())
            } else {
                Err(WorkspaceHistoryError::Repo(format!(
                    "pin history record {commit}: {err}"
                )))
            }
        }
    }
}

/// Result of one compare-and-set on `current`.
enum AdvanceOutcome {
    Advanced,
    /// `current` no longer matched `expected`; the caller must rebuild.
    Conflict(String),
}

/// Compare-and-set `current` from `expected` to `next`.
///
/// A constraint failure is a normal outcome under concurrent recorders, not an
/// error: the caller re-reads the tip and appends again.
fn advance_current(
    repo: &gix::Repository,
    expected: Option<gix::ObjectId>,
    next: gix::ObjectId,
) -> Result<AdvanceOutcome, WorkspaceHistoryError> {
    let constraint = match expected {
        Some(oid) => PreviousValue::MustExistAndMatch(gix::refs::Target::Object(oid)),
        None => PreviousValue::MustNotExist,
    };
    match repo.reference(
        CURRENT_REF,
        next,
        constraint,
        "lattice workspace history mutation",
    ) {
        Ok(_) => Ok(AdvanceOutcome::Advanced),
        Err(err) => {
            // Distinguish "someone else moved it" from a real repository
            // failure by re-reading: if the tip is no longer what we built on,
            // this was contention.
            let current = ref_target(repo, CURRENT_REF)?;
            if current == expected {
                Err(WorkspaceHistoryError::Repo(format!(
                    "advance history current ref: {err}"
                )))
            } else {
                Ok(AdvanceOutcome::Conflict(err.to_string()))
            }
        }
    }
}

// ─── Commit messages ────────────────────────────────────────────────────────

fn mutation_message(record: &MutationRecord, path: &str) -> String {
    format!(
        "{method} {path}\n\n\
         {KIND_TRAILER}: {KIND_MUTATION}\n\
         lattice-method: {method}\n\
         lattice-turn-id: {turn}\n\
         lattice-call-id: {call}\n\
         lattice-path: {path}\n\
         lattice-content-hash: {hash}\n",
        method = record.method,
        turn = record.turn_id,
        call = record.call_id,
        hash = record.content_hash,
    )
}

fn baseline_message(record: &MutationRecord, path: &str) -> String {
    format!(
        "baseline {path}\n\n\
         {KIND_TRAILER}: {KIND_BASELINE}\n\
         lattice-method: {method}\n\
         lattice-turn-id: {turn}\n\
         lattice-call-id: {call}\n\
         lattice-path: {path}\n",
        method = record.method,
        turn = record.turn_id,
        call = record.call_id,
    )
}

// ─── Repository helpers ─────────────────────────────────────────────────────

/// Open the private bare repository, creating it on first use.
///
/// `isolated()` keeps the user's global/system Git configuration out of the
/// private history: it is LATTICE state, not a user repository.
fn open_or_init_bare(path: &Path) -> Result<gix::Repository, WorkspaceHistoryError> {
    if let Ok(repo) = gix::open_opts(path, gix::open::Options::isolated()) {
        return Ok(repo);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            WorkspaceHistoryError::Repo(format!(
                "create history parent directory {}: {e}",
                parent.display()
            ))
        })?;
    }
    let init_error = match gix::init_bare(path) {
        // Re-open isolated so subsequent reads/writes ignore ambient Git config.
        Ok(repo) => match gix::open_opts(repo.path(), gix::open::Options::isolated()) {
            Ok(repo) => return Ok(repo),
            Err(err) => err.to_string(),
        },
        Err(err) => err.to_string(),
    };
    // Two LATTICE processes can reach a not-yet-created history at the same
    // time; the loser sees a half-written or already-initialized directory.
    // A successful open now means the winner finished, which is a success for
    // this caller too.
    gix::open_opts(path, gix::open::Options::isolated()).map_err(|open_err| {
        WorkspaceHistoryError::Repo(format!(
            "open history repository {}: {open_err} (init failed: {init_error})",
            path.display()
        ))
    })
}

fn ref_target(
    repo: &gix::Repository,
    name: &str,
) -> Result<Option<gix::ObjectId>, WorkspaceHistoryError> {
    match repo.try_find_reference(name) {
        Ok(Some(reference)) => Ok(reference.target().try_id().map(|id| id.to_owned())),
        Ok(None) => Ok(None),
        Err(e) => Err(WorkspaceHistoryError::Repo(format!(
            "find history ref {name}: {e}"
        ))),
    }
}

fn commit_tree_id(
    repo: &gix::Repository,
    commit: gix::ObjectId,
) -> Result<gix::ObjectId, WorkspaceHistoryError> {
    let commit = repo
        .find_commit(commit)
        .map_err(|e| WorkspaceHistoryError::Repo(format!("find history commit: {e}")))?;
    commit
        .tree_id()
        .map(|id| id.detach())
        .map_err(|e| WorkspaceHistoryError::Repo(format!("read history commit tree id: {e}")))
}

/// Bytes recorded at `path` in `tree`, or `None` when the path is absent.
fn tree_blob(
    repo: &gix::Repository,
    tree: gix::ObjectId,
    path: &str,
) -> Result<Option<Vec<u8>>, WorkspaceHistoryError> {
    let tree = repo
        .find_tree(tree)
        .map_err(|e| WorkspaceHistoryError::Repo(format!("find history tree: {e}")))?;
    let Some(entry) = tree
        .lookup_entry_by_path(path)
        .map_err(|e| WorkspaceHistoryError::Repo(format!("lookup '{path}' in history: {e}")))?
    else {
        return Ok(None);
    };
    let object = entry
        .object()
        .map_err(|e| WorkspaceHistoryError::Repo(format!("load '{path}' from history: {e}")))?;
    match object.try_into_blob() {
        Ok(blob) => Ok(Some(blob.data.clone())),
        // A tree (or anything else) at that path is not recorded content.
        Err(_) => Ok(None),
    }
}

fn remove_path(
    repo: &gix::Repository,
    base_tree: gix::ObjectId,
    path: &str,
) -> Result<gix::ObjectId, WorkspaceHistoryError> {
    let mut editor = repo
        .edit_tree(base_tree)
        .map_err(|e| WorkspaceHistoryError::Repo(format!("open history tree editor: {e}")))?;
    editor
        .remove(path)
        .map_err(|e| WorkspaceHistoryError::Repo(format!("remove '{path}' from history: {e}")))?;
    editor
        .write()
        .map(|id| id.detach())
        .map_err(|e| WorkspaceHistoryError::Repo(format!("write history tree: {e}")))
}

fn collect_tree_files(
    tree: &gix::Tree<'_>,
    prefix: &str,
    out: &mut BTreeMap<String, Vec<u8>>,
) -> Result<(), WorkspaceHistoryError> {
    for entry in tree.iter() {
        let entry =
            entry.map_err(|e| WorkspaceHistoryError::Repo(format!("iterate history tree: {e}")))?;
        let name = entry.filename().to_string();
        let full = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        let object = entry
            .object()
            .map_err(|e| WorkspaceHistoryError::Repo(format!("load history object: {e}")))?;
        match object.kind {
            gix::object::Kind::Tree => {
                let subtree = object.try_into_tree().map_err(|e| {
                    WorkspaceHistoryError::Repo(format!("peel history subtree: {e}"))
                })?;
                collect_tree_files(&subtree, &full, out)?;
            }
            gix::object::Kind::Blob => {
                let blob = object
                    .try_into_blob()
                    .map_err(|e| WorkspaceHistoryError::Repo(format!("peel history blob: {e}")))?;
                out.insert(full, blob.data.clone());
            }
            _ => {}
        }
    }
    Ok(())
}

fn record_ref_name(commit: &gix::ObjectId) -> String {
    format!("{RECORD_REF_PREFIX}/{commit}")
}

fn history_signature(ts: i64) -> gix::actor::Signature {
    gix::actor::Signature {
        name: HISTORY_AUTHOR_NAME.into(),
        email: HISTORY_AUTHOR_EMAIL.into(),
        time: gix::date::Time::new(ts, 0),
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn history_at(dir: &TempDir) -> WorkspaceHistory {
        WorkspaceHistory::open(WorkspaceHistoryBacking::under_project_state_dir(dir.path()))
            .expect("open history")
    }

    fn create(path: &str, after: &str) -> MutationRecord {
        MutationRecord {
            method: "mutate.create".into(),
            turn_id: "turn-1".into(),
            call_id: 1,
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
            call_id: 2,
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
        // A fresh handle stands in for a process restart.
        let reopened = history_at(&state);
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
        assert_eq!(plan.steps[0].call_id, Some(2));
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
        let signature = history_signature(1_700_000_010);
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
}

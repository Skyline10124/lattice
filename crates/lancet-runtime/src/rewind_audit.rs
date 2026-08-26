// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Append-only Rewind audit chain inside `workspace-history.git`
//! (ADR 0005 §16, #139-B Commit 3).
//!
//! Every explicit Rewind — success, structured conflict, restore/history
//! partial failure, and Session-ref failure — is recorded on the independent
//! first-parent ref for its typed SessionId. These per-session chains are
//! deliberately separate from mutation `current`:
//!
//! - filtering is structural, so another session can never consume a bounded
//!   read window;
//! - their commits are never ancestors of mutation `current`, so rewinding the
//!   workspace can never make an audit record unreachable;
//! - publication is one CAS fast-forward append under the repository write
//!   lock (never two repository locks at once);
//! - Rewind itself never moves or deletes the refs, and kernel retention never
//!   expires them;
//! - records hold hashes, paths, and object ids only — never file content.
//!
//! Ownership moved here from the transitional SQLite audit store in
//! `lancet-audit` in the same commit that deleted those reads/writes; old
//! Rewind rows are not migrated.

use gix::objs::tree::EntryKind;
use lattice_store::{
    Blocking, FlockGuard, LockMode, RefExpectation, RefTxnEdit, StoreRepo, TreeSpec,
};
use serde::{Deserialize, Serialize};

use crate::history::{HISTORY_REPO_DIR, HISTORY_STORE_KIND, HISTORY_STORE_SCHEMA_VERSION};

/// Prefix for the independent per-session Rewind audit refs.
const REWIND_AUDIT_REF_PREFIX: &str = "refs/lattice/rewind-audit/sessions";

fn session_audit_ref(session_id: lancet_protocol::SessionId) -> String {
    format!("{REWIND_AUDIT_REF_PREFIX}/{session_id}")
}

/// Tree path of the record inside each audit commit.
const RECORD_BLOB: &str = "rewind-audit.json";

/// Schema version of the recorded payload; bump on any record-shape change.
pub const REWIND_AUDIT_SCHEMA_VERSION: u32 = 3;

/// Upper bound on records returned by one bounded read.
pub const MAX_REWIND_AUDIT_READ: usize = 500;

/// Durable record of one explicit Rewind (GitHub #64, moved to
/// `lancet-runtime` by #139-B Commit 3).
///
/// Captures the target, the reversed steps, the per-path preflight
/// revisions, and the outcome — hashes, paths, and object ids only, never
/// file content. Written on success, on structured conflict, and on every
/// partial-failure path alike.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RewindAuditRecord {
    /// Payload schema version.
    pub schema_version: u32,
    /// Session the Rewind ran in.
    pub session_id: lancet_protocol::SessionId,
    /// The selected target Turn identity.
    pub target_turn_id: lancet_protocol::TurnId,
    /// Immutable Session commit selected as the Rewind target.
    pub target_commit_id: String,
    /// Workspace-history identity the Rewind moved to (`None` = empty).
    pub target_history_oid: Option<String>,
    /// Workspace-history identity the Rewind started from (`None` = empty).
    pub from_history_oid: Option<String>,
    /// `success`, `conflict`, `restore_failure`, `history_ref_failure`, or
    /// `session_ref_failure`.
    pub outcome: String,
    /// JSON array of reversed steps (commit/method/turn/call/path/hash).
    pub steps_json: String,
    /// JSON array of per-path preflight observations (revisions only).
    pub preflight_json: String,
    /// JSON array of restored paths and their post-restore revisions.
    #[serde(default)]
    pub restored_json: Option<String>,
    /// JSON array of divergent paths when the outcome is a conflict.
    #[serde(default)]
    pub conflicts_json: Option<String>,
    /// Exact `SessionHeadMove` replay payload for `success`.
    ///
    /// Kept as strict JSON because this lower layer cannot depend on `lattice-api`;
    /// the product coordinator owns deserialization and semantic validation.
    #[serde(default)]
    pub session_head_json: Option<String>,
    /// RFC 3339 timestamp supplied by the writer.
    pub recorded_at: String,
}

/// One newest-first bounded window from a session's Rewind audit chain.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RewindAuditPage {
    /// Newest records, capped by the requested and hard limits.
    pub records: Vec<RewindAuditRecord>,
    /// Whether at least one older record exists beyond `records`.
    pub has_older: bool,
}

/// Errors of the Rewind audit chain.
#[derive(Debug, thiserror::Error)]
pub enum RewindAuditError {
    /// The backing has no project store, so there is nowhere to append.
    #[error("rewind audit is disabled: the workspace history backing has no project store")]
    Disabled,
    /// Repository/kernel failure.
    #[error(transparent)]
    Repo(#[from] crate::history::WorkspaceHistoryError),
    /// The on-disk chain holds a record this build cannot interpret.
    #[error("corrupt rewind audit chain: {detail}")]
    Corruption { detail: String },
}

/// The append-only Rewind audit chain in one project's `workspace-history.git`.
///
/// A separate handle from [`crate::WorkspaceHistory`] on purpose: the audit
/// append is its own short repository transaction, taken after the Rewind's
/// workspace and Session actions, while the caller still holds the project
/// `WorkspaceOperationLock`. It never takes any operation lock itself.
pub struct RewindAuditLog {
    store: Option<StoreRepo>,
}

impl RewindAuditLog {
    /// Strictly open the audit chain for `backing`.
    ///
    /// The repository identity is the workspace-history store's; the audit
    /// chain is an independent ref inside it. A disabled backing opens as a
    /// no-op log (`append` fails typed, reads return empty) — there is no
    /// project binding to durably record against.
    pub fn open(
        backing: &crate::history::WorkspaceHistoryBacking,
    ) -> Result<Self, RewindAuditError> {
        match backing {
            crate::history::WorkspaceHistoryBacking::Disabled => Ok(Self { store: None }),
            crate::history::WorkspaceHistoryBacking::Project { layout, key } => {
                let store = StoreRepo::open_project(
                    layout,
                    key,
                    HISTORY_REPO_DIR,
                    HISTORY_STORE_KIND,
                    HISTORY_STORE_SCHEMA_VERSION,
                )
                .map_err(crate::history::WorkspaceHistoryError::from)?;
                Ok(Self { store: Some(store) })
            }
        }
    }

    /// Whether records are durably kept.
    pub fn is_enabled(&self) -> bool {
        self.store.is_some()
    }

    /// Append one record as a new fast-forward commit on its session's audit chain.
    ///
    /// One repository write lock spans read-tip, object creation, the
    /// compare-and-set, and the durability barrier. A concurrent append
    /// contender loses the CAS and fails: the caller reports the typed
    /// partial-state error rather than silently merging records. Rewind is
    /// serialized by the exclusive `WorkspaceOperationLock` the caller holds,
    /// so a loser indicates real interleaving, not noise.
    pub fn append(&self, record: &RewindAuditRecord) -> Result<(), RewindAuditError> {
        let Some(store) = self.store.as_ref() else {
            return Err(RewindAuditError::Disabled);
        };
        let mut record = record.clone();
        record.schema_version = REWIND_AUDIT_SCHEMA_VERSION;
        let ref_name = session_audit_ref(record.session_id);

        let _write = store
            .write_lock()
            .map_err(crate::history::WorkspaceHistoryError::from)?;
        let tip = store
            .ref_target(&ref_name)
            .map_err(crate::history::WorkspaceHistoryError::from)?;

        let bytes = lattice_store::canon::canonical_json_bytes(&record)
            .map_err(crate::history::WorkspaceHistoryError::from)?;
        let blob = store
            .write_blob(&bytes)
            .map_err(crate::history::WorkspaceHistoryError::from)?;
        let mut tree = TreeSpec::new();
        tree.upsert(RECORD_BLOB, EntryKind::Blob, blob);
        let tree_oid = store
            .write_tree(&tree)
            .map_err(crate::history::WorkspaceHistoryError::from)?;
        let parents: Vec<gix::ObjectId> = tip.into_iter().collect();
        let commit = store
            .write_commit_at(
                tree_oid,
                &parents,
                &format!("rewind-audit {} {}", record.outcome, record.session_id),
                0,
            )
            .map_err(crate::history::WorkspaceHistoryError::from)?;
        let expectation = match parents.first() {
            Some(parent) => RefExpectation::MustMatch(*parent),
            None => RefExpectation::MustNotExist,
        };
        store
            .ref_transaction(&[RefTxnEdit {
                name: ref_name.clone(),
                expect: expectation,
                new: Some(commit),
            }])
            .map_err(crate::history::WorkspaceHistoryError::from)?;
        store
            .fsync_pending(&[ref_name.as_str()])
            .map_err(crate::history::WorkspaceHistoryError::from)?;
        Ok(())
    }

    /// Read one bounded newest-first page from a session's private chain.
    ///
    /// The traversal reads at most `limit + 1` commits (after applying the hard
    /// ceiling). The extra record supplies [`RewindAuditPage::has_older`]; no
    /// capped traversal is ever presented as an exact chain length.
    pub fn records_for_session(
        &self,
        session_id: &lancet_protocol::SessionId,
        limit: usize,
    ) -> Result<RewindAuditPage, RewindAuditError> {
        let Some(store) = self.store.as_ref() else {
            return Ok(RewindAuditPage::default());
        };
        let limit = limit.min(MAX_REWIND_AUDIT_READ);
        let read_limit = limit.saturating_add(1);
        let ref_name = session_audit_ref(*session_id);
        let mut records = Vec::with_capacity(read_limit);
        let mut cursor = store
            .ref_target(&ref_name)
            .map_err(crate::history::WorkspaceHistoryError::from)?;
        while records.len() < read_limit
            && let Some(oid) = cursor
        {
            let tree = store
                .commit_tree(oid)
                .map_err(crate::history::WorkspaceHistoryError::from)?;
            let blob = store
                .read_tree_path(tree, RECORD_BLOB)
                .map_err(crate::history::WorkspaceHistoryError::from)?
                .ok_or_else(|| RewindAuditError::Corruption {
                    detail: format!("audit commit {oid} carries no {RECORD_BLOB}"),
                })?;
            let record: RewindAuditRecord =
                serde_json::from_slice(&blob).map_err(|err| RewindAuditError::Corruption {
                    detail: format!("audit commit {oid} carries a malformed record: {err}"),
                })?;
            if record.schema_version != REWIND_AUDIT_SCHEMA_VERSION {
                return Err(RewindAuditError::Corruption {
                    detail: format!(
                        "audit commit {oid} has schema version {} != {REWIND_AUDIT_SCHEMA_VERSION}",
                        record.schema_version
                    ),
                });
            }
            if record.session_id != *session_id {
                return Err(RewindAuditError::Corruption {
                    detail: format!(
                        "audit commit {oid} records session {} but ref {ref_name} belongs to {session_id}",
                        record.session_id
                    ),
                });
            }
            let parents = store
                .commit_parents(oid)
                .map_err(crate::history::WorkspaceHistoryError::from)?;
            cursor = match parents.as_slice() {
                [] => None,
                [parent] => Some(*parent),
                _ => {
                    return Err(RewindAuditError::Corruption {
                        detail: format!(
                            "audit commit {oid} has {} parents; expected a first-parent chain",
                            parents.len()
                        ),
                    });
                }
            };
            records.push(record);
        }
        let has_older = records.len() > limit;
        records.truncate(limit);
        Ok(RewindAuditPage { records, has_older })
    }
}

/// Acquire the per-project `WorkspaceOperationLock` for a Rewind (#139-B
/// Commit 3: ownership lives in the product-level `RewindCoordinator`, which
/// passes the held guard down; lower layers never reacquire it).
///
/// Returns `Ok(None)` when the backing is disabled: there is no project
/// binding to serialize against.
pub fn lock_rewind_operation(
    backing: &crate::history::WorkspaceHistoryBacking,
    mode: LockMode,
    blocking: Blocking,
) -> Result<Option<FlockGuard>, RewindAuditError> {
    let Some(path) = backing.operation_lock_path() else {
        return Ok(None);
    };
    let guard = lattice_store::acquire_flock(&path, mode, blocking)
        .map_err(crate::history::WorkspaceHistoryError::from)?;
    Ok(Some(guard))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::WorkspaceHistoryBacking;
    use lattice_store::{ProjectStoreKey, StateLayout};
    use sha2::Digest as _;
    use tempfile::TempDir;

    fn lattice_store_hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn backing() -> (TempDir, WorkspaceHistoryBacking) {
        let tmp = TempDir::new().unwrap();
        let key = ProjectStoreKey::derive(tmp.path()).unwrap();
        let backing = WorkspaceHistoryBacking::for_key(StateLayout::in_state_root(tmp.path()), key);
        // First-create the store the way the composition entry does, so open
        // is strict.
        crate::history::WorkspaceHistory::create(backing.clone()).unwrap();
        (tmp, backing)
    }

    fn session(index: u128) -> lancet_protocol::SessionId {
        lancet_protocol::SessionId::parse(&format!("{index:032x}")).expect("session id")
    }

    fn target_turn() -> lancet_protocol::TurnId {
        lancet_protocol::TurnId::parse("10000000-0000-4000-8000-000000000001").expect("turn id")
    }

    fn record(outcome: &str, session_id: lancet_protocol::SessionId) -> RewindAuditRecord {
        RewindAuditRecord {
            schema_version: REWIND_AUDIT_SCHEMA_VERSION,
            session_id,
            target_turn_id: target_turn(),
            target_commit_id: "b".repeat(40),
            target_history_oid: None,
            from_history_oid: Some("a".repeat(40)),
            outcome: outcome.to_string(),
            steps_json: "[]".to_string(),
            preflight_json: "[]".to_string(),
            restored_json: None,
            conflicts_json: None,
            session_head_json: None,
            recorded_at: "2026-08-23T12:00:00Z".to_string(),
        }
    }

    #[test]
    fn disabled_backing_fails_append_typed_and_reads_empty() {
        let log = RewindAuditLog::open(&WorkspaceHistoryBacking::Disabled).unwrap();
        assert!(!log.is_enabled());
        assert!(matches!(
            log.append(&record("success", session(1))),
            Err(RewindAuditError::Disabled)
        ));
        assert_eq!(
            log.records_for_session(&session(1), 10).unwrap(),
            RewindAuditPage::default()
        );
    }

    #[test]
    fn append_builds_per_session_fast_forward_chains_that_survive_reopen() {
        let (_tmp, backing) = backing();
        let log = RewindAuditLog::open(&backing).unwrap();
        log.append(&record("conflict", session(1))).unwrap();
        log.append(&record("success", session(1))).unwrap();
        log.append(&record("success", session(2))).unwrap();

        let reopened = RewindAuditLog::open(&backing).unwrap();
        let first = reopened.records_for_session(&session(1), 10).unwrap();
        assert_eq!(first.records.len(), 2);
        assert!(!first.has_older);
        assert_eq!(first.records[0].outcome, "success");
        assert_eq!(first.records[1].outcome, "conflict");
        assert!(
            first
                .records
                .iter()
                .all(|record| record.session_id == session(1))
        );

        let second = reopened.records_for_session(&session(2), 10).unwrap();
        assert_eq!(second.records.len(), 1);
        assert!(!second.has_older);
        assert_eq!(second.records[0].session_id, session(2));
    }

    #[test]
    fn same_session_page_reports_when_older_records_exist() {
        let (_tmp, backing) = backing();
        let log = RewindAuditLog::open(&backing).unwrap();
        for _ in 0..5 {
            log.append(&record("success", session(1))).unwrap();
        }

        let page = log.records_for_session(&session(1), 3).unwrap();
        assert_eq!(page.records.len(), 3);
        assert!(page.has_older);
        let complete = log.records_for_session(&session(1), 5).unwrap();
        assert_eq!(complete.records.len(), 5);
        assert!(!complete.has_older);
    }

    #[test]
    fn newer_foreign_session_records_cannot_displace_an_older_session_page() {
        let (_tmp, backing) = backing();
        let log = RewindAuditLog::open(&backing).unwrap();
        log.append(&record("target", session(1))).unwrap();
        for index in 2..8 {
            log.append(&record("foreign", session(index))).unwrap();
        }

        let page = log.records_for_session(&session(1), 3).unwrap();
        assert_eq!(page.records.len(), 1);
        assert_eq!(page.records[0].outcome, "target");
        assert!(!page.has_older);
    }

    #[test]
    fn audit_chain_is_not_part_of_the_mutation_current_chain() {
        let (tmp, backing) = backing();
        let history = crate::history::WorkspaceHistory::open(backing.clone()).unwrap();
        history
            .record(&crate::history::MutationRecord {
                turn_id: lancet_protocol::TurnId::parse("10000000-0000-4000-8000-000000000001")
                    .expect("turn id"),
                invocation_id: lancet_protocol::InvocationId::parse(
                    "00000000-0000-4000-8000-000000000001",
                )
                .expect("invocation id"),
                method: "mutate.create".to_string(),
                path: "file.txt".to_string(),
                before: None,
                after: b"content".to_vec(),
                content_hash: lattice_store_hex(&sha2::Sha256::digest(b"content")),
                recorded_at: 0,
            })
            .unwrap();
        let log = RewindAuditLog::open(&backing).unwrap();
        log.append(&record("success", session(1))).unwrap();

        let store = StoreRepo::open_project(
            &StateLayout::in_state_root(tmp.path()),
            &ProjectStoreKey::derive(tmp.path()).unwrap(),
            HISTORY_REPO_DIR,
            HISTORY_STORE_KIND,
            HISTORY_STORE_SCHEMA_VERSION,
        )
        .unwrap();
        let mutation_tip = store.ref_target(crate::history::CURRENT_REF).unwrap();
        let audit_tip = store
            .ref_target(&session_audit_ref(session(1)))
            .unwrap()
            .unwrap();
        // The audit commit is not the mutation tip and not its ancestor: the
        // chains only share the empty root at most.
        let walk = store.rev_walk(&[mutation_tip.unwrap()], &[]).unwrap();
        assert!(!walk.contains(&audit_tip));
    }

    #[test]
    fn tampered_payload_fails_closed_on_read() {
        let (_tmp, backing) = backing();
        let log = RewindAuditLog::open(&backing).unwrap();
        log.append(&record("success", session(1))).unwrap();

        // Overwrite the record blob with a future-schema payload by forging
        // a direct commit through the same kernel; the read path must fail
        // closed on the schema mismatch.
        let store = log.store.as_ref().unwrap();
        let ref_name = session_audit_ref(session(1));
        let tip = store.ref_target(&ref_name).unwrap().unwrap();
        let tree = store.commit_tree(tip).unwrap();
        let blob = store.read_tree_path(tree, RECORD_BLOB).unwrap().unwrap();
        let mut payload: serde_json::Value = serde_json::from_slice(&blob).unwrap();
        payload["schema_version"] = serde_json::Value::from(99u32);
        let bytes = serde_json::to_vec(&payload).unwrap();
        let new_blob = store.write_blob(&bytes).unwrap();
        let mut spec = TreeSpec::new();
        spec.upsert(RECORD_BLOB, EntryKind::Blob, new_blob);
        let new_tree = store.write_tree(&spec).unwrap();
        let commit = store
            .write_commit_at(new_tree, &[tip], "forged", 0)
            .unwrap();
        store
            .ref_transaction(&[RefTxnEdit {
                name: ref_name,
                expect: RefExpectation::MustMatch(tip),
                new: Some(commit),
            }])
            .unwrap();

        let reopened = RewindAuditLog::open(&backing).unwrap();
        assert!(matches!(
            reopened.records_for_session(&session(1), 10),
            Err(RewindAuditError::Corruption { .. })
        ));
    }

    #[test]
    fn malformed_record_blob_fails_closed_on_read() {
        let (_tmp, backing) = backing();
        let log = RewindAuditLog::open(&backing).unwrap();
        log.append(&record("success", session(1))).unwrap();
        let store = log.store.as_ref().unwrap();
        let ref_name = session_audit_ref(session(1));
        let tip = store.ref_target(&ref_name).unwrap().unwrap();
        let blob = store.write_blob(b"not json").unwrap();
        let mut tree = TreeSpec::new();
        tree.upsert(RECORD_BLOB, EntryKind::Blob, blob);
        let tree = store.write_tree(&tree).unwrap();
        let forged = store.write_commit_at(tree, &[tip], "forged", 0).unwrap();
        store
            .ref_transaction(&[RefTxnEdit {
                name: ref_name,
                expect: RefExpectation::MustMatch(tip),
                new: Some(forged),
            }])
            .unwrap();

        assert!(matches!(
            log.records_for_session(&session(1), 10),
            Err(RewindAuditError::Corruption { .. })
        ));
    }

    #[test]
    fn record_and_ref_session_mismatch_fails_closed() {
        let (_tmp, backing) = backing();
        let log = RewindAuditLog::open(&backing).unwrap();
        log.append(&record("first", session(1))).unwrap();
        log.append(&record("foreign", session(2))).unwrap();
        let store = log.store.as_ref().unwrap();
        let first_ref = session_audit_ref(session(1));
        let first_tip = store.ref_target(&first_ref).unwrap().unwrap();
        let foreign_tip = store
            .ref_target(&session_audit_ref(session(2)))
            .unwrap()
            .unwrap();
        store
            .ref_transaction(&[RefTxnEdit {
                name: first_ref,
                expect: RefExpectation::MustMatch(first_tip),
                new: Some(foreign_tip),
            }])
            .unwrap();

        assert!(matches!(
            log.records_for_session(&session(1), 10),
            Err(RewindAuditError::Corruption { .. })
        ));
    }

    #[test]
    fn merge_commit_on_session_chain_fails_closed() {
        let (_tmp, backing) = backing();
        let log = RewindAuditLog::open(&backing).unwrap();
        log.append(&record("root", session(1))).unwrap();
        log.append(&record("tip", session(1))).unwrap();
        let store = log.store.as_ref().unwrap();
        let ref_name = session_audit_ref(session(1));
        let tip = store.ref_target(&ref_name).unwrap().unwrap();
        let parent = store.commit_parents(tip).unwrap()[0];
        let tree = store.commit_tree(tip).unwrap();
        let merge = store
            .write_commit_at(tree, &[tip, parent], "forged merge", 0)
            .unwrap();
        store
            .ref_transaction(&[RefTxnEdit {
                name: ref_name,
                expect: RefExpectation::MustMatch(tip),
                new: Some(merge),
            }])
            .unwrap();

        assert!(matches!(
            log.records_for_session(&session(1), 10),
            Err(RewindAuditError::Corruption { .. })
        ));
    }

    #[test]
    fn operation_lock_acquire_matches_history_lock_path() {
        let (_tmp, backing) = backing();
        let history = crate::history::WorkspaceHistory::open(backing.clone()).unwrap();
        let guard = lock_rewind_operation(&backing, LockMode::Exclusive, Blocking::Block)
            .unwrap()
            .expect("project backing locks");
        // The lock is the history's own operation lock: the same path is
        // already held exclusively, so a second exclusive acquisition by the
        // history handle must not succeed while we hold it.
        let contended =
            history.lock_workspace_operation(LockMode::Exclusive, Blocking::NonBlocking);
        assert!(
            contended.is_err(),
            "rewind lock must serialize with mutation lock"
        );
        drop(guard);
        assert!(
            history
                .lock_workspace_operation(LockMode::Exclusive, Blocking::NonBlocking)
                .is_ok()
        );
    }
}

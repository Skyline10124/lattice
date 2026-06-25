// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Snapshot journal crash-safety tests.
//!
//! Covers:
//! - `journal_*`: WAL create/append/fsync/parse, torn-line tolerance.
//! - `journal_recovery_*`: crash-window convergence at each fault point.
//! - `journal_orphan_*`: no-commit snapshot removal.
//! - `journal_phantom_*`: missing committed snapshot marks operational loss.
//! - `resource_matrix_*`: retain/release/destroy/none per disposition.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use lattice_api::turn::snapshot_journal::{
    SnapshotJournal, SnapshotJournalRecord, SnapshotJournalState, SnapshotReconciler,
    parse_journal, reconcile_pending_snapshots,
};
use lattice_api::{
    LifecycleFailureKind, ResourceAction, TurnLifecycleOutcome, classify_lifecycle_failure,
    commit_promoted_with_retry, finalize_turn, outcome_promoted, outcome_workspace_less,
};
use lattice_session::{TurnCommitStatus, WorkspaceDisposition};

// ── Helpers ──────────────────────────────────────────────────────────────────

fn test_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("snapshot-journal-tests")
        .join(name);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn intent_record(session_id: &str, commit_id: &str, snapshot_name: &str) -> SnapshotJournalRecord {
    SnapshotJournalRecord::Intent {
        session_id: session_id.to_string(),
        prepared_commit_id: commit_id.to_string(),
        snapshot_name: snapshot_name.to_string(),
    }
}

fn retained_record(
    session_id: &str,
    commit_id: &str,
    snapshot_name: &str,
) -> SnapshotJournalRecord {
    SnapshotJournalRecord::Retained {
        session_id: session_id.to_string(),
        prepared_commit_id: commit_id.to_string(),
        snapshot_name: snapshot_name.to_string(),
    }
}

fn committed_record(
    session_id: &str,
    commit_id: &str,
    snapshot_name: &str,
) -> SnapshotJournalRecord {
    SnapshotJournalRecord::SessionCommitted {
        session_id: session_id.to_string(),
        commit_id: commit_id.to_string(),
        snapshot_name: snapshot_name.to_string(),
    }
}

/// In-memory reconciler for testing.
struct TestReconciler {
    snapshots: HashMap<String, bool>,
    commits: HashMap<String, bool>,
    released: Vec<String>,
    lost: Vec<String>,
}

impl TestReconciler {
    fn new() -> Self {
        Self {
            snapshots: HashMap::new(),
            commits: HashMap::new(),
            released: Vec::new(),
            lost: Vec::new(),
        }
    }
}

impl SnapshotReconciler for TestReconciler {
    fn snapshot_exists(&self, name: &str) -> Result<bool, String> {
        Ok(self.snapshots.get(name).copied().unwrap_or(false))
    }

    fn release_snapshot(&mut self, name: &str) -> Result<(), String> {
        self.snapshots.remove(name);
        self.released.push(name.to_string());
        Ok(())
    }

    fn commit_exists(&self, _session_id: &str, commit_id: &str) -> Result<bool, String> {
        Ok(self.commits.get(commit_id).copied().unwrap_or(false))
    }

    fn set_snapshot_lost(&mut self, commit_id: &str) -> Result<(), String> {
        self.lost.push(commit_id.to_string());
        Ok(())
    }
}

/// Commit function that returns the prepared_commit_id, mirroring production
/// where the caller pre-generates the commit UUID and the DB commit uses it.
fn success_commit(_outcome: &TurnLifecycleOutcome, _snap: &str) -> Result<String, String> {
    Ok("prepared-commit-1".to_string())
}

const PREPARED_CID: &str = "prepared-commit-1";

// ── Journal CRUD tests ───────────────────────────────────────────────────────

#[test]
fn journal_create_writes_header_and_is_parseable() {
    let dir = test_dir("journal_create");
    let journal = SnapshotJournal::create_new(&dir).expect("create");
    drop(journal);

    let parsed = parse_journal(&SnapshotJournal::journal_path(&dir)).expect("parse");
    assert_eq!(parsed.records.len(), 1);
    assert!(matches!(
        parsed.records[0],
        SnapshotJournalRecord::Header {
            ref journal_id,
            ..
        } if journal_id == "pending-snapshots"
    ));
}

#[test]
fn journal_append_intent_retained_committed() {
    let dir = test_dir("journal_append");
    let mut journal = SnapshotJournal::create_new(&dir).expect("create");

    journal
        .append_record(intent_record("sess1", "commit1", "snap1"))
        .expect("append intent");
    journal
        .append_record(retained_record("sess1", "commit1", "snap1"))
        .expect("append retained");
    journal
        .append_record(committed_record("sess1", "commit1", "snap1"))
        .expect("append committed");

    drop(journal);
    let parsed = parse_journal(&SnapshotJournal::journal_path(&dir)).expect("parse");
    // Header + 3 records
    assert_eq!(parsed.records.len(), 4);
}

#[test]
fn journal_torn_final_line_is_tolerated() {
    let dir = test_dir("journal_torn");
    let path = SnapshotJournal::journal_path(&dir);

    // Write two complete records + a partial line (simulating crash mid-write)
    let r1 = serde_json::to_string(&SnapshotJournalRecord::Header {
        journal_id: "pending-snapshots".to_string(),
    })
    .unwrap();
    let r2 = serde_json::to_string(&intent_record("s", "c", "sn")).unwrap();
    let content = format!("{r1}\n{r2}\n{{\"partial_line");
    fs::write(&path, content).unwrap();

    let parsed = parse_journal(&path).expect("parse with torn line");
    assert!(parsed.torn_final_line);
    assert_eq!(parsed.records.len(), 2); // Header + Intent
}

#[test]
fn journal_durable_remove_deletes_file() {
    let dir = test_dir("journal_remove");
    let mut journal = SnapshotJournal::create_new(&dir).expect("create");
    journal
        .append_record(intent_record("s", "c", "sn"))
        .expect("append");

    let path = SnapshotJournal::journal_path(&dir);
    assert!(path.exists());

    journal.remove_durable().expect("remove");
    assert!(!path.exists());
}

#[test]
fn journal_exists_returns_false_when_absent() {
    let dir = test_dir("journal_exists_absent");
    assert!(!SnapshotJournal::exists(&dir));
}

#[test]
fn journal_exists_returns_true_after_create() {
    let dir = test_dir("journal_exists_true");
    let _journal = SnapshotJournal::create_new(&dir).expect("create");
    assert!(SnapshotJournal::exists(&dir));
}

// ── Fault injection tests ───────────────────────────────────────────────────

#[test]
fn journal_fault_after_intent_converges() {
    let dir = test_dir("fault_intent");
    let mut journal = SnapshotJournal::create_new(&dir).expect("create");

    journal
        .append_record(intent_record("sess1", "commit1", "snap1"))
        .expect("append intent");

    // Simulate crash after intent
    drop(journal);

    // Parse and verify state
    let parsed = parse_journal(&SnapshotJournal::journal_path(&dir)).expect("parse");
    let states = parsed.entry_states();
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].state, SnapshotJournalState::Intent);
}

#[test]
fn journal_fault_after_retained_converges() {
    let dir = test_dir("fault_retained");
    let mut journal = SnapshotJournal::create_new(&dir).expect("create");

    journal
        .append_record(intent_record("sess1", "commit1", "snap1"))
        .expect("append intent");
    journal
        .append_record(retained_record("sess1", "commit1", "snap1"))
        .expect("append retained");

    // Simulate crash after retained
    drop(journal);

    let parsed = parse_journal(&SnapshotJournal::journal_path(&dir)).expect("parse");
    let states = parsed.entry_states();
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].state, SnapshotJournalState::Retained);
}

#[test]
fn journal_fault_after_committed_converges() {
    let dir = test_dir("fault_committed");
    let mut journal = SnapshotJournal::create_new(&dir).expect("create");

    journal
        .append_record(intent_record("sess1", "commit1", "snap1"))
        .expect("append intent");
    journal
        .append_record(retained_record("sess1", "commit1", "snap1"))
        .expect("append retained");
    journal
        .append_record(committed_record("sess1", "commit1", "snap1"))
        .expect("append committed");

    // Simulate crash after committed (journal not yet removed)
    drop(journal);

    let parsed = parse_journal(&SnapshotJournal::journal_path(&dir)).expect("parse");
    let states = parsed.entry_states();
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].state, SnapshotJournalState::SessionCommitted);
}

// ── Reconciliation tests ────────────────────────────────────────────────────

#[test]
fn journal_orphan_snapshot_is_released() {
    let dir = test_dir("orphan_release");
    let mut journal = SnapshotJournal::create_new(&dir).expect("create");
    journal
        .append_record(intent_record("s1", "c1", "snap1"))
        .expect("intent");
    journal
        .append_record(retained_record("s1", "c1", "snap1"))
        .expect("retained");
    drop(journal);

    let mut reconciler = TestReconciler::new();
    reconciler.snapshots.insert("snap1".to_string(), true);
    // No commit exists

    let result = reconcile_pending_snapshots(&dir, &mut reconciler).expect("reconcile");
    assert_eq!(result.orphans_released, vec!["snap1".to_string()]);
    assert!(result.phantoms_marked_lost.is_empty());
    assert!(result.stale_cleared.is_empty());
    assert!(!reconciler.snapshots.contains_key("snap1"));
}

#[test]
fn journal_phantom_snapshot_marks_lost() {
    let dir = test_dir("phantom_lost");
    let mut journal = SnapshotJournal::create_new(&dir).expect("create");
    journal
        .append_record(intent_record("s1", "c1", "snap1"))
        .expect("intent");
    journal
        .append_record(retained_record("s1", "c1", "snap1"))
        .expect("retained");
    journal
        .append_record(committed_record("s1", "c1", "snap1"))
        .expect("committed");
    drop(journal);

    let mut reconciler = TestReconciler::new();
    reconciler.commits.insert("c1".to_string(), true);
    // Snapshot does NOT exist

    let result = reconcile_pending_snapshots(&dir, &mut reconciler).expect("reconcile");
    assert!(result.orphans_released.is_empty());
    assert_eq!(result.phantoms_marked_lost, vec!["c1".to_string()]);
    assert!(result.stale_cleared.is_empty());
    assert_eq!(reconciler.lost, vec!["c1".to_string()]);
}

#[test]
fn journal_stale_completed_journal_is_cleared() {
    let dir = test_dir("stale_clear");
    let mut journal = SnapshotJournal::create_new(&dir).expect("create");
    journal
        .append_record(intent_record("s1", "c1", "snap1"))
        .expect("intent");
    journal
        .append_record(retained_record("s1", "c1", "snap1"))
        .expect("retained");
    journal
        .append_record(committed_record("s1", "c1", "snap1"))
        .expect("committed");
    drop(journal);

    let mut reconciler = TestReconciler::new();
    reconciler.commits.insert("c1".to_string(), true);
    reconciler.snapshots.insert("snap1".to_string(), true);

    let result = reconcile_pending_snapshots(&dir, &mut reconciler).expect("reconcile");
    assert!(result.orphans_released.is_empty());
    assert!(result.phantoms_marked_lost.is_empty());
    assert_eq!(result.stale_cleared, vec!["snap1".to_string()]);
    // Journal file should be removed
    assert!(!SnapshotJournal::journal_path(&dir).exists());
}

#[test]
fn journal_reconcile_empty_journal_dir_succeeds() {
    let dir = test_dir("reconcile_empty");
    let mut reconciler = TestReconciler::new();
    let result = reconcile_pending_snapshots(&dir, &mut reconciler).expect("reconcile");
    assert!(result.orphans_released.is_empty());
    assert!(result.phantoms_marked_lost.is_empty());
    assert!(result.stale_cleared.is_empty());
}

#[test]
fn journal_intent_only_orphan_releases_existing_snapshot() {
    // If only an Intent record exists but a snapshot was partially created,
    // reconciliation should release it.
    let dir = test_dir("intent_only_orphan");
    let mut journal = SnapshotJournal::create_new(&dir).expect("create");
    journal
        .append_record(intent_record("s1", "c1", "snap1"))
        .expect("intent");
    drop(journal);

    let mut reconciler = TestReconciler::new();
    reconciler.snapshots.insert("snap1".to_string(), true);

    let result = reconcile_pending_snapshots(&dir, &mut reconciler).expect("reconcile");
    assert_eq!(result.orphans_released, vec!["snap1".to_string()]);
    assert!(!reconciler.snapshots.contains_key("snap1"));
}

#[test]
fn journal_multiple_entries_reconcile_independently() {
    let dir = test_dir("multi_entry");
    let mut journal = SnapshotJournal::create_new(&dir).expect("create");
    // Entry 1: completed (should be cleared)
    journal
        .append_record(intent_record("s1", "c1", "snap1"))
        .expect("intent1");
    journal
        .append_record(retained_record("s1", "c1", "snap1"))
        .expect("retained1");
    journal
        .append_record(committed_record("s1", "c1", "snap1"))
        .expect("committed1");
    // Entry 2: retained but no commit (orphan)
    journal
        .append_record(intent_record("s2", "c2", "snap2"))
        .expect("intent2");
    journal
        .append_record(retained_record("s2", "c2", "snap2"))
        .expect("retained2");
    drop(journal);

    let mut reconciler = TestReconciler::new();
    reconciler.snapshots.insert("snap1".to_string(), true);
    reconciler.snapshots.insert("snap2".to_string(), true);
    reconciler.commits.insert("c1".to_string(), true);

    let result = reconcile_pending_snapshots(&dir, &mut reconciler).expect("reconcile");
    assert!(result.stale_cleared.contains(&"snap1".to_string()));
    assert!(result.orphans_released.contains(&"snap2".to_string()));
}

// ── Parser edge cases ────────────────────────────────────────────────────────

#[test]
fn journal_parser_rejects_empty_file() {
    let dir = test_dir("parser_empty");
    let path = SnapshotJournal::journal_path(&dir);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "").unwrap();
    let err = parse_journal(&path).unwrap_err();
    assert!(err.contains("empty"));
}

#[test]
fn journal_parser_rejects_missing_header() {
    let dir = test_dir("parser_no_header");
    let path = SnapshotJournal::journal_path(&dir);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let r = serde_json::to_string(&intent_record("s", "c", "sn")).unwrap();
    fs::write(&path, format!("{r}\n")).unwrap();
    let err = parse_journal(&path).unwrap_err();
    assert!(err.contains("header") || err.contains("Header"));
}

// ── Resource matrix tests (finalization) ────────────────────────────────

#[test]
fn resource_matrix_pending_worktree_retains_snapshot() {
    let dir = test_dir("rm_pending_worktree");
    let mut journal = SnapshotJournal::create_new(&dir).unwrap();
    let mut reconciler = TestReconciler::new();

    let outcome = classify_lifecycle_failure(
        TurnCommitStatus::Completed,
        true,
        false,
        LifecycleFailureKind::CleanupExhaustion,
    );
    assert_eq!(outcome.disposition, WorkspaceDisposition::PendingWorktree);
    assert_eq!(outcome.resource_action, ResourceAction::Retain);

    let cid = finalize_turn(
        &outcome,
        "sess1",
        "snap-wt",
        PREPARED_CID,
        &mut journal,
        &mut reconciler,
        success_commit,
    )
    .unwrap();

    assert_eq!(cid, PREPARED_CID);

    // Journal has intent → retained → committed
    let parsed = parse_journal(&SnapshotJournal::journal_path(&dir)).unwrap();
    let states = parsed.entry_states();
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].state, SnapshotJournalState::SessionCommitted);
}

#[test]
fn resource_matrix_pending_verification_retains_snapshot() {
    let dir = test_dir("rm_pending_verify");
    let mut journal = SnapshotJournal::create_new(&dir).unwrap();
    let mut reconciler = TestReconciler::new();

    let outcome = classify_lifecycle_failure(
        TurnCommitStatus::Completed,
        true,
        false,
        LifecycleFailureKind::MissingVerifyConfig,
    );
    assert_eq!(
        outcome.disposition,
        WorkspaceDisposition::PendingVerification
    );
    assert_eq!(outcome.resource_action, ResourceAction::Retain);

    let cid = finalize_turn(
        &outcome,
        "sess1",
        "snap-pv",
        PREPARED_CID,
        &mut journal,
        &mut reconciler,
        success_commit,
    )
    .unwrap();

    assert_eq!(cid, PREPARED_CID);
    let parsed = parse_journal(&SnapshotJournal::journal_path(&dir)).unwrap();
    let states = parsed.entry_states();
    assert_eq!(states[0].state, SnapshotJournalState::SessionCommitted);
}

#[test]
fn resource_matrix_pending_conflict_no_snapshot() {
    let dir = test_dir("rm_pending_conflict");
    let mut journal = SnapshotJournal::create_new(&dir).unwrap();
    let mut reconciler = TestReconciler::new();

    let outcome = classify_lifecycle_failure(
        TurnCommitStatus::Completed,
        true,
        false,
        LifecycleFailureKind::BaselineMovement,
    );
    assert_eq!(outcome.disposition, WorkspaceDisposition::PendingConflict);
    assert_eq!(outcome.resource_action, ResourceAction::Destroy);

    let cid = finalize_turn(
        &outcome,
        "sess1",
        "snap-pc",
        PREPARED_CID,
        &mut journal,
        &mut reconciler,
        success_commit,
    )
    .unwrap();

    assert_eq!(cid, PREPARED_CID);
    // No journal entries for None action
    let parsed = parse_journal(&SnapshotJournal::journal_path(&dir)).unwrap();
    let states = parsed.entry_states();
    assert!(states.is_empty());
}

#[test]
fn resource_matrix_pending_promotion_releases_after_pin() {
    let dir = test_dir("rm_pending_promotion");
    let mut journal = SnapshotJournal::create_new(&dir).unwrap();
    let mut reconciler = TestReconciler::new();
    reconciler.snapshots.insert("snap-pp".to_string(), true);

    let outcome = classify_lifecycle_failure(
        TurnCommitStatus::Completed,
        true,
        true,
        LifecycleFailureKind::PinnedTemporarilyBlocked,
    );
    assert_eq!(outcome.disposition, WorkspaceDisposition::PendingPromotion);
    assert_eq!(
        outcome.resource_action,
        ResourceAction::ReleaseAfterCandidatePin
    );

    let cid = finalize_turn(
        &outcome,
        "sess1",
        "snap-pp",
        PREPARED_CID,
        &mut journal,
        &mut reconciler,
        success_commit,
    )
    .unwrap();

    assert_eq!(cid, PREPARED_CID);
    // Snapshot released
    assert!(reconciler.released.contains(&"snap-pp".to_string()));
}

#[test]
fn resource_matrix_promoted_releases_snapshot() {
    let dir = test_dir("rm_promoted");
    let mut journal = SnapshotJournal::create_new(&dir).unwrap();
    let mut reconciler = TestReconciler::new();
    reconciler.snapshots.insert("snap-promo".to_string(), true);

    let outcome = outcome_promoted("ref-oid-123".to_string(), true);
    assert_eq!(outcome.disposition, WorkspaceDisposition::Promoted);
    assert_eq!(
        outcome.resource_action,
        ResourceAction::ReleaseAfterCandidatePin
    );

    let cid = finalize_turn(
        &outcome,
        "sess1",
        "snap-promo",
        PREPARED_CID,
        &mut journal,
        &mut reconciler,
        success_commit,
    )
    .unwrap();

    assert_eq!(cid, PREPARED_CID);
    assert!(reconciler.released.contains(&"snap-promo".to_string()));
}

#[test]
fn resource_matrix_rejected_destroys() {
    let dir = test_dir("rm_rejected");
    let mut journal = SnapshotJournal::create_new(&dir).unwrap();
    let mut reconciler = TestReconciler::new();

    let outcome = classify_lifecycle_failure(
        TurnCommitStatus::Error,
        false,
        false,
        LifecycleFailureKind::ExportRejected {
            reason: "test export failure".to_string(),
        },
    );
    assert_eq!(outcome.disposition, WorkspaceDisposition::Rejected);
    assert_eq!(outcome.resource_action, ResourceAction::Destroy);

    let cid = finalize_turn(
        &outcome,
        "sess1",
        "snap-rej",
        PREPARED_CID,
        &mut journal,
        &mut reconciler,
        success_commit,
    )
    .unwrap();

    assert_eq!(cid, PREPARED_CID);
    // No journal entries for Destroy (no snapshot retained)
    let parsed = parse_journal(&SnapshotJournal::journal_path(&dir)).unwrap();
    let states = parsed.entry_states();
    assert!(states.is_empty());
}

#[test]
fn resource_matrix_aborted_destroys() {
    let dir = test_dir("rm_aborted");
    let mut journal = SnapshotJournal::create_new(&dir).unwrap();
    let mut reconciler = TestReconciler::new();

    let outcome = classify_lifecycle_failure(
        TurnCommitStatus::Error,
        false,
        false,
        LifecycleFailureKind::AgentErrorBeforeCandidate,
    );
    assert_eq!(outcome.disposition, WorkspaceDisposition::Aborted);
    assert_eq!(outcome.resource_action, ResourceAction::Destroy);

    let cid = finalize_turn(
        &outcome,
        "sess1",
        "snap-abort",
        PREPARED_CID,
        &mut journal,
        &mut reconciler,
        success_commit,
    )
    .unwrap();

    assert_eq!(cid, PREPARED_CID);
}

#[test]
fn resource_matrix_workspace_less_destroy() {
    let dir = test_dir("rm_workspace_less");
    let mut journal = SnapshotJournal::create_new(&dir).unwrap();
    let mut reconciler = TestReconciler::new();

    let outcome = outcome_workspace_less();
    assert_eq!(outcome.disposition, WorkspaceDisposition::NotApplicable);
    assert_eq!(outcome.resource_action, ResourceAction::Destroy);

    let cid = finalize_turn(
        &outcome,
        "sess1",
        "",
        PREPARED_CID,
        &mut journal,
        &mut reconciler,
        success_commit,
    )
    .unwrap();

    assert_eq!(cid, PREPARED_CID);
}

#[test]
fn resource_matrix_promoted_retry_first_failure_succeeds() {
    let outcome = outcome_promoted("ref-oid-retry".to_string(), true);
    let mut attempts = 0;
    let cid = commit_promoted_with_retry(&outcome, "snap-retry", |_, _| {
        attempts += 1;
        if attempts == 1 {
            Err("simulated first failure".to_string())
        } else {
            Ok("commit-promoted-retry".to_string())
        }
    })
    .unwrap();

    assert_eq!(cid, "commit-promoted-retry");
    assert_eq!(attempts, 2); // One retry
}

#[test]
fn resource_matrix_promoted_retry_both_failures_returns_error() {
    let outcome = outcome_promoted("ref-oid-fail".to_string(), true);
    let mut attempts = 0;
    let result = commit_promoted_with_retry(&outcome, "snap-fail", |_, _| {
        attempts += 1;
        Err(format!("failure #{attempts}"))
    });

    assert!(result.is_err());
    assert_eq!(attempts, 2); // Retried once
    let err = result.unwrap_err();
    assert!(err.contains("failure #1"));
    assert!(err.contains("failure #2"));
}

#[test]
fn resource_matrix_promoted_retry_uses_same_commit_id() {
    // Verify that both retry attempts use the same commit_fn, producing the
    // same logical commit identity.
    let outcome = outcome_promoted("ref-oid-same".to_string(), true);
    let mut commit_ids_seen = Vec::new();
    let mut attempts = 0;
    let cid = commit_promoted_with_retry(&outcome, "snap-same", |outcome, _snap| {
        attempts += 1;
        let id = format!("stable-{}", outcome.disposition.as_str());
        commit_ids_seen.push(id.clone());
        if attempts == 1 {
            Err("first fail".to_string())
        } else {
            Ok(id)
        }
    })
    .unwrap();

    assert_eq!(cid, "stable-promoted");
    assert_eq!(commit_ids_seen.len(), 2);
    assert_eq!(commit_ids_seen[0], commit_ids_seen[1]); // Same ID
}

#[test]
fn resource_matrix_d14_retained_count_matches() {
    // retention rule assertion: exactly these dispositions retain snapshots.
    let retain_dispositions = [
        WorkspaceDisposition::PendingWorktree,
        WorkspaceDisposition::PendingVerification,
    ];
    let no_retain_dispositions = [
        WorkspaceDisposition::PendingConflict,
        WorkspaceDisposition::PendingPromotion, // release after pin, not retain
        WorkspaceDisposition::Promoted,         // release after pin
        WorkspaceDisposition::Rejected,
        WorkspaceDisposition::Aborted,
        WorkspaceDisposition::NotApplicable,
    ];

    // Verify classify_lifecycle_failure produces Retain for the right dispositions
    let retain_failures = [
        LifecycleFailureKind::CleanupExhaustion,
        LifecycleFailureKind::MissingVerifyConfig,
        LifecycleFailureKind::VerifyFailed,
        LifecycleFailureKind::VerifyTimeout,
        LifecycleFailureKind::PostExportHeadChange,
    ];
    for kind in &retain_failures {
        let outcome =
            classify_lifecycle_failure(TurnCommitStatus::Completed, true, false, kind.clone());
        assert_eq!(
            outcome.resource_action,
            ResourceAction::Retain,
            "Expected Retain for {kind:?}"
        );
        assert!(
            retain_dispositions.contains(&outcome.disposition),
            "Unexpected disposition {:?} for {kind:?}",
            outcome.disposition
        );
    }

    // Verify no-retain dispositions
    let no_retain_failures = [
        LifecycleFailureKind::BaselineMovement,
        LifecycleFailureKind::ExportRejected {
            reason: "test".to_string(),
        },
        LifecycleFailureKind::AgentErrorBeforeCandidate,
    ];
    for kind in &no_retain_failures {
        let outcome =
            classify_lifecycle_failure(TurnCommitStatus::Completed, false, false, kind.clone());
        assert_ne!(
            outcome.resource_action,
            ResourceAction::Retain,
            "Expected non-Retain for {kind:?}"
        );
        assert!(
            no_retain_dispositions.contains(&outcome.disposition),
            "Unexpected disposition {:?} for {kind:?}",
            outcome.disposition
        );
    }

    // Promoted uses ReleaseAfterCandidatePin, not Retain
    let promoted = outcome_promoted("ref".to_string(), true);
    assert_eq!(
        promoted.resource_action,
        ResourceAction::ReleaseAfterCandidatePin
    );
    assert_eq!(promoted.disposition, WorkspaceDisposition::Promoted);
}

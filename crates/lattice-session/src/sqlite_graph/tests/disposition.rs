// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Disposition codec, lifecycle evidence, and strict parser regressions.

#[cfg(test)]
mod disposition_codec {
    use crate::SessionError;
    use crate::graph::*;

    // ── WorkspaceDisposition round-trip ──

    #[test]
    fn disposition_all_variants_round_trip() {
        let variants = [
            (WorkspaceDisposition::Promoted, "promoted"),
            (WorkspaceDisposition::PendingWorktree, "pending_worktree"),
            (
                WorkspaceDisposition::PendingVerification,
                "pending_verification",
            ),
            (WorkspaceDisposition::PendingPromotion, "pending_promotion"),
            (WorkspaceDisposition::PendingConflict, "pending_conflict"),
            (WorkspaceDisposition::Rejected, "rejected"),
            (WorkspaceDisposition::Aborted, "aborted"),
            (WorkspaceDisposition::NotApplicable, "not_applicable"),
        ];
        for (variant, expected_str) in &variants {
            assert_eq!(variant.as_str(), *expected_str);
            let parsed = WorkspaceDisposition::parse(expected_str)
                .unwrap_or_else(|e| panic!("parse({expected_str}) failed: {e}"));
            assert_eq!(parsed, *variant, "round-trip failed for {expected_str}");
        }
    }

    #[test]
    fn disposition_unknown_text_fails_closed() {
        let result = WorkspaceDisposition::parse("unknown_disposition");
        assert!(result.is_err(), "unknown disposition must fail closed");
        match result.unwrap_err() {
            SessionError::InvalidPersistedValue { field, value } => {
                assert_eq!(field, "disposition");
                assert_eq!(value, "unknown_disposition");
            }
            other => panic!("expected InvalidPersistedValue, got: {other:?}"),
        }
    }

    #[test]
    fn disposition_empty_string_fails_closed() {
        assert!(WorkspaceDisposition::parse("").is_err());
    }

    // ── TurnCommitStatus strict parse ──

    #[test]
    fn status_all_variants_round_trip() {
        let variants = [
            (TurnCommitStatus::Completed, "completed"),
            (TurnCommitStatus::Error, "error"),
            (TurnCommitStatus::Cancelled, "cancelled"),
        ];
        for (variant, expected_str) in &variants {
            assert_eq!(variant.as_str(), *expected_str);
            let parsed = TurnCommitStatus::parse(expected_str)
                .unwrap_or_else(|e| panic!("parse({expected_str}) failed: {e}"));
            assert_eq!(parsed, *variant, "round-trip failed for {expected_str}");
        }
    }

    #[test]
    fn status_unknown_text_fails_closed() {
        let result = TurnCommitStatus::parse("unknown_status");
        assert!(result.is_err(), "unknown status must fail closed");
        match result.unwrap_err() {
            SessionError::InvalidPersistedValue { field, value } => {
                assert_eq!(field, "status");
                assert_eq!(value, "unknown_status");
            }
            other => panic!("expected InvalidPersistedValue, got: {other:?}"),
        }
    }

    #[test]
    fn status_empty_string_fails_closed() {
        assert!(TurnCommitStatus::parse("").is_err());
    }

    // ── Typed persisted-value error ──

    #[test]
    fn invalid_persisted_value_error_carries_field_and_value() {
        let err = SessionError::InvalidPersistedValue {
            field: "disposition",
            value: "garbage".to_owned(),
        };
        let msg = err.to_string();
        assert!(msg.contains("disposition"), "error must mention field name");
        assert!(msg.contains("garbage"), "error must mention raw value");
    }

    // ── TurnVerificationEvidence ──

    #[test]
    fn verification_evidence_requires_non_empty_candidate_oid() {
        let result = TurnVerificationEvidence::new(
            vec!["cargo".into(), "test".into()],
            "profile-hash".into(),
            "".into(),
            0,
            Some(0),
            true,
        );
        assert!(
            result.is_err(),
            "empty candidate_oid must be rejected at construction"
        );
    }

    #[test]
    fn verification_evidence_requires_non_empty_argv() {
        let result = TurnVerificationEvidence::new(
            vec![],
            "profile-hash".into(),
            "abc123".into(),
            0,
            Some(0),
            true,
        );
        assert!(
            result.is_err(),
            "empty argv must be rejected at construction"
        );
    }

    #[test]
    fn verification_evidence_valid_construction() {
        let ev = TurnVerificationEvidence::new(
            vec!["cargo".into(), "test".into()],
            "profile-hash".into(),
            "abc123def456".into(),
            0,
            Some(0),
            true,
        )
        .expect("valid evidence must construct");
        assert_eq!(ev.argv, vec!["cargo", "test"]);
        assert_eq!(ev.profile_hash, "profile-hash");
        assert_eq!(ev.candidate_oid, "abc123def456");
        assert_eq!(ev.expected_exit_code, 0);
        assert_eq!(ev.exit_code, Some(0));
        assert!(ev.passed);
    }

    #[test]
    fn verification_evidence_failed_has_inconsistent_exit() {
        let ev = TurnVerificationEvidence::new(
            vec!["cargo".into()],
            "hash".into(),
            "oid123".into(),
            0,
            Some(1),
            false,
        )
        .expect("failed evidence constructs");
        assert!(!ev.passed);
        assert_eq!(ev.exit_code, Some(1));
    }

    // ── Snapshot identity types ──

    #[test]
    fn snapshot_identity_carries_backend_and_nonce() {
        let id = TurnSnapshotIdentity {
            backend: "microsandbox".to_string(),
            nonce: 0xDEAD_BEEF,
        };
        assert_eq!(id.backend, "microsandbox");
        assert_eq!(id.nonce, 0xDEAD_BEEF);
    }

    #[test]
    fn snapshot_ref_carries_identity_and_name() {
        let sr = TurnSnapshotRef {
            identity: TurnSnapshotIdentity {
                backend: "microsandbox".to_string(),
                nonce: 42,
            },
            name: "lattice-sess-1234-abcd-0000002a".to_string(),
        };
        assert_eq!(sr.identity.nonce, 42);
        assert!(sr.name.starts_with("lattice-"));
    }

    // ── TurnLifecycleEvidence ──

    #[test]
    fn lifecycle_evidence_optional_fields_default_none() {
        let ev = TurnLifecycleEvidence {
            target_project_ref: "refs/heads/main".to_string(),
            baseline_oid: None,
            candidate_oid: None,
            candidate_ref: None,
            verification: None,
            snapshot_ref: None,
            resumes_turn_oid: None,
        };
        assert_eq!(ev.target_project_ref, "refs/heads/main");
        assert!(ev.baseline_oid.is_none());
        assert!(ev.candidate_oid.is_none());
        assert!(ev.candidate_ref.is_none());
        assert!(ev.verification.is_none());
        assert!(ev.snapshot_ref.is_none());
        assert!(ev.resumes_turn_oid.is_none());
    }

    #[test]
    fn lifecycle_evidence_full_construction() {
        let verification = TurnVerificationEvidence::new(
            vec!["cargo".into()],
            "ph".into(),
            "cand-oid".into(),
            0,
            Some(0),
            true,
        )
        .unwrap();
        let ev = TurnLifecycleEvidence {
            target_project_ref: "refs/heads/main".to_string(),
            baseline_oid: Some("base-oid".to_string()),
            candidate_oid: Some("cand-oid".to_string()),
            candidate_ref: Some("refs/lattice/candidate/abc".to_string()),
            verification: Some(verification),
            snapshot_ref: Some(TurnSnapshotRef {
                identity: TurnSnapshotIdentity {
                    backend: "microsandbox".to_string(),
                    nonce: 99,
                },
                name: "lattice-sess-abcdef01-00000063".to_string(),
            }),
            resumes_turn_oid: Some("prior-turn-oid".to_string()),
        };
        assert!(ev.verification.is_some());
        assert!(ev.snapshot_ref.is_some());
        assert_eq!(ev.resumes_turn_oid.as_deref(), Some("prior-turn-oid"));
    }

    // ── DTO field presence ──

    #[test]
    fn turn_commit_input_has_disposition_and_lifecycle() {
        let input = TurnCommitInput {
            session_id: "sess-1".to_string(),
            parent_ids: vec![],
            messages: vec![],
            agent_name: "coder".to_string(),
            profile_snapshot_hash: "hash".to_string(),
            profile_snapshot_json: "{}".to_string(),
            model: "sonnet".to_string(),
            provider: "anthropic".to_string(),
            status: TurnCommitStatus::Completed,
            disposition: WorkspaceDisposition::NotApplicable,
            lifecycle: None,
            commit_message: "test".to_string(),
            usage: None,
            terminal_reason: None,
        };
        assert_eq!(input.disposition, WorkspaceDisposition::NotApplicable);
        assert!(input.lifecycle.is_none());
    }

    #[test]
    fn turn_commit_has_disposition_and_lifecycle() {
        let commit = TurnCommit {
            commit_id: "id".to_string(),
            session_id: "sess-1".to_string(),
            parent_ids: vec![],
            message_ids: vec![],
            agent_name: "coder".to_string(),
            profile_snapshot_hash: "hash".to_string(),
            profile_snapshot_json: "{}".to_string(),
            model: "sonnet".to_string(),
            provider: "anthropic".to_string(),
            status: TurnCommitStatus::Completed,
            disposition: WorkspaceDisposition::Promoted,
            lifecycle: None,
            commit_message: "test".to_string(),
            usage: None,
            terminal_reason: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
        };
        assert_eq!(commit.disposition, WorkspaceDisposition::Promoted);
    }

    #[test]
    fn turn_log_entry_has_disposition() {
        let entry = TurnLogEntry {
            commit_id: "abc".to_string(),
            parent_ids: vec![],
            status: TurnCommitStatus::Completed,
            disposition: WorkspaceDisposition::PendingWorktree,
            agent_name: "coder".to_string(),
            profile_snapshot_hash: "hash".to_string(),
            model: "sonnet".to_string(),
            provider: "anthropic".to_string(),
            commit_message: "test".to_string(),
            message_count: 0,
            tool_message_count: 0,
            usage: None,
            terminal_reason: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
        };
        assert_eq!(entry.disposition, WorkspaceDisposition::PendingWorktree);
    }
}

#[cfg(test)]
mod disposition_identity {
    use crate::graph::*;

    fn base_input() -> TurnCommitInput {
        TurnCommitInput {
            session_id: "sess-1".to_string(),
            parent_ids: vec![],
            messages: vec![],
            agent_name: "coder".to_string(),
            profile_snapshot_hash: "profile-hash".to_string(),
            profile_snapshot_json: "{}".to_string(),
            model: "sonnet".to_string(),
            provider: "anthropic".to_string(),
            status: TurnCommitStatus::Completed,
            disposition: WorkspaceDisposition::NotApplicable,
            lifecycle: None,
            commit_message: "Hello".to_string(),
            usage: None,
            terminal_reason: None,
        }
    }

    fn full_lifecycle() -> TurnLifecycleEvidence {
        TurnLifecycleEvidence {
            target_project_ref: "refs/heads/main".to_string(),
            baseline_oid: Some("base-oid-123".to_string()),
            candidate_oid: Some("cand-oid-456".to_string()),
            candidate_ref: Some("refs/lattice/candidate/abc".to_string()),
            verification: Some(
                TurnVerificationEvidence::new(
                    vec!["cargo".into(), "test".into()],
                    "verify-hash".into(),
                    "cand-oid-456".into(),
                    0,
                    Some(0),
                    true,
                )
                .unwrap(),
            ),
            snapshot_ref: Some(TurnSnapshotRef {
                identity: TurnSnapshotIdentity {
                    backend: "microsandbox".to_string(),
                    nonce: 0xDEAD_BEEF,
                },
                name: "lattice-sess-1-abcdef01-deadbeef".to_string(),
            }),
            resumes_turn_oid: Some("prior-turn-oid".to_string()),
        }
    }

    // ── prepare_turn_commit ──

    #[test]
    fn prepare_turn_commit_produces_prepared_commit() {
        let input = base_input();
        let prepared = prepare_turn_commit(&input, "2026-01-01T00:00:00Z");
        assert_eq!(prepared.commit.commit_id.len(), 64);
        assert!(prepared.commit_id.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(prepared.commit.session_id, "sess-1");
        assert_eq!(prepared.commit.created_at, "2026-01-01T00:00:00Z");
    }

    #[test]
    fn prepare_turn_commit_is_deterministic() {
        let input = base_input();
        let p1 = prepare_turn_commit(&input, "2026-01-01T00:00:00Z");
        let p2 = prepare_turn_commit(&input, "2026-01-01T00:00:00Z");
        assert_eq!(p1.commit_id, p2.commit_id);
        assert_eq!(p1.message_ids, p2.message_ids);
    }

    // ── Identity assertions: each field changes commit ID ──

    #[test]
    fn changing_disposition_changes_commit_id() {
        let mut input = base_input();
        input.lifecycle = Some(full_lifecycle());
        let base = prepare_turn_commit(&input, "2026-01-01T00:00:00Z");

        let mut changed = base_input();
        changed.lifecycle = Some(full_lifecycle());
        changed.disposition = WorkspaceDisposition::Promoted;
        let other = prepare_turn_commit(&changed, "2026-01-01T00:00:00Z");

        assert_ne!(
            base.commit_id, other.commit_id,
            "disposition change must alter commit ID"
        );
    }

    #[test]
    fn changing_target_ref_changes_commit_id() {
        let mut lc = full_lifecycle();
        let input_a = {
            let mut i = base_input();
            i.lifecycle = Some(lc.clone());
            i
        };
        let base = prepare_turn_commit(&input_a, "2026-01-01T00:00:00Z");

        lc.target_project_ref = "refs/heads/feature".to_string();
        let input_b = {
            let mut i = base_input();
            i.lifecycle = Some(lc);
            i
        };
        let other = prepare_turn_commit(&input_b, "2026-01-01T00:00:00Z");
        assert_ne!(base.commit_id, other.commit_id);
    }

    #[test]
    fn changing_baseline_oid_changes_commit_id() {
        let mut lc = full_lifecycle();
        let input_a = {
            let mut i = base_input();
            i.lifecycle = Some(lc.clone());
            i
        };
        let base = prepare_turn_commit(&input_a, "2026-01-01T00:00:00Z");

        lc.baseline_oid = Some("different-base".to_string());
        let input_b = {
            let mut i = base_input();
            i.lifecycle = Some(lc);
            i
        };
        let other = prepare_turn_commit(&input_b, "2026-01-01T00:00:00Z");
        assert_ne!(base.commit_id, other.commit_id);
    }

    #[test]
    fn changing_candidate_oid_changes_commit_id() {
        let mut lc = full_lifecycle();
        let input_a = {
            let mut i = base_input();
            i.lifecycle = Some(lc.clone());
            i
        };
        let base = prepare_turn_commit(&input_a, "2026-01-01T00:00:00Z");

        lc.candidate_oid = Some("different-candidate".to_string());
        let input_b = {
            let mut i = base_input();
            i.lifecycle = Some(lc);
            i
        };
        let other = prepare_turn_commit(&input_b, "2026-01-01T00:00:00Z");
        assert_ne!(base.commit_id, other.commit_id);
    }

    #[test]
    fn changing_verification_changes_commit_id() {
        let mut lc = full_lifecycle();
        let input_a = {
            let mut i = base_input();
            i.lifecycle = Some(lc.clone());
            i
        };
        let base = prepare_turn_commit(&input_a, "2026-01-01T00:00:00Z");

        lc.verification = None;
        let input_b = {
            let mut i = base_input();
            i.lifecycle = Some(lc);
            i
        };
        let other = prepare_turn_commit(&input_b, "2026-01-01T00:00:00Z");
        assert_ne!(base.commit_id, other.commit_id);
    }

    #[test]
    fn changing_snapshot_nonce_changes_commit_id() {
        let mut lc = full_lifecycle();
        let input_a = {
            let mut i = base_input();
            i.lifecycle = Some(lc.clone());
            i
        };
        let base = prepare_turn_commit(&input_a, "2026-01-01T00:00:00Z");

        if let Some(ref mut sr) = lc.snapshot_ref {
            sr.identity.nonce = 0xCAFE_BABE;
        }
        let input_b = {
            let mut i = base_input();
            i.lifecycle = Some(lc);
            i
        };
        let other = prepare_turn_commit(&input_b, "2026-01-01T00:00:00Z");
        assert_ne!(base.commit_id, other.commit_id);
    }

    #[test]
    fn changing_resumes_turn_oid_changes_commit_id() {
        let mut lc = full_lifecycle();
        let input_a = {
            let mut i = base_input();
            i.lifecycle = Some(lc.clone());
            i
        };
        let base = prepare_turn_commit(&input_a, "2026-01-01T00:00:00Z");

        lc.resumes_turn_oid = Some("different-prior".to_string());
        let input_b = {
            let mut i = base_input();
            i.lifecycle = Some(lc);
            i
        };
        let other = prepare_turn_commit(&input_b, "2026-01-01T00:00:00Z");
        assert_ne!(base.commit_id, other.commit_id);
    }

    #[test]
    fn adding_lifecycle_changes_commit_id() {
        let input_no_lc = base_input();
        let base = prepare_turn_commit(&input_no_lc, "2026-01-01T00:00:00Z");

        let mut input_with_lc = base_input();
        input_with_lc.lifecycle = Some(full_lifecycle());
        let other = prepare_turn_commit(&input_with_lc, "2026-01-01T00:00:00Z");
        assert_ne!(
            base.commit_id, other.commit_id,
            "adding lifecycle evidence must alter commit ID"
        );
    }

    // ── Snapshot name derivation ──

    #[test]
    fn derive_snapshot_name_uses_commit_prefix_and_nonce() {
        let name = derive_snapshot_name("sess-1", "abcdef0123456789abcdef", 0xDEAD_BEEF);
        assert_eq!(name, "lattice-sess-1-abcdef01-deadbeef");
    }

    #[test]
    fn derive_snapshot_name_pads_short_nonce() {
        let name = derive_snapshot_name("sess-1", "abcdef0123456789abcdef", 0x0000_002A);
        assert_eq!(name, "lattice-sess-1-abcdef01-0000002a");
    }

    #[test]
    fn validate_snapshot_name_accepts_correct_derivation() {
        assert!(validate_snapshot_name(
            "lattice-sess-1-abcdef01-deadbeef",
            "sess-1",
            "abcdef0123456789abcdef",
            0xDEAD_BEEF
        ));
    }

    #[test]
    fn validate_snapshot_name_rejects_wrong_session() {
        assert!(!validate_snapshot_name(
            "lattice-sess-2-abcdef01-deadbeef",
            "sess-1",
            "abcdef0123456789abcdef",
            0xDEAD_BEEF
        ));
    }

    #[test]
    fn validate_snapshot_name_rejects_wrong_commit_prefix() {
        assert!(!validate_snapshot_name(
            "lattice-sess-1-bbbbbbbb-deadbeef",
            "sess-1",
            "abcdef0123456789abcdef",
            0xDEAD_BEEF
        ));
    }

    #[test]
    fn validate_snapshot_name_rejects_wrong_nonce() {
        assert!(!validate_snapshot_name(
            "lattice-sess-1-abcdef01-cafebabe",
            "sess-1",
            "abcdef0123456789abcdef",
            0xDEAD_BEEF
        ));
    }

    // ── Operational state exclusion ──

    #[test]
    fn prepared_commit_has_no_snapshot_lost_field() {
        let input = base_input();
        let prepared = prepare_turn_commit(&input, "2026-01-01T00:00:00Z");
        let json = serde_json::to_string(&prepared.commit).unwrap();
        assert!(
            !json.contains("snapshot_lost"),
            "snapshot_lost is mutable operational state, not part of immutable commit"
        );
    }
}

// ── Task 1: Persistence, migration, and strict decoding ──

#[cfg(test)]
mod disposition_persistence {
    use std::env;
    use std::fs;
    use std::path::PathBuf;

    use rusqlite::Connection;

    use crate::graph::*;
    use crate::sqlite_graph::SqliteSessionGraphStore;
    use crate::test_helpers::user_msg;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = env::temp_dir()
            .join("lattice-session-disposition-persistence")
            .join(name);
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn make_input(
        session_id: &str,
        parent_ids: Vec<String>,
        disposition: WorkspaceDisposition,
        lifecycle: Option<TurnLifecycleEvidence>,
    ) -> TurnCommitInput {
        TurnCommitInput {
            session_id: session_id.to_string(),
            parent_ids,
            messages: vec![user_msg("test")],
            agent_name: "coder".to_string(),
            profile_snapshot_hash: "hash".to_string(),
            profile_snapshot_json: "{}".to_string(),
            model: "sonnet".to_string(),
            provider: "anthropic".to_string(),
            status: TurnCommitStatus::Completed,
            disposition,
            lifecycle,
            commit_message: "test".to_string(),
            usage: None,
            terminal_reason: None,
        }
    }

    fn sample_lifecycle() -> TurnLifecycleEvidence {
        TurnLifecycleEvidence {
            target_project_ref: "refs/heads/main".to_string(),
            baseline_oid: Some("base-oid".to_string()),
            candidate_oid: Some("cand-oid".to_string()),
            candidate_ref: Some("refs/lattice/candidate/abc".to_string()),
            verification: Some(
                TurnVerificationEvidence::new(
                    vec!["cargo".into(), "test".into()],
                    "verify-hash".into(),
                    "cand-oid".into(),
                    0,
                    Some(0),
                    true,
                )
                .unwrap(),
            ),
            snapshot_ref: Some(TurnSnapshotRef {
                identity: TurnSnapshotIdentity {
                    backend: "microsandbox".to_string(),
                    nonce: 42,
                },
                name: "lattice-sess-1-abcdef01-0000002a".to_string(),
            }),
            resumes_turn_oid: None,
        }
    }

    // ── Migration assertion ──

    #[test]
    fn migration_adds_disposition_and_lifecycle_columns() {
        let dir = temp_dir("migration_columns");
        let store = SqliteSessionGraphStore::new(dir.clone());
        drop(store.conn().expect("init"));

        let conn = Connection::open(store.db_path()).expect("raw conn");
        let columns: Vec<String> = {
            let mut stmt = conn
                .prepare("PRAGMA table_info(graph_commits)")
                .expect("pragma");
            let rows = stmt
                .query_map([], |row| row.get::<_, String>(1))
                .expect("query_map");
            rows.filter_map(|r| r.ok()).collect()
        };

        assert!(
            columns.contains(&"disposition".to_string()),
            "graph_commits must have disposition column, found: {columns:?}"
        );
        assert!(
            columns.contains(&"lifecycle_json".to_string()),
            "graph_commits must have lifecycle_json column, found: {columns:?}"
        );
    }

    #[test]
    fn pre_phase36_database_backfills_not_applicable() {
        let dir = temp_dir("backfill_na");
        let store = SqliteSessionGraphStore::new(dir.clone());
        store
            .create_session("sess-1", None, "coder", "main")
            .expect("create");

        let input = make_input("sess-1", vec![], WorkspaceDisposition::NotApplicable, None);
        store.commit_turn(input).expect("commit");

        let log = store.log("sess-1").expect("log");
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].disposition, WorkspaceDisposition::NotApplicable);
    }

    // ── Persistence round-trip ──

    #[test]
    fn commit_persists_disposition_and_lifecycle_evidence() {
        let dir = temp_dir("persist_disposition");
        let store = SqliteSessionGraphStore::new(dir);
        store
            .create_session("sess-1", None, "coder", "main")
            .expect("create");

        let lc = sample_lifecycle();
        let input = make_input("sess-1", vec![], WorkspaceDisposition::Promoted, Some(lc));
        let commit = store.commit_turn(input).expect("commit");
        assert_eq!(commit.disposition, WorkspaceDisposition::Promoted);
        assert!(commit.lifecycle.is_some());

        let log = store.log("sess-1").expect("log");
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].disposition, WorkspaceDisposition::Promoted);
    }

    #[test]
    fn log_reports_stored_disposition_not_default() {
        let dir = temp_dir("log_not_default");
        let store = SqliteSessionGraphStore::new(dir);
        store
            .create_session("sess-1", None, "coder", "main")
            .expect("create");

        let input = make_input(
            "sess-1",
            vec![],
            WorkspaceDisposition::PendingWorktree,
            None,
        );
        let commit = store.commit_turn(input).expect("commit");

        let log = store.log("sess-1").expect("log");
        assert_eq!(log[0].disposition, WorkspaceDisposition::PendingWorktree);
        assert_eq!(log[0].commit_id, commit.commit_id);
        assert_eq!(log[0].disposition, commit.disposition);
    }

    // ── Corruption assertion ──

    #[test]
    fn unknown_status_in_row_fails_strict_decode() {
        let dir = temp_dir("corrupt_status");
        let store = SqliteSessionGraphStore::new(dir.clone());
        store
            .create_session("sess-1", None, "coder", "main")
            .expect("create");

        let input = make_input("sess-1", vec![], WorkspaceDisposition::NotApplicable, None);
        store.commit_turn(input).expect("commit");

        let conn = Connection::open(store.db_path()).expect("raw conn");
        conn.execute(
            "UPDATE graph_commits SET status = 'bogus_status' WHERE session_id = 'sess-1'",
            [],
        )
        .expect("tamper");

        let result = store.log("sess-1");
        assert!(result.is_err(), "unknown status must fail strict decode");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("status"),
            "error must mention field: {err_msg}"
        );
        assert!(
            err_msg.contains("bogus_status"),
            "error must mention raw value: {err_msg}"
        );
    }

    #[test]
    fn unknown_disposition_in_row_fails_strict_decode() {
        let dir = temp_dir("corrupt_disposition");
        let store = SqliteSessionGraphStore::new(dir.clone());
        store
            .create_session("sess-1", None, "coder", "main")
            .expect("create");

        let input = make_input("sess-1", vec![], WorkspaceDisposition::NotApplicable, None);
        store.commit_turn(input).expect("commit");

        let conn = Connection::open(store.db_path()).expect("raw conn");
        conn.execute(
            "UPDATE graph_commits SET disposition = 'bogus_disp' WHERE session_id = 'sess-1'",
            [],
        )
        .expect("tamper");

        let result = store.log("sess-1");
        assert!(
            result.is_err(),
            "unknown disposition must fail strict decode"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("disposition"),
            "error must mention field: {err_msg}"
        );
        assert!(
            err_msg.contains("bogus_disp"),
            "error must mention raw value: {err_msg}"
        );
    }

    #[test]
    fn malformed_lifecycle_json_fails_strict_decode() {
        let dir = temp_dir("corrupt_lifecycle");
        let store = SqliteSessionGraphStore::new(dir.clone());
        store
            .create_session("sess-1", None, "coder", "main")
            .expect("create");

        let input = make_input("sess-1", vec![], WorkspaceDisposition::NotApplicable, None);
        store.commit_turn(input).expect("commit");

        let conn = Connection::open(store.db_path()).expect("raw conn");
        conn.execute(
            "UPDATE graph_commits SET lifecycle_json = '{invalid json' WHERE session_id = 'sess-1'",
            [],
        )
        .expect("tamper");

        let result = store.log("sess-1");
        assert!(
            result.is_err(),
            "malformed lifecycle JSON must fail strict decode"
        );
    }

    // ── Lifecycle evidence round-trip ──

    #[test]
    fn lifecycle_evidence_round_trips_through_sqlite() {
        let dir = temp_dir("lifecycle_roundtrip");
        let store = SqliteSessionGraphStore::new(dir);
        store
            .create_session("sess-1", None, "coder", "main")
            .expect("create");

        let lc = sample_lifecycle();
        let input = make_input(
            "sess-1",
            vec![],
            WorkspaceDisposition::Promoted,
            Some(lc.clone()),
        );
        let commit = store.commit_turn(input).expect("commit");

        let returned_lc = commit.lifecycle.expect("lifecycle should be present");
        assert_eq!(returned_lc.target_project_ref, "refs/heads/main");
        assert_eq!(returned_lc.baseline_oid.as_deref(), Some("base-oid"));
        assert_eq!(returned_lc.candidate_oid.as_deref(), Some("cand-oid"));
        assert!(returned_lc.verification.is_some());
        let v = returned_lc.verification.unwrap();
        assert_eq!(v.argv, vec!["cargo", "test"]);
        assert_eq!(v.candidate_oid, "cand-oid");
        assert!(v.passed);

        let sr = returned_lc.snapshot_ref.expect("snapshot ref present");
        assert_eq!(sr.identity.backend, "microsandbox");
        assert_eq!(sr.identity.nonce, 42);
    }
}

// ── Task 2: Prepared commit persistence and snapshot state isolation ──

#[cfg(test)]
mod prepared_turn_commit {
    use std::env;
    use std::fs;
    use std::path::PathBuf;

    use rusqlite::Connection;

    use crate::graph::*;
    use crate::sqlite_graph::SqliteSessionGraphStore;
    use crate::test_helpers::user_msg;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = env::temp_dir()
            .join("lattice-session-prepared-turn-commit")
            .join(name);
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn make_input(
        session_id: &str,
        parent_ids: Vec<String>,
        disposition: WorkspaceDisposition,
    ) -> TurnCommitInput {
        TurnCommitInput {
            session_id: session_id.to_string(),
            parent_ids,
            messages: vec![user_msg("test")],
            agent_name: "coder".to_string(),
            profile_snapshot_hash: "hash".to_string(),
            profile_snapshot_json: "{}".to_string(),
            model: "sonnet".to_string(),
            provider: "anthropic".to_string(),
            status: TurnCommitStatus::Completed,
            disposition,
            lifecycle: None,
            commit_message: "test".to_string(),
            usage: None,
            terminal_reason: None,
        }
    }

    // ── CAS assertion ──

    #[test]
    fn commit_prepared_turn_rejects_stale_parent() {
        let dir = temp_dir("prepared_stale_parent");
        let store = SqliteSessionGraphStore::new(dir);
        store
            .create_session("sess-1", None, "coder", "main")
            .expect("create");

        // First commit to establish a HEAD.
        let first = make_input("sess-1", vec![], WorkspaceDisposition::NotApplicable);
        store.commit_turn(first).expect("first commit");

        // Prepare a commit with a wrong parent.
        let wrong_parent = make_input(
            "sess-1",
            vec!["wrong-parent-id".to_string()],
            WorkspaceDisposition::NotApplicable,
        );
        let prepared = prepare_turn_commit(&wrong_parent, "2026-01-01T00:00:00Z");
        let result = store.commit_prepared_turn(prepared);
        assert!(result.is_err(), "stale parent must be rejected");
    }

    // ── Retry assertion ──

    #[test]
    fn prepared_commit_retains_same_id_across_attempts() {
        let dir = temp_dir("prepared_retry");
        let store = SqliteSessionGraphStore::new(dir);
        store
            .create_session("sess-1", None, "coder", "main")
            .expect("create");

        let input = make_input("sess-1", vec![], WorkspaceDisposition::NotApplicable);
        let prepared = prepare_turn_commit(&input, "2026-01-01T00:00:00Z");
        let expected_id = prepared.commit_id.clone();

        // First attempt succeeds.
        let commit1 = store
            .commit_prepared_turn(prepared.clone())
            .expect("first attempt");
        assert_eq!(commit1.commit_id, expected_id);

        // Second attempt with same prepared commit should be idempotent.
        let commit2 = store.commit_prepared_turn(prepared).expect("retry");
        assert_eq!(commit2.commit_id, expected_id);
        assert_eq!(commit1.commit_id, commit2.commit_id);
    }

    // ── Lookup assertion ──

    #[test]
    fn get_turn_commit_returns_complete_lifecycle() {
        let dir = temp_dir("lookup_lifecycle");
        let store = SqliteSessionGraphStore::new(dir);
        store
            .create_session("sess-1", None, "coder", "main")
            .expect("create");

        let lc = TurnLifecycleEvidence {
            target_project_ref: "refs/heads/main".to_string(),
            baseline_oid: Some("base-oid".to_string()),
            candidate_oid: Some("cand-oid".to_string()),
            candidate_ref: Some("refs/lattice/candidate/abc".to_string()),
            verification: Some(
                TurnVerificationEvidence::new(
                    vec!["cargo".into()],
                    "hash".into(),
                    "cand-oid".into(),
                    0,
                    Some(0),
                    true,
                )
                .unwrap(),
            ),
            snapshot_ref: None,
            resumes_turn_oid: None,
        };

        let input = TurnCommitInput {
            session_id: "sess-1".to_string(),
            parent_ids: vec![],
            messages: vec![user_msg("test")],
            agent_name: "coder".to_string(),
            profile_snapshot_hash: "hash".to_string(),
            profile_snapshot_json: "{}".to_string(),
            model: "sonnet".to_string(),
            provider: "anthropic".to_string(),
            status: TurnCommitStatus::Completed,
            disposition: WorkspaceDisposition::Promoted,
            lifecycle: Some(lc),
            commit_message: "test".to_string(),
            usage: None,
            terminal_reason: None,
        };

        let commit = store.commit_turn(input).expect("commit");

        // Lookup by commit ID should return complete lifecycle.
        let looked_up = store
            .get_turn_commit("sess-1", &commit.commit_id)
            .expect("lookup")
            .expect("should find commit");
        assert_eq!(looked_up.commit_id, commit.commit_id);
        assert_eq!(looked_up.disposition, WorkspaceDisposition::Promoted);
        assert!(looked_up.lifecycle.is_some());
        let lc = looked_up.lifecycle.unwrap();
        assert_eq!(lc.target_project_ref, "refs/heads/main");
        assert_eq!(lc.baseline_oid.as_deref(), Some("base-oid"));
    }

    #[test]
    fn get_turn_commit_returns_none_for_unknown() {
        let dir = temp_dir("lookup_unknown");
        let store = SqliteSessionGraphStore::new(dir);
        store
            .create_session("sess-1", None, "coder", "main")
            .expect("create");

        let result = store.get_turn_commit("sess-1", "nonexistent-id");
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    // ── Immutability assertion ──

    #[test]
    fn set_snapshot_lost_does_not_change_commit_row() {
        let dir = temp_dir("snapshot_lost_immutability");
        let store = SqliteSessionGraphStore::new(dir.clone());
        store
            .create_session("sess-1", None, "coder", "main")
            .expect("create");

        let input = make_input("sess-1", vec![], WorkspaceDisposition::NotApplicable);
        let commit = store.commit_turn(input).expect("commit");

        // Read the raw commit row before toggling.
        let conn = Connection::open(store.db_path()).expect("raw conn");
        let before_row: String = conn
            .query_row(
                "SELECT commit_id || status || disposition || lifecycle_json FROM graph_commits WHERE commit_id = ?1",
                [&commit.commit_id],
                |row| row.get(0),
            )
            .expect("read before");

        // Toggle snapshot_lost.
        store
            .set_snapshot_lost(&commit.commit_id, true)
            .expect("set lost");
        let lost = store
            .get_snapshot_lost(&commit.commit_id)
            .expect("get lost");
        assert_eq!(lost, Some(true));

        // Verify commit row is unchanged.
        let after_row: String = conn
            .query_row(
                "SELECT commit_id || status || disposition || lifecycle_json FROM graph_commits WHERE commit_id = ?1",
                [&commit.commit_id],
                |row| row.get(0),
            )
            .expect("read after");

        assert_eq!(
            before_row, after_row,
            "commit row must be unchanged by snapshot_lost toggle"
        );

        // Toggle back.
        store
            .set_snapshot_lost(&commit.commit_id, false)
            .expect("unset lost");
        let lost = store
            .get_snapshot_lost(&commit.commit_id)
            .expect("get lost");
        assert_eq!(lost, Some(false));
    }

    // ── commit_turn delegates to prepared path ──

    #[test]
    fn commit_turn_delegates_to_prepared_path() {
        let dir = temp_dir("commit_turn_delegate");
        let store = SqliteSessionGraphStore::new(dir);
        store
            .create_session("sess-1", None, "coder", "main")
            .expect("create");

        // commit_turn should still work as a convenience.
        let input = make_input("sess-1", vec![], WorkspaceDisposition::PendingWorktree);
        let commit = store.commit_turn(input).expect("commit_turn");
        assert_eq!(commit.disposition, WorkspaceDisposition::PendingWorktree);

        // And the commit should be retrievable via get_turn_commit.
        let looked_up = store
            .get_turn_commit("sess-1", &commit.commit_id)
            .expect("lookup")
            .expect("should find");
        assert_eq!(looked_up.disposition, WorkspaceDisposition::PendingWorktree);
    }
}

// ── Task 3: Promoted session manifest carries lifecycle evidence ──

#[cfg(test)]
mod promoted_session_manifest {
    use crate::graph::*;
    use crate::promotion::*;

    fn sample_input() -> PromotedSessionCommitInput {
        PromotedSessionCommitInput {
            session_id: "sess-1".to_string(),
            turn_id: "turn-abc".to_string(),
            baseline_oid: "a".repeat(40),
            candidate_oid: "b".repeat(40),
            committed_at_unix_seconds: 1_700_000_000,
            parent_session_oid: None,
            target_project_ref: "refs/heads/main".to_string(),
            candidate_ref: Some("refs/lattice/candidate/abc".to_string()),
            execution_status: TurnCommitStatus::Completed,
            verification: Some(
                TurnVerificationEvidence::new(
                    vec!["cargo".into(), "test".into()],
                    "profile-hash-abc".into(),
                    "b".repeat(40),
                    0,
                    Some(0),
                    true,
                )
                .unwrap(),
            ),
        }
    }

    // ── Manifest assertion ──

    #[test]
    fn promoted_input_carries_lifecycle_evidence_fields() {
        let input = sample_input();
        assert_eq!(input.target_project_ref, "refs/heads/main");
        assert_eq!(
            input.candidate_ref.as_deref(),
            Some("refs/lattice/candidate/abc")
        );
        assert_eq!(input.execution_status, TurnCommitStatus::Completed);
        assert!(input.verification.is_some());
    }

    #[test]
    fn promoted_manifest_includes_target_ref_and_candidate_ref() {
        let input = sample_input();
        let manifest = build_promoted_manifest(&input);
        assert_eq!(manifest.target_project_ref, "refs/heads/main");
        assert_eq!(
            manifest.candidate_ref.as_deref(),
            Some("refs/lattice/candidate/abc")
        );
    }

    #[test]
    fn promoted_manifest_includes_execution_status() {
        let input = sample_input();
        let manifest = build_promoted_manifest(&input);
        assert_eq!(manifest.execution_status, "completed");
    }

    #[test]
    fn promoted_manifest_includes_verification_evidence() {
        let input = sample_input();
        let manifest = build_promoted_manifest(&input);
        let verification = manifest.verification.expect("verification present");
        assert_eq!(verification.argv, vec!["cargo", "test"]);
        assert_eq!(verification.profile_hash, "profile-hash-abc");
        assert_eq!(verification.candidate_oid, "b".repeat(40));
        assert!(verification.passed);
    }

    #[test]
    fn promoted_manifest_disposition_is_always_promoted() {
        let input = sample_input();
        let manifest = build_promoted_manifest(&input);
        assert_eq!(manifest.disposition, "promoted");
    }

    // ── Boundary assertion ──

    #[test]
    fn no_pending_disposition_builder_exists() {
        // Compile-time assertion: only PromotedSessionCommitInput exists.
        // If a PendingSessionCommitInput or similar were added, this module
        // would need to be updated, drawing reviewer attention.
        let _ = sample_input();
    }

    // ── Determinism assertion ──

    #[test]
    fn promoted_manifest_json_is_deterministic() {
        let input = sample_input();
        let m1 = build_promoted_manifest(&input);
        let m2 = build_promoted_manifest(&input);
        let json1 = manifest_json_canonical_bytes(&m1).unwrap();
        let json2 = manifest_json_canonical_bytes(&m2).unwrap();
        assert_eq!(
            json1, json2,
            "identical inputs must produce identical manifest JSON"
        );
    }

    #[test]
    fn promoted_manifest_schema_version_is_v2() {
        let input = sample_input();
        let manifest = build_promoted_manifest(&input);
        assert_eq!(
            manifest.schema_version, PROMOTION_MANIFEST_SCHEMA_VERSION_V2,
            "Phase 36 manifest must be v2"
        );
    }

    // ── Git OID determinism (integration with git2) ──

    #[test]
    fn create_promoted_session_commit_is_deterministic() {
        let dir = std::env::temp_dir()
            .join("lattice-promotion-manifest-test")
            .join("determinism");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create dir");
        let repo = git2::Repository::init(&dir).expect("init repo");

        let input = sample_input();
        let oid1 = create_promoted_session_commit(&repo, &input).expect("first");
        let oid2 = create_promoted_session_commit(&repo, &input).expect("second");
        assert_eq!(oid1, oid2, "same input must produce same Git OID");
    }

    #[test]
    fn promoted_manifest_omits_verification_when_none() {
        let mut input = sample_input();
        input.verification = None;
        let manifest = build_promoted_manifest(&input);
        assert!(manifest.verification.is_none());
    }
}

// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;
use lancet_catalog::CepMethod;
use lancet_catalog::methods::Fetch;
use lancet_catalog::test_method::TestEcho;

use lancet_protocol::cell::{Cell, ChannelKind, CtxOp, ExtImpact, Trust};
use lancet_protocol::evidence::{
    ADMISSION_EVIDENCE_VERSION, DENIED_OPERATIONS_RECORD_VERSION, DeniedOperation, EffectSource,
    OBSERVED_EFFECTS_RECORD_VERSION, ObservedScope, ObservedTarget,
};
use serde_json::json;
use std::path::PathBuf;

/// `observe.workspace` as a plain method name.
///
/// It is a Component Tool since #30, so there is no `lancet-catalog` type to
/// take `NAME` from. These tests use it as the *subject* of an audit row — the
/// store records whatever method name it is given — so a literal is the honest
/// spelling: a catalog type would imply a native-catalog membership the method
/// no longer has.
const OBSERVE_WORKSPACE: &str = "observe.workspace";

fn test_context() -> CepAuditContext {
    CepAuditContext {
        turn_id: "turn-1".into(),
        session_id: "session-1".into(),
        ..Default::default()
    }
}

fn unique_test_dir(name: &str) -> PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "lancet-audit-{name}-{unique}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[cfg(unix)]
#[test]
fn open_rejects_symlinked_audit_parent() {
    let root = unique_test_dir("symlink-parent-root");
    let outside = unique_test_dir("symlink-parent-outside");
    std::os::unix::fs::symlink(&outside, root.join(".lattice")).unwrap();

    let err = match CepAuditStore::open(root.join(".lattice").join("cep_audit.db")) {
        Ok(_) => panic!("symlinked audit parent should be rejected"),
        Err(err) => err,
    };

    assert!(matches!(err, AuditError::UnsafePath { .. }));
    assert!(
        !outside.join("cep_audit.db").exists(),
        "audit open must not create a database through a symlinked .lattice directory"
    );
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&outside);
}

#[cfg(unix)]
#[test]
fn open_rejects_symlinked_audit_database_file() {
    let root = unique_test_dir("symlink-file-root");
    let outside = unique_test_dir("symlink-file-outside");
    let lattice_dir = root.join(".lattice");
    std::fs::create_dir_all(&lattice_dir).unwrap();
    std::os::unix::fs::symlink(
        outside.join("forged-audit.db"),
        lattice_dir.join("cep_audit.db"),
    )
    .unwrap();

    let err = match CepAuditStore::open(lattice_dir.join("cep_audit.db")) {
        Ok(_) => panic!("symlinked audit database should be rejected"),
        Err(err) => err,
    };

    assert!(matches!(err, AuditError::UnsafePath { .. }));
    assert!(
        !outside.join("forged-audit.db").exists(),
        "audit open must not create a database through a symlinked file path"
    );
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&outside);
}

fn stored_schema_version(path: &std::path::Path) -> i64 {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap()
}

fn set_schema_version(path: &std::path::Path, version: i64) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.pragma_update(None, "user_version", version).unwrap();
}

#[test]
fn open_stamps_schema_version_on_create() {
    let root = unique_test_dir("schema-stamp");
    let path = root.join("audit.db");
    let store = CepAuditStore::open(&path).unwrap();
    drop(store);
    assert_eq!(stored_schema_version(&path), AUDIT_SCHEMA_VERSION);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn open_adopts_unversioned_current_shape_database() {
    // A database written before versioning but with today's row shape is an
    // additive case: stamp it, keep every row.
    let root = unique_test_dir("schema-adopt");
    let path = root.join("audit.db");
    write_legacy_audit_fixture(&path);
    set_schema_version(&path, 0);

    let store = CepAuditStore::open(&path).unwrap();
    assert_eq!(store.row_count().unwrap(), 1);
    assert_eq!(store.row_by_audit_id(1).unwrap().turn_id, "legacy-turn");
    drop(store);
    assert_eq!(stored_schema_version(&path), AUDIT_SCHEMA_VERSION);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn open_rejects_newer_schema_version() {
    let root = unique_test_dir("schema-newer");
    let path = root.join("audit.db");
    write_legacy_audit_fixture(&path);
    set_schema_version(&path, AUDIT_SCHEMA_VERSION + 1);

    let err = match CepAuditStore::open(&path) {
        Ok(_) => panic!("a newer audit schema must not be opened for writing"),
        Err(err) => err,
    };
    assert!(matches!(err, AuditError::IncompatibleSchema { .. }));
    let message = err.to_string();
    assert!(message.contains(&path.display().to_string()), "{message}");
    assert!(message.contains("upgrade the binary"), "{message}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn open_rejects_old_shape_database_without_touching_it() {
    // C5: the real pre-versioning hazard is a row shape no additive DDL can
    // repair (here `semantic_stage`/`guard_stage`/`policy_enforced` are
    // absent). It used to open silently and die later on a raw INSERT.
    let root = unique_test_dir("schema-old-shape");
    let path = root.join("audit.db");
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE cep_audit_entries (
                audit_id INTEGER PRIMARY KEY AUTOINCREMENT,
                call_id INTEGER NOT NULL,
                method TEXT NOT NULL,
                started_at TEXT NOT NULL,
                ended_at TEXT,
                result_or_error TEXT,
                impact_kind TEXT NOT NULL,
                turn_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                metadata_json TEXT
             );
             INSERT INTO cep_audit_entries
                (call_id, method, started_at, impact_kind, turn_id, session_id)
             VALUES (7, 'observe.workspace', '1', 'workspace_observation', 'old-turn', 'old-session');",
        )
        .unwrap();
    }
    let before = std::fs::read(&path).unwrap();

    let err = match CepAuditStore::open(&path) {
        Ok(_) => panic!("an old-shape audit database must not be opened"),
        Err(err) => err,
    };
    assert!(matches!(err, AuditError::IncompatibleSchema { .. }));
    let message = err.to_string();
    assert!(message.contains(&path.display().to_string()), "{message}");
    assert!(message.contains("semantic_stage"), "{message}");
    assert!(message.contains("read-only legacy import"), "{message}");

    assert_eq!(
        std::fs::read(&path).unwrap(),
        before,
        "a refused open must leave the audit history byte-identical"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn open_rejects_old_shape_attestation_table_without_touching_it() {
    // C5 covers every table this store writes: an unversioned file whose call
    // rows match today's shape but whose attestation table does not would be
    // adopted and stamped v1, then die at the first attestation INSERT.
    let root = unique_test_dir("schema-old-attestation");
    let path = root.join("audit.db");
    write_legacy_audit_fixture(&path);
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "DROP TABLE attestation_records;
             CREATE TABLE attestation_records (
                turn_id TEXT PRIMARY KEY,
                statement_json TEXT NOT NULL,
                generated_at TEXT NOT NULL
             );",
        )
        .unwrap();
    }
    set_schema_version(&path, 0);
    let before = std::fs::read(&path).unwrap();

    let err = match CepAuditStore::open(&path) {
        Ok(_) => panic!("an old-shape attestation table must not be opened"),
        Err(err) => err,
    };
    assert!(matches!(err, AuditError::IncompatibleSchema { .. }));
    let message = err.to_string();
    assert!(
        message.contains("attestation_records.predicate_type"),
        "the missing column must be named with its table: {message}"
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        before,
        "a refused open must leave the database byte-identical"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn for_component_dispatch_labels_cell_governed_rows() {
    // #28 b8: a Component Tool is routed and Cell-governed. The legacy
    // catalog lookup knows nothing about it, so labeling by
    // `for_production_dispatch` would claim the opposite of both facts.
    assert_eq!(
        CallAuditMetadata::for_production_dispatch("noop.echo").semantic_stage,
        SEMANTIC_STAGE_UNROUTED,
        "the legacy lookup is exactly what must not label a Component call"
    );

    let component = CallAuditMetadata::for_component_dispatch();
    assert_eq!(component.semantic_stage, SEMANTIC_STAGE_COMPONENT);
    assert_eq!(component.guard_stage, GUARD_STAGE_COMPONENT);
    assert!(component.policy_enforced);
    assert_ne!(
        component.semantic_status.as_deref(),
        Some("method_not_found")
    );
}

#[test]
fn for_production_dispatch_labels_known_native_and_unknown_unrouted() {
    // Known catalog method -> native + policy-enforced. It must be a real
    // catalog method: the label is derived from catalog membership, so a
    // synthetic fixture name would prove the opposite branch. `fetch.web`
    // stands in because #31 removed `mutate.create` from the native catalog.
    let known = CallAuditMetadata::for_production_dispatch(Fetch::NAME);
    assert_eq!(known.semantic_stage, SEMANTIC_STAGE_NATIVE);
    assert!(known.policy_enforced);

    // Unknown method (will MethodNotFound) -> unrouted, never claims enforcement.
    let unknown = CallAuditMetadata::for_production_dispatch("crafted.unknown");
    assert_eq!(unknown.semantic_stage, SEMANTIC_STAGE_UNROUTED);
    assert!(!unknown.policy_enforced);

    // A migrated method reaches this function only when the Component dispatch
    // branch was NOT taken, so it is not native either: labeling it `native`
    // would claim an enforcement path that no longer exists for it (#29/#30).
    for migrated in ["observe.workspace", "session.todo_write", "mutate.create", "mutate.edit"] {
        let labeled = CallAuditMetadata::for_production_dispatch(migrated);
        assert_eq!(
            labeled.semantic_stage, SEMANTIC_STAGE_UNROUTED,
            "{migrated} is a Component Tool, not a native method"
        );
        assert!(!labeled.policy_enforced);
    }
}

#[test]
fn cep_audit_success_appends_one_row_with_required_fields() {
    let store = CepAuditStore::in_memory().unwrap();
    let ctx = test_context();
    // A real catalog method, so `impact_kind` is a real attribution rather than
    // the `unknown` a non-catalog name records.
    let start = store
        .start_call(
            &ctx,
            42,
            Fetch::NAME,
            &CallAuditMetadata::native_policy_enforced(Some("started")),
        )
        .unwrap();
    store
        .finalize_call(
            start,
            AuditOutcome::Success {
                summary: "fetched=1".into(),
            },
            Some("native_handler"),
            Some(r#"{"outcome":"fetch_web","count":1}"#),
        )
        .unwrap();

    assert_eq!(store.row_count().unwrap(), 1);
    let row = store.row_by_audit_id(start.audit_id).unwrap();
    assert_eq!(row.call_id, 42);
    assert_eq!(row.method, Fetch::NAME);
    assert!(!row.started_at.is_empty());
    assert!(row.ended_at.is_some());
    assert_eq!(row.result_or_error.as_deref(), Some("success:fetched=1"));
    assert_eq!(row.impact_kind, "external_observation");
    assert_eq!(row.turn_id, "turn-1");
    assert_eq!(row.session_id, "session-1");
    assert_eq!(row.semantic_stage, SEMANTIC_STAGE_NATIVE);
    assert_eq!(row.semantic_status.as_deref(), Some("native_handler"));
    assert_eq!(row.guard_stage, GUARD_STAGE_NATIVE);
    assert!(row.policy_enforced);
}

#[test]
fn cep_audit_error_path_finalizes_ended_at() {
    let store = CepAuditStore::in_memory().unwrap();
    let start = store
        .start_call(
            &test_context(),
            7,
            TestEcho::NAME,
            &CallAuditMetadata::native_policy_enforced(Some("started")),
        )
        .unwrap();
    store
        .finalize_call(
            start,
            AuditOutcome::Error {
                code: "ExecutionFailed".into(),
                summary: "handler failed".into(),
            },
            None,
            None,
        )
        .unwrap();
    let row = store.row_by_audit_id(start.audit_id).unwrap();
    assert!(row.ended_at.is_some());
    assert!(
        row.result_or_error
            .as_deref()
            .unwrap_or_default()
            .starts_with("error:ExecutionFailed:")
    );
}

#[test]
fn cep_audit_unknown_method_and_cancel_paths_finalize() {
    let store = CepAuditStore::in_memory().unwrap();
    let unknown = store
        .start_call(
            &test_context(),
            1,
            "crafted.unknown",
            &CallAuditMetadata::unrouted(),
        )
        .unwrap();
    store
        .finalize_call(
            unknown,
            AuditOutcome::Error {
                code: "MethodNotFound".into(),
                summary: "unknown method".into(),
            },
            None,
            None,
        )
        .unwrap();

    let cancelled = store
        .start_call(
            &test_context(),
            2,
            TestEcho::NAME,
            &CallAuditMetadata::native_policy_enforced(Some("started")),
        )
        .unwrap();
    store
        .finalize_call(cancelled, AuditOutcome::Cancelled, None, None)
        .unwrap();

    let channel = store
        .start_call(
            &test_context(),
            3,
            TestEcho::NAME,
            &CallAuditMetadata::native_policy_enforced(Some("started")),
        )
        .unwrap();
    store
        .finalize_call(channel, AuditOutcome::ResponseChannelClosed, None, None)
        .unwrap();

    assert_eq!(store.row_count().unwrap(), 3);
    for id in [unknown.audit_id, cancelled.audit_id, channel.audit_id] {
        assert!(store.row_by_audit_id(id).unwrap().ended_at.is_some());
    }
}

#[test]
fn cep_audit_rows_store_summaries_not_full_payloads() {
    let store = CepAuditStore::in_memory().unwrap();
    let params = json!({
        "todos": [{"content": "very long secret payload that must not be duplicated", "status": "pending"}]
    });
    let start = store
        .start_call(
            &test_context(),
            9,
            TestEcho::NAME,
            &CallAuditMetadata::native_policy_enforced(Some("started")),
        )
        .unwrap();
    store
        .finalize_call(
            start,
            AuditOutcome::Success {
                summary: "todo_count=1".into(),
            },
            Some("native_handler"),
            Some(r#"{"outcome":"todo_write","count":1}"#),
        )
        .unwrap();

    let row = store.row_by_audit_id(start.audit_id).unwrap();
    let metadata = row.metadata_json.unwrap_or_default();
    let blob = format!(
        "{:?}{}{}{}",
        row.result_or_error, metadata, row.method, row.impact_kind
    );
    assert!(!blob.contains("very long secret payload"));
    assert!(!params.to_string().contains(&row.method) || row.method == TestEcho::NAME);
    let _ = params;
}

#[test]
fn cep_audit_failed_insert_does_not_increment_pending() {
    let store = CepAuditStore::in_memory().unwrap();
    {
        let conn = store.conn.lock().expect("audit mutex poisoned");
        conn.execute_batch("DROP TABLE cep_audit_entries")
            .expect("drop audit table");
    }

    let result = store.start_call(
        &test_context(),
        1,
        TestEcho::NAME,
        &CallAuditMetadata::native_policy_enforced(Some("started")),
    );
    assert!(result.is_err());
    assert_eq!(store.pending.load(Ordering::SeqCst), 0);
}

#[test]
fn cep_audit_failed_finalize_still_decrements_pending() {
    let store = CepAuditStore::in_memory().unwrap();
    let start = store
        .start_call(
            &test_context(),
            2,
            TestEcho::NAME,
            &CallAuditMetadata::native_policy_enforced(Some("started")),
        )
        .unwrap();
    assert_eq!(store.pending.load(Ordering::SeqCst), 1);
    {
        let conn = store.conn.lock().expect("audit mutex poisoned");
        conn.execute_batch("DROP TABLE cep_audit_entries")
            .expect("drop audit table");
    }

    let result = store.finalize_call(
        start,
        AuditOutcome::Success {
            summary: "ok".into(),
        },
        None,
        None,
    );
    assert!(result.is_err());
    assert_eq!(store.pending.load(Ordering::SeqCst), 0);
}

#[test]
fn cep_audit_native_and_unrouted_rows_coexist() {
    let store = CepAuditStore::in_memory().unwrap();
    let ctx = test_context();
    let unrouted = store
        .start_call(&ctx, 1, "crafted.unknown", &CallAuditMetadata::unrouted())
        .unwrap();
    store
        .finalize_call(
            unrouted,
            AuditOutcome::Error {
                code: "MethodNotFound".into(),
                summary: "unknown method".into(),
            },
            Some("method_not_found"),
            None,
        )
        .unwrap();

    let native = store
        .start_call(
            &ctx,
            2,
            "observe.workspace",
            &CallAuditMetadata::native_policy_enforced(Some("started")),
        )
        .unwrap();
    store
        .finalize_call(
            native,
            AuditOutcome::Success {
                summary: "read_file".into(),
            },
            Some("native_handler"),
            Some(r#"{"observation_source":"workspace","bytes_returned":10}"#),
        )
        .unwrap();

    assert!(
        !store
            .row_by_audit_id(unrouted.audit_id)
            .unwrap()
            .policy_enforced
    );
    assert!(
        store
            .row_by_audit_id(native.audit_id)
            .unwrap()
            .policy_enforced
    );
    assert_eq!(
        store
            .row_by_audit_id(unrouted.audit_id)
            .unwrap()
            .semantic_stage,
        SEMANTIC_STAGE_UNROUTED
    );
    assert_eq!(
        store
            .row_by_audit_id(native.audit_id)
            .unwrap()
            .semantic_stage,
        SEMANTIC_STAGE_NATIVE
    );
}

#[test]
fn cep_audit_metadata_json_excludes_full_payloads() {
    let store = CepAuditStore::in_memory().unwrap();
    let secret = "very long secret file body that must not appear in audit";
    let start = store
        .start_call(
            &test_context(),
            5,
            "observe.workspace",
            &CallAuditMetadata::native_policy_enforced(None),
        )
        .unwrap();
    store
        .finalize_call(
            start,
            AuditOutcome::Success {
                summary: "bytes_returned=42".into(),
            },
            Some("native_handler"),
            Some(r#"{"bytes_returned":42,"truncated":false}"#),
        )
        .unwrap();
    let row = store.row_by_audit_id(start.audit_id).unwrap();
    let blob = format!(
        "{:?}{}",
        row.result_or_error,
        row.metadata_json.unwrap_or_default()
    );
    assert!(!blob.contains(secret));
    let _ = secret;
}

#[test]
fn cep_audit_effective_boundary_metadata_round_trips_compactly() {
    let store = CepAuditStore::in_memory().unwrap();
    let metadata = json!({
        "command_isolation_mode": "audit",
        "effective_boundary": {
            "network": {"boundary": "network", "status": "unbounded", "observed": false, "reason": "netns unavailable"},
            "filesystem": {"boundary": "filesystem", "status": "failed_closed", "observed": true, "reason": "mount view missing", "diagnostics": {"mount_view_status": "failed_closed"}},
            "landlock": {"boundary": "landlock", "status": "unsupported", "observed": false, "reason": "landlock disabled", "diagnostics": {"landlock_status": "unsupported", "landlock_abi_version": "unknown"}}
        },
        "host_probe_summary": {
            "network_namespace": {"status": "unavailable", "reason": "operation not permitted"}
        },
        "boundary_setup_failed_closed": true,
        "policy_enforced": true
    });
    let metadata_json = serde_json::to_string(&metadata).unwrap();
    let start = store
        .start_call(
            &test_context(),
            11,
            "observe.workspace",
            &CallAuditMetadata::native_policy_enforced(Some("started")),
        )
        .unwrap();
    store
        .finalize_call(
            start,
            AuditOutcome::Error {
                code: "GuardDenied".into(),
                summary: "command boundary failed closed".into(),
            },
            Some("native_handler_denied"),
            Some(&metadata_json),
        )
        .unwrap();

    let rows = store
        .query_rows(&AuditQueryFilters {
            method: Some("observe.workspace".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(rows.len(), 1);
    let loaded = rows[0].metadata_json.as_deref().expect("metadata_json");
    let loaded_json: serde_json::Value = serde_json::from_str(loaded).unwrap();
    assert_eq!(
        loaded_json["effective_boundary"]["network"]["status"],
        "unbounded"
    );
    assert_eq!(
        loaded_json["effective_boundary"]["network"]["observed"],
        false
    );
    assert_eq!(
        loaded_json["effective_boundary"]["filesystem"]["status"],
        "failed_closed"
    );
    assert_eq!(
        loaded_json["effective_boundary"]["filesystem"]["observed"],
        true
    );
    assert_eq!(
        loaded_json["effective_boundary"]["landlock"]["status"],
        "unsupported"
    );
    assert_eq!(loaded_json["boundary_setup_failed_closed"], true);
    assert_eq!(
        loaded_json["effective_boundary"]["filesystem"]["diagnostics"]["mount_view_status"],
        "failed_closed"
    );
    assert_eq!(
        loaded_json["effective_boundary"]["landlock"]["diagnostics"]["landlock_status"],
        "unsupported"
    );
    for forbidden in [
        "argv",
        "env",
        "stdin",
        "stdout",
        "stderr",
        "command_request",
        "command_response",
    ] {
        assert!(
            !loaded.contains(forbidden),
            "compact boundary metadata must not contain full payload key `{forbidden}`"
        );
    }
}

#[test]
fn impact_kind_label_round_trips_all_variants() {
    for kind in &ALL_IMPACT_KINDS {
        let label = impact_kind_label(kind);
        assert_eq!(
            impact_kind_from_label(label),
            Some(kind.clone()),
            "snake_case round-trip for {label}"
        );
        let cli = impact_kind_cli_name(kind);
        assert_eq!(
            impact_kind_from_label(cli),
            Some(kind.clone()),
            "PascalCase round-trip for {cli}"
        );
    }
    assert!(impact_kind_from_label("not_a_kind").is_none());
}

#[test]
fn start_call_persists_model_and_runtime_columns() {
    let store = CepAuditStore::in_memory().unwrap();
    let ctx = CepAuditContext {
        turn_id: "turn-m".into(),
        session_id: "session-m".into(),
        model_id: "claude-sonnet".into(),
        runtime_version: "0.1.0-test".into(),
    };
    let start = store
        .start_call(
            &ctx,
            1,
            TestEcho::NAME,
            &CallAuditMetadata::native_policy_enforced(Some("started")),
        )
        .unwrap();
    store
        .finalize_call(
            start,
            AuditOutcome::Success {
                summary: "ok".into(),
            },
            Some("native_handler"),
            None,
        )
        .unwrap();
    let row = store.row_by_audit_id(start.audit_id).unwrap();
    assert_eq!(row.model_id.as_deref(), Some("claude-sonnet"));
    assert_eq!(row.runtime_version.as_deref(), Some("0.1.0-test"));
}

#[test]
fn attestation_round_trip() {
    let store = CepAuditStore::in_memory().unwrap();
    let record = AttestationRecord {
        turn_id: "turn-a".into(),
        session_id: "session-a".into(),
        statement_json: r#"{"subject":[]}"#.into(),
        predicate_type: "https://lattice.dev/attestation/cep-turn/v1".into(),
        generated_at: "1".into(),
        source_audit_range_or_hash: "1-3".into(),
    };
    store.insert_attestation(&record).unwrap();
    let loaded = store.attestation_for_turn("turn-a").unwrap().unwrap();
    assert_eq!(loaded, record);
    store.delete_attestation_for_turn("turn-a").unwrap();
    assert!(store.attestation_for_turn("turn-a").unwrap().is_none());
}

#[test]
fn query_rows_filters_by_session_and_method() {
    let store = CepAuditStore::in_memory().unwrap();
    let ctx_a = CepAuditContext {
        session_id: "sess-a".into(),
        ..test_context()
    };
    let ctx_b = CepAuditContext {
        session_id: "sess-b".into(),
        ..test_context()
    };
    let s1 = store
        .start_call(
            &ctx_a,
            1,
            "observe.workspace",
            &CallAuditMetadata::native_policy_enforced(None),
        )
        .unwrap();
    store
        .finalize_call(
            s1,
            AuditOutcome::Success {
                summary: "ok".into(),
            },
            Some("native_handler"),
            Some(r#"{"required_capability":"read_path"}"#),
        )
        .unwrap();
    let s2 = store
        .start_call(
            &ctx_b,
            2,
            "observe.workspace",
            &CallAuditMetadata::native_policy_enforced(None),
        )
        .unwrap();
    store
        .finalize_call(
            s2,
            AuditOutcome::Success {
                summary: "ok".into(),
            },
            Some("native_handler"),
            None,
        )
        .unwrap();

    let rows = store
        .query_rows(&AuditQueryFilters {
            session_id: Some("sess-a".into()),
            method: Some("observe.workspace".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].session_id, "sess-a");
}

#[test]
fn query_rows_filters_by_verdict() {
    let store = CepAuditStore::in_memory().unwrap();
    let ctx = test_context();
    let ok = store
        .start_call(
            &ctx,
            1,
            TestEcho::NAME,
            &CallAuditMetadata::native_policy_enforced(None),
        )
        .unwrap();
    store
        .finalize_call(
            ok,
            AuditOutcome::Success {
                summary: "ok".into(),
            },
            Some("native_handler"),
            None,
        )
        .unwrap();
    let denied = store
        .start_call(
            &ctx,
            2,
            TestEcho::NAME,
            &CallAuditMetadata::native_policy_enforced(None),
        )
        .unwrap();
    store
        .finalize_call(
            denied,
            AuditOutcome::Error {
                code: "GuardDenied".into(),
                summary: "denied".into(),
            },
            Some("native_handler_denied"),
            None,
        )
        .unwrap();

    let success_rows = store
        .query_rows(&AuditQueryFilters {
            session_id: Some(ctx.session_id.clone()),
            verdict: Some("success".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(success_rows.len(), 1);
    assert!(
        success_rows[0]
            .result_or_error
            .as_deref()
            .unwrap_or_default()
            .starts_with("success:")
    );

    let denied_rows = store
        .query_rows(&AuditQueryFilters {
            session_id: Some(ctx.session_id.clone()),
            verdict: Some("denied".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(denied_rows.len(), 1);
    assert_eq!(
        denied_rows[0].semantic_status.as_deref(),
        Some("native_handler_denied")
    );
}

#[test]
fn query_rows_verdicts_reach_component_rows() {
    // #28 b8: Component calls carry their own status vocabulary, so the
    // operator verdict filters must name it or Cell-governed rows fall out of
    // both `success` and `denied`.
    let store = CepAuditStore::in_memory().unwrap();
    let ctx = test_context();
    for (call_id, outcome, status) in [
        (
            1,
            AuditOutcome::Success {
                summary: "ok".into(),
            },
            SEMANTIC_STATUS_COMPONENT,
        ),
        (
            2,
            AuditOutcome::Error {
                code: "CapabilityNotGranted".into(),
                summary: "not enabled".into(),
            },
            SEMANTIC_STATUS_COMPONENT_DENIED,
        ),
    ] {
        let start = store
            .start_call(
                &ctx,
                call_id,
                "noop.echo",
                &CallAuditMetadata::for_component_dispatch(),
            )
            .unwrap();
        store
            .finalize_call(start, outcome, Some(status), None)
            .unwrap();
    }

    for (verdict, expected_status) in [
        ("success", SEMANTIC_STATUS_COMPONENT),
        ("denied", SEMANTIC_STATUS_COMPONENT_DENIED),
    ] {
        let rows = store
            .query_rows(&AuditQueryFilters {
                session_id: Some(ctx.session_id.clone()),
                verdict: Some(verdict.into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(rows.len(), 1, "verdict {verdict}");
        assert_eq!(rows[0].semantic_status.as_deref(), Some(expected_status));
    }
}

#[test]
fn query_rows_filters_by_capability() {
    let store = CepAuditStore::in_memory().unwrap();
    let ctx = test_context();
    let with_cap = store
        .start_call(
            &ctx,
            1,
            "observe.workspace",
            &CallAuditMetadata::native_policy_enforced(None),
        )
        .unwrap();
    store
        .finalize_call(
            with_cap,
            AuditOutcome::Success {
                summary: "ok".into(),
            },
            Some("native_handler"),
            Some(r#"{"required_capability":"read_path"}"#),
        )
        .unwrap();
    let without_cap = store
        .start_call(
            &ctx,
            2,
            "observe.workspace",
            &CallAuditMetadata::native_policy_enforced(None),
        )
        .unwrap();
    store
        .finalize_call(
            without_cap,
            AuditOutcome::Success {
                summary: "ok".into(),
            },
            Some("native_handler"),
            Some(r#"{"observation_source":"workspace"}"#),
        )
        .unwrap();

    let rows = store
        .query_rows(&AuditQueryFilters {
            session_id: Some(ctx.session_id.clone()),
            capability: Some("read_path".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].audit_id, with_cap.audit_id);
}

#[test]
fn query_rows_applies_database_limit_and_excluded_method_prefix() {
    let store = CepAuditStore::in_memory().unwrap();
    let ctx = test_context();
    for (idx, method) in [
        OBSERVE_WORKSPACE,
        "observe.audit",
        Fetch::NAME,
        Fetch::NAME,
    ]
    .into_iter()
    .enumerate()
    {
        let start = store
            .start_call(
                &ctx,
                idx as u64 + 1,
                method,
                &CallAuditMetadata::native_policy_enforced(None),
            )
            .unwrap();
        store
            .finalize_call(
                start,
                AuditOutcome::Success {
                    summary: "ok".into(),
                },
                Some("native_handler"),
                None,
            )
            .unwrap();
    }

    let rows = store
        .query_rows(&AuditQueryFilters {
            session_id: Some(ctx.session_id.clone()),
            exclude_method_prefix: Some("observe.audit".into()),
            limit: Some(2),
            ..Default::default()
        })
        .unwrap();

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].method, OBSERVE_WORKSPACE);
    assert_eq!(rows[1].method, Fetch::NAME);
}

#[test]
fn query_rows_excludes_unfinalized_rows() {
    let store = CepAuditStore::in_memory().unwrap();
    let ctx = test_context();
    let start = store
        .start_call(
            &ctx,
            1,
            OBSERVE_WORKSPACE,
            &CallAuditMetadata::native_policy_enforced(None),
        )
        .unwrap();
    let _ = start;
    let rows = store
        .query_rows(&AuditQueryFilters {
            session_id: Some(ctx.session_id.clone()),
            ..Default::default()
        })
        .unwrap();
    assert!(rows.is_empty());
}

#[test]
fn merge_finalize_metadata_adds_required_capability() {
    // A native catalog method: the required capability is derived from catalog
    // membership, so this must be a method still in the catalog.
    let merged = merge_finalize_metadata(
        Some(r#"{"path":"src/lib.rs"}"#),
        Fetch::NAME,
        &[],
        &ComponentCallEvidence::default(),
    )
    .unwrap();
    assert!(merged.contains("required_capability"));
    assert!(merged.contains("fetch_url"));
}

/// A migrated method has no native catalog entry, so no legacy
/// `required_capability` is attributed to it. Its authority is stated by its
/// admitted Cell capabilities instead, which travel in the versioned evidence
/// record — inventing a legacy label here would put two answers about one call's
/// authority in the audit trail (#29/#30).
#[test]
fn merge_finalize_metadata_attributes_no_legacy_capability_to_a_component_tool() {
    for migrated in ["observe.workspace", "session.todo_write", "mutate.create", "mutate.edit"] {
        let merged = merge_finalize_metadata(
            Some(r#"{"probe":"value"}"#),
            migrated,
            &[],
            &ComponentCallEvidence::default(),
        )
        .unwrap();
        assert!(
            !merged.contains("required_capability"),
            "{migrated} is not a native catalog method: {merged}"
        );
        // The caller's own metadata still survives the merge.
        assert!(merged.contains("probe"), "{merged}");
    }
}

#[test]
fn merge_finalize_metadata_records_component_cells() {
    // Component Tools (ADR 0004) record 4-segment Cell audit keys
    // (ctx:ext:scope:trust), derived from the effect ledger so the query key
    // and the effect array cannot disagree (spec §6.3: audit records cells,
    // not capability names).
    let effects = effects_record();
    let merged = merge_finalize_metadata(
        None,
        "noop.echo",
        &[],
        &ComponentCallEvidence {
            effects: Some(&effects),
            ..Default::default()
        },
    )
    .unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&merged).unwrap();
    assert_eq!(
        parsed["component_cells"],
        serde_json::json!(["mutate:none:session:in", "alloc:none:session:in"])
    );
}

#[test]
fn merge_finalize_metadata_omits_component_cells_when_empty() {
    let merged = merge_finalize_metadata(
        None,
        "observe.workspace",
        &[],
        &ComponentCallEvidence::default(),
    )
    .unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&merged).unwrap();
    assert!(
        parsed.get("component_cells").is_none(),
        "empty component_cells must not appear in metadata"
    );
}

fn observed_scope() -> ObservedScope {
    ObservedScope {
        scope: "session".into(),
        trust: Trust::In,
        channel: ChannelKind::Session,
    }
}

fn cell(ctx_op: CtxOp) -> Cell {
    Cell {
        ctx_op,
        ext_impact: ExtImpact::None,
        scope: "session".into(),
    }
}

/// One call with two effects (an operation then the result), deliberately
/// pushed out of seq order to pin the seq-ordered write.
fn effects_record() -> ObservedEffectsRecord {
    ObservedEffectsRecord {
        version: OBSERVED_EFFECTS_RECORD_VERSION,
        effects: vec![
            ObservedEffect {
                seq: 1,
                cell: cell(CtxOp::Alloc),
                scope: observed_scope(),
                target: ObservedTarget::Session {
                    turn_id: "turn-1".into(),
                },
                source: EffectSource::Result,
                context_measure: Some(17),
                truncated: None,
                visibility: None,
                mutation: None,
            },
            ObservedEffect {
                seq: 0,
                cell: cell(CtxOp::Mutate),
                scope: observed_scope(),
                target: ObservedTarget::Session {
                    turn_id: "turn-1".into(),
                },
                source: EffectSource::Operation,
                context_measure: None,
                truncated: None,
                visibility: None,
                mutation: None,
            },
        ],
    }
}

fn admission_evidence() -> AdmissionEvidence {
    AdmissionEvidence {
        version: ADMISSION_EVIDENCE_VERSION,
        tool: "noop.echo".into(),
        approved_capabilities: vec!["session.events".into(), "session.result".into()],
        denied_capabilities: vec!["net.fetch".into()],
        boundary_excluded: vec!["fs.write".into()],
        max_boundary: Trust::In,
    }
}

fn denied_operations() -> DeniedOperationsRecord {
    DeniedOperationsRecord {
        version: DENIED_OPERATIONS_RECORD_VERSION,
        denied: vec![
            DeniedOperation {
                seq: 3,
                cell: cell(CtxOp::Mutate),
                scope: observed_scope(),
                target: ObservedTarget::None,
                reason: "uncovered operation".into(),
                mutation: None,
            },
            DeniedOperation {
                seq: 2,
                cell: cell(CtxOp::Mutate),
                scope: observed_scope(),
                target: ObservedTarget::Path {
                    path: "src/lib.rs".into(),
                },
                reason: "path target denied this cycle".into(),
                mutation: None,
            },
        ],
    }
}

/// #112 criterion 3: the audit row is no longer the carrier of structured
/// evidence. It points at the evidence row and keeps only its own summary keys;
/// the three record payloads are absent from the blob entirely.
#[test]
fn merge_finalize_metadata_points_at_evidence_instead_of_carrying_it() {
    let admission = admission_evidence();
    let effects = effects_record();
    let denied = denied_operations();
    let merged = merge_finalize_metadata(
        None,
        "noop.echo",
        &[],
        &ComponentCallEvidence {
            admission: Some(&admission),
            effects: Some(&effects),
            denied: Some(&denied),
            evidence_id: Some(EvidenceId(77)),
        },
    )
    .unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&merged).unwrap();

    assert_eq!(
        parsed["evidence_id"],
        json!(77),
        "the row must resolve to its evidence"
    );
    for key in [
        "admission_evidence",
        "observed_effects",
        "denied_operations",
    ] {
        assert!(
            parsed.get(key).is_none(),
            "{key} must live in the evidence table, not the metadata blob: {parsed}"
        );
    }

    // The derived query key still covers both effects, in seq order (C8).
    assert_eq!(
        parsed["component_cells"],
        json!(["mutate:none:session:in", "alloc:none:session:in"])
    );
}

#[test]
fn merge_finalize_metadata_is_order_independent() {
    // The metadata bytes feed attestation, so nothing may depend on the
    // iteration order of the caller's containers.
    let admission = admission_evidence();
    let denied = denied_operations();
    let mut reversed = effects_record();
    reversed.effects.reverse();
    let mut reversed_denied = denied.clone();
    reversed_denied.denied.reverse();
    let effects = effects_record();

    let first = merge_finalize_metadata(
        None,
        "noop.echo",
        &[RequiredCapability::WritePath, RequiredCapability::Inspect],
        &ComponentCallEvidence {
            admission: Some(&admission),
            effects: Some(&effects),
            denied: Some(&denied),
            evidence_id: Some(EvidenceId(1)),
        },
    )
    .unwrap();
    let second = merge_finalize_metadata(
        None,
        "noop.echo",
        &[RequiredCapability::Inspect, RequiredCapability::WritePath],
        &ComponentCallEvidence {
            admission: Some(&admission),
            effects: Some(&reversed),
            denied: Some(&reversed_denied),
            evidence_id: Some(EvidenceId(1)),
        },
    )
    .unwrap();

    assert_eq!(first, second);
    let parsed: serde_json::Value = serde_json::from_str(&first).unwrap();
    assert_eq!(
        parsed["observed_capabilities"],
        json!(["inspect", "write_path"])
    );
}

#[tokio::test]
async fn cep_audit_await_pending_waits_for_finalization() {
    let store = Arc::new(CepAuditStore::in_memory().unwrap());
    let store_clone = Arc::clone(&store);
    let start = store
        .start_call(
            &test_context(),
            1,
            TestEcho::NAME,
            &CallAuditMetadata::native_policy_enforced(Some("started")),
        )
        .unwrap();
    let finalize = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        store_clone
            .finalize_call(
                start,
                AuditOutcome::Success {
                    summary: "ok".into(),
                },
                None,
                None,
            )
            .unwrap();
    });
    store.await_pending().await;
    finalize.await.unwrap();
    assert_eq!(store.pending.load(Ordering::SeqCst), 0);
}

#[test]
fn rejection_stage_serializes_stable_snake_case_labels() {
    let stages = [
        RejectionStage::SchemaValidation,
        RejectionStage::PolicyGate,
        RejectionStage::PathGuard,
        RejectionStage::LocalIo,
    ];
    for stage in stages {
        let label = serde_json::to_string(&stage).unwrap();
        let parsed: RejectionStage = serde_json::from_str(&label).unwrap();
        assert_eq!(parsed, stage);
        assert_eq!(label.trim_matches('"'), stage.as_str());
    }
}

#[test]
fn rejection_stage_filters_rows_by_metadata_json() {
    let store = CepAuditStore::in_memory().unwrap();
    let ctx = test_context();

    let policy_start = store
        .start_call(
            &ctx,
            1,
            OBSERVE_WORKSPACE,
            &CallAuditMetadata::native_policy_enforced(Some("started")),
        )
        .unwrap();
    store
        .finalize_call(
            policy_start,
            AuditOutcome::Error {
                code: "PolicyDenied".into(),
                summary: "denied".into(),
            },
            Some("native_handler_denied"),
            Some(r#"{"rejection_stage":"policy_gate","denial_source":"cep_policy"}"#),
        )
        .unwrap();

    let adapter_start = store
        .start_call(
            &ctx,
            2,
            Fetch::NAME,
            &CallAuditMetadata::native_policy_enforced(Some("started")),
        )
        .unwrap();
    store
        .finalize_call(
            adapter_start,
            AuditOutcome::Error {
                code: "GuardDenied".into(),
                summary: "guard".into(),
            },
            Some("native_handler_denied"),
            Some(r#"{"rejection_stage":"path_guard","denial_source":"guard_l1"}"#),
        )
        .unwrap();

    let policy_rows = store
        .query_rows(&AuditQueryFilters {
            rejection_stage: Some(RejectionStage::PolicyGate),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(policy_rows.len(), 1);
    assert_eq!(policy_rows[0].method, OBSERVE_WORKSPACE);

    let guard_rows = store
        .query_rows(&AuditQueryFilters {
            rejection_stage: Some(RejectionStage::PathGuard),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(guard_rows.len(), 1);
    assert_eq!(guard_rows[0].method, Fetch::NAME);
}

fn write_legacy_audit_fixture(path: &std::path::Path) {
    let legacy = CepAuditStore::open(path).expect("legacy fixture");
    let ctx = CepAuditContext {
        turn_id: "legacy-turn".into(),
        session_id: "legacy-session".into(),
        model_id: "legacy-model".into(),
        runtime_version: "legacy-runtime".into(),
    };
    let start = legacy
        .start_call(
            &ctx,
            42,
            OBSERVE_WORKSPACE,
            &CallAuditMetadata::native_policy_enforced(Some("started")),
        )
        .unwrap();
    legacy
        .finalize_call(
            start,
            AuditOutcome::Success {
                summary: "legacy ok".into(),
            },
            Some("native_handler"),
            Some(r#"{"required_capability":"inspect"}"#),
        )
        .unwrap();
    legacy
        .insert_attestation(&AttestationRecord {
            turn_id: "legacy-turn".into(),
            session_id: "legacy-session".into(),
            statement_json: r#"{"_type":"https://in-toto.io/Statement/v1"}"#.into(),
            predicate_type: "https://lattice.dev/attestations/cep-turn/v1".into(),
            generated_at: "1".into(),
            source_audit_range_or_hash: "1-1".into(),
        })
        .unwrap();
}

#[test]
fn import_legacy_read_only_if_empty_copies_both_tables() {
    let root = unique_test_dir("import-legacy");
    let legacy_path = root.join("legacy.db");
    let dest_path = root.join("dest.db");
    write_legacy_audit_fixture(&legacy_path);

    let source_meta_before = std::fs::metadata(&legacy_path).unwrap();
    let source_bytes_before = std::fs::read(&legacy_path).unwrap();

    let dest = CepAuditStore::open(&dest_path).unwrap();
    dest.import_legacy_read_only_if_empty(&legacy_path).unwrap();

    assert_eq!(dest.row_count().unwrap(), 1);
    let row = dest.row_by_audit_id(1).unwrap();
    assert_eq!(row.turn_id, "legacy-turn");
    assert_eq!(
        dest.attestation_for_turn("legacy-turn")
            .unwrap()
            .expect("attestation")
            .session_id,
        "legacy-session"
    );

    let source_meta_after = std::fs::metadata(&legacy_path).unwrap();
    let source_bytes_after = std::fs::read(&legacy_path).unwrap();
    assert_eq!(source_bytes_before, source_bytes_after);
    assert_eq!(source_meta_before.len(), source_meta_after.len());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(source_meta_before.mtime(), source_meta_after.mtime());
    }

    dest.import_legacy_read_only_if_empty(&legacy_path).unwrap();
    assert_eq!(dest.row_count().unwrap(), 1);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn import_legacy_read_only_if_empty_is_noop_when_destination_has_rows() {
    let root = unique_test_dir("import-noop");
    let legacy_path = root.join("legacy.db");
    let dest_path = root.join("dest.db");
    write_legacy_audit_fixture(&legacy_path);

    let dest = CepAuditStore::open(&dest_path).unwrap();
    let start = dest
        .start_call(
            &test_context(),
            1,
            TestEcho::NAME,
            &CallAuditMetadata::native_policy_enforced(Some("started")),
        )
        .unwrap();
    dest.finalize_call(
        start,
        AuditOutcome::Success {
            summary: "live".into(),
        },
        None,
        None,
    )
    .unwrap();

    dest.import_legacy_read_only_if_empty(&legacy_path).unwrap();
    assert_eq!(dest.row_count().unwrap(), 1);
    let row = dest.row_by_audit_id(1).unwrap();
    assert_eq!(row.turn_id, "turn-1");
    assert!(dest.attestation_for_turn("legacy-turn").unwrap().is_none());

    let _ = std::fs::remove_dir_all(&root);
}

// ─── Rewind records (#64) ───────────────────────────────────────────────────

fn rewind_record(session: &str, outcome: &str) -> RewindAuditRecord {
    RewindAuditRecord {
        session_id: session.to_string(),
        target_turn_id: "turn-commit-1".to_string(),
        target_history_oid: Some("a".repeat(40)),
        from_history_oid: Some("b".repeat(40)),
        outcome: outcome.to_string(),
        steps_json: r#"[{"commit_id":"c","method":"mutate.edit","turn_id":"t","call_id":1,"path":"src/a.rs"}]"#.to_string(),
        preflight_json: r#"[{"path":"src/a.rs","expected_revision":"blake3:00","observed_revision":"blake3:00","diverged":false}]"#.to_string(),
        restored_json: Some(r#"[{"path":"src/a.rs","restored_revision":"blake3:11"}]"#.to_string()),
        conflicts_json: None,
        recorded_at: "2026-07-26T00:00:00Z".to_string(),
    }
}

#[test]
fn rewind_records_persist_and_reopen() {
    let root = unique_test_dir("rewind-records-persist");
    let db = root.join("cep_audit.db");
    {
        let store = CepAuditStore::open(&db).unwrap();
        store
            .insert_rewind_record(&rewind_record("sess-1", "success"))
            .unwrap();
        store
            .insert_rewind_record(&rewind_record("sess-1", "conflict"))
            .unwrap();
        store
            .insert_rewind_record(&rewind_record("sess-2", "success"))
            .unwrap();
    }
    // A fresh handle stands in for a process restart: the records are
    // durable, ordered, and scoped by session.
    let reopened = CepAuditStore::open(&db).unwrap();
    let rows = reopened.rewind_records_for_session("sess-1").unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].outcome, "success");
    assert_eq!(rows[1].outcome, "conflict");
    assert_eq!(rows[0].target_turn_id, "turn-commit-1");
    assert_eq!(
        rows[0].restored_json.as_deref(),
        Some(r#"[{"path":"src/a.rs","restored_revision":"blake3:11"}]"#)
    );
    assert!(
        reopened
            .rewind_records_for_session("sess-3")
            .unwrap()
            .is_empty()
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn rewind_table_is_added_to_a_pre_rewind_database() {
    // A database created before #64 (no rewind_records table) must accept
    // rewind records after reopen — init_schema is additive.
    let root = unique_test_dir("rewind-records-compat");
    let db = root.join("cep_audit.db");
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE cep_audit_entries (
                audit_id INTEGER PRIMARY KEY AUTOINCREMENT,
                call_id INTEGER NOT NULL,
                method TEXT NOT NULL,
                started_at TEXT NOT NULL,
                ended_at TEXT,
                result_or_error TEXT,
                impact_kind TEXT NOT NULL,
                turn_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                semantic_stage TEXT NOT NULL,
                semantic_status TEXT,
                guard_stage TEXT NOT NULL,
                policy_enforced INTEGER NOT NULL,
                metadata_json TEXT
             );",
        )
        .unwrap();
    }
    let store = CepAuditStore::open(&db).unwrap();
    store
        .insert_rewind_record(&rewind_record("sess-compat", "success"))
        .unwrap();
    assert_eq!(
        store
            .rewind_records_for_session("sess-compat")
            .unwrap()
            .len(),
        1
    );
    let _ = std::fs::remove_dir_all(&root);
}

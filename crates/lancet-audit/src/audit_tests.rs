// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;
use lancet_catalog::methods::{
    AuditObserve, Create, Fetch, ObserveWorkspace, TodoWrite as TodoWriteMethod,
};
use lancet_catalog::CepMethod;
use serde_json::json;
use std::path::PathBuf;

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

#[test]
fn for_production_dispatch_labels_known_native_and_unknown_unrouted() {
    // Known catalog method -> native + policy-enforced.
    let known = CallAuditMetadata::for_production_dispatch(TodoWriteMethod::NAME);
    assert_eq!(known.semantic_stage, SEMANTIC_STAGE_NATIVE);
    assert!(known.policy_enforced);

    // Unknown method (will MethodNotFound) -> unrouted, never claims enforcement.
    let unknown = CallAuditMetadata::for_production_dispatch("crafted.unknown");
    assert_eq!(unknown.semantic_stage, SEMANTIC_STAGE_UNROUTED);
    assert!(!unknown.policy_enforced);
    assert_eq!(unknown.backend_invoked, Some(false));
}

#[test]
fn cep_audit_success_appends_one_row_with_required_fields() {
    let store = CepAuditStore::in_memory().unwrap();
    let ctx = test_context();
    let start = store
        .start_call(
            &ctx,
            42,
            TodoWriteMethod::NAME,
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

    assert_eq!(store.row_count().unwrap(), 1);
    let row = store.row_by_audit_id(start.audit_id).unwrap();
    assert_eq!(row.call_id, 42);
    assert_eq!(row.method, TodoWriteMethod::NAME);
    assert!(!row.started_at.is_empty());
    assert!(row.ended_at.is_some());
    assert_eq!(row.result_or_error.as_deref(), Some("success:todo_count=1"));
    assert_eq!(row.impact_kind, "session_state_mutation");
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
            TodoWriteMethod::NAME,
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
    assert!(row
        .result_or_error
        .as_deref()
        .unwrap_or_default()
        .starts_with("error:ExecutionFailed:"));
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
            TodoWriteMethod::NAME,
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
            TodoWriteMethod::NAME,
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
            TodoWriteMethod::NAME,
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
    assert!(!params.to_string().contains(&row.method) || row.method == TodoWriteMethod::NAME);
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
        TodoWriteMethod::NAME,
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
            TodoWriteMethod::NAME,
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
        "backend_invoked": false,
        "policy_enforced": true
    });
    let metadata_json = serde_json::to_string(&metadata).unwrap();
    let start = store
        .start_call(
            &test_context(),
            11,
            "exec.command_run",
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
            method: Some("exec.command_run".into()),
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
            TodoWriteMethod::NAME,
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
            TodoWriteMethod::NAME,
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
            TodoWriteMethod::NAME,
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
    assert!(success_rows[0]
        .result_or_error
        .as_deref()
        .unwrap_or_default()
        .starts_with("success:"));

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
        ObserveWorkspace::NAME,
        AuditObserve::NAME,
        Create::NAME,
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
    assert_eq!(rows[0].method, ObserveWorkspace::NAME);
    assert_eq!(rows[1].method, Create::NAME);
}

#[test]
fn merge_finalize_metadata_adds_required_capability() {
    let merged = merge_finalize_metadata(
        Some(r#"{"observation_source":"workspace"}"#),
        "observe.workspace",
        &[],
        None,
    )
    .unwrap();
    assert!(merged.contains("required_capability"));
    assert!(merged.contains("inspect"));
}

#[tokio::test]
async fn cep_audit_await_pending_waits_for_finalization() {
    let store = Arc::new(CepAuditStore::in_memory().unwrap());
    let store_clone = Arc::clone(&store);
    let start = store
        .start_call(
            &test_context(),
            1,
            TodoWriteMethod::NAME,
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

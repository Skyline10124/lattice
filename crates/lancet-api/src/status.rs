// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Public LANCET status API for frontends.

use std::path::Path;

use lancet_audit::{
    AuditQueryFilters, AuditQueryOrder, CepAuditStore, OperatorCellEvidence,
    materialize_operator_cell_evidence,
};
use serde::{Deserialize, Serialize};

use crate::config::audit_db_path_for_project;
use crate::error::{LancetError, LancetResult};

const RECENT_CALL_LIMIT: usize = 20;

/// Audit-derived LANCET status.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LancetStatusOutput {
    pub recent_calls: Vec<LancetRecentCallEntry>,
    pub last_guard_verdict: Option<String>,
}

/// One recent CEP call for frontend status rendering.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LancetRecentCallEntry {
    pub audit_id: i64,
    pub method: String,
    pub impact: String,
    pub verdict: Option<String>,
    pub duration_ms: Option<i64>,
    pub evidence: Option<OperatorCellEvidence>,
}

/// Build status data from the audit store without spawning a CEP server.
pub fn collect_status(project_root: &Path) -> LancetResult<LancetStatusOutput> {
    let db_path = audit_db_path_for_project(project_root)
        .map_err(|err| LancetError::Config(format!("resolve external audit path: {err}")))?;

    if !db_path.exists() {
        return Ok(LancetStatusOutput {
            recent_calls: vec![],
            last_guard_verdict: None,
        });
    }

    let store = CepAuditStore::open(&db_path)
        .map_err(|err| LancetError::Internal(format!("open audit store: {err}")))?;

    let recent_rows = store
        .query_rows(&AuditQueryFilters {
            limit: Some(RECENT_CALL_LIMIT),
            order: AuditQueryOrder::Descending,
            ..Default::default()
        })
        .map_err(|err| LancetError::Internal(format!("query recent rows: {err}")))?;

    let last_guard_verdict = recent_rows.first().map(|row| {
        row.semantic_status
            .clone()
            .unwrap_or_else(|| row.guard_stage.clone())
    });

    let recent_calls = recent_rows
        .iter()
        .map(|row| {
            Ok(LancetRecentCallEntry {
                audit_id: row.audit_id,
                method: row.method.clone(),
                impact: row.impact_kind.clone(),
                verdict: row.semantic_status.clone(),
                duration_ms: compute_duration_ms(row.started_at.as_str(), row.ended_at.as_deref()),
                evidence: materialize_operator_cell_evidence(&store, row).map_err(|error| {
                    LancetError::Internal(format!(
                        "materialize status audit row {}: {error}",
                        row.audit_id
                    ))
                })?,
            })
        })
        .collect::<LancetResult<Vec<_>>>()?;

    Ok(LancetStatusOutput {
        recent_calls,
        last_guard_verdict,
    })
}

fn compute_duration_ms(started_at: &str, ended_at: Option<&str>) -> Option<i64> {
    let ended = ended_at?;
    let start = started_at.parse::<i64>().ok()?;
    let end = ended.parse::<i64>().ok()?;
    Some(end.saturating_sub(start))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lancet_audit::{
        AuditOutcome, CallAuditMetadata, CallEvidence, CepAuditContext, EvidenceOutcome,
        ScopedXdgStateHome,
    };
    use lancet_protocol::{
        ADMISSION_EVIDENCE_VERSION, AdmissionEvidence, DENIED_OPERATIONS_RECORD_VERSION,
        DeniedOperationsRecord, OBSERVED_EFFECTS_RECORD_VERSION, ObservedEffectsRecord, Trust,
    };
    use tempfile::TempDir;

    #[test]
    fn status_queries_only_the_twenty_newest_finalized_rows() {
        let _xdg = ScopedXdgStateHome::new();
        let project = TempDir::new().expect("project");
        let db_path = audit_db_path_for_project(project.path()).expect("audit path");
        std::fs::create_dir_all(db_path.parent().expect("audit parent")).expect("audit parent");
        let audit = CepAuditStore::open(&db_path).expect("open audit");
        let ctx = CepAuditContext {
            turn_id: "turn-status-bound".to_string(),
            session_id: "session-status-bound".to_string(),
            model_id: "status-model".to_string(),
            runtime_version: "test".to_string(),
        };
        for call_id in 1..=21 {
            let started = audit
                .start_call(
                    &ctx,
                    call_id,
                    "fetch.web",
                    &CallAuditMetadata::native_policy_enforced(Some("started")),
                )
                .expect("start native row");
            let verdict = format!("native_handler_{call_id}");
            audit
                .finalize_call(
                    started,
                    AuditOutcome::Success {
                        summary: format!("native result {call_id}"),
                    },
                    Some(&verdict),
                    None,
                )
                .expect("finalize native row");
        }
        drop(audit);

        // A DB-side DESC LIMIT 20 query never maps this out-of-window row.
        // A full-table load followed by an in-memory slice fails while decoding
        // it, which makes the bounded-read contract externally observable.
        let conn = rusqlite::Connection::open(&db_path).expect("open raw audit db");
        conn.execute(
            "UPDATE cep_audit_entries SET call_id = ?1 WHERE audit_id = 1",
            rusqlite::params!["outside-recent-window"],
        )
        .expect("corrupt the oldest out-of-window row");
        drop(conn);

        let status = collect_status(project.path()).expect("bounded recent status");
        assert_eq!(status.recent_calls.len(), 20);
        assert_eq!(
            status
                .recent_calls
                .iter()
                .map(|row| row.audit_id)
                .collect::<Vec<_>>(),
            (2_i64..=21).rev().collect::<Vec<_>>()
        );
        assert_eq!(
            status.recent_calls[0].verdict.as_deref(),
            Some("native_handler_21")
        );
        assert!(
            status
                .recent_calls
                .iter()
                .all(|row| row.duration_ms.is_some() && row.evidence.is_none())
        );
        assert_eq!(
            status.last_guard_verdict.as_deref(),
            Some("native_handler_21")
        );
    }

    #[test]
    fn status_preserves_native_and_component_rows_with_optional_evidence() {
        let _xdg = ScopedXdgStateHome::new();
        let project = TempDir::new().expect("project");
        let db_path = audit_db_path_for_project(project.path()).expect("audit path");
        std::fs::create_dir_all(db_path.parent().expect("audit parent")).expect("audit parent");
        let audit = CepAuditStore::open(&db_path).expect("open audit");
        let ctx = CepAuditContext {
            turn_id: "turn-status".to_string(),
            session_id: "session-status".to_string(),
            model_id: "status-model".to_string(),
            runtime_version: "test".to_string(),
        };

        let native = audit
            .start_call(
                &ctx,
                1,
                "fetch.web",
                &CallAuditMetadata::native_policy_enforced(Some("started")),
            )
            .expect("start native");
        audit
            .finalize_call(
                native,
                AuditOutcome::Success {
                    summary: "native result".to_string(),
                },
                Some("native_handler"),
                Some(r#"{"required_capability":"fetch_url"}"#),
            )
            .expect("finalize native");

        let admission = AdmissionEvidence {
            version: ADMISSION_EVIDENCE_VERSION,
            tool: "session.todo_write".to_string(),
            approved_capabilities: vec!["session.write".to_string()],
            denied_capabilities: vec![],
            boundary_excluded: vec![],
            max_boundary: Trust::In,
        };
        let effects = ObservedEffectsRecord {
            version: OBSERVED_EFFECTS_RECORD_VERSION,
            effects: vec![],
        };
        let denied = DeniedOperationsRecord {
            version: DENIED_OPERATIONS_RECORD_VERSION,
            denied: vec![],
        };
        let evidence_id = audit
            .record_call_evidence(&CallEvidence {
                turn_id: &ctx.turn_id,
                session_id: &ctx.session_id,
                call_id: 2,
                tool: "session.todo_write",
                outcome: EvidenceOutcome::Completed,
                admission: &admission,
                effects: Some(&effects),
                denied: Some(&denied),
            })
            .expect("record Component evidence");
        let component = audit
            .start_call(
                &ctx,
                2,
                "session.todo_write",
                &CallAuditMetadata::for_component_dispatch(),
            )
            .expect("start Component");
        let metadata = serde_json::json!({"evidence_id": evidence_id.0}).to_string();
        audit
            .finalize_call(
                component,
                AuditOutcome::Success {
                    summary: "Component result".to_string(),
                },
                Some("component_handler"),
                Some(&metadata),
            )
            .expect("finalize Component");

        let status = collect_status(project.path()).expect("status");
        assert_eq!(status.recent_calls.len(), 2);
        assert_eq!(status.recent_calls[0].method, "session.todo_write");
        assert!(status.recent_calls[0].evidence.is_some());
        assert_eq!(status.recent_calls[1].method, "fetch.web");
        assert!(status.recent_calls[1].evidence.is_none());
        assert_eq!(
            status.last_guard_verdict.as_deref(),
            Some("component_handler")
        );

        let value = serde_json::to_value(&status).expect("status serde");
        assert_eq!(value["recent_calls"][0]["evidence"]["outcome"], "completed");
        assert!(value["recent_calls"][1]["evidence"].is_null());
        assert!(
            value["recent_calls"][0].get("metadata_json").is_none(),
            "status must not serialize raw metadata"
        );
        let round_trip: LancetStatusOutput =
            serde_json::from_value(value).expect("status DTO round trip");
        assert_eq!(round_trip, status);
    }

    #[test]
    fn status_preserves_a_component_row_without_an_evidence_pointer() {
        let _xdg = ScopedXdgStateHome::new();
        let project = TempDir::new().expect("project");
        let db_path = audit_db_path_for_project(project.path()).expect("audit path");
        std::fs::create_dir_all(db_path.parent().expect("audit parent")).expect("audit parent");
        let audit = CepAuditStore::open(&db_path).expect("open audit");
        let ctx = CepAuditContext {
            turn_id: "turn-status-bad-evidence".to_string(),
            session_id: "session-status-bad-evidence".to_string(),
            model_id: "status-model".to_string(),
            runtime_version: "test".to_string(),
        };
        let component = audit
            .start_call(
                &ctx,
                1,
                "session.todo_write",
                &CallAuditMetadata::for_component_dispatch(),
            )
            .expect("start Component");
        audit
            .finalize_call(
                component,
                AuditOutcome::Success {
                    summary: "Component result".to_string(),
                },
                Some("component_handler"),
                None,
            )
            .expect("finalize Component without pointer");

        let status = collect_status(project.path()).expect("status keeps pointerless row");
        assert_eq!(status.recent_calls.len(), 1);
        assert_eq!(status.recent_calls[0].method, "session.todo_write");
        assert_eq!(
            status.recent_calls[0].verdict.as_deref(),
            Some("component_handler")
        );
        assert!(status.recent_calls[0].duration_ms.is_some());
        assert_eq!(status.recent_calls[0].evidence, None);
        assert_eq!(
            status.last_guard_verdict.as_deref(),
            Some("component_handler")
        );
    }
}

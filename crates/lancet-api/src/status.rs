// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Public LANCET status API for frontends.

use std::path::Path;

use lancet_audit::{AuditQueryFilters, CepAuditStore};

use crate::config::audit_db_path_for_project;
use crate::error::{LancetError, LancetResult};

/// Audit-derived LANCET status.
#[derive(Clone, Debug)]
pub struct LancetStatusOutput {
    pub recent_calls: Vec<LancetRecentCallEntry>,
    pub last_guard_verdict: Option<String>,
}

/// One recent CEP call for frontend status rendering.
#[derive(Clone, Debug)]
pub struct LancetRecentCallEntry {
    pub audit_id: i64,
    pub method: String,
    pub impact: String,
    pub verdict: Option<String>,
    pub duration_ms: Option<i64>,
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

    let all_rows = store
        .query_rows(&AuditQueryFilters::default())
        .map_err(|err| LancetError::Internal(format!("query all rows: {err}")))?;

    let last_guard_verdict = all_rows.last().map(|row| {
        row.semantic_status
            .clone()
            .unwrap_or_else(|| row.guard_stage.clone())
    });

    let recent_start = all_rows.len().saturating_sub(20);
    let recent_calls = all_rows[recent_start..]
        .iter()
        .rev()
        .map(|row| LancetRecentCallEntry {
            audit_id: row.audit_id,
            method: row.method.clone(),
            impact: row.impact_kind.clone(),
            verdict: row.semantic_status.clone(),
            duration_ms: compute_duration_ms(row.started_at.as_str(), row.ended_at.as_deref()),
        })
        .collect();

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

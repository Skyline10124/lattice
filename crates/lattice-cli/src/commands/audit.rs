// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `lattice audit` — query and tail the CEP audit store.

use std::path::Path;

use anyhow::{Context, Result};
use clap::Args;
use lancet_api::{query_audit, tail_audit, AuditRequest};

#[derive(Args, Debug, Clone)]
pub struct AuditCmd {
    /// Session id scope (must match rows in the audit store).
    #[arg(long)]
    pub session: Option<String>,
    /// Filter by turn id.
    #[arg(long)]
    pub turn: Option<String>,
    /// Filter by CEP method name.
    #[arg(long)]
    pub method: Option<String>,
    /// Filter by impact kind label.
    #[arg(long)]
    pub impact: Option<String>,
    /// Filter by capability (metadata `required_capability`).
    #[arg(long)]
    pub capability: Option<String>,
    /// Filter by verdict (`success`, `denied`, `error`, `cancelled`).
    #[arg(long)]
    pub verdict: Option<String>,
    /// Stream audit rows via `observe.audit` (auto-continues 1000-row batches until drained).
    #[arg(long)]
    pub tail: bool,
    /// Tail cursor (`audit_id`); only the first batch uses this (continuation is automatic).
    #[arg(long)]
    pub since: Option<i64>,
    /// Emit JSON instead of a human-readable table.
    #[arg(long)]
    pub json: bool,
}

pub fn run(cmd: AuditCmd, global_json: bool) -> Result<()> {
    let project_root = std::env::current_dir().context("current working directory")?;
    run_at(&project_root, cmd, global_json)
}

/// Run audit against an explicit project root (tests — avoids `set_current_dir` races).
pub fn run_at(project_root: &Path, cmd: AuditCmd, global_json: bool) -> Result<()> {
    let json = cmd.json || global_json;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("tokio runtime")?;
    rt.block_on(run_async(cmd, json, project_root))
}

async fn run_async(cmd: AuditCmd, json: bool, project_root: &Path) -> Result<()> {
    let session_id = cmd.session.clone().unwrap_or_else(|| "default".to_string());
    let mut request = AuditRequest::new(project_root);
    request.session_id = session_id;
    request.turn_id = cmd.turn.clone();
    request.method = cmd.method.clone();
    request.impact = cmd.impact.clone();
    request.capability = cmd.capability.clone();
    request.verdict = cmd.verdict.clone();
    request.since_audit_id = cmd.since;
    if cmd.tail {
        let output = tail_audit(request)
            .await
            .map_err(|err| anyhow::anyhow!("{err}"))?;
        render_tail_output(&output, json)
    } else {
        let output = query_audit(request)
            .await
            .map_err(|err| anyhow::anyhow!("{err}"))?;
        render_query_output(&output, json)
    }
}

#[cfg(test)]
fn audit_db_path(project_root: &Path) -> std::path::PathBuf {
    project_root.join(".lattice").join("cep_audit.db")
}

fn render_query_output(output: &lancet_api::AuditQueryOutput, json: bool) -> Result<()> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "row_count": output.row_count,
                "fragment": &output.fragment,
            }))?
        );
    } else {
        println!(
            "session={} rows={} activities={} entities={} agents={}",
            output.session_id,
            output.row_count,
            output.fragment.activities.len(),
            output.fragment.entities.len(),
            output.fragment.agents.len()
        );
        for activity in &output.fragment.activities {
            println!(
                "  {} {} {:?}",
                activity.id, activity.method, activity.impact_kind
            );
        }
    }
    Ok(())
}

fn render_tail_output(output: &lancet_api::AuditTailOutput, json: bool) -> Result<()> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "rows": &output.rows,
                "last_audit_id": output.last_audit_id,
                "done": output.done,
                "batches": output.batches,
            }))?
        );
    } else {
        println!(
            "session={} tail_rows={} last_audit_id={} done={} batches={}",
            output.session_id,
            output.rows.len(),
            output.last_audit_id,
            output.done,
            output.batches,
        );
        for p in &output.rows {
            println!(
                "  {} {} {} {}",
                p["audit_id"],
                p["method"].as_str().unwrap_or("?"),
                p["impact_kind"].as_str().unwrap_or("?"),
                p["summary"].as_str().unwrap_or("")
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lancet_catalog::methods::workspace::ObserveWorkspace;
    use lancet_catalog::CepMethod;
    use lancet_native::audit::{CallAuditMetadata, CepAuditContext, CepAuditStore};
    use tempfile::TempDir;

    fn seed_audit_db(dir: &Path, session_id: &str) {
        let db_path = audit_db_path(dir);
        std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
        let audit = CepAuditStore::open(&db_path).expect("open audit db");
        let ctx = CepAuditContext {
            turn_id: "turn-cli".into(),
            session_id: session_id.into(),
            model_id: "cli-model".into(),
            runtime_version: "test".into(),
        };
        let start = audit
            .start_call(
                &ctx,
                1,
                ObserveWorkspace::NAME,
                &CallAuditMetadata::native_policy_enforced(Some("started")),
            )
            .unwrap();
        audit
            .finalize_call(
                start,
                lancet_native::audit::AuditOutcome::Success {
                    summary: "read ok".into(),
                },
                Some("native_handler"),
                Some(r#"{"required_capability":"read_path","content_hash":"sha256:abc"}"#),
            )
            .unwrap();
    }

    #[test]
    fn audit_query_prints_fragment_for_seeded_db() {
        let dir = TempDir::new().unwrap();
        let session = "sess-cli-query";
        seed_audit_db(dir.path(), session);

        let cmd = AuditCmd {
            session: Some(session.to_string()),
            turn: None,
            method: None,
            impact: None,
            capability: None,
            verdict: None,
            tail: false,
            since: None,
            json: true,
        };
        run_at(dir.path(), cmd, false).expect("audit query should succeed");
    }

    #[tokio::test]
    async fn audit_tail_streams_rows_for_seeded_db() {
        let dir = TempDir::new().unwrap();
        let session = "sess-cli-tail";
        seed_audit_db(dir.path(), session);

        let cmd = AuditCmd {
            session: Some(session.to_string()),
            turn: None,
            method: None,
            impact: None,
            capability: None,
            verdict: None,
            tail: true,
            since: Some(0),
            json: true,
        };

        run_async(cmd, true, dir.path())
            .await
            .expect("audit tail should succeed");
    }
}

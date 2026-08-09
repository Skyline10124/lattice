// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `lattice audit` — query, tail, and replay the CEP audit store.
//!
//! Audit readers resolve the external project-keyed path via
//! `audit_db_path_for_project` (XDG state: `lattice/projects/<key>/cep_audit.db`).
//! No path here composes a CEP server: every reader calls the `lancet-api`
//! facade, which delegates to `lancet-audit` — a crate that depends on none of
//! `lancet-native`, `lancet-dispatch`, `lancet-cep`, or `lancet-runtime`, so the
//! read path cannot reach server, transport, or registry types at all.

use std::path::Path;

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use lancet_api::{AuditReplayRequest, AuditRequest, query_audit, replay_audit, tail_audit};

/// Query/tail flags (default action when no `replay` subcommand is given).
#[derive(Args, Debug, Clone)]
pub struct AuditQueryArgs {
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
    /// Stream audit rows (auto-continues 1000-row batches until drained).
    #[arg(long)]
    pub tail: bool,
    /// Tail cursor (`audit_id`); only the first batch uses this (continuation is automatic).
    #[arg(long)]
    pub since: Option<i64>,
    /// Emit JSON instead of a human-readable table.
    #[arg(long)]
    pub json: bool,
}

/// Replay a single turn's finalized audit calls.
#[derive(Args, Debug, Clone)]
pub struct AuditReplayArgs {
    /// Session id scope (must match rows in the audit store).
    #[arg(long)]
    pub session: Option<String>,
    /// Turn id to read (required).
    #[arg(long)]
    pub turn: String,
    /// Emit JSON instead of a human-readable summary.
    #[arg(long)]
    pub json: bool,
}

#[derive(Subcommand, Debug, Clone)]
pub enum AuditAction {
    /// Query or tail audit rows (default when no subcommand is given).
    Query(AuditQueryArgs),
    /// Read a turn's finalized audit calls.
    Replay(AuditReplayArgs),
}

#[derive(Args, Debug, Clone)]
pub struct AuditCmd {
    #[command(subcommand)]
    pub action: Option<AuditAction>,
    /// Backward-compat: query/tail flags usable directly on `lattice audit`
    /// (forwarded to `Query` when no subcommand is given).
    #[command(flatten)]
    pub query: AuditQueryArgs,
}

pub async fn run(cmd: AuditCmd, global_json: bool) -> Result<()> {
    // The binary entry runs inside `#[tokio::main]` (multi-thread runtime), so
    // it must NOT build its own runtime and `block_on` - that nests a runtime
    // and panics with "Cannot start a runtime from within a runtime" (surfaced
    // by the `planned_audit_cli` black-box scenario on a missing/empty store).
    // Await `run_async` on the existing runtime instead.
    let project_root = std::env::current_dir().context("current working directory")?;
    run_async(cmd, global_json, &project_root).await
}

async fn run_async(cmd: AuditCmd, global_json: bool, project_root: &Path) -> Result<()> {
    match cmd.action {
        Some(AuditAction::Replay(args)) => run_replay(args, global_json, project_root).await,
        Some(AuditAction::Query(args)) => run_query(args, global_json, project_root).await,
        None => run_query(cmd.query, global_json, project_root).await,
    }
}

async fn run_query(args: AuditQueryArgs, global_json: bool, project_root: &Path) -> Result<()> {
    let json = args.json || global_json;
    let session_id = args
        .session
        .clone()
        .unwrap_or_else(|| "default".to_string());
    let mut request = AuditRequest::new(project_root);
    request.session_id = session_id;
    request.turn_id = args.turn.clone();
    request.method = args.method.clone();
    request.impact = args.impact.clone();
    request.capability = args.capability.clone();
    request.verdict = args.verdict.clone();
    request.since_audit_id = args.since;
    if args.tail {
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

async fn run_replay(args: AuditReplayArgs, global_json: bool, project_root: &Path) -> Result<()> {
    let json = args.json || global_json;
    let session_id = args
        .session
        .clone()
        .unwrap_or_else(|| "default".to_string());
    let request = AuditReplayRequest::new(project_root, session_id, args.turn);
    let replay = replay_audit(request)
        .await
        .map_err(|err| anyhow::anyhow!("{err}"))?;
    render_replay_output(&replay, json)
}

fn render_query_output(output: &lancet_api::AuditQueryOutput, json: bool) -> Result<()> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "session_id": &output.session_id,
                "row_count": output.row_count,
                "rows": &output.rows,
            }))?
        );
    } else {
        println!("session={} rows={}", output.session_id, output.row_count);
        for row in &output.rows {
            println!(
                "  audit_id={} method={} impact={}",
                row.audit_id, row.method, row.impact_kind
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
                "session_id": &output.session_id,
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
        for row in &output.rows {
            println!(
                "  {} {} {} {}",
                row.audit_id, row.method, row.impact_kind, row.summary
            );
        }
    }
    Ok(())
}

fn render_replay_output(replay: &lancet_api::AuditReplayResult, json: bool) -> Result<()> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "trace": &replay.trace,
            }))?
        );
    } else {
        println!(
            "turn={} calls={}",
            replay.trace.turn_id,
            replay.trace.calls.len()
        );
        for call in &replay.trace.calls {
            println!(
                "  audit_id={} call_id={} method={} impact={} {}",
                call.audit_id,
                call.call_id,
                call.method,
                call.impact_kind,
                call.outcome_summary.as_deref().unwrap_or(""),
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lancet_api::audit_db_path_for_project;
    use lancet_audit::ScopedXdgStateHome;
    use tempfile::TempDir;

    fn query_cmd(session: &str, json: bool) -> AuditCmd {
        AuditCmd {
            action: None,
            query: AuditQueryArgs {
                session: Some(session.to_string()),
                turn: None,
                method: None,
                impact: None,
                capability: None,
                verdict: None,
                tail: false,
                since: None,
                json,
            },
        }
    }

    /// Legacy audit SQLite (no `model_id` / `runtime_version` columns) remains readable.
    ///
    /// Kept crate-internal on purpose (triage ledger: `keep`, 局部不变量): the
    /// Direct-Local harness may not hand-seed the internal audit DB, and a real
    /// turn only ever writes the current schema, so no black-box scenario can
    /// reach this state. The seeded-store, missing-store, and replay surfaces of
    /// `lattice audit` are covered by `fast_audit_cli` and `direct_operator_*`
    /// in `lattice-system-harness`.
    #[tokio::test]
    async fn audit_query_reads_legacy_schema_store() {
        let _xdg = ScopedXdgStateHome::new();
        let dir = TempDir::new().unwrap();
        let db_path = audit_db_path_for_project(dir.path()).expect("audit path");
        std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();

        {
            let conn = rusqlite::Connection::open(&db_path).expect("legacy db");
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
            conn.execute(
                "INSERT INTO cep_audit_entries (
                    call_id, method, started_at, ended_at, result_or_error, impact_kind,
                    turn_id, session_id, semantic_stage, semantic_status, guard_stage,
                    policy_enforced, metadata_json
                ) VALUES (1, ?1, '1', '2', 'success:legacy', 'workspace_observation',
                    'turn-legacy', 'sess-legacy-cli', 'native', 'native_handler', 'native', 1, NULL)",
                // A literal, not a catalog type: this fixture writes a row as an
                // OLDER build wrote it, and #30 migrated `observe.workspace` to a
                // Component Tool. Real audit databases still hold rows with this
                // method name and the legacy `workspace_observation` impact, so
                // the operator read path must keep parsing them — pinning the
                // string to a type that no longer exists would have quietly
                // narrowed what this test covers.
                ["observe.workspace"],
            )
            .unwrap();
        }

        run_async(query_cmd("sess-legacy-cli", true), true, dir.path())
            .await
            .expect("legacy schema query via CLI path");
    }
}

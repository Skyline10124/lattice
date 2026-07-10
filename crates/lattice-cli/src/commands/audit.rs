// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `lattice audit` — query, tail, and replay the CEP audit store.
//!
//! Audit readers resolve the external project-keyed path via
//! `audit_db_path_for_project` (XDG state: `lattice/projects/<key>/cep_audit.db`).
//! No path here composes a CEP server; all readers call `lancet-api` store
//! readers directly (see `audit_command_source_does_not_reference_cep_server`).

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

/// Replay a single turn's provenance from audit rows.
#[derive(Args, Debug, Clone)]
pub struct AuditReplayArgs {
    /// Session id scope (must match rows in the audit store).
    #[arg(long)]
    pub session: Option<String>,
    /// Turn id to reconstruct (required).
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
    /// Reconstruct a turn's provenance trace and in-toto statement from audit rows.
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
    // Await `run_async` on the existing runtime instead. `run_at` (below) keeps
    // its own runtime for the crate-internal unit tests, which call it outside
    // any runtime context.
    let project_root = std::env::current_dir().context("current working directory")?;
    run_async(cmd, global_json, &project_root).await
}

/// Run audit against an explicit project root (tests — avoids `set_current_dir` races).
#[cfg(test)]
pub fn run_at(project_root: &Path, cmd: AuditCmd, global_json: bool) -> Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("tokio runtime")?;
    rt.block_on(run_async(cmd, global_json, project_root))
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

fn render_replay_output(replay: &lancet_api::AuditReplayResult, json: bool) -> Result<()> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "trace": &replay.trace,
                "statement": &replay.statement,
                "stored_statement_match": &replay.stored_statement_match,
                "source_audit_range_or_hash": &replay.source_audit_range_or_hash,
            }))?
        );
    } else {
        println!(
            "turn={} calls={} activities={} was_informed_by={}",
            replay.trace.turn_id,
            replay.trace.calls.len(),
            replay.trace.fragment.activities.len(),
            replay.trace.fragment.was_informed_by.len(),
        );
        for call in &replay.trace.calls {
            println!(
                "  audit_id={} call_id={} {} {:?} {}",
                call.audit_id,
                call.call_id,
                call.method,
                call.impact_kind,
                call.outcome_summary.as_deref().unwrap_or(""),
            );
        }
        match replay.stored_statement_match {
            Some(true) => println!("attestation: stored statement matches derived"),
            Some(false) => println!("attestation: stored statement DOES NOT match derived"),
            None => println!("attestation: no stored statement for this turn"),
        }
        if let Some(hash) = &replay.source_audit_range_or_hash {
            println!("source_audit_range_or_hash: {hash}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_env::ScopedXdgStateHome;
    use lancet_api::audit_db_path_for_project;
    use lancet_api::store_turn_attestation;
    use lancet_catalog::CepMethod;
    use lancet_catalog::methods::workspace::ObserveWorkspace;
    use lancet_native::audit::{CallAuditMetadata, CepAuditContext, CepAuditStore};
    use tempfile::TempDir;

    fn seed_audit_db(dir: &Path, session_id: &str) {
        let db_path = audit_db_path_for_project(dir).expect("resolve external audit path");
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

    fn tail_cmd(session: &str, json: bool) -> AuditCmd {
        AuditCmd {
            action: Some(AuditAction::Query(AuditQueryArgs {
                session: Some(session.to_string()),
                turn: None,
                method: None,
                impact: None,
                capability: None,
                verdict: None,
                tail: true,
                since: Some(0),
                json,
            })),
            query: AuditQueryArgs {
                session: None,
                turn: None,
                method: None,
                impact: None,
                capability: None,
                verdict: None,
                tail: false,
                since: None,
                json: false,
            },
        }
    }

    fn replay_cmd(session: &str, turn: &str, json: bool) -> AuditCmd {
        AuditCmd {
            action: Some(AuditAction::Replay(AuditReplayArgs {
                session: Some(session.to_string()),
                turn: turn.to_string(),
                json,
            })),
            query: AuditQueryArgs {
                session: None,
                turn: None,
                method: None,
                impact: None,
                capability: None,
                verdict: None,
                tail: false,
                since: None,
                json: false,
            },
        }
    }

    #[test]
    fn audit_query_prints_fragment_for_seeded_db() {
        let _xdg = ScopedXdgStateHome::new();
        let dir = TempDir::new().unwrap();
        let session = "sess-cli-query";
        seed_audit_db(dir.path(), session);

        run_at(dir.path(), query_cmd(session, true), false).expect("audit query should succeed");
    }

    #[tokio::test]
    async fn audit_tail_streams_rows_for_seeded_db() {
        let _xdg = ScopedXdgStateHome::new();
        let dir = TempDir::new().unwrap();
        let session = "sess-cli-tail";
        seed_audit_db(dir.path(), session);

        run_async(tail_cmd(session, true), false, dir.path())
            .await
            .expect("audit tail should succeed");
    }
    #[test]
    fn audit_query_empty_result_for_unknown_session() {
        let _xdg = ScopedXdgStateHome::new();
        let dir = TempDir::new().unwrap();
        seed_audit_db(dir.path(), "sess-seeded");
        run_at(dir.path(), query_cmd("other-session", true), false).expect("empty query ok");
    }

    #[tokio::test]
    async fn audit_replay_reconstructs_turn_trace_for_seeded_db() {
        let _xdg = ScopedXdgStateHome::new();
        let dir = TempDir::new().unwrap();
        let session = "sess-cli-replay";
        seed_audit_db(dir.path(), session);

        run_async(replay_cmd(session, "turn-cli", true), false, dir.path())
            .await
            .expect("audit replay should succeed");
    }

    #[tokio::test]
    async fn audit_replay_reports_stored_attestation_match() {
        let _xdg = ScopedXdgStateHome::new();
        let dir = TempDir::new().unwrap();
        let session = "sess-cli-replay-match";
        let db_path = audit_db_path_for_project(dir.path()).expect("audit path");
        std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
        let audit = CepAuditStore::open(&db_path).expect("open audit db");
        let turn_id = "turn-cli-match";
        let ctx = CepAuditContext {
            turn_id: turn_id.into(),
            session_id: session.into(),
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
                Some(r#"{"required_capability":"read_path"}"#),
            )
            .unwrap();
        store_turn_attestation(&audit, &ctx, turn_id).expect("store attestation");

        run_async(replay_cmd(session, turn_id, true), false, dir.path())
            .await
            .expect("replay with stored attestation should succeed");
    }

    #[test]
    fn audit_replay_errors_for_unknown_turn() {
        let _xdg = ScopedXdgStateHome::new();
        let dir = TempDir::new().unwrap();
        let session = "sess-cli-replay-empty";
        seed_audit_db(dir.path(), session);
        let err = run_at(
            dir.path(),
            replay_cmd(session, "turn-nonexistent", true),
            false,
        )
        .expect_err("unknown turn should error");
        assert!(err.to_string().contains("no audit rows for turn_id"));
    }
    /// Legacy audit SQLite (no `model_id` / `runtime_version` columns) remains readable.
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
                [ObserveWorkspace::NAME],
            )
            .unwrap();
        }

        run_async(query_cmd("sess-legacy-cli", true), true, dir.path())
            .await
            .expect("legacy schema query via CLI path");
    }
}

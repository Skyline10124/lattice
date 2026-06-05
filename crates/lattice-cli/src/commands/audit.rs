// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `lattice audit` — query and tail the CEP audit store.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use clap::Args;
use lancet_api::LancetServer;
use lancet_authority::CepTurnContext;
use lancet_catalog::methods::audit::{
    AuditObserve, AuditObserveParams, AuditObserveResult, AuditQueryParams, AuditTailResult,
};
use lancet_native::{impact_kind_from_label, CepAuditStore, CepClient, ServerRunConfig};
use lancet_protocol::envelope::{BaseExecutionEnvelope, ImpactKind};
use tokio::sync::RwLock;

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

    if cmd.tail {
        run_tail(project_root, &session_id, &cmd, json).await
    } else {
        run_query(project_root, &session_id, &cmd, json).await
    }
}

fn audit_db_path(project_root: &Path) -> PathBuf {
    project_root.join(".lattice").join("cep_audit.db")
}

async fn with_audit_client<F, Fut>(project_root: &Path, session_id: &str, f: F) -> Result<()>
where
    F: FnOnce(CepClient) -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    let audit_path = audit_db_path(project_root);
    if !audit_path.exists() {
        bail!(
            "audit database not found at {}; run an agent turn in this project first",
            audit_path.display()
        );
    }

    let audit = Arc::new(
        CepAuditStore::open(&audit_path).map_err(|e| anyhow::anyhow!("open audit store: {e}"))?,
    );

    let root = project_root
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("project root path is not UTF-8"))?;

    let mut config = ServerRunConfig::in_memory_for_tests();
    config.audit = audit;
    config.base_envelope =
        BaseExecutionEnvelope::for_workspace_root(root, "lancet audit").with_read_audit();
    config.cep_context = Arc::new(CepTurnContext::defaults_for_project_root(root));
    config.audit_context = Arc::new(RwLock::new(lancet_native::CepAuditContext {
        session_id: session_id.to_string(),
        ..Default::default()
    }));

    let (client, lifecycle) =
        LancetServer::spawn(config, 64).map_err(|e| anyhow::anyhow!("spawn CEP server: {e}"))?;

    f(client).await?;

    drop(lifecycle);
    Ok(())
}

async fn run_query(
    project_root: &Path,
    session_id: &str,
    cmd: &AuditCmd,
    json: bool,
) -> Result<()> {
    let impact_kind = parse_impact_kind(cmd.impact.as_deref())?;

    with_audit_client(project_root, session_id, |client| async move {
        let result = client
            .call::<AuditObserve>(AuditObserveParams {
                stream: false,
                query: AuditQueryParams {
                    method: cmd.method.clone(),
                    impact_kind,
                    capability: cmd.capability.clone(),
                    verdict: cmd.verdict.clone(),
                    since_audit_id: cmd.since,
                    until_audit_id: None,
                    envelope_id: None,
                    turn_id: cmd.turn.clone(),
                    session_id: None,
                },
            })
            .await
            .map_err(|e| anyhow::anyhow!("observe.audit: {}", e.message))?;
        let AuditObserveResult::Query(result) = result else {
            bail!("observe.audit returned a tail result for stream=false");
        };

        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "row_count": result.row_count,
                    "fragment": result.fragment,
                }))?
            );
        } else {
            println!(
                "session={session_id} rows={} activities={} entities={} agents={}",
                result.row_count,
                result.fragment.activities.len(),
                result.fragment.entities.len(),
                result.fragment.agents.len()
            );
            for activity in &result.fragment.activities {
                println!(
                    "  {} {} {:?}",
                    activity.id, activity.method, activity.impact_kind
                );
            }
        }
        Ok(())
    })
    .await
}

/// Safety cap: 1000 rows per handler batch × this many CLI continuations.
const AUDIT_TAIL_MAX_BATCHES: usize = 1000;

async fn run_tail(project_root: &Path, session_id: &str, cmd: &AuditCmd, json: bool) -> Result<()> {
    with_audit_client(project_root, session_id, |client| async move {
        let mut since = cmd.since.unwrap_or(0);
        let mut all_tail_lines = Vec::new();
        let mut tail_result = AuditTailResult {
            last_audit_id: since,
            done: true,
        };
        let mut batches = 0usize;

        loop {
            batches += 1;
            if batches > AUDIT_TAIL_MAX_BATCHES {
                bail!(
                    "audit tail exceeded {AUDIT_TAIL_MAX_BATCHES} batches ({} rows emitted); \
                     narrow with filters or resume with --since {}",
                    all_tail_lines.len(),
                    tail_result.last_audit_id
                );
            }

            let events = client.events();
            let call = client.call::<AuditObserve>(AuditObserveParams {
                stream: true,
                query: AuditQueryParams {
                    since_audit_id: Some(since),
                    ..Default::default()
                },
            });

            let mut batch_lines = Vec::new();
            let collect = async {
                loop {
                    let Some(event) = events.lock().await.recv().await else {
                        break;
                    };
                    let finished = event.method == "cep.call_finished";
                    if event.method == "audit.tail_row" {
                        batch_lines.push(event);
                    }
                    if finished {
                        break;
                    }
                }
            };

            let (result, _) = tokio::join!(call, collect);
            let result = result.map_err(|e| anyhow::anyhow!("observe.audit: {}", e.message))?;
            let AuditObserveResult::Tail(result) = result else {
                bail!("observe.audit returned a query result for stream=true");
            };
            tail_result = result;
            all_tail_lines.extend(batch_lines);

            if tail_result.done {
                break;
            }
            since = tail_result.last_audit_id;
        }

        if json {
            let rows: Vec<_> = all_tail_lines.iter().map(|ev| ev.params.clone()).collect();
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "rows": rows,
                    "last_audit_id": tail_result.last_audit_id,
                    "done": tail_result.done,
                    "batches": batches,
                }))?
            );
        } else {
            println!(
                "session={session_id} tail_rows={} last_audit_id={} done={} batches={batches}",
                all_tail_lines.len(),
                tail_result.last_audit_id,
                tail_result.done,
            );
            for ev in &all_tail_lines {
                let p = &ev.params;
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
    })
    .await
}

fn parse_impact_kind(label: Option<&str>) -> Result<Option<ImpactKind>> {
    let Some(label) = label else {
        return Ok(None);
    };
    impact_kind_from_label(label)
        .ok_or_else(|| anyhow::anyhow!("unknown impact kind: {label}"))
        .map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lancet_catalog::methods::workspace::ObserveWorkspace;
    use lancet_catalog::CepMethod;
    use lancet_native::audit::{CallAuditMetadata, CepAuditContext};
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

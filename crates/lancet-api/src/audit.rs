// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Public audit query/tail API for frontends.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lancet_audit::{impact_kind_from_label, CepAuditContext, CepAuditStore};
use lancet_authority::CepTurnContext;
use lancet_catalog::methods::audit::{
    AuditObserve, AuditObserveParams, AuditObserveResult, AuditQueryParams, AuditTailResult,
};
use lancet_dispatch::CepClient;
use lancet_native::ServerRunConfig;
use lancet_protocol::envelope::{BaseExecutionEnvelope, ImpactKind};
use lancet_protocol::provenance::ProvGraphFragment;
use serde_json::Value;
use tokio::sync::RwLock;

use crate::error::{LancetError, LancetResult};
use crate::runtime::LancetServer;

/// Audit rows are streamed by the handler in batches; this caps frontend
/// continuation loops so a broad tail cannot run forever.
pub const AUDIT_TAIL_MAX_BATCHES: usize = 1000;

/// Request shape for audit query/tail frontends.
#[derive(Clone, Debug)]
pub struct AuditRequest {
    pub project_root: PathBuf,
    pub session_id: String,
    pub turn_id: Option<String>,
    pub method: Option<String>,
    pub impact: Option<String>,
    pub capability: Option<String>,
    pub verdict: Option<String>,
    pub since_audit_id: Option<i64>,
}

impl AuditRequest {
    pub fn new(project_root: impl Into<PathBuf>) -> Self {
        Self {
            project_root: project_root.into(),
            session_id: "default".to_string(),
            turn_id: None,
            method: None,
            impact: None,
            capability: None,
            verdict: None,
            since_audit_id: None,
        }
    }
}

/// Result of a bounded audit query.
#[derive(Clone, Debug)]
pub struct AuditQueryOutput {
    pub session_id: String,
    pub row_count: usize,
    pub fragment: ProvGraphFragment,
}

/// Result of a bounded audit tail.
#[derive(Clone, Debug)]
pub struct AuditTailOutput {
    pub session_id: String,
    pub rows: Vec<Value>,
    pub last_audit_id: i64,
    pub done: bool,
    pub batches: usize,
}

/// Query the CEP audit store through the production `observe.audit` method.
pub async fn query_audit(request: AuditRequest) -> LancetResult<AuditQueryOutput> {
    let impact_kind = parse_impact_kind(request.impact.as_deref())?;
    let session_id = request.session_id.clone();
    let output_session_id = session_id.clone();
    with_audit_client(&request.project_root, &session_id, |client| async move {
        let result = client
            .call::<AuditObserve>(AuditObserveParams {
                stream: false,
                query: AuditQueryParams {
                    method: request.method,
                    impact_kind,
                    capability: request.capability,
                    verdict: request.verdict,
                    since_audit_id: request.since_audit_id,
                    until_audit_id: None,
                    envelope_id: None,
                    turn_id: request.turn_id,
                    session_id: None,
                },
            })
            .await
            .map_err(|err| LancetError::Internal(format!("observe.audit: {}", err.message)))?;
        let AuditObserveResult::Query(result) = result else {
            return Err(LancetError::Internal(
                "observe.audit returned a tail result for stream=false".to_string(),
            ));
        };
        Ok(AuditQueryOutput {
            session_id: output_session_id,
            row_count: result.row_count,
            fragment: result.fragment,
        })
    })
    .await
}

/// Tail CEP audit rows through the production `observe.audit` method.
pub async fn tail_audit(request: AuditRequest) -> LancetResult<AuditTailOutput> {
    let session_id = request.session_id.clone();
    let output_session_id = session_id.clone();
    with_audit_client(&request.project_root, &session_id, |client| async move {
        let mut since = request.since_audit_id.unwrap_or(0);
        let mut rows = Vec::new();
        let mut tail_result = AuditTailResult {
            last_audit_id: since,
            done: true,
        };
        let mut batches = 0usize;

        loop {
            batches += 1;
            if batches > AUDIT_TAIL_MAX_BATCHES {
                return Err(LancetError::Config(format!(
                    "audit tail exceeded {AUDIT_TAIL_MAX_BATCHES} batches ({} rows emitted); narrow with filters or resume with --since {}",
                    rows.len(),
                    tail_result.last_audit_id
                )));
            }

            let events = client.events();
            let call = client.call::<AuditObserve>(AuditObserveParams {
                stream: true,
                query: AuditQueryParams {
                    since_audit_id: Some(since),
                    ..Default::default()
                },
            });

            let mut batch_rows = Vec::new();
            let collect = async {
                loop {
                    let Some(event) = events.lock().await.recv().await else {
                        break;
                    };
                    let finished = event.method == "cep.call_finished";
                    if event.method == "audit.tail_row" {
                        batch_rows.push(event.params);
                    }
                    if finished {
                        break;
                    }
                }
            };

            let (result, _) = tokio::join!(call, collect);
            let result =
                result.map_err(|err| LancetError::Internal(format!("observe.audit: {}", err.message)))?;
            let AuditObserveResult::Tail(result) = result else {
                return Err(LancetError::Internal(
                    "observe.audit returned a query result for stream=true".to_string(),
                ));
            };
            tail_result = result;
            rows.extend(batch_rows);

            if tail_result.done {
                break;
            }
            since = tail_result.last_audit_id;
        }

        Ok(AuditTailOutput {
            session_id: output_session_id,
            rows,
            last_audit_id: tail_result.last_audit_id,
            done: tail_result.done,
            batches,
        })
    })
    .await
}

fn audit_db_path(project_root: &Path) -> PathBuf {
    project_root.join(".lattice").join("cep_audit.db")
}

async fn with_audit_client<T, F, Fut>(
    project_root: &Path,
    session_id: &str,
    f: F,
) -> LancetResult<T>
where
    F: FnOnce(CepClient) -> Fut,
    Fut: std::future::Future<Output = LancetResult<T>>,
{
    let audit_path = audit_db_path(project_root);
    if !audit_path.exists() {
        return Err(LancetError::Config(format!(
            "audit database not found at {}; run an agent turn in this project first",
            audit_path.display()
        )));
    }

    let audit = Arc::new(
        CepAuditStore::open(&audit_path)
            .map_err(|err| LancetError::Internal(format!("open audit store: {err}")))?,
    );

    let root = project_root
        .to_str()
        .ok_or_else(|| LancetError::Config("project root path is not UTF-8".to_string()))?;

    let mut config = ServerRunConfig::in_memory_for_tests();
    config.audit = audit;
    config.base_envelope =
        BaseExecutionEnvelope::for_workspace_root(root, "lancet audit").with_read_audit();
    config.cep_context = Arc::new(CepTurnContext::defaults_for_project_root(root));
    config.audit_context = Arc::new(RwLock::new(CepAuditContext {
        session_id: session_id.to_string(),
        ..Default::default()
    }));

    let (client, mut lifecycle) = LancetServer::spawn(config, 64)?;
    let result = f(client).await;
    lifecycle.shutdown().await;
    result
}

fn parse_impact_kind(label: Option<&str>) -> LancetResult<Option<ImpactKind>> {
    let Some(label) = label else {
        return Ok(None);
    };
    impact_kind_from_label(label)
        .ok_or_else(|| LancetError::Config(format!("unknown impact kind: {label}")))
        .map(Some)
}

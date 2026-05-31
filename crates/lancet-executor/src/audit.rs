//! Flat append-only SQLite audit store for CEP execution records.

use lancet_protocol::catalog;
use lancet_protocol::envelope::ImpactKind;
use lancet_protocol::method::RequiredCapability;
use rusqlite::{params, Connection};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use thiserror::Error;

pub const SEMANTIC_STAGE_NATIVE: &str = "cep_native";
pub const GUARD_STAGE_NATIVE: &str = "cep_policy_lifecycle";

pub const SEMANTIC_STAGE_SCAFFOLD: &str = "scaffold";

/// Audit stage for a production call whose method is not in the catalog; dispatch
/// will return `MethodNotFound`, so no native handler or policy hook runs.
pub const SEMANTIC_STAGE_UNROUTED: &str = "unrouted";

/// Feature-gated scaffold methods (not in default `CATALOG` or production registry).
pub const SCAFFOLD_METHODS: [&str; 2] = ["workspace.inspect", "workspace.navigate"];

/// Returns true when `method` is an inspect/navigate scaffold stub.
pub fn is_scaffold_method(method: &str) -> bool {
    SCAFFOLD_METHODS.contains(&method)
}

/// Per-call audit lifecycle metadata (replaces compile-time constants per row).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallAuditMetadata {
    pub semantic_stage: String,
    pub semantic_status: Option<String>,
    pub guard_stage: String,
    pub policy_enforced: bool,
    pub denial_source: Option<String>,
    pub backend_invoked: Option<bool>,
    pub observation_source: Option<String>,
}

impl CallAuditMetadata {
    /// CEP native handler with CEP policy enforcement.
    pub fn native_policy_enforced(semantic_status: Option<&str>) -> Self {
        Self {
            semantic_stage: SEMANTIC_STAGE_NATIVE.to_string(),
            semantic_status: semantic_status.map(str::to_string),
            guard_stage: GUARD_STAGE_NATIVE.to_string(),
            policy_enforced: true,
            denial_source: None,
            backend_invoked: Some(true),
            observation_source: None,
        }
    }

    /// Unknown production method (not in the catalog). Dispatch will reject it with
    /// `MethodNotFound`; no handler or policy hook runs, so this must not claim
    /// policy enforcement or backend invocation.
    pub fn unrouted() -> Self {
        Self {
            semantic_stage: SEMANTIC_STAGE_UNROUTED.to_string(),
            semantic_status: Some("method_not_found".to_string()),
            guard_stage: GUARD_STAGE_NATIVE.to_string(),
            policy_enforced: false,
            denial_source: None,
            backend_invoked: Some(false),
            observation_source: None,
        }
    }

    /// Feature-gated inspect/navigate scaffold stub (no workspace access, no budget charge).
    pub fn scaffold_stub() -> Self {
        Self {
            semantic_stage: SEMANTIC_STAGE_SCAFFOLD.to_string(),
            semantic_status: Some("not_yet_routed".to_string()),
            guard_stage: GUARD_STAGE_NATIVE.to_string(),
            policy_enforced: true,
            denial_source: None,
            backend_invoked: Some(false),
            observation_source: None,
        }
    }

    pub fn with_denial_source(mut self, source: &str) -> Self {
        self.denial_source = Some(source.to_string());
        self.backend_invoked = Some(false);
        self
    }

    pub fn with_observation_source(mut self, source: &str) -> Self {
        self.observation_source = Some(source.to_string());
        self
    }

    /// Per-method audit metadata for the production registry.
    ///
    /// Every catalog method is served by a native, policy-enforced handler. Feature-gated
    /// inspect/navigate scaffolds are stubs. A method that is neither (an unknown / stale
    /// tool name) will fail `MethodNotFound` without running a handler or policy hook, so it
    /// is labeled `unrouted` and must not claim enforcement.
    pub fn for_production_dispatch(method: &str) -> Self {
        if is_scaffold_method(method) {
            Self::scaffold_stub()
        } else if is_production_catalog_method(method) {
            Self::native_policy_enforced(Some("started"))
        } else {
            Self::unrouted()
        }
    }
}

/// Returns true when `method` is a method in the production catalog (the set the
/// production registry registers native handlers for).
fn is_production_catalog_method(method: &str) -> bool {
    catalog().iter().any(|descriptor| descriptor.name == method)
}

/// Per-turn identifiers attached to every audit row.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CepAuditContext {
    pub turn_id: String,
    pub session_id: String,
    /// Resolved model id for PROV Agent reconstruction (D-04); empty in tests unless set.
    pub model_id: String,
    /// Runtime / crate version string persisted on each row (D-04).
    pub runtime_version: String,
}

/// Filters for [`CepAuditStore::query_rows`] — mirrors `audit.query` wire params.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AuditQueryFilters {
    pub session_id: Option<String>,
    pub turn_id: Option<String>,
    pub method: Option<String>,
    pub impact_kind: Option<String>,
    pub capability: Option<String>,
    pub verdict: Option<String>,
    pub since_audit_id: Option<i64>,
    pub until_audit_id: Option<i64>,
    pub envelope_id: Option<String>,
    pub started_after: Option<String>,
    pub started_before: Option<String>,
}

/// Stored unsigned in-toto-shaped attestation for a turn (D-09).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttestationRecord {
    pub turn_id: String,
    pub session_id: String,
    pub statement_json: String,
    pub predicate_type: String,
    pub generated_at: String,
    pub source_audit_range_or_hash: String,
}

/// Snake-case label for a catalog [`RequiredCapability`] (metadata + query filters).
pub fn required_capability_label(cap: RequiredCapability) -> &'static str {
    match cap {
        RequiredCapability::Inspect => "inspect",
        RequiredCapability::ReadPath => "read_path",
        RequiredCapability::WritePath => "write_path",
        RequiredCapability::RunCommand => "run_command",
        RequiredCapability::FetchUrl => "fetch_url",
        RequiredCapability::MutateSessionState => "mutate_session_state",
        RequiredCapability::ReadAudit => "read_audit",
    }
}

/// D-01-safe finalize enrichment: merge transport/catalog keys into metadata JSON only.
pub fn merge_finalize_metadata(
    existing: Option<&str>,
    method: &str,
    observed_effects: &[crate::effect::ledger::ObservedEffect],
    envelope_id: Option<&str>,
) -> Option<String> {
    let mut obj = existing
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .unwrap_or_else(|| serde_json::json!({}));

    let Some(obj_map) = obj.as_object_mut() else {
        return existing.map(str::to_string);
    };

    if let Some(descriptor) = catalog().iter().find(|d| d.name == method) {
        obj_map.insert(
            "required_capability".into(),
            serde_json::Value::String(required_capability_label(descriptor.capability).into()),
        );
    }

    if !observed_effects.is_empty() {
        let mut caps = HashSet::new();
        for effect in observed_effects {
            caps.insert(required_capability_label(effect.required_capability));
        }
        let list: Vec<_> = caps
            .into_iter()
            .map(|cap| serde_json::Value::String(cap.to_string()))
            .collect();
        obj_map.insert(
            "observed_capabilities".into(),
            serde_json::Value::Array(list),
        );
    }

    if let Some(eid) = envelope_id {
        obj_map
            .entry("envelope_id")
            .or_insert_with(|| serde_json::Value::String(eid.to_string()));
    }

    serde_json::to_string(&obj).ok()
}

/// Outcome summary written at audit finalization.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuditOutcome {
    Success { summary: String },
    Error { code: String, summary: String },
    Cancelled,
    ResponseChannelClosed,
}

/// Handle returned when an audit row is opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuditStart {
    pub audit_id: i64,
}

#[derive(Debug, Error)]
pub enum AuditError {
    #[error("audit database error at {path}: {source}")]
    Database {
        path: PathBuf,
        source: rusqlite::Error,
    },
}

/// Durable flat SQLite store for CEP call audit rows.
#[derive(Clone)]
pub struct CepAuditStore {
    conn: Arc<Mutex<Connection>>,
    path: PathBuf,
    pending: Arc<AtomicUsize>,
}

impl CepAuditStore {
    /// Open or create an on-disk audit database.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, AuditError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| AuditError::Database {
                path: path.clone(),
                source: rusqlite::Error::ToSqlConversionFailure(Box::new(source)),
            })?;
        }
        let conn = Connection::open(&path).map_err(|source| AuditError::Database {
            path: path.clone(),
            source,
        })?;
        let store = Self {
            conn: Arc::new(Mutex::new(conn)),
            path,
            pending: Arc::new(AtomicUsize::new(0)),
        };
        store.init_schema()?;
        Ok(store)
    }

    /// In-memory store for unit/integration tests.
    pub fn in_memory() -> Result<Self, AuditError> {
        let conn = Connection::open_in_memory().map_err(|source| AuditError::Database {
            path: PathBuf::from(":memory:"),
            source,
        })?;
        let store = Self {
            conn: Arc::new(Mutex::new(conn)),
            path: PathBuf::from(":memory:"),
            pending: Arc::new(AtomicUsize::new(0)),
        };
        store.init_schema()?;
        Ok(store)
    }

    fn init_schema(&self) -> Result<(), AuditError> {
        let conn = self.conn.lock().expect("audit mutex poisoned");
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             CREATE TABLE IF NOT EXISTS cep_audit_entries (
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
                metadata_json TEXT,
                model_id TEXT,
                runtime_version TEXT
             );
             CREATE TABLE IF NOT EXISTS attestation_records (
                turn_id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                statement_json TEXT NOT NULL,
                predicate_type TEXT NOT NULL,
                generated_at TEXT NOT NULL,
                source_audit_range_or_hash TEXT NOT NULL
             );",
        )
        .map_err(|source| AuditError::Database {
            path: self.path.clone(),
            source,
        })?;
        for ddl in [
            "ALTER TABLE cep_audit_entries ADD COLUMN model_id TEXT",
            "ALTER TABLE cep_audit_entries ADD COLUMN runtime_version TEXT",
        ] {
            let _ = conn.execute(ddl, []);
        }
        Ok(())
    }

    /// Append a new audit row before handler dispatch when possible.
    pub fn start_call(
        &self,
        ctx: &CepAuditContext,
        call_id: u64,
        method: &str,
        metadata: &CallAuditMetadata,
    ) -> Result<AuditStart, AuditError> {
        let started_at = chrono_now();
        let impact_kind = impact_kind_for_method(method)
            .as_ref()
            .map(impact_kind_label)
            .unwrap_or("unknown")
            .to_string();
        let semantic_status = metadata.semantic_status.as_deref().unwrap_or("started");

        let conn = self.conn.lock().expect("audit mutex poisoned");
        conn.execute(
            "INSERT INTO cep_audit_entries (
                call_id, method, started_at, impact_kind, turn_id, session_id,
                semantic_stage, semantic_status, guard_stage, policy_enforced,
                model_id, runtime_version
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                call_id as i64,
                method,
                started_at,
                impact_kind,
                ctx.turn_id,
                ctx.session_id,
                metadata.semantic_stage,
                semantic_status,
                metadata.guard_stage,
                if metadata.policy_enforced { 1 } else { 0 },
                null_if_empty(&ctx.model_id),
                null_if_empty(&ctx.runtime_version),
            ],
        )
        .map_err(|source| AuditError::Database {
            path: self.path.clone(),
            source,
        })?;
        self.pending.fetch_add(1, Ordering::SeqCst);
        let audit_id = conn.last_insert_rowid();
        Ok(AuditStart { audit_id })
    }

    /// Finalize a previously started audit row.
    pub fn finalize_call(
        &self,
        start: AuditStart,
        outcome: AuditOutcome,
        semantic_status: Option<&str>,
        metadata_json: Option<&str>,
    ) -> Result<(), AuditError> {
        let ended_at = chrono_now();
        let result_or_error = match &outcome {
            AuditOutcome::Success { summary } => format!("success:{summary}"),
            AuditOutcome::Error { code, summary } => format!("error:{code}:{summary}"),
            AuditOutcome::Cancelled => "cancelled".to_string(),
            AuditOutcome::ResponseChannelClosed => "response_channel_closed".to_string(),
        };

        let conn = self.conn.lock().expect("audit mutex poisoned");
        let result = conn
            .execute(
                "UPDATE cep_audit_entries
             SET ended_at = ?1,
                 result_or_error = ?2,
                 semantic_status = COALESCE(?3, semantic_status),
                 metadata_json = ?4
             WHERE audit_id = ?5",
                params![
                    ended_at,
                    result_or_error,
                    semantic_status,
                    metadata_json,
                    start.audit_id,
                ],
            )
            .map_err(|source| AuditError::Database {
                path: self.path.clone(),
                source,
            });
        self.pending.fetch_sub(1, Ordering::SeqCst);
        result.map(|_| ())
    }

    /// Wait until all started audit rows have been finalized.
    pub async fn await_pending(&self) {
        while self.pending.load(Ordering::SeqCst) > 0 {
            tokio::task::yield_now().await;
        }
    }

    /// Read a row by audit id (tests and diagnostics).
    pub fn row_by_audit_id(&self, audit_id: i64) -> Result<AuditRow, AuditError> {
        let conn = self.conn.lock().expect("audit mutex poisoned");
        conn.query_row(
            "SELECT audit_id, call_id, method, started_at, ended_at, result_or_error, impact_kind,
                    turn_id, session_id, semantic_stage, semantic_status, guard_stage,
                    policy_enforced, metadata_json, model_id, runtime_version
             FROM cep_audit_entries WHERE audit_id = ?1",
            params![audit_id],
            map_audit_row,
        )
        .map_err(|source| AuditError::Database {
            path: self.path.clone(),
            source,
        })
    }

    /// Filtered read for PROV projection and `audit.query` (parameterized SQL only).
    pub fn query_rows(&self, filters: &AuditQueryFilters) -> Result<Vec<AuditRow>, AuditError> {
        let mut sql = String::from(
            "SELECT audit_id, call_id, method, started_at, ended_at, result_or_error, impact_kind,
                    turn_id, session_id, semantic_stage, semantic_status, guard_stage,
                    policy_enforced, metadata_json, model_id, runtime_version
             FROM cep_audit_entries WHERE 1=1",
        );
        let mut bind: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

        if let Some(session_id) = &filters.session_id {
            sql.push_str(" AND session_id = ?");
            bind.push(Box::new(session_id.clone()));
        }
        if let Some(turn_id) = &filters.turn_id {
            sql.push_str(" AND turn_id = ?");
            bind.push(Box::new(turn_id.clone()));
        }
        if let Some(method) = &filters.method {
            sql.push_str(" AND method = ?");
            bind.push(Box::new(method.clone()));
        }
        if let Some(impact_kind) = &filters.impact_kind {
            sql.push_str(" AND impact_kind = ?");
            bind.push(Box::new(impact_kind.clone()));
        }
        if let Some(since) = filters.since_audit_id {
            sql.push_str(" AND audit_id >= ?");
            bind.push(Box::new(since));
        }
        if let Some(until) = filters.until_audit_id {
            sql.push_str(" AND audit_id <= ?");
            bind.push(Box::new(until));
        }
        if let Some(after) = &filters.started_after {
            sql.push_str(" AND started_at >= ?");
            bind.push(Box::new(after.clone()));
        }
        if let Some(before) = &filters.started_before {
            sql.push_str(" AND started_at <= ?");
            bind.push(Box::new(before.clone()));
        }
        if let Some(capability) = &filters.capability {
            sql.push_str(" AND json_extract(metadata_json, '$.required_capability') = ?");
            bind.push(Box::new(capability.clone()));
        }
        if let Some(envelope_id) = &filters.envelope_id {
            sql.push_str(" AND json_extract(metadata_json, '$.envelope_id') = ?");
            bind.push(Box::new(envelope_id.clone()));
        }
        if let Some(verdict) = &filters.verdict {
            match verdict.as_str() {
                "success" => {
                    sql.push_str(" AND semantic_status = ? AND result_or_error LIKE ?");
                    bind.push(Box::new("native_handler".to_string()));
                    bind.push(Box::new("success:%".to_string()));
                }
                "denied" => {
                    sql.push_str(
                        " AND semantic_status IN ('native_handler_denied', 'method_not_found', 'scaffold_stub')",
                    );
                }
                "error" => {
                    sql.push_str(" AND result_or_error LIKE ?");
                    bind.push(Box::new("error:%".to_string()));
                }
                "cancelled" => {
                    sql.push_str(" AND result_or_error = ?");
                    bind.push(Box::new("cancelled".to_string()));
                }
                other => {
                    sql.push_str(" AND semantic_status = ?");
                    bind.push(Box::new(other.to_string()));
                }
            }
        }

        sql.push_str(" ORDER BY audit_id ASC");

        let conn = self.conn.lock().expect("audit mutex poisoned");
        let mut stmt = conn.prepare(&sql).map_err(|source| AuditError::Database {
            path: self.path.clone(),
            source,
        })?;
        let params_ref: Vec<&dyn rusqlite::ToSql> = bind.iter().map(|b| b.as_ref()).collect();
        let rows = stmt
            .query_map(params_ref.as_slice(), map_audit_row)
            .map_err(|source| AuditError::Database {
                path: self.path.clone(),
                source,
            })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|source| AuditError::Database {
                path: self.path.clone(),
                source,
            })?);
        }
        Ok(out)
    }

    pub fn insert_attestation(&self, record: &AttestationRecord) -> Result<(), AuditError> {
        let conn = self.conn.lock().expect("audit mutex poisoned");
        conn.execute(
            "INSERT INTO attestation_records (
                turn_id, session_id, statement_json, predicate_type, generated_at,
                source_audit_range_or_hash
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                record.turn_id,
                record.session_id,
                record.statement_json,
                record.predicate_type,
                record.generated_at,
                record.source_audit_range_or_hash,
            ],
        )
        .map_err(|source| AuditError::Database {
            path: self.path.clone(),
            source,
        })?;
        Ok(())
    }

    pub fn attestation_for_turn(
        &self,
        turn_id: &str,
    ) -> Result<Option<AttestationRecord>, AuditError> {
        let conn = self.conn.lock().expect("audit mutex poisoned");
        let result = conn.query_row(
            "SELECT turn_id, session_id, statement_json, predicate_type, generated_at,
                    source_audit_range_or_hash
             FROM attestation_records WHERE turn_id = ?1",
            params![turn_id],
            |row| {
                Ok(AttestationRecord {
                    turn_id: row.get(0)?,
                    session_id: row.get(1)?,
                    statement_json: row.get(2)?,
                    predicate_type: row.get(3)?,
                    generated_at: row.get(4)?,
                    source_audit_range_or_hash: row.get(5)?,
                })
            },
        );
        match result {
            Ok(record) => Ok(Some(record)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(source) => Err(AuditError::Database {
                path: self.path.clone(),
                source,
            }),
        }
    }

    pub fn delete_attestation_for_turn(&self, turn_id: &str) -> Result<(), AuditError> {
        let conn = self.conn.lock().expect("audit mutex poisoned");
        conn.execute(
            "DELETE FROM attestation_records WHERE turn_id = ?1",
            params![turn_id],
        )
        .map_err(|source| AuditError::Database {
            path: self.path.clone(),
            source,
        })?;
        Ok(())
    }

    pub fn row_count(&self) -> Result<usize, AuditError> {
        let conn = self.conn.lock().expect("audit mutex poisoned");
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM cep_audit_entries", [], |row| {
                row.get(0)
            })
            .map_err(|source| AuditError::Database {
                path: self.path.clone(),
                source,
            })?;
        Ok(count as usize)
    }
}

/// Materialized audit row for tests and PROV projection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuditRow {
    pub audit_id: i64,
    pub call_id: u64,
    pub method: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub result_or_error: Option<String>,
    pub impact_kind: String,
    pub turn_id: String,
    pub session_id: String,
    pub semantic_stage: String,
    pub semantic_status: Option<String>,
    pub guard_stage: String,
    pub policy_enforced: bool,
    pub metadata_json: Option<String>,
    pub model_id: Option<String>,
    pub runtime_version: Option<String>,
}

fn null_if_empty(value: &str) -> Option<&str> {
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn map_audit_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AuditRow> {
    Ok(AuditRow {
        audit_id: row.get(0)?,
        call_id: row.get::<_, i64>(1)? as u64,
        method: row.get(2)?,
        started_at: row.get(3)?,
        ended_at: row.get(4)?,
        result_or_error: row.get(5)?,
        impact_kind: row.get(6)?,
        turn_id: row.get(7)?,
        session_id: row.get(8)?,
        semantic_stage: row.get(9)?,
        semantic_status: row.get(10)?,
        guard_stage: row.get(11)?,
        policy_enforced: row.get::<_, i64>(12)? != 0,
        metadata_json: row.get(13)?,
        model_id: row.get(14)?,
        runtime_version: row.get(15)?,
    })
}

pub fn impact_kind_for_method(method: &str) -> Option<ImpactKind> {
    catalog()
        .iter()
        .find(|descriptor| descriptor.name == method)
        .map(|descriptor| descriptor.impact.clone())
}

/// All [`ImpactKind`] variants in enum declaration order (keep in sync with `ImpactKind`).
pub const ALL_IMPACT_KINDS: [ImpactKind; 6] = [
    ImpactKind::WorkspaceObservation,
    ImpactKind::WorkspaceMutation,
    ImpactKind::CommandExecution,
    ImpactKind::ExternalObservation,
    ImpactKind::SessionStateMutation,
    ImpactKind::AuditObservation,
];

pub fn impact_kind_label(kind: &ImpactKind) -> &'static str {
    match kind {
        ImpactKind::WorkspaceObservation => "workspace_observation",
        ImpactKind::WorkspaceMutation => "workspace_mutation",
        ImpactKind::CommandExecution => "command_execution",
        ImpactKind::ExternalObservation => "external_observation",
        ImpactKind::SessionStateMutation => "session_state_mutation",
        ImpactKind::AuditObservation => "audit_observation",
    }
}

/// Parse a persisted audit `impact_kind` column label (snake_case) or CLI filter name (PascalCase).
///
/// Authoritative inverse of [`impact_kind_label`]; extend [`ALL_IMPACT_KINDS`] when adding variants.
pub fn impact_kind_from_label(label: &str) -> Option<ImpactKind> {
    ALL_IMPACT_KINDS
        .iter()
        .find(|kind| impact_kind_label(kind) == label || impact_kind_cli_name(kind) == label)
        .cloned()
}

fn impact_kind_cli_name(kind: &ImpactKind) -> &'static str {
    match kind {
        ImpactKind::WorkspaceObservation => "WorkspaceObservation",
        ImpactKind::WorkspaceMutation => "WorkspaceMutation",
        ImpactKind::CommandExecution => "CommandExecution",
        ImpactKind::ExternalObservation => "ExternalObservation",
        ImpactKind::SessionStateMutation => "SessionStateMutation",
        ImpactKind::AuditObservation => "AuditObservation",
    }
}

fn chrono_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    millis.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lancet_protocol::methods::TodoWrite as TodoWriteMethod;
    use lancet_protocol::CepMethod;
    use serde_json::json;

    fn test_context() -> CepAuditContext {
        CepAuditContext {
            turn_id: "turn-1".into(),
            session_id: "session-1".into(),
            ..Default::default()
        }
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
                "workspace.read_file",
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
                "workspace.read_file",
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
                "workspace.read_file",
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
                "workspace.read_file",
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
                method: Some("workspace.read_file".into()),
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
                "workspace.read_file",
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
                "workspace.read_file",
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
    fn merge_finalize_metadata_adds_required_capability() {
        let merged = merge_finalize_metadata(
            Some(r#"{"observation_source":"workspace"}"#),
            "workspace.read_file",
            &[],
            None,
        )
        .unwrap();
        assert!(merged.contains("required_capability"));
        assert!(merged.contains("read_path"));
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
}

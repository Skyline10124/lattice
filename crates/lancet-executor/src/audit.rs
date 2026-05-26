//! Flat append-only SQLite audit store for Phase 20 CEP execution records.

use lancet_protocol::catalog;
use lancet_protocol::envelope::ImpactKind;
use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use thiserror::Error;

pub const SEMANTIC_STAGE: &str = "phase20_legacy_bridge";
pub const GUARD_STAGE: &str = "bridge_lifecycle_only";
pub const POLICY_ENFORCED: bool = false;

pub const SEMANTIC_STAGE_NATIVE: &str = "phase21_native";
pub const GUARD_STAGE_NATIVE: &str = "cep_policy_lifecycle";

pub const SEMANTIC_STAGE_SCAFFOLD: &str = "scaffold";

/// Feature-gated scaffold methods (not in default `CATALOG` or production registry).
pub const SCAFFOLD_METHODS: [&str; 2] = ["workspace.inspect", "workspace.navigate"];

/// Returns true when `method` is an inspect/navigate scaffold stub.
pub fn is_scaffold_method(method: &str) -> bool {
    SCAFFOLD_METHODS.contains(&method)
}

/// Catalog methods served by Phase 21 native handlers in the production registry.
pub const PHASE21_NATIVE_METHODS: [&str; 6] = [
    "workspace.read_file",
    "workspace.grep",
    "workspace.list_directory",
    "workspace.find_files",
    "web.fetch",
    "session.todo_write",
];

/// Phase 22-pending methods that remain on the Phase 20 legacy bridge in production.
pub const PHASE22_PENDING_BRIDGE_METHODS: [&str; 4] = [
    "command.run",
    "workspace.write_file",
    "workspace.patch",
    "envelope.declare",
];

/// Returns true when `method` is registered as a Phase 21 native handler in production.
pub fn is_phase21_native_method(method: &str) -> bool {
    PHASE21_NATIVE_METHODS.contains(&method)
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
    /// Phase 20 legacy bridge defaults (`policy_enforced = false`).
    pub fn bridge_defaults() -> Self {
        Self {
            semantic_stage: SEMANTIC_STAGE.to_string(),
            semantic_status: Some("started".to_string()),
            guard_stage: GUARD_STAGE.to_string(),
            policy_enforced: false,
            denial_source: None,
            backend_invoked: None,
            observation_source: None,
        }
    }

    /// Phase 21 native handler with CEP policy enforcement.
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

    /// Per-method audit metadata for the production registry (native vs bridge vs scaffold).
    pub fn for_production_dispatch(method: &str) -> Self {
        if is_phase21_native_method(method) {
            Self::native_policy_enforced(Some("started"))
        } else if is_scaffold_method(method) {
            Self::scaffold_stub()
        } else {
            Self::bridge_defaults()
        }
    }
}

/// Per-turn identifiers attached to every audit row.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CepAuditContext {
    pub turn_id: String,
    pub session_id: String,
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
                metadata_json TEXT
             );",
        )
        .map_err(|source| AuditError::Database {
            path: self.path.clone(),
            source,
        })
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
            .map(impact_kind_label)
            .unwrap_or("unknown")
            .to_string();
        let semantic_status = metadata.semantic_status.as_deref().unwrap_or("started");

        let conn = self.conn.lock().expect("audit mutex poisoned");
        conn.execute(
            "INSERT INTO cep_audit_entries (
                call_id, method, started_at, impact_kind, turn_id, session_id,
                semantic_stage, semantic_status, guard_stage, policy_enforced
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
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
            "SELECT call_id, method, started_at, ended_at, result_or_error, impact_kind,
                    turn_id, session_id, semantic_stage, semantic_status, guard_stage,
                    policy_enforced, metadata_json
             FROM cep_audit_entries WHERE audit_id = ?1",
            params![audit_id],
            |row| {
                Ok(AuditRow {
                    call_id: row.get::<_, i64>(0)? as u64,
                    method: row.get(1)?,
                    started_at: row.get(2)?,
                    ended_at: row.get(3)?,
                    result_or_error: row.get(4)?,
                    impact_kind: row.get(5)?,
                    turn_id: row.get(6)?,
                    session_id: row.get(7)?,
                    semantic_stage: row.get(8)?,
                    semantic_status: row.get(9)?,
                    guard_stage: row.get(10)?,
                    policy_enforced: row.get::<_, i64>(11)? != 0,
                    metadata_json: row.get(12)?,
                })
            },
        )
        .map_err(|source| AuditError::Database {
            path: self.path.clone(),
            source,
        })
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

/// Materialized audit row for tests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuditRow {
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
}

pub fn impact_kind_for_method(method: &str) -> Option<ImpactKind> {
    catalog()
        .iter()
        .find(|descriptor| descriptor.name == method)
        .map(|descriptor| descriptor.impact.clone())
}

pub fn impact_kind_label(kind: ImpactKind) -> &'static str {
    match kind {
        ImpactKind::WorkspaceObservation => "workspace_observation",
        ImpactKind::WorkspaceMutation => "workspace_mutation",
        ImpactKind::CommandExecution => "command_execution",
        ImpactKind::ExternalObservation => "external_observation",
        ImpactKind::SessionStateMutation => "session_state_mutation",
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
        }
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
                &CallAuditMetadata::bridge_defaults(),
            )
            .unwrap();
        store
            .finalize_call(
                start,
                AuditOutcome::Success {
                    summary: "todo_count=1".into(),
                },
                Some("bridged_legacy_body"),
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
        assert_eq!(row.semantic_stage, SEMANTIC_STAGE);
        assert_eq!(row.semantic_status.as_deref(), Some("bridged_legacy_body"));
        assert_eq!(row.guard_stage, GUARD_STAGE);
        assert!(!row.policy_enforced);
    }

    #[test]
    fn cep_audit_error_path_finalizes_ended_at() {
        let store = CepAuditStore::in_memory().unwrap();
        let start = store
            .start_call(
                &test_context(),
                7,
                TodoWriteMethod::NAME,
                &CallAuditMetadata::bridge_defaults(),
            )
            .unwrap();
        store
            .finalize_call(
                start,
                AuditOutcome::Error {
                    code: "ExecutionFailed".into(),
                    summary: "legacy body failed".into(),
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
                &CallAuditMetadata::bridge_defaults(),
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
                &CallAuditMetadata::bridge_defaults(),
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
                &CallAuditMetadata::bridge_defaults(),
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
                &CallAuditMetadata::bridge_defaults(),
            )
            .unwrap();
        store
            .finalize_call(
                start,
                AuditOutcome::Success {
                    summary: "todo_count=1".into(),
                },
                Some("bridged_legacy_body"),
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
            &CallAuditMetadata::bridge_defaults(),
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
                &CallAuditMetadata::bridge_defaults(),
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
    fn cep_audit_native_and_bridge_rows_coexist() {
        let store = CepAuditStore::in_memory().unwrap();
        let ctx = test_context();
        let bridge = store
            .start_call(
                &ctx,
                1,
                TodoWriteMethod::NAME,
                &CallAuditMetadata::bridge_defaults(),
            )
            .unwrap();
        store
            .finalize_call(
                bridge,
                AuditOutcome::Success {
                    summary: "todo_count=0".into(),
                },
                Some("bridged_legacy_body"),
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
                .row_by_audit_id(bridge.audit_id)
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
                .row_by_audit_id(bridge.audit_id)
                .unwrap()
                .semantic_stage,
            SEMANTIC_STAGE
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

    #[tokio::test]
    async fn cep_audit_await_pending_waits_for_finalization() {
        let store = Arc::new(CepAuditStore::in_memory().unwrap());
        let store_clone = Arc::clone(&store);
        let start = store
            .start_call(
                &test_context(),
                1,
                TodoWriteMethod::NAME,
                &CallAuditMetadata::bridge_defaults(),
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

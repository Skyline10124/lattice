// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Flat append-only SQLite audit store for CEP execution records.

use crate::evidence::{CallEvidence, EvidenceError, EvidenceId, StoredCallEvidence};
use lancet_catalog::{RequiredCapability, catalog};
use lancet_protocol::effect_types::ImpactKind;
use lancet_protocol::evidence::{
    AdmissionEvidence, DeniedOperationsRecord, ObservedEffect, ObservedEffectsRecord,
};
use rusqlite::{Connection, OpenFlags, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
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

/// Audit stage for a Component Tool call (#28): authorization is Cell capability
/// admission, not the legacy method-impact path. Distinct from
/// [`SEMANTIC_STAGE_UNROUTED`], which means the method has no handler at all.
pub const SEMANTIC_STAGE_COMPONENT: &str = "cell_component";

/// Guard stage for a Component Tool call: staged capability admission plus
/// per-operation Cell authorization, never the native policy lifecycle.
pub const GUARD_STAGE_COMPONENT: &str = "cell_admission";

/// Finalize `semantic_status` for a Component Tool call that completed.
pub const SEMANTIC_STATUS_COMPONENT: &str = "component_handler";

/// Finalize `semantic_status` for a refused Component Tool call. A Component
/// refusal is a Cell policy denial — the tool exists and was dispatched to, so
/// it is never `method_not_found`.
pub const SEMANTIC_STATUS_COMPONENT_DENIED: &str = "component_denied";

/// Feature-gated scaffold methods (not in default `CATALOG` or production registry).
pub const SCAFFOLD_METHODS: [&str; 2] = ["workspace.inspect", "workspace.navigate"];

/// Hard upper bound for filtered audit reads requested through CEP-facing helpers.
pub const AUDIT_QUERY_MAX_ROWS: usize = 10_000;

/// Schema version stamped in `PRAGMA user_version`. Version 0 means a database
/// written before audit schemas were versioned at all.
///
/// Version 2 adds the #112 Cell evidence tables (`evidence_calls`,
/// `evidence_effects`, `evidence_denials`). A version-1 file is adopted and
/// gains them additively; a file already claiming 2 must carry their full shape
/// or it is refused with an operator action rather than altered.
pub const AUDIT_SCHEMA_VERSION: i64 = crate::evidence::EVIDENCE_SCHEMA_VERSION;

/// The schema version before the #112 evidence tables existed. A file stamped
/// with this version predates them and is upgraded additively on open.
///
/// Referenced by the compatibility test that pins that upgrade
/// (`evidence_tests::pre_evidence_database_gains_the_tables_and_keeps_history`)
/// so the boundary is named rather than a bare `1` in a test.
pub const AUDIT_SCHEMA_VERSION_PRE_EVIDENCE: i64 = 1;

/// `cep_audit_entries` columns an unversioned database must already have to be
/// adopted. Only the NOT NULL columns are listed: `ended_at`,
/// `result_or_error`, `semantic_status`, `metadata_json`, `model_id` and
/// `runtime_version` are nullable and are restored by the additive DDL in
/// [`CepAuditStore::init_schema`], but a missing NOT NULL column means the file
/// predates the current row shape and no additive statement can repair it.
const REQUIRED_AUDIT_COLUMNS: [&str; 10] = [
    "audit_id",
    "call_id",
    "method",
    "started_at",
    "impact_kind",
    "turn_id",
    "session_id",
    "semantic_stage",
    "guard_stage",
    "policy_enforced",
];

/// `attestation_records` columns an unversioned database must already have.
/// Every column of this table is NOT NULL, so an older shape cannot be repaired
/// additively either; detection covers both tables the store writes, not just
/// the call rows (C5).
const REQUIRED_ATTESTATION_COLUMNS: [&str; 6] = [
    "turn_id",
    "session_id",
    "statement_json",
    "predicate_type",
    "generated_at",
    "source_audit_range_or_hash",
];

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
            observation_source: None,
        }
    }

    /// Unknown production method (not in the catalog). Dispatch will reject it with
    /// `MethodNotFound`; no handler or policy hook runs, so this must not claim
    /// policy enforcement.
    pub fn unrouted() -> Self {
        Self {
            semantic_stage: SEMANTIC_STAGE_UNROUTED.to_string(),
            semantic_status: Some("method_not_found".to_string()),
            guard_stage: GUARD_STAGE_NATIVE.to_string(),
            policy_enforced: false,
            denial_source: None,
            observation_source: None,
        }
    }

    /// Component Tool call governed by Cell capability admission (#28).
    ///
    /// The tool is in the closed Component catalog, so the call is routed and
    /// authorized — by the unified Cell path, not by method impact identity.
    /// Labeling it `unrouted` would claim the opposite of both facts.
    pub fn for_component_dispatch() -> Self {
        Self {
            semantic_stage: SEMANTIC_STAGE_COMPONENT.to_string(),
            semantic_status: Some("started".to_string()),
            guard_stage: GUARD_STAGE_COMPONENT.to_string(),
            policy_enforced: true,
            denial_source: None,
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
            observation_source: None,
        }
    }

    pub fn with_denial_source(mut self, source: &str) -> Self {
        self.denial_source = Some(source.to_string());
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
    ///
    /// Component Tools are not in this catalog and must never reach this
    /// constructor: they are labeled by [`Self::for_component_dispatch`].
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
    /// Resolved model id for PROV Agent reconstruction; empty in tests unless set.
    pub model_id: String,
    /// Runtime / crate version string persisted on each row.
    pub runtime_version: String,
}

/// Stable rejection stage labels persisted in `metadata_json.rejection_stage`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectionStage {
    SchemaValidation,
    PolicyGate,
    /// Guard L1 path validation or the concrete Local Workspace's secure
    /// path resolution rejected the call (post-#60 (Codeberg #38) vocabulary;
    /// replaces the Upper-era `adapter_preflight`).
    PathGuard,
    /// The concrete Local Workspace's I/O failed after policy and guard
    /// checks passed (replaces the Upper-era `backend_execution`).
    LocalIo,
}

impl RejectionStage {
    /// Snake-case wire label for JSON metadata and SQL filters.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SchemaValidation => "schema_validation",
            Self::PolicyGate => "policy_gate",
            Self::PathGuard => "path_guard",
            Self::LocalIo => "local_io",
        }
    }
}

/// Typed rejection evidence merged into durable call metadata.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RejectionRecord {
    pub rejection_stage: RejectionStage,
    pub denial_source: String,
}

/// Filters for [`CepAuditStore::query_rows`] (operator audit query/tail/replay).
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
    pub started_after: Option<String>,
    pub started_before: Option<String>,
    /// Filter by `metadata_json.rejection_stage` (no schema migration).
    pub rejection_stage: Option<RejectionStage>,
    /// Exclude methods with this prefix before applying any row limit.
    pub exclude_method_prefix: Option<String>,
    /// Database-level row cap for this query, clamped to [`AUDIT_QUERY_MAX_ROWS`].
    pub limit: Option<usize>,
}

/// Stored unsigned in-toto-shaped attestation for a turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttestationRecord {
    pub turn_id: String,
    pub session_id: String,
    pub statement_json: String,
    pub predicate_type: String,
    pub generated_at: String,
    pub source_audit_range_or_hash: String,
}

/// Durable record of one explicit Rewind (GitHub #64).
///
/// Captures the target, the reversed steps, the per-path preflight
/// revisions, and the outcome — hashes, paths, and object ids only, never
/// file content. Written on success, on structured conflict, and on a ref
/// durability failure alike.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RewindAuditRecord {
    /// Session the Rewind ran in.
    pub session_id: String,
    /// The selected target Turn's commit id.
    pub target_turn_id: String,
    /// Workspace-history identity the Rewind moved to (`None` = empty).
    pub target_history_oid: Option<String>,
    /// Workspace-history identity the Rewind started from (`None` = empty).
    pub from_history_oid: Option<String>,
    /// `success`, `conflict`, `history_ref_failure`, or `session_ref_failure`.
    pub outcome: String,
    /// JSON array of reversed steps (commit/method/turn/call/path/hash).
    pub steps_json: String,
    /// JSON array of per-path preflight observations (revisions only).
    pub preflight_json: String,
    /// JSON array of restored paths and their post-restore revisions.
    pub restored_json: Option<String>,
    /// JSON array of divergent paths when the outcome is a conflict.
    pub conflicts_json: Option<String>,
    /// RFC 3339 timestamp supplied by the writer.
    pub recorded_at: String,
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

fn required_capability_for_method(method: &str) -> Option<RequiredCapability> {
    catalog()
        .iter()
        .find(|descriptor| descriptor.name == method)
        .map(|descriptor| descriptor.capability)
}

/// One Component Tool call's evidence, as recorded by the runtime ledger
/// (#28 B5/A2). *Why* the call was authorized (`admission`) and *what actually
/// happened* (`effects`, `denied`) travel separately; a capability alias is
/// admission evidence only and never becomes an effect. Native handler calls
/// pass the default (no Component evidence).
///
/// Since #112 the durable home of all three records is the versioned evidence
/// table, and `evidence_id` is the identity that resolves them. The records are
/// still carried here because the audit row's own summary keys
/// (`component_cells`) are derived from them.
#[derive(Clone, Copy, Debug, Default)]
pub struct ComponentCallEvidence<'a> {
    pub admission: Option<&'a AdmissionEvidence>,
    pub effects: Option<&'a ObservedEffectsRecord>,
    pub denied: Option<&'a DeniedOperationsRecord>,
    /// Identity of this call's row in the evidence table. `None` for a native
    /// handler call, which produces no Cell evidence.
    pub evidence_id: Option<EvidenceId>,
}

/// Merge transport and catalog summary keys into metadata JSON only.
///
/// #112 criterion 3: `metadata_json` is **no longer the authoritative carrier**
/// of structured evidence. The `AdmissionEvidence` / `ObservedEffectsRecord` /
/// `DeniedOperationsRecord` payloads live in the versioned evidence tables
/// (`crate::evidence`); this row carries only `evidence_id`, the stable identity
/// that resolves them. A reader that wants the evidence follows the id.
///
/// What stays here is audit-row summary, not evidence: `required_capability`,
/// the label-ordered `observed_capabilities`, and `component_cells` — the
/// 4-segment Cell audit keys (`ctx:ext:scope:trust`) kept as a *query key* for
/// the operator read path (spec §6.3), derived from the observed effects so it
/// can never disagree with the ledger (C8). Both are projections of evidence,
/// cheap to recompute, and neither is a second record shape.
pub fn merge_finalize_metadata(
    existing: Option<&str>,
    method: &str,
    observed_capabilities: &[RequiredCapability],
    component: &ComponentCallEvidence<'_>,
) -> Option<String> {
    let mut obj = existing
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .unwrap_or_else(|| serde_json::json!({}));

    let Some(obj_map) = obj.as_object_mut() else {
        return existing.map(str::to_string);
    };

    if let Some(capability) = required_capability_for_method(method) {
        obj_map.insert(
            "required_capability".into(),
            serde_json::Value::String(required_capability_label(capability).into()),
        );
    }

    if !observed_capabilities.is_empty() {
        let caps: BTreeSet<&str> = observed_capabilities
            .iter()
            .copied()
            .map(required_capability_label)
            .collect();
        let list: Vec<_> = caps
            .into_iter()
            .map(|cap| serde_json::Value::String(cap.to_string()))
            .collect();
        obj_map.insert(
            "observed_capabilities".into(),
            serde_json::Value::Array(list),
        );
    }

    // #112 criterion 3: the row points at the evidence, it does not carry it.
    // `evidence_id` is written for every Component call — including a refused
    // one, whose evidence is its `AdmissionEvidence` — so "a Component row
    // always resolves to evidence" is checkable from the row alone.
    if let Some(evidence_id) = component.evidence_id {
        obj_map.insert(
            "evidence_id".into(),
            serde_json::Value::Number(evidence_id.0.into()),
        );
    }

    // `component_cells` stays a query key, derived from the effects (C8). It is
    // a projection of evidence, not a second record: it carries no version, no
    // target, no measure, and no admission basis.
    if let Some(record) = component.effects {
        let mut effects: Vec<ObservedEffect> = record.effects.clone();
        effects.sort_by_key(|effect| effect.seq);
        let cells: Vec<_> = effects
            .iter()
            .map(|effect| serde_json::Value::String(effect.cell.audit_key(effect.scope.trust)))
            .collect();
        if !cells.is_empty() {
            obj_map.insert("component_cells".into(), serde_json::Value::Array(cells));
        }
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
    #[error("unsafe audit database path at {path}: {reason}")]
    UnsafePath { path: PathBuf, reason: String },
    /// The store refuses to open a database whose schema it cannot write
    /// without rewriting or dropping existing history; the message carries the
    /// operator action.
    #[error("incompatible audit schema at {path}: {reason}")]
    IncompatibleSchema { path: PathBuf, reason: String },
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
            reject_symlink_path(parent)?;
            std::fs::create_dir_all(parent).map_err(|source| AuditError::Database {
                path: path.clone(),
                source: rusqlite::Error::ToSqlConversionFailure(Box::new(source)),
            })?;
            reject_symlink_path(parent)?;
        }
        reject_symlink_path(&path)?;
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

    /// Create or adopt the schema and stamp [`AUDIT_SCHEMA_VERSION`].
    ///
    /// An unversioned database is adopted only when both `cep_audit_entries`
    /// and `attestation_records` still carry every required column; an older
    /// shape is reported with an operator action instead of being altered, so
    /// audit history is never silently dropped or rewritten (the read-only
    /// legacy import stays the supported way to carry it forward). Detection is
    /// by column name: a table whose names match but whose column types differ
    /// is still adopted and fails later at INSERT.
    fn init_schema(&self) -> Result<(), AuditError> {
        let conn = self.conn.lock().expect("audit mutex poisoned");
        let version = self.schema_version(&conn)?;
        if version > AUDIT_SCHEMA_VERSION {
            return Err(AuditError::IncompatibleSchema {
                path: self.path.clone(),
                reason: format!(
                    "database reports schema version {version}, this binary writes \
                     {AUDIT_SCHEMA_VERSION}; upgrade the binary or move the database aside"
                ),
            });
        }
        if version == 0
            && let Some(missing) = self.missing_required_columns(&conn)?
        {
            return Err(AuditError::IncompatibleSchema {
                path: self.path.clone(),
                reason: format!(
                    "unversioned database is missing required column(s) {missing}; \
                     the database is left untouched — move it aside so a new one is created, \
                     then bring the history over with the read-only legacy import"
                ),
            });
        }
        // #112 criterion 4: a file that already claims the evidence schema
        // version must carry the evidence tables' full shape. A partial or
        // renamed evidence table cannot be repaired additively (the missing
        // column would be NOT NULL), so it is reported with an operator action
        // rather than altered — history is never silently dropped.
        if version == AUDIT_SCHEMA_VERSION
            && let Some(missing) = self.missing_evidence_columns(&conn)?
        {
            return Err(AuditError::IncompatibleSchema {
                path: self.path.clone(),
                reason: format!(
                    "database reports schema version {version} but its evidence table shape is \
                     missing required column(s) {missing}; the database is left untouched — \
                     move it aside so a new one is created, then bring the history over with \
                     the read-only legacy import"
                ),
            });
        }

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
             );
             CREATE TABLE IF NOT EXISTS rewind_records (
                rewind_id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                target_turn_id TEXT NOT NULL,
                target_history_oid TEXT,
                from_history_oid TEXT,
                outcome TEXT NOT NULL,
                steps_json TEXT NOT NULL,
                preflight_json TEXT NOT NULL,
                restored_json TEXT,
                conflicts_json TEXT,
                recorded_at TEXT NOT NULL
             );",
        )
        .map_err(|source| AuditError::Database {
            path: self.path.clone(),
            source,
        })?;
        // #112: the Cell evidence tables. Additive, so a version-1 file gains
        // them on first open without touching its existing history.
        conn.execute_batch(crate::evidence::EVIDENCE_SCHEMA_DDL)
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
        if version != AUDIT_SCHEMA_VERSION {
            conn.pragma_update(None, "user_version", AUDIT_SCHEMA_VERSION)
                .map_err(|source| AuditError::Database {
                    path: self.path.clone(),
                    source,
                })?;
        }
        Ok(())
    }

    fn schema_version(&self, conn: &Connection) -> Result<i64, AuditError> {
        conn.pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(|source| AuditError::Database {
                path: self.path.clone(),
                source,
            })
    }

    /// Required columns absent from either table this store writes, qualified by
    /// table name, or `None` when both are complete or absent (fresh file).
    fn missing_required_columns(&self, conn: &Connection) -> Result<Option<String>, AuditError> {
        let mut missing = Vec::new();
        for (table, required) in [
            ("cep_audit_entries", &REQUIRED_AUDIT_COLUMNS[..]),
            ("attestation_records", &REQUIRED_ATTESTATION_COLUMNS[..]),
        ] {
            missing.extend(self.missing_columns_of(conn, table, required)?);
        }
        if missing.is_empty() {
            Ok(None)
        } else {
            Ok(Some(missing.join(", ")))
        }
    }

    /// Required evidence-table columns absent from a file that already claims
    /// [`AUDIT_SCHEMA_VERSION`], qualified by table name. An absent evidence
    /// table reports nothing — the additive DDL creates it, which is how a
    /// version-1 file is upgraded.
    fn missing_evidence_columns(&self, conn: &Connection) -> Result<Option<String>, AuditError> {
        let mut missing = Vec::new();
        for (table, required) in [
            (
                "evidence_calls",
                &crate::evidence::REQUIRED_EVIDENCE_CALL_COLUMNS[..],
            ),
            (
                "evidence_effects",
                &crate::evidence::REQUIRED_EVIDENCE_EFFECT_COLUMNS[..],
            ),
            (
                "evidence_denials",
                &crate::evidence::REQUIRED_EVIDENCE_DENIAL_COLUMNS[..],
            ),
        ] {
            missing.extend(self.missing_columns_of(conn, table, required)?);
        }
        if missing.is_empty() {
            Ok(None)
        } else {
            Ok(Some(missing.join(", ")))
        }
    }

    /// Required columns of one table that the file does not have. An absent
    /// table reports nothing: `CREATE TABLE IF NOT EXISTS` will create it.
    fn missing_columns_of(
        &self,
        conn: &Connection,
        table: &str,
        required: &[&str],
    ) -> Result<Vec<String>, AuditError> {
        let mut present = BTreeSet::new();
        conn.pragma(None, "table_info", table, |row| {
            present.insert(row.get::<_, String>("name")?);
            Ok(())
        })
        .map_err(|source| AuditError::Database {
            path: self.path.clone(),
            source,
        })?;
        if present.is_empty() {
            return Ok(Vec::new());
        }
        Ok(required
            .iter()
            .filter(|column| !present.contains(**column))
            .map(|column| format!("{table}.{column}"))
            .collect())
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

    /// Record one Tool call's Cell evidence and return its stable identity
    /// (#112 criteria 1/2/3/5).
    ///
    /// This is the authoritative write path for structured evidence. The
    /// returned [`EvidenceId`] is what a later runtime event references; it does
    /// not depend on `cep_audit_entries`, so it survives that store's removal.
    ///
    /// Every outcome is recorded, not just successes: an admission denial
    /// carries its `AdmissionEvidence` with
    /// [`EvidenceOutcome::AdmissionDenied`], per-operation refusals land as
    /// `DeniedOperation`, and a failed call keeps the operation effects it had
    /// already produced.
    pub fn record_call_evidence(
        &self,
        evidence: &CallEvidence<'_>,
    ) -> Result<EvidenceId, EvidenceError> {
        let recorded_at = chrono_now();
        let mut conn = self.conn.lock().expect("audit mutex poisoned");
        crate::evidence::insert_call_evidence(&mut conn, evidence, &recorded_at)
    }

    /// Read one call's evidence by its identity.
    pub fn call_evidence(
        &self,
        evidence_id: EvidenceId,
    ) -> Result<Option<StoredCallEvidence>, EvidenceError> {
        let conn = self.conn.lock().expect("audit mutex poisoned");
        crate::evidence::call_evidence_by_id(&conn, evidence_id)
    }

    /// Read every Tool call's evidence for one turn, oldest first (#112
    /// criterion 1: addressable by turn).
    pub fn call_evidence_for_turn(
        &self,
        turn_id: &str,
    ) -> Result<Vec<StoredCallEvidence>, EvidenceError> {
        let conn = self.conn.lock().expect("audit mutex poisoned");
        crate::evidence::call_evidence_for_turn(&conn, turn_id)
    }

    /// Read the evidence of one dispatched call (#112 criterion 1: addressable
    /// by call), identified the way the transport identifies a call: session +
    /// turn + per-turn `call_id`.
    pub fn call_evidence_for_call(
        &self,
        session_id: &str,
        turn_id: &str,
        call_id: u64,
    ) -> Result<Vec<StoredCallEvidence>, EvidenceError> {
        let conn = self.conn.lock().expect("audit mutex poisoned");
        crate::evidence::call_evidence_for_call(&conn, session_id, turn_id, call_id)
    }

    /// Run a statement against the store's connection. Test-only: the evidence
    /// contract tests need to corrupt a persisted `version` or enum label to
    /// prove the read path refuses it rather than coercing it.
    #[cfg(test)]
    pub(crate) fn with_connection_for_tests<T>(
        &self,
        f: impl FnOnce(&Connection) -> Result<T, rusqlite::Error>,
    ) -> Result<T, rusqlite::Error> {
        let conn = self.conn.lock().expect("audit mutex poisoned");
        f(&conn)
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

    /// Filtered read for PROV projection and operator audit APIs (parameterized SQL only).
    pub fn query_rows(&self, filters: &AuditQueryFilters) -> Result<Vec<AuditRow>, AuditError> {
        let mut sql = String::from(
            "SELECT audit_id, call_id, method, started_at, ended_at, result_or_error, impact_kind,
                    turn_id, session_id, semantic_stage, semantic_status, guard_stage,
                    policy_enforced, metadata_json, model_id, runtime_version
             FROM cep_audit_entries WHERE 1=1 AND ended_at IS NOT NULL AND result_or_error IS NOT NULL",
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
        if let Some(stage) = filters.rejection_stage {
            sql.push_str(" AND json_extract(metadata_json, '$.rejection_stage') = ?");
            bind.push(Box::new(stage.as_str().to_string()));
        }
        if let Some(exclude_prefix) = &filters.exclude_method_prefix {
            sql.push_str(" AND method NOT LIKE ?");
            bind.push(Box::new(format!("{exclude_prefix}%")));
        }
        if let Some(verdict) = &filters.verdict {
            match verdict.as_str() {
                "success" => {
                    sql.push_str(" AND semantic_status IN (?, ?) AND result_or_error LIKE ?");
                    bind.push(Box::new("native_handler".to_string()));
                    bind.push(Box::new(SEMANTIC_STATUS_COMPONENT.to_string()));
                    bind.push(Box::new("success:%".to_string()));
                }
                "denied" => {
                    sql.push_str(" AND semantic_status IN (?, ?, ?, ?)");
                    for status in [
                        "native_handler_denied",
                        "method_not_found",
                        "scaffold_stub",
                        SEMANTIC_STATUS_COMPONENT_DENIED,
                    ] {
                        bind.push(Box::new(status.to_string()));
                    }
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

        let effective_limit = filters.limit.map(|limit| limit.min(AUDIT_QUERY_MAX_ROWS));

        sql.push_str(" ORDER BY audit_id ASC");
        if let Some(limit) = effective_limit {
            sql.push_str(" LIMIT ?");
            bind.push(Box::new(limit as i64));
        }

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

    /// Append one durable Rewind record (#64).
    pub fn insert_rewind_record(&self, record: &RewindAuditRecord) -> Result<(), AuditError> {
        let conn = self.conn.lock().expect("audit mutex poisoned");
        conn.execute(
            "INSERT INTO rewind_records (
                session_id, target_turn_id, target_history_oid, from_history_oid,
                outcome, steps_json, preflight_json, restored_json, conflicts_json,
                recorded_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                record.session_id,
                record.target_turn_id,
                record.target_history_oid,
                record.from_history_oid,
                record.outcome,
                record.steps_json,
                record.preflight_json,
                record.restored_json,
                record.conflicts_json,
                record.recorded_at,
            ],
        )
        .map_err(|source| AuditError::Database {
            path: self.path.clone(),
            source,
        })?;
        Ok(())
    }

    /// All Rewind records for a session, oldest first.
    pub fn rewind_records_for_session(
        &self,
        session_id: &str,
    ) -> Result<Vec<RewindAuditRecord>, AuditError> {
        let conn = self.conn.lock().expect("audit mutex poisoned");
        let mut stmt = conn
            .prepare(
                "SELECT session_id, target_turn_id, target_history_oid, from_history_oid,
                        outcome, steps_json, preflight_json, restored_json, conflicts_json,
                        recorded_at
                 FROM rewind_records WHERE session_id = ?1 ORDER BY rewind_id ASC",
            )
            .map_err(|source| AuditError::Database {
                path: self.path.clone(),
                source,
            })?;
        let rows = stmt
            .query_map(params![session_id], |row| {
                Ok(RewindAuditRecord {
                    session_id: row.get(0)?,
                    target_turn_id: row.get(1)?,
                    target_history_oid: row.get(2)?,
                    from_history_oid: row.get(3)?,
                    outcome: row.get(4)?,
                    steps_json: row.get(5)?,
                    preflight_json: row.get(6)?,
                    restored_json: row.get(7)?,
                    conflicts_json: row.get(8)?,
                    recorded_at: row.get(9)?,
                })
            })
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

    /// Import historical audit rows from `source_path` when this store is empty.
    ///
    /// Opens the source read-only, copies `cep_audit_entries` and `attestation_records`
    /// into the destination in one transaction, preserves row ids and metadata, and
    /// never writes to or mutates the source database or its WAL/SHM sidecars.
    /// Idempotent: no-op when the destination already has audit rows.
    pub fn import_legacy_read_only_if_empty(
        &self,
        source_path: impl AsRef<Path>,
    ) -> Result<(), AuditError> {
        if self.row_count()? > 0 {
            return Ok(());
        }

        let source_path = source_path.as_ref().to_path_buf();
        reject_symlink_path(&source_path)?;

        let source = Connection::open_with_flags(&source_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|source| AuditError::Database {
                path: source_path.clone(),
                source,
            })?;

        let dest = self.conn.lock().expect("audit mutex poisoned");
        dest.execute("BEGIN IMMEDIATE", [])
            .map_err(|source| AuditError::Database {
                path: self.path.clone(),
                source,
            })?;

        let import_result = import_legacy_tables(&source, &dest, &self.path);
        match import_result {
            Ok(()) => {
                dest.execute("COMMIT", [])
                    .map_err(|source| AuditError::Database {
                        path: self.path.clone(),
                        source,
                    })?;
            }
            Err(err) => {
                let _ = dest.execute("ROLLBACK", []);
                return Err(err);
            }
        }
        Ok(())
    }
}

/// Drop an imported row's `evidence_id`, which does not resolve in this store.
///
/// The import copies audit rows only — the evidence tables are not carried, and
/// `evidence_id` is a per-store AUTOINCREMENT rowid that restarts at 1. Keeping
/// the pointer would let a historical row resolve to an unrelated *later* call's
/// evidence: silently, with no version mismatch and no error. A row with no
/// pointer is correctly read as "this call has no evidence in this store", which
/// is the truth.
fn strip_evidence_pointer(metadata_json: Option<&str>) -> Option<String> {
    let raw = metadata_json?;
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(raw) else {
        // Not JSON: nothing to strip, and rewriting it would lose data.
        return Some(raw.to_string());
    };
    let Some(object) = value.as_object_mut() else {
        return Some(raw.to_string());
    };
    if object.remove("evidence_id").is_none() {
        return Some(raw.to_string());
    }
    serde_json::to_string(&value).ok()
}

fn import_legacy_tables(
    source: &Connection,
    dest: &Connection,
    dest_path: &Path,
) -> Result<(), AuditError> {
    let mut audit_stmt = source
        .prepare(
            "SELECT audit_id, call_id, method, started_at, ended_at, result_or_error, impact_kind,
                    turn_id, session_id, semantic_stage, semantic_status, guard_stage,
                    policy_enforced, metadata_json, model_id, runtime_version
             FROM cep_audit_entries ORDER BY audit_id ASC",
        )
        .map_err(|source| AuditError::Database {
            path: dest_path.to_path_buf(),
            source,
        })?;
    let audit_rows =
        audit_stmt
            .query_map([], map_audit_row)
            .map_err(|source| AuditError::Database {
                path: dest_path.to_path_buf(),
                source,
            })?;
    for row in audit_rows {
        let row = row.map_err(|source| AuditError::Database {
            path: dest_path.to_path_buf(),
            source,
        })?;
        dest.execute(
            "INSERT INTO cep_audit_entries (
                audit_id, call_id, method, started_at, ended_at, result_or_error, impact_kind,
                turn_id, session_id, semantic_stage, semantic_status, guard_stage,
                policy_enforced, metadata_json, model_id, runtime_version
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![
                row.audit_id,
                row.call_id as i64,
                row.method,
                row.started_at,
                row.ended_at,
                row.result_or_error,
                row.impact_kind,
                row.turn_id,
                row.session_id,
                row.semantic_stage,
                row.semantic_status,
                row.guard_stage,
                if row.policy_enforced { 1 } else { 0 },
                strip_evidence_pointer(row.metadata_json.as_deref()),
                row.model_id,
                row.runtime_version,
            ],
        )
        .map_err(|source| AuditError::Database {
            path: dest_path.to_path_buf(),
            source,
        })?;
    }

    let mut attestation_stmt = source
        .prepare(
            "SELECT turn_id, session_id, statement_json, predicate_type, generated_at,
                    source_audit_range_or_hash
             FROM attestation_records",
        )
        .map_err(|source| AuditError::Database {
            path: dest_path.to_path_buf(),
            source,
        })?;
    let attestation_rows = attestation_stmt
        .query_map([], |row| {
            Ok(AttestationRecord {
                turn_id: row.get(0)?,
                session_id: row.get(1)?,
                statement_json: row.get(2)?,
                predicate_type: row.get(3)?,
                generated_at: row.get(4)?,
                source_audit_range_or_hash: row.get(5)?,
            })
        })
        .map_err(|source| AuditError::Database {
            path: dest_path.to_path_buf(),
            source,
        })?;
    for row in attestation_rows {
        let record = row.map_err(|source| AuditError::Database {
            path: dest_path.to_path_buf(),
            source,
        })?;
        dest.execute(
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
            path: dest_path.to_path_buf(),
            source,
        })?;
    }

    let _ = dest.execute(
        "UPDATE sqlite_sequence SET seq = (SELECT COALESCE(MAX(audit_id), 0) FROM cep_audit_entries)
         WHERE name = 'cep_audit_entries'",
        [],
    );
    Ok(())
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
    if value.is_empty() { None } else { Some(value) }
}

fn reject_symlink_path(path: &Path) -> Result<(), AuditError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(AuditError::UnsafePath {
            path: path.to_path_buf(),
            reason: "audit store refuses to follow symlink paths".to_string(),
        }),
        Ok(_) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(AuditError::Database {
            path: path.to_path_buf(),
            source: rusqlite::Error::ToSqlConversionFailure(Box::new(err)),
        }),
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
#[path = "audit_tests.rs"]
mod tests;

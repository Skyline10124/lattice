// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Self-contained SQLite source extractor.
//!
//! Reads all five legacy SQLite tables via a direct `rusqlite::Connection` —
//! NOT through the `SessionGraphStore` trait. The trait's `log` and
//! `head_transcript` are HEAD-anchored and miss reset-orphans, non-HEAD tips,
//! and full message ordinals; a self-contained extractor is also what lets
//! `sqlite_graph/` be deleted in Plan 07.
//!
//! The topological walk is copied (not imported) from
//! `sqlite_graph/helpers.rs:collect_reachable_commits_topological` because that
//! directory is deleted in Plan 07. The walk covers the reachable closure of
//! every `graph_refs.commit_id` tip plus each session's `head_commit_id`
//! — not just HEAD.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};

use crate::graph::{GraphMessage, GraphToolCall, TurnLifecycleEvidence};
use crate::{Result, SessionError};

/// Maximum number of `?` bind parameters per single `IN (...)` clause.
/// Mirrors `sqlite_graph::SQLITE_BIND_CHUNK_SIZE` (SQLite's default
/// `SQLITE_MAX_VARIABLE_NUMBER` is 999).
const SQLITE_BIND_CHUNK_SIZE: usize = 900;

/// A legacy session row plus its refs and topologically-ordered commits.
#[derive(Debug, Clone)]
pub(crate) struct ExtractedSession {
    pub session_id: String,
    pub label: Option<String>,
    pub active_agent_name: String,
    pub head_ref: Option<String>,
    pub head_commit_id: Option<String>,
    /// Defaults to `"main"` when the SQLite column is NULL (Pitfall #4).
    pub project_target_branch: String,
    pub created_at: String,
    pub updated_at: String,
    pub refs: Vec<ExtractedRef>,
    /// Commits in parents-before-children order (write order).
    pub commits: Vec<ExtractedCommit>,
}

/// A legacy `graph_refs` row.
#[derive(Debug, Clone)]
pub(crate) struct ExtractedRef {
    pub name: String,
    pub commit_id: Option<String>,
}

/// A legacy `graph_commits` row with its messages loaded and ordered by
/// `ordinal`, plus the `snapshot_lost` flag from `graph_snapshot_state`.
#[derive(Debug, Clone)]
pub(crate) struct ExtractedCommit {
    pub commit_id: String,
    pub parent_ids: Vec<String>,
    pub agent_name: String,
    pub profile_snapshot_hash: String,
    pub profile_snapshot_json: String,
    pub model: String,
    pub provider: String,
    pub status: String,
    pub commit_message: String,
    pub usage_json: Option<String>,
    pub terminal_reason: Option<String>,
    pub disposition: String,
    pub lifecycle_json: String,
    pub created_at: String,
    pub messages: Vec<GraphMessage>,
    pub snapshot_lost: bool,
}

/// Persistent legacy SHA-256 `commit_id` → new Git OID map.
///
/// Checkpointed at `<target_repo>/.lattice/migration/idmap.json` so a restart
/// skips commits already written (content-addressed determinism + idempotent
/// refs make re-runs converge).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct Idmap {
    #[serde(default)]
    pub entries: HashMap<String, String>,
}

impl Idmap {
    pub fn get(&self, legacy_id: &str) -> Option<&str> {
        self.entries.get(legacy_id).map(|s| s.as_str())
    }

    pub fn insert(&mut self, legacy_id: String, oid: String) {
        self.entries.insert(legacy_id, oid);
    }

    /// Load from `<target_repo>/.lattice/migration/idmap.json` if present.
    pub fn load(target_repo: &Path) -> Result<Self> {
        let path = idmap_path(target_repo);
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| {
                SessionError::Git(format!("parse idmap.json: {e}"))
            }),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(SessionError::Io {
                operation: "reading idmap",
                path,
                source,
            }),
        }
    }

    /// Atomically checkpoint to `<target_repo>/.lattice/migration/idmap.json`.
    pub fn save(&self, target_repo: &Path) -> Result<()> {
        let path = idmap_path(target_repo);
        let parent = path.parent().expect("idmap path has parent");
        crate::create_dir_all(parent)?;
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|e| SessionError::Git(format!("serialize idmap: {e}")))?;
        crate::git_graph::helpers::atomic_write(&path, &bytes)
    }
}

fn idmap_path(target_repo: &Path) -> std::path::PathBuf {
    target_repo
        .join(".lattice")
        .join("migration")
        .join("idmap.json")
}

/// Extract every session with its refs and topologically-ordered commits.
///
/// `run_migration` and `verify_equivalence` both own their connection and call
/// this directly (the path-opening `extract_all` variant had no callers
/// and was removed).
pub(crate) fn extract_all_with_conn(conn: &Connection) -> Result<Vec<ExtractedSession>> {
    let session_rows = read_session_rows(conn)?;
    let mut out = Vec::with_capacity(session_rows.len());
    for srow in session_rows {
        let refs = read_ref_rows(conn, &srow.session_id)?;
        let tips = collect_tips(&srow, &refs);
        let topo = collect_reachable_commits_topological(conn, &srow.session_id, &tips)?;
        let mut commits = Vec::with_capacity(topo.len());
        for cid in &topo {
            commits.push(read_commit(conn, &srow.session_id, cid)?);
        }
        out.push(ExtractedSession {
            session_id: srow.session_id,
            label: srow.label,
            active_agent_name: srow.active_agent_name,
            head_ref: srow.head_ref,
            head_commit_id: srow.head_commit_id,
            project_target_branch: srow.project_target_branch,
            created_at: srow.created_at,
            updated_at: srow.updated_at,
            refs,
            commits,
        });
    }
    Ok(out)
}

struct SessionRow {
    session_id: String,
    label: Option<String>,
    active_agent_name: String,
    head_ref: Option<String>,
    head_commit_id: Option<String>,
    project_target_branch: String,
    created_at: String,
    updated_at: String,
}

fn read_session_rows(conn: &Connection) -> Result<Vec<SessionRow>> {
    let mut stmt = conn
        .prepare(
            "SELECT session_id, label, active_agent_name, head_ref, head_commit_id,
                    project_target_branch, created_at, updated_at
             FROM graph_sessions",
        )
        .map_err(|source| SessionError::Database {
            path: Path::new("").to_path_buf(),
            source,
        })?;
    let rows = stmt
        .query_map([], |row| {
            let project_target_branch: Option<String> = row.get(5)?;
            Ok(SessionRow {
                session_id: row.get(0)?,
                label: row.get(1)?,
                active_agent_name: row.get(2)?,
                head_ref: row.get(3)?,
                head_commit_id: row.get(4)?,
                // Pitfall #4 / A1: NULL project_target_branch defaults to "main".
                project_target_branch: project_target_branch
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "main".to_string()),
                created_at: row.get(6)?,
                updated_at: row.get(7)?,
            })
        })
        .map_err(|source| SessionError::Database {
            path: Path::new("").to_path_buf(),
            source,
        })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|source| SessionError::Database {
            path: Path::new("").to_path_buf(),
            source,
        })?);
    }
    Ok(out)
}

fn read_ref_rows(conn: &Connection, session_id: &str) -> Result<Vec<ExtractedRef>> {
    let mut stmt = conn
        .prepare(
            "SELECT name, commit_id
             FROM graph_refs WHERE session_id=?1",
        )
        .map_err(|source| SessionError::Database {
            path: Path::new("").to_path_buf(),
            source,
        })?;
    let rows = stmt
        .query_map(params![session_id], |row| {
            Ok(ExtractedRef {
                name: row.get(0)?,
                commit_id: row.get(1)?,
            })
        })
        .map_err(|source| SessionError::Database {
            path: Path::new("").to_path_buf(),
            source,
        })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|source| SessionError::Database {
            path: Path::new("").to_path_buf(),
            source,
        })?);
    }
    Ok(out)
}

/// The reachable closure seeds = every ref tip plus the session HEAD.
fn collect_tips(session: &SessionRow, refs: &[ExtractedRef]) -> Vec<String> {
    let mut tips: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for r in refs {
        if let Some(cid) = &r.commit_id {
            if !cid.is_empty() && seen.insert(cid.clone()) {
                tips.push(cid.clone());
            }
        }
    }
    if let Some(cid) = &session.head_commit_id {
        if !cid.is_empty() && seen.insert(cid.clone()) {
            tips.push(cid.clone());
        }
    }
    tips
}

/// Topological walk over all tips. Iterative DFS with Enter/Exit frames
/// and cycle detection — copied from
/// `sqlite_graph/helpers.rs:collect_reachable_commits_topological` (lines
/// 209-252). Parents are visited in `parents_json` array order (first parent
/// / mainline first).
fn collect_reachable_commits_topological(
    conn: &Connection,
    session_id: &str,
    tips: &[String],
) -> Result<Vec<String>> {
    enum Frame {
        Enter(String),
        Exit(String),
    }

    let mut visiting: HashSet<String> = HashSet::new();
    let mut visited: HashSet<String> = HashSet::new();
    let mut ordered: Vec<String> = Vec::new();
    let mut stack: Vec<Frame> = Vec::new();

    // Seed with all tips. Reverse so the first tip is processed first.
    for tip in tips.iter().rev() {
        stack.push(Frame::Enter(tip.clone()));
    }

    while let Some(frame) = stack.pop() {
        match frame {
            Frame::Enter(cid) => {
                if visited.contains(&cid) {
                    continue;
                }
                if !visiting.insert(cid.clone()) {
                    return Err(graph_cycle_error(&cid));
                }
                stack.push(Frame::Exit(cid.clone()));
                let parents = read_parents_json(conn, session_id, &cid)?;
                for parent in parents.into_iter().rev() {
                    if !visited.contains(&parent) {
                        stack.push(Frame::Enter(parent));
                    }
                }
            }
            Frame::Exit(cid) => {
                visiting.remove(&cid);
                if visited.insert(cid.clone()) {
                    ordered.push(cid);
                }
            }
        }
    }
    Ok(ordered)
}

fn graph_cycle_error(commit_id: &str) -> SessionError {
    SessionError::Git(format!("cycle detected in commit graph at {commit_id}"))
}

fn read_parents_json(conn: &Connection, session_id: &str, commit_id: &str) -> Result<Vec<String>> {
    let raw: String = conn
        .query_row(
            "SELECT parents_json FROM graph_commits WHERE commit_id=?1 AND session_id=?2",
            params![commit_id, session_id],
            |row| row.get(0),
        )
        .map_err(|_| SessionError::MissingCommit(commit_id.to_string()))?;
    parse_json_array("parents_json", &raw)
}

fn parse_json_array(column: &str, raw: &str) -> Result<Vec<String>> {
    serde_json::from_str::<Vec<String>>(raw).map_err(|err| {
        SessionError::Database {
            path: Path::new("").to_path_buf(),
            source: rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("malformed JSON in column '{column}': {err}"),
                )),
            ),
        }
    })
}

fn read_commit(conn: &Connection, session_id: &str, commit_id: &str) -> Result<ExtractedCommit> {
    let (
        parents_json,
        message_ids_json,
        agent_name,
        profile_snapshot_hash,
        profile_snapshot_json,
        model,
        provider,
        status,
        commit_message,
        usage_json,
        terminal_reason,
        disposition,
        lifecycle_json,
        created_at,
    ): (
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        String,
        String,
        String,
    ) = conn
        .query_row(
            "SELECT parents_json, message_ids_json, agent_name, profile_snapshot_hash,
                    profile_snapshot_json, model, provider, status, commit_message,
                    usage_json, terminal_reason, disposition, lifecycle_json, created_at
             FROM graph_commits WHERE commit_id=?1 AND session_id=?2",
            params![commit_id, session_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                    row.get(10)?,
                    row.get(11)?,
                    row.get(12)?,
                    row.get(13)?,
                ))
            },
        )
        .map_err(|_| SessionError::MissingCommit(commit_id.to_string()))?;

    let parent_ids = parse_json_array("parents_json", &parents_json)?;
    let message_ids = parse_json_array("message_ids_json", &message_ids_json)?;
    let messages = load_messages(conn, session_id, &message_ids)?;
    let snapshot_lost = read_snapshot_lost(conn, commit_id)?;

    // strict-decode lifecycle to fail closed on malformed data (matches
    // sqlite_graph/commit.rs:decode_lifecycle_json). Propagate the error at the
    // extraction boundary with commit context — `let _ =` previously discarded
    // the Result, defeating the "fail closed" intent. The writer still consumes
    // `lifecycle_json` directly, so the decoded value itself is not threaded.
    decode_lifecycle_json(&lifecycle_json).map_err(|e| {
        SessionError::Git(format!("commit {commit_id}: malformed lifecycle_json: {e}"))
    })?;

    Ok(ExtractedCommit {
        commit_id: commit_id.to_string(),
        parent_ids,
        agent_name,
        profile_snapshot_hash,
        profile_snapshot_json,
        model,
        provider,
        status,
        commit_message,
        usage_json,
        terminal_reason,
        disposition,
        lifecycle_json,
        created_at,
        messages,
        snapshot_lost,
    })
}

/// Load messages by ID, chunked at `SQLITE_BIND_CHUNK_SIZE`, ordered by
/// `ordinal ASC` (matches `sqlite_graph/helpers.rs:load_graph_messages_by_id`
/// plus the `ORDER BY ordinal` from the RESEARCH.md query plan).
fn load_messages(
    conn: &Connection,
    session_id: &str,
    message_ids: &[String],
) -> Result<Vec<GraphMessage>> {
    if message_ids.is_empty() {
        return Ok(Vec::new());
    }

    // Load all rows by ID into a map, then re-order by the message_ids array
    // (which is the persisted ordinal order from message_ids_json).
    let mut by_id: HashMap<String, GraphMessage> = HashMap::with_capacity(message_ids.len());
    for chunk in message_ids.chunks(SQLITE_BIND_CHUNK_SIZE) {
        let placeholders = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT message_id, role, content, reasoning_content, tool_call_id, name, tool_calls_json
             FROM graph_messages WHERE session_id=? AND message_id IN ({placeholders})
             ORDER BY ordinal ASC"
        );
        let mut param_refs: Vec<&dyn rusqlite::types::ToSql> = Vec::with_capacity(chunk.len() + 1);
        param_refs.push(&session_id);
        param_refs.extend(chunk.iter().map(|s| s as &dyn rusqlite::types::ToSql));

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|source| SessionError::Database {
                path: Path::new("").to_path_buf(),
                source,
            })?;
        let rows = stmt
            .query_map(param_refs.as_slice(), |row| {
                let tool_calls_json: String = row.get(6)?;
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    tool_calls_json,
                ))
            })
            .map_err(|source| SessionError::Database {
                path: Path::new("").to_path_buf(),
                source,
            })?;
        for row in rows {
            let (message_id, role, content, reasoning_content, tool_call_id, name, tool_calls_json) =
                row.map_err(|source| SessionError::Database {
                    path: Path::new("").to_path_buf(),
                    source,
                })?;
            let tool_calls: Option<Vec<GraphToolCall>> =
                serde_json::from_str(&tool_calls_json).ok().filter(|v: &Vec<GraphToolCall>| !v.is_empty());
            by_id.insert(
                message_id,
                GraphMessage {
                    role,
                    content,
                    reasoning_content,
                    tool_calls,
                    tool_call_id,
                    name,
                },
            );
        }
    }

    // Re-order by the persisted message_ids order (ordinal order).
    let mut ordered = Vec::with_capacity(message_ids.len());
    for mid in message_ids {
        match by_id.remove(mid) {
            Some(m) => ordered.push(m),
            None => {
                return Err(SessionError::MissingCommit(format!(
                    "message {mid} referenced by commit but not found in graph_messages"
                )));
            }
        }
    }
    Ok(ordered)
}

fn read_snapshot_lost(conn: &Connection, commit_id: &str) -> Result<bool> {
    let lost: Option<i64> = conn
        .query_row(
            "SELECT snapshot_lost FROM graph_snapshot_state WHERE commit_id=?1",
            params![commit_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|source| SessionError::Database {
            path: Path::new("").to_path_buf(),
            source,
        })?;
    Ok(lost.unwrap_or(0) != 0)
}

/// Decode lifecycle JSON, returning `None` for the empty object `{}` (matches
/// `sqlite_graph/commit.rs:decode_lifecycle_json` lines 30-48). Used for strict
/// validation during extraction; the writer persists `lifecycle_json` directly.
pub(crate) fn decode_lifecycle_json(raw: &str) -> Result<Option<TurnLifecycleEvidence>> {
    if raw == "{}" {
        return Ok(None);
    }
    serde_json::from_str(raw).map_err(|err| SessionError::Database {
        path: Path::new("").to_path_buf(),
        source: rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("malformed JSON in lifecycle_json: {err}"),
            )),
        ),
    })
}

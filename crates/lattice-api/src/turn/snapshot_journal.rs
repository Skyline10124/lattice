// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Retain-to-commit write-ahead journal for crash-safe snapshot lifecycle.
//!
//! Records snapshot intent → retained → session-committed transitions in a
//! fsynced JSONL journal at `.lattice/pending-snapshots.jsonl`. Recovery on
//! startup compares journal state against immutable commit lookup and
//! operational snapshot state to converge orphan, phantom, and stale entries.
//!
//! Follows the WAL durability pattern: create/append newline,
//! `sync_all`, parent fsync, tolerate only a torn final line, and durable
//! removal.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const JOURNAL_FILE: &str = "pending-snapshots.jsonl";

/// One durable JSONL journal record.
///
/// Records are append-only. Each entry is identified by the
/// `(session_id, prepared_commit_id)` pair; multiple records for the same
/// entry represent state transitions (Intent → Retained → SessionCommitted).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SnapshotJournalRecord {
    /// Journal header, always the first record.
    Header { journal_id: String },
    /// Intent: snapshot creation is about to begin.
    Intent {
        session_id: String,
        prepared_commit_id: String,
        snapshot_name: String,
    },
    /// Retained: snapshot was successfully created.
    Retained {
        session_id: String,
        prepared_commit_id: String,
        snapshot_name: String,
    },
    /// SessionCommitted: the SQLite session commit succeeded.
    SessionCommitted {
        session_id: String,
        commit_id: String,
        snapshot_name: String,
    },
}

impl SnapshotJournalRecord {
    /// Extract the entry key (session_id, commit_id) for grouping.
    fn entry_key(&self) -> Option<(&str, &str)> {
        match self {
            Self::Header { .. } => None,
            Self::Intent {
                session_id,
                prepared_commit_id,
                ..
            } => Some((session_id.as_str(), prepared_commit_id.as_str())),
            Self::Retained {
                session_id,
                prepared_commit_id,
                ..
            } => Some((session_id.as_str(), prepared_commit_id.as_str())),
            Self::SessionCommitted {
                session_id,
                commit_id,
                ..
            } => Some((session_id.as_str(), commit_id.as_str())),
        }
    }

    fn snapshot_name(&self) -> Option<&str> {
        match self {
            Self::Header { .. } => None,
            Self::Intent { snapshot_name, .. }
            | Self::Retained { snapshot_name, .. }
            | Self::SessionCommitted { snapshot_name, .. } => Some(snapshot_name.as_str()),
        }
    }
}

/// Parsed on-disk journal state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedJournal {
    pub path: PathBuf,
    pub records: Vec<SnapshotJournalRecord>,
    /// Whether the last line was incomplete (crash mid-write).
    pub torn_final_line: bool,
}

impl ParsedJournal {
    /// Group records by entry key and compute the latest state for each.
    pub fn entry_states(&self) -> Vec<JournalEntryState> {
        let mut entries: Vec<JournalEntryState> = Vec::new();
        for record in &self.records {
            if let Some(key) = record.entry_key() {
                let snapshot_name = record.snapshot_name().unwrap_or("").to_string();
                let state = match record {
                    SnapshotJournalRecord::Intent { .. } => SnapshotJournalState::Intent,
                    SnapshotJournalRecord::Retained { .. } => SnapshotJournalState::Retained,
                    SnapshotJournalRecord::SessionCommitted { .. } => {
                        SnapshotJournalState::SessionCommitted
                    }
                    _ => continue,
                };

                if let Some(existing) = entries
                    .iter_mut()
                    .find(|e| e.session_id == key.0 && e.commit_id == key.1)
                {
                    // Update to the latest state
                    existing.state = state;
                } else {
                    entries.push(JournalEntryState {
                        session_id: key.0.to_string(),
                        commit_id: key.1.to_string(),
                        snapshot_name,
                        state,
                    });
                }
            }
        }
        entries
    }
}

/// Logical state of one journal entry (latest transition).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalEntryState {
    pub session_id: String,
    pub commit_id: String,
    pub snapshot_name: String,
    pub state: SnapshotJournalState,
}

/// Latest recorded state for a journal entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotJournalState {
    Intent,
    Retained,
    SessionCommitted,
}

/// Test-only crash injection points.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultPoint {
    AfterIntent,
    AfterRetained,
    AfterSessionCommitted,
    BeforeJournalRemove,
}

/// Append-only JSONL journal for snapshot lifecycle durability.
///
/// Lives at `<project_root>/.lattice/pending-snapshots.jsonl`.
pub struct SnapshotJournal {
    path: PathBuf,
    file: File,
}

impl SnapshotJournal {
    /// The canonical journal path within the given `.lattice` directory.
    pub fn journal_path(lattice_dir: &Path) -> PathBuf {
        lattice_dir.join(JOURNAL_FILE)
    }

    /// Whether a journal file exists in the given directory.
    pub fn exists(lattice_dir: &Path) -> bool {
        Self::journal_path(lattice_dir).exists()
    }

    /// Create a new journal with a header record.
    pub fn create_new(lattice_dir: &Path) -> Result<Self, String> {
        fs::create_dir_all(lattice_dir).map_err(|e| format!("create journal dir: {e}"))?;
        let path = Self::journal_path(lattice_dir);
        if path.exists() {
            return Err("journal already exists".to_string());
        }
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| format!("create journal file: {e}"))?;
        let mut journal = Self { path, file };
        journal.append_record(SnapshotJournalRecord::Header {
            journal_id: "pending-snapshots".to_string(),
        })?;
        Ok(journal)
    }

    /// Open an existing journal for appending.
    pub fn open_existing(lattice_dir: &Path) -> Result<Self, String> {
        let path = Self::journal_path(lattice_dir);
        let file = OpenOptions::new()
            .append(true)
            .open(&path)
            .map_err(|e| format!("open journal: {e}"))?;
        Ok(Self { path, file })
    }

    /// Append a record, fsync the file and parent directory.
    pub fn append_record(&mut self, record: SnapshotJournalRecord) -> Result<(), String> {
        let line = serde_json::to_string(&record).map_err(|e| format!("serialize record: {e}"))?;
        self.file
            .write_all(line.as_bytes())
            .map_err(|e| format!("write record: {e}"))?;
        self.file
            .write_all(b"\n")
            .map_err(|e| format!("write newline: {e}"))?;
        self.file
            .sync_all()
            .map_err(|e| format!("fsync journal: {e}"))?;
        fsync_parent(&self.path)?;
        Ok(())
    }

    /// Durably remove the journal file (close handle, delete, fsync parent).
    pub fn remove_durable(&mut self) -> Result<(), String> {
        let path = self.path.clone();
        // remove the file BEFORE releasing the write handle. On Unix,
        // `remove_file` unlinks the directory entry while the open write handle
        // keeps the inode alive; if removal fails, the write handle stays usable
        // for `append_record`. Replacing the handle with a read-only one first
        // (the previous order) left the journal unable to append on a remove
        // failure — a corruption bug in the crash-recovery path. The stale write
        // handle is dropped when the `SnapshotJournal` itself is dropped.
        fs::remove_file(&path).map_err(|e| format!("remove journal: {e}"))?;
        fsync_parent(&path)?;
        Ok(())
    }

    /// Return the journal file path.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Parse a journal file, tolerating only a torn final JSONL line.
pub fn parse_journal(path: &Path) -> Result<ParsedJournal, String> {
    let data = fs::read_to_string(path).map_err(|e| format!("read journal: {e}"))?;
    if data.is_empty() {
        return Err("empty journal".to_string());
    }

    let ends_with_newline = data.ends_with('\n');
    let mut lines: Vec<&str> = data.split('\n').collect();
    let torn_final_line = if ends_with_newline {
        false
    } else {
        lines.last().is_some_and(|line| !line.is_empty())
    };
    if torn_final_line {
        lines.pop();
    }
    let complete_lines: Vec<String> = lines
        .into_iter()
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect();

    if complete_lines.is_empty() {
        return Err("empty journal (only torn lines)".to_string());
    }

    let mut records = Vec::new();
    for line in &complete_lines {
        let record: SnapshotJournalRecord =
            serde_json::from_str(line).map_err(|e| format!("invalid journal record: {e}"))?;
        records.push(record);
    }

    // First record must be Header.
    match records.first() {
        Some(SnapshotJournalRecord::Header { .. }) => {}
        _ => return Err("first record must be Header".to_string()),
    }

    Ok(ParsedJournal {
        path: path.to_path_buf(),
        records,
        torn_final_line,
    })
}

/// Reconciler trait for snapshot/commit existence checks during recovery.
///
/// Abstracts over the workspace adapter and session store so that
/// reconciliation logic is testable without full infrastructure.
pub trait SnapshotReconciler {
    /// Check if a retained snapshot exists by name.
    fn snapshot_exists(&self, name: &str) -> Result<bool, String>;
    /// Release (delete) a retained snapshot.
    fn release_snapshot(&mut self, name: &str) -> Result<(), String>;
    /// Check if an immutable session commit exists.
    fn commit_exists(&self, session_id: &str, commit_id: &str) -> Result<bool, String>;
    /// Mark a commit's snapshot as operationally lost.
    fn set_snapshot_lost(&mut self, commit_id: &str) -> Result<(), String>;
}

/// Result of reconciling pending journal entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotRecoveryResult {
    /// Snapshot names that were released because no commit exists.
    pub orphans_released: Vec<String>,
    /// Commit IDs whose snapshots are missing (marked lost).
    pub phantoms_marked_lost: Vec<String>,
    /// Snapshot names whose journal entries were cleared (fully completed).
    pub stale_cleared: Vec<String>,
}

/// Reconcile pending journal entries against operational state.
///
/// For each journal entry (grouped by session_id + commit_id):
/// - **Intent/Retained, no commit:** release orphan snapshot.
/// - **SessionCommitted, snapshot exists:** clear stale journal.
/// - **SessionCommitted, no snapshot:** mark `snapshot_lost`.
///
/// Removes the journal file after processing all entries.
/// Never rewrites a committed lifecycle row.
pub fn reconcile_pending_snapshots(
    lattice_dir: &Path,
    reconciler: &mut dyn SnapshotReconciler,
) -> Result<SnapshotRecoveryResult, String> {
    let path = SnapshotJournal::journal_path(lattice_dir);
    if !path.exists() {
        return Ok(SnapshotRecoveryResult {
            orphans_released: vec![],
            phantoms_marked_lost: vec![],
            stale_cleared: vec![],
        });
    }

    let parsed = parse_journal(&path)?;
    let states = parsed.entry_states();

    let mut result = SnapshotRecoveryResult {
        orphans_released: vec![],
        phantoms_marked_lost: vec![],
        stale_cleared: vec![],
    };

    for entry in &states {
        let snap_exists = reconciler.snapshot_exists(&entry.snapshot_name)?;
        let commit_exists = reconciler.commit_exists(&entry.session_id, &entry.commit_id)?;

        match entry.state {
            SnapshotJournalState::Intent | SnapshotJournalState::Retained => {
                if !commit_exists {
                    // No commit → release orphan snapshot if it exists.
                    if snap_exists {
                        reconciler.release_snapshot(&entry.snapshot_name)?;
                        result.orphans_released.push(entry.snapshot_name.clone());
                    }
                }
                // If commit exists but journal lacks SessionCommitted,
                // the journal is stale — will be cleaned below.
            }
            SnapshotJournalState::SessionCommitted => {
                if snap_exists {
                    // Fully completed — clear stale journal entry.
                    result.stale_cleared.push(entry.snapshot_name.clone());
                } else {
                    // Commit exists but snapshot is missing → mark lost.
                    reconciler.set_snapshot_lost(&entry.commit_id)?;
                    result.phantoms_marked_lost.push(entry.commit_id.clone());
                }
            }
        }
    }

    // Remove the journal after reconciliation.
    if !states.is_empty() {
        let mut journal = SnapshotJournal::open_existing(lattice_dir)?;
        journal.remove_durable()?;
    }

    Ok(result)
}

fn fsync_parent(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        let dir = File::open(parent).map_err(|e| format!("open parent dir: {e}"))?;
        dir.sync_all()
            .map_err(|e| format!("fsync parent dir: {e}"))?;
    }
    Ok(())
}

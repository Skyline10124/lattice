// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! JSONL promotion journal, crash recovery, and fault injection hooks.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use git2::Repository;
use serde::{Deserialize, Serialize};

use super::PromotionError;
use super::materialize::{
    DurableFs, MaterializationPlan, MaterializeEntry, PathClassification, classify_path,
};

const JOURNAL_FILE: &str = "materialization.jsonl";

/// Recovery direction derived from the committed project ref.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryDirection {
    TowardCandidate,
    TowardBaseline,
    RefConflict,
}

/// Result of replaying or inspecting a promotion journal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryOutcome {
    Complete,
    RecoveryRequired,
    RecoveryConflict { detail: String },
}

/// Test-only crash injection points.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultPoint {
    AfterHeaderSync,
    AfterRefsCommitted,
    AfterIntent { index: usize },
    AfterEffect { index: usize },
    AfterComplete { index: usize },
    AfterFinished,
    BeforeJournalRemove,
}

/// Header persisted before any ref movement or Lower effects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalHeader {
    pub journal_id: String,
    pub project_ref: String,
    pub baseline_oid: String,
    pub candidate_oid: String,
    pub path_plan: MaterializationPlan,
}

/// One durable JSONL journal record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JournalRecord {
    Header(JournalHeader),
    RefsCommitted {
        project_oid: String,
        session_oid: String,
        candidate_ref_oid: String,
    },
    Intent {
        index: usize,
        entry: MaterializeEntry,
    },
    Complete {
        index: usize,
    },
    Finished,
}

/// Parsed on-disk journal state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedJournal {
    pub path: PathBuf,
    pub header: JournalHeader,
    pub records: Vec<JournalRecord>,
    pub torn_final_line: bool,
}

/// Append-only JSONL promotion journal under the common Git directory.
pub struct PromotionJournal {
    path: PathBuf,
    file: File,
    next_index: usize,
}

impl PromotionJournal {
    pub fn path_in(state_dir: &Path) -> PathBuf {
        state_dir.join(JOURNAL_FILE)
    }

    pub fn exists(state_dir: &Path) -> bool {
        Self::path_in(state_dir).exists()
    }

    pub fn create_new(state_dir: &Path, header: JournalHeader) -> Result<Self, PromotionError> {
        fs::create_dir_all(state_dir)?;
        let path = Self::path_in(state_dir);
        if path.exists() {
            return Err(PromotionError::JournalExists);
        }
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        let mut journal = Self {
            path,
            file,
            next_index: 0,
        };
        journal.append_record(JournalRecord::Header(header))?;
        Ok(journal)
    }

    pub fn open_existing(state_dir: &Path) -> Result<Self, PromotionError> {
        let path = Self::path_in(state_dir);
        let parsed = parse_journal(&path)?;
        let next_index = parsed
            .records
            .iter()
            .filter_map(|record| match record {
                JournalRecord::Complete { index } => Some(*index + 1),
                _ => None,
            })
            .max()
            .unwrap_or(0);
        let file = OpenOptions::new().append(true).open(&path)?;
        Ok(Self {
            path,
            file,
            next_index,
        })
    }

    pub fn append_record(&mut self, record: JournalRecord) -> Result<(), PromotionError> {
        let line =
            serde_json::to_string(&record).map_err(|e| PromotionError::JournalMalformed {
                detail: e.to_string(),
            })?;
        self.file.write_all(line.as_bytes())?;
        self.file.write_all(b"\n")?;
        self.file.sync_all()?;
        fsync_parent(&self.path)?;
        Ok(())
    }

    pub fn record_refs_committed(
        &mut self,
        project_oid: &str,
        session_oid: &str,
        candidate_ref_oid: &str,
    ) -> Result<(), PromotionError> {
        self.append_record(JournalRecord::RefsCommitted {
            project_oid: project_oid.to_owned(),
            session_oid: session_oid.to_owned(),
            candidate_ref_oid: candidate_ref_oid.to_owned(),
        })
    }

    pub fn record_intent(&mut self, entry: MaterializeEntry) -> Result<usize, PromotionError> {
        let index = self.next_index;
        self.append_record(JournalRecord::Intent { index, entry })?;
        Ok(index)
    }

    pub fn record_complete(&mut self, index: usize) -> Result<(), PromotionError> {
        self.append_record(JournalRecord::Complete { index })?;
        self.next_index = index + 1;
        Ok(())
    }

    pub fn finish(&mut self) -> Result<(), PromotionError> {
        self.append_record(JournalRecord::Finished)
    }

    pub fn remove_durable(&mut self) -> Result<(), PromotionError> {
        let path = self.path.clone();
        drop(std::mem::replace(
            &mut self.file,
            OpenOptions::new().read(true).open(&path)?,
        ));
        fs::remove_file(&path)?;
        fsync_parent(&path)?;
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

pub fn parse_journal(path: &Path) -> Result<ParsedJournal, PromotionError> {
    let data = fs::read_to_string(path)?;
    if data.is_empty() {
        return Err(PromotionError::JournalMalformed {
            detail: "empty journal".into(),
        });
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
        return Err(PromotionError::JournalMalformed {
            detail: "empty journal".into(),
        });
    }

    let mut records = Vec::new();
    for line in &complete_lines {
        let record: JournalRecord =
            serde_json::from_str(line).map_err(|e| PromotionError::JournalMalformed {
                detail: format!("invalid jsonl record: {e}"),
            })?;
        records.push(record);
    }

    let header = match records.first() {
        Some(JournalRecord::Header(header)) => header.clone(),
        _ => {
            return Err(PromotionError::JournalMalformed {
                detail: "first record must be header".into(),
            });
        }
    };

    Ok(ParsedJournal {
        path: path.to_path_buf(),
        header,
        records,
        torn_final_line,
    })
}

pub fn resolve_recovery_direction(
    project_ref_oid: &str,
    header: &JournalHeader,
) -> RecoveryDirection {
    if project_ref_oid == header.candidate_oid {
        RecoveryDirection::TowardCandidate
    } else if project_ref_oid == header.baseline_oid {
        RecoveryDirection::TowardBaseline
    } else {
        RecoveryDirection::RefConflict
    }
}

pub fn recover_journal(
    repository_path: &Path,
    worktree_root: &Path,
    state_dir: &Path,
    project_ref_oid: &str,
) -> Result<RecoveryOutcome, PromotionError> {
    let path = PromotionJournal::path_in(state_dir);
    if !path.exists() {
        return Ok(RecoveryOutcome::Complete);
    }
    let parsed = parse_journal(&path)?;
    let direction = resolve_recovery_direction(project_ref_oid, &parsed.header);
    if direction == RecoveryDirection::RefConflict {
        return Ok(RecoveryOutcome::RecoveryConflict {
            detail: format!(
                "project ref oid {project_ref_oid} matches neither baseline {} nor candidate {}",
                parsed.header.baseline_oid, parsed.header.candidate_oid
            ),
        });
    }

    let repo = Repository::open(repository_path).map_err(|e| PromotionError::Git(e.to_string()))?;
    let mut fs = DurableFs::new(worktree_root, &repo, None);

    for entry in parsed.header.path_plan.entries() {
        let class = classify_path(worktree_root, &repo, &entry.path, &entry.old, &entry.new)?;
        if class == PathClassification::ThirdState {
            return Ok(RecoveryOutcome::RecoveryConflict {
                detail: format!("third-state path {}", entry.path),
            });
        }
        let at_target = match direction {
            RecoveryDirection::TowardCandidate => class == PathClassification::MatchesNew,
            RecoveryDirection::TowardBaseline => class == PathClassification::MatchesOld,
            RecoveryDirection::RefConflict => unreachable!(),
        };
        if at_target {
            continue;
        }
        let transition = match direction {
            RecoveryDirection::TowardCandidate => entry.clone(),
            RecoveryDirection::TowardBaseline => MaterializeEntry {
                path: entry.path.clone(),
                old: entry.new.clone(),
                new: entry.old.clone(),
            },
            RecoveryDirection::RefConflict => unreachable!(),
        };
        fs.apply_entry(&transition)?;
    }

    for entry in parsed.header.path_plan.entries() {
        let class = classify_path(worktree_root, &repo, &entry.path, &entry.old, &entry.new)?;
        if class == PathClassification::ThirdState {
            return Ok(RecoveryOutcome::RecoveryConflict {
                detail: format!("third-state path {}", entry.path),
            });
        }
        let at_target = match direction {
            RecoveryDirection::TowardCandidate => class == PathClassification::MatchesNew,
            RecoveryDirection::TowardBaseline => class == PathClassification::MatchesOld,
            RecoveryDirection::RefConflict => unreachable!(),
        };
        if !at_target {
            return Ok(RecoveryOutcome::RecoveryRequired);
        }
    }

    let mut journal = PromotionJournal::open_existing(state_dir)?;
    journal.remove_durable()?;
    Ok(RecoveryOutcome::Complete)
}

#[allow(clippy::too_many_arguments)]
pub fn run_materialization(
    repository_path: &Path,
    worktree_root: &Path,
    state_dir: &Path,
    header: JournalHeader,
    project_ref_oid: &str,
    session_oid: &str,
    candidate_ref_oid: &str,
    fault: Option<FaultPoint>,
) -> Result<RecoveryOutcome, PromotionError> {
    let mut journal = PromotionJournal::create_new(state_dir, header.clone())?;
    maybe_fault(fault, FaultPoint::AfterHeaderSync)?;

    journal.record_refs_committed(project_ref_oid, session_oid, candidate_ref_oid)?;
    maybe_fault(fault, FaultPoint::AfterRefsCommitted)?;

    let repo = Repository::open(repository_path).map_err(|e| PromotionError::Git(e.to_string()))?;
    let mut fs = DurableFs::new(worktree_root, &repo, None);
    for entry in header.path_plan.entries() {
        let index = journal.record_intent(entry.clone())?;
        maybe_fault(fault, FaultPoint::AfterIntent { index })?;
        fs.apply_entry(entry)?;
        maybe_fault(fault, FaultPoint::AfterEffect { index })?;
        journal.record_complete(index)?;
        maybe_fault(fault, FaultPoint::AfterComplete { index })?;
    }

    journal.finish()?;
    maybe_fault(fault, FaultPoint::AfterFinished)?;
    maybe_fault(fault, FaultPoint::BeforeJournalRemove)?;
    journal.remove_durable()?;
    Ok(RecoveryOutcome::Complete)
}

fn maybe_fault(active: Option<FaultPoint>, point: FaultPoint) -> Result<(), PromotionError> {
    if active == Some(point) {
        return Err(PromotionError::SimulatedCrash);
    }
    Ok(())
}

fn fsync_parent(path: &Path) -> Result<(), PromotionError> {
    if let Some(parent) = path.parent() {
        let dir = File::open(parent)?;
        dir.sync_all()?;
    }
    Ok(())
}

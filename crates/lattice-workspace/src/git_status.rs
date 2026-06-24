// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Strict porcelain-v1 `-z` parser for structured git status facts.
//!
//! Parses the NUL-delimited output of
//! `git status --porcelain=v1 -z --untracked-files=all --ignore-submodules=none`
//! into typed [`GitStatusEntry`] records.
//!
//! # Safety
//!
//! - Rejects non-UTF-8 model-visible paths (fail-closed).
//! - Never uses `String::from_utf8_lossy`, `.lines()`, or whitespace splitting.
//! - Preserves embedded whitespace and newlines in paths exactly.
//! - Carries the second path for rename/copy records.

use crate::WorkspacePath;

/// Two-column porcelain-v1 status code for a single entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitStatusCode {
    /// ` ` — no change in this column.
    Unmodified,
    /// `M` — modified content.
    Modified,
    /// `A` — added to index or worktree.
    Added,
    /// `D` — deleted from index or worktree.
    Deleted,
    /// `R` — renamed (index only; second path is the source).
    Renamed,
    /// `C` — copied (index only; second path is the source).
    Copied,
    /// `?` — untracked file (both columns).
    Untracked,
    /// `!` — ignored file (both columns).
    Ignored,
}

impl GitStatusCode {
    /// Parse a single porcelain-v1 status byte.
    ///
    /// Returns `None` for invalid bytes.
    pub fn from_byte(b: u8) -> Option<Self> {
        match b {
            b' ' => Some(Self::Unmodified),
            b'M' => Some(Self::Modified),
            b'A' => Some(Self::Added),
            b'D' => Some(Self::Deleted),
            b'R' => Some(Self::Renamed),
            b'C' => Some(Self::Copied),
            b'?' => Some(Self::Untracked),
            b'!' => Some(Self::Ignored),
            _ => None,
        }
    }

    /// The single-character porcelain representation.
    pub fn as_char(self) -> char {
        match self {
            Self::Unmodified => ' ',
            Self::Modified => 'M',
            Self::Added => 'A',
            Self::Deleted => 'D',
            Self::Renamed => 'R',
            Self::Copied => 'C',
            Self::Untracked => '?',
            Self::Ignored => '!',
        }
    }
}

/// One typed status entry from porcelain-v1 `-z` output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitStatusEntry {
    /// Index (staged) status — the first XY column.
    pub index_status: GitStatusCode,
    /// Worktree (unstaged) status — the second XY column.
    pub worktree_status: GitStatusCode,
    /// Current path (destination for rename/copy, sole path otherwise).
    pub path: String,
    /// Source path for rename (`R`) or copy (`C`) entries.
    pub source_path: Option<String>,
}

impl GitStatusEntry {
    /// Format a structured JSON workspace-dirty fact from entries.
    ///
    /// Uses JSON serialization/escaping so hostile filenames (quotes, newlines,
    /// braces) cannot inject another record into the fact.
    pub fn workspace_dirty_fact(entries: &[GitStatusEntry]) -> String {
        let entry_values: Vec<serde_json::Value> = entries
            .iter()
            .map(|e| {
                let mut obj = serde_json::Map::new();
                obj.insert(
                    "xy".to_string(),
                    serde_json::Value::String(format!(
                        "{}{}",
                        e.index_status.as_char(),
                        e.worktree_status.as_char()
                    )),
                );
                obj.insert(
                    "path".to_string(),
                    serde_json::Value::String(e.path.clone()),
                );
                if let Some(ref src) = e.source_path {
                    obj.insert(
                        "source_path".to_string(),
                        serde_json::Value::String(src.clone()),
                    );
                }
                serde_json::Value::Object(obj)
            })
            .collect();
        let fact = serde_json::json!({
            "type": "workspace_dirty",
            "entries": entry_values,
        });
        serde_json::to_string(&fact).expect("JSON serialization must not fail")
    }
}

/// Result of parsing porcelain-v1 `-z` output.
#[derive(Clone, Debug)]
pub struct GitStatus {
    /// Parsed entries; empty when the working tree is clean.
    pub entries: Vec<GitStatusEntry>,
    /// `true` when `entries` is empty.
    pub is_clean: bool,
}

impl Default for GitStatus {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            is_clean: true,
        }
    }
}

/// Error returned when porcelain-v1 `-z` output cannot be parsed.
#[derive(Debug, thiserror::Error)]
pub enum StatusParseError {
    #[error("truncated porcelain-v1 -z record at byte offset {byte_offset}")]
    TruncatedRecord { byte_offset: usize },
    #[error("non-UTF-8 model-visible path in porcelain-v1 -z at record {record_index}")]
    NonUtf8Path { record_index: usize },
    #[error("invalid path in porcelain-v1 -z at record {record_index}: {reason}")]
    InvalidPath { record_index: usize, reason: String },
    #[error("invalid status code byte 0x{byte:02x} at record {record_index}")]
    InvalidStatusCode { record_index: usize, byte: u8 },
}

/// Parse NUL-delimited porcelain-v1 output from
/// `git status --porcelain=v1 -z --untracked-files=all --ignore-submodules=none`.
///
/// # Format
///
/// Each record has the structure `XY SP path NUL`. For rename (`R`) and copy
/// (`C`) entries, a second path follows: `XY SP source NUL destination NUL`
/// where XY has `R` or `C` in the index column.
///
/// - Empty input produces a clean status (no entries).
/// - Ordinary XY entries consume one NUL-terminated path.
/// - Rename (`R`) and copy (`C`) entries consume two NUL-terminated paths
///   (source then destination) per porcelain-v1 -z specification.
/// - Truncated, absolute, traversal, or non-UTF-8 model-visible paths return
///   a typed error.
///
/// # Safety
///
/// Never uses `String::from_utf8_lossy`, `.lines()`, or whitespace splitting.
pub fn parse_git_status_z(raw: &[u8]) -> std::result::Result<GitStatus, StatusParseError> {
    if raw.is_empty() {
        return Ok(GitStatus {
            entries: Vec::new(),
            is_clean: true,
        });
    }

    let mut entries = Vec::new();
    let mut pos = 0;
    let mut record_index: usize = 0;

    while pos < raw.len() {
        let record_start = pos;

        // Need at least 4 bytes remaining: X Y SP path_byte NUL
        // (pos+3 is the minimum index of the NUL terminator)
        if pos + 3 > raw.len() {
            return Err(StatusParseError::TruncatedRecord {
                byte_offset: record_start,
            });
        }

        // Parse X (index) status byte
        let x_byte = raw[pos];
        let index_status = parse_status_byte(x_byte, record_index)?;
        pos += 1;

        // Parse Y (worktree) status byte
        let y_byte = raw[pos];
        let worktree_status = parse_status_byte(y_byte, record_index)?;
        pos += 1;

        // Expect SP separator
        if raw[pos] != b' ' {
            return Err(StatusParseError::TruncatedRecord {
                byte_offset: record_start,
            });
        }
        pos += 1;

        // Find NUL-terminated path
        let path_start = pos;
        let nul_pos = match find_nul(raw, pos) {
            Some(p) => p,
            None => {
                return Err(StatusParseError::TruncatedRecord {
                    byte_offset: record_start,
                });
            }
        };

        // Decode path as strict UTF-8 (fail closed)
        let path_bytes = &raw[path_start..nul_pos];
        let path = std::str::from_utf8(path_bytes)
            .map_err(|_| StatusParseError::NonUtf8Path { record_index })?;

        // Validate path through WorkspacePath
        validate_status_path(path, record_index)?;

        pos = nul_pos + 1; // advance past NUL

        // For rename/copy entries in the index column, consume the second NUL path
        let is_rename_copy = matches!(index_status, GitStatusCode::Renamed | GitStatusCode::Copied);
        let (source_path, dest_path) = if is_rename_copy {
            // In porcelain-v1 -z format, for rename/copy entries:
            //   XY SP source_path NUL dest_path NUL
            // The path we already read is the source; the next NUL path is the destination.
            if pos >= raw.len() {
                return Err(StatusParseError::TruncatedRecord {
                    byte_offset: record_start,
                });
            }
            let dest_start = pos;
            let dest_nul = match find_nul(raw, pos) {
                Some(p) => p,
                None => {
                    return Err(StatusParseError::TruncatedRecord {
                        byte_offset: record_start,
                    });
                }
            };
            let dest_bytes = &raw[dest_start..dest_nul];
            let dest = std::str::from_utf8(dest_bytes)
                .map_err(|_| StatusParseError::NonUtf8Path { record_index })?;
            validate_status_path(dest, record_index)?;
            pos = dest_nul + 1;
            (Some(path.to_string()), dest.to_string())
        } else {
            (None, path.to_string())
        };

        entries.push(GitStatusEntry {
            index_status,
            worktree_status,
            path: dest_path,
            source_path,
        });

        record_index += 1;
    }

    Ok(GitStatus {
        is_clean: entries.is_empty(),
        entries,
    })
}

/// Find the next NUL byte starting from `offset`.
fn find_nul(data: &[u8], offset: usize) -> Option<usize> {
    data[offset..]
        .iter()
        .position(|&b| b == 0)
        .map(|p| p + offset)
}

/// Validate that a path from a porcelain record is acceptable.
///
/// Rejects empty paths, absolute paths, and parent-traversal paths.
/// Each path is also normalized through [`WorkspacePath::parse`].
fn validate_status_path(
    path: &str,
    record_index: usize,
) -> std::result::Result<WorkspacePath, StatusParseError> {
    WorkspacePath::parse(path).map_err(|e| StatusParseError::InvalidPath {
        record_index,
        reason: e.to_string(),
    })
}

/// Parse a single status byte, returning a typed error if invalid.
fn parse_status_byte(
    b: u8,
    record_index: usize,
) -> std::result::Result<GitStatusCode, StatusParseError> {
    GitStatusCode::from_byte(b).ok_or(StatusParseError::InvalidStatusCode {
        record_index,
        byte: b,
    })
}

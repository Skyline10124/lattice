// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use super::commit_baseline::{CommitBaselineReport, HunkAnchorEvidence};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatchDocument {
    pub operations: Vec<PatchOperation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
#[allow(clippy::enum_variant_names)]
pub enum PatchOperation {
    UpdateFile {
        path: String,
        old_hash: String,
        hunks: Vec<PatchHunk>,
    },
    DeleteFile {
        path: String,
        old_hash: String,
    },
}

impl PatchOperation {
    pub fn path(&self) -> &str {
        match self {
            PatchOperation::UpdateFile { path, .. } | PatchOperation::DeleteFile { path, .. } => {
                path
            }
        }
    }

    pub fn kind(&self) -> FileOperationKind {
        match self {
            PatchOperation::UpdateFile { .. } => FileOperationKind::Update,
            PatchOperation::DeleteFile { .. } => FileOperationKind::Delete,
        }
    }

    pub fn old_hash(&self) -> Option<&str> {
        match self {
            PatchOperation::UpdateFile { old_hash, .. }
            | PatchOperation::DeleteFile { old_hash, .. } => Some(old_hash),
        }
    }

    pub fn hunk_count(&self) -> usize {
        match self {
            PatchOperation::UpdateFile { hunks, .. } => hunks.len(),
            PatchOperation::DeleteFile { .. } => 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatchHunk {
    pub index: usize,
    pub lines: Vec<PatchHunkLine>,
}

impl PatchHunk {
    #[cfg(test)]
    pub fn context_lines(&self) -> Vec<&str> {
        self.lines
            .iter()
            .filter_map(|line| match line {
                PatchHunkLine::Context(value) => Some(value.text.as_str()),
                PatchHunkLine::Remove(_) | PatchHunkLine::Add(_) => None,
            })
            .collect()
    }

    #[cfg(test)]
    pub fn removed_lines(&self) -> Vec<&str> {
        self.lines
            .iter()
            .filter_map(|line| match line {
                PatchHunkLine::Remove(value) => Some(value.text.as_str()),
                PatchHunkLine::Context(_) | PatchHunkLine::Add(_) => None,
            })
            .collect()
    }

    #[cfg(test)]
    pub fn inserted_lines(&self) -> Vec<&str> {
        self.lines
            .iter()
            .filter_map(|line| match line {
                PatchHunkLine::Add(value) => Some(value.as_str()),
                PatchHunkLine::Context(_) | PatchHunkLine::Remove(_) => None,
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "text", rename_all = "snake_case")]
pub enum PatchHunkLine {
    Context(HashLine),
    Remove(HashLine),
    Add(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HashLine {
    pub hash: String,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileOperationKind {
    Update,
    Delete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PatchErrorCode {
    ParseError,
    BoundaryDenied,
    MissingFile,
    DuplicatePath,
    HashMismatch,
    HunkMissing,
    HunkAmbiguous,
    IoError,
    RollbackFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatchDiagnostic {
    pub code: PatchErrorCode,
    pub message: String,
    pub path: Option<String>,
    pub operation_index: Option<usize>,
    pub hunk_index: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptedOperation {
    pub index: usize,
    pub path: String,
    pub operation: FileOperationKind,
    pub old_hash: Option<String>,
    pub hunk_count: usize,
}

impl AttemptedOperation {
    pub fn from_operation(index: usize, operation: &PatchOperation) -> Self {
        Self {
            index,
            path: operation.path().to_string(),
            operation: operation.kind(),
            old_hash: operation.old_hash().map(ToOwned::to_owned),
            hunk_count: operation.hunk_count(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangedFile {
    pub path: String,
    pub operation: FileOperationKind,
    pub bytes_before: usize,
    pub bytes_after: usize,
    pub lines_before: usize,
    pub lines_after: usize,
    pub hash_before: Option<String>,
    pub hash_after: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub line_changes: Vec<LineChange>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LineChangeKind {
    Added,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineChange {
    pub kind: LineChangeKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_hash: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyPlan {
    pub changes: Vec<PlannedFileChange>,
    pub attempted_operations: Vec<AttemptedOperation>,
    pub(crate) commit_baseline_evidence: Vec<HunkAnchorEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedFileChange {
    pub path: String,
    pub target_path: PathBuf,
    pub operation_index: usize,
    pub operation: FileOperationKind,
    pub bytes_before: Option<Vec<u8>>,
    pub bytes_after: Option<Vec<u8>>,
    pub manifest: ChangedFile,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatchResultEnvelope {
    pub ok: bool,
    pub summary: String,
    pub errors: Vec<PatchDiagnostic>,
    pub changed_files: Vec<ChangedFile>,
    pub attempted_operations: Vec<AttemptedOperation>,
    #[serde(skip)]
    pub(crate) commit_baseline_report: Option<CommitBaselineReport>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hashline(text: &str) -> HashLine {
        HashLine {
            hash: "blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            text: text.into(),
        }
    }

    #[test]
    fn serializes_every_required_patch_error_code() {
        let cases = [
            (PatchErrorCode::ParseError, "parse_error"),
            (PatchErrorCode::BoundaryDenied, "boundary_denied"),
            (PatchErrorCode::MissingFile, "missing_file"),
            (PatchErrorCode::DuplicatePath, "duplicate_path"),
            (PatchErrorCode::HashMismatch, "hash_mismatch"),
            (PatchErrorCode::HunkMissing, "hunk_missing"),
            (PatchErrorCode::HunkAmbiguous, "hunk_ambiguous"),
            (PatchErrorCode::IoError, "io_error"),
            (PatchErrorCode::RollbackFailed, "rollback_failed"),
        ];

        for (code, expected) in cases {
            let value = serde_json::to_value(code).unwrap();
            assert_eq!(value, serde_json::json!(expected));
        }
    }

    #[test]
    fn serializes_patch_diagnostic_coordinates() {
        let diagnostic = PatchDiagnostic {
            code: PatchErrorCode::HunkMissing,
            message: "context not found".into(),
            path: Some("src/lib.rs".into()),
            operation_index: Some(2),
            hunk_index: Some(1),
        };

        let value = serde_json::to_value(diagnostic).unwrap();

        assert_eq!(value["code"], "hunk_missing");
        assert_eq!(value["message"], "context not found");
        assert_eq!(value["path"], "src/lib.rs");
        assert_eq!(value["operation_index"], 2);
        assert_eq!(value["hunk_index"], 1);
    }

    #[test]
    fn serializes_patch_result_envelope_and_changed_file_manifest() {
        let operation = PatchOperation::UpdateFile {
            path: "src/lib.rs".into(),
            old_hash: "blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                .into(),
            hunks: vec![PatchHunk {
                index: 0,
                lines: vec![
                    PatchHunkLine::Context(hashline("fn main() {")),
                    PatchHunkLine::Remove(hashline("    old();")),
                    PatchHunkLine::Add("    new();".into()),
                    PatchHunkLine::Context(hashline("}")),
                ],
            }],
        };

        let envelope = PatchResultEnvelope {
            ok: true,
            summary: "patched 1 file".into(),
            errors: Vec::new(),
            changed_files: vec![ChangedFile {
                path: "src/lib.rs".into(),
                operation: FileOperationKind::Update,
                bytes_before: 12,
                bytes_after: 13,
                lines_before: 2,
                lines_after: 2,
                hash_before: Some(
                    "blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                        .into(),
                ),
                hash_after: Some(
                    "blake3:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                        .into(),
                ),
                line_changes: vec![],
            }],
            attempted_operations: vec![AttemptedOperation::from_operation(0, &operation)],
            commit_baseline_report: None,
        };

        let value = serde_json::to_value(envelope).unwrap();

        assert_eq!(value["ok"], true);
        assert_eq!(value["changed_files"][0]["operation"], "update");
        assert_eq!(value["changed_files"][0]["bytes_before"], 12);
        assert_eq!(value["changed_files"][0]["bytes_after"], 13);
        assert_eq!(value["changed_files"][0]["lines_before"], 2);
        assert_eq!(value["changed_files"][0]["lines_after"], 2);
        assert_eq!(
            value["changed_files"][0]["hash_before"],
            "blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        assert_eq!(
            value["changed_files"][0]["hash_after"],
            "blake3:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
        );
        assert_eq!(value["attempted_operations"][0]["path"], "src/lib.rs");
        assert_eq!(value["attempted_operations"][0]["hunk_count"], 1);
    }
}

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use lattice_tooling::{ToolError, ToolLimits};

use super::hash::{content_hash, line_hash};
use super::parser::parse_patch_document;
use super::types::{
    ApplyPlan, AttemptedOperation, ChangedFile, FileOperationKind, HashLine, LineChange,
    LineChangeKind, PatchDiagnostic, PatchDocument, PatchErrorCode, PatchHunk, PatchHunkLine,
    PatchOperation, PatchResultEnvelope, PlannedFileChange,
};

pub async fn apply_patch(
    project_root: &Path,
    limits: &ToolLimits,
    patch: &str,
) -> PatchResultEnvelope {
    let document = match parse_patch_document(patch) {
        Ok(document) => document,
        Err(error) => {
            return failure_envelope(
                "patch rejected during parse",
                error.diagnostics,
                attempted_operations_from_headers(patch),
            );
        }
    };

    let attempted_operations = document
        .operations
        .iter()
        .enumerate()
        .map(|(index, operation)| AttemptedOperation::from_operation(index, operation))
        .collect::<Vec<_>>();

    let plan = match build_apply_plan(project_root, limits, &document, attempted_operations).await {
        Ok(plan) => plan,
        Err((diagnostics, attempted_operations)) => {
            return failure_envelope(
                "patch rejected during preflight",
                diagnostics,
                attempted_operations,
            );
        }
    };

    match write_apply_plan(&plan).await {
        Ok(()) => {
            let changed_files = plan
                .changes
                .iter()
                .map(|change| change.manifest.clone())
                .collect::<Vec<_>>();
            PatchResultEnvelope {
                ok: true,
                summary: format!("applied {} operation(s)", plan.attempted_operations.len()),
                errors: Vec::new(),
                changed_files,
                attempted_operations: plan.attempted_operations,
            }
        }
        Err(diagnostics) => failure_envelope(
            "patch failed during write stage; rollback attempted",
            diagnostics,
            plan.attempted_operations,
        ),
    }
}

async fn build_apply_plan(
    project_root: &Path,
    limits: &ToolLimits,
    document: &PatchDocument,
    attempted_operations: Vec<AttemptedOperation>,
) -> Result<ApplyPlan, (Vec<PatchDiagnostic>, Vec<AttemptedOperation>)> {
    let root = match tokio::fs::canonicalize(project_root).await {
        Ok(root) => root,
        Err(error) => {
            return Err((
                vec![PatchDiagnostic::new(
                    PatchErrorCode::IoError,
                    ToolError::IoError {
                        path: project_root.display().to_string(),
                        error,
                    }
                    .to_string(),
                )],
                attempted_operations,
            ));
        }
    };

    let mut diagnostics = Vec::new();
    let mut seen_paths = HashSet::new();
    let mut changes = Vec::new();

    for (operation_index, operation) in document.operations.iter().enumerate() {
        let path = operation.path().to_string();
        if !seen_paths.insert(path.clone()) {
            diagnostics.push(
                PatchDiagnostic::new(
                    PatchErrorCode::DuplicatePath,
                    format!("duplicate patch operation for path '{path}'"),
                )
                .with_path(path)
                .with_operation_index(operation_index),
            );
            continue;
        }

        let resolved = match resolve_target(&root, operation, operation_index).await {
            Ok(resolved) => resolved,
            Err(diagnostic) => {
                diagnostics.push(diagnostic);
                continue;
            }
        };

        let change = match operation {
            PatchOperation::UpdateFile {
                path,
                old_hash,
                hunks,
            } => {
                plan_update_file(
                    path,
                    old_hash,
                    hunks,
                    &resolved.target_path,
                    operation_index,
                    limits,
                )
                .await
            }
            PatchOperation::DeleteFile { path, old_hash } => {
                plan_delete_file(path, old_hash, &resolved.target_path, operation_index).await
            }
        };

        match change {
            Ok(change) => changes.push(change),
            Err(diagnostic) => diagnostics.push(diagnostic),
        }
    }

    if diagnostics.is_empty() {
        Ok(ApplyPlan {
            changes,
            attempted_operations,
        })
    } else {
        Err((diagnostics, attempted_operations))
    }
}

struct ResolvedTarget {
    target_path: PathBuf,
}

async fn resolve_target(
    root: &Path,
    operation: &PatchOperation,
    operation_index: usize,
) -> Result<ResolvedTarget, PatchDiagnostic> {
    let path = operation.path();
    let relative = Path::new(path);

    if relative.is_absolute() || has_disallowed_component(relative) {
        return Err(PatchDiagnostic::new(
            PatchErrorCode::BoundaryDenied,
            format!("patch path '{path}' must be relative and stay under the project root"),
        )
        .with_path(path.to_string())
        .with_operation_index(operation_index));
    }

    let requested = root.join(relative);
    let target_path = match operation {
        PatchOperation::UpdateFile { .. } | PatchOperation::DeleteFile { .. } => {
            let canonical = tokio::fs::canonicalize(&requested).await.map_err(|error| {
                PatchDiagnostic::new(
                    PatchErrorCode::MissingFile,
                    ToolError::IoError {
                        path: requested.display().to_string(),
                        error,
                    }
                    .to_string(),
                )
                .with_path(path.to_string())
                .with_operation_index(operation_index)
            })?;
            if !canonical.starts_with(root) {
                return Err(PatchDiagnostic::new(
                    PatchErrorCode::BoundaryDenied,
                    format!("patch path '{path}' resolves outside the project root"),
                )
                .with_path(path.to_string())
                .with_operation_index(operation_index));
            }
            canonical
        }
    };

    Ok(ResolvedTarget { target_path })
}

fn has_disallowed_component(path: &Path) -> bool {
    path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    })
}

async fn plan_update_file(
    path: &str,
    old_hash: &str,
    hunks: &[PatchHunk],
    target_path: &Path,
    operation_index: usize,
    limits: &ToolLimits,
) -> Result<PlannedFileChange, PatchDiagnostic> {
    let before = read_file_bytes(path, target_path, operation_index).await?;
    validate_old_hash(path, old_hash, &before, operation_index)?;
    let before_text = String::from_utf8(before.clone()).map_err(|error| {
        PatchDiagnostic::new(
            PatchErrorCode::IoError,
            format!("IO error accessing {path}: file is not valid UTF-8: {error}"),
        )
        .with_path(path.to_string())
        .with_operation_index(operation_index)
    })?;
    let apply_outcome = apply_hunks(path, &before_text, hunks, operation_index)?;
    let after_text = apply_outcome.after_text;
    let after = after_text.into_bytes();

    if after.len() > limits.max_write_size {
        return Err(PatchDiagnostic::new(
            PatchErrorCode::IoError,
            ToolError::SizeLimit {
                limit: limits.max_write_size,
                actual: after.len(),
            }
            .to_string(),
        )
        .with_path(path.to_string())
        .with_operation_index(operation_index));
    }

    Ok(PlannedFileChange {
        path: path.to_string(),
        target_path: target_path.to_path_buf(),
        operation_index,
        operation: FileOperationKind::Update,
        bytes_before: Some(before.clone()),
        bytes_after: Some(after.clone()),
        manifest: manifest(
            path,
            FileOperationKind::Update,
            Some(&before),
            Some(&after),
            apply_outcome.line_changes,
        ),
    })
}

async fn plan_delete_file(
    path: &str,
    old_hash: &str,
    target_path: &Path,
    operation_index: usize,
) -> Result<PlannedFileChange, PatchDiagnostic> {
    let before = read_file_bytes(path, target_path, operation_index).await?;
    validate_old_hash(path, old_hash, &before, operation_index)?;

    Ok(PlannedFileChange {
        path: path.to_string(),
        target_path: target_path.to_path_buf(),
        operation_index,
        operation: FileOperationKind::Delete,
        bytes_before: Some(before.clone()),
        bytes_after: None,
        manifest: manifest(
            path,
            FileOperationKind::Delete,
            Some(&before),
            None,
            delete_line_changes(&before),
        ),
    })
}

async fn read_file_bytes(
    path: &str,
    target_path: &Path,
    operation_index: usize,
) -> Result<Vec<u8>, PatchDiagnostic> {
    tokio::fs::read(target_path).await.map_err(|error| {
        PatchDiagnostic::new(
            PatchErrorCode::IoError,
            ToolError::IoError {
                path: target_path.display().to_string(),
                error,
            }
            .to_string(),
        )
        .with_path(path.to_string())
        .with_operation_index(operation_index)
    })
}

fn validate_old_hash(
    path: &str,
    expected_hash: &str,
    bytes: &[u8],
    operation_index: usize,
) -> Result<(), PatchDiagnostic> {
    let current_hash = content_hash(bytes);
    if current_hash != expected_hash {
        return Err(PatchDiagnostic::new(
            PatchErrorCode::HashMismatch,
            format!("hash mismatch for '{path}': expected {expected_hash}, current {current_hash}"),
        )
        .with_path(path.to_string())
        .with_operation_index(operation_index));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ApplyHunksOutcome {
    pub after_text: String,
    pub line_changes: Vec<LineChange>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WorkingLine {
    text: String,
    original_line_number: Option<usize>,
    added_change_index: Option<usize>,
}

pub(crate) fn apply_hunks(
    path: &str,
    before: &str,
    hunks: &[PatchHunk],
    operation_index: usize,
) -> Result<ApplyHunksOutcome, PatchDiagnostic> {
    let final_newline = before.ends_with('\n');
    let mut lines = split_lines(before)
        .into_iter()
        .enumerate()
        .map(|(index, text)| WorkingLine {
            text,
            original_line_number: Some(index + 1),
            added_change_index: None,
        })
        .collect::<Vec<_>>();
    let mut line_changes = Vec::new();

    for hunk in hunks {
        let original = hunk_original_lines(hunk);
        if original.is_empty() {
            return Err(PatchDiagnostic::new(
                PatchErrorCode::HunkAmbiguous,
                "hunk_ambiguous: insertion-only hunks require surrounding context",
            )
            .with_path(path.to_string())
            .with_operation_index(operation_index)
            .with_hunk_index(hunk.index));
        }

        let matches = find_hashline_matches(&lines, &original);
        match matches.as_slice() {
            [] => {
                return Err(PatchDiagnostic::new(
                    PatchErrorCode::HunkMissing,
                    "hunk_missing: original hunk context was not found exactly",
                )
                .with_path(path.to_string())
                .with_operation_index(operation_index)
                .with_hunk_index(hunk.index));
            }
            [start] => {
                let replacement = hunk_replacement_lines(
                    hunk,
                    &lines[*start..(*start + original.len())],
                    &mut line_changes,
                );
                lines.splice(*start..(*start + original.len()), replacement);
            }
            _ => {
                return Err(PatchDiagnostic::new(
                    PatchErrorCode::HunkAmbiguous,
                    format!(
                        "hunk_ambiguous: original hunk context matched {} locations",
                        matches.len()
                    ),
                )
                .with_path(path.to_string())
                .with_operation_index(operation_index)
                .with_hunk_index(hunk.index));
            }
        }
    }

    for (index, line) in lines.iter().enumerate() {
        if let Some(change_index) = line.added_change_index {
            if let Some(change) = line_changes.get_mut(change_index) {
                change.new_hash = Some(line_hash(index + 1, &line.text));
            }
        }
    }

    Ok(ApplyHunksOutcome {
        after_text: join_lines(&lines, final_newline),
        line_changes,
    })
}

fn split_lines(content: &str) -> Vec<String> {
    if content.is_empty() {
        return Vec::new();
    }
    let mut lines = content
        .split('\n')
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    if content.ends_with('\n') {
        lines.pop();
    }
    lines
}

fn join_lines(lines: &[WorkingLine], final_newline: bool) -> String {
    let mut content = lines
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    if final_newline && !content.is_empty() {
        content.push('\n');
    }
    content
}

fn hunk_original_lines(hunk: &PatchHunk) -> Vec<HashLine> {
    hunk.lines
        .iter()
        .filter_map(|line| match line {
            PatchHunkLine::Context(value) | PatchHunkLine::Remove(value) => Some(value.clone()),
            PatchHunkLine::Add(_) => None,
        })
        .collect()
}

fn hunk_replacement_lines(
    hunk: &PatchHunk,
    matched_original: &[WorkingLine],
    line_changes: &mut Vec<LineChange>,
) -> Vec<WorkingLine> {
    let mut replacement = Vec::new();
    let mut original_index = 0usize;

    for line in &hunk.lines {
        match line {
            PatchHunkLine::Context(_) => {
                if let Some(original) = matched_original.get(original_index) {
                    replacement.push(original.clone());
                }
                original_index += 1;
            }
            PatchHunkLine::Remove(hashline) => {
                line_changes.push(LineChange {
                    kind: LineChangeKind::Removed,
                    old_hash: Some(hashline.hash.clone()),
                    new_hash: None,
                });
                original_index += 1;
            }
            PatchHunkLine::Add(text) => {
                let change_index = line_changes.len();
                line_changes.push(LineChange {
                    kind: LineChangeKind::Added,
                    old_hash: None,
                    new_hash: None,
                });
                replacement.push(WorkingLine {
                    text: text.clone(),
                    original_line_number: None,
                    added_change_index: Some(change_index),
                });
            }
        }
    }

    replacement
}

fn find_hashline_matches(lines: &[WorkingLine], needle: &[HashLine]) -> Vec<usize> {
    if needle.is_empty() || needle.len() > lines.len() {
        return Vec::new();
    }

    lines
        .windows(needle.len())
        .enumerate()
        .filter_map(|(index, window)| {
            let matched = window.iter().zip(needle.iter()).all(|(line, expected)| {
                let Some(line_number) = line.original_line_number else {
                    return false;
                };
                line.text == expected.text && line_hash(line_number, &line.text) == expected.hash
            });
            matched.then_some(index)
        })
        .collect()
}

async fn write_apply_plan(plan: &ApplyPlan) -> Result<(), Vec<PatchDiagnostic>> {
    let mut backups = Vec::new();

    for change in &plan.changes {
        let write_result = match (&change.operation, &change.bytes_after) {
            (FileOperationKind::Update, Some(after)) => {
                tokio::fs::write(&change.target_path, after).await
            }
            (FileOperationKind::Delete, None) => tokio::fs::remove_file(&change.target_path).await,
            _ => unreachable!("invalid planned patch change"),
        };

        match write_result {
            Ok(()) => backups.push(CompletedWrite {
                path: change.path.clone(),
                target_path: change.target_path.clone(),
                operation_index: change.operation_index,
                operation: change.operation,
                previous_bytes: change.bytes_before.clone(),
            }),
            Err(error) => {
                let mut diagnostics = vec![PatchDiagnostic::new(
                    PatchErrorCode::IoError,
                    ToolError::IoError {
                        path: change.target_path.display().to_string(),
                        error,
                    }
                    .to_string(),
                )
                .with_path(change.path.clone())
                .with_operation_index(change.operation_index)];
                diagnostics.extend(rollback_failed(&backups).await);
                return Err(diagnostics);
            }
        }
    }

    Ok(())
}

struct CompletedWrite {
    path: String,
    target_path: PathBuf,
    operation_index: usize,
    operation: FileOperationKind,
    previous_bytes: Option<Vec<u8>>,
}

async fn rollback_failed(backups: &[CompletedWrite]) -> Vec<PatchDiagnostic> {
    let mut diagnostics = Vec::new();

    for backup in backups.iter().rev() {
        let rollback_result = match (&backup.operation, &backup.previous_bytes) {
            (FileOperationKind::Update, Some(bytes)) | (FileOperationKind::Delete, Some(bytes)) => {
                tokio::fs::write(&backup.target_path, bytes).await
            }
            _ => Ok(()),
        };

        if let Err(error) = rollback_result {
            diagnostics.push(
                PatchDiagnostic::new(
                    PatchErrorCode::RollbackFailed,
                    format!(
                        "rollback_failed: {}",
                        ToolError::IoError {
                            path: backup.target_path.display().to_string(),
                            error,
                        }
                    ),
                )
                .with_path(backup.path.clone())
                .with_operation_index(backup.operation_index),
            );
        }
    }

    diagnostics
}

fn manifest(
    path: &str,
    operation: FileOperationKind,
    before: Option<&[u8]>,
    after: Option<&[u8]>,
    line_changes: Vec<LineChange>,
) -> ChangedFile {
    ChangedFile {
        path: path.to_string(),
        operation,
        bytes_before: before.map_or(0, <[u8]>::len),
        bytes_after: after.map_or(0, <[u8]>::len),
        lines_before: before.map_or(0, line_count),
        lines_after: after.map_or(0, line_count),
        hash_before: before.map(content_hash),
        hash_after: after.map(content_hash),
        line_changes,
    }
}

fn delete_line_changes(before: &[u8]) -> Vec<LineChange> {
    String::from_utf8_lossy(before)
        .lines()
        .enumerate()
        .map(|(index, line)| LineChange {
            kind: LineChangeKind::Removed,
            old_hash: Some(line_hash(index + 1, line)),
            new_hash: None,
        })
        .collect()
}

fn line_count(bytes: &[u8]) -> usize {
    String::from_utf8_lossy(bytes).lines().count()
}

fn failure_envelope(
    summary: impl Into<String>,
    errors: Vec<PatchDiagnostic>,
    attempted_operations: Vec<AttemptedOperation>,
) -> PatchResultEnvelope {
    PatchResultEnvelope {
        ok: false,
        summary: summary.into(),
        errors,
        changed_files: Vec::new(),
        attempted_operations,
    }
}

fn attempted_operations_from_headers(patch: &str) -> Vec<AttemptedOperation> {
    let mut attempted = Vec::new();
    let mut current: Option<AttemptedOperation> = None;

    for line in patch.lines() {
        if let Some(path) = line.strip_prefix("*** Update File:") {
            push_current(&mut attempted, &mut current);
            current = Some(AttemptedOperation {
                index: attempted.len(),
                path: path.trim().to_string(),
                operation: FileOperationKind::Update,
                old_hash: None,
                hunk_count: 0,
            });
        } else if let Some(path) = line.strip_prefix("*** Delete File:") {
            push_current(&mut attempted, &mut current);
            current = Some(AttemptedOperation {
                index: attempted.len(),
                path: path.trim().to_string(),
                operation: FileOperationKind::Delete,
                old_hash: None,
                hunk_count: 0,
            });
        } else if let Some(hash) = line.strip_prefix("*** Old Hash:") {
            if let Some(operation) = current.as_mut() {
                operation.old_hash = Some(hash.trim().to_string());
            }
        } else if line.starts_with("@@") {
            if let Some(operation) = current.as_mut() {
                operation.hunk_count += 1;
            }
        }
    }

    push_current(&mut attempted, &mut current);
    attempted
}

fn push_current(attempted: &mut Vec<AttemptedOperation>, current: &mut Option<AttemptedOperation>) {
    if let Some(operation) = current.take() {
        attempted.push(operation);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(content: &str) -> String {
        content_hash(content.as_bytes())
    }

    fn parse_envelope(envelope: &PatchResultEnvelope) -> serde_json::Value {
        serde_json::to_value(envelope).unwrap()
    }

    fn context(line_number: usize, line: &str) -> String {
        format!(" {}\t{}", line_hash(line_number, line), line)
    }

    fn remove(line_number: usize, line: &str) -> String {
        format!("-{}\t{}", line_hash(line_number, line), line)
    }

    #[tokio::test]
    async fn apply_patch_engine_missing_hash_on_update_fails_before_any_write() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("sample.txt");
        std::fs::write(&file, "old\n").unwrap();
        let patch = "\
*** Begin Patch
*** Update File: sample.txt
@@
-old
+new
*** End Patch";

        let result = apply_patch(dir.path(), &ToolLimits::default(), patch).await;

        assert!(!result.ok);
        assert_eq!(result.errors[0].code.as_str(), "parse_error");
        assert_eq!(std::fs::read_to_string(file).unwrap(), "old\n");
        assert!(result.changed_files.is_empty());
        assert_eq!(result.attempted_operations[0].path, "sample.txt");
    }

    #[tokio::test]
    async fn apply_patch_engine_missing_hash_on_delete_fails_before_any_write() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("sample.txt");
        std::fs::write(&file, "old\n").unwrap();
        let patch = "\
*** Begin Patch
*** Delete File: sample.txt
*** End Patch";

        let result = apply_patch(dir.path(), &ToolLimits::default(), patch).await;

        assert!(!result.ok);
        assert_eq!(result.errors[0].code.as_str(), "parse_error");
        assert!(file.exists());
        assert!(result.changed_files.is_empty());
        assert_eq!(
            result.attempted_operations[0].operation,
            FileOperationKind::Delete
        );
    }

    #[tokio::test]
    async fn apply_patch_engine_hash_mismatch_fails_before_any_write() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("sample.txt");
        std::fs::write(&file, "actual\n").unwrap();
        let stale_hash = hash("stale\n");
        let actual = remove(1, "actual");
        let patch = format!(
            "\
*** Begin Patch
*** Update File: sample.txt
*** Old Hash: {stale_hash}
@@
{actual}
+new
*** End Patch"
        );

        let result = apply_patch(dir.path(), &ToolLimits::default(), &patch).await;

        assert!(!result.ok);
        assert_eq!(result.errors[0].code.as_str(), "hash_mismatch");
        assert_eq!(std::fs::read_to_string(file).unwrap(), "actual\n");
        assert!(result.changed_files.is_empty());
        assert_eq!(result.attempted_operations.len(), 1);
    }

    #[tokio::test]
    async fn apply_patch_engine_line_numbered_hashes_disambiguate_repeated_content() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("sample.txt");
        let before = "same\nold\nsame\nold\n";
        std::fs::write(&file, before).unwrap();
        let old_hash = hash(before);
        let same = context(3, "same");
        let old = remove(4, "old");
        let patch = format!(
            "\
*** Begin Patch
*** Update File: sample.txt
*** Old Hash: {old_hash}
@@
{same}
{old}
+new
*** End Patch"
        );

        let result = apply_patch(dir.path(), &ToolLimits::default(), &patch).await;

        assert!(result.ok);
        assert_eq!(
            std::fs::read_to_string(file).unwrap(),
            "same\nold\nsame\nnew\n"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn apply_patch_engine_write_stage_failure_rolls_back_already_written_files() {
        use std::os::unix::fs::PermissionsExt;

        // This test forces a write failure via a read-only (0o444) file. Root
        // bypasses DAC permission checks, so the write would succeed and the
        // rollback path under test would never trigger — skip when running as
        // root (e.g. inside containerized CI).
        if unsafe { libc::geteuid() } == 0 {
            eprintln!("skipping: read-only write-failure path is not exercised as root");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.txt");
        let second = dir.path().join("second.txt");
        let first_before = "first old\n";
        let second_before = "second old\n";
        std::fs::write(&first, first_before).unwrap();
        std::fs::write(&second, second_before).unwrap();
        std::fs::set_permissions(&second, std::fs::Permissions::from_mode(0o444)).unwrap();
        let first_old = remove(1, "first old");
        let second_old = remove(1, "second old");

        let patch = format!(
            "\
*** Begin Patch
*** Update File: first.txt
*** Old Hash: {}
@@
{first_old}
+first new
*** Update File: second.txt
*** Old Hash: {}
@@
{second_old}
+second new
*** End Patch",
            hash(first_before),
            hash(second_before)
        );

        let result = apply_patch(dir.path(), &ToolLimits::default(), &patch).await;

        std::fs::set_permissions(&second, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(!result.ok);
        assert_eq!(result.errors[0].code.as_str(), "io_error");
        assert_eq!(std::fs::read_to_string(first).unwrap(), first_before);
        assert_eq!(std::fs::read_to_string(second).unwrap(), second_before);
        assert!(result.changed_files.is_empty());
    }

    #[tokio::test]
    async fn apply_patch_engine_success_payload_includes_changed_file_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let update = dir.path().join("update.txt");
        let delete = dir.path().join("delete.txt");
        let update_before = "alpha\nold\nomega\n";
        let delete_before = "remove me\n";
        std::fs::write(&update, update_before).unwrap();
        std::fs::write(&delete, delete_before).unwrap();
        let alpha = context(1, "alpha");
        let old = remove(2, "old");
        let omega = context(3, "omega");

        let patch = format!(
            "\
*** Begin Patch
*** Update File: update.txt
*** Old Hash: {}
@@
{alpha}
{old}
+new
{omega}
*** Delete File: delete.txt
*** Old Hash: {}
*** End Patch",
            hash(update_before),
            hash(delete_before)
        );

        let result = apply_patch(dir.path(), &ToolLimits::default(), &patch).await;
        let value = parse_envelope(&result);

        assert!(result.ok);
        assert_eq!(result.changed_files.len(), 2);
        assert_eq!(value["changed_files"][0]["path"], "update.txt");
        assert!(value["changed_files"][0]["bytes_before"].as_u64().unwrap() > 0);
        assert!(value["changed_files"][0]["bytes_after"].as_u64().unwrap() > 0);
        assert_eq!(
            std::fs::read_to_string(update).unwrap(),
            "alpha\nnew\nomega\n"
        );
        assert!(!delete.exists());
    }

    #[tokio::test]
    async fn apply_patch_engine_failure_payload_keeps_changed_files_empty_with_attempts() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("sample.txt");
        let before = "actual\n";
        std::fs::write(&file, before).unwrap();
        let old_hash = hash(before);
        let missing = remove(1, "missing");
        let patch = format!(
            "\
*** Begin Patch
*** Update File: sample.txt
*** Old Hash: {old_hash}
@@
{missing}
+new
*** End Patch"
        );

        let result = apply_patch(dir.path(), &ToolLimits::default(), &patch).await;

        assert!(!result.ok);
        assert_eq!(result.errors[0].code.as_str(), "hunk_missing");
        assert!(result.changed_files.is_empty());
        assert_eq!(result.attempted_operations.len(), 1);
        assert_eq!(std::fs::read_to_string(file).unwrap(), before);
    }

    #[tokio::test]
    async fn apply_patch_engine_allows_full_file_replacement_with_hashline_anchors() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("schema.txt");
        let before = "old one\nold two\n";
        std::fs::write(&file, before).unwrap();
        let old_hash = hash(before);
        let old_one = remove(1, "old one");
        let old_two = remove(2, "old two");
        let patch = format!(
            "\
*** Begin Patch
*** Update File: schema.txt
*** Old Hash: {old_hash}
@@
{old_one}
{old_two}
+new one
+new two
*** End Patch"
        );

        let result = apply_patch(dir.path(), &ToolLimits::default(), &patch).await;

        assert!(result.ok);
        assert_eq!(std::fs::read_to_string(file).unwrap(), "new one\nnew two\n");
        assert_eq!(result.changed_files.len(), 1);
        assert_eq!(result.changed_files[0].line_changes.len(), 4);
        assert!(result.changed_files[0]
            .line_changes
            .iter()
            .any(|change| matches!(change.kind, LineChangeKind::Removed)));
        assert!(result.changed_files[0]
            .line_changes
            .iter()
            .any(|change| matches!(change.kind, LineChangeKind::Added)));
    }
}

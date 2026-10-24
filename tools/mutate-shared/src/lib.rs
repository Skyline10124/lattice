// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Shared semantic-edit logic for `mutate__create` / `mutate__edit` (#31).
//!
//! This crate owns the pure text transforms and the diagnostic messages the
//! model sees when an edit cannot be applied. It performs no I/O and imports
//! no WIT: the host alone resolves paths, rechecks Local Revisions, and
//! installs candidate bytes.
//!
//! The HashLine *transport* is gone — guests send candidate bytes — but the
//! behavior invariants from the native HashLine compile path are preserved:
//! no-op refusal, trailing-newline preservation, empty-file insert refusal,
//! and the same occurrence / missing-anchor diagnostics.

#![no_std]
extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use serde::Deserialize;

/// One semantic edit operation, matching the catalog/`mutate__edit` wire shape.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum EditOp {
    ReplaceText {
        old: String,
        new: String,
        #[serde(default)]
        occurrence: Option<u64>,
    },
    InsertBefore {
        anchor: String,
        content: String,
        #[serde(default)]
        occurrence: Option<u64>,
    },
    InsertAfter {
        anchor: String,
        content: String,
        #[serde(default)]
        occurrence: Option<u64>,
    },
}

/// Guest-visible diagnostic for a refused semantic edit.
///
/// `field` / `reason` mirror the native `Validation` / `ReplanRequired`
/// payloads so Component wire text stays stable across the migration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditDiag {
    pub field: String,
    pub reason: String,
    pub replan: bool,
}

impl EditDiag {
    fn validation(field: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            reason: reason.into(),
            replan: false,
        }
    }

    fn replan(field: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            reason: reason.into(),
            replan: true,
        }
    }
}

/// Apply semantic edits to `before`, enforcing the native behavior pins.
///
/// Order of checks after the ops succeed:
/// 1. no-op (`before == after`) → validation on `edits`
/// 2. trailing newline changed → validation on `edits`
/// 3. empty existing file gained content → validation on `edits`
pub fn apply_edits(before: &str, edits: &[EditOp]) -> Result<String, EditDiag> {
    if edits.is_empty() {
        return Err(EditDiag::validation(
            "edits",
            "mutate__edit requires at least one edit operation",
        ));
    }

    let after = apply_semantic_edit_operations(before, edits)?;
    validate_edit_result(before, &after)?;
    Ok(after)
}

/// Post-apply behavior pins shared with the former HashLine compile path.
fn validate_edit_result(before: &str, after: &str) -> Result<(), EditDiag> {
    if before == after {
        return Err(EditDiag::validation(
            "edits",
            "semantic edit produced no changes",
        ));
    }

    if trailing_newline(before) != trailing_newline(after) {
        return Err(EditDiag::validation(
            "edits",
            "semantic edits must preserve the file's trailing newline state",
        ));
    }

    // Preserve the HashLine empty-file insert refusal: an existing empty file
    // cannot receive content through semantic insert/replace under the old
    // compile path, and the same refusal keeps model-facing diagnostics
    // stable after the candidate-bytes migration.
    if before.is_empty() && !after.is_empty() {
        return Err(EditDiag::validation(
            "edits",
            "semantic insertion into an empty existing file is not supported by hashline patch yet",
        ));
    }

    Ok(())
}

fn apply_semantic_edit_operations(before: &str, edits: &[EditOp]) -> Result<String, EditDiag> {
    let mut content = before.to_string();
    for edit in edits {
        match edit {
            EditOp::ReplaceText {
                old,
                new,
                occurrence,
            } => {
                let (start, end) = find_semantic_occurrence(&content, old, *occurrence, "old")?;
                content.replace_range(start..end, new);
            }
            EditOp::InsertBefore {
                anchor,
                content: inserted,
                occurrence,
            } => {
                let (start, _) = find_semantic_occurrence(&content, anchor, *occurrence, "anchor")?;
                content.insert_str(start, inserted);
            }
            EditOp::InsertAfter {
                anchor,
                content: inserted,
                occurrence,
            } => {
                let (_, end) = find_semantic_occurrence(&content, anchor, *occurrence, "anchor")?;
                content.insert_str(end, inserted);
            }
        }
    }
    Ok(content)
}

fn find_semantic_occurrence(
    haystack: &str,
    needle: &str,
    occurrence: Option<u64>,
    field: &str,
) -> Result<(usize, usize), EditDiag> {
    const MAX_COMPONENT_OCCURRENCE: u64 = u32::MAX as u64;

    if needle.is_empty() {
        return Err(EditDiag::validation(field, "match text must not be empty"));
    }
    let matches: Vec<(usize, usize)> = haystack
        .match_indices(needle)
        .map(|(start, value)| (start, start + value.len()))
        .collect();

    let index = match occurrence {
        Some(0) => {
            return Err(EditDiag::validation("occurrence", "occurrence is 1-based"));
        }
        Some(value) if value > MAX_COMPONENT_OCCURRENCE => {
            return Err(EditDiag::validation(
                "occurrence",
                "occurrence exceeds the supported range",
            ));
        }
        Some(value) => usize::try_from(value - 1).map_err(|_| {
            EditDiag::validation("occurrence", "occurrence exceeds the supported range")
        })?,
        None if matches.len() == 1 => 0,
        None if matches.is_empty() => {
            return Err(EditDiag::replan(
                field,
                format!("{field} text was not found"),
            ));
        }
        None => {
            return Err(EditDiag::replan(
                field,
                format!(
                    "{field} text matched {} occurrences; provide occurrence",
                    matches.len()
                ),
            ));
        }
    };

    matches.get(index).copied().ok_or_else(|| {
        EditDiag::replan(
            field,
            format!("{field} occurrence {} was not found", index + 1),
        )
    })
}

fn trailing_newline(content: &str) -> bool {
    content.ends_with('\n')
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    fn replace(old: &str, new: &str, occurrence: Option<u64>) -> EditOp {
        EditOp::ReplaceText {
            old: old.to_string(),
            new: new.to_string(),
            occurrence,
        }
    }

    fn insert_before(anchor: &str, content: &str, occurrence: Option<u64>) -> EditOp {
        EditOp::InsertBefore {
            anchor: anchor.to_string(),
            content: content.to_string(),
            occurrence,
        }
    }

    fn insert_after(anchor: &str, content: &str, occurrence: Option<u64>) -> EditOp {
        EditOp::InsertAfter {
            anchor: anchor.to_string(),
            content: content.to_string(),
            occurrence,
        }
    }

    #[test]
    fn replace_text_rewrites_the_requested_occurrence() {
        let before = "aa bb aa\n";
        let after = apply_edits(before, &[replace("aa", "XX", Some(2))]).unwrap();
        assert_eq!(after, "aa bb XX\n");
    }

    #[test]
    fn insert_before_and_after_honor_anchors() {
        let before = "one\ntwo\n";
        let after = apply_edits(
            before,
            &[
                insert_before("two", "1.5\n", None),
                insert_after("one\n", "mid\n", None),
            ],
        )
        .unwrap();
        assert_eq!(after, "one\nmid\n1.5\ntwo\n");
    }

    #[test]
    fn apply_edits_rejects_invalid_operations_with_stable_diagnostics() {
        for (name, before, edit, expected_reason, expected_field, expected_replan) in [
            (
                "ambiguous_match",
                "x x\n",
                replace("x", "y", None),
                "old text matched 2 occurrences; provide occurrence",
                Some("old"),
                Some(true),
            ),
            (
                "missing_anchor",
                "hello\n",
                insert_after("missing", "!\n", None),
                "anchor text was not found",
                None,
                Some(true),
            ),
            (
                "no_op_replace",
                "same\n",
                replace("same", "same", None),
                "semantic edit produced no changes",
                None,
                Some(false),
            ),
            (
                "trailing_newline",
                "line\n",
                replace("line\n", "line", None),
                "semantic edits must preserve the file's trailing newline state",
                None,
                None,
            ),
            (
                "empty_match_text",
                "a\n",
                replace("", "b", None),
                "match text must not be empty",
                Some("old"),
                None,
            ),
            (
                "occurrence_zero",
                "a a\n",
                replace("a", "b", Some(0)),
                "occurrence is 1-based",
                Some("occurrence"),
                None,
            ),
            (
                "occurrence_above_wasm32_index_range",
                "a a\n",
                replace("a", "b", Some(u64::from(u32::MAX) + 2)),
                "occurrence exceeds the supported range",
                Some("occurrence"),
                Some(false),
            ),
        ] {
            let err = apply_edits(before, &[edit]).expect_err(name);
            assert_eq!(err.reason, expected_reason, "{name}");
            if let Some(field) = expected_field {
                assert_eq!(err.field, field, "{name}");
            }
            if let Some(replan) = expected_replan {
                assert_eq!(err.replan, replan, "{name}");
            }
        }
    }

    #[test]
    fn empty_file_gaining_content_is_refused_with_stable_message() {
        // Semantic ops cannot reach empty→non-empty (anchors need content);
        // pin the post-apply guard directly so the wire text stays covered.
        let err = validate_edit_result("", "content").unwrap_err();
        assert_eq!(
            err.reason,
            "semantic insertion into an empty existing file is not supported by hashline patch yet"
        );
    }

    #[test]
    fn edit_op_deserializes_tagged_wire_shape() {
        let op: EditOp =
            serde_json::from_str(r#"{"op":"replace_text","old":"a","new":"b","occurrence":1}"#)
                .unwrap();
        assert_eq!(op, replace("a", "b", Some(1)));
    }
}

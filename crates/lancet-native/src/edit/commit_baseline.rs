// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

#![allow(dead_code)]

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::hash::{content_hash, line_hash};
use super::types::{HashLine, PatchDocument, PatchHunkLine, PatchOperation};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HunkAnchorEvidence {
    pub(crate) file_path: String,
    pub(crate) hunk_index: usize,
    pub(crate) anchors: Vec<HashLine>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HunkAnchorSummary {
    pub(crate) file_path: String,
    pub(crate) hunk_index: usize,
    pub(crate) anchor_count: usize,
    pub(crate) anchor_hashes: Vec<String>,
    pub(crate) match_result: HunkAnchorMatchResult,
    pub(crate) match_offset: Option<usize>,
    pub(crate) baseline_hash: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HunkAnchorMatchResult {
    Matched,
    MissingAnchor,
    HashMismatch,
    AmbiguousHunk,
}

impl HunkAnchorMatchResult {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Matched => "matched",
            Self::MissingAnchor => "missing_anchor",
            Self::HashMismatch => "hash_mismatch",
            Self::AmbiguousHunk => "ambiguous_hunk",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CommitBaselineValidation {
    Passed,
    Failed(CommitBaselineFailureReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommitBaselineFailureReason {
    LowerDrift,
    MissingAnchor,
    HashMismatch,
    AmbiguousHunk,
    VerifyLayerContamination,
}

impl CommitBaselineFailureReason {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::LowerDrift => "lower_drift",
            Self::MissingAnchor => "missing_anchor",
            Self::HashMismatch => "hash_mismatch",
            Self::AmbiguousHunk => "ambiguous_hunk",
            Self::VerifyLayerContamination => "verify_layer_contamination",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CommitBaselineReport {
    pub(crate) validation: CommitBaselineValidation,
    pub(crate) hunks: Vec<HunkAnchorSummary>,
    pub(crate) lower_drift_paths: Vec<String>,
}

pub(crate) fn collect_hunk_anchor_evidence(document: &PatchDocument) -> Vec<HunkAnchorEvidence> {
    document
        .operations
        .iter()
        .flat_map(|operation| match operation {
            PatchOperation::UpdateFile { path, hunks, .. } => hunks
                .iter()
                .map(|hunk| HunkAnchorEvidence {
                    file_path: path.clone(),
                    hunk_index: hunk.index,
                    anchors: hunk
                        .lines
                        .iter()
                        .filter_map(|line| match line {
                            PatchHunkLine::Context(hashline) | PatchHunkLine::Remove(hashline) => {
                                Some(hashline.clone())
                            }
                            PatchHunkLine::Add(_) => None,
                        })
                        .collect(),
                })
                .collect::<Vec<_>>(),
            PatchOperation::DeleteFile { .. } => Vec::new(),
        })
        .collect()
}

pub(crate) fn validate_commit_baseline_evidence(
    evidence: &[HunkAnchorEvidence],
    baseline_files: &BTreeMap<String, Vec<u8>>,
) -> CommitBaselineReport {
    let mut validation = CommitBaselineValidation::Passed;
    let hunks = evidence
        .iter()
        .map(|hunk| {
            let current = baseline_files
                .get(&hunk.file_path)
                .and_then(|content| std::str::from_utf8(content).ok());
            let baseline_hash = baseline_files
                .get(&hunk.file_path)
                .map_or_else(|| content_hash(&[]), |content| content_hash(content));
            let summary = validate_hunk(hunk, current, baseline_hash);

            if matches!(validation, CommitBaselineValidation::Passed)
                && let Some(reason) = failure_reason(summary.match_result)
            {
                validation = CommitBaselineValidation::Failed(reason);
            }

            summary
        })
        .collect();

    CommitBaselineReport {
        validation,
        hunks,
        lower_drift_paths: Vec::new(),
    }
}

pub(crate) fn commit_baseline_details_json(report: &CommitBaselineReport) -> Value {
    let (validation, failure_reason) = match &report.validation {
        CommitBaselineValidation::Passed => ("passed", None),
        CommitBaselineValidation::Failed(reason) => ("failed", Some(reason.as_str())),
    };
    let hunks = report
        .hunks
        .iter()
        .map(|hunk| {
            let mut value = json!({
                "file_path": hunk.file_path,
                "hunk_index": hunk.hunk_index,
                "anchor_count": hunk.anchor_count,
                "anchor_hashes": hunk.anchor_hashes,
                "match_result": hunk.match_result.as_str(),
                "baseline_hash": hunk.baseline_hash,
            });
            if let Some(match_offset) = hunk.match_offset {
                value["match_offset"] = json!(match_offset);
            }
            value
        })
        .collect::<Vec<_>>();

    json!({
        "commit_baseline_validation": validation,
        "commit_baseline_failure_reason": failure_reason,
        "lower_drift_paths": report.lower_drift_paths,
        "hunks": hunks,
    })
}

fn validate_hunk(
    evidence: &HunkAnchorEvidence,
    current_content: Option<&str>,
    baseline_hash: String,
) -> HunkAnchorSummary {
    let current_lines = current_content.map(split_lines).unwrap_or_default();
    let anchor_hashes = evidence
        .anchors
        .iter()
        .map(|anchor| anchor.hash.clone())
        .collect::<Vec<_>>();
    let (match_result, match_offset) = match_anchor_window(&current_lines, &evidence.anchors);

    HunkAnchorSummary {
        file_path: evidence.file_path.clone(),
        hunk_index: evidence.hunk_index,
        anchor_count: evidence.anchors.len(),
        anchor_hashes,
        match_result,
        match_offset,
        baseline_hash,
    }
}

fn match_anchor_window(
    lines: &[&str],
    anchors: &[HashLine],
) -> (HunkAnchorMatchResult, Option<usize>) {
    if anchors.is_empty() || anchors.len() > lines.len() {
        return (HunkAnchorMatchResult::MissingAnchor, None);
    }

    let mut full_matches = Vec::new();
    let mut text_window_seen = false;
    for (offset, window) in lines.windows(anchors.len()).enumerate() {
        let text_matches = window
            .iter()
            .zip(anchors.iter())
            .all(|(line, anchor)| *line == anchor.text);
        if !text_matches {
            continue;
        }
        text_window_seen = true;

        let hash_matches = window
            .iter()
            .zip(anchors.iter())
            .enumerate()
            .all(|(index, (line, anchor))| line_hash(offset + index + 1, line) == anchor.hash);
        if hash_matches {
            full_matches.push(offset);
        }
    }

    match full_matches.as_slice() {
        [offset] => (HunkAnchorMatchResult::Matched, Some(*offset)),
        [] if text_window_seen => (HunkAnchorMatchResult::HashMismatch, None),
        [] => (HunkAnchorMatchResult::MissingAnchor, None),
        _ => (HunkAnchorMatchResult::AmbiguousHunk, None),
    }
}

fn split_lines(content: &str) -> Vec<&str> {
    if content.is_empty() {
        return Vec::new();
    }
    let mut lines = content.split('\n').collect::<Vec<_>>();
    if content.ends_with('\n') {
        lines.pop();
    }
    lines
}

fn failure_reason(match_result: HunkAnchorMatchResult) -> Option<CommitBaselineFailureReason> {
    match match_result {
        HunkAnchorMatchResult::Matched => None,
        HunkAnchorMatchResult::MissingAnchor => Some(CommitBaselineFailureReason::MissingAnchor),
        HunkAnchorMatchResult::HashMismatch => Some(CommitBaselineFailureReason::HashMismatch),
        HunkAnchorMatchResult::AmbiguousHunk => Some(CommitBaselineFailureReason::AmbiguousHunk),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::hash::line_hash;
    use crate::edit::types::{PatchHunk, PatchHunkLine};

    fn anchor(line_number: usize, text: &str) -> HashLine {
        HashLine {
            hash: line_hash(line_number, text),
            text: text.to_string(),
        }
    }

    #[test]
    fn preserved_hunk_anchor_evidence_keeps_distinct_ordered_hunks() {
        let document = PatchDocument {
            operations: vec![PatchOperation::UpdateFile {
                path: "src/lib.rs".to_string(),
                old_hash: "blake3:old".to_string(),
                hunks: vec![
                    PatchHunk {
                        index: 0,
                        lines: vec![
                            PatchHunkLine::Context(anchor(1, "same")),
                            PatchHunkLine::Remove(anchor(2, "remove-a")),
                            PatchHunkLine::Add("new-a".to_string()),
                            PatchHunkLine::Context(anchor(3, "same")),
                        ],
                    },
                    PatchHunk {
                        index: 1,
                        lines: vec![
                            PatchHunkLine::Context(anchor(10, "same")),
                            PatchHunkLine::Remove(anchor(11, "remove-b")),
                            PatchHunkLine::Add("new-b".to_string()),
                            PatchHunkLine::Context(anchor(12, "same")),
                        ],
                    },
                ],
            }],
        };

        let evidence = collect_hunk_anchor_evidence(&document);

        assert_eq!(evidence.len(), 2);
        assert_eq!(evidence[0].file_path, "src/lib.rs");
        assert_eq!(evidence[0].hunk_index, 0);
        assert_eq!(evidence[0].anchors.len(), 3);
        assert_eq!(
            evidence[0]
                .anchors
                .iter()
                .map(|anchor| anchor.text.as_str())
                .collect::<Vec<_>>(),
            vec!["same", "remove-a", "same"]
        );
        assert_eq!(evidence[1].file_path, "src/lib.rs");
        assert_eq!(evidence[1].hunk_index, 1);
        assert_eq!(evidence[1].anchors.len(), 3);
        assert_eq!(
            evidence[1]
                .anchors
                .iter()
                .map(|anchor| anchor.hash.as_str())
                .collect::<Vec<_>>(),
            vec![
                line_hash(10, "same"),
                line_hash(11, "remove-b"),
                line_hash(12, "same")
            ]
        );
        assert_ne!(evidence[0].anchors, evidence[1].anchors);
    }

    #[test]
    fn commit_baseline_matches_repeated_text_by_one_hash_window() {
        let evidence = vec![HunkAnchorEvidence {
            file_path: "src/lib.rs".to_string(),
            hunk_index: 7,
            anchors: vec![anchor(3, "repeat"), anchor(4, "target")],
        }];
        let baseline = baseline(
            "src/lib.rs",
            "repeat\nother\nrepeat\ntarget\nrepeat\ntarget\n",
        );

        let report = validate_commit_baseline_evidence(&evidence, &baseline);

        assert_eq!(report.validation, CommitBaselineValidation::Passed);
        assert_eq!(report.hunks[0].match_result, HunkAnchorMatchResult::Matched);
        assert_eq!(report.hunks[0].match_offset, Some(2));
    }

    #[test]
    fn commit_baseline_reports_missing_anchor_text() {
        let evidence = vec![HunkAnchorEvidence {
            file_path: "src/lib.rs".to_string(),
            hunk_index: 0,
            anchors: vec![anchor(2, "missing")],
        }];
        let baseline = baseline("src/lib.rs", "alpha\nbeta\n");

        let report = validate_commit_baseline_evidence(&evidence, &baseline);

        assert_eq!(
            report.validation,
            CommitBaselineValidation::Failed(CommitBaselineFailureReason::MissingAnchor)
        );
        assert_eq!(
            report.hunks[0].match_result,
            HunkAnchorMatchResult::MissingAnchor
        );
    }

    #[test]
    fn commit_baseline_reports_hash_mismatch_for_shifted_text_window() {
        let evidence = vec![HunkAnchorEvidence {
            file_path: "src/lib.rs".to_string(),
            hunk_index: 0,
            anchors: vec![anchor(2, "beta")],
        }];
        let baseline = baseline("src/lib.rs", "inserted\nalpha\nbeta\n");

        let report = validate_commit_baseline_evidence(&evidence, &baseline);

        assert_eq!(
            report.validation,
            CommitBaselineValidation::Failed(CommitBaselineFailureReason::HashMismatch)
        );
        assert_eq!(
            report.hunks[0].match_result,
            HunkAnchorMatchResult::HashMismatch
        );
    }

    #[test]
    fn commit_baseline_reports_ambiguous_continuous_windows() {
        let text = colliding_text_for_lines(1, 2);
        let evidence = vec![HunkAnchorEvidence {
            file_path: "src/lib.rs".to_string(),
            hunk_index: 0,
            anchors: vec![anchor(1, &text)],
        }];
        let baseline = baseline("src/lib.rs", &format!("{text}\n{text}\n"));

        let report = validate_commit_baseline_evidence(&evidence, &baseline);

        assert_eq!(
            report.validation,
            CommitBaselineValidation::Failed(CommitBaselineFailureReason::AmbiguousHunk)
        );
        assert_eq!(
            report.hunks[0].match_result,
            HunkAnchorMatchResult::AmbiguousHunk
        );
    }

    #[test]
    fn commit_baseline_details_json_omits_anchor_source_text() {
        let evidence = vec![HunkAnchorEvidence {
            file_path: "src/lib.rs".to_string(),
            hunk_index: 0,
            anchors: vec![anchor(1, "secret source line")],
        }];
        let baseline = baseline("src/lib.rs", "secret source line\n");
        let report = validate_commit_baseline_evidence(&evidence, &baseline);

        let value = commit_baseline_details_json(&report);
        let encoded = serde_json::to_string(&value).unwrap();

        assert_eq!(
            value["hunks"][0]["anchor_hashes"][0],
            line_hash(1, "secret source line")
        );
        assert_eq!(
            value["hunks"][0]["baseline_hash"],
            content_hash(b"secret source line\n")
        );
        assert!(!encoded.contains("secret source line"));
        assert!(!encoded.contains("anchor_text"));
        assert!(!encoded.contains("source"));
    }

    fn baseline(path: &str, content: &str) -> BTreeMap<String, Vec<u8>> {
        BTreeMap::from([(path.to_string(), content.as_bytes().to_vec())])
    }

    fn colliding_text_for_lines(first: usize, second: usize) -> String {
        (0..200_000)
            .map(|index| format!("collision-candidate-{index}"))
            .find(|text| line_hash(first, text) == line_hash(second, text))
            .expect("short hash prefix should produce a deterministic collision")
    }
}

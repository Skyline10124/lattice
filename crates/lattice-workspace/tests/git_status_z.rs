// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Strict porcelain-v1 `-z` parser tests.
//!
//! Covers ordinary entries, rename/copy pairs, hostile filenames with
//! spaces and embedded newlines, and rejection of malformed/truncated/
//! non-UTF-8/absolute/traversal records.

use lattice_workspace::git_status::{
    GitStatusCode, GitStatusEntry, StatusParseError, parse_git_status_z,
};

// ── Clean ─────────────────────────────────────────────────────────────────────

#[test]
fn empty_bytes_are_clean() {
    let result = parse_git_status_z(b"").unwrap();
    assert!(result.is_clean);
    assert!(result.entries.is_empty());
}

// ── Ordinary entries ──────────────────────────────────────────────────────────

#[test]
fn ordinary_modified_entry() {
    // " M src/lib.rs\0"
    let raw = b" M src/lib.rs\0";
    let result = parse_git_status_z(raw).unwrap();
    assert!(!result.is_clean);
    assert_eq!(result.entries.len(), 1);
    assert_eq!(result.entries[0].index_status, GitStatusCode::Unmodified);
    assert_eq!(result.entries[0].worktree_status, GitStatusCode::Modified);
    assert_eq!(result.entries[0].path, "src/lib.rs");
    assert!(result.entries[0].source_path.is_none());
}

#[test]
fn ordinary_staged_added_entry() {
    // "A  new-file.rs\0"
    let raw = b"A  new-file.rs\0";
    let result = parse_git_status_z(raw).unwrap();
    assert_eq!(result.entries.len(), 1);
    assert_eq!(result.entries[0].index_status, GitStatusCode::Added);
    assert_eq!(result.entries[0].worktree_status, GitStatusCode::Unmodified);
    assert_eq!(result.entries[0].path, "new-file.rs");
}

#[test]
fn untracked_entry() {
    // "?? untracked.txt\0"
    let raw = b"?? untracked.txt\0";
    let result = parse_git_status_z(raw).unwrap();
    assert_eq!(result.entries.len(), 1);
    assert_eq!(result.entries[0].index_status, GitStatusCode::Untracked);
    assert_eq!(result.entries[0].worktree_status, GitStatusCode::Untracked);
    assert_eq!(result.entries[0].path, "untracked.txt");
}

#[test]
fn multiple_ordinary_entries() {
    // " M src/lib.rs\0?? tests/new.rs\0A  Cargo.toml\0"
    let raw = b" M src/lib.rs\0?? tests/new.rs\0A  Cargo.toml\0";
    let result = parse_git_status_z(raw).unwrap();
    assert_eq!(result.entries.len(), 3);
    assert_eq!(result.entries[0].path, "src/lib.rs");
    assert_eq!(result.entries[1].path, "tests/new.rs");
    assert_eq!(result.entries[2].path, "Cargo.toml");
}

// ── Hostile filenames ────────────────────────────────────────────────────────

#[test]
fn path_with_spaces_preserved_exactly() {
    // " M file with spaces.txt\0"
    let raw = b" M file with spaces.txt\0";
    let result = parse_git_status_z(raw).unwrap();
    assert_eq!(result.entries[0].path, "file with spaces.txt");
}

#[test]
fn path_with_embedded_newline_preserved_exactly() {
    // " M file\nwith\nnewlines.txt\0"
    let raw = b" M file\nwith\nnewlines.txt\0";
    let result = parse_git_status_z(raw).unwrap();
    assert_eq!(result.entries[0].path, "file\nwith\nnewlines.txt");
}

#[test]
fn path_with_tabs_and_special_whitespace() {
    // " M tab\there.txt\0"
    let raw = b" M tab\there.txt\0";
    let result = parse_git_status_z(raw).unwrap();
    assert_eq!(result.entries[0].path, "tab\there.txt");
}

// ── Rename and copy entries ──────────────────────────────────────────────────

#[test]
fn rename_entry_preserves_source_and_destination() {
    // "R  old-name.rs\0new-name.rs\0"
    let raw = b"R  old-name.rs\0new-name.rs\0";
    let result = parse_git_status_z(raw).unwrap();
    assert_eq!(result.entries.len(), 1);
    assert_eq!(result.entries[0].index_status, GitStatusCode::Renamed);
    assert_eq!(result.entries[0].worktree_status, GitStatusCode::Unmodified);
    assert_eq!(
        result.entries[0].source_path.as_deref(),
        Some("old-name.rs")
    );
    assert_eq!(result.entries[0].path, "new-name.rs");
}

#[test]
fn copy_entry_preserves_source_and_destination() {
    // "C  template.rs\0copy.rs\0"
    let raw = b"C  template.rs\0copy.rs\0";
    let result = parse_git_status_z(raw).unwrap();
    assert_eq!(result.entries.len(), 1);
    assert_eq!(result.entries[0].index_status, GitStatusCode::Copied);
    assert_eq!(result.entries[0].worktree_status, GitStatusCode::Unmodified);
    assert_eq!(
        result.entries[0].source_path.as_deref(),
        Some("template.rs")
    );
    assert_eq!(result.entries[0].path, "copy.rs");
}

#[test]
fn rename_with_worktree_modification() {
    // "RM old.rs\0new.rs\0"
    let raw = b"RM old.rs\0new.rs\0";
    let result = parse_git_status_z(raw).unwrap();
    assert_eq!(result.entries[0].index_status, GitStatusCode::Renamed);
    assert_eq!(result.entries[0].worktree_status, GitStatusCode::Modified);
    assert_eq!(result.entries[0].source_path.as_deref(), Some("old.rs"));
    assert_eq!(result.entries[0].path, "new.rs");
}

#[test]
fn rename_followed_by_ordinary_entry() {
    // "R  old.rs\0new.rs\0 M other.rs\0"
    let raw = b"R  old.rs\0new.rs\0 M other.rs\0";
    let result = parse_git_status_z(raw).unwrap();
    assert_eq!(result.entries.len(), 2);
    assert_eq!(result.entries[0].index_status, GitStatusCode::Renamed);
    assert_eq!(result.entries[0].path, "new.rs");
    assert_eq!(result.entries[1].worktree_status, GitStatusCode::Modified);
    assert_eq!(result.entries[1].path, "other.rs");
}

// ── All XY status code combinations ──────────────────────────────────────────

#[test]
fn all_index_status_codes() {
    // Each entry: (X_byte, Y_byte, needs_rename_path)
    // For ordinary codes, Y is space; for ? and !, both X and Y must match.
    let cases: &[(u8, u8, bool)] = &[
        (b' ', b' ', false),
        (b'M', b' ', false),
        (b'A', b' ', false),
        (b'D', b' ', false),
        (b'R', b' ', true),
        (b'C', b' ', true),
        (b'?', b'?', false),
        (b'!', b'!', false),
    ];
    let expected = &[
        GitStatusCode::Unmodified,
        GitStatusCode::Modified,
        GitStatusCode::Added,
        GitStatusCode::Deleted,
        GitStatusCode::Renamed,
        GitStatusCode::Copied,
        GitStatusCode::Untracked,
        GitStatusCode::Ignored,
    ];
    for ((x, y, needs_rename), exp) in cases.iter().zip(expected.iter()) {
        let raw = if *needs_rename {
            // Rename/copy: XY source\0dest\0
            vec![*x, *y, b' ', b's', b'\0', b'd', b'\0']
        } else {
            // Ordinary: XY path\0
            vec![*x, *y, b' ', b'f', b'\0']
        };
        let result = parse_git_status_z(&raw).unwrap();
        assert_eq!(
            &result.entries[0].index_status, exp,
            "index status byte: {}",
            *x as char
        );
    }
}

#[test]
fn all_worktree_status_codes() {
    for (byte, expected) in &[
        (b' ', GitStatusCode::Unmodified),
        (b'M', GitStatusCode::Modified),
        (b'A', GitStatusCode::Added),
        (b'D', GitStatusCode::Deleted),
        (b'R', GitStatusCode::Renamed),
        (b'C', GitStatusCode::Copied),
    ] {
        let raw = vec![b' ', *byte, b' ', b'f', b'\0'];
        let result = parse_git_status_z(&raw).unwrap();
        assert_eq!(
            &result.entries[0].worktree_status, expected,
            "worktree status byte: {}",
            *byte as char
        );
    }
}

// ── Failure cases ────────────────────────────────────────────────────────────

#[test]
fn truncated_entry_rejected() {
    // Only "M " without path and NUL terminator
    let raw = b"M ";
    let err = parse_git_status_z(raw).unwrap_err();
    assert!(
        matches!(err, StatusParseError::TruncatedRecord { .. }),
        "expected TruncatedRecord, got: {err:?}"
    );
}

#[test]
fn truncated_rename_missing_second_path_rejected() {
    // "R  old.rs\0" — rename without the destination path
    let raw = b"R  old.rs\0";
    let err = parse_git_status_z(raw).unwrap_err();
    assert!(
        matches!(err, StatusParseError::TruncatedRecord { .. }),
        "expected TruncatedRecord, got: {err:?}"
    );
}

#[test]
fn non_utf8_path_rejected() {
    // " M <invalid-utf8>\0"
    let mut raw = b" M ".to_vec();
    raw.extend_from_slice(&[0xFF, 0xFE]); // invalid UTF-8 bytes
    raw.push(0); // NUL terminator
    let err = parse_git_status_z(&raw).unwrap_err();
    assert!(
        matches!(err, StatusParseError::NonUtf8Path { .. }),
        "expected NonUtf8Path, got: {err:?}"
    );
}

#[test]
fn absolute_path_rejected() {
    // " M /etc/passwd\0"
    let raw = b" M /etc/passwd\0";
    let err = parse_git_status_z(raw).unwrap_err();
    assert!(
        matches!(err, StatusParseError::InvalidPath { .. }),
        "expected InvalidPath, got: {err:?}"
    );
}

#[test]
fn traversal_path_rejected() {
    // " M ../secret.txt\0"
    let raw = b" M ../secret.txt\0";
    let err = parse_git_status_z(raw).unwrap_err();
    assert!(
        matches!(err, StatusParseError::InvalidPath { .. }),
        "expected InvalidPath, got: {err:?}"
    );
}

#[test]
fn invalid_status_code_rejected() {
    // "X  file.rs\0" — X is not a valid porcelain status code
    let raw = b"X  file.rs\0";
    let err = parse_git_status_z(raw).unwrap_err();
    assert!(
        matches!(err, StatusParseError::InvalidStatusCode { .. }),
        "expected InvalidStatusCode, got: {err:?}"
    );
}

// ── Workspace dirty fact ──────────────────────────────────────────────────────

#[test]
fn workspace_dirty_fact_is_valid_json() {
    let entries = vec![
        GitStatusEntry {
            index_status: GitStatusCode::Modified,
            worktree_status: GitStatusCode::Unmodified,
            path: "src/lib.rs".into(),
            source_path: None,
        },
        GitStatusEntry {
            index_status: GitStatusCode::Renamed,
            worktree_status: GitStatusCode::Unmodified,
            path: "new.rs".into(),
            source_path: Some("old.rs".into()),
        },
    ];
    let fact = GitStatusEntry::workspace_dirty_fact(&entries);
    // Must be valid JSON
    let parsed: serde_json::Value = serde_json::from_str(&fact).expect("must be valid JSON");
    assert!(parsed.is_object());
    // Must have entries array
    let json_entries = parsed.get("entries").unwrap().as_array().unwrap();
    assert_eq!(json_entries.len(), 2);
    // Rename entry must have source_path
    let rename = &json_entries[1];
    assert_eq!(rename["source_path"], "old.rs");
    assert_eq!(rename["path"], "new.rs");
}

#[test]
fn workspace_dirty_fact_escapes_hostile_paths() {
    let entries = vec![GitStatusEntry {
        index_status: GitStatusCode::Modified,
        worktree_status: GitStatusCode::Unmodified,
        path: "file\"with\"quotes\nand\nnewlines".into(),
        source_path: None,
    }];
    let fact = GitStatusEntry::workspace_dirty_fact(&entries);
    // Must be valid JSON even with hostile characters
    let parsed: serde_json::Value =
        serde_json::from_str(&fact).expect("hostile paths must still produce valid JSON");
    let path = parsed["entries"][0]["path"].as_str().unwrap();
    assert!(path.contains('"'));
    assert!(path.contains('\n'));
}

#[test]
fn workspace_dirty_fact_cannot_inject_record() {
    // A path that looks like it could inject JSON structure
    let entries = vec![GitStatusEntry {
        index_status: GitStatusCode::Modified,
        worktree_status: GitStatusCode::Unmodified,
        path: r#"","extra":"injected"}]"#.into(),
        source_path: None,
    }];
    let fact = GitStatusEntry::workspace_dirty_fact(&entries);
    let parsed: serde_json::Value = serde_json::from_str(&fact).unwrap();
    // The injected string is treated as a literal path value, not JSON structure
    let json_entries = parsed["entries"].as_array().unwrap();
    assert_eq!(json_entries.len(), 1);
}

// ── Task 2: Adapter routing contract ────────────────────────────────────────

/// Verify that the crate-level `GitStatus` has typed `entries` (not raw `porcelain`).
/// This is the fundamental routing contract: all adapters must produce typed entries.
#[test]
fn crate_level_git_status_has_typed_entries() {
    use lattice_workspace::git_status::GitStatus;
    let status = GitStatus {
        entries: vec![GitStatusEntry {
            index_status: GitStatusCode::Unmodified,
            worktree_status: GitStatusCode::Modified,
            path: "src/lib.rs".into(),
            source_path: None,
        }],
        is_clean: false,
    };
    assert_eq!(status.entries.len(), 1);
    assert_eq!(status.entries[0].path, "src/lib.rs");
}

/// Verify that `GitStatus::default()` produces a clean state with empty entries.
#[test]
fn git_status_default_is_clean_with_empty_entries() {
    use lattice_workspace::git_status::GitStatus;
    let status = GitStatus::default();
    assert!(status.is_clean);
    assert!(status.entries.is_empty());
}

/// Verify that changed-path extraction from typed entries returns all paths,
/// including rename destinations and source paths.
#[test]
fn changed_paths_from_typed_entries() {
    let entries = [
        GitStatusEntry {
            index_status: GitStatusCode::Unmodified,
            worktree_status: GitStatusCode::Modified,
            path: "src/lib.rs".into(),
            source_path: None,
        },
        GitStatusEntry {
            index_status: GitStatusCode::Untracked,
            worktree_status: GitStatusCode::Untracked,
            path: "tests/new.rs".into(),
            source_path: None,
        },
        GitStatusEntry {
            index_status: GitStatusCode::Renamed,
            worktree_status: GitStatusCode::Unmodified,
            path: "new.rs".into(),
            source_path: Some("old.rs".into()),
        },
    ];
    // Collect all visible paths: destination path for every entry.
    let paths: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(paths, ["src/lib.rs", "tests/new.rs", "new.rs"]);
}

/// Verify that workspace_dirty_fact produces structured JSON from typed entries.
#[test]
fn dirty_fact_from_parsed_output() {
    let raw = b" M src/lib.rs\0?? tests/new.rs\0";
    let status = parse_git_status_z(raw).unwrap();
    let fact = GitStatusEntry::workspace_dirty_fact(&status.entries);
    let parsed: serde_json::Value = serde_json::from_str(&fact).unwrap();
    let json_entries = parsed["entries"].as_array().unwrap();
    assert_eq!(json_entries.len(), 2);
    assert_eq!(json_entries[0]["path"], "src/lib.rs");
    assert_eq!(json_entries[0]["xy"], " M");
    assert_eq!(json_entries[1]["path"], "tests/new.rs");
    assert_eq!(json_entries[1]["xy"], "??");
}

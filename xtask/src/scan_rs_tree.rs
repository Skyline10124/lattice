// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::path::Path;

use walkdir::WalkDir;

/// One matched source line, formatted as `relative/path.rs:line_no:line_text`.
pub type LineHit = String;

pub fn is_excluded_walk_dir(path: &Path) -> bool {
    path.file_name()
        .and_then(|s| s.to_str())
        .is_some_and(|name| name == "target" || name == ".planning")
}

/// Walk `walk_root`, visit `.rs` files, emit hits where `line_matches` is true.
pub fn collect_rs_line_hits(
    walk_root: &Path,
    repo_root: &Path,
    skip_file: impl Fn(&Path) -> bool,
    line_matches: impl Fn(&str) -> bool,
) -> anyhow::Result<Vec<LineHit>> {
    let mut hits = Vec::new();
    for entry in WalkDir::new(walk_root)
        .into_iter()
        .filter_entry(|e| !is_excluded_walk_dir(e.path()))
    {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("rs") {
            continue;
        }
        if skip_file(path) {
            continue;
        }
        let content = std::fs::read_to_string(path)?;
        for (line_no, line) in content.lines().enumerate() {
            if line_matches(line) {
                hits.push(format_line_hit(repo_root, path, line_no + 1, line));
            }
        }
    }
    Ok(hits)
}

pub fn format_line_hit(repo_root: &Path, path: &Path, line_no: usize, line: &str) -> LineHit {
    let rel = path.strip_prefix(repo_root).unwrap_or(path);
    format!("{}:{}:{line}", rel.display(), line_no)
}

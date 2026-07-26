// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Shared helpers for lattice-api integration tests.
//!
//! #37 removed the Upper workspace override surface; these helpers now only
//! provide the git-repo bootstrap used by Direct-Local test fixtures.
use std::path::Path;

use lattice_session::test_utils::init_fixture_repo;

pub fn init_git_repo(root: &Path) {
    // Capture any pre-written project files into the initial commit so HEAD is
    // born and the worktree baseline matches the fixture.
    let mut files: Vec<(String, String)> = Vec::new();
    collect_files(root, root, &mut files);
    let owned: Vec<(String, String)> = files;
    let refs: Vec<(&str, &str)> = owned
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_str()))
        .collect();
    let _ = init_fixture_repo(root, &refs);
}

fn collect_files(root: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if entry.file_name() == ".git" {
            continue;
        }
        if path.is_dir() {
            collect_files(root, &path, out);
        } else if path.is_file() {
            let rel = path
                .strip_prefix(root)
                .expect("under root")
                .to_string_lossy()
                .replace('\\', "/");
            let content = std::fs::read_to_string(&path).unwrap_or_default();
            out.push((rel, content));
        }
    }
}

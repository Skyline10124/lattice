# uninode a1 — Offline Context-Compiler Spike Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a new `lattice-uninode` crate that constructs a static File-graph from a git repo, compiles a per-query context subset via Seed→Expand→Rank→Pack, and proves (or disproves) on a git-commit-replay benchmark that this beats naive whole-file context at equal token budget — entirely offline, touching no v1.5 turn-execution code.

**Architecture:** A pure-synchronous library + one eval binary. The graph has only `File` nodes (no runtime `Operation` nodes — that needs fanotify, blocked on Phase 34) with two offline edge sources: `Sibling` (same directory) and `CoChange` (git co-commit history). The compiler is four pure functions. The benchmark replays historical commits: commit message = query, the commit's changed-file set = ground truth, with leave-one-out so co-change never sees the held-out commit. Success is measured as recall@token-budget and token efficiency vs a naive baseline.

**Tech Stack:** Rust (edition 2024), `git2` 0.20 (history + tracked files), `blake3` (node ids + content hash), `serde`/`serde_json` (JSONL graph dump), `clap` (eval CLI), `tempfile` (test repos). No async, no sandbox, no KVM.

---

## Scope

**In scope (M1–M3):** the `lattice-uninode` crate — graph build, compiler, and offline eval harness. Self-contained, runs on any git repo, CI-friendly (no KVM).

**Out of scope (separate later plan, "a2"/"M4"):** wiring the compiler into `lattice-api`'s turn at `crates/lattice-agent/src/agent_loop/mod.rs:1036` (`build_messages`), fanotify/runtime `Operation` edges, learned edge weights, CEP/promote, AST/Symbol `code` bundle. This plan deliberately stops at "is the graph worth it?" — answered before any main-line code is touched.

## File Structure

```
crates/lattice-uninode/
├── Cargo.toml                 # new crate manifest (workspace deps)
├── src/
│   ├── lib.rs                 # Error, Result, module decls, #[cfg(test)] testing helper
│   ├── graph.rs               # NodeKind, EdgeKind, Node, Edge, Graph, node_id, to_jsonl
│   ├── scanner.rs             # scan_repo: git-tracked files → File nodes + Sibling edges
│   ├── cochange.rs            # add_cochange_edges: git log → CoChange edges (leave-one-out)
│   ├── compiler.rs            # seed, expand, rank, pack, Fidelity, Selected, est_tokens
│   ├── eval.rs                # recall, precision, naive_select, replay case extraction
│   └── bin/
│       └── eval.rs            # clap binary: walk N commits, run naive vs uninode, print table
└── (root) Cargo.toml          # add "crates/lattice-uninode" to [workspace] members
```

Each `src/*.rs` has one responsibility and is independently testable with `#[cfg(test)]` unit tests using the shared `crate::testing::TestRepo` helper.

---

## Task 1: Crate scaffold + test helper

**Files:**
- Modify: `Cargo.toml` (workspace root, `members` array)
- Create: `crates/lattice-uninode/Cargo.toml`
- Create: `crates/lattice-uninode/src/lib.rs`

- [ ] **Step 1: Register the crate in the workspace**

Modify `Cargo.toml` (root) — add the member line to the `members` array (after the `lattice-workspace` line):

```toml
    "crates/lattice-workspace",
    "crates/lattice-uninode",
]
```

- [ ] **Step 2: Create the crate manifest**

Create `crates/lattice-uninode/Cargo.toml`:

```toml
[package]
name = "lattice-uninode"
version.workspace = true
license.workspace = true
edition.workspace = true
authors.workspace = true
description = "Offline node-graph context compiler spike (uninode a1): static File graph + Seed-Expand-Rank-Pack + git-replay evaluation"

[dependencies]
blake3 = { workspace = true }
git2 = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
thiserror = { workspace = true }
clap = { workspace = true }

[dev-dependencies]
tempfile = { workspace = true }

[[bin]]
name = "uninode-eval"
path = "src/bin/eval.rs"
```

- [ ] **Step 3: Create `lib.rs` with Error, Result, module decls, and the test helper**

Create `crates/lattice-uninode/src/lib.rs`:

```rust
// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! uninode a1 — offline static File-graph + context compiler spike.
//!
//! Pure synchronous: builds a graph from a git repo (`scanner` + `cochange`),
//! compiles a per-query subset (`compiler`), and scores it on a commit-replay
//! benchmark (`eval`). No async, no sandbox; touches no turn-execution code.

pub mod compiler;
pub mod cochange;
pub mod eval;
pub mod graph;
pub mod scanner;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("git error: {0}")]
    Git(#[from] git2::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("repo has no workdir (bare repo)")]
    NoWorkdir,
}

pub type Result<T> = std::result::Result<T, Error>;

/// Test-only helper: build throwaway git repos via the `git` CLI.
#[cfg(test)]
pub mod testing {
    use std::path::Path;
    use std::process::Command;
    use tempfile::TempDir;

    pub struct TestRepo {
        pub dir: TempDir,
    }

    impl TestRepo {
        pub fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            git(dir.path(), &["init", "-q", "-b", "main"]);
            git(dir.path(), &["config", "user.email", "t@t"]);
            git(dir.path(), &["config", "user.name", "t"]);
            TestRepo { dir }
        }

        pub fn path(&self) -> &Path {
            self.dir.path()
        }

        pub fn write(&self, rel: &str, content: &str) {
            let full = self.dir.path().join(rel);
            if let Some(p) = full.parent() {
                std::fs::create_dir_all(p).unwrap();
            }
            std::fs::write(full, content).unwrap();
        }

        /// Stage the given files and commit them with `msg`.
        pub fn commit(&self, msg: &str, files: &[&str]) {
            for f in files {
                git(self.path(), &["add", f]);
            }
            git(self.path(), &["commit", "-q", "-m", msg]);
        }

        pub fn open(&self) -> git2::Repository {
            git2::Repository::open(self.path()).unwrap()
        }
    }

    pub fn git(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_TERMINAL_PROMPT", "0")
            .status()
            .expect("git must be installed")
            .success();
        assert!(ok, "git {args:?} failed");
    }
}
```

- [ ] **Step 4: Create empty module files so the crate compiles**

Create each of these with only the license header (they get filled by later tasks):

`crates/lattice-uninode/src/graph.rs`, `scanner.rs`, `cochange.rs`, `compiler.rs`, `eval.rs`:

```rust
// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0
```

Create `crates/lattice-uninode/src/bin/eval.rs`:

```rust
// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

fn main() {}
```

- [ ] **Step 5: Verify it builds, then commit**

Run: `cargo build -p lattice-uninode`
Expected: compiles clean (empty modules, empty bin).

```bash
git add Cargo.toml crates/lattice-uninode
git commit -m "feat(uninode): scaffold lattice-uninode crate for a1 spike"
```

---

## Task 2: Graph types + node id

**Files:**
- Modify: `crates/lattice-uninode/src/graph.rs`

- [ ] **Step 1: Write the failing test**

Append to `crates/lattice-uninode/src/graph.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_id_is_stable_and_path_sensitive() {
        let a = node_id(NodeKind::File, "src/main.rs");
        let b = node_id(NodeKind::File, "src/main.rs");
        let c = node_id(NodeKind::File, "src/lib.rs");
        assert_eq!(a, b, "same kind+path must yield same id");
        assert_ne!(a, c, "different path must yield different id");
        assert_eq!(a.len(), 64, "blake3 hex is 64 chars");
    }

    #[test]
    fn add_node_and_edge_track_out_degree() {
        let mut g = Graph::new();
        let id1 = node_id(NodeKind::File, "a.rs");
        let id2 = node_id(NodeKind::File, "b.rs");
        g.add_node(Node {
            id: id1.clone(),
            kind: NodeKind::File,
            path: "a.rs".into(),
            size: 10,
            hash: "h".into(),
        });
        g.add_edge(&id1, Edge { target: id2.clone(), kind: EdgeKind::Sibling, weight: 0.3 });
        assert_eq!(g.out_degree(&id1), 1);
        assert_eq!(g.out_degree(&id2), 0);
        assert_eq!(g.neighbors(&id1)[0].target, id2);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p lattice-uninode graph::`
Expected: FAIL — `cannot find type Graph/Node/Edge/NodeKind/EdgeKind`, `cannot find function node_id`.

- [ ] **Step 3: Write minimal implementation**

Insert above the `#[cfg(test)]` block in `crates/lattice-uninode/src/graph.rs`:

```rust
use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeKind {
    File,
}

impl NodeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            NodeKind::File => "File",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EdgeKind {
    /// Same-directory structural proximity (a1 approximation of `Contain`).
    Sibling,
    /// Files changed together in git history (offline source of `RelatedTo`).
    CoChange,
}

impl EdgeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EdgeKind::Sibling => "Sibling",
            EdgeKind::CoChange => "CoChange",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    pub kind: NodeKind,
    pub path: String,
    pub size: u64,
    pub hash: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Edge {
    pub target: String,
    pub kind: EdgeKind,
    pub weight: f64,
}

/// Stable node address: `blake3(kind || 0x00 || path)`, content-independent.
pub fn node_id(kind: NodeKind, path: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(kind.as_str().as_bytes());
    hasher.update(b"\0");
    hasher.update(path.as_bytes());
    hasher.finalize().to_hex().to_string()
}

#[derive(Clone, Debug, Default)]
pub struct Graph {
    pub nodes: HashMap<String, Node>,
    pub edges: HashMap<String, Vec<Edge>>,
}

impl Graph {
    pub fn new() -> Self {
        Graph::default()
    }

    pub fn add_node(&mut self, node: Node) {
        self.nodes.insert(node.id.clone(), node);
    }

    pub fn add_edge(&mut self, source: &str, edge: Edge) {
        self.edges.entry(source.to_string()).or_default().push(edge);
    }

    pub fn out_degree(&self, id: &str) -> usize {
        self.edges.get(id).map_or(0, |e| e.len())
    }

    pub fn neighbors(&self, id: &str) -> &[Edge] {
        self.edges.get(id).map_or(&[], |v| v.as_slice())
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p lattice-uninode graph::`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/lattice-uninode/src/graph.rs
git commit -m "feat(uninode): graph types + stable node id"
```

---

## Task 3: Graph JSONL dump

**Files:**
- Modify: `crates/lattice-uninode/src/graph.rs`

- [ ] **Step 1: Write the failing test**

Add inside the existing `mod tests` in `crates/lattice-uninode/src/graph.rs`:

```rust
    #[test]
    fn to_jsonl_emits_one_sorted_line_per_node_with_edges() {
        let mut g = Graph::new();
        let id1 = node_id(NodeKind::File, "a.rs");
        let id2 = node_id(NodeKind::File, "b.rs");
        for (id, path) in [(&id1, "a.rs"), (&id2, "b.rs")] {
            g.add_node(Node {
                id: id.clone(),
                kind: NodeKind::File,
                path: path.into(),
                size: 1,
                hash: "h".into(),
            });
        }
        g.add_edge(&id1, Edge { target: id2.clone(), kind: EdgeKind::Sibling, weight: 0.3 });

        let jsonl = g.to_jsonl();
        let lines: Vec<&str> = jsonl.lines().collect();
        assert_eq!(lines.len(), 2, "one line per node");
        // Each line is valid JSON with the expected fields.
        let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first["kind"], "File");
        assert_eq!(first["bundle"], "base");
        assert!(first["edges"].is_array());
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p lattice-uninode graph::tests::to_jsonl`
Expected: FAIL — `no method named to_jsonl`.

- [ ] **Step 3: Write minimal implementation**

Add an `impl Graph` method (place after the existing `neighbors` method, inside the same `impl Graph` block or a new one):

```rust
impl Graph {
    /// Serialize as JSONL: one line per node, sorted by id, edges inlined.
    /// Mirrors the `.uninode/nodes/_.jsonl` shape from the spec (single-file a1
    /// variant; per-directory grouping is deferred to the full implementation).
    pub fn to_jsonl(&self) -> String {
        #[derive(serde::Serialize)]
        struct NodeLine<'a> {
            id: &'a str,
            bundle: &'static str,
            kind: &'static str,
            path: &'a str,
            hash: &'a str,
            size: u64,
            edges: &'a [Edge],
        }

        let mut ids: Vec<&String> = self.nodes.keys().collect();
        ids.sort();

        let mut out = String::new();
        let empty: Vec<Edge> = Vec::new();
        for id in ids {
            let node = &self.nodes[id];
            let edges = self.edges.get(id).unwrap_or(&empty);
            let line = NodeLine {
                id: &node.id,
                bundle: "base",
                kind: node.kind.as_str(),
                path: &node.path,
                hash: &node.hash,
                size: node.size,
                edges,
            };
            out.push_str(&serde_json::to_string(&line).expect("node line serializes"));
            out.push('\n');
        }
        out
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p lattice-uninode graph::`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/lattice-uninode/src/graph.rs
git commit -m "feat(uninode): JSONL graph dump"
```

---

## Task 4: Scanner — File nodes + Sibling edges

**Files:**
- Modify: `crates/lattice-uninode/src/scanner.rs`

- [ ] **Step 1: Write the failing test**

Append to `crates/lattice-uninode/src/scanner.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{node_id, EdgeKind, NodeKind};
    use crate::testing::TestRepo;

    #[test]
    fn scans_tracked_files_into_file_nodes() {
        let repo = TestRepo::new();
        repo.write("src/main.rs", "fn main() {}\n");
        repo.write("src/lib.rs", "pub fn x() {}\n");
        repo.write("README.md", "# hi\n");
        repo.commit("init", &["src/main.rs", "src/lib.rs", "README.md"]);

        let g = scan_repo(&repo.open()).unwrap();
        assert_eq!(g.nodes.len(), 3);
        let main_id = node_id(NodeKind::File, "src/main.rs");
        let node = &g.nodes[&main_id];
        assert_eq!(node.path, "src/main.rs");
        assert_eq!(node.size, 12);
        assert_eq!(node.hash.len(), 64);
    }

    #[test]
    fn same_directory_files_get_sibling_edges() {
        let repo = TestRepo::new();
        repo.write("src/a.rs", "a\n");
        repo.write("src/b.rs", "b\n");
        repo.write("docs/c.md", "c\n");
        repo.commit("init", &["src/a.rs", "src/b.rs", "docs/c.md"]);

        let g = scan_repo(&repo.open()).unwrap();
        let a = node_id(NodeKind::File, "src/a.rs");
        let b = node_id(NodeKind::File, "src/b.rs");
        let c = node_id(NodeKind::File, "docs/c.md");
        // a and b share src/ → mutual Sibling edge; c is alone in docs/.
        assert!(g.neighbors(&a).iter().any(|e| e.target == b && e.kind == EdgeKind::Sibling));
        assert!(g.neighbors(&b).iter().any(|e| e.target == a && e.kind == EdgeKind::Sibling));
        assert_eq!(g.out_degree(&c), 0, "lone file has no sibling");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p lattice-uninode scanner::`
Expected: FAIL — `cannot find function scan_repo`.

- [ ] **Step 3: Write minimal implementation**

Insert above the `#[cfg(test)]` block in `crates/lattice-uninode/src/scanner.rs`:

```rust
use std::collections::HashMap;

use crate::graph::{node_id, Edge, EdgeKind, Graph, Node, NodeKind};
use crate::{Error, Result};

/// Weight of a same-directory structural edge.
pub const SIBLING_WEIGHT: f64 = 0.3;

/// Build a File-only graph from a repo's git-tracked files plus Sibling edges.
pub fn scan_repo(repo: &git2::Repository) -> Result<Graph> {
    let workdir = repo.workdir().ok_or(Error::NoWorkdir)?.to_path_buf();
    let mut graph = Graph::new();
    let mut by_dir: HashMap<String, Vec<String>> = HashMap::new();

    let index = repo.index()?;
    for entry in index.iter() {
        let path = String::from_utf8_lossy(&entry.path).to_string();
        let full = workdir.join(&path);
        let Ok(bytes) = std::fs::read(&full) else {
            continue; // tracked but absent from workdir; skip in a1
        };
        let id = node_id(NodeKind::File, &path);
        graph.add_node(Node {
            id: id.clone(),
            kind: NodeKind::File,
            path: path.clone(),
            size: bytes.len() as u64,
            hash: blake3::hash(&bytes).to_hex().to_string(),
        });
        let dir = match path.rsplit_once('/') {
            Some((d, _)) => d.to_string(),
            None => String::new(),
        };
        by_dir.entry(dir).or_default().push(id);
    }

    for ids in by_dir.values() {
        for a in ids {
            for b in ids {
                if a != b {
                    graph.add_edge(
                        a,
                        Edge { target: b.clone(), kind: EdgeKind::Sibling, weight: SIBLING_WEIGHT },
                    );
                }
            }
        }
    }

    Ok(graph)
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p lattice-uninode scanner::`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/lattice-uninode/src/scanner.rs
git commit -m "feat(uninode): scanner — File nodes + Sibling edges"
```

---

## Task 5: Co-change edges (leave-one-out)

**Files:**
- Modify: `crates/lattice-uninode/src/cochange.rs`

- [ ] **Step 1: Write the failing test**

Append to `crates/lattice-uninode/src/cochange.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{node_id, EdgeKind, NodeKind};
    use crate::scanner::scan_repo;
    use crate::testing::TestRepo;

    #[test]
    fn files_changed_together_get_a_cochange_edge() {
        let repo = TestRepo::new();
        // Three commits where a.rs and b.rs change together twice.
        repo.write("a.rs", "1");
        repo.write("b.rs", "1");
        repo.commit("c1", &["a.rs", "b.rs"]);
        repo.write("a.rs", "2");
        repo.write("b.rs", "2");
        repo.commit("c2", &["a.rs", "b.rs"]);
        repo.write("c.rs", "1");
        repo.commit("c3", &["c.rs"]);

        let r = repo.open();
        let mut g = scan_repo(&r).unwrap();
        add_cochange_edges(&mut g, &r, None, 100).unwrap();

        let a = node_id(NodeKind::File, "a.rs");
        let b = node_id(NodeKind::File, "b.rs");
        let edge = g
            .neighbors(&a)
            .iter()
            .find(|e| e.target == b && e.kind == EdgeKind::CoChange)
            .expect("a–b cochange edge exists");
        // Changed together in 2 of 2 of a's commits → weight 1.0.
        assert!((edge.weight - 1.0).abs() < 1e-9, "weight was {}", edge.weight);
    }

    #[test]
    fn leave_one_out_excludes_the_held_out_commit() {
        let repo = TestRepo::new();
        repo.write("a.rs", "1");
        repo.write("b.rs", "1");
        repo.commit("c1", &["a.rs", "b.rs"]);
        repo.write("a.rs", "2");
        repo.write("b.rs", "2");
        repo.commit("c2 held-out", &["a.rs", "b.rs"]);

        let r = repo.open();
        // Use only history *before* HEAD (i.e. starting at HEAD's parent).
        let head = r.head().unwrap().peel_to_commit().unwrap();
        let parent = head.parent(0).unwrap().id();

        let mut g = scan_repo(&r).unwrap();
        add_cochange_edges(&mut g, &r, Some(parent), 100).unwrap();

        let a = node_id(NodeKind::File, "a.rs");
        let b = node_id(NodeKind::File, "b.rs");
        // Only c1 is visible → still one co-change, weight 1.0, but proves the
        // walk started at `parent` (c2 not double-counted: weight stays 1.0).
        let edge = g.neighbors(&a).iter().find(|e| e.target == b).unwrap();
        assert!((edge.weight - 1.0).abs() < 1e-9);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p lattice-uninode cochange::`
Expected: FAIL — `cannot find function add_cochange_edges`.

- [ ] **Step 3: Write minimal implementation**

Insert above the `#[cfg(test)]` block in `crates/lattice-uninode/src/cochange.rs`:

```rust
use std::collections::HashMap;

use crate::graph::{node_id, Edge, EdgeKind, Graph, NodeKind};
use crate::Result;

/// Add `CoChange` edges from git history. `start` selects the walk root: `None`
/// walks from HEAD; `Some(oid)` walks from that commit and older (leave-one-out
/// for the eval harness). Only the most recent `max_commits` are considered.
/// Merge commits and the root commit (parent_count != 1) are skipped.
pub fn add_cochange_edges(
    graph: &mut Graph,
    repo: &git2::Repository,
    start: Option<git2::Oid>,
    max_commits: usize,
) -> Result<()> {
    let mut walk = repo.revwalk()?;
    match start {
        Some(oid) => walk.push(oid)?,
        None => walk.push_head()?,
    }

    let mut pair_counts: HashMap<(String, String), u32> = HashMap::new();
    let mut file_counts: HashMap<String, u32> = HashMap::new();

    for (i, oid) in walk.enumerate() {
        if i >= max_commits {
            break;
        }
        let commit = repo.find_commit(oid?)?;
        if commit.parent_count() != 1 {
            continue;
        }
        let parent = commit.parent(0)?;
        let diff =
            repo.diff_tree_to_tree(Some(&parent.tree()?), Some(&commit.tree()?), None)?;

        let mut files: Vec<String> = Vec::new();
        diff.foreach(
            &mut |delta, _| {
                if let Some(p) = delta.new_file().path() {
                    files.push(p.to_string_lossy().to_string());
                }
                true
            },
            None,
            None,
            None,
        )?;
        files.sort();
        files.dedup();

        for f in &files {
            *file_counts.entry(f.clone()).or_default() += 1;
        }
        for a in 0..files.len() {
            for b in (a + 1)..files.len() {
                *pair_counts.entry((files[a].clone(), files[b].clone())).or_default() += 1;
            }
        }
    }

    for ((fa, fb), co) in pair_counts {
        let ca = *file_counts.get(&fa).unwrap_or(&1);
        let cb = *file_counts.get(&fb).unwrap_or(&1);
        // Confidence-style normalization: co-occurrences / min individual count.
        let weight = co as f64 / ca.min(cb) as f64;
        let ida = node_id(NodeKind::File, &fa);
        let idb = node_id(NodeKind::File, &fb);
        if graph.nodes.contains_key(&ida) && graph.nodes.contains_key(&idb) {
            graph.add_edge(&ida, Edge { target: idb.clone(), kind: EdgeKind::CoChange, weight });
            graph.add_edge(&idb, Edge { target: ida.clone(), kind: EdgeKind::CoChange, weight });
        }
    }

    Ok(())
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p lattice-uninode cochange::`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/lattice-uninode/src/cochange.rs
git commit -m "feat(uninode): co-change edges from git history with leave-one-out"
```

---

## Task 6: Compiler — Seed

**Files:**
- Modify: `crates/lattice-uninode/src/compiler.rs`

- [ ] **Step 1: Write the failing test**

Append to `crates/lattice-uninode/src/compiler.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{node_id, Graph, Node, NodeKind};

    fn file_node(g: &mut Graph, path: &str) -> String {
        let id = node_id(NodeKind::File, path);
        g.add_node(Node {
            id: id.clone(),
            kind: NodeKind::File,
            path: path.into(),
            size: 1,
            hash: "h".into(),
        });
        id
    }

    #[test]
    fn seed_matches_filename_higher_than_path() {
        let mut g = Graph::new();
        let auth = file_node(&mut g, "src/auth.rs");
        let other = file_node(&mut g, "src/session.rs");
        let nested = file_node(&mut g, "auth/helpers.rs");

        let seeds = seed(&g, "fix the auth bug");
        // filename "auth.rs" contains "auth" → score 2.0.
        assert_eq!(seeds[&auth], 2.0);
        // "auth/helpers.rs": filename "helpers.rs" no match, path contains "auth" → 1.0.
        assert_eq!(seeds[&nested], 1.0);
        // session.rs: no term match → absent.
        assert!(!seeds.contains_key(&other));
    }

    #[test]
    fn seed_ignores_short_tokens() {
        let mut g = Graph::new();
        let _ = file_node(&mut g, "a.rs");
        // "a" and "is" are < 3 chars and must be dropped.
        let seeds = seed(&g, "a is");
        assert!(seeds.is_empty());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p lattice-uninode compiler::tests::seed`
Expected: FAIL — `cannot find function seed`.

- [ ] **Step 3: Write minimal implementation**

Insert above the `#[cfg(test)]` block in `crates/lattice-uninode/src/compiler.rs`:

```rust
use std::collections::HashMap;

use crate::graph::Graph;

/// Stage 1 — Seed. Lexical match of query terms against node filename (weight 2)
/// and full path (weight 1). High precision, low recall (recall is Stage 2's job).
pub fn seed(graph: &Graph, query: &str) -> HashMap<String, f64> {
    let terms: Vec<String> = query
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() >= 3)
        .map(|t| t.to_string())
        .collect();

    let mut seeds = HashMap::new();
    for node in graph.nodes.values() {
        let path = node.path.to_lowercase();
        let fname = node.path.rsplit('/').next().unwrap_or(&node.path).to_lowercase();
        let mut score = 0.0;
        for t in &terms {
            if fname.contains(t.as_str()) {
                score += 2.0;
            } else if path.contains(t.as_str()) {
                score += 1.0;
            }
        }
        if score > 0.0 {
            seeds.insert(node.id.clone(), score);
        }
    }
    seeds
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p lattice-uninode compiler::tests::seed`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/lattice-uninode/src/compiler.rs
git commit -m "feat(uninode): compiler seed stage"
```

---

## Task 7: Compiler — Expand (fan-effect spreading)

**Files:**
- Modify: `crates/lattice-uninode/src/compiler.rs`

- [ ] **Step 1: Write the failing test**

Add inside the existing `mod tests` in `crates/lattice-uninode/src/compiler.rs`:

```rust
    use crate::graph::{Edge, EdgeKind};

    #[test]
    fn expand_spreads_activation_through_edges_with_fan_normalization() {
        let mut g = Graph::new();
        let a = file_node(&mut g, "a.rs");
        let b = file_node(&mut g, "b.rs");
        let c = file_node(&mut g, "c.rs");
        // a → b (weight 1.0), a → c (weight 1.0). a has out-degree 2 → fan=2.
        g.add_edge(&a, Edge { target: b.clone(), kind: EdgeKind::CoChange, weight: 1.0 });
        g.add_edge(&a, Edge { target: c.clone(), kind: EdgeKind::CoChange, weight: 1.0 });

        let mut seeds = std::collections::HashMap::new();
        seeds.insert(a.clone(), 1.0);

        let scores = expand(&g, &seeds, 1, 1.0);
        // Seed retains its score.
        assert!((scores[&a] - 1.0).abs() < 1e-9);
        // b receives 1.0 (seed) * 1.0 (weight) / 2 (fan) * 1.0 (decay) = 0.5.
        assert!((scores[&b] - 0.5).abs() < 1e-9, "b was {}", scores[&b]);
        assert!((scores[&c] - 0.5).abs() < 1e-9);
    }

    #[test]
    fn expand_zero_hops_returns_seeds_unchanged() {
        let mut g = Graph::new();
        let a = file_node(&mut g, "a.rs");
        let mut seeds = std::collections::HashMap::new();
        seeds.insert(a.clone(), 3.0);
        let scores = expand(&g, &seeds, 0, 0.7);
        assert_eq!(scores.len(), 1);
        assert_eq!(scores[&a], 3.0);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p lattice-uninode compiler::tests::expand`
Expected: FAIL — `cannot find function expand`.

- [ ] **Step 3: Write minimal implementation**

Add to `crates/lattice-uninode/src/compiler.rs` (after `seed`):

```rust
/// Stage 2 — Expand. Best-first activation spreading from seeds along edges,
/// with **fan effect** (ACT-R): a source's contribution is divided by its
/// out-degree, so high-fan-out hubs don't flood the frontier. Iterates `hops`
/// rounds; each hop's contribution is scaled by `decay`.
pub fn expand(
    graph: &Graph,
    seeds: &HashMap<String, f64>,
    hops: usize,
    decay: f64,
) -> HashMap<String, f64> {
    let mut scores = seeds.clone();
    let mut frontier = seeds.clone();

    for _ in 0..hops {
        let mut next: HashMap<String, f64> = HashMap::new();
        for (j, aj) in &frontier {
            let fan = graph.out_degree(j).max(1) as f64;
            for edge in graph.neighbors(j) {
                let contrib = aj * edge.weight / fan * decay;
                *next.entry(edge.target.clone()).or_default() += contrib;
            }
        }
        for (k, v) in &next {
            *scores.entry(k.clone()).or_default() += v;
        }
        frontier = next;
    }
    scores
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p lattice-uninode compiler::tests::expand`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/lattice-uninode/src/compiler.rs
git commit -m "feat(uninode): compiler expand stage with fan-effect"
```

---

## Task 8: Compiler — Rank (hub down-weight)

**Files:**
- Modify: `crates/lattice-uninode/src/compiler.rs`

- [ ] **Step 1: Write the failing test**

Add inside the existing `mod tests`:

```rust
    #[test]
    fn rank_downweights_high_degree_hubs() {
        let mut g = Graph::new();
        let lonely = file_node(&mut g, "lonely.rs");
        let hub = file_node(&mut g, "hub.rs");
        // Give hub many out-edges so its degree is high.
        for i in 0..10 {
            let t = file_node(&mut g, &format!("dep{i}.rs"));
            g.add_edge(&hub, Edge { target: t, kind: EdgeKind::CoChange, weight: 1.0 });
        }

        let mut scores = std::collections::HashMap::new();
        scores.insert(lonely.clone(), 1.0);
        scores.insert(hub.clone(), 1.0);

        let ranked = rank(&g, &scores, 1.0);
        let pos = |id: &str| ranked.iter().position(|(i, _)| i == id).unwrap();
        // Equal raw score, but hub is connected to everything → ranked lower.
        assert!(pos(&lonely) < pos(&hub), "lonely should outrank the hub");
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p lattice-uninode compiler::tests::rank`
Expected: FAIL — `cannot find function rank`.

- [ ] **Step 3: Write minimal implementation**

Add to `crates/lattice-uninode/src/compiler.rs`:

```rust
/// Stage 3 — Rank. Multiplies each node's spread score by an inverse-degree
/// factor so god-nodes (ubiquitous, low-specificity) are down-weighted:
/// `final = score / (1 + beta * ln(1 + out_degree))`. Sorted descending.
pub fn rank(graph: &Graph, scores: &HashMap<String, f64>, beta: f64) -> Vec<(String, f64)> {
    let mut ranked: Vec<(String, f64)> = scores
        .iter()
        .map(|(id, s)| {
            let deg = graph.out_degree(id) as f64;
            let factor = 1.0 / (1.0 + beta * (1.0 + deg).ln());
            (id.clone(), s * factor)
        })
        .collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    ranked
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p lattice-uninode compiler::tests::rank`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/lattice-uninode/src/compiler.rs
git commit -m "feat(uninode): compiler rank stage with idf hub down-weight"
```

---

## Task 9: Compiler — Pack (budget + fidelity downgrade)

**Files:**
- Modify: `crates/lattice-uninode/src/compiler.rs`

- [ ] **Step 1: Write the failing test**

Add inside the existing `mod tests`:

```rust
    #[test]
    fn est_tokens_is_chars_over_four() {
        assert_eq!(est_tokens(""), 1); // floor at 1
        assert_eq!(est_tokens("12345678"), 2); // 8 chars / 4
    }

    #[test]
    fn pack_downgrades_fidelity_instead_of_dropping() {
        let dir = tempfile::tempdir().unwrap();
        // A big file (~250 tokens) and a tiny one.
        let big = "x".repeat(1000) + "\n";
        std::fs::write(dir.path().join("big.rs"), &big).unwrap();
        std::fs::write(dir.path().join("small.rs"), "ok\n").unwrap();

        let mut g = Graph::new();
        let big_id = file_node(&mut g, "big.rs");
        let small_id = file_node(&mut g, "small.rs");
        let ranked = vec![(big_id.clone(), 9.0), (small_id.clone(), 1.0)];

        // Budget too small for big.rs at Full, but room for Head/PathOnly + small.
        let selected = pack(&g, &ranked, 60, dir.path());
        let big_sel = selected.iter().find(|s| s.id == big_id).unwrap();
        assert!(
            matches!(big_sel.fidelity, Fidelity::Head | Fidelity::PathOnly),
            "big.rs should be downgraded, got {:?}",
            big_sel.fidelity
        );
        // small.rs still makes it in (not dropped).
        assert!(selected.iter().any(|s| s.id == small_id));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p lattice-uninode compiler::tests::pack`
Expected: FAIL — `cannot find function pack` / `Fidelity` / `est_tokens`.

- [ ] **Step 3: Write minimal implementation**

Add to `crates/lattice-uninode/src/compiler.rs` (add `use std::path::Path;` to the imports at the top of the file):

```rust
use std::path::Path;

/// How much of a file is materialized into context.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fidelity {
    Full,
    Head,
    PathOnly,
}

/// A node selected into the compiled context at a chosen fidelity.
#[derive(Clone, Debug)]
pub struct Selected {
    pub id: String,
    pub path: String,
    pub fidelity: Fidelity,
    pub tokens: usize,
}

/// Crude token estimate: ~4 chars/token, floored at 1.
pub fn est_tokens(s: &str) -> usize {
    (s.chars().count() / 4).max(1)
}

const HEAD_LINES: usize = 50;

/// Stage 4 — Pack. Greedy fill under `budget`, downgrading fidelity
/// (Full → Head → PathOnly) rather than dropping a relevant node outright.
/// `workdir` is the repo root from which file contents are read.
pub fn pack(
    graph: &Graph,
    ranked: &[(String, f64)],
    budget: usize,
    workdir: &Path,
) -> Vec<Selected> {
    let mut out = Vec::new();
    let mut used = 0usize;

    for (id, _score) in ranked {
        let Some(node) = graph.nodes.get(id) else {
            continue;
        };
        let content = std::fs::read_to_string(workdir.join(&node.path)).unwrap_or_default();
        let full_t = est_tokens(&content);
        let head: String = content.lines().take(HEAD_LINES).collect::<Vec<_>>().join("\n");
        let head_t = est_tokens(&head);
        let path_t = est_tokens(&node.path) + 1;

        let (fidelity, tokens) = if used + full_t <= budget {
            (Fidelity::Full, full_t)
        } else if used + head_t <= budget {
            (Fidelity::Head, head_t)
        } else if used + path_t <= budget {
            (Fidelity::PathOnly, path_t)
        } else {
            continue; // even a path mention doesn't fit; skip, keep scanning
        };

        used += tokens;
        out.push(Selected { id: id.clone(), path: node.path.clone(), fidelity, tokens });
    }
    out
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p lattice-uninode compiler::`
Expected: PASS (all compiler tests).

- [ ] **Step 5: Commit**

```bash
git add crates/lattice-uninode/src/compiler.rs
git commit -m "feat(uninode): compiler pack stage with fidelity downgrade"
```

---

## Task 10: Eval — metrics + naive baseline + replay cases

**Files:**
- Modify: `crates/lattice-uninode/src/eval.rs`

- [ ] **Step 1: Write the failing test**

Append to `crates/lattice-uninode/src/eval.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::scan_repo;
    use crate::testing::TestRepo;

    #[test]
    fn recall_and_precision_basic() {
        let selected = vec!["a".to_string(), "b".to_string()];
        let truth = vec!["a".to_string(), "c".to_string()];
        assert!((recall(&selected, &truth) - 0.5).abs() < 1e-9); // hit a of {a,c}
        assert!((precision(&selected, &truth) - 0.5).abs() < 1e-9); // a of {a,b}
        assert_eq!(recall(&[], &[]), 1.0);
        assert_eq!(precision(&[], &truth), 0.0);
    }

    #[test]
    fn replay_cases_extracts_query_and_changed_files() {
        let repo = TestRepo::new();
        repo.write("a.rs", "1");
        repo.commit("root", &["a.rs"]);
        repo.write("a.rs", "2");
        repo.write("b.rs", "1");
        repo.commit("fix the thing in a", &["a.rs", "b.rs"]);

        let r = repo.open();
        let cases = replay_cases(&r, 10).unwrap();
        // Root commit (parent_count 0) is skipped; one case remains.
        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0].query, "fix the thing in a");
        let mut truth = cases[0].truth.clone();
        truth.sort();
        assert_eq!(truth, vec!["a.rs".to_string(), "b.rs".to_string()]);
        assert!(cases[0].parent.is_some());
    }

    #[test]
    fn naive_select_pulls_seed_files_and_siblings() {
        let repo = TestRepo::new();
        repo.write("src/auth.rs", "fn a() {}\n");
        repo.write("src/session.rs", "fn s() {}\n");
        repo.commit("init", &["src/auth.rs", "src/session.rs"]);

        let r = repo.open();
        let g = scan_repo(&r).unwrap();
        let selected = naive_select(&g, "auth", 10_000, r.workdir().unwrap());
        let paths: Vec<&str> = selected.iter().map(|s| s.path.as_str()).collect();
        // seed = auth.rs; sibling = session.rs → both pulled in.
        assert!(paths.contains(&"src/auth.rs"));
        assert!(paths.contains(&"src/session.rs"));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p lattice-uninode eval::`
Expected: FAIL — `cannot find function recall / precision / replay_cases / naive_select`.

- [ ] **Step 3: Write minimal implementation**

Insert above the `#[cfg(test)]` block in `crates/lattice-uninode/src/eval.rs`:

```rust
use std::collections::HashSet;
use std::path::Path;

use crate::compiler::{est_tokens, seed, Fidelity, Selected};
use crate::graph::{EdgeKind, Graph};
use crate::Result;

/// A replayed historical commit used as one benchmark case.
pub struct CommitCase {
    /// The commit's summary line — used as the query.
    pub query: String,
    /// Paths the commit actually changed — the ground truth.
    pub truth: Vec<String>,
    /// The commit's first parent — the leave-one-out history root for co-change.
    pub parent: Option<git2::Oid>,
}

/// Fraction of ground-truth paths present in the selection.
pub fn recall(selected: &[String], truth: &[String]) -> f64 {
    if truth.is_empty() {
        return 1.0;
    }
    let set: HashSet<&String> = selected.iter().collect();
    let hit = truth.iter().filter(|t| set.contains(*t)).count();
    hit as f64 / truth.len() as f64
}

/// Fraction of selected paths that are in the ground truth.
pub fn precision(selected: &[String], truth: &[String]) -> f64 {
    if selected.is_empty() {
        return 0.0;
    }
    let tset: HashSet<&String> = truth.iter().collect();
    let hit = selected.iter().filter(|s| tset.contains(*s)).count();
    hit as f64 / selected.len() as f64
}

/// Extract up to `max` benchmark cases from the most recent non-merge,
/// non-root commits (newest first).
pub fn replay_cases(repo: &git2::Repository, max: usize) -> Result<Vec<CommitCase>> {
    let mut walk = repo.revwalk()?;
    walk.push_head()?;
    let mut cases = Vec::new();

    for oid in walk {
        if cases.len() >= max {
            break;
        }
        let commit = repo.find_commit(oid?)?;
        if commit.parent_count() != 1 {
            continue;
        }
        let parent = commit.parent(0)?;
        let diff =
            repo.diff_tree_to_tree(Some(&parent.tree()?), Some(&commit.tree()?), None)?;

        let mut truth: Vec<String> = Vec::new();
        diff.foreach(
            &mut |delta, _| {
                if let Some(p) = delta.new_file().path() {
                    truth.push(p.to_string_lossy().to_string());
                }
                true
            },
            None,
            None,
            None,
        )?;
        truth.sort();
        truth.dedup();

        cases.push(CommitCase {
            query: commit.summary().unwrap_or("").to_string(),
            truth,
            parent: Some(parent.id()),
        });
    }
    Ok(cases)
}

/// Naive baseline B: seed files plus their directory siblings, materialized at
/// full text, greedily filled to `budget` with no ranking or downgrade.
pub fn naive_select(graph: &Graph, query: &str, budget: usize, workdir: &Path) -> Vec<Selected> {
    let seeds = seed(graph, query);
    let mut ids: Vec<String> = Vec::new();
    for id in seeds.keys() {
        ids.push(id.clone());
        for e in graph.neighbors(id) {
            if e.kind == EdgeKind::Sibling {
                ids.push(e.target.clone());
            }
        }
    }
    ids.sort();
    ids.dedup();

    let mut out = Vec::new();
    let mut used = 0usize;
    for id in ids {
        let Some(node) = graph.nodes.get(&id) else {
            continue;
        };
        let content = std::fs::read_to_string(workdir.join(&node.path)).unwrap_or_default();
        let t = est_tokens(&content);
        if used + t > budget {
            continue;
        }
        used += t;
        out.push(Selected { id: id.clone(), path: node.path.clone(), fidelity: Fidelity::Full, tokens: t });
    }
    out
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p lattice-uninode eval::`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/lattice-uninode/src/eval.rs
git commit -m "feat(uninode): eval metrics, replay cases, naive baseline"
```

---

## Task 11: Eval binary — replay A/B/C and print the table

**Files:**
- Modify: `crates/lattice-uninode/src/bin/eval.rs`

- [ ] **Step 1: Write the implementation (binary; smoke-tested by running it)**

This task's "test" is an executable smoke run against this very repo. Replace `crates/lattice-uninode/src/bin/eval.rs` with:

```rust
// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! uninode-eval — replay historical commits and compare context selection.
//!
//! For each of the last N non-merge commits: query = commit summary, ground
//! truth = changed files. Build the graph from history *before* that commit
//! (leave-one-out), then compare:
//!   B (naive):   seed files + siblings, full text, budget-filled
//!   C (uninode): seed → expand → rank → pack (fidelity downgrade)
//! Report mean recall@budget and mean context tokens for B and C.

use std::path::PathBuf;

use clap::Parser;
use lattice_uninode::cochange::add_cochange_edges;
use lattice_uninode::compiler::{expand, pack, rank, seed};
use lattice_uninode::eval::{naive_select, recall, replay_cases};
use lattice_uninode::scanner::scan_repo;

#[derive(Parser)]
#[command(about = "Replay commits and compare naive vs uninode context selection")]
struct Args {
    /// Path to a git repository to evaluate against.
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    /// Number of recent commits to replay.
    #[arg(long, default_value_t = 50)]
    commits: usize,
    /// Context token budget per case.
    #[arg(long, default_value_t = 4000)]
    budget: usize,
    /// Co-change history depth.
    #[arg(long, default_value_t = 500)]
    history: usize,
    /// Expand hops.
    #[arg(long, default_value_t = 2)]
    hops: usize,
    /// Expand per-hop decay.
    #[arg(long, default_value_t = 0.7)]
    decay: f64,
    /// Rank hub-penalty strength.
    #[arg(long, default_value_t = 1.0)]
    beta: f64,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let repo = git2::Repository::open(&args.repo)?;
    let workdir = repo.workdir().ok_or("repo has no workdir")?.to_path_buf();

    let cases = replay_cases(&repo, args.commits)?;
    if cases.is_empty() {
        eprintln!("no replayable commits found");
        return Ok(());
    }

    let (mut b_recall, mut b_tokens) = (0.0_f64, 0.0_f64);
    let (mut c_recall, mut c_tokens) = (0.0_f64, 0.0_f64);
    let mut counted = 0usize;

    for case in &cases {
        // Graph at current workdir; co-change restricted to history before the case.
        let mut graph = scan_repo(&repo)?;
        add_cochange_edges(&mut graph, &repo, case.parent, args.history)?;

        // B — naive
        let b = naive_select(&graph, &case.query, args.budget, &workdir);
        let b_paths: Vec<String> = b.iter().map(|s| s.path.clone()).collect();

        // C — uninode
        let seeds = seed(&graph, &case.query);
        let spread = expand(&graph, &seeds, args.hops, args.decay);
        let ranked = rank(&graph, &spread, args.beta);
        let c = pack(&graph, &ranked, args.budget, &workdir);
        let c_paths: Vec<String> = c.iter().map(|s| s.path.clone()).collect();

        b_recall += recall(&b_paths, &case.truth);
        b_tokens += b.iter().map(|s| s.tokens).sum::<usize>() as f64;
        c_recall += recall(&c_paths, &case.truth);
        c_tokens += c.iter().map(|s| s.tokens).sum::<usize>() as f64;
        counted += 1;
    }

    let n = counted as f64;
    println!("cases evaluated : {counted}  (budget={} tokens)", args.budget);
    println!("                   {:>12}  {:>12}", "recall@budget", "mean tokens");
    println!("B  naive         : {:>12.3}  {:>12.0}", b_recall / n, b_tokens / n);
    println!("C  uninode       : {:>12.3}  {:>12.0}", c_recall / n, c_tokens / n);
    Ok(())
}
```

- [ ] **Step 2: Verify it builds**

Run: `cargo build -p lattice-uninode --bin uninode-eval`
Expected: compiles clean.

- [ ] **Step 3: Smoke-run it against this repo**

Run: `cargo run -p lattice-uninode --bin uninode-eval -- --repo . --commits 30 --budget 4000`
Expected: prints a 2-row table with non-NaN recall and token numbers for B and C. (Exact values are data, not assertions — this step proves the end-to-end pipeline runs on real history.)

- [ ] **Step 4: Run the whole crate test suite + clippy**

Run: `cargo test -p lattice-uninode && cargo clippy -p lattice-uninode -- -D warnings`
Expected: all tests PASS, no clippy warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/lattice-uninode/src/bin/eval.rs
git commit -m "feat(uninode): eval binary — commit replay A/B/C comparison"
```

---

## Interpreting the Result (the actual deliverable)

After Task 11, run the binary across a few budgets and record the table:

```bash
for b in 1000 2000 4000 8000; do
  echo "budget=$b"; cargo run -q -p lattice-uninode --bin uninode-eval -- --repo . --budget $b
done
```

**Continue signal (uninode is worth it):** at a fixed budget C's `recall@budget` is meaningfully higher than B's, **or** C reaches B's recall at fewer mean tokens. Either confirms the graph (expand + rank) adds recall that naive same-directory + lexical does not.

**Stop signal (disprove, cheaply):** C ≈ B across budgets → expand/rank contribute nothing; the graph isn't earning its keep on this repo. Stop here — no turn-execution code was touched, no Phase 34 dependency was incurred.

This is the whole point of a1: a go/no-go number on the most expensive assumption, produced before any main-line change.

---

## Self-Review

**1. Spec coverage** (against the a1 scope from the design discussion):
- Offline File graph → Tasks 2, 4 ✓
- Sibling edges (Contain approximation) → Task 4 ✓
- Co-change edges (RelatedTo, offline, leave-one-out) → Task 5 ✓
- Seed → Task 6 ✓ · Expand w/ fan-effect → Task 7 ✓ · Rank w/ hub down-weight → Task 8 ✓ · Pack w/ fidelity downgrade → Task 9 ✓
- Naive baseline B → Task 10 ✓ · recall/precision metrics → Task 10 ✓
- Commit-replay harness + A/B/C table → Tasks 10, 11 ✓
- JSONL dump (validates spec layout) → Task 3 ✓
- Explicitly deferred (documented in Scope): fanotify/Operation, learned weights, turn injection (`build_messages`), CEP, AST bundle.

**2. Placeholder scan:** no `TODO`/`tbd`/"handle errors appropriately"/"similar to Task N" — every code step is complete and self-contained. Baseline A (empty context, recall 0) is intentionally not implemented offline; it only has meaning at M4 end-to-end, noted in Scope.

**3. Type consistency:** `Graph`/`Node`/`Edge`/`NodeKind`/`EdgeKind`/`node_id` (Task 2) are used unchanged in Tasks 3–11. `seed`/`expand`/`rank` return `HashMap<String,f64>` / `Vec<(String,f64)>` consistently; `pack`/`naive_select` both return `Vec<Selected>`; `Fidelity`/`Selected`/`est_tokens` defined once (Task 9) and reused in Task 10/11. `CommitCase { query, truth, parent }` defined in Task 10 and consumed in Task 11. `replay_cases(&Repository, usize)`, `add_cochange_edges(&mut Graph, &Repository, Option<Oid>, usize)`, `scan_repo(&Repository)` signatures match their call sites in the binary.

# uninode a1 — Context-Compiler Spike v2 (LCA-evaluated) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> **Supersedes** `2026-06-20-uninode-a1-compiler-spike.md` (v1). v1 was reviewed by codex (gpt-5.5) and had real bugs (size assert, leave-one-out test, **IDF using out-degree instead of in-degree**, non-deterministic rank ties) and — more importantly — a hand-rolled commit-replay benchmark whose ground truth ("changed files") is *edit-localization*, not *useful context*, forcing an endless pile of correctness patches (survival/rename/history-pollution filters). v2 keeps v1's graph + compiler core (bugs fixed) and **replaces the evaluation** with a standard IR protocol over an off-the-shelf, human-curated benchmark.

**Goal:** Decide go/no-go on one question: does graph expansion over sibling + git-cochange edges retrieve the relevant files for a real issue better than a strong lexical baseline (BM25), measured with standard IR metrics on the Long Code Arena (LCA) bug-localization benchmark.

**Architecture:** A pure-synchronous Rust crate `lattice-uninode` (File graph + BM25 + expand/rank) plus a tiny one-time Python step to export an LCA subset to `tasks.jsonl` and clone each repo at its pre-fix `base_sha`. Evaluation is an **ablation**: B = BM25 alone; C1 = BM25 seeds + sibling expansion; C2 = + cochange expansion. Because the LCA snapshot is the repo *at* `base_sha`, co-change history (walked from HEAD=base_sha) is automatically leave-one-out — no history-pollution or survival patching needed.

**Tech Stack:** Rust edition 2024 (`git2` 0.20, `blake3`, `serde`/`serde_json`, `clap`, `tempfile`); Python 3 with `datasets` (one-time export only, not a crate dependency); `git` CLI for cloning and tests.

**Why this is simpler than v1:** the benchmark supplies the query (real issue), the snapshot (`base_sha`), and curated gold — so all of v1's commit-replay patches disappear. Metrics are standard Recall@k / MAP / nDCG on a ranked file list, so `pack`/fidelity is **not on the eval path** at all (it stays in the codebase only for the future M4 turn-injection).

---

## Scope

**In scope:** `lattice-uninode` crate (graph, scanner, cochange, BM25, expand/rank), LCA data export + repo prep, ablation eval binary with standard metrics and a pre-declared GO threshold.

**Out of scope (later "a2"/"M4" plan):** fanotify/runtime `Operation` edges; learned edge weights; turn injection at `crates/lattice-agent/src/agent_loop/mod.rs:1036` (`build_messages`); CEP/promote; AST/Symbol bundle. `pack`/fidelity is implemented (carried for M4) but not exercised by the eval.

## Prerequisites — one-time data prep (not a Rust task)

These produce inputs the eval reads. Do this once before Task 9.

- **P1 — Export an LCA subset.** Requires `pip install datasets`. The script in Task 8 writes `data/lca/tasks.jsonl`. Run: `python scripts/export_lca.py --config py --split test --limit 40 --out data/lca/tasks.jsonl` (Python config is largest; `test` split is human-verified; 40 keeps cloning feasible).
- **P2 — Clone each repo at `base_sha`.** The eval expects each repo checked out at its pre-fix commit under `--repos-dir`, named `{repo_owner}__{repo_name}`. Task 8 emits `scripts/prepare_repos.sh` which reads `tasks.jsonl` and does exactly this. Run: `bash scripts/prepare_repos.sh data/lca/tasks.jsonl repos/`.
- Both `data/` and `repos/` are git-ignored (Task 1, Step 1).

---

## Task 1: Crate scaffold + test helper

**Files:**
- Modify: `Cargo.toml` (workspace root `members`)
- Modify: `.gitignore` (root)
- Create: `crates/lattice-uninode/Cargo.toml`
- Create: `crates/lattice-uninode/src/lib.rs`

- [ ] **Step 1: Register the crate and ignore eval data**

In root `Cargo.toml`, add to `members` (after `"crates/lattice-workspace",`):

```toml
    "crates/lattice-uninode",
]
```

Append to root `.gitignore`:

```gitignore
# uninode a1 spike eval inputs (large, reproducible from scripts/export_lca.py)
/data/lca/
/repos/
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
description = "Offline context-compiler spike (uninode a1): File graph + BM25 + sibling/cochange expansion, evaluated on Long Code Arena bug-localization"

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

- [ ] **Step 3: Create `lib.rs` (Error/Result, modules, test helper)**

Create `crates/lattice-uninode/src/lib.rs`:

```rust
// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! uninode a1 (v2) — File graph + BM25 + sibling/cochange expansion, evaluated
//! on the Long Code Arena bug-localization benchmark. Pure synchronous; touches
//! no turn-execution code.

pub mod bm25;
pub mod cochange;
pub mod compiler;
pub mod eval;
pub mod graph;
pub mod scanner;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("git error: {0}")]
    Git(#[from] git2::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("repo has no workdir (bare repo)")]
    NoWorkdir,
}

pub type Result<T> = std::result::Result<T, Error>;

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

- [ ] **Step 4: Create empty module files**

Create `graph.rs`, `scanner.rs`, `cochange.rs`, `bm25.rs`, `compiler.rs`, `eval.rs` each with:

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

- [ ] **Step 5: Build, then commit (include Cargo.lock)**

Run: `cargo build -p lattice-uninode`
Expected: compiles clean.

```bash
git add Cargo.toml Cargo.lock .gitignore crates/lattice-uninode
git commit -m "feat(uninode): scaffold lattice-uninode crate (a1 v2)"
```

---

## Task 2: Graph types + node id + in/out degree

**Files:**
- Modify: `crates/lattice-uninode/src/graph.rs`

- [ ] **Step 1: Write the failing test**

Append to `graph.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_id_stable_and_path_sensitive() {
        let a = node_id(NodeKind::File, "src/main.rs");
        assert_eq!(a, node_id(NodeKind::File, "src/main.rs"));
        assert_ne!(a, node_id(NodeKind::File, "src/lib.rs"));
        assert_eq!(a.len(), 64);
    }

    #[test]
    fn tracks_out_and_in_degree() {
        let mut g = Graph::new();
        let a = node_id(NodeKind::File, "a.rs");
        let b = node_id(NodeKind::File, "b.rs");
        let c = node_id(NodeKind::File, "c.rs");
        for (id, p) in [(&a, "a.rs"), (&b, "b.rs"), (&c, "c.rs")] {
            g.add_node(Node { id: id.clone(), kind: NodeKind::File, path: p.into(), size: 1, hash: "h".into() });
        }
        // a→b, c→b : b has in-degree 2, out-degree 0.
        g.add_edge(&a, Edge { target: b.clone(), kind: EdgeKind::CoChange, weight: 1.0 });
        g.add_edge(&c, Edge { target: b.clone(), kind: EdgeKind::CoChange, weight: 1.0 });
        let indeg = g.in_degrees();
        assert_eq!(indeg.get(&b).copied().unwrap_or(0), 2);
        assert_eq!(g.out_degree(&a), 1);
        assert_eq!(indeg.get(&a).copied().unwrap_or(0), 0);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p lattice-uninode graph::`
Expected: FAIL — types/functions not found.

- [ ] **Step 3: Write minimal implementation**

Insert above the test module in `graph.rs`:

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
    Sibling,
    CoChange,
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

pub fn node_id(kind: NodeKind, path: &str) -> String {
    let mut h = blake3::Hasher::new();
    h.update(kind.as_str().as_bytes());
    h.update(b"\0");
    h.update(path.as_bytes());
    h.finalize().to_hex().to_string()
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
    /// In-degree of every node (number of edges pointing AT it), computed once.
    /// Used by rank's hub penalty — god-objects are high IN-degree (referenced
    /// everywhere), distinct from fan-effect's out-degree normalization.
    pub fn in_degrees(&self) -> HashMap<String, usize> {
        let mut m: HashMap<String, usize> = HashMap::new();
        for edges in self.edges.values() {
            for e in edges {
                *m.entry(e.target.clone()).or_default() += 1;
            }
        }
        m
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p lattice-uninode graph::`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/lattice-uninode/src/graph.rs
git commit -m "feat(uninode): graph types, node id, in/out degree"
```

---

## Task 3: Scanner — File nodes + Sibling edges

**Files:**
- Modify: `crates/lattice-uninode/src/scanner.rs`

- [ ] **Step 1: Write the failing test**

Append to `scanner.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{node_id, EdgeKind, NodeKind};
    use crate::testing::TestRepo;

    #[test]
    fn scans_tracked_files() {
        let repo = TestRepo::new();
        repo.write("src/main.rs", "fn main() {}\n"); // 13 bytes
        repo.write("README.md", "# hi\n");
        repo.commit("init", &["src/main.rs", "README.md"]);
        let g = scan_repo(&repo.open()).unwrap();
        assert_eq!(g.nodes.len(), 2);
        let node = &g.nodes[&node_id(NodeKind::File, "src/main.rs")];
        assert_eq!(node.size, 13);
        assert_eq!(node.hash.len(), 64);
    }

    #[test]
    fn same_dir_files_get_sibling_edges() {
        let repo = TestRepo::new();
        repo.write("src/a.rs", "a\n");
        repo.write("src/b.rs", "b\n");
        repo.write("docs/c.md", "c\n");
        repo.commit("init", &["src/a.rs", "src/b.rs", "docs/c.md"]);
        let g = scan_repo(&repo.open()).unwrap();
        let a = node_id(NodeKind::File, "src/a.rs");
        let b = node_id(NodeKind::File, "src/b.rs");
        let c = node_id(NodeKind::File, "docs/c.md");
        assert!(g.neighbors(&a).iter().any(|e| e.target == b && e.kind == EdgeKind::Sibling));
        assert_eq!(g.out_degree(&c), 0);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p lattice-uninode scanner::`
Expected: FAIL — `scan_repo` not found.

- [ ] **Step 3: Write minimal implementation**

Insert above the test module in `scanner.rs`:

```rust
use std::collections::HashMap;

use crate::graph::{node_id, Edge, EdgeKind, Graph, Node, NodeKind};
use crate::{Error, Result};

pub const SIBLING_WEIGHT: f64 = 0.3;

pub fn scan_repo(repo: &git2::Repository) -> Result<Graph> {
    let workdir = repo.workdir().ok_or(Error::NoWorkdir)?.to_path_buf();
    let mut graph = Graph::new();
    let mut by_dir: HashMap<String, Vec<String>> = HashMap::new();

    let index = repo.index()?;
    for entry in index.iter() {
        let path = String::from_utf8_lossy(&entry.path).to_string();
        let Ok(bytes) = std::fs::read(workdir.join(&path)) else {
            continue;
        };
        let id = node_id(NodeKind::File, &path);
        graph.add_node(Node {
            id: id.clone(),
            kind: NodeKind::File,
            path: path.clone(),
            size: bytes.len() as u64,
            hash: blake3::hash(&bytes).to_hex().to_string(),
        });
        let dir = path.rsplit_once('/').map_or(String::new(), |(d, _)| d.to_string());
        by_dir.entry(dir).or_default().push(id);
    }

    for ids in by_dir.values() {
        for a in ids {
            for b in ids {
                if a != b {
                    graph.add_edge(a, Edge { target: b.clone(), kind: EdgeKind::Sibling, weight: SIBLING_WEIGHT });
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

## Task 4: Cochange edges — Jaccard normalization + rename detection

**Files:**
- Modify: `crates/lattice-uninode/src/cochange.rs`

- [ ] **Step 1: Write the failing test**

Append to `cochange.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{node_id, EdgeKind, NodeKind};
    use crate::scanner::scan_repo;
    use crate::testing::TestRepo;

    #[test]
    fn cochange_weight_is_jaccard() {
        let repo = TestRepo::new();
        repo.write("a.rs", "1");
        repo.write("b.rs", "1");
        repo.commit("root", &["a.rs", "b.rs"]); // root: skipped (no parent)
        repo.write("a.rs", "2");
        repo.write("b.rs", "2");
        repo.commit("c2", &["a.rs", "b.rs"]); // a&b together
        repo.write("a.rs", "3");
        repo.commit("c3", &["a.rs"]); // a alone
        // Non-root commits processed: c2 (a,b), c3 (a). count_a=2, count_b=1, co=1.
        // Jaccard = 1 / (2 + 1 - 1) = 0.5.
        let r = repo.open();
        let mut g = scan_repo(&r).unwrap();
        add_cochange_edges(&mut g, &r, 100).unwrap();
        let a = node_id(NodeKind::File, "a.rs");
        let b = node_id(NodeKind::File, "b.rs");
        let e = g.neighbors(&a).iter().find(|e| e.target == b && e.kind == EdgeKind::CoChange).unwrap();
        assert!((e.weight - 0.5).abs() < 1e-9, "weight was {}", e.weight);
    }

    #[test]
    fn rename_is_followed() {
        let repo = TestRepo::new();
        repo.write("old.rs", "fn x() {}\n");
        repo.write("pair.rs", "p\n");
        repo.commit("root", &["old.rs", "pair.rs"]);
        repo.write("pair.rs", "p2\n");
        repo.commit("touch pair", &["pair.rs"]);
        // Rename old.rs -> new.rs together with a pair.rs change in one commit.
        crate::testing::git(repo.path(), &["mv", "old.rs", "new.rs"]);
        repo.write("pair.rs", "p3\n");
        repo.commit("rename old to new + touch pair", &["old.rs", "new.rs", "pair.rs"]);
        let r = repo.open();
        let mut g = scan_repo(&r).unwrap();
        add_cochange_edges(&mut g, &r, 100).unwrap();
        // With rename detection on, the rename commit's path is new.rs (not a
        // delete+add), so new.rs co-changes with pair.rs.
        let newn = node_id(NodeKind::File, "new.rs");
        let pair = node_id(NodeKind::File, "pair.rs");
        assert!(g.neighbors(&newn).iter().any(|e| e.target == pair && e.kind == EdgeKind::CoChange));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p lattice-uninode cochange::`
Expected: FAIL — `add_cochange_edges` not found.

- [ ] **Step 3: Write minimal implementation**

Insert above the test module in `cochange.rs`:

```rust
use std::collections::HashMap;

use crate::graph::{node_id, Edge, EdgeKind, Graph, NodeKind};
use crate::Result;

/// Add `CoChange` edges from history reachable from HEAD (newest `max_commits`).
/// Caller controls leave-one-out by checking out the repo at the pre-fix commit:
/// HEAD = base_sha means only history strictly before the fix is walked.
/// Merge/root commits (parent_count != 1) are skipped. Renames are followed, so
/// a renamed file keeps a single identity (its post-rename path).
pub fn add_cochange_edges(graph: &mut Graph, repo: &git2::Repository, max_commits: usize) -> Result<()> {
    let mut walk = repo.revwalk()?;
    walk.push_head()?;

    let mut pair: HashMap<(String, String), u32> = HashMap::new();
    let mut count: HashMap<String, u32> = HashMap::new();

    for (i, oid) in walk.enumerate() {
        if i >= max_commits {
            break;
        }
        let commit = repo.find_commit(oid?)?;
        if commit.parent_count() != 1 {
            continue;
        }
        let parent = commit.parent(0)?;
        let mut diff = repo.diff_tree_to_tree(Some(&parent.tree()?), Some(&commit.tree()?), None)?;
        let mut find = git2::DiffFindOptions::new();
        find.renames(true);
        diff.find_similar(Some(&mut find))?;

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
            *count.entry(f.clone()).or_default() += 1;
        }
        for a in 0..files.len() {
            for b in (a + 1)..files.len() {
                *pair.entry((files[a].clone(), files[b].clone())).or_default() += 1;
            }
        }
    }

    for ((fa, fb), co) in pair {
        let ca = *count.get(&fa).unwrap_or(&0);
        let cb = *count.get(&fb).unwrap_or(&0);
        let union = (ca + cb).saturating_sub(co);
        if union == 0 {
            continue;
        }
        let w = co as f64 / union as f64; // Jaccard
        let ida = node_id(NodeKind::File, &fa);
        let idb = node_id(NodeKind::File, &fb);
        if graph.nodes.contains_key(&ida) && graph.nodes.contains_key(&idb) {
            graph.add_edge(&ida, Edge { target: idb.clone(), kind: EdgeKind::CoChange, weight: w });
            graph.add_edge(&idb, Edge { target: ida.clone(), kind: EdgeKind::CoChange, weight: w });
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
git commit -m "feat(uninode): cochange edges — Jaccard + rename detection"
```

---

## Task 5: BM25 baseline scorer

**Files:**
- Modify: `crates/lattice-uninode/src/bm25.rs`

- [ ] **Step 1: Write the failing test**

Append to `bm25.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn tokenize_lowercases_and_drops_short() {
        assert_eq!(tokenize("Fix the AuthSession bug"), vec!["fix", "the", "authsession", "bug"]);
        assert_eq!(tokenize("a -- b3"), vec!["b3"]); // < 2 chars dropped
    }

    #[test]
    fn bm25_ranks_term_matching_docs_higher() {
        let mut docs: HashMap<String, String> = HashMap::new();
        docs.insert("auth.rs".into(), "fn authenticate session token".into());
        docs.insert("math.rs".into(), "fn add subtract multiply".into());
        docs.insert("util.rs".into(), "fn helper".into());
        let scores = bm25_scores(&docs, "authenticate session");
        assert!(scores["auth.rs"] > scores["math.rs"]);
        assert!(scores["auth.rs"] > 0.0);
        // A doc with no query term is either absent or zero.
        assert!(scores.get("math.rs").copied().unwrap_or(0.0) <= 0.0 + 1e-9);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p lattice-uninode bm25::`
Expected: FAIL — `tokenize` / `bm25_scores` not found.

- [ ] **Step 3: Write minimal implementation**

Insert above the test module in `bm25.rs`:

```rust
use std::collections::HashMap;

const K1: f64 = 1.5;
const B: f64 = 0.75;

/// Lowercase alphanumeric tokens of length >= 2.
pub fn tokenize(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() >= 2)
        .map(|t| t.to_string())
        .collect()
}

/// Okapi BM25 over a corpus of `id -> document text`, scoring each document
/// against `query`. Documents with no query-term match score 0 (omitted).
pub fn bm25_scores(docs: &HashMap<String, String>, query: &str) -> HashMap<String, f64> {
    let n = docs.len().max(1) as f64;
    // Per-doc term frequencies + lengths.
    let mut tf: HashMap<String, HashMap<String, u32>> = HashMap::new();
    let mut len: HashMap<String, f64> = HashMap::new();
    let mut df: HashMap<String, u32> = HashMap::new();
    let mut total_len = 0.0;
    for (id, text) in docs {
        let toks = tokenize(text);
        total_len += toks.len() as f64;
        len.insert(id.clone(), toks.len() as f64);
        let mut counts: HashMap<String, u32> = HashMap::new();
        for t in toks {
            *counts.entry(t).or_default() += 1;
        }
        for t in counts.keys() {
            *df.entry(t.clone()).or_default() += 1;
        }
        tf.insert(id.clone(), counts);
    }
    let avgdl = (total_len / n).max(1.0);

    let q_terms: Vec<String> = tokenize(query);
    let mut scores: HashMap<String, f64> = HashMap::new();
    for (id, counts) in &tf {
        let dl = len[id];
        let mut s = 0.0;
        for t in &q_terms {
            let f = counts.get(t).copied().unwrap_or(0) as f64;
            if f == 0.0 {
                continue;
            }
            let nt = df.get(t).copied().unwrap_or(0) as f64;
            let idf = ((n - nt + 0.5) / (nt + 0.5) + 1.0).ln();
            s += idf * (f * (K1 + 1.0)) / (f + K1 * (1.0 - B + B * dl / avgdl));
        }
        if s > 0.0 {
            scores.insert(id.clone(), s);
        }
    }
    scores
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p lattice-uninode bm25::`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/lattice-uninode/src/bm25.rs
git commit -m "feat(uninode): BM25 baseline scorer"
```

---

## Task 6: Compiler — expand (edge-kind filtered, fan-effect) + rank (in-degree hub penalty, deterministic)

**Files:**
- Modify: `crates/lattice-uninode/src/compiler.rs`

- [ ] **Step 1: Write the failing test**

Append to `compiler.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{node_id, Edge, EdgeKind, Graph, Node, NodeKind};
    use std::collections::HashMap;

    fn fnode(g: &mut Graph, p: &str) -> String {
        let id = node_id(NodeKind::File, p);
        g.add_node(Node { id: id.clone(), kind: NodeKind::File, path: p.into(), size: 1, hash: "h".into() });
        id
    }

    #[test]
    fn expand_only_traverses_allowed_edge_kinds() {
        let mut g = Graph::new();
        let a = fnode(&mut g, "a.rs");
        let sib = fnode(&mut g, "sib.rs");
        let co = fnode(&mut g, "co.rs");
        g.add_edge(&a, Edge { target: sib.clone(), kind: EdgeKind::Sibling, weight: 1.0 });
        g.add_edge(&a, Edge { target: co.clone(), kind: EdgeKind::CoChange, weight: 1.0 });
        let mut seeds = HashMap::new();
        seeds.insert(a.clone(), 1.0);
        // Sibling-only expansion reaches sib but not co.
        let s = expand(&g, &seeds, 1, 1.0, &[EdgeKind::Sibling]);
        assert!(s.contains_key(&sib));
        assert!(!s.contains_key(&co));
    }

    #[test]
    fn expand_applies_fan_effect() {
        let mut g = Graph::new();
        let a = fnode(&mut g, "a.rs");
        let b = fnode(&mut g, "b.rs");
        let c = fnode(&mut g, "c.rs");
        g.add_edge(&a, Edge { target: b.clone(), kind: EdgeKind::CoChange, weight: 1.0 });
        g.add_edge(&a, Edge { target: c.clone(), kind: EdgeKind::CoChange, weight: 1.0 });
        let mut seeds = HashMap::new();
        seeds.insert(a.clone(), 1.0);
        let s = expand(&g, &seeds, 1, 1.0, &[EdgeKind::CoChange]);
        // 1.0 * 1.0 / fan(2) = 0.5
        assert!((s[&b] - 0.5).abs() < 1e-9);
    }

    #[test]
    fn rank_penalizes_high_in_degree_and_breaks_ties_deterministically() {
        let mut g = Graph::new();
        let lonely = fnode(&mut g, "lonely.rs");
        let hub = fnode(&mut g, "hub.rs");
        // Many nodes point AT hub → high IN-degree.
        for i in 0..10 {
            let dep = fnode(&mut g, &format!("dep{i}.rs"));
            g.add_edge(&dep, Edge { target: hub.clone(), kind: EdgeKind::CoChange, weight: 1.0 });
        }
        let indeg = g.in_degrees();
        let mut scores = HashMap::new();
        scores.insert(lonely.clone(), 1.0);
        scores.insert(hub.clone(), 1.0);
        let ranked = rank(&scores, &indeg, 1.0);
        let pos = |id: &str| ranked.iter().position(|(i, _)| i == id).unwrap();
        assert!(pos(&lonely) < pos(&hub), "lonely should outrank high-in-degree hub");

        // Determinism: equal scores + equal in-degree → tie broken by id, stable.
        let x = fnode(&mut g, "x.rs");
        let y = fnode(&mut g, "y.rs");
        let mut s2 = HashMap::new();
        s2.insert(x.clone(), 1.0);
        s2.insert(y.clone(), 1.0);
        let r1 = rank(&s2, &g.in_degrees(), 1.0);
        let r2 = rank(&s2, &g.in_degrees(), 1.0);
        assert_eq!(r1, r2);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p lattice-uninode compiler::`
Expected: FAIL — `expand` / `rank` not found.

- [ ] **Step 3: Write minimal implementation**

Insert above the test module in `compiler.rs`:

```rust
use std::collections::HashMap;

use crate::graph::{EdgeKind, Graph};

/// Stage 2 — Expand. Activation spreading from seeds along edges whose kind is
/// in `kinds`, with fan-effect: a source's contribution is divided by its TOTAL
/// out-degree. Iterates `hops` rounds scaled by `decay`.
pub fn expand(
    graph: &Graph,
    seeds: &HashMap<String, f64>,
    hops: usize,
    decay: f64,
    kinds: &[EdgeKind],
) -> HashMap<String, f64> {
    let mut scores = seeds.clone();
    let mut frontier = seeds.clone();
    for _ in 0..hops {
        let mut next: HashMap<String, f64> = HashMap::new();
        for (j, aj) in &frontier {
            let fan = graph.out_degree(j).max(1) as f64;
            for edge in graph.neighbors(j) {
                if !kinds.contains(&edge.kind) {
                    continue;
                }
                *next.entry(edge.target.clone()).or_default() += aj * edge.weight / fan * decay;
            }
        }
        for (k, v) in &next {
            *scores.entry(k.clone()).or_default() += v;
        }
        frontier = next;
    }
    scores
}

/// Stage 3 — Rank. Hub penalty by IN-degree (god-objects are referenced
/// everywhere): `final = score / (1 + beta * ln(1 + in_degree))`. Ties broken
/// deterministically by id so packing/metrics are reproducible.
pub fn rank(
    scores: &HashMap<String, f64>,
    in_degrees: &HashMap<String, usize>,
    beta: f64,
) -> Vec<(String, f64)> {
    let mut ranked: Vec<(String, f64)> = scores
        .iter()
        .map(|(id, s)| {
            let indeg = in_degrees.get(id).copied().unwrap_or(0) as f64;
            (id.clone(), s / (1.0 + beta * (1.0 + indeg).ln()))
        })
        .collect();
    ranked.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    ranked
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p lattice-uninode compiler::`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/lattice-uninode/src/compiler.rs
git commit -m "feat(uninode): expand (edge-kind filtered, fan-effect) + rank (in-degree, deterministic)"
```

---

## Task 7: Eval metrics — Recall@k, MAP, nDCG@k (path-based)

**Files:**
- Modify: `crates/lattice-uninode/src/eval.rs`

- [ ] **Step 1: Write the failing test**

Append to `eval.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn gold(items: &[&str]) -> HashSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn recall_at_k_counts_hits_in_top_k() {
        let ranked = vec!["a".to_string(), "x".to_string(), "b".to_string()];
        let g = gold(&["a", "b", "c"]);
        assert!((recall_at_k(&ranked, &g, 1) - 1.0 / 3.0).abs() < 1e-9); // a
        assert!((recall_at_k(&ranked, &g, 3) - 2.0 / 3.0).abs() < 1e-9); // a,b
        assert_eq!(recall_at_k(&ranked, &gold(&[]), 3), 1.0); // empty gold
    }

    #[test]
    fn average_precision_rewards_early_hits() {
        let g = gold(&["a", "b"]);
        // hits at ranks 1 and 2 → AP = (1/1 + 2/2)/2 = 1.0
        assert!((average_precision(&["a".into(), "b".into(), "c".into()], &g) - 1.0).abs() < 1e-9);
        // hits at ranks 1 and 3 → AP = (1/1 + 2/3)/2 = 0.8333...
        let ap = average_precision(&["a".into(), "x".into(), "b".into()], &g);
        assert!((ap - (1.0 + 2.0 / 3.0) / 2.0).abs() < 1e-9);
    }

    #[test]
    fn ndcg_at_k_is_one_for_perfect_prefix() {
        let g = gold(&["a", "b"]);
        assert!((ndcg_at_k(&["a".into(), "b".into(), "c".into()], &g, 10) - 1.0).abs() < 1e-9);
        // reversed-ish: relevant items later → < 1
        assert!(ndcg_at_k(&["x".into(), "a".into(), "b".into()], &g, 10) < 1.0);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p lattice-uninode eval::`
Expected: FAIL — metric functions not found.

- [ ] **Step 3: Write minimal implementation**

Insert above the test module in `eval.rs`:

```rust
use std::collections::HashSet;

/// Fraction of gold items appearing in the top-k of the ranked list.
pub fn recall_at_k(ranked: &[String], gold: &HashSet<String>, k: usize) -> f64 {
    if gold.is_empty() {
        return 1.0;
    }
    let hits = ranked.iter().take(k).filter(|r| gold.contains(*r)).count();
    hits as f64 / gold.len() as f64
}

/// Average precision over the full ranked list (the per-query term of MAP).
pub fn average_precision(ranked: &[String], gold: &HashSet<String>) -> f64 {
    if gold.is_empty() {
        return 1.0;
    }
    let mut hits = 0.0;
    let mut sum = 0.0;
    for (i, r) in ranked.iter().enumerate() {
        if gold.contains(r) {
            hits += 1.0;
            sum += hits / (i as f64 + 1.0);
        }
    }
    sum / gold.len() as f64
}

/// Normalized DCG at k with binary relevance.
pub fn ndcg_at_k(ranked: &[String], gold: &HashSet<String>, k: usize) -> f64 {
    if gold.is_empty() {
        return 1.0;
    }
    let mut dcg = 0.0;
    for (i, r) in ranked.iter().take(k).enumerate() {
        if gold.contains(r) {
            dcg += 1.0 / ((i as f64 + 2.0).log2());
        }
    }
    let ideal = gold.len().min(k);
    let mut idcg = 0.0;
    for i in 0..ideal {
        idcg += 1.0 / ((i as f64 + 2.0).log2());
    }
    if idcg == 0.0 { 0.0 } else { dcg / idcg }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p lattice-uninode eval::`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/lattice-uninode/src/eval.rs
git commit -m "feat(uninode): eval metrics — Recall@k, MAP, nDCG@k"
```

---

## Task 8: LCA data export + repo prep scripts (Python/shell, one-time)

**Files:**
- Create: `scripts/export_lca.py`
- Create: `scripts/prepare_repos.sh`

These are run once (Prerequisites P1/P2); they are not part of the Rust build/test. Validation is running them and inspecting `tasks.jsonl`.

- [ ] **Step 1: Write the export script**

Create `scripts/export_lca.py`:

```python
# Copyright 2026 Ast Rin
# SPDX-License-Identifier: Apache-2.0
"""Export a Long Code Arena bug-localization subset to tasks.jsonl.

Each line: {id, repo_owner, repo_name, base_sha, query, gold_files}.
`changed_files` in LCA is a JSON-encoded string list; gold_files is parsed and
filtered to code files (drops obvious mechanical paths).
"""
import argparse
import json

from datasets import load_dataset

MECHANICAL = ("CHANGELOG", "lock", ".md", ".txt", ".cfg", ".toml", ".lock")


def parse_changed(raw: str) -> list[str]:
    try:
        files = json.loads(raw)
    except (json.JSONDecodeError, TypeError):
        files = [p for p in str(raw).splitlines() if p.strip()]
    return [f for f in files if not f.endswith(MECHANICAL)]


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--config", default="py")
    ap.add_argument("--split", default="test")
    ap.add_argument("--limit", type=int, default=40)
    ap.add_argument("--out", default="data/lca/tasks.jsonl")
    args = ap.parse_args()

    ds = load_dataset("JetBrains-Research/lca-bug-localization", args.config, split=args.split)

    import os
    os.makedirs(os.path.dirname(args.out), exist_ok=True)
    written = 0
    with open(args.out, "w") as fh:
        for row in ds:
            gold = parse_changed(row["changed_files"])
            if not gold:
                continue
            query = f"{row.get('issue_title','')}\n\n{row.get('issue_body','')}".strip()
            rec = {
                "id": row["id"],
                "repo_owner": row["repo_owner"],
                "repo_name": row["repo_name"],
                "base_sha": row["base_sha"],
                "query": query,
                "gold_files": gold,
            }
            fh.write(json.dumps(rec) + "\n")
            written += 1
            if written >= args.limit:
                break
    print(f"wrote {written} tasks to {args.out}")


if __name__ == "__main__":
    main()
```

- [ ] **Step 2: Write the repo-prep script**

Create `scripts/prepare_repos.sh`:

```bash
#!/usr/bin/env bash
# Copyright 2026 Ast Rin
# SPDX-License-Identifier: Apache-2.0
# Clone each task's repo and check out its pre-fix base_sha.
# Usage: bash scripts/prepare_repos.sh data/lca/tasks.jsonl repos/
set -euo pipefail
TASKS="${1:?tasks.jsonl}"
DEST="${2:?repos dir}"
mkdir -p "$DEST"

# Requires jq for robust JSON field extraction.
while IFS= read -r line; do
  owner=$(printf '%s' "$line" | jq -r .repo_owner)
  name=$(printf '%s' "$line" | jq -r .repo_name)
  sha=$(printf '%s' "$line" | jq -r .base_sha)
  dir="$DEST/${owner}__${name}__${sha:0:10}"
  if [ -d "$dir/.git" ]; then continue; fi
  echo "preparing $owner/$name @ ${sha:0:10}"
  git clone -q "https://github.com/${owner}/${name}.git" "$dir" || { echo "clone failed: $owner/$name"; continue; }
  git -C "$dir" checkout -q "$sha" || { echo "checkout failed: $owner/$name@$sha"; rm -rf "$dir"; }
done < "$TASKS"
echo "done"
```

- [ ] **Step 3: Run the export + inspect the real schema**

Run:
```bash
pip install datasets jq 2>/dev/null || true
python scripts/export_lca.py --config py --split test --limit 40 --out data/lca/tasks.jsonl
head -1 data/lca/tasks.jsonl | python -m json.tool
```
Expected: prints `wrote N tasks ...` and one well-formed record with non-empty `query`, a `base_sha` (40-hex), and a non-empty `gold_files` array. **This step is the schema verification** — if field names differ from LCA's published schema (`issue_title`/`issue_body`/`base_sha`/`changed_files`/`repo_owner`/`repo_name`), fix `export_lca.py` here before proceeding.

- [ ] **Step 4: Prepare repos (subset)**

Run: `bash scripts/prepare_repos.sh data/lca/tasks.jsonl repos/`
Expected: clones the subset's repos under `repos/<owner>__<name>__<sha10>/`, each at its `base_sha`. Some clones may fail (deleted/renamed repos) — those tasks are skipped by the eval (Task 9).

- [ ] **Step 5: Commit the scripts**

```bash
git add scripts/export_lca.py scripts/prepare_repos.sh
git commit -m "feat(uninode): LCA export + repo-prep scripts"
```

---

## Task 9: Eval binary — B/C1/C2 ablation over LCA, metrics + GO check

**Files:**
- Modify: `crates/lattice-uninode/src/bin/eval.rs`

- [ ] **Step 1: Write the implementation (binary; validated by a smoke run)**

Replace `crates/lattice-uninode/src/bin/eval.rs` with:

```rust
// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! uninode-eval (v2) — ablation over Long Code Arena bug-localization.
//!
//! For each task: open the repo checked out at base_sha, build the File graph,
//! BM25-score files against the issue, then compare three arms by Recall@k /
//! MAP / nDCG against the gold changed files:
//!   B  = BM25 ranking alone
//!   C1 = BM25 seeds + sibling expansion
//!   C2 = BM25 seeds + sibling + cochange expansion
//! Co-change is leave-one-out for free: HEAD = base_sha, so only pre-fix history
//! is walked.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use clap::Parser;
use lattice_uninode::bm25::bm25_scores;
use lattice_uninode::cochange::add_cochange_edges;
use lattice_uninode::compiler::{expand, rank};
use lattice_uninode::eval::{average_precision, ndcg_at_k, recall_at_k};
use lattice_uninode::graph::{EdgeKind, Graph};
use lattice_uninode::scanner::scan_repo;

#[derive(serde::Deserialize)]
struct Task {
    repo_owner: String,
    repo_name: String,
    base_sha: String,
    query: String,
    gold_files: Vec<String>,
}

#[derive(Parser)]
#[command(about = "Ablation eval of uninode context selection on LCA bug-localization")]
struct Args {
    #[arg(long, default_value = "data/lca/tasks.jsonl")]
    tasks: PathBuf,
    #[arg(long, default_value = "repos")]
    repos_dir: PathBuf,
    #[arg(long, default_value_t = 500)]
    history: usize,
    #[arg(long, default_value_t = 2)]
    hops: usize,
    #[arg(long, default_value_t = 0.7)]
    decay: f64,
    #[arg(long, default_value_t = 1.0)]
    beta: f64,
}

#[derive(Default)]
struct Agg {
    r5: f64,
    r10: f64,
    map: f64,
    ndcg: f64,
    n: f64,
}

impl Agg {
    fn add(&mut self, ranked: &[String], gold: &HashSet<String>) {
        self.r5 += recall_at_k(ranked, gold, 5);
        self.r10 += recall_at_k(ranked, gold, 10);
        self.map += average_precision(ranked, gold);
        self.ndcg += ndcg_at_k(ranked, gold, 10);
        self.n += 1.0;
    }
    fn line(&self, name: &str) -> String {
        let n = self.n.max(1.0);
        format!("{name:<4} R@5={:.3}  R@10={:.3}  MAP={:.3}  nDCG@10={:.3}", self.r5 / n, self.r10 / n, self.map / n, self.ndcg / n)
    }
}

fn ranked_paths(graph: &Graph, scores: &HashMap<String, f64>) -> Vec<String> {
    let indeg = graph.in_degrees();
    rank(scores, &indeg, 1.0)
        .into_iter()
        .filter_map(|(id, _)| graph.nodes.get(&id).map(|n| n.path.clone()))
        .collect()
}

fn run_task(args: &Args, task: &Task) -> Option<(Vec<String>, Vec<String>, Vec<String>)> {
    let dir = args
        .repos_dir
        .join(format!("{}__{}__{}", task.repo_owner, task.repo_name, &task.base_sha[..10.min(task.base_sha.len())]));
    let repo = git2::Repository::open(&dir).ok()?;
    let workdir: &Path = repo.workdir()?;

    let mut graph = scan_repo(&repo).ok()?;
    // File contents for BM25 (path + content as the document).
    let mut docs: HashMap<String, String> = HashMap::new();
    for node in graph.nodes.values() {
        let body = std::fs::read_to_string(workdir.join(&node.path)).unwrap_or_default();
        docs.insert(node.id.clone(), format!("{}\n{}", node.path, body));
    }
    add_cochange_edges(&mut graph, &repo, args.history).ok()?;

    let seeds = bm25_scores(&docs, &task.query);
    if seeds.is_empty() {
        return None;
    }

    let b = ranked_paths(&graph, &seeds);
    let c1 = ranked_paths(&graph, &expand(&graph, &seeds, args.hops, args.decay, &[EdgeKind::Sibling]));
    let c2 = ranked_paths(&graph, &expand(&graph, &seeds, args.hops, args.decay, &[EdgeKind::Sibling, EdgeKind::CoChange]));
    Some((b, c1, c2))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let text = std::fs::read_to_string(&args.tasks)?;

    let (mut b, mut c1, mut c2) = (Agg::default(), Agg::default(), Agg::default());
    let mut evaluated = 0usize;
    let mut skipped = 0usize;

    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let task: Task = serde_json::from_str(line)?;
        let gold: HashSet<String> = task.gold_files.iter().cloned().collect();
        if gold.is_empty() {
            skipped += 1;
            continue;
        }
        match run_task(&args, &task) {
            Some((rb, rc1, rc2)) => {
                b.add(&rb, &gold);
                c1.add(&rc1, &gold);
                c2.add(&rc2, &gold);
                evaluated += 1;
            }
            None => skipped += 1,
        }
    }

    println!("evaluated {evaluated} tasks ({skipped} skipped: missing repo / no BM25 hit)");
    println!("{}", b.line("B"));
    println!("{}", c1.line("C1"));
    println!("{}", c2.line("C2"));

    // Pre-declared GO: C2 beats B on Recall@10 by >= 5 absolute points,
    // reaches >= 0.50 absolute, and does not regress nDCG@10.
    let n = b.n.max(1.0);
    let (b_r10, c2_r10) = (b.r10 / n, c2.r10 / n);
    let go = (c2_r10 - b_r10) >= 0.05 && c2_r10 >= 0.50 && (c2.ndcg / n) >= (b.ndcg / n);
    println!("\nGO criterion (C2 R@10 - B R@10 >= 0.05, C2 R@10 >= 0.50, nDCG not worse): {}", if go { "GO" } else { "NO-GO" });
    Ok(())
}
```

- [ ] **Step 2: Build**

Run: `cargo build -p lattice-uninode --bin uninode-eval`
Expected: compiles clean.

- [ ] **Step 3: Smoke-run against the prepared subset**

Run: `cargo run -p lattice-uninode --bin uninode-eval -- --tasks data/lca/tasks.jsonl --repos-dir repos`
Expected: prints `evaluated N tasks ...` then three metric rows (B/C1/C2) and a GO/NO-GO line. Values are data, not assertions — this proves the full pipeline runs end-to-end on real LCA tasks.

- [ ] **Step 4: Full crate test + clippy + fmt**

Run: `cargo test -p lattice-uninode && cargo clippy -p lattice-uninode -- -D warnings && cargo fmt -p lattice-uninode -- --check`
Expected: all PASS, no warnings, formatted.

- [ ] **Step 5: Commit**

```bash
git add crates/lattice-uninode/src/bin/eval.rs
git commit -m "feat(uninode): LCA ablation eval binary (B/C1/C2) with GO check"
```

---

## Interpreting the Result (the deliverable)

Run the ablation, then sweep a couple of parameters:

```bash
cargo run -q -p lattice-uninode --bin uninode-eval -- --hops 2 --beta 1.0
cargo run -q -p lattice-uninode --bin uninode-eval -- --hops 1 --beta 0.5
```

Read the three rows as an **ablation ladder**: B (BM25) → C1 (+sibling) → C2 (+cochange).

- **GO:** C2 improves Recall@10 by ≥ 5 absolute points over B, reaches ≥ 0.50, without hurting nDCG@10 — graph expansion adds retrieval value a strong lexical baseline does not. If C1 already captures most of the lift, sibling structure is doing the work; if the lift is C1→C2, co-change is. Either is a GO with a clear "why".
- **NO-GO:** C1 ≈ C2 ≈ B → expansion over these offline edges doesn't beat BM25 on this benchmark. Stop — no turn-execution code was touched, no Phase 34 dependency incurred.

Optional cross-check: re-run with `--config java` (different language, different repos) to see if the conclusion holds beyond Python.

---

## Self-Review

**1. Spec coverage (against codex's review + the LCA pivot):**
- codex [BUG] size 13 → Task 3 test uses 13 ✓
- codex [BLOCKER] leave-one-out test → eliminated; LCA snapshot at `base_sha` makes co-change leave-one-out structurally (Task 4 doc + Task 9), and Task 4's test uses a non-root commit so a real CoChange edge is asserted ✓
- codex [BUG] IDF used out-degree → rank now uses **in-degree** via `in_degrees()` (Task 2, Task 6) ✓
- codex [BUG] non-deterministic rank ties → tie-break by id (Task 6) ✓
- codex [BUG] co-change min-normalization → **Jaccard** (Task 4) ✓
- codex (data) rename handling → `DiffFindOptions::renames(true)` + `find_similar` (Task 4) ✓
- codex [BLOCKER] unequal packers / PathOnly recall credit → eliminated: eval ranks files and uses Recall@k/MAP/nDCG on the ranked list; no packer on the eval path; all arms share BM25 seeds and the same ranker (Task 9) ✓
- codex [BLOCKER] B sorted by blake3 id → eliminated: B is BM25-ranked (Task 9) ✓
- codex [BLOCKER] current-workdir vs historical truth → eliminated: graph is built from the repo **at base_sha** (Task 9) ✓
- codex [BUG] empty-diff recall=1.0 / precision unreported → empty gold skipped (Task 9); MAP+nDCG reported alongside recall ✓
- codex [QUESTION] commit-msg-as-query + mechanical files → query is the real **issue** (LCA); mechanical files filtered in export (Task 8) ✓
- codex [BLOCKER] success can fire at 0 recall → GO requires absolute Recall@10 ≥ 0.50 (Task 9) ✓
- codex [BLOCKER] Cargo.lock not staged → Task 1 Step 5 stages `Cargo.lock` ✓
- codex [BUG] est_tokens floor / read_to_string-as-empty → `pack`/tokens removed from the eval path entirely (not used for Recall@k); deferred to M4 ✓

**2. Placeholder scan:** every code step has complete code; scripts are complete; the only "verify against reality" point is Task 8 Step 3, which is an explicit schema-inspection gate, not a placeholder.

**3. Type consistency:** `Graph`/`Node`/`Edge`/`NodeKind`/`EdgeKind`/`node_id`/`in_degrees`/`out_degree`/`neighbors` (Task 2) are used unchanged in Tasks 3–9. `scan_repo(&Repository) -> Result<Graph>` (Task 3), `add_cochange_edges(&mut Graph, &Repository, usize)` (Task 4), `bm25_scores(&HashMap<String,String>, &str) -> HashMap<String,f64>` (Task 5), `expand(&Graph, &HashMap, usize, f64, &[EdgeKind])` + `rank(&HashMap, &HashMap<String,usize>, f64)` (Task 6), and `recall_at_k`/`average_precision`/`ndcg_at_k` (Task 7) match their call sites in the eval binary (Task 9). The `Task` struct fields match `export_lca.py`'s output keys (Task 8).
```

# AGENTS.md

Crate-local guidance for `lattice-session`.

## Role

This crate owns the conversation graph contract: sessions, transcript messages,
immutable turn commits, branch refs, detached HEAD state, graph diffs, usage
records, and the SQLite-backed session graph store.

## Sub-Architecture

- `src/graph.rs` owns public graph traits and data models.
- `src/lib.rs` owns crate errors, branch validation, re-exports, and test
  helpers.
- `src/sqlite_graph/schema.rs` owns SQLite schema and initialization.
- `src/sqlite_graph/session.rs` owns session creation/open/list/status logic.
- `src/sqlite_graph/commit.rs` owns turn commit writes and transcript handling.
- `src/sqlite_graph/refs.rs` owns branch, checkout, reset, and HEAD movement.
- `src/sqlite_graph/helpers.rs` owns shared query and topology helpers.
- `src/sqlite_graph/tests/` owns persistence and topology regression coverage.
- `src/git_graph/mod.rs` owns the GitSessionGraphStore struct, lazy repository handle, and SessionGraphStore trait impl delegation.
- `src/git_graph/session.rs` owns session metadata sidecar I/O (`.lattice/sessions/<id>/meta.json`), create/open/list/status logic.
- `src/git_graph/commit.rs` owns Git tree building, commit creation, transcript reconstruction via Revwalk, and manifest serialization.
- `src/git_graph/refs.rs` owns branch, checkout, reset, and diff via git2 ref APIs and merge_base.
- `src/git_graph/helpers.rs` owns shared git2 tree/blob/sidecar helpers and operational state (`ops/<oid>.json`) I/O.
- `src/git_graph/tests/` owns Git-backed persistence and topology regression coverage.

## Boundaries

- No LLM provider logic, profile discovery, CLI rendering, or native LANCET
  execution here.
- Keep branch/ref validation strict and deterministic.
- Keep session graph SQLite separate from CEP audit SQLite in `lancet-audit`.
- Treat commit IDs and canonical JSON hashing as durable graph contracts.
- Git session commits live under `refs/lattice/sessions/<id>/**`; candidate refs under `refs/lattice/candidates/<id>/<turn>`. Do not add remote push refspecs for `refs/lattice/**`.
- Mutable session metadata lives in sidecar files, never in Git objects.
- Multi-ref promotion transactions remain in `lattice-workspace`; the Git store performs single-ref operations only.

## Recent Issues

- OPEN L-20: `SqliteSessionGraphStore` uses a single shared `Connection` behind
  a `Mutex`, serializing reads and writes despite WAL.
- FIXED M-11: `head_transcript_impl` now traverses reachable commits
  topologically so merge-side messages are not dropped.
- FIXED M-20: large `IN (...)` queries are chunked to avoid SQLite bind limits.
- FIXED M-12: graph JSON hashing now canonicalizes object keys.
- WATCH: branch and detached HEAD behavior is heavily consumed by API and CLI.

## Next Actions

- Revisit L-20 before multi-session daemon/TUI/server work; evaluate connection
  pooling or per-task connections while preserving WAL behavior.
- Keep topology tests for linear, merge, diamond, branch reset, and detached
  HEAD paths when changing transcript or ref logic.
- Add schema migration coverage before changing persisted rows or JSON columns.
- Keep test helpers hidden and clearly non-production.
- Keep Git store topology tests for linear, merge, diamond, branch reset, and detached HEAD paths when changing transcript or ref logic.
- Keep sidecar I/O atomic (temp + rename) and test crash-recovery paths.

## Record Maintenance

- Update `Recent Issues` and `Next Actions` when graph topology, persistence,
  schema, refs, or branch validation changes.
- Record unresolved session graph risks in root `FINDINGS.md`.
- Record landed session fixes in root `FIX.md` with verification commands and
  changed files.
- If a performance issue stays acceptable for CLI-only use, state the trigger
  that would promote it.
- Keep this file near 80 lines by pruning stale closed bullets after `FIX.md`
  records them.

## Verification

- Local: `cargo test -p lattice-session`.
- Schema/ref/topology changes: also run affected `lattice-api` and
  `lattice-cli` session tests.
- Performance changes should include contention or chunking cases when relevant.

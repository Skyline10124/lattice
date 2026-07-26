# AGENTS.md

Crate-local guidance for `lattice-session`.

## Role

This crate owns the conversation graph contract: sessions, transcript messages,
immutable turn commits, branch refs, detached HEAD state, graph diffs, usage
records, and the **git-backed** session graph store (`GitSessionGraphStore`).
The previous SQLite-backed store and its one-shot SQLite→Git migration path
were retired; the git-backed store is the only production path (gix).

## Sub-Architecture

- `src/graph.rs` owns public graph traits and data models.
- `src/lib.rs` owns crate errors, branch validation, re-exports, and test
  helpers.
- `src/git_graph/mod.rs` owns the `GitSessionGraphStore` struct, lazy repository
  handle, and `SessionGraphStore` trait impl delegation.
- `src/git_graph/session.rs` owns session metadata sidecar I/O
  (`.lattice/sessions/<id>/meta.json`), create/open/list/status logic.
- `src/git_graph/commit.rs` owns Git tree building, commit creation, transcript
  reconstruction via Revwalk, and manifest serialization.
- `src/git_graph/refs.rs` owns branch, checkout, reset, and diff via gix ref
  APIs and `merge_base`.
- `src/git_graph/helpers.rs` owns shared gix tree/blob/sidecar helpers.
- `src/git_graph/tests/` owns Git-backed persistence and topology regression
  coverage.
- `src/test_utils.rs` (feature `test-utils`) owns shared gix fixture helpers
  for downstream crate tests.
- #40 removed `src/promotion.rs` (promoted-commit builder) and the
  `set_snapshot_lost`/`get_snapshot_lost` retained-snapshot ops-state chain
  (`ops/<oid>.json`); both only served the deleted Upper promotion path.

## Boundaries

- No LLM provider logic, profile discovery, CLI rendering, or native LANCET
  execution here.
- Keep branch/ref validation strict and deterministic.
- The session graph is git-backed and separate from CEP audit SQLite in
  `lancet-audit`. Do not reintroduce a SQLite session graph store.
- Treat commit IDs and canonical JSON hashing as durable graph contracts.
- Git session commits live under `refs/lattice/sessions/<id>/**`; candidate refs
  under `refs/lattice/candidates/<id>/<turn>`. Do not add remote push refspecs
  for `refs/lattice/**`.
- Mutable session metadata lives in sidecar files, never in Git objects.
- The Git store performs single-ref operations only; the `lattice-workspace`
  multi-ref promotion transaction surface was deleted with that crate in #40.

## Recent Issues

- ADDED #63: `TurnCommitInput`/`TurnCommit` carry `workspace_history_oid` and
  the git manifest persists it (`skip_serializing_if`, so history-less turns
  stay byte-identical to pre-#63 manifests). It is manifest-only — no
  canonical seed or turn-id derivation changed — and `get_turn_commit`
  re-reads it from the commit tree, so it survives session reopen, branch
  movement, and process restart. The private history itself lives in
  `lancet-runtime`, never in this store and never in the project `.git`.
- FIXED M-11: `head_transcript_impl` now traverses reachable commits
  topologically so merge-side messages are not dropped.
- FIXED M-12: graph JSON hashing now canonicalizes object keys.
- WATCH: branch and detached HEAD behavior is heavily consumed by API and CLI.

## Next Actions

- Keep topology tests for linear, merge, diamond, branch reset, and detached
  HEAD paths when changing transcript or ref logic.
- Keep test helpers hidden and clearly non-production.
- Keep Git store topology tests for linear, merge, diamond, branch reset, and
  detached HEAD paths when changing transcript or ref logic.
- Keep sidecar I/O atomic (temp + rename) and test crash-recovery paths.
- Do not reintroduce SQLite session storage or a git2 dependency; keep the
  store on gix.
- Keep `workspace_history_oid` manifest-only and optional; adding it to the
  canonical seed would move every turn id. Note `deny_unknown_fields`: an
  older binary cannot read a manifest that carries the field.

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

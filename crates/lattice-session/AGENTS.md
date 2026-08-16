# AGENTS.md

Crate-local guidance for `lattice-session`.

## Role

This crate owns the conversation graph contract: sessions, transcript messages,
immutable turn commits, branch refs, detached HEAD state, graph diffs, usage
records, and the **git-backed** session graph store (`GitSessionGraphStore`).
The previous SQLite-backed store and its one-shot SQLite→Git migration path
were retired; the git-backed store is the only production path (gix).

Since #142 the pure-data transcript types (`GraphMessage`, `GraphToolCall`,
`HeadTranscript`) and the usage contract (`TurnUsage`, a rename of
`lattice_conversation::TokenUsage`) are owned by `lattice-conversation` and
re-exported here unchanged; this crate remains the persistence owner. Do not
edit the re-exported serde shapes in place — they are frozen for committed
payload compatibility.

## Sub-Architecture

- `src/graph.rs` owns public graph traits and data models.
- `src/lib.rs` owns crate errors, branch validation, re-exports, and test
  helpers.
- `src/git_graph/mod.rs` owns the `GitSessionGraphStore` struct, lazy repository
  handle, and `SessionGraphStore` trait impl delegation.
- `src/git_graph/session.rs` owns create/open/list/status over immutable
  `SessionState` objects (no sidecars since #143).
- `src/git_graph/state.rs` owns the `SessionState` payload, the session ref
  namespace (`state`, `heads/<branch>`, candidates), and state publication.
- `src/git_graph/commit.rs` owns Git tree building, commit creation, transcript
  reconstruction via Revwalk, and manifest serialization.
- `src/git_graph/refs.rs` owns branch, checkout, reset, and diff over
  session-scoped refs and `lattice-store`'s `merge_base`.
- `src/git_graph/pending.rs` owns the PendingTurn journal (#144): the
  `active-turn`/`turns/<TurnId>` refs, CAS snapshot publication over the
  `lattice-store` kernel, ResultDraft publication/classification, and the
  one final ref transaction (head advance + turn pin + state update +
  active-turn delete, all-or-nothing).
- `src/git_graph/helpers.rs` owns shared manifest/tree/JSONL helpers.
- `src/git_graph/tests/` owns Git-backed persistence and topology regression
  coverage; `pending_journal.rs` covers the journal/finalization mechanics,
  `pending_crash.rs` the crash-window fixtures, and `pending_fixtures.rs`
  the shared deterministic builders.
- `src/test_utils.rs` (feature `test-utils`) owns shared gix fixture helpers
  for downstream crate tests.
- #62 (Codeberg #40) removed `src/promotion.rs` (promoted-commit builder) and
  the `set_snapshot_lost`/`get_snapshot_lost` retained-snapshot ops-state chain
  (`ops/<oid>.json`); both only served the deleted Upper promotion path.

## Boundaries

- No LLM provider logic, profile discovery, CLI rendering, or native LANCET
  execution here.
- Keep branch/ref validation strict and deterministic.
- The session graph is git-backed and separate from CEP audit SQLite in
  `lancet-audit`. Do not reintroduce a SQLite session graph store.
- Treat commit IDs and canonical JSON hashing as durable graph contracts.
- Session refs live in the global `$XDG_STATE_HOME/lattice/sessions.git`:
  `refs/lattice/sessions/<id>/state`, `refs/lattice/sessions/<id>/heads/<branch>`,
  candidates under `refs/lattice/candidates/<id>/<turn>`, the direct
  `active-turn` PendingTurn authority, and the create-once
  `turns/<TurnId>` permanent pins (#144). Do not add remote push refspecs
  for `refs/lattice/**`.
- Since #143 every session fact is an immutable `SessionState` commit; mutable
  `.lattice/sessions/**` sidecars are gone, with no reader, importer, or second
  authority. A Session is permanently bound to one path-derived
  `ProjectStoreKey` supplied by the composition layer — this crate never
  derives it from the process cwd.
- The store never initializes on open: `GitSessionGraphStore::open` fails
  closed (`NotInitialized`/`Corruption`), and `create` is reserved for the
  `session new` composition path.
- Every mutation takes the per-session execution lock
  (`locks/sessions/<id>.lock`) and then the repository write lock, in that
  order. `status` takes a non-blocking shared hold and reports `SessionBusy`.
  Never open two repository write locks together.
- Publication is one multi-ref transaction plus one `fsync_pending` barrier:
  conversation head, session state, and any candidate pin advance together or
  not at all.
- ADR 0005 keeps Session graph and PendingTurn semantics in this crate while
  #143 moved shared Git durability mechanics into `lattice-store`. #144 added
  one durable active Turn per session (`src/pending/` contract + reducer,
  `git_graph/pending.rs` store), immutable CAS stages, ResultDraft
  publication classification, and the pure reconciliation reducer against
  synthetic Invocation facts. Do not put InvocationStore/evidence semantics
  here (that is #139-A/#139-B), wire the journal into the production turn
  loop, or redispatch model/Tool work while reopening a PendingTurn. The
  journal's typed `TurnOutcome`/`ClosureOrigin` is the outcome authority;
  the conversation manifest's status/disposition/terminal_reason are a
  legacy projection until #139-B deletes them.

## Recent Issues

- ADDED #144: `src/pending/` (contract + reducer) and
  `src/git_graph/pending.rs` (journal store) implement the durable
  PendingTurn CAS journal, ResultDraft classification, the final ref
  transaction, and `reconcile()` against synthetic Invocation facts.
  Added the `lattice-session -> lancet-protocol` edge (ADR 0005 §19);
  `SessionError::JournalCorruption` is the typed damage error. Journal
  topology: `turns/<TurnId> -> TurnCommit -> Committed -> predecessor`,
  with the chain root grafted onto the frozen conversation tip.
  Acceptance lives in `pending*` tests under `src/git_graph/tests/` and
  inline reducer tests.
- ADDED #143: the store is built on the `lattice-store` kernel. Sessions live
  in the global `sessions.git`; `SessionState` commits replaced sidecars;
  `create_session` takes a `ProjectStoreKey`; `SessionGraphStore` gained
  `lock_execution`; `SessionError` gained `SessionBusy` and `Store`.
  Acceptance lives in `src/git_graph/tests/state_and_locks.rs`.
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

- Keep Git store topology tests for linear, merge, diamond, branch reset, and
  detached HEAD paths when changing transcript or ref logic.
- Keep test helpers hidden and clearly non-production.
- Do not reintroduce sidecar metadata, a sidecar importer, SQLite session
  storage, or a git2 dependency; durability primitives belong to
  `lattice-store`, and this crate must not hand-roll them.
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

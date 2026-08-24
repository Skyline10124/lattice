# AGENTS.md

Crate-local guidance for `lattice-session`.

## Role

This crate owns the conversation graph contract: sessions, transcript messages,
immutable turn commits, branch refs, detached HEAD state, graph diffs, usage
records, and the **git-backed** session graph store (`GitSessionGraphStore`).
The previous SQLite-backed store and migration path are deleted; the Git store is the only production path.

Since #142 the pure-data transcript types (`NativeMessage<BoundToolCall>` and `HeadTranscript`) and the usage contract (`TurnUsage`, a rename of
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
  active-turn delete, all-or-nothing). #139-A also adds the constrained
  same-stage `revise_binding_candidates` CAS used only for clean UUID
  collisions: candidate IDs and binding digests may change; structural binding
  fields may not.
- `src/git_graph/helpers.rs` owns shared manifest/tree/JSONL helpers.
- `src/git_graph/validation.rs` owns the derived per-session strict-open
  projection (`refs/lattice/session-validation/<id>`). It is authenticated
  to the live session ref set, rebuilt on each session mutation in the same
  kernel ref transaction, and never written on open. A miss or mismatch
  forces a full per-session object validation before the handle is returned.
- `src/git_graph/tests/` owns Git-backed persistence and topology regression
  coverage; `pending_journal.rs` covers the journal/finalization mechanics
  plus the strict-open, lineage, and legacy-projection migration guards,
  `pending_crash.rs` the crash-window fixtures (injected durability faults
  and one real SIGKILL child process), and `pending_fixtures.rs`
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
- The session graph is Git-backed and separate from the project-keyed Git InvocationStore in `lancet-audit`.
- Treat commit IDs and canonical JSON hashing as durable graph contracts.
- Session refs live in the global `$XDG_STATE_HOME/lattice/sessions.git`:
  `refs/lattice/sessions/<id>/state`, `refs/lattice/sessions/<id>/heads/<branch>`,
  candidates under `refs/lattice/candidates/<id>/<turn>`, the direct
  `active-turn` PendingTurn authority, and the create-once
  `turns/<TurnId>` permanent pins (#144), and a derived
  `refs/lattice/session-validation/<id>` projection used only to skip
  already-authenticated object parsing on strict open. Do not add remote
  push refspecs for `refs/lattice/**`.
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
  synthetic Invocation facts. Journal CAS publication relies on `cas_ref`'s
  object-then-CAS-then-ref barriers; callers must not add a second
  `fsync_pending` after success. Exact retry of a published-but-undurable
  snapshot still calls `fsync_pending` because it does not call `cas_ref`.
  InvocationStore/evidence semantics remain in `lancet-audit`; production
  composition and reconciliation live in `lattice-api`. Reopen never redispatches
  model or Tool work. Typed `TurnOutcome`/`ClosureOrigin` and the explicit workspace
  checkpoint are the sole v4 manifest authority; legacy projections are deleted.

## Recent Issues

- LANDED #139-B Commit 6: the production `TurnCoordinator` drives the PendingTurn journal against the Git InvocationStore. The existing
  PendingTurn correctness and structural regression tests remain mandatory.
  `session_journal_pin_ns` growth and durability performance qualification move
  to non-blocking follow-up #153; do not move invocation-ledger I/O into this
  crate while investigating it.
- ADDED #139-A: Invocation facts now carry mandatory `PlanKey` ownership and an
  exact candidate key-set contract, and `reconcile` returns a
  `ReconciliationDecision`: either a closure `ReconciliationPlan` or a
  `CandidateRevisionRequest`. A clean foreign-owned collision produces the
  request and no `TurnOutcome` at all, so a revision can never be mistaken for
  a closed Turn; the same collision at `Bound` stays typed corruption.
  `execute_candidate_revision` verifies the request against the pinned
  `active-turn` tip and CAS-publishes only fresh IDs plus their derived binding
  digests through `revise_binding_candidates`. Invocation ledger I/O remains in
  `lancet-audit`; this crate stays data/reducer/session-journal only and must
  not depend on `lancet-audit`.

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
- ADDED #144 review round 2: journal identity and lineage are cross-checked
  in depth. `StepClosedKind::ContinueModel` carries `model_step_ordinal`, so
  the transition contract decides step contiguity and the zero first attempt
  from the two snapshots alone. `PendingSnapshot::validate` cross-checks every
  binding/draft/candidate address against the journal's `SessionId`. Every
  journal read requires the payload `predecessor` and the commit's git parent
  to agree (the root's parent being its frozen conversation tip). Strict open
  validates the whole `active-turn` snapshot and the `turns/<TurnId>` pin's
  manifest identity. `advance_pending_turn` confirms an exact already-published
  candidate before failing a retry as an illegal transition, and `reconcile`
  fails closed when a tip carries no journal committer time. Real
  process-crash coverage is `pending_crash.rs`'s SIGKILL child fixture.
- ADDED #143: the store is built on the `lattice-store` kernel. Sessions live
  in the global `sessions.git`; `SessionState` commits replaced sidecars;
  `create_session` takes a `ProjectStoreKey`; `SessionGraphStore` gained
  `lock_execution`; `SessionError` gained `SessionBusy` and `Store`.
  Acceptance lives in `src/git_graph/tests/state_and_locks.rs`.
- LANDED #139-B Commit 6/7: every final Turn carries explicit typed
  `WorkspaceHistoryCheckpoint::{Empty, Oid}` in the v4 manifest and canonical
  identity. Final publication runs only through the PendingTurn transaction;
  direct commit builders and methods are available only under `test-utils`.
  Workspace history itself remains owned by `lancet-runtime`.
- FIXED M-11: `head_transcript_impl` now traverses reachable commits
  topologically so merge-side messages are not dropped.
- FIXED M-12: graph JSON hashing now canonicalizes object keys.
- WATCH: branch and detached HEAD behavior is heavily consumed by API and CLI.

## Next Actions

- Keep Git store topology tests for linear, merge, diamond, branch reset, and
  detached HEAD paths when changing transcript or ref logic.
- Keep test helpers hidden and clearly non-production. A consumer reaches
  `test-utils` from its `[dev-dependencies]` entry only — enabling it from
  `[dependencies]` cancels the `#[cfg]` gate and ships `init_fixture_repo` in
  the release binary. `cargo xtask lancet-boundary` fails on any workspace
  crate that does so (#88).
- Do not reintroduce sidecar metadata, a sidecar importer, SQLite session
  storage, or a git2 dependency; durability primitives belong to
  `lattice-store`, and this crate must not hand-roll them.
- Keep `workspace_history_checkpoint` explicit and typed (`Empty | Oid`) in the v4 manifest and canonical Turn identity.

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

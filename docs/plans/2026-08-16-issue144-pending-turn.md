# Issue #144 — Durable PendingTurn journal and deterministic reconciliation

Date: 2026-08-16
Issue: Skyline10124/lattice#144
ADR: `docs/adr/0005-durable-turn-invocation-and-private-git-stores.md` §9, §12, §14
Prerequisite handoff: #142 (`lancet-protocol` identities + `lattice-conversation`), #143 (`lattice-store` kernel + sessions.git migration)

## Scope

`lattice-session` gains the first-class PendingTurn journal, the direct
`active-turn` CAS authority, and the pure reconciliation reducer, validated
with synthetic `InvocationFacts`. No production InvocationStore, no Tool
dispatch, no FinalizeBudget numbers, no production JSON-RPC/numeric-ID/
SQLite/operator/dispatch changes.

## Placement

| Concern | Location |
|---|---|
| Stage contract, envelope, transition validation | `crates/lattice-session/src/pending/contract.rs` |
| `TurnStarted` frozen facts, catalog, limits | `crates/lattice-session/src/pending/config.rs` |
| Exact ordered binding plan | `crates/lattice-session/src/pending/binding.rs` |
| `ResultDraft` + publication classification | `crates/lattice-session/src/pending/draft.rs` |
| `StepClosing` cursor, `StepClosed` sum | `crates/lattice-session/src/pending/step.rs` |
| Typed `TurnOutcome`/`ClosureOrigin`/usage aggregate | `crates/lattice-session/src/pending/outcome.rs` |
| Synthetic invocation facts | `crates/lattice-session/src/pending/facts.rs` |
| Pure reducer | `crates/lattice-session/src/pending/reconcile.rs` |
| Store journal (refs, CAS, final transaction) | `crates/lattice-session/src/git_graph/pending.rs` |
| Journal crash fixtures | `crates/lattice-session/src/git_graph/tests/pending_crash.rs` |

Dependency delta (ADR 0005 §19): `ADD lattice-session -> lancet-protocol`.

Refs (already reserved by #143's domain validation):

```text
refs/lattice/sessions/<SessionId>/active-turn   -> current PendingSnapshot commit
refs/lattice/sessions/<SessionId>/turns/<TurnId> -> TurnCommit conversation commit (create-once pin)
```

## Journal topology

- Each snapshot is a commit with canonical `pending.json`; its git parent is
  the predecessor snapshot (predecessor chain). The `TurnStarted` snapshot's
  git parent is the frozen parent conversation tip, grafting the chain onto
  the conversation graph at acceptance.
- `Committed` is a journal commit whose parent is the `TurnClosing` snapshot;
  it is never published through `active-turn`.
- Final publication is one kernel `ref_transaction`:
  1. advance the conversation branch head (or detached state) to the
     TurnCommit commit;
  2. create `turns/<TurnId>` pointing at it;
  3. update `state` (timestamp; detached head target);
  4. delete `active-turn` under `MustMatch(tip)`.
- Git parentage of the TurnCommit conversation commit is
  `[Committed] ++ candidate.parent_ids[1..]`: `parent_ids[0]` (the frozen
  conversation tip) stays reachable through the journal chain root, and any
  merge side-parents keep their place. This makes the ADR arrow
  `turns/<TurnId> -> TurnCommit -> Committed -> predecessor` literal and
  keeps the journal chain reachable after `active-turn` is deleted.
- Transcript walks (`head_transcript`, `log`) skip journal commits (a commit
  carrying `pending.json` instead of `turn/manifest.json`); a commit carrying
  neither is corruption.

## Decisions not fixed by the issue/ADR (conservative readings)

1. **Reconciled `created_at`.** The issue forbids reconstructing a pinned
   candidate from a new clock or mutable input. For closures from stages
   before `TurnClosing` (no candidate pinned yet), reconciliation constructs
   the candidate once and pins the durable journal tip's own committer time
   (`PendingSnapshot.recorded_at`) — no new clock enters recovery. A live
   owner pins its real clock at pinning time.
2. **`SESSION_STORE_SCHEMA_VERSION` stays 1.** #143 forward-tolerated the
   `turns/*` and `active-turn` names; #144 extends domain validation to
   their content instead of bumping the schema (no-import hard cut).
3. **Reconciled usage aggregate is conservatively `Partial`.** `reconcile()`
   sees the tip snapshot, not the whole attempt history, so it can never
   prove `Complete`; the live coordinator derives `Complete | Partial` from
   the full attempt chain via `TurnUsageAggregate::derive`.
4. **One stage machine for reconcile closures.** A `BindingPlanned`/`Bound`
   reopen closure walks the same legal stages (`Bound` → `StepClosing`
   cursor advances → `StepClosed`) rather than a shortcut stage; the reducer
   emits the full ordered action list.
5. **Attempt-interruption reasons pass through.** A recorded
   `ModelAttemptInterrupted(Cancelled)` at reopen closes `Interrupted` with
   that durable reason (ADR 0005 §14 table); live cancellation closing the
   Turn as `Cancelled` is live-owner behavior, not a reopen closure.
6. **Facts vocabulary ownership.** The synthetic facts projection lives in
   `lattice-session` (reducer input contract); #139-A/#139-B maps the real
   InvocationStore into it rather than the reverse.

## Commit slices

1. `contract` — pending module types + validation + the
   `lattice-session -> lancet-protocol` edge.
2. `store` — journal persistence over `lattice-store`, draft
   publication/classification, final ref transaction.
3. `reducer` — pure `reconcile()` with exhaustive stage closures.
4. `crash tests` — fault fixtures across stages, binding windows, draft
   outcomes, final transaction kill points; docs/AGENTS updates.

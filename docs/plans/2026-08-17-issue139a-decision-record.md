# Issue #139-A — InvocationStore adapter and collision-aware facts

Date: 2026-08-17
Issue: Skyline10124/lattice#139-A
ADR: `docs/adr/0005-durable-turn-invocation-and-private-git-stores.md`
Prerequisites: #142 (typed identities and conversation data), #143 (`lattice-store`), #144 (PendingTurn journal and pure reconciliation)

## Scope

#139-A adds a non-production Git `InvocationStore` adapter and qualifies its
correctness and performance before the #139-B hard cut. It does not connect the
production Tool path, execute Tool effects, replace the SQLite operator adapter,
or add dual writes or legacy import reads. The production switch remains one
atomic #139-B change.

## Ownership and dependency decisions

> **Correction (2026-08-18, second independent review).** This section
> originally recorded a `lancet-audit -> lattice-session` production
> dependency and the delta
> `lancet-audit -> lattice-session,lattice-store,gix`. That decision was
> wrong: ADR 0005 §19's exact post-#139-B graph has no such edge, and the
> normal dependency made `lancet-api -> lancet-audit -> lattice-session` real,
> breaking LANCET's independent embeddability. The edge has been removed; the
> decisions below are the corrected ones.

- `lancet-audit` owns the invocation ledger schema. Its public surface returns
  audit-domain types only (`PinnedLedgerView`, `InvocationRecord`,
  `PlanReservation`); it does not name a session type.
- Projecting a pinned ledger view into
  `lattice_session::pending::InvocationFactsSnapshot`, running the pure
  reducer over it, and applying the resulting candidate revision are
  cross-domain composition. In #139-A they live in the non-production
  integration seam `crates/lancet-audit/tests/invocation_qualification.rs`,
  backed by a `lattice-session` **dev**-dependency. In #139-B the production
  home is `lattice-api`'s `TurnCoordinator`, which already sees both domains.
- `lattice-session` continues to own that facts snapshot, the pure reducer, and
  the session-domain journal I/O that applies a revision
  (`GitSessionGraphStore::execute_candidate_revision`); it must not depend on
  `lancet-audit`.
- `lattice-store` remains a domain-neutral Git/lock/durability kernel and does
  not absorb invocation or PendingTurn schemas.
- The #139-A production dependency delta is `lancet-audit -> lattice-store,gix`
  and nothing else. `cargo tree -p lancet-api -e normal` must contain no
  `lattice-session`.

## Facts and reservation authority

`InvocationFactsSnapshot.invocations` contains every queried candidate ID. Each
map value is `InvocationRecordFact { owner: PlanKey, lifecycle:
InvocationLifecycleFact }`; the owner is mandatory so the reducer can distinguish
an absent candidate, an exact reservation, and a candidate owned by another
plan without consulting an ambient store.

An exact reservation is one ordered `PlanReservation` carrying the complete
batch identities. `InvocationStore::reserve_batch` is all-or-nothing and accepts
an idempotent retry only when the existing reservation is byte-equal and in the
same order. A reordered, partial, differently bound, or foreign-owned batch
fails closed.

The reducer's plan classification is:

- no plan and no candidate records: `AbsentPlan`;
- any candidate owned by another `PlanKey`: `Collision`;
- one exact ordered plan plus exactly matching owned records: `ExactMatch`;
- every other combination: corruption.

`ExactMatch` requires key-set equality, not merely presence:
`invocations.keys()` must equal the durable plan's candidate set exactly. An
extra own-owned record, an extra foreign record, a missing record, or a
duplicate candidate in the plan all fail closed, so an unfaithful projection
cannot be laundered into an exact match by a caller's promise.

A clean collision produces a `CandidateRevisionRequest`, not a
`ReconciliationPlan`: `reconcile` returns a `ReconciliationDecision`, and the
revision variant carries no `TurnOutcome` at all, so a revision can never be
read as a closed Turn. (The first implementation returned an action-only plan
carrying a provisional `Interrupted(ProcessInterruptedDuringToolBinding)`
outcome; the second independent review rejected that on 2026-08-18 and it was
replaced by the decision type.) The request carries the exact session, Turn,
and byte-exact durable `BindingPlan`; the executor verifies it against the
pinned `active-turn` tip and the CAS pins that tip's OID.

The I/O owner generates fresh UUIDv4 candidates, rebuilds the complete
`BindingPlan`, and CAS-publishes that same-stage replacement through
`GitSessionGraphStore::execute_candidate_revision` /
`revise_binding_candidates`. The journal API permits only candidate UUID and
derived binding-digest changes; the message, order, addresses, methods,
descriptor identities, and argument digests remain fixed. Reusing any old
candidate or changing a structural field is rejected. A collision observed at
`Bound` is corruption, not a revision: `Bound` is published only after
`reserve_batch` confirms the entire exact plan durable.

## Ledger and read decisions

The private project store is `invocations.git` with one authoritative
`refs/lattice/invocations/ledger` data ref and one non-data
`refs/lattice/invocations/high-water` witness. Publication advances and durably
barriers the data ref before advancing the witness. A crash may therefore leave
the witness at an ancestor; reads accept that ordered state and write retries
repair it. A missing, divergent, or ledger-behind-witness state fails closed,
so moving the ledger ref to an otherwise valid ancestor is still detectable.
Each commit contains exactly one canonical versioned ledger event: empty genesis,
ordered batch reservation, or one lifecycle transition. Replaying the immutable
linear ancestry derives the current snapshot; persistent bytes therefore grow
linearly rather than rewriting the accumulated snapshot on every transition.
Lifecycle transitions are `Reserved -> DispatchStarted -> Terminal`, with the
reserved-to-terminal `NotExecuted` exception. Terminal records carry typed
`TerminalOutcome`, typed material availability, and versioned structured
admission/effects/denials evidence; no JSON string fields are domain authority.

Every public read pins an immutable ledger commit, validates a linear ancestry
to the versioned genesis event, rejects merge/truncated/deleted/rewound or
unreachable cursors, and replays query, tail, and PendingTurn facts from that
same pinned event sequence.

A write takes the repository flock before reading and CAS-publishing the next
event. An exact idempotent retry repeats the pending durability barrier; it does
not report success from a prior in-memory observation. The first write on an
open handle validates and replays the full lineage. Later cooperating writes
advance the cached replay state only through the new immutable event suffix,
including events published by another open handle; a rewound, divergent,
truncated, merged, or invalid suffix fails closed.

## Qualification decisions

Two ignored Btrfs harnesses produce 20-run raw distributions:

1. local ledger: 8 concurrent sessions x 20 steps x 8 invocations (1,280
   invocations and 2,720 ledger commits per run), followed by reopen/query/tail
   assertions;
2. result finalize: real PendingTurn journals driven through the full durable
   order. The closing loop's baseline is its own transition, so the order is
   `BindingPlanned -> exact reservation -> Bound -> StepClosing(cursor 0, no
   closures, no candidate)` and then, per binding `i`, `DispatchStarted ->
   StepClosing(cursor i + exact ResultDraft candidate) -> CleanAbsent |
   Durable | Uncertain-then-revalidated draft -> typed terminal + evidence ->
   StepClosing(cursor i+1, closure appended, candidate cleared)`, ending at
   `StepClosed(ContinueModel)`. Pinning a candidate and advancing the cursor
   are always separate transitions. Reduction over the projected invocation
   facts is checked mid-loop at `StepClosing` (the stage that validates them)
   as well as at the end, and every terminal must be a deterministic
   `Succeeded` with zero `Indeterminate`. (The first implementation published terminals while the
   PendingTurn was still `Bound`, which the reducer classifies as corruption;
   the second independent review rejected it on 2026-08-18 and the harness was
   rewritten. Its measurements were withdrawn with it.)

The local bundle is retained byte-for-byte at
`target/invocation-store-bundles/<timestamp>-<git-sha>/` and contains
`environment.json`, `source-history.bundle`, both raw JSONL groups,
`summary.json`, `summary.md`, and the human review `assessment.md`. Bundles are
ignored local artifacts rather than tracked source files. The runner pins
`TMPDIR` beneath the Btrfs workspace, verifies and records the
store filesystem separately, and executes both harnesses with the release profile.
It records repository-lock wait, append+fsync/barrier, operation and full-finalize
latencies, wall time, per-writer wall fairness, max consecutive wait, and CAS
retry count. CAS retries are a documented structural zero because cooperating
writers are serialized by the exclusive repository flock; no synthetic retry
loop is added solely to make the metric nonzero.

No `FinalizeBudget`, latency floor/multiplier, regression tolerance, or pass/fail
performance threshold is approved here. Those values may land only after both
20-run Btrfs groups finish, in a separate reviewable commit citing the retained
raw distributions. Samples are never deleted; outliers are annotated in place,
and an invalid environment causes a full-group rerun with the old bundle kept.
The evidence recorded on 2026-08-18 in
`docs/plans/2026-08-18-issue139a-qualification-approval.md` is now **partly
withdrawn** (see that record's STATUS block and FINDINGS L-GH139A-03): its
local-ledger group remains valid, while its result-finalize group, the 60 s
`FinalizeBudget`, and every result-finalize threshold were retracted because
the harness that produced them published Invocation terminals in the `Bound`
window. The corrected harness was then requalified on 2026-08-18 in bundle
`20260818T115511Z-4a51b5d42484`, and that record's Requalification section is
the authoritative source for the budget and every threshold.

## Deferred to #139-B

- production Tool dispatch and real Tool effects;
- the `lattice-api::TurnCoordinator` integration;
- SQLite operator adapter replacement and the single hard-cut identity switch;
- deletion of the legacy outcome projection tracked by L-GH144-02.

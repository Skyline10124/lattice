# Issue #139-B — Production durable Turn / Invocation hard cut

Status: implementation-ready plan

Date: 2026-08-23

Baseline: clean `main` at `8fad9810` (contains the authorized `a05090c8` #139-B
start point plus subsequently merged mainline work)

Issue: GitHub #139, production-wiring slice after merged #142, #143, #144,
and #139-A

## 1. Goal

Replace the current monolithic `lattice-agent::AgentLoop`, process-local numeric
CEP call identity, and SQLite audit/evidence authority with the accepted ADR
0005 production design in one hard cut:

- `lattice-api::TurnCoordinator` is the sole production Turn saga owner;
- `lattice-agent` performs one provider/model step and owns no persistence,
  invocation allocation, Tool execution, retry policy, reconciliation, or Turn
  commit;
- every Tool request carries a caller-allocated typed `InvocationId` from its
  durable `BindingPlan` through JSON-RPC, dispatch, cancellation, Component Host
  context, evidence, operator reads, and workspace history;
- `lancet-audit::InvocationStore` becomes the sole production invocation ledger
  and operator-read authority;
- `lattice-session` PendingTurn plus immutable ResultDraft material closes every
  crash window without resending a provider request or redispatching a Tool;
- the SQLite audit/evidence store, numeric `call_id`/`audit_id`/`evidence_id`
  identity family, legacy Turn outcome projection, and production
  `ProvisionalCompletionHook` are deleted rather than kept as fallbacks.

This is one branch and one PR with small verified commits. New code may exist
privately before the switch, but no commit may expose two production
coordinators, dual-write SQLite and Git, choose a backend by store presence, or
read/import a legacy durable format.

## 2. Authority and non-negotiable invariants

The normative source is
`docs/adr/0005-durable-turn-invocation-and-private-git-stores.md`, including its
accepted amendments and implementation-order sections. The issue body remains
checklist. This plan only fixes ordering, ownership, and review-sized commit
boundaries.

### 2.1 Durable ownership

| Domain | Owner after #139-B | Durable home |
| --- | --- | --- |
| conversation graph, PendingTurn, ResultDraft, final Turn commit | `lattice-session` | global `sessions.git` |
| reservation, dispatch and terminal invocation facts, typed Cell evidence, operator query/tail/replay | `lancet-audit` | project `invocations.git` |
| workspace mutation history and Rewind audit | `lancet-runtime` | project `workspace-history.git` |
| provider wire/replay projection | `lattice-llm` over `lattice-conversation` | conversation data only |
| cross-store orchestration | `lattice-api::TurnCoordinator` / separate `RewindCoordinator` | no fourth store |

`lancet-audit` must not depend on `lattice-session`. `lattice-api` projects a
pinned audit-domain ledger view into
`lattice_session::pending::InvocationFactsSnapshot` and invokes the pure reducer.
`lattice-store` remains schema-free Git/lock/durability mechanics.

### 2.2 Lock order and crash model

A full Turn holds `SessionExecutionLock`, but no repository write lock is held
across an await or a call into another repository. The only legal nesting is:

```text
SessionExecutionLock
  -> optional WorkspaceOperationLock
     -> at most one repository write lock
```

Every blocking Git/filesystem operation invoked from async production code runs
through `spawn_blocking` or an equivalent narrow blocking adapter. There is no
cross-repository transaction, two-phase commit, intent store, PendingTurn-based
Rewind journal, or simultaneous repository write lock.

### 2.3 One live execution path

`stream_turn` remains the single live execution core. `run_turn` continues to
drain that stream through `RunTurnCollector`. The coordinator replaces the
orchestration inside `stream_turn`; do not add `stream_turn_v2`, a feature flag,
or a fallback AgentLoop path.

Lower-level LANCET runtime/transport entry points remain available only for
internal tests and must receive explicit typed binding facts. They are not a
Standalone/direct product invocation contract. Reservation-gated dispatch and
the coordinator cutover land in the same production switch. Before that commit,
private helpers and contracts may compile, but the production AgentLoop path
remains numeric/SQLite and the temporary `lattice-api` adapter must not reserve,
publish `DispatchStarted`, or become a second saga owner. #139-B adds no inbound
client-to-server JSON-RPC notification ingress: existing notifications remain
server-to-client Tool events. The non-allocating/no-dispatch notification rule
is therefore enforced by structural absence tests, not a new untrusted frame
parser.

### 2.4 Hard-cut compatibility policy

This slice deliberately does not migrate development state:

- no SQLite audit/evidence import, dual read, or dual write;
- no legacy session outcome reader/projection after its deletion;
- no numeric WorkspaceHistory `call_id` compatibility reader;
- no old Rewind row migration;
- no synthetic provider Tool Call IDs.

After the durable schema switches, old local development state is removed and
rebuilt explicitly per section 9. The program fails closed with a clear
operator-facing diagnostic when strict open observes an old schema.

## 3. Accepted design decisions from the implementation grill

The owner accepted Q1–Q24 on 2026-08-23. These decisions are part of the plan:

1. One branch, one PR, small commits, exactly one production switch.
2. Tests replace `AuditBacking::InMemory` with `TempDir` plus real Git stores;
   there is no in-memory InvocationStore backend.
3. `FinalizeBudget` is a private fixed policy: a monotonic 60-second elapsed
   budget anchored immediately after the executor returns. It includes
   repository-lock acquisition, exact uncertain-draft revalidation, terminal /
   evidence publication attempts, and retry delays. The first attempt is
   immediate; exponential backoff starts at 25 ms and is capped at 1 second; no
   new attempt starts after the deadline. It is neither profile configuration
   nor public API. Corruption and identity/digest mismatch never retry.
4. Binding collisions republish a complete `BindingPlanned` revision. The I/O
   owner generates fresh UUIDv4 candidates; the existing pure reducer returns
   `ReconciliationDecision::ReviseCandidates`; `reserve_batch` remains
   all-or-nothing. Revision is legal only after an exact pinned ledger view
   proves the previous candidate plan absent and the candidates owned by another
   plan. An uncertain, partial, or different reservation retains the durable
   candidates and fails closed; it never revises them.
5. `DispatchStarted` is published at server acceptance after validating the
   exact durable reservation and before executor handoff. A durability failure
   prevents execution.
6. Result ordering is:

   ```text
   Tool return
     -> compute canonical content/digest/length/candidate OID
     -> publish StepClosing with the exact candidate
     -> publish/classify/revalidate ResultDraft
     -> publish invocation terminal + typed evidence
     -> publish ToolResult closure / next journal cursor
     -> TurnClosing
     -> final Turn commit transaction
   ```

7. Reopen runs bounded deterministic reconciliation before any new Turn. It
   never calls the provider, runs completion hooks, or redispatches a Tool.
8. Cancellation is classified from durable facts, not from receipt of a signal
   or an error string: pre-dispatch becomes `NotExecuted`, a proven executor
   cancellation becomes `Cancelled`, and post-dispatch uncertainty becomes
   `Indeterminate(MayHaveOccurred)`. Later bindings close in order as
   `NotExecuted`.
9. A missing provider call ID is a valid conversation fact. Same-provider replay
   preserves its absence. If a provider cannot encode a later ToolResult, the
   next request fails with a typed projection error at the `lattice-llm` wire
   boundary; execution history is not rewritten and no ID is synthesized.
10. Provider opening retry is allowed only before the first stream event, after
    persisting the failed attempt, with a new `ModelAttemptKey`. A provider is
    never retried after a stream event. A Tool is never automatically resent.
11. JSON-RPC request IDs, events, pending maps, cancellation, public status, and
    logs use canonical `InvocationId`. Notifications may control only an
    existing reserved invocation and cannot allocate, reserve, or dispatch one.
12. Logging and metrics may include typed IDs, method/descriptor identity,
    lifecycle stage, outcome class, retry/budget timing, byte lengths/digests,
    lock wait, CAS count, and ledger cursor. They must not include arguments,
    result/fetched content, provider payload or opaque replay state,
    credentials, or provider call IDs as LANCET correlation. Operator DTOs obey
    the same content boundary: they expose closed outcomes, typed evidence,
    digests and byte lengths, never Tool arguments, Tool result material,
    provider payload, opaque replay state, credentials, or internal metadata
    blobs.
13. `RewindAuditRecord`, schema, and read/write APIs move from `lancet-audit` to
    `lancet-runtime`. Records live in `workspace-history.git` on an independent
    append-only chain at `refs/lattice/rewind-audit/log`:
    - its commits are not ancestors of mutation `current`;
    - CAS permits only fast-forward append;
    - Rewind never moves/deletes it and retention never expires it;
   - a project Rewind first holds the full Session's `SessionExecutionLock`, then
     acquires `WorkspaceOperationLock` exactly once in the product-level
     `lattice-api::RewindCoordinator` and holds it across
     workspace preflight/restore, workspace-history ref movement, Session ref
     movement, and the later audit append; lower layers accept the existing
     guard and never reacquire it;
   - the audit append is a separate short repository transaction after the
     Session ref attempt and before returning any success or structured conflict;
   - every conflict, partial failure, Session-ref failure, and success is
     recorded while the operation lock is still held; audit publication failure
     returns a typed partial-state error and never reports Rewind success;
    - repository-level loss may make both history and failure recording
      unavailable; that shared failure domain is accepted;
    - no fourth repository and no Rewind records in `invocations.git`.
14. The remote Rocky Linux/ext4 run is a merge-time correctness smoke, not a
    performance campaign or an everyday CI lane. #153 remains diagnostic-only
    and non-blocking.

## 4. Target production flow

### 4.1 Start or recover

1. Canonicalize the project root and strictly open `sessions.git`,
   `invocations.git`, and `workspace-history.git` for the selected session.
2. Acquire the session's full-Turn `SessionExecutionLock`.
3. Read `active-turn`.
4. If it exists, pin one Invocation ledger view, project the required IDs into
   `InvocationFactsSnapshot`, classify the pinned ResultDraft where relevant,
   call `reconcile`, and execute only its deterministic closure plan. A crash at
   `BindingPlanned` with a proven-absent exact plan may execute the existing
   `ReconciliationAction::ReserveExactPlan`, publish `Bound`, and terminalize the
   same candidates as `NotExecuted(BeforeDispatch)`; this is closure-only
   reservation, not Tool redispatch. Reopen gets a fresh finite recovery budget
   per Session command and returns `RecoveryRequired(EvidenceDurabilityFailed)`
   without deleting the journal when closure cannot be proved.
5. Only after the old Turn is durably closed, freeze the effective
   credential-free profile/policy snapshot, selected model/provider,
   `TurnExecutionLimits`, user input, and a `ToolCatalogSnapshot` built from the
   live enabled generated descriptor inventory: runtime build identity, ordered
   names, descriptor identities, canonical input schemas and digest. Enforce the
   one MiB catalog and user-input limits at this boundary.
6. Publish `TurnStarted` only after every frozen fact validates. Failure before
   that publication creates no TurnId, PendingTurn, user message or provider
   request. Construct the live monotonic Turn deadline immediately afterwards.

### 4.2 Model step and binding

For each model step:

1. publish `ModelAttemptStarted`;
2. call `ModelStepDriver` once with assembled `NativeMessage` history;
3. stream public deltas best-effort while retaining one authoritative ordered
   attempt assembly;
4. publish `ModelAttemptFailed`/`Interrupted`/`Rejected` or `MessageComplete`;
5. for a Tool-bearing message, generate one UUIDv4 per ordered Tool Call and
   publish an exact `BindingPlanned` containing method, descriptor identity,
   canonical argument digest, structural address, and message identity;
6. release the Session repository write lock, call `reserve_batch`, release the
   Invocation repository write lock, then publish `Bound`;
7. on a foreign-owner UUID collision proved from an exact pinned ledger view
   where the previous plan is absent, obtain
   `ReconciliationDecision::ReviseCandidates`, replace only colliding UUID
   candidates, and CAS a complete new `BindingPlanned` revision before retrying
   the same exact batch; an uncertain, partial, or different reservation cannot
   trigger revision and fails closed with the original durable candidates;
8. same owner plus byte-identical tuple is idempotent; same owner plus different
   tuple is corruption.

### 4.3 Dispatch and closure

For each binding, in message order:

1. send JSON-RPC with `id = InvocationId`; the request does not create an
   invocation;
2. server acceptance opens/reads the project InvocationStore and admits only an
   exact `Reserved` record whose method/descriptor/arguments/session/Turn/address
   match. `DispatchStarted`, terminal, unknown, malformed, mismatched or
   unreserved identities are rejected before execution. The server then durably
   publishes `DispatchStarted` exactly once and only afterwards hands off to the
   native or Component executor. Coordinator recovery never resubmits a request;
   it uses a distinct closure/reconciliation path, so idempotent confirmation of
   a durable `DispatchStarted` cannot trigger a second executor handoff;
3. map the executor's typed result, structured error and Cell evidence to the
   closed invocation outcome. If workspace bytes were installed but the
   workspace-history append failed, record observed effect evidence and classify
   `Indeterminate(MayHaveOccurred)` — never `Succeeded` and never ordinary
   `Failed`;
4. compute canonical ToolResult material and pin it in `StepClosing` before any
   draft publication;
5. publish or exactly revalidate the ResultDraft;
6. finalize terminal outcome, material and typed evidence under the fixed
   `FinalizeBudget` using one immutable terminal candidate;
7. only after terminal durability, append the provider ToolResult closure to
   conversation state and advance the journal cursor;
8. if an invocation becomes `Indeterminate`, close all remaining undispatched
   bindings in order as `NotExecuted(PrecedingInvocationIndeterminate)` and stop
   the Tool batch. Dispatch inside one reserved batch is strictly sequential:
   the next Tool cannot execute until the current terminal fact and ToolResult
   closure are durable.

Result material never changes execution outcome. Oversize (>65,536 bytes) writes
no draft and records the real outcome plus
`Unavailable(OutputTooLarge { limit, actual_length, full_digest })`. A
`CleanAbsent` candidate records the real outcome plus
`Unavailable(DraftPublicationFailed)`. `Uncertain` is revalidated only against
the exact ref, candidate identity and digests.

If terminal/evidence publication cannot finish before the `FinalizeBudget`, the
caller receives `EvidenceDurabilityFailed`; the invocation remains
`DispatchStarted`, the journal remains at its existing `StepClosing`, later
bindings remain reserved, and no ToolResult closure or TurnCommit is published.
The next Session command uses a fresh finite recovery budget. When the ledger is
writable it publishes `Indeterminate(MayHaveOccurred)`, closes remaining
bindings in order as `NotExecuted(PrecedingInvocationIndeterminate)`, advances
`StepClosing`, and closes the Turn as
`Interrupted { ClosureOrigin::Reconciled }` without using a draft to infer
outcome or evidence. If publication still cannot complete, it returns
`RecoveryRequired(EvidenceDurabilityFailed)`, preserves `active-turn`, and
accepts no new prompt.

### 4.4 Turn commit

Once the final assistant step or controlling terminal outcome is known:

1. derive one typed `TurnOutcome` and `ClosureOrigin`;
2. pin the complete `TurnCommitCandidate` at `TurnClosing`, including the
   `TurnId`, transcript, aggregate usage, provider/model, profile snapshot,
   workspace-history OID, parents, and timestamp;
3. acquire the project `WorkspaceOperationLock` shared, read the current
   workspace-history checkpoint, and hold that shared lock through the
   SessionStore Turn/ref transaction so a mutation cannot interleave between
   the checkpoint and Turn publication. A checkpoint read failure is a commit
   error, never silently `Empty`;
4. run the existing idempotent final transaction to publish the immutable Turn,
   update the session refs, pin `turns/<TurnId>`, and delete `active-turn`;
5. emit `TurnCommitted` only after the durable transaction is known complete.

`TurnId` becomes an explicit stored field in conversation commit manifests and
public APIs. It is never reconstructed from timestamp/model/provider and is not
used as a Git OID. This closes GitHub #100-B's duplicate sibling-Turn identity
risk as part of the authoritative Turn identity change.

## 5. Implementation plan: small commits

Each numbered slice is one intended review commit unless the listed test volume
requires splitting tests immediately after their production code. Every commit
must compile and pass the focused commands listed for that slice. The
production path remains unique at every point.

### Commit 1 — Add dependencies, private coordinator shell, and structural gates

**Primary files**

- root `Cargo.toml` / `Cargo.lock` as needed;
- `crates/lattice-api/Cargo.toml`;
- `crates/lattice-api/src/turn/mod.rs`;
- new `crates/lattice-api/src/turn/coordinator.rs`;
- new `crates/lattice-api/src/turn/invocation_facts.rs`;
- `xtask/src/lancet_boundary.rs` and/or focused source-regression tests;
- affected crate `AGENTS.md` files.

**Work**

- add the accepted `lattice-api -> lancet-audit,lattice-conversation` normal
  dependency edges;
- define private blocking adapters for session and invocation I/O and the
  `PinnedLedgerView -> InvocationFactsSnapshot` projection in `lattice-api`;
- define a private coordinator dependency bundle without routing production
  execution through it;
- add structural checks for forbidden dependency edges and an explicit temporary
  allowlist for the existing numeric/SQLite/AgentLoop files; the allowlist must
  become machine-checked empty by Commit 7;
- keep `lancet-audit -> lattice-session` absent.

**Focused verification**

```bash
cargo check -p lattice-api
cargo test -p lattice-api --lib
cargo tree -p lattice-api -e normal
cargo tree -p lancet-api -e normal
cargo tree -p lancet-audit -e normal
cargo xtask lancet-boundary
```

### Commit 2 — Complete provider-neutral Tool closure contracts and fixtures

**Primary files**

- `crates/lattice-conversation/src/message.rs` and exports;
- `crates/lattice-llm/src/provider_projection/{mod,openai,anthropic,gemini}.rs`;
- `crates/lattice-llm/tests/provider_projection.rs` and
  `tests/fixtures/projection/**`.

**Work**

- make the existing `BoundToolCall` / `ToolResult` family the only future
  persisted Tool closure vocabulary, with closed outcomes `Succeeded`,
  `Failed`, `Rejected`, `Cancelled`, `NotExecuted`, and `Indeterminate`;
- finish pure projection for bound Tool Calls, ToolResults, usage,
  interruption/rejection, optional provider call IDs, signatures and opaque
  replay state;
- preserve same-provider wire facts exactly and drop provider IDs, opaque state,
  and source reasoning on cross-provider projection;
- return a typed projection error only when the target provider cannot encode an
  already-recorded closure; never synthesize a provider ID;
- do **not** switch/delete the existing production provider codec in this commit.
  Production adoption belongs to Commit 4, so no parallel production codec is
  introduced.

**Required fixtures**

OpenAI-compatible, Anthropic, and Gemini official-wire fixtures cover real and
missing provider IDs, same-provider replay, reasoning signatures/thought state,
cross-provider non-leakage, ToolResult blocks, usage, cancellation/interruption,
and the ADR 0005 attempt ceilings — 8 MiB ToolCall arguments, 2 MiB assistant
text, 2 MiB reasoning, and 16 MiB total — with exact-boundary acceptance and
plus-one interruption.

**Focused verification**

```bash
cargo test -p lattice-conversation
cargo test -p lattice-llm
```

### Commit 3 — Move the complete Rewind audit workflow beside workspace history

**Primary files**

- `crates/lancet-runtime/src/history.rs` or new
  `crates/lancet-runtime/src/rewind_audit.rs`;
- `crates/lancet-runtime/src/lib.rs`;
- `crates/lancet-api/src/rewind.rs` and exports;
- `crates/lattice-api/src/rewind.rs`;
- delete SQLite Rewind schema/read/write exports from `lancet-audit`;
- Rewind API and harness tests.

**Work**

- move `RewindAuditRecord`, schema, append and query ownership to
  `lancet-runtime` and add the independent
  `refs/lattice/rewind-audit/log` commit chain with strict validation, CAS
  fast-forward append, fsync, bounded reads and permanent reachability;
- elevate exclusive `WorkspaceOperationLock` ownership to
  `lattice-api::RewindCoordinator`: acquire it once and pass the held guard into
  lower workspace/history operations, which must not reacquire it;
- hold the guard across preflight/restore, workspace-history ref movement,
  Session ref movement, and the later short audit transaction;
- record conflict, restore/history partial failure, Session-ref failure, and
  success after the corresponding action but before returning; a failed audit
  append returns a typed partial-state error and never reports success;
- add deterministic barriers at every action -> audit window, including
  history-ref move -> audit, Session-ref move -> audit, and success -> return;
- stop and delete all SQLite Rewind reads/writes in this same commit. The rest of
  `CepAuditStore` remains production authority until Commit 6; there is never a
  second Rewind adapter and old Rewind rows are not migrated;
- keep temporary numeric fields inside Rewind step payloads until Commit 6
  supplies the real InvocationId; do not invent a bridge identity.

**Focused verification**

```bash
cargo test -p lancet-runtime --lib
cargo test -p lattice-api --test rewind_acceptance
cargo build -p lattice-cli
LATTICE_BIN=$PWD/target/debug/lattice cargo test -p lattice-system-harness --test direct_rewind
```

### Commit 4 — Narrow `lattice-agent` to `ModelStepDriver` and adopt provider projection

**Primary files**

- `crates/lattice-agent/src/agent_loop/{mod,traits,types,messages}.rs`;
- `crates/lattice-agent/src/lib.rs`, `src/testing.rs`, and tests;
- `crates/lattice-api/src/turn/llm.rs` plus a temporary private multi-round
  adapter;
- existing production `lattice-llm` provider entry points.

**Work**

- replace the Agent production API with a one-attempt `ModelStepDriver` that
  accepts provider-neutral history and returns/streams one bounded typed attempt;
- move temporary multi-round sequencing into one private `lattice-api` adapter
  only so the branch still compiles before the hard cut;
- switch model request/response assembly to the projection completed in Commit 2
  and delete the parallel production Tool-call codec in the same commit;
- route every provider stream delta through
  `lattice_conversation::AttemptAssembly`, enforcing 8 MiB ToolCall arguments,
  2 MiB assistant text, 2 MiB reasoning and 16 MiB total. Exact boundaries
  pass; plus-one publishes `ModelAttemptInterrupted(OutputLimitExceeded { kind })`
  with bounded diagnostics, no partial conversation, and no InvocationId;
- preserve opening/stream cancellation and deadline, best-effort live deltas,
  authoritative ordered assembly, token usage, and typed attempt outcomes;
- remove Tool sequencing, UUID generation, retry policy, store/commit authority
  and Turn lifecycle decisions from `lattice-agent`;
- the temporary `lattice-api` adapter may continue using the current numeric CEP
  client, but it must not call `reserve_batch`, publish `DispatchStarted`, write
  InvocationStore, or become a second coordinator. It is removed in Commit 6.

**Focused verification**

```bash
cargo test -p lattice-conversation
cargo test -p lattice-llm
cargo test -p lattice-agent
cargo test -p lattice-agent --features test-utils
cargo check -p lattice-api
```

### Commit 5 — Add the fixed FinalizeBudget and terminal candidate executor

**Primary files**

- new `crates/lattice-api/src/turn/finalize.rs`;
- private `crates/lattice-api/src/turn/coordinator.rs`;
- `crates/lancet-audit/src/invocation_store.rs` only for narrow retryability
  classification;
- focused deterministic time/fault tests.

**Work**

- implement a private monotonic 60-second budget anchored immediately after the
  executor returns; it includes repository-lock wait, exact uncertain-draft
  revalidation, terminal/evidence publication attempts, and retry sleeps;
- attempt immediately, then back off from 25 ms exponentially up to 1 second;
  check the deadline before every attempt and never start after it;
- freeze one terminal candidate containing identity, real execution outcome,
  material, digests, and typed evidence; every retry is byte-identical;
- retry only explicit store/durability failures; corruption, topology, binding,
  identity, material and digest mismatch fail immediately;
- add typed `EvidenceDurabilityFailed` and
  `RecoveryRequired(EvidenceDurabilityFailed)` API mapping owned by
  `lattice-api`, with no configuration axis;
- use private injected time in tests and assert the clock begins at executor
  return, including lock wait.

**Focused verification**

```bash
cargo test -p lattice-api --lib turn::finalize
cargo test -p lancet-audit invocation_store
cargo test -p lattice-session pending
```

### Commit 6 — Perform the single production hard cut

This is intentionally one large atomic switch commit. It may be prepared as
several uncommitted or fixup patches for review, but no intermediate commit may
run a production invocation through both stores or through a coordinator whose
wire/ledger/operator halves disagree.

**Primary files**

- `crates/lattice-api/src/turn/{coordinator,live,execute,collector,llm,mapping,types,lifecycle}.rs`;
- `crates/lattice-api/src/session.rs` and coordinator integration tests;
- `crates/lattice-session/src/{graph.rs,pending/**,git_graph/**}`;
- `crates/lancet-protocol/src/{jsonrpc,policy}.rs` and event/control contracts;
- `crates/lancet-dispatch/src/{transport,registry}.rs`;
- `crates/lancet-native/src/server/{transport,mod,call_durable_metadata}.rs`;
- `crates/lancet-runtime/src/{runtime,host,evidence,host_mutate,history}.rs` and
  workspace mutation plumbing;
- `crates/lancet-audit/src/{lib,operator,invocation_store}.rs`;
- delete `crates/lancet-audit/src/{audit,evidence,store_path}.rs` and SQLite
  tests/migrations;
- `crates/lancet-api/src/{audit,status,config,runtime}.rs`;
- `crates/lattice-api/src/turn/cep.rs`;
- `crates/lattice-cli/src/commands/{audit,lancet,run}.rs`;
- `crates/lattice-tui/src/app.rs`;
- `crates/lattice-system-harness/src/session_send.rs` and public event fixtures;
- all production/test SQLite and in-memory audit call sites; Cargo manifests and
  lockfile.

**Coordinator and recovery**

- route `stream_turn` only through `TurnCoordinator`; `run_turn` remains a
  collector over the same stream;
- reconcile existing `active-turn` before a new prompt, including closure-only
  `ReserveExactPlan`, exact candidate revision through the existing
  `ReconciliationDecision::ReviseCandidates`, and a fresh finite recovery budget
  per command;
- implement the complete model-attempt, binding, `reserve_batch`, `Bound`,
  strictly sequential dispatch, StepClosing, ResultDraft, terminal/evidence,
  ToolResult cursor, retry/cancellation, TurnClosing and final transaction flow;
- enforce the frozen `TurnExecutionLimits`: `max_model_steps` is the accepted
  `MAX_MODEL_STEPS = 32`, and before advancing to a new model step the limit
  closes as `Failed(ModelStepLimitExceeded)`, not journal corruption. Provider
  retries reuse the same `model_step_ordinal`, still publish a new
  `ModelAttemptStarted`, are gated by `max_model_attempts`, and never consume a
  model step;
- construct the monotonic live Turn deadline after durable `TurnStarted`. It
  gates only the start of later provider attempts and Tool dispatches; it never
  aborts mandatory ResultDraft, terminal/evidence, StepClosing, TurnClosing or
  final Turn publication, and never replaces the independent FinalizeBudget;
- remove the temporary adapter and production `ProvisionalCompletionHook`; reopen
  never calls provider, hook, or Tool executor;
- preserve best-effort public streaming and collector parity;
- make explicit `TurnId` durable in `TurnCommitInput`, immutable `TurnCommit`,
  manifest and public reads in this same switch, validate it against PendingTurn,
  delete the legacy `TurnCommitStatus`/`WorkspaceDisposition`/
  `TurnLifecycleEvidence`/`terminal_reason` projection and `legacy_status()`, and
  land the M-GH100-01 sibling-Turn uniqueness regression.

**Identity and dispatch**

- make JSON-RPC request ID, pending/event maps, cancellation, public
  `TurnEvent`/JSON event fields, CLI/TUI rendering and harness parsers use the
  caller-supplied canonical `InvocationId`; delete pair-local `AtomicU64`;
- require explicit binding context for lower-level tests;
- at server acceptance admit only the exact `Reserved` state, atomically advance
  it to durable `DispatchStarted`, and only then execute. A duplicate request at
  `DispatchStarted` or terminal is rejected before handoff; same InvocationId
  submitted twice has one effect. Recovery confirms/closes through a distinct
  path and never retries the request;
- add no inbound client notification parser. Keep server-to-client event
  notifications and enforce the no-inbound-control invariant structurally;
- propagate `InvocationId`, `TurnId`, structural address and effect ordinal
  through Component/native context, evidence, `host_mutate` refusal maps,
  MutationRecord/RewindStep and workspace-history trailers; bump the history
  schema with no compatibility reader.

**Result/finalization invariants**

- keep real execution outcome separate from result material; fixtures must prove
  a successful 65,537-byte Tool remains `Succeeded` with
  `Unavailable(OutputTooLarge { ... })`, and `CleanAbsent` keeps the real outcome
  with `Unavailable(DraftPublicationFailed)`;
- on FinalizeBudget exhaustion publish no later journal stage. A later command
  terminalizes `Indeterminate(MayHaveOccurred)` and ordered
  `NotExecuted(PrecedingInvocationIndeterminate)` only when the ledger is writable;
- during Turn closing hold shared `WorkspaceOperationLock` across current-history
  read and SessionStore final ref transaction; checkpoint read failure is never
  coerced to empty.

**Invocation/operator authority deletion**

- stop opening/writing `CepAuditStore` before enabling production
  InvocationStore writes; after this commit one invocation writes one ledger;
- switch query/tail/replay/status to pinned Git ledger views with `InvocationId`
  identity and immutable ledger OID cursor. Query returns one current-state row
  per InvocationId at the pinned tip, including unfinalized `Reserved` and
  `DispatchStarted` records; tail returns transitions after a reachable exclusive
  commit-OID cursor; reads take neither operation lock. Operator DTOs expose only
  closed outcomes, typed content-free evidence, digests and byte lengths — never
  Tool arguments, ToolResult bytes, provider payload/opaque state, credentials or
  internal metadata blobs;
- preserve ledger/high-water semantics: a lagging witness at an authoritative
  ledger ancestor is accepted and repaired by a later writer; a missing witness,
  witness ahead of the ledger, or witness unreachable from it fails closed;
- port the deleted SQLite secret-content regressions to InvocationStore terminal
  publication and operator projection, and exercise a Tool-bearing secret case
  in `direct_secrets`;
- replace every `AuditBacking::InMemory` / `CepAuditStore::in_memory` test with a
  `TempDir` real InvocationStore;
- delete SQLite schema/path/pointers, `audit_id`, numeric `call_id`,
  `evidence_id`, old Rewind APIs, `rusqlite` edges, fallback/import logic, and
  direct SQLite CLI readers.

**Focused verification**

```bash
cargo test -p lancet-protocol
cargo test -p lattice-session
cargo test -p lancet-dispatch
cargo test -p lancet-runtime
cargo test -p lancet-native
cargo test -p lancet-audit
cargo test -p lancet-api
cargo test -p lattice-agent --features test-utils
cargo test -p lattice-api
cargo test -p lattice-api --test consistency --test cep_round_trip --test workspace_history_acceptance --test rewind_acceptance
cargo test -p lattice-cli
cargo test -p lattice-tui
cargo build -p lattice-cli
LATTICE_BIN=$PWD/target/debug/lattice cargo test -p lattice-system-harness --test direct_operator --test direct_mutation --test direct_workspace_history --test direct_rewind --test direct_secrets
cargo tree -p lancet-audit -e normal
cargo xtask lancet-boundary
```

### Commit 7 — Delete legacy outcome/orchestration residue and empty structural allowlists

**Primary files**

- remaining `lattice-agent` obsolete modules/types/tests;
- `crates/lattice-session/src/{graph,git_graph/**,pending/**}`;
- legacy `crates/lattice-api/src/turn/**` builders/mappers;
- all remaining consumers and DTO fixtures;
- structural source/dependency gates.

**Work**

- delete all leftover `AgentLoop`, `ToolCallExecutor`, old retry/backoff,
  completion hook, and parallel Tool outcome/message assembly surfaces;
- remove DTO/build/test residue now that Commit 6 already made `TurnOutcome` /
  `ClosureOrigin`, explicit durable `TurnId`, and typed public projections the
  sole production authority; Commit 7 must not defer any production schema or
  L-GH144-02/M-GH100-01 acceptance work from the hard cut;
- require the numeric/SQLite/legacy structural allowlist introduced in Commit 1
  to be empty and fail CI if a production symbol returns.

**Focused verification**

```bash
cargo test -p lattice-conversation
cargo test -p lattice-session
cargo test -p lattice-agent --features test-utils
cargo test -p lattice-api
cargo test -p lattice-cli
cargo test -p lattice-tui
cargo xtask lancet-boundary
```

### Commit 8 — Add end-to-end crash, cancellation, notification and privacy gates

**Primary files**

- coordinator/subprocess integration tests under `crates/lattice-api/tests/` or
  `crates/lattice-system-harness/tests/`;
- deterministic IPC barrier support in the system harness;
- PendingTurn and InvocationStore fault tests;
- operator DTO/privacy fixtures.

**Real subprocess/SIGKILL barriers**

Cover `TurnStarted`, `ModelAttemptStarted`, both binding windows,
`Bound -> acceptance`, `DispatchStarted -> executor`, candidate -> `StepClosing`,
`StepClosing -> ResultDraft`, ResultDraft -> terminal, terminal -> ToolResult,
each multi-Tool cursor, `StepClosed -> TurnClosing`, and TurnClosing -> final
transaction. Also cover Rewind's history-ref -> audit, Session-ref -> audit, and
success-audit -> return windows.

For every barrier, assert the exact durable refs/stage observed before killing;
a misplaced barrier must fail. The fixture provider request counter and Tool
effect log live in a dedicated fsynced directory outside both the Local Workspace
and `$XDG_STATE_HOME/lattice`, so neither Rewind nor development-state cleanup
can reset them. Each case asserts a non-zero pre-kill count; a missing, reset or
lower counter fails rather than passing. The Tool effect log is append-only and
fsynced. Strict reopen must observe unchanged provider/effect counts, unchanged
InvocationIds, and removal of `active-turn` only after complete closure.

**Deterministic fault/protocol matrix**

Cover object write, object/ref fsync, ref CAS, locks, published-but-undurable
retry, lagging/high-water-ahead witness, absent/uncertain/oversized material,
terminal publication, final transaction, collision revision and uncertain
reservation, provider opening before/after first event, corruption non-retry,
cancel before/after dispatch and terminal, ordered later closures, a duplicate
request at `DispatchStarted` (second rejected, effect count unchanged), structural
absence of inbound notification ingress, server-to-client event routing across
multiple transport pairs, shared-lock checkpoint/mutation exclusion, model-step
and attempt limits, Turn deadline before provider/Tool start with durability
closure exempt, and argument/result/provider payload/secret/provider-ID redaction
in logs **and operator DTOs**.

**Focused verification**

```bash
cargo test -p lattice-session pending_crash -- --nocapture
cargo test -p lancet-audit invocation_store -- --nocapture
cargo test -p lattice-api coordinator -- --nocapture
cargo build -p lattice-cli
LATTICE_BIN=$PWD/target/debug/lattice cargo test -p lattice-system-harness --test direct_failures --test direct_turn_contract --test direct_tools --test direct_mutation --test direct_workspace_history --test direct_operator --test direct_rewind --test direct_secrets
cargo xtask lancet-boundary
```

### Commit 9 — Close documentation, findings, and the hard-cut ledger

**Primary files**

- root and affected crate `AGENTS.md` files;
- `README.md`, `docs/ARCHITECTURE.md`, `docs/DEVELOPMENT.md`, and
  `docs/GETTING-STARTED.md` where stale;
- operator/development state-reset documentation;
- `FINDINGS.md` and append-only `FIX.md`.

**Work**

- document final ownership, coordinator, identity, Git operator adapter, Rewind
  audit chain and manual state rebuild;
- remove SQLite/AgentLoop authority claims;
- close exact findings `L-GH144-02` and `M-GH100-01` only if their acceptance
  tests are green; retain #153 diagnostic findings unchanged;
- append the #139-B `FIX.md` entry only after code/tests land.

**Focused verification**

```bash
rg -n 'CepAuditStore|AuditBacking|ProvisionalCompletionHook|ToolCallExecutor|AgentLoop|call_id: u64|AtomicU64|evidence_id|legacy_status' crates docs README.md AGENTS.md
cargo fmt --all -- --check
cargo test -p xtask
```

Every production-code hit must be rejected by the empty machine structural gate;
remaining documentation hits must be explicitly historical.

## 6. Acceptance matrix

The implementation is not complete until all rows are green.

| Area | Required proof |
| --- | --- |
| prerequisite ancestry | #142/#143/#144/#139-A commits remain ancestors of the branch |
| identity | one typed `InvocationId` across binding, JSON-RPC, events, cancellation, Component/native context, evidence, operator output and workspace history; no numeric allocator |
| reservation | exact ordered all-or-nothing batch; idempotent same owner/tuple; revision only after an exact pinned view proves the prior plan absent plus foreign ownership; uncertain/partial/mismatched reservation keeps candidates and fails closed |
| dispatch | server admits only exact `Reserved`, atomically publishes durable `DispatchStarted`, then hands off once; duplicate `DispatchStarted` request is rejected; dispatch within one batch is sequential; no inbound notification ingress or request creates an invocation |
| result order | exact candidate -> `StepClosing` -> ResultDraft -> terminal/evidence -> ToolResult closure; execution outcome remains independent from material availability, including +1 oversize and `CleanAbsent` |
| mutation durability | installed bytes plus failed workspace-history append publishes observed effect evidence and `Indeterminate(MayHaveOccurred)`, never `Succeeded` or ordinary `Failed` |
| finalize | fixed 60 s monotonic budget begins at executor return and includes lock wait, exact revalidation, publication and delays; 25 ms exponential backoff capped at 1 s; no post-deadline attempt or corruption retry |
| recovery | bounded closure only with a fresh budget per command; `ReserveExactPlan` is allowed but provider/executor counts never increase; exhaustion retains `StepClosing`/`active-turn`, and later closure writes typed Indeterminate/NotExecuted facts only when the ledger is writable |
| live limits | frozen `max_model_attempts` gates retries on the same step; retries do not consume a model step; advancing beyond `MAX_MODEL_STEPS = 32` closes typed `ModelStepLimitExceeded`; 8/2/2/16 MiB assembly ceilings interrupt without partial conversation or InvocationId; monotonic Turn deadline starts after `TurnStarted`, blocks only new provider/Tool work, and never interrupts mandatory durable closure |
| cancellation | durable-state classification, ordered later closures, no string-based authority |
| provider replay | official fixtures for three provider families, optional provider ID preserved, same-provider exact replay, cross-provider opaque/reasoning non-leakage |
| operator | one Git Adapter; query returns a bounded pinned-tip current-state row per InvocationId including `Reserved`/`DispatchStarted`; replay is complete; tail uses an exclusive reachable OID cursor; lagging witness repairs and missing/ahead/unreachable witness fails; typed content-free evidence/digests/lengths only; no operation locks, payload/internal metadata, SQLite, import or fallback |
| turn acceptance | `TurnStarted` freezes credential-free effective profile/policy, selected model/provider, exact execution limits, bounded user input, and a live enabled ToolCatalogSnapshot whose digest matches generated descriptors; prepublication failure establishes no Turn or provider request |
| Rewind | one product-held exclusive operation lock spans workspace/history/Session/audit; append-only independent audit ref survives mutation rewind; every action-to-audit window is tested; publication failure never reports success; no fourth repository |
| Turn commit | explicit durable `TurnId`, typed outcome/origin only, shared workspace lock spans checkpoint read through final Session transaction, atomic final publication and no legacy outcome fields |
| locking | full-Turn lock, correct workspace lock order, at most one repository write lock, blocking Git off async workers |
| privacy | logs and operator DTOs expose no arguments, result/fetched content, provider payload/opaque state, credentials, provider IDs as correlation, or internal metadata blobs; deleted SQLite secret regressions are replaced on Git terminal/operator paths and a Tool-bearing `direct_secrets` case |
| crashes | deterministic fault matrix plus real subprocess SIGKILL at all listed barriers; pre-kill durable state and crash-surviving provider/effect counters make every barrier falsifiable |
| structure | forbidden dependency edges, SQLite deps/symbols, numeric IDs, dual coordinator and dual adapter absent; the sole production switch never opens/writes both InvocationStore and CepAuditStore for one invocation |

## 7. Full local gate

Run from the #139-B worktree with ambient binary overrides removed:

```bash
env -u LATTICE_BIN cargo xtask ci
cargo test --workspace
cargo test -p noop-echo --target wasm32-unknown-unknown
cargo build --workspace \
  --exclude noop-echo \
  --exclude observe-workspace \
  --exclude mutate-create \
  --exclude mutate-edit \
  --exclude session-todo_write
cargo tree -p lancet-api -e normal
cargo tree -p lancet-audit -e normal
cargo tree -p lattice-api -e normal
cargo xtask lancet-boundary
```

A host `cargo build --workspace` is not a gate because all five guest cdylib
crates are wasm32-only. The host build excludes `noop-echo`,
`observe-workspace`, `mutate-create`, `mutate-edit`, and `session-todo_write`;
guest artifacts are built/tested through their wasm32 targets and xtask gates.
`cargo xtask ci` must always
run with `LATTICE_BIN` unset so the harness cannot silently use a stale binary.

Before committing the plan or implementation, run the available LSP/lens
diagnostics on edited files when the harness exposes them, then run the concrete
fmt/test gates above. Diagnostics availability is not itself an acceptance gate;
`git status --short` must contain only intended changes.

## 8. Remote Rocky Linux/ext4 correctness smoke

This is a merge-time manual acceptance gate, not normal CI and not a new
performance qualification campaign.

1. Build the release `lattice` binary and `lattice-store-probe` from the exact
   reviewed source SHA on a clean Linux builder.
2. Copy the binaries plus the deterministic local provider/tool fixtures to a
   clean Rocky Linux host whose selected `$XDG_STATE_HOME` and Local Workspace
   are on ext4.
3. Record source SHA, binary SHA-256, kernel, mount source/options/filesystem,
   XDG state root, workspace root, commands, UTC start/end and exit status.
4. Run `lattice-store-probe` against the exact state location.
5. Run a correctness-only smoke covering:
   - session create, ordinary no-Tool Turn and strict reopen;
   - Component observation and mutation with one durable InvocationId visible in
     conversation/operator/history projections;
   - forced process termination at one post-`DispatchStarted` and one
     post-ResultDraft barrier, followed by deterministic reopen with unchanged
     provider/effect counts;
   - operator query/tail/replay using Git cursor semantics;
   - explicit Rewind plus a surviving append-only Rewind audit record.
6. Assert no duplicate Tool effect, no replayed provider request, no lingering
   `active-turn` after complete closure, no SQLite audit file creation, and no
   secret/content leakage in captured logs.

Do not set latency thresholds, rewrite #139-A summaries, or commit benchmark
bundles. Failure is a correctness blocker; performance observations are routed
to #153.

## 9. Manual development-state rebuild

The hard cut intentionally rejects pre-#139-B durable development formats.
Before the first post-switch local run:

1. stop all LATTICE processes using the state root;
2. back up any state a maintainer needs for forensic reference;
3. identify the exact disposable development `$XDG_STATE_HOME/lattice` and Local
   Workspace roots; never guess or run broad glob deletion;
4. treat global `sessions.git` separately: deleting it destroys sessions for
   every project in that state root, so do so only when the maintainer explicitly
   declares the entire state root disposable. Otherwise choose a fresh
   `$XDG_STATE_HOME` for #139-B development;
5. remove each selected project's old `invocations.git` and
   `workspace-history.git`, plus the transitional SQLite audit database at its
   actual resolved location (including `<project>/.lattice/cep_audit.db` where
   applicable), only after printing and inspecting every exact path;
6. create new sessions through `lattice session new`; this is the only creation
   path for the strict project stores;
7. run `lattice-store-probe` and one no-Tool Turn before Tool-bearing smoke.

The implementation docs must provide concrete commands only after confirming the
final path helpers and filenames in code. They must include a printed-path/dry
inspection step and require an explicit maintainer-selected state root. Do not
add automatic migration, automatic deletion, a compatibility reader, or a
runtime `--migrate` flag.

## 10. Review protocol and stop conditions

Use two independent review axes before implementation merge:

- **Standards review:** repository rules, ownership/dependency boundaries,
  Linux-only durability/locks, headers, test placement, FIX/FINDINGS hygiene,
  no unrequested compatibility or abstraction;
- **Spec review:** every issue acceptance item and ADR invariant mapped to code
  plus a test or explicit remote smoke assertion.

Request adversarial review specifically for:

- cross-store windows and idempotent retry after published-but-undurable refs;
- whether any path can execute before `DispatchStarted` durability;
- StepClosing/ResultDraft/terminal ordering;
- cancellation vs `Indeterminate` misclassification;
- provider replay ID synthesis or opaque-state leakage;
- Rewind audit chain reachability and partial-state error reporting;
- accidental direct invocation path, second adapter, legacy reader or numeric
  identity;
- lock nesting or blocking Git on async workers.

Stop and return to design review if implementation appears to require any of:

- a fourth repository;
- cross-store atomic transaction or simultaneous repository write locks;
- SQLite/Git dual writes or legacy import;
- an in-memory production-style InvocationStore abstraction;
- a second production coordinator or provider codec;
- synthetic provider call IDs;
- automatic Tool redispatch or provider replay during reopen;
- weakening the fixed FinalizeBudget or fail-closed corruption behavior.

## 11. Definition of done

#139-B is done only when:

- all nine implementation commits (or review-equivalent smaller prep commits plus
  the one indivisible production switch) have landed on one branch with no
  production dual-authority commit;
- the local full gate and remote Rocky/ext4 correctness smoke pass against the
  reviewed source SHA;
- static searches show no production SQLite audit/evidence authority, numeric
  invocation allocator, old AgentLoop/Tool executor ownership, completion hook,
  legacy Turn outcome projection, or compatibility reader;
- `cargo tree` preserves the documented layer graph and the absence of
  `lancet-audit -> lattice-session`;
- old development state has been rebuilt rather than migrated;
- `FIX.md`, `FINDINGS.md`, root/crate `AGENTS.md`, and user/developer docs match
  the final code;
- the branch is committed and clean.

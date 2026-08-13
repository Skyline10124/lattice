# #141 Accepted Architecture Decisions

This is the compact implementation handoff from the #141 review. ADR 0005 is
the normative architecture authority; this document is an index, not a second
specification.

## Identity and addressing

- `InvocationId` and `TurnId` are strict lowercase hyphenated UUID v4 types in
  `lancet-protocol`; `TurnCoordinator` generates them through an injectable
  source.
- Provider Tool Call IDs and opaque replay state stay in conversation/provider
  data and never enter LANCET identity or InvocationStore bindings.
- `ProjectStoreKey` hashes the canonical Linux path bytes of the full Local
  Workspace root. It is path addressing, not content/project recognition.
- Same-path reuse intentionally reuses state; moving a project requires a new
  Session. `[lancet.project_root].root` is policy scope, not identity.

## Store topology and ownership

```text
$XDG_STATE_HOME/lattice/
├── sessions.git
├── projects/<ProjectStoreKey>/invocations.git
├── projects/<ProjectStoreKey>/workspace-history.git
├── locks/sessions/<SessionId>.lock
└── locks/projects/<ProjectStoreKey>/workspace.lock
```

- `lattice-store` owns addressing and generic Git/Linux durability mechanics,
  never domain schemas or cross-store transactions.
- Session, invocation/operator, and workspace-history semantics remain in
  `lattice-session`, `lancet-audit`, and `lancet-runtime`.
- `session new` resolves one `LocalWorkspaceRoot` through the shared API path,
  derives the key, creates/validates project stores, and publishes the bound
  Session; there is no first-Turn lazy binding.
- `session new` is the only normal first-create path. Expected missing stores
  are corruption; there is no automatic recreation, cascade delete, or GC.
- Mutable Session sidecars are deleted. `sessions.git` owns immutable
  `SessionState`, heads, permanent Turn pins, and one direct `active-turn` ref.

## Locking

```text
SessionExecutionLock
  -> optional WorkspaceOperationLock
     -> at most one repository write lock
```

- Session lock covers a full live Turn and all coherent Session mutations.
- Project workspace lock is per observation/mutation/rewind/checkpoint
  operation, not per Turn.
- Mutation and rewind take it exclusive; observation and Turn-closing history
  sampling take it shared.
- The current WorkspaceHistory repository lock becomes the standard kernel
  repo lock rather than surviving as a duplicate.

## Turn journal and coordinator

- `TurnStarted` is the durable request-acceptance boundary and freezes effective
  profile/policy/model/system/catalog/limits without credentials.
- `TurnCoordinator` owns the multi-step loop, deadline, retry/backoff,
  cancellation, usage, UUIDs, reservation, dispatch, result durability,
  reconciliation, and Turn commit.
- `lattice-agent` becomes one provider-attempt `ModelStepDriver`.
  `ProvisionalCompletionHook` is deleted.
- A valid Tool-bearing response atomically becomes `BindingPlanned`; there is
  no durable Tool-bearing `MessageComplete` window.
- `PlanKey = { TurnId, model_step_ordinal }`. Bindings use structured message and
  part addresses plus per-binding digests; there is no random PlanId or batch
  digest.
- A Step closes ToolCalls sequentially. Later calls execute only after the
  current terminal and ToolResult closure are durable.
- Reopen is closure-only: no provider retry, Tool redispatch, hook rerun, new
  binding identity, or guessed outcome.

## Validation, usage, and limits

- Tool arguments must be a JSON object and execute from canonical JSON bytes.
  One invalid call rejects the complete attempt; partial assistant text does
  not enter conversation.
- Retries are allowed only before any stream event and get a new attempt
  ordinal. Any partial stream is interrupted and not retried.
- Usage is authoritative per attempt. Missing usage is unknown, not zero;
  Turn totals are derived and marked complete or partial.
- Protocol limits: 8 MiB per Tool arguments, 2 MiB assistant, 2 MiB reasoning,
  16 MiB per attempt, 1 MiB frozen Tool catalog, and 32 model steps.

## Invocation and result authority

- Reservation creates an Invocation. The JSON-RPC request carries the same
  `InvocationId`; server acceptance publishes `DispatchStarted` before handoff.
- Invocation terminal and all Cell evidence publish atomically in one ledger
  update. Outcome and result material are independent typed fields.
- `ResultDraft` has no outcome. It is bounded material keyed by exact identity,
  binding, address, digest, and byte length.
- ToolContent above 65,536 bytes is not truncated and writes no draft. The
  terminal preserves the real execution outcome with unavailable material.
- Draft uncertainty and terminal finalization use a measured finite
  `FinalizeBudget`; budget exhaustion leaves `DispatchStarted` and the Turn's
  `StepClosing` unfinalized with `active-turn` retained. Later commands reconcile
  only; no TurnCommit exists until every binding closes.

## Final Turn and history

- `TurnOutcome` is a typed `Completed | Failed | Cancelled | Interrupted` sum,
  with separate `Live | Reconciled` closure origin.
- Current `WorkspaceDisposition`, `TurnLifecycleEvidence`, and logic-bearing
  free-text terminal reason are deleted in the hard cut.
- `TurnCommit.workspace_history_oid` becomes an explicit global history
  checkpoint, sampled under the shared project lock through Session publication.
- Mutation-history commits associate `InvocationId`, `TurnId`, effect ordinal,
  method, path, and revisions. They are not terminal authority.
- `TurnCoordinator` owns the Turn saga; `RewindCoordinator` separately owns the
  current rewind saga. PendingTurn is not a rewind journal, and ADR 0005/#139 do
  not add a new rewind crash journal.

## Ordered implementation

```text
#141 -> (#142 + #143) -> #144 -> #139-A -> #139-B -> #32 -> #34
```

#142 also adds typed `TurnId`; #143 includes XDG topology, SessionState/heads and
operation locks; #144 adds the TurnId-keyed pins, direct `active-turn` journal,
and synthetic invocation facts.
#139-A measures the real InvocationStore and finalization path. #139-B performs
the production hard cut with no dual write, import, compatibility reader, or
runtime backend switch.

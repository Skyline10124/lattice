---
status: accepted
---

# ADR 0005: Durable Turn, Invocation Identity, and Private Git Stores

- Status: Accepted
- Date: 2026-08-13
- Amends: ADR 0004 decision 8 and the execution assumptions in GitHub #24/#34
- Depends on: ADR 0003 and ADR 0004
- Implementation tracking: GitHub #141–#144 and #139
- Superseded in part by: ADR 0006 (PendingTurn phases, the three-store layout, ModelAttempt events, the Interrupted outcome, Turn checkpoint and file Rewind; see ADR 0006 “与 ADR 0005 的关系”)

> 承接说明：会话、对话、请求接受、PendingTurn、ResultDraft、重开及最终发布由[会话合同](../architecture/session.md#验证与承接)维护；调用身份、预留、分发、终态与操作员读取由 [LANCET 合同](../architecture/lancet.md#来源与承接范围)维护，Cell 证据由 [Cell 合同](../architecture/cell.md#来源与迁移边界)维护。
> 项目寻址、三库布局、Git 内核、锁及位置能力由[存储合同](../architecture/storage.md#验证入口与承接)维护；项目全局 checkpoint、文件历史与 Rewind 由[工作区合同](../architecture/workspace.md#来源与承接范围)维护。
> 本 ADR 保留理由、取舍、历史与原有修订关系，不再作为当前实施流水。第 11 节的 ledger/high-water witness 与冷读复杂度由 LANCET 合同承接，第 12 节的 FinalizeBudget 及耦合由会话合同承接；资格化的现行 diagnostic-only 操作、manifest 单一来源和证据规则见 [InvocationStore 诊断指南](../development/invocation-store-diagnostics.md)。原批准、撤回与 campaign 数值/verdict 不因当前裁决改写。旧独立 admission-control 队列义务已由所有者废止，现有锁与实际等待计入预算的要求不变；旧开发状态重建步骤不是当前清理授权。

## Context

LATTICE currently joins one Tool execution through several unrelated values:

- an optional provider Tool Call ID;
- a process-local numeric CEP `call_id`;
- SQLite `audit_id` and `evidence_id` values;
- transcript order and workspace-history metadata.

None is an end-to-end execution identity. Provider IDs are protocol replay
metadata, numeric CEP IDs are transport counters, and SQLite IDs are storage
keys. Correlating a transcript Tool Call with the execution, mutation history,
terminal Cell evidence, ToolResult, cancellation, and operator output therefore
depends on order or local identifiers. That is ambiguous under concurrency,
provider switching, retry, cancellation, process failure, and storage migration.

The live Agent loop is also one in-memory multi-step operation. It has no durable
fact for an accepted user Turn, a provider attempt, a complete message awaiting
binding, a reserved invocation awaiting dispatch, a returned Tool result awaiting
terminal evidence, or a Turn awaiting final commit. Reopen cannot distinguish a
safe closure from an unsafe retry without those facts.

ADR 0004 fixed the build-closed Tool runtime but did not decide these identity,
conversation, recovery, and private-store contracts. GitHub #114 permanently
retired Turn/audit PROV and in-toto attestations while retaining a transitional
SQLite operator Adapter. This ADR fixes the replacement architecture before the
production identity hard cut. It is a target contract, not an assertion that the
current code already implements it.

## Decision

### 1. Canonical identities and addresses

`lancet-protocol` owns two strict, data-only identity types:

- `InvocationId`: one Tool invocation, lowercase hyphenated UUID v4;
- `TurnId`: one accepted user Turn, lowercase hyphenated UUID v4.

The protocol crate validates parse, display, serde, and schema forms but does not
generate either value. `lattice-api::TurnCoordinator` generates both from an
injectable UUID source. Production randomness uses UUID v4; deterministic sources
exist only in tests.

An `InvocationId` is generated after a valid complete model message and before
reservation. The same value identifies its conversation binding, CEP request ID,
transport events and cancellation, Host execution context, mutation-history
association, terminal Cell evidence, ToolResult closure, and operator output.

A provider Tool Call ID is separate. It is optional opaque part-local metadata
owned by `lattice-llm` and `lattice-conversation`. Real values are preserved for
same-provider replay when the provider supplies them. They are never synthesized
as LANCET identity and never enter reservation, authorization, dispatch, history,
evidence, or operator correlation.

Pre-commit conversation addresses are structural rather than random:

```text
ModelMessageAddress = { SessionId, TurnId, model_step_ordinal }
ToolCallAddress      = { ModelMessageAddress, part_ordinal }
ModelAttemptKey      = { TurnId, model_step_ordinal, attempt_ordinal }
PlanKey              = { TurnId, model_step_ordinal }
```

`TurnCommit` preserves the explicit `TurnId`; it is never derived late from
content or a commit seed. `PlanKey` has no random `PlanId` and no second aggregate
batch digest. Exact ordered bindings are the idempotency input.

### 2. Project storage addressing is path addressing, not project recognition

LATTICE does not try to decide whether changing content at a path is the “same
project”, nor whether an unrelated project later reused that path. A project may
change arbitrarily over time. The canonical Local Workspace path is therefore a
storage address, not a stable project entity identity.

`lattice-store::ProjectStoreKey` is:

```text
canonical_root = canonicalize(full Local Workspace root)
path_bytes     = Linux OsStrExt bytes of canonical_root
ProjectStoreKey = lower_hex(
    SHA256("lattice-project-store-key-v1\0" ||
           u64_be(len(path_bytes)) || path_bytes)
)
```

The full runtime Local Workspace root is used. Profile
`[lancet.project_root].root` is only a policy subdirectory/scope and does not
participate in store addressing. UTF-8 conversion is not required.

The consequences are deliberate:

- the same canonical path always uses the same project namespace;
- moving a project to another path produces a different key;
- an unrelated project reusing a path sees the existing namespace;
- there is no content fingerprint, generation registry, project-recognition
  heuristic, or operator-created generation;
- deleting a workspace does not delete private state, but WorkspaceHistory is
  only LATTICE mutation history, not a whole-project backup.

Each Session is permanently bound to one `ProjectStoreKey`. Opening or executing
it from a workspace with a different key fails closed. Moving a project requires
creating a new Session; there is no relink or compatibility migration.

### 3. One state root, three repositories, three failure domains

All mutable private state lives under the XDG state root, not the configuration
root. With the standard fallback, `$XDG_STATE_HOME` means
`$HOME/.local/state` when the variable is unset.

```text
$XDG_STATE_HOME/lattice/
├── sessions.git
├── projects/
│   └── <ProjectStoreKey>/
│       ├── invocations.git
│       └── workspace-history.git
└── locks/
    ├── sessions/<SessionId>.lock
    └── projects/<ProjectStoreKey>/workspace.lock
```

`sessions.git` is global because it contains Sessions that are already bound to
their project key. Invocation and workspace mutation history are project-scoped.
The repositories are never nested in a workspace and never merged into one
repository or commit chain:

- `lattice-session` owns Session state, conversation DAG, PendingTurn, and
  `sessions.git` refs;
- `lancet-audit` owns invocation lifecycle, Cell evidence, operator reads, and
  `invocations.git` refs;
- `lancet-runtime` owns mutation/rewind semantics and
  `workspace-history.git` refs.

Cross-repository work is a saga. A reservation can survive a later SessionStore
failure; installed workspace bytes and mutation history can survive terminal
publication failure; rewind does not rewrite invocation history. No API claims
cross-repository atomicity.

`session new` is the only product command allowed to first-create `sessions.git`
and the required project stores before publishing a Session. Later commands
strictly open expected stores. #139-A may explicitly initialize
`invocations.git` inside its isolated qualification harness. A missing store that
an existing Session references is corruption, not an invitation to recreate it.

The public Session creation entry receives one explicitly resolved
`LocalWorkspaceRoot`. CLI and TUI obtain it from the same `lattice-api` resolver
for the current workspace; neither has a separate binding heuristic or lazy
first-Turn path. The composition layer canonicalizes the root, derives
`ProjectStoreKey`, creates/validates the required project stores, and only then
publishes `SessionState { project_store_key, ... }`. `lattice-session` consumes
the typed key and never derives it from process cwd.

There is no cascade delete or automatic GC of a Session, workspace, invocation,
or history domain.

### 4. `lattice-store` is a generic durability kernel

`lattice-store` owns only reusable Linux/private-Git mechanics:

- `ProjectStoreKey` derivation and the XDG state-root layout;
- typed create/open and repository identity validation;
- canonical bytes and immutable object/commit creation;
- ref compare-and-set and multi-ref transactions within one repository;
- per-repository Linux `flock`;
- file, ref, and directory durability barriers;
- atomic first-create publication;
- retention primitives and typed corruption/durability errors.

It does not own a catalog of domain schemas, a generic `put(key, value)` API, a
raw gix repository getter, an all-stores handle, a pluggable backend, or a
cross-store transaction. Domain wrappers own their repository names, refs,
record schemas, invariants, and migrations.

Every repository has:

```text
refs/lattice/meta -> {
    repository_format_version,
    store_kind,
    store_schema_version,
    scope: Global | Project(ProjectStoreKey),
}
```

`repository_format_version` is the kernel envelope version.
`store_schema_version` and `store_kind` are supplied and interpreted by the
domain wrapper. Project stores must match their directory key and scope. There
is no `StoreInstanceId`: the design does not promise to detect an operator who
deletes a whole store and installs a different schema-valid empty repository at
the same location.

An absent target is `StoreNotInitialized`. An existing target is only opened and
validated. Wrong kind, scope, version, canonical bytes, missing object, illegal
ref, or broken domain invariant is typed corruption. Open never falls back to
init, repair, quarantine, a legacy path, or an empty replacement.

First creation uses a target-specific parent lock and a same-parent sibling
temporary directory. The creator initializes the bare repository, metadata and
initial refs, completes durability barriers, publishes with
`renameat2(RENAME_NOREPLACE)`, fsyncs the parent, and reopens the published
identity. A concurrent loser validates the winner and observes the same parent
durability barrier. A first-create lock and a repository write lock never overlap.

Each repository has exactly one standard kernel write lock. The current
WorkspaceHistory-specific repository lock is migrated to it rather than retained
as a second lock. A write holds the lock across old-tip read, object creation,
CAS/ref transaction, and durability barriers. CAS remains the publication
authority. Readers pin a ref OID and read immutable objects without that lock.

Acknowledged state remains reachable by domain refs. The first release performs
no automatic GC, repack, maintenance, or history rewrite. Only unpublished
objects and CAS losers may be unreachable.

### 5. Operation locks are outside repository locks

Repository locks protect Git publication only. Two external operation locks
protect larger invariants.

`SessionExecutionLock` is a Linux `flock` at
`locks/sessions/<SessionId>.lock`. Exclusive ownership covers the full live Turn,
reconciliation, rewind, reset, checkout, branch switch, and every Session
state/branch/active-turn mutation. It has no PID, heartbeat, timeout, mtime, lock
steal, or stale-owner protocol; process exit releases it in the kernel.

Snapshot readers such as open/list/log/diff may pin committed refs without the
lock. A coherent read of current mutable Session status takes a nonblocking
shared lock and returns `SessionBusy` if an exclusive owner exists; it does not
misreport busy state as corruption.

`WorkspaceOperationLock` is a project-scoped Linux `flock` at
`locks/projects/<ProjectStoreKey>/workspace.lock`:

- observation (`read`, `list`, `grep`, `find`) takes it shared for one bounded
  operation;
- each mutation takes it exclusive before Local Revision/precondition checks and
  holds it through atomic installation and workspace-history publication;
- rewind takes it exclusive for its entire plan, restore, and history-ref move;
- Turn closing takes it shared from reading the history checkpoint through the
  SessionStore Turn/ref transaction.

The lock is per operation, not per Turn. A multi-Tool Turn has no workspace
snapshot isolation; external non-LATTICE changes remain governed by path safety
and Local Revision checks.

The partial order is:

```text
SessionExecutionLock (when the operation is Session-bound)
  -> WorkspaceOperationLock (when the operation touches/coherently samples workspace state)
     -> zero or one repository write lock
```

Code never acquires an operation lock while holding a repository lock, upgrades a
shared lock, or holds two repository write locks. Operator invocation reads take
neither operation lock; they pin one `invocations.git` ledger tip.

### 6. Session authority is entirely in `sessions.git`

The target removes mutable `.lattice/sessions/**` sidecars and ignores the bare
repository's top-level `HEAD`. The fixed namespace is:

```text
refs/lattice/sessions/<SessionId>/state
refs/lattice/sessions/<SessionId>/heads/<BranchName>
refs/lattice/sessions/<SessionId>/turns/<TurnId>
refs/lattice/sessions/<SessionId>/active-turn
```

`state` points to one immutable `SessionState` containing the bound
`ProjectStoreKey`, label, active agent, creation metadata, and current
attached/detached head selector. It does not duplicate an active Turn.
`active-turn` existence and target are the only authority for that fact, and the
target object contains its `TurnId`.

`heads/*` are movable conversation branches. `turns/*` are create-once permanent
pins for committed Turns, so reset or rewind does not make an acknowledged Turn
depend on reflog retention. Every state/ref mutation uses CAS.

Final Turn publication is one `sessions.git` ref transaction that:

1. advances the selected conversation branch or detached state;
2. creates `turns/<TurnId>`;
3. updates `state` if required; and
4. deletes `active-turn`.

The `TurnCommit` references the final PendingTurn journal OID. `TurnClosing`
pins the exact canonical `Committed` and `TurnCommit` candidate identities,
including every timestamp or other commit-time field, before the ref transaction
is attempted; recovery never reconstructs either candidate from a new clock or
mutable input. Failure of any ref precondition leaves the whole transaction
unpublished.

### 7. Conversation is a data-only contract

`lattice-conversation` owns provider-neutral conversation data:

- ordered `NativeMessage<ToolCallState>` and `MessagePart` values;
- `UnboundToolCall` and `BoundToolCall` typestates;
- provider-neutral Tool result closure;
- semantic and replay digests;
- per-attempt usage and interruption/rejection records;
- provider origin and bounded part-local opaque replay state.

It depends only on the data-only protocol layer and neutral data dependencies. It
owns no filesystem, store, lock, runtime, provider HTTP client, or ID generator.

The ordered part vector is durable conversation order. A complete reasoning part
is stored and participates in the provider-neutral semantic digest, but LANCET
does not interpret it and cross-provider projection does not replay source
reasoning. Protocol-required signatures remain part-local opaque replay state.
Partial reasoning or assistant output from an interrupted attempt is diagnostic
state, not committed conversation.

Two digests remain distinct:

- `semantic_digest`: provider-neutral text, complete reasoning, canonical Tool
  semantics, and their order; binding references this digest;
- `replay_state_digest`: provider origin, real provider call IDs, and part-local
  opaque replay state.

Provider metadata is excluded from InvocationStore bindings. Same-provider replay
may use it; cross-provider projection uses neutral semantics and never fabricates
target-provider identity.

Tool arguments must be one JSON object. Empty strings, arrays, scalars, malformed
JSON, and duplicate/incomplete Tool calls are invalid. The adapter canonicalizes
through `lancet-protocol::canonical_json`; the arguments digest is SHA-256 over a
domain separator, an explicit byte length, and those canonical bytes. Execution
uses the canonical object, not the provider's textual spelling.

If any ToolCall in a completed provider batch is invalid, the entire attempt is
`ModelAttemptRejected(InvalidToolCall)`. Its bounded assembled assistant,
reasoning, ToolCall data, usage, and diagnostics are retained for Session
diagnosis, but it does not create `MessageComplete`, `BindingPlanned`, an
`InvocationId`, or transcript content. The current behavior that drops the
invalid ToolCall and commits only assistant text is removed.

### 8. `TurnStarted` is the durable request-acceptance boundary

Before `TurnStarted`, while holding `SessionExecutionLock`, the coordinator:

- validates Session binding against the current `ProjectStoreKey`;
- strictly opens and validates both project stores;
- loads and freezes the effective profile, policy, model, system prompt, Tool
  catalog, retry/timeout/step limits, and provider-neutral initial conversation;
- validates user input and object-size limits.

Failure before publication establishes no durable `TurnId`, PendingTurn, user
message, or provider request. An in-memory UUID candidate used to build the CAS
object is unobservable and may be discarded.

`TurnStarted` atomically publishes the user message, explicit `TurnId`, parent
conversation tip, complete canonical effective profile/policy snapshot and
digest, model/provider selection, and execution limits. The snapshot contains no
credentials; configuration source paths are diagnostics only. Once published,
the request is accepted and every later failure, cancellation, or crash must close
that Turn rather than erase it.

The enabled Tool catalog is stored as a content-addressed provider-neutral
snapshot containing ordered names, descriptor identities, descriptions,
canonical input schemas, and runtime build identity, plus its digest. Provider
wire projection is not stored. The snapshot has a 1 MiB protocol limit and is
validated before `TurnStarted`.

### 9. PendingTurn is one immutable CAS journal

`refs/.../active-turn` points directly to the current immutable PendingTurn
snapshot. There is no second `pending/<TurnId>` ref and no active-Turn field in
`SessionState`. Every snapshot points to its predecessor; stage progression is
validated and published by CAS.

The durable states are:

```text
TurnStarted
ModelAttemptStarted
ModelAttemptFailed       # retryable or retry-exhausted failure before any stream event
ModelAttemptInterrupted  # partial stream, limit, cancellation, timeout, or crash
ModelAttemptRejected     # provider Done, invalid complete output
MessageComplete          # valid zero-Tool message
BindingPlanned           # valid Tool message + candidates, atomically
Bound
StepClosing
StepClosed
TurnClosing
Committed
```

`Committed` is an immutable final journal object constructed with the TurnCommit
candidate. It is never separately published through `active-turn`: the one
SessionStore ref transaction makes the TurnCommit/`turns/<TurnId>` pin and its
referenced `Committed` object reachable while deleting `active-turn`. This keeps
the accepted stage name without creating a second mutable completion authority
or a circular object reference. The direction is
`turns/<TurnId> -> TurnCommit -> Committed -> predecessor`; `Committed` contains
the data used to construct the TurnCommit but never its OID.

For a valid Tool-bearing response, the coordinator generates all candidate
`InvocationId`s and publishes `BindingPlanned` together with the embedded
complete unbound message. There is no durable window containing a Tool-bearing
`MessageComplete` without a binding plan. `BindingPlanned` stores `PlanKey`, an
ordered binding set, each candidate ID, each `ToolCallAddress`, method,
descriptor identity, canonical arguments digest, and per-binding digest. It does
not store a second batch digest.

A clean UUID collision before any reservation may CAS-publish a new candidate
revision only after InvocationStore proves the prior exact plan absent. Any
uncertain publication or partial/different reservation reuses the exact durable
candidates and fails closed on mismatch.

`Bound` is published only after `reserve_batch` confirms the entire exact plan
durable. A crash at `BindingPlanned` with the plan absent may closure-only reserve
the same candidates, publish `Bound`, terminalize them as
`NotExecuted(BeforeDispatch)`, and generate ToolResult closures. It never creates
new candidates or executes a Tool.

`StepClosing` is one stage represented by successive immutable snapshots. It
contains the complete ordered bindings and a monotonic closure cursor. Only the
lowest unclosed ordinal may advance; the current binding's exact ResultDraft
candidate is recorded before draft publication. The next Tool is not dispatched
until the current terminal fact and ToolResult closure are durable. Later
bindings that will not run receive ordered `NotExecuted` closures. `StepClosed`
is published only when every binding is closed.

### 10. The coordinator owns the loop; Agent drives one attempt

`lattice-api::TurnCoordinator` is the only production cross-store saga and
production invocation initiator. It owns:

- full-Turn locking and PendingTurn stages;
- profile/policy/catalog freezing;
- Turn deadline, model-step limit, provider retry/backoff, cancellation, and
  usage accounting;
- `InvocationId` allocation and batch reservation;
- Tool dispatch, ResultDraft, terminal evidence, closure, and final Turn commit;
- live recovery and reopen reconciliation.

`lattice-agent` becomes a one-provider-attempt `ModelStepDriver`. It receives an
assembled `ConversationHistory` and frozen request data. It owns no Session or
Invocation store, UUID generation, multi-step loop, Tool executor, recovery,
completion hook, or Turn-commit authority. `ProvisionalCompletionHook` and its
decision vocabulary are deleted rather than migrated.

Every outbound provider request is preceded by
`ModelAttemptStarted(ModelAttemptKey)`. Retry is allowed only when no stream
event has been received and the adapter explicitly classifies the opening error
as retryable. It writes `ModelAttemptFailed` before backoff and starts a new
attempt ordinal. Once any stream event arrives, error, timeout, cancellation, or
process failure is `ModelAttemptInterrupted` and is never automatically retried.
A Step accepts at most one completed valid message.

The live Turn deadline starts after `TurnStarted` publication using a monotonic
clock. `timeout_secs` is frozen, but a monotonic deadline is not persisted. The
deadline forbids starting later provider requests or Tool calls; it does not
interrupt mandatory terminal, Step, or Turn durability work. Reopen never
continues execution, so it does not reconstruct remaining time.

`MAX_MODEL_STEPS = 32` is a code-defined safety limit, recorded in
`TurnStarted`, and is not profile-configurable. A retry attempt does not consume
another model step. Reaching the limit closes the Turn as
`Failed(ModelStepLimitExceeded)` while preserving already closed messages and
ToolResults.

Stream assembly uses protocol limits, counted as UTF-8 bytes:

```text
single ToolCall arguments      8 MiB
assistant content             2 MiB
reasoning content             2 MiB
one ModelAttempt total       16 MiB
```

Exceeding a limit stops consumption and records
`ModelAttemptInterrupted(OutputLimitExceeded)` with bounded diagnostics. No
partial conversation or InvocationId is produced.

Each attempt stores provider-reported `Option<TokenUsage>`. Missing usage is
unknown, not zero; failed/interrupted attempts retain usage if supplied. Retry
attempts count separately. `StepClosed` has no independent cumulative usage
authority. `TurnCommit` may store a derived `Complete | Partial` aggregate whose
source of truth remains the attempt facts.

### 11. InvocationStore is one append-only ledger

`lancet-audit` owns `invocations.git` with one authoritative data ref and one
non-authoritative high-water witness:

```text
refs/lattice/invocations/ledger      # authoritative append-only event ledger
refs/lattice/invocations/high-water  # non-data monotonic witness
```

The first-created repository contains a domain genesis commit. Every
reservation, `DispatchStarted`, and terminal update is a direct child of the
prior ledger tip, published by CAS. Publication durably advances the ledger
before the witness. A crash may therefore leave the witness at a ledger
ancestor; reads accept that ordered state and a later write repairs it. A
missing or divergent witness, or a ledger behind the witness, fails closed, so
moving the ledger to an otherwise valid ancestor remains detectable. The
ledger cannot be rebased, merged, truncated, rewritten, or deleted; all cursors
remain ancestors of its tip.

`reserve_batch(PlanKey, exact_ordered_bindings)` is all-or-nothing. The exact
tuple includes `InvocationId`, Session/Turn/message/part addresses, method,
descriptor identity, and canonical arguments digest. It excludes provider
origin, provider call ID, opaque replay state, and workspace-history OIDs.

Repeating the same PlanKey and byte-identical bindings is idempotent. A different
binding for a recorded PlanKey, duplicate InvocationId in another plan, partial
batch, or domain mismatch fails closed. Reservation is the creation authority;
the later JSON-RPC request only dispatches an already reserved Invocation.

The lifecycle is:

```text
Absent
Reserved
DispatchStarted
Terminal {
    execution_outcome,
    result_material,
    complete Cell evidence,
}
```

`Absent` is a query result, not a record. `DispatchStarted` must be durable before
handoff to the executor and is the conservative “may have occurred” boundary.
There is no `HandoffCompleted` state. A reserved ID is permanently consumed.

The terminal and all Cell evidence for an Invocation are one atomic ledger
commit/ref publication. The execution outcome is a closed sum:

```text
Succeeded | Failed | Rejected | Cancelled | NotExecuted | Indeterminate
```

Result material is independently `Available { digest, byte_length }` or
`Unavailable { reason, digest?, byte_length? }`. Thus a successful execution can
have unavailable model-visible material without being relabeled as execution
failure.

Only standard JSON-RPC request `id` carries the canonical `InvocationId`; the
deleted `session.envelope` Tool is not reintroduced. A request references an
existing `Reserved` invocation. On request acceptance, the server durably
publishes `DispatchStarted` before executor handoff. Notifications may reference
an existing ID for cancellation/progress/control but cannot reserve or dispatch
an effectful Tool invocation.

### 12. ResultDraft is material, never outcome authority

After a Tool returns, the coordinator canonicalizes provider-neutral
`ToolContent`, computes its digest/length and exact candidate OID, then publishes
a `StepClosing` snapshot before attempting the draft write.

```text
ResultDraft {
    InvocationId,
    binding_digest,
    TurnId,
    ModelMessageAddress,
    part_ordinal,
    canonical_tool_content,
    content_digest,
    byte_length,
}
```

The draft contains no outcome. It is temporary cross-store material and cannot
upgrade itself into success or failure after a crash. Publication is classified:

```text
Durable { draft_oid }
CleanAbsent
Uncertain { exact candidate identity and digests }
```

Only a durable draft matching the terminal digest/length may become a formal
content-bearing ToolResult. `CleanAbsent` permits a terminal with the true
execution outcome and `Unavailable(DraftPublicationFailed)`. `Uncertain` must be
revalidated against the exact candidate during the live finite
`FinalizeBudget`; mismatch is corruption.

Canonical ToolContent is limited to 65,536 bytes. A plus-one result is never
truncated, because an arbitrary prefix may be invalid JSON, invalid UTF-8, or a
semantically false result. Oversize content writes no draft; the terminal keeps
the real execution outcome plus
`Unavailable(OutputTooLarge { limit, actual_length, full_digest })`. Tools that
need large output must define pagination, cursors, or summaries in their own
contract.

`FinalizeBudget` begins after the executor returns and covers repository-lock
acquisition, uncertain draft revalidation, terminal publication attempts, and
retry delays. The Turn deadline does not govern it. Its shape is fixed here:
the first attempt is immediate, retriable durability failures back off
deterministically, no new attempt starts after the deadline, and corruption or
identity/digest mismatch is never retried.

# 139-A's retained 20+20 Arch Linux/Btrfs qualification previously approved a

**60-second monotonic budget**: the first attempt is immediate, retriable
durability failures use deterministic exponential delays from 25 milliseconds
capped at 1 second, and no new attempt starts after the deadline. It is the
finite envelope above one worst-contention repository-lock acquisition, one
complete publication, and one retry. Bundle `20260819T150644Z-a9eca929371e`
predates the post-re-review repairs (8/20 and 7/20). The post-repair bundle
`20260820T061235Z-0f3d19a6bdb3` (source `0f3d19a6`) is a historical
**valid 5/20 full-finalize FAIL**. The barrier-fix bundle
`20260820T112752Z-d9744add1280` (source `d9744add`) is a historical
**valid 9/20 full-finalize FAIL** and **9/20 terminal-publish FAIL**. The
fanout bundle `20260820T173608Z-d2300158fee2` (source `d2300158`) is a
historical **valid 10/20 full-finalize FAIL** and **9/20 terminal-publish
FAIL** against K=4/20 and K=3/20 **under the old per-step `sessions-N.git`
benchmark**. The corrected-harness bundle
`20260821T115648Z-d474236ca1e6` (source `d474236`) is a **valid 5/20
full-finalize FAIL** and **5/20 terminal-publish FAIL**. K is not raised.
The `FinalizeBudget` remains an architecture and failure-semantics constraint.
On 2026-08-22, #139-A was approved on its correctness and structural evidence;

# 139-B may proceed. The performance qualification is diagnostic-only pending

GitHub #153, and historical campaign verdicts remain unchanged evidence rather
than a production-wiring prerequisite. Journal CAS callers must not add a second
`fsync_pending` after successful `cas_ref`; that contract change is not an
incidence fix. First-create precreates the complete loose-object fanout
and steady-state object publication does not fsync top-level `objects/`.
Five Btrfs diagnostic rounds of the old per-step harness measured **43–44**
transaction commits per result-finalize run versus the previous **862–863**.
The authorized 20+20+20 on that old benchmark still fails incidence. The
later 20+20+20 on the corrected harness also fails incidence (5/20 / 5/20).

Run incidence is a contract-v1 gate and is retired to a diagnostic by
qualification contract v2
(`docs/plans/2026-08-21-issue139a-qualification-contract-v2.md`): a per-run p99
over 160 samples is the second-order statistic, and it flipped verdict between
two campaigns whose measured product was byte-identical. Contract v2 keeps the
60-second budget, its backoff schedule, and its coupling rule unchanged, gates
the worst single finalize absolutely, and requires every super-threshold stall
cluster to be attributable to a shared kernel durability wait. Under v2 the
`d474236` evidence fails one gate: strict `GitSessionGraphStore::open` cost
rises monotonically within a run (FINDINGS L-GH139A-11), which the retired
per-step harness structurally hid. Its remaining performance implications are
tracked by #153 and do not block #139-B.

Cold reads and first writers cost O(state bytes) plus a bounded delta replay,
decoupled from accumulated history depth, via periodically anchored
authenticated `state_digest` checkpoints; an ordinary transition's critical
section is O(event bytes). The maximum-latency regression gates are coupled to
this ceiling and cannot be widened independently of it.

An earlier 60-second figure recorded on 2026-08-18 from the first 20+20 run was
withdrawn, because the `RESULT_FINALIZE_STRESS` harness it came from published
Invocation terminals while the PendingTurn was still `Bound` and so never
measured the real durable order. The 60-second envelope above was later
re-derived from the corrected harness; that later derivation is itself
historical pending post-repair requalification. The measured baselines and
regression gates are recorded in
`docs/plans/2026-08-18-issue139a-qualification-approval.md`.

If the budget expires before terminal/evidence publication, the Invocation stays
`DispatchStarted` and the caller receives
`EvidenceDurabilityFailed` with the affected Invocation classified as
indeterminate. The coordinator publishes no later PendingTurn stage: the Turn
remains at the existing `StepClosing` snapshot, `active-turn` remains present,
and there is no `StepClosed`, TurnCommit, Turn pin, or durable `TurnOutcome`.
Later bindings remain reserved and undispatched; they are not closed until the
InvocationStore is writable.

The next Session command performs reconciliation only and receives a fresh,
finite finalization budget. It first writes
`Terminal(Indeterminate(MayHaveOccurred))` for the affected
`DispatchStarted` Invocation, then ordered
`NotExecuted(PrecedingInvocationIndeterminate)` terminals for the remaining
reserved bindings, advances `StepClosing`, and closes the Turn as
`Interrupted { ClosureOrigin::Reconciled }`. It never uses an existing draft to
infer outcome or reconstruct evidence. If the InvocationStore remains
unwritable or that finite budget also expires, the Turn remains pending and the
command returns `RecoveryRequired(EvidenceDurabilityFailed)`; no new prompt is
accepted.

### 13. Batch execution and cancellation are ordered and fail closed

All InvocationIds in one model ToolCall batch are reserved before any dispatch.
Tools execute in part order, one at a time.

After a deterministic, durable `Succeeded`, `Failed`, or `Rejected`, the next
binding may execute. The batch stops after `NotExecuted`, `Cancelled`,
`Indeterminate`, or any terminal/evidence durability uncertainty. When the
InvocationStore is writable, remaining bindings receive ordered
`NotExecuted(PrecedingInvocationIndeterminate)` or the corresponding
cancellation closure without dispatch. If it is not writable, they remain
reserved and the Turn stays at `StepClosing` under the FinalizeBudget exhaustion
rule in section 12.

Cancellation is classified per invocation:

- an existing terminal remains unchanged;
- `Reserved` becomes `NotExecuted(CancelledBeforeDispatch)`;
- confirmed executor cancellation after `DispatchStarted` becomes `Cancelled`;
- uncertain execution becomes `Indeterminate`.

The containing Turn closes as cancelled when cancellation is the controlling
cause, but already durable invocation facts and conversation closures remain.

### 14. Reopen performs closure only

Reconciliation is a pure decision function over a Pending snapshot and a pinned
Invocation facts snapshot. I/O executes the returned plan through narrow domain
ports. Reopen never sends a provider request, reruns a completion hook,
redispatches a Tool, changes uncertain binding identity, or guesses success or
failure.

Every durable stage has exactly one reopen closure. The exhaustive rules are:

- `TurnStarted` with no attempt ->
  `Interrupted(ProcessInterruptedBeforeModelAttempt)`;
- incomplete `ModelAttemptStarted` -> interrupted Turn;
- `ModelAttemptFailed` between retries -> interrupted Turn;
- non-retryable `ModelAttemptFailed` -> `Failed(ModelRequestFailed)`;
- `ModelAttemptInterrupted` -> `Interrupted` with its durable typed reason;
- `ModelAttemptRejected` -> `Failed` with its durable typed rejection;
- zero-Tool `MessageComplete` -> `StepClosed(FinishTurn)` using the already
  durable complete message, then completed `TurnClosing`; no policy hook exists;
- `BindingPlanned` with absent plan -> reserve the exact candidates, mark them
  `NotExecuted(BeforeDispatch)`, and close the Step;
- `BindingPlanned` with the exact durable reservation -> publish `Bound` and
  continue closure; a partial or different plan is corruption;
- `Bound/Reserved` with no `DispatchStarted` -> `NotExecuted(BeforeDispatch)`;
- `DispatchStarted` with no terminal -> conservative
  `Indeterminate(MayHaveOccurred)` when it can be made durable;
- existing terminal -> idempotent ToolResult closure;
- `StepClosing` -> resume at its exact lowest unclosed ordinal using that
  binding's pinned draft candidate and Invocation fact; earlier ordinals are
  immutable and later ordinals cannot be skipped;
- `StepClosed(ContinueModel)` -> interrupted Turn, never a next provider request;
- terminal `StepClosed` -> its recorded `TurnClosing` outcome;
- `TurnClosing` -> idempotent final SessionStore publication;
- a reachable `Committed` object with an uncompleted final ref transaction ->
  retry that exact SessionStore transaction and fail closed on any CAS mismatch.

`StepClosed` is a closed sum:

```text
FinishTurn { final_message }
ContinueModel { ordered_tool_results, conversation_digest }
FailTurn { reason }
CancelTurn { reason }
InterruptTurn { reason }
```

Only a live owner may advance `ContinueModel` to another provider attempt.

A recovered Turn may end with ordered ToolResults and no synthetic assistant
message. Its outcome is interrupted with `ClosureOrigin::Reconciled`. The next
user request may project explicit closure framing if a provider protocol needs
it, but that framing is not model-authored speech.

If a user sends a new prompt while `active-turn` exists, that command reconciles
only and reports the recovered Turn; it does not silently accept the new prompt.
The user sends the prompt again after recovery. There is no force-discard command.

### 15. Turn outcome and final checkpoint are typed

The target replaces current `TurnCommitStatus + WorkspaceDisposition +
terminal_reason` with:

```text
TurnOutcome =
    Completed { finish_reason }
  | Failed { reason: TurnFailure }
  | Cancelled { reason: TurnCancellation }
  | Interrupted { reason: TurnInterruption }

ClosureOrigin = Live | Reconciled
```

Free text may be retained as bounded diagnostics but is never logic authority.
`WorkspaceDisposition` and `TurnLifecycleEvidence` are deleted; they belong to
the worktree/promotion lifecycle removed by GitHub #62.

Every committed Turn records a project-global WorkspaceHistory checkpoint:

```text
WorkspaceHistoryCheckpoint = Empty | Oid(ObjectId)
```

It is a cut through all LATTICE mutations in that project, including other
Sessions, not a receipt containing only the current Turn's mutations. Under a
shared `WorkspaceOperationLock`, Turn closing reads the current history ref and
holds the shared lock through the SessionStore Turn/ref transaction. A mutation
cannot be inserted between the checkpoint and Turn publication. A read failure
is an error, never silently converted to `Empty`.

### 16. WorkspaceHistory records association, not terminal authority

Each successful mutation-history commit records `InvocationId`, `TurnId`, effect
ordinal, method, path, before/after revisions, and the mutation content needed by
rewind. It does not record numeric `call_id` or provider metadata. Invocation
terminal does not copy a workspace-history OID; `TurnCommit` holds the closing
project checkpoint.

Mutation takes the exclusive project lock through install and history append. A
history failure after installation is a durability failure: the execution is
reported indeterminate and never success. InvocationStore may record that
indeterminacy and observed effect evidence, but the system does not scan history
to guess a successful terminal.

Rewind uses the target Turn's project-global checkpoint, so it plans the global
LATTICE mutation suffix after that cut, including later mutations from other
Sessions. Existing divergence checks still abort before writes. The
`RewindCoordinator` in `lattice-api` shares the lock order with TurnCoordinator;
it is not part of PendingTurn. ADR 0005 and the #139 program do not add a rewind
crash journal. If the existing rewind workflow later requires a new durable
cross-store journal, that needs a separate issue and architecture decision.

### 17. Operator reads are direct pinned ledger reads

Operator query, tail, replay, and status stay in `lancet-audit` and are only
re-exported through `lancet-api`. They do not compose CEP/runtime and do not take
Session or Workspace operation locks.

- query pins one ledger tip and returns one current-state row per InvocationId,
  including unfinalized records;
- tail returns lifecycle transitions after a reachable commit-OID cursor;
- replay returns durable terminal facts and never invents evidence;
- status is a bounded projection of a pinned snapshot, not persistence authority.

Reads may observe an intermediate saga state. That is expected. The public domain
has no numeric `audit_id`, `call_id`, or `evidence_id`; Git OIDs are cursors and
storage addresses, not invocation identity.

### 18. Filesystem support is capability-qualified per target location

Capability probes run against every actual publication parent/location used by a
store, not once per host or filesystem name. They exercise:

- same-mount sibling publication;
- `renameat2(RENAME_NOREPLACE)`;
- file and directory fsync;
- local `flock` behavior;
- minimal repository create, publish, reopen, and identity validation.

A missing primitive fails closed. Local Arch/Btrfs is the maintained full
correctness, fault-injection, and performance gate. Remote Rocky/ext4 runs a
prebuilt lightweight qualification binary for mount reporting, probes, and
minimal repository smoke; it does not compile the workspace or set performance
thresholds. Another filesystem that passes the same probes may run as
`Unverified` without creating another maintenance promise.

SIGKILL/reopen tests demonstrate process-crash consistency. Injected barrier
failures demonstrate call ordering and classification. Neither is described as
proof of real hardware power-loss consistency.

### 19. Ownership and dependency graph

Semantic ownership remains deep and separate:

```text
lancet-protocol      InvocationId, TurnId, canonical wire/data primitives
lattice-conversation provider-neutral conversation and replay metadata contract
lattice-store        generic private-Git/Linux durability kernel
lattice-session      SessionState, conversation DAG, PendingTurn, ResultDraft
lancet-audit         InvocationStore, Cell evidence, operator reads
lancet-runtime       Tool Host, Local Workspace, WorkspaceHistory
lattice-llm           provider codecs and provider request/stream behavior
lattice-agent         one-attempt ModelStepDriver
lattice-api           TurnCoordinator and RewindCoordinator
```

The exact post-#139-B graph, before #32/#34 remove migration crates, is:

```text
Layer 0:    lancet-protocol   net-guard   lattice-store
Layer 0.5:  lattice-conversation(->lancet-protocol)
            lattice-llm(->net-guard,lattice-conversation)
Layer 1:    lancet-catalog(->lancet-protocol)
            lattice-profile(->lancet-protocol)
            lattice-agent(->lattice-llm,lattice-conversation)
            lattice-session(->lancet-protocol,lattice-conversation,lattice-store)
Layer 2:    lancet-dispatch(->lancet-protocol,lancet-catalog)
            lancet-audit(->lancet-protocol,lancet-catalog,lattice-store)
            lancet-authority(->lancet-protocol,lancet-catalog,net-guard)
Layer 2.5:  lancet-cep(->lancet-protocol,lancet-catalog,lancet-dispatch)
            lancet-runtime(->lancet-protocol,lancet-dispatch,
                            lancet-authority,lattice-store)
Layer 3:    lancet-native(->lancet-protocol,lancet-catalog,lancet-dispatch,
                          lancet-audit,lancet-authority,net-guard,
                          lancet-runtime)
Layer 3.5:  lancet-api(->lancet-protocol,lancet-catalog,lancet-dispatch,
                       lancet-cep,lancet-audit,lancet-authority,
                       lancet-native,lancet-runtime)
Layer 4:    lattice-api(->lancet-api,lancet-protocol,lancet-catalog,
                        lancet-audit,lancet-authority,lancet-native,
                        lattice-agent,lattice-conversation,lattice-llm,
                        lattice-profile,lattice-session)
Layer 5:    lattice-tui(->lattice-api)
            lattice-cli(->lattice-api,lattice-session,lattice-tui,lancet-api)
```

After #34 removes `lancet-catalog`, `lancet-dispatch`, `lancet-cep`, and
`lancet-native`, the contraction is:

```text
Layer 0:    lancet-protocol   net-guard   lattice-store
Layer 0.5:  lattice-conversation(->lancet-protocol)
            lattice-llm(->net-guard,lattice-conversation)
Layer 1:    lattice-profile(->lancet-protocol)
            lattice-agent(->lattice-llm,lattice-conversation)
            lattice-session(->lancet-protocol,lattice-conversation,lattice-store)
Layer 2:    lancet-audit(->lancet-protocol,lattice-store)
            lancet-authority(->lancet-protocol,net-guard)
Layer 2.5:  lancet-runtime(->lancet-protocol,lancet-authority,net-guard,
                            lattice-store)
Layer 3:    lancet-api(->lancet-protocol,lancet-audit,lancet-authority,
                       lancet-runtime)
Layer 4:    lattice-api(->lancet-api,lancet-protocol,lancet-audit,
                        lancet-authority,lattice-agent,lattice-conversation,
                        lattice-llm,lattice-profile,lattice-session)
Layer 5:    lattice-tui(->lattice-api)
            lattice-cli(->lattice-api,lattice-session,lattice-tui,lancet-api)
```

The graph is documentation until each prerequisite lands. GitHub #108 remains
the machine-enforced dependency-graph owner and is updated incrementally, not by
teaching `lattice-store` domain schemas.

### 20. Implementation is a hard cut in ordered prerequisites

```text
#114 (complete)
  -> #141 ADR alignment
       -> #142 InvocationId + TurnId + lattice-conversation ----+
       -> #143 lattice-store + existing store migration --------+-> #144 PendingTurn
                                                                  -> #139-A
                                                                  -> #139-B
                                                                  -> #32
                                                                  -> #34
```

# 142 adds data types, strict Tool argument/message contracts, provider fixtures

and conversation ownership without switching the production numeric path. #143
adds the generic kernel, XDG topology, SessionState/head persistence, operation
locks, and WorkspaceHistory mechanics without creating production
InvocationStore. #142 and #143 may land independently after #141. #144 consumes
both to add the TurnId-keyed permanent Turn pins, direct `active-turn` journal,
PendingTurn state machine, and pure reducer against synthetic Invocation facts.

# 139-A implements the real non-production InvocationStore and stress harnesses

`LOCAL_LEDGER_STRESS` retains the accepted 8 sessions × 20 steps × 8 invocations
= 1,280 invocations and 2,720 ledger commits. `RESULT_FINALIZE_STRESS` combines
the real SessionStore and InvocationStore with synthetic result/effect facts.
Campaign sample counts are set by the qualification contract, not by this ADR:
contract v2 runs `LOCAL_LEDGER_STRESS` ten times, `RESULT_FINALIZE_STRESS`
twenty times, and `cold_reopen_benchmark` eight times on maintained Btrfs, each
count justified by estimator precision or by host-stall exposure in
`benchmarks/invocation-store/qualification-contract-v2.json`. The earlier flat
"twenty times" requirement is superseded; it supported contract v1's run
incidence, which contract v2 retires to a diagnostic. Contract v2 adjudicates a
campaign through deterministic-structure, typical-cost, scaling-invariance,
safety-budget, fairness, and stall-attribution gate families. `FinalizeBudget`
and its coupling rule are unchanged.

The corrected-harness 20+20+20 on `d474236` is a valid contract-v1 5/20 / 5/20
FAIL against K=4/20 and K=3/20, and under contract v2 an external evaluation
fails only the revalidation scaling gate (FINDINGS L-GH139A-11). The fanout
20+20+20 on `d2300158` remains a historical 10/20 / 9/20 FAIL of the old
per-step `sessions-N.git` benchmark; the prior 9/20 on `d9744add` and 5/20 on
`0f3d19a6` remain historical. K is void for contract v2 and was never raised
for contract v1. #139-A is approved and #139-B may proceed; the qualification
redesign is tracked by #153.
Record: `docs/plans/2026-08-21-issue139a-qualification-contract-v2.md`.

# 139-B performs one production hard cut: canonical JSON-RPC InvocationId

TurnCoordinator, dispatch, ResultDraft, terminal evidence, operator Adapter,
conversation/Turn outcome schema, and deletion of SQLite persistence, numeric
IDs, old sidecars/lifecycle fields, ProvisionalCompletionHook, legacy readers,
imports, and dual writes. #139-B retains the existing
`lattice-api -> lancet-native` transition edge; #34 deletes it when
`lancet-native` itself is removed.

# 143 is already a no-import hard cut for the Session and WorkspaceHistory

locations/formats: old config-root Session data and old WorkspaceHistory data are
manually removed/rebuilt, and rewind does not span the cut. #143 does not change
or orphan the transitional SQLite audit resolver; that store remains the #114
operator authority until #139-B deletes its path. #139-B then requires another
manual development-state rebuild for the production InvocationStore cut.

## Amendments to existing authority

ADR 0003 remains authoritative for guarded Direct-Local Workspace execution.
This ADR does not reintroduce a projection, snapshot lifecycle, or pluggable
workspace backend.

ADR 0004 remains authoritative for build-closed Component Tools, manifest-owned
descriptors, Tool Host authority, and no ambient WASI. This ADR amends only its
decision 8/lifecycle implication: invocation/evidence/operator persistence moves
from transitional SQLite to InvocationStore, while WorkspaceHistory remains a
separate runtime domain.

GitHub #114's finalized-row SQLite Adapter is the complete transitional
authority. #139-A is non-production; #139-B switches the Adapter and deletes the
SQLite path. PROV, in-toto Turn statements, Turn attestations, and
stored-vs-derived projection remain retired.

Older #114 text that refers to “ADR 0005 decision 4” predates this document's
final section numbering; the normative amendment is this section and the ADR
0004 paragraph immediately above.

GitHub #24/#34 execution assumptions are amended: production invocation is
Session-bound and initiated only by `lattice-api::TurnCoordinator`. Lower-level
runtime/transport APIs remain internal test and composition seams. There is no
Standalone/direct CEP product contract.

## Consequences

- Every durable execution fact is joined by one typed `InvocationId`.
- Provider fidelity remains available without contaminating domain identity.
- Accepted user Turns and every unsafe retry boundary become explicit facts.
- Session, invocation, and workspace state share mechanics but not authority,
  locks, repositories, or failure domains.
- A project move deliberately creates a new state namespace; same-path reuse
  deliberately retains the old namespace.
- Cross-store failures remain visible, typed, and reconcilable rather than
  disguised as atomic success.
- Hard limits bound private-Git growth from provider and Tool output.
- The hard cut rejects migration complexity because the project is unpublished.

## Rejected alternatives

- recognizing a project by content, Git remote, inode, or generation registry;
- treating `[lancet.project_root].root` as project identity;
- storing sessions in XDG config or nesting state in the workspace;
- one repository, one cross-domain commit chain, or nested repository locks;
- a central schema authority or all-stores handle in `lattice-store`;
- `StoreInstanceId` and replacement-detection recursion;
- mutable Session sidecars or two active-Turn authorities;
- reusing provider IDs, numeric IDs, or SQLite IDs as invocation identity;
- synthetic provider IDs for cross-provider replay;
- optional InvocationId instead of bound/unbound conversation typestate;
- general Tool-result truncation;
- using ResultDraft as execution outcome/evidence authority;
- resending provider requests or redispatching Tools after reopen;
- a background reconciler, force-discard command, or unlimited finalize retry;
- a direct CEP/Standalone production caller;
- restoring PROV/in-toto/Turn attestation;
- a filesystem-name allowlist or remote workspace compilation requirement.

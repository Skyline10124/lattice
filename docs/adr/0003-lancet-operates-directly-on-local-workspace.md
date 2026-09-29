---
status: accepted
---

# ADR 0003: LANCET Operates Directly on the Local Workspace

- Status: Accepted
- Date: 2026-07-16
- Supersedes: ADR 0002
- Amended by: ADR 0004 (Tool catalog and execution ownership only)
- Superseded in part by: ADR 0006 (the Rewind section only; file Rewind is removed and `rewind` moves only the Session position)
- 术语导航：[领域术语表](../reference/glossary.md)

> 承接说明：Direct-Local、路径策略、单路径安装与 Rewind 的已承接规范由[工作区合同](../architecture/workspace.md#来源与承接范围)维护；工具目录与执行归属结合 ADR 0004 的修订，见 [LANCET 合同](../architecture/lancet.md#构建与封闭组成)及[组件职责](../architecture/lancet.md#组件职责)。
> 本 ADR 保留决策理由、取舍、历史及修订关系，原实施顺序不作为当前任务；现行规则由上述合同承接，仍需核验的路径竞争、效果报告和 Rewind 恢复问题见[工作区合同待核验项](../architecture/workspace.md#实现与验证待办)。

## Context

ADR 0002 made the Local Workspace authoritative but still copied it into a
turn-scoped isolated workspace mirror, snapshotted each Tool call, watched
Local changes, merged two filesystem histories, and wrote approved results
back. Evaluation of that projection-and-isolation stack (and of alternative
VM sandbox candidates) showed that faithfully projecting project trees,
preserving metadata, establishing consistent savepoints, and operating the
required VM infrastructure creates more machinery than the Agent's actual
capability surface requires.

The production CEP catalog contains exactly five Agent-visible methods:
`observe.workspace`, `mutate.create`, `mutate.edit`, `fetch.web`, and
`session.todo_write`. Command execution is permanently retired. The Agent
cannot launch a shell, run a user binary, install a plugin, or submit an
arbitrary program. Workspace side effects are therefore enumerable structured
operations whose path, input, effect, and result can be checked directly.

The sandbox design optimized for arbitrary process execution that LATTICE no
longer exposes. Keeping it would add a second filesystem, projection rules,
snapshot retention, reconciliation, and merge failure modes without enclosing
an arbitrary execution capability.

## Decision

1. **LANCET operates on the Local Workspace directly.** The configured,
   canonical project root is both the observation source and mutation target.
   There is no Turn Workspace, Workspace Projection, Upper, Local watcher,
   writeback phase, replacement sandbox, or retained workspace snapshot.

2. **The closed structured catalog is a hard safety boundary.** No production
   Agent method may accept a shell command, command line, executable path,
   script, plugin, or user-supplied program. Adding arbitrary process execution
   would invalidate this decision and requires a new ADR with a new isolation
   boundary. Internal implementation helpers likewise must not reinterpret
   Agent data as command syntax.

3. **`lancet-native` owns one concrete Local Workspace module.** Native
   handlers call project-relative observation and mutation operations on this
   module after authority, Guard L1, effect, and policy checks. The module hides
   secure path resolution, symlink handling, bounded reads, content hashing,
   atomic replacement, and filesystem error normalization. It is an internal
   deep module, not a public pluggable execution-backend interface.

4. **Paths are rooted and policy-visible.** Every workspace path is normalized
   as UTF-8 project-relative input and resolved beneath the canonical project
   root without following an escaping symlink. Runtime-owned System Path
   Exclusions always deny project Git administrative state, LATTICE private
   state, and any path outside the root. Profile policy may further reduce read
   or write access but can never relax those exclusions. Write policy remains a
   blacklist rather than an allowlist envelope.

5. **Observation visibility preserves Git-ignore boundaries without copying
   files.** The Policy-Visible Workspace contains tracked and non-ignored files
   by default. `[lancet.workspace].included_ignored_read_patterns` may expose
   specific ignored paths, while `denied_read_patterns` may remove paths from
   either set. Precedence is System Path Exclusion, explicit read denial,
   explicit ignored-path inclusion, then ordinary Git-ignore selection. Every
   `observe.workspace` mode applies the same rule to file contents, names, and
   search results; direct Local access must not newly expose ignored secrets.

6. **Observations are bounded snapshots of the operation they answer.** File
   observations return a content hash with the bytes or metadata used to answer
   the request. Directory, search, and grep observations enforce entry, depth,
   byte, and match limits and report truncation or limit failure explicitly.
   There is no claim that separate observations share one global filesystem
   snapshot.

7. **Mutations use optimistic preconditions.** `mutate.edit` must carry the
   expected content hash of the file state on which the edit was planned.
   `mutate.create` requires the target to remain absent. Native code rechecks
   the applicable condition at commit time. A stale or missing precondition
   returns `ReplanRequired`; the runtime never silently retries the old Tool
   call, auto-merges it, or overwrites newer state as an intended behavior.

8. **Successful mutations are atomic and immediate.** A mutation writes a
   temporary file in the target directory, applies required metadata, syncs as
   required by the durability contract, and atomically installs the target.
   A create uses no-replace installation. An edit replaces only after its
   precondition and policy checks pass. A successful operation is immediately
   visible in Local Workspace and is not rolled back if a later Tool call, model call,
   or turn fails.

9. **Filesystem atomicity is not external-writer serializability.** Atomic
   installation prevents torn Agent writes. LANCET detects stale state at its
   explicit checks, but a non-cooperating process may write between the final
   check and installation or immediately afterward. The runtime cannot provide
   a cross-process content-hash compare-and-swap on ordinary filesystems
   without locking or isolating every writer. This lost-update window is an
   accepted consequence of direct Local operation and must not be described as
   sandbox-equivalent isolation.

10. **Session History does not own Local files.** Session commits retain Tool
    transcripts, audit identity, and the private workspace-history object ID
    reached after the Turn, not a copied working directory. Ordinary session
    checkout or branch movement changes conversation state only. Explicit
    `/rewind` additionally restores recorded LATTICE mutations as described
    below.

11. **Workspace-specific sandbox profile is retired.** The `[workspace]`
    image, CPU, memory, egress, credential, and warm-template configuration is
    removed with the Upper lifecycle. `[lancet.workspace]` remains the policy
    home for Local observation and mutation restrictions. Network policy for
    `fetch.web` remains under LANCET network configuration and continues to use
    host allowlisting, DNS pinning, redirect checks, and SSRF guards.

## Rewind

`/rewind` reverses only successful Atomic Local Mutations made by LATTICE after
the selected Turn. It preserves unrelated external changes. If a path changed
by LATTICE no longer has the recorded post-mutation content, rewind reports a
conflict rather than overwriting the current Local Revision.

Workspace history is stored in a private bare repository implemented with
`gix`, outside both the Local Workspace and the user's project `.git`. It stores
Git blobs, trees, commits, and refs for LATTICE mutation history; it is not an
execution workspace, projection, or sandbox. Before and after states are
represented by Git trees rather than a parallel custom blob or receipt format.

Each completed Turn records the private workspace-history object ID reached by
its mutations. To rewind, the runtime walks LATTICE mutation commits after the
target in reverse, validates their recorded post-state against current Local
content, restores the corresponding parent-tree state, then moves the private
workspace-history ref and Session History ref to the target. The source object
IDs remain reachable through internal refs without a time-based expiry. This
decision defines no additional user-facing command or name for those internal
refs.

Rewind does not append compensation commits. Audit records the operation, while
gix objects retain file history and mutable refs identify the selected state.

## Module Seams

- `lancet-catalog` remains the sole declaration of Agent-visible methods and
  their typed contracts.
- `lancet-authority` continues to own policy semantics, capability matching,
  Guard L1, effect regions, and the effect ledger.
- `lancet-native` owns concrete Local filesystem, web, mutation, audit, and
  effect-contract execution. Its Local Workspace module takes a canonical root
  and resolved policy; it does not expose shell or process execution.
- `lancet-api::LancetRuntime` composes native execution from a project root and
  policy. `LancetRuntimeConfig` no longer receives `ExecutionRouting` or an
  externally supplied workspace backend.
- **Ownership amended by ADR 0005/#139-B:** `lattice-api` still resolves profile
  policy and project root and builds the LANCET runtime, but
  `TurnCoordinator` now owns binding, reservation, dispatch, durable closure, and
  the final Session transaction. `lattice-agent::ModelStepDriver` performs one
  provider attempt and receives no CEP client or store. No layer allocates,
  retains, restores, or destroys an Upper.
- `lancet-native` owns the private gix workspace-history implementation alongside
  the concrete Local Workspace module. `lattice-api` coordinates explicit
  `/rewind` with `lattice-session`; neither layer writes the user's project
  `.git`.

  > **Ownership amended by GitHub #63 (2026-07-26).** The private gix
  > workspace-history implementation landed in `lancet-runtime`
  > (`lancet_runtime::WorkspaceHistory`), not `lancet-native`, following ADR
  > 0004's placement of the concrete host responsibilities and the crate-role
  > table in the root `AGENTS.md`. `lancet-native` still owns *when* a mutation
  > is recorded: its `LocalWorkspace` holds the history handle and its mutation
  > handlers record only after a successful atomic install, so installation and
  > recording remain behind one concrete host module as this ADR requires.
  > Everything else in this bullet is unchanged — no layer writes the user's
  > project `.git`.
- `lattice-workspace`, `WorkspaceExecutionBackend`, `ExecutionBackend`,
  `ExecutionRouting`, `UpperContext`, and snapshot-journal coordination are
  migration residue and are removed rather than retained as compatibility
  abstractions.

Tests use temporary Local project roots through the production concrete module.
Narrow internal fault injection is permitted to prove that failure before
atomic installation leaves the target unchanged; it must not recreate a
general backend plugin seam.

## Mutation Contract

The implementation must preserve the existing defense layers in this order:

1. Decode and validate the typed CEP request.
2. Resolve catalog authority and policy for the requested effect.
3. Parse and guard the project-relative path, including System Path Exclusions.
4. Resolve the target without following an unsafe symlink or crossing the root.
5. Enforce read/write policy, operation limits, and effect-region constraints.
6. Check the mutation precondition against current Local state.
7. Prepare and atomically install the result without exposing partial content.
8. Record success, rejection evidence, content identities, and impact in the
   durable audit trail.

An error before installation leaves the target unchanged. If installation
succeeds but later audit finalization fails, the API reports the durability
failure without pretending the Local mutation was rolled back. Recovery and
audit diagnostics must preserve this distinction.

## Migration Sequence

1. Implement the concrete Local Workspace module in `lancet-native` by moving
   the secure host-filesystem behavior currently present only in test support
   behind native observation and mutation operations. Add temporary-directory
   tests for root escape, symlink parent/leaf rejection, System Path Exclusions,
   limits, expected-hash conflict, no-replace create, atomic replacement, and
   partial-write failure.
2. Change native registry composition and `LancetRuntimeConfig` to take the
   canonical project root directly. Remove `ExecutionBackend`,
   `ExecutionRouting`, routing invocation flags, Upper identity audit fields,
   `NullExecutionBackend`, and adapter-preflight metadata. Replace them with
   Local operation evidence that describes actual guarded paths and effects.
3. Remove `WorkspaceExecutionBackend`, `prepare_upper`, `UpperLease`, the
   workspace override test seam, snapshot journal/reconciliation, retained
   snapshot lifecycle, and all turn-level Upper creation from `lattice-api`.
   Prove plain-text turns and every catalog Tool run without KVM or an MSB
   daemon.
4. Remove `[workspace]` schema/defaults and its translation. Keep and tighten
   `[lancet.workspace]` policy, making System Path Exclusions non-configurable.
   Add `included_ignored_read_patterns` and `denied_read_patterns`, validate
   them as project-relative patterns, and apply one visibility decision across
   read, list, grep, and find operations without invoking a Git subprocess.
5. Delete guest-Git baseline delivery, candidate export, changed-path
   porcelain, warm templates, capable-host probes, KVM guards,
   and the `lattice-workspace` crate once no production or test dependency
   remains. Update generated architecture documentation after the dependency
   graph changes.
6. Replace retired-lane harness scenarios with Direct-Local scenarios covering
   observation, mutation, policy denial, concurrent external edits,
   cancellation, audit evidence, and session reopen against changed Local
   state. Remove tests whose only contract was sandbox lifecycle.
7. Add the private bare gix workspace history, bind its object ID into completed
   Turn commits, and implement `/rewind` by reversing recorded LATTICE mutations
   before moving workspace and session refs. Keep prior object IDs reachable
   through internal refs without append-only compensation commits or additional
   user-facing commands.

Each migration step must compile and retain the closed five-method catalog. No
temporary command-execution path or dual sandbox/Local production mode is
allowed during the cutover.

## Rejected Alternatives

- Continuing ADR 0002's Turn Workspace projection, savepoint, watcher, merge,
  and writeback model.
- Replacing the retired projection sandbox with another VM sandbox product;
  stronger VM snapshots do not remove projection, protocol-ownership, and
  deployment costs, and they restore process/memory state that LATTICE does
  not want.
- A Host-private project copy with later promotion. It retains duplicate state,
  reconciliation, and crash-recovery machinery while providing no process
  isolation benefit for the closed structured catalog.
- Storing rewind content in a custom blob/receipt database instead of using Git
  blobs, trees, commits, refs, and diff through `gix`.
- Writing workspace history into the user's project `.git`; private history must
  not alter user objects, refs, index, or HEAD.
- Recording rewind as append-only compensation commits. Rewind restores the
  selected state and moves refs; audit records that operation separately.
- A writable bind mount inside a VM. It mutates Local directly while retaining
  VM lifecycle cost and gives no rollback for the mounted content.
- Exposing every Local file by default. That would make direct execution reveal
  ignored secrets and build state that the existing Upper baseline omitted.
- Denying only a hand-maintained list of common secret filenames. Such a list
  cannot represent project-specific ignore policy and will inevitably miss
  sensitive paths.
- Preserving `ExecutionBackend` as a public abstraction for hypothetical future
  sandboxes. There is one selected production behavior; a shallow plugin seam
  would spread path and safety invariants across implementations.
- Reintroducing arbitrary command execution with path checks alone. Structured
  file policy cannot contain an arbitrary process with the host user's ambient
  authority.
- Automatically merging a stale edit or replaying the same Tool call. The Agent
  must observe current state and plan again after `ReplanRequired`.

## Consequences

- Normal turns no longer depend on KVM, libkrun, an MSB daemon, OCI images,
  guest package installation, projection transfer, or snapshot storage.
- Tool observations see current Local state, including external changes made
  between Tool calls, without a watcher or synchronization protocol.
- Ignored files stay hidden from all workspace observations unless profile
  policy explicitly includes them; this requires in-process Git-ignore
  evaluation on direct Local reads and searches.
- Approved writes have immediate host impact and survive later turn failure.
  Users can inspect them with their ordinary editor and project VCS.
- LATTICE gives up sandbox rollback, stable whole-Tool-call filesystem views, and
  isolation from non-cooperating writers. Content preconditions and atomic
  installation reduce conflicts but do not recreate those properties.
- The security claim becomes narrower and easier to audit: a closed structured
  method catalog plus native path/effect/network controls. A bug in those
  controls executes with the LATTICE process's host permissions; there is no VM
  defense-in-depth boundary. This accepted risk must remain explicit.
- Session History and project history remain independent. Explicit `/rewind`
  restores only recorded LATTICE mutations through private gix history; it is
  not an exact historical snapshot of every Local Workspace change.
- The architecture removes a leaf crate and multiple cross-layer lifecycle
  seams after migration, reducing production and test configuration surface.

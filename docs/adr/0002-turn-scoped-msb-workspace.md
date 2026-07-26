# ADR 0002: Turn-Scoped Microsandbox Workspace Mirrors Local State

- Status: Superseded by ADR 0003
- Date: 2026-07-16
- Supersedes: ADR 0001
- Canonical vocabulary: root `CONTEXT.md`

## Context

ADR 0001 introduced a LATTICE-owned CoW VCS, private gix object store,
persistent Host Layer, explicit Intake/Delivery, and stateless sandbox cache.
That model duplicated capabilities already present in Microsandbox and added a
second version-control system between the Agent and the user's project. The
desired result can instead be expressed by making the Local Workspace
authoritative, using one logical Microsandbox workspace per turn, and using gix
for structured project-repository operations and merge judgement. The logical
workspace semantics do not assume that Microsandbox 0.6.6 exposes a native CoW
projection of an arbitrary host project tree.

## Decision

1. **The Local Workspace remains the source of truth.** A watcher continuously
   observes external Local Workspace changes for the lifetime of a turn. The
   runtime does not create a private LATTICE VCS or make a user-visible
   Intake/Delivery cycle authoritative. The workspace adapter captures and
   synchronizes Local state through the generic reconciliation lifecycle.

2. **One turn owns one logical Turn Workspace.** It starts lazily when the
   first workspace-using Act needs it and is reused by every later Act in that
   turn. It presents the complete project view required by arbitrary Tool
   processes while keeping Agent writes isolated from Local state until the
   generic reconciliation lifecycle approves them. The adapter realizes this by
   materializing a complete Workspace Projection into `/workspace` in the
   Microsandbox OCI writable upper; it never mounts Local Workspace there.
   Turn closure either destroys the workspace or retains its final Act
   Checkpoint as a Turn Snapshot. A project commit may relate to multiple Act
   Checkpoints and Turn Snapshots.

3. **Every Act is isolated by a full cold savepoint.** After initial projection,
   the runtime stops and snapshots the OCI upper before the first Act. After a
   successful Act, its full Act Checkpoint becomes the next Act Savepoint. The
   Tool receives a stable Turn Workspace view; Local changes detected while it
   runs are queued rather than written underneath the Tool. The adapter may
   stop, start, or replace a physical sandbox at Act boundaries while preserving
   one opaque logical Turn Workspace identity for callers. All Tools use this
   lifecycle equally. Project Git Tools are structured peers of file or
   formatter Tools, not a special workspace path; their implementation may use
   gix, never an Agent-authored shell command.

4. **Workspace synchronization is event-driven within a turn and transition-
   based in durable history.** Local watcher facts, Tool completion, merge
   outcomes, savepoint restoration, persistence, and turn closure enter one
   ordered transient Workspace Event flow. Before an event advances state, its
   observed Local and Turn Workspace revisions are checked. The flow does not
   provide event-level crash recovery and is not persisted verbatim.

   At turn completion, the runtime embeds compact ordered Workspace
   Transitions in `TurnCommit`. A transition references the existing Tool call
   by ID and adds only workspace facts absent from the transcript: Act
   Checkpoint identity, project Git identities, Local revisions, merge outcome,
   and restoration or snapshot identity. The Session context owns the Workspace
   Event schema, pure reducer,
   Workspace Transition schema, and TurnCommit embedding. `lattice-api` drives
   asynchronous effects requested by the reducer, while `lattice-workspace`
   implements the MSB, savepoint, merge, and synchronization effects. No
   additional crate or pending durable event journal is introduced.

5. **gix merge defines conflict.** After a Tool returns, queued Local changes
   are reconciled using the repository merge engine, with the Act Savepoint
   state as base, the Tool result as ours, and current Local state as theirs.
   A clean merge updates the Turn Workspace without rollback and emits
   `WorkspaceChanged` before the Agent's next Think step. A merge conflict
   restores the Act Savepoint, synchronizes current Local state, discards that
   Act result, and emits `WorkspaceConflict`; the Agent must Think again and
   choose a new Act. The runtime never blindly reruns the old Tool call.

6. **Approved Act effects return to Local during the turn.** Each Act crosses
   the same generic reconciliation, audit, persistence, and Local writeback
   lifecycle. Local changes that reconcile cleanly are retained and reported
   to the Agent; they do not become effects of the Tool that happened to be
   running when they were observed. Tool-specific semantics stay inside the
   Tool interface and implementation.

7. **Projection is complete and policy-visible.** The adapter recursively
   streams the allowed Local tree through Microsandbox's structured filesystem
   operations before the first Tool runs. Its default set is tracked files plus
   non-ignored untracked files according to the project's Git ignore rules.
   `[lancet.workspace].included_projection_patterns` explicitly adds required
   ignored paths; `excluded_projection_patterns` removes paths from either set.
   Precedence is system hard exclusion, explicit exclusion, explicit inclusion,
   then ordinary Git ignore selection. An include can override Git ignore but
   never an explicit or system exclusion.

   These new fields are independent of `denied_write_patterns`: excluding a
   file from mutation does not imply that Tools or project processes must be
   unable to read it. Project `.git` administration and LATTICE private state are
   system exclusions and cannot be opted back into the sandbox. Sparse
   on-demand transfer is forbidden because an arbitrary process cannot
   distinguish a not-yet-materialized path from a real `ENOENT`.

   Projection uses fixed internal limits rather than adding resource knobs to
   the profile: at most 100,000 total entries, depth 64, 256 MiB for one regular
   file, and 2 GiB total apparent regular-file bytes. Scanning and limit checks
   finish before creating an MSB or transferring bytes. Explicit includes do not
   bypass limits. These values reserve roughly half of the default 4 GiB OCI
   upper for Agent writes and filesystem overhead.

   Projection accepts only safely representable UTF-8 directories, regular
   files, and symlinks that cannot escape the projected root. A non-UTF-8 name,
   escaping symlink, socket, FIFO, device, or another unsupported file kind
   fails the entire projection before the first Tool runs and returns a bounded
   diagnostic. It is never skipped with a warning, because that would recreate
   a false `ENOENT`.

8. **gix merge storage is transient.** The runtime creates a temporary bare gix
   repository for the turn's Merge Scratch. It explicitly writes every
   representable projection path into Base, Ours, and Theirs trees, so project
   ignore status does not remove a file from merge judgement. It calls
   `Repository::merge_trees` there and deletes the scratch repository when the
   turn closes. The runtime never writes merge objects or refs into the user's
   project `.git`, and Merge Scratch is not a durable checkpoint store. External
   merge drivers are outside the structured runtime contract; merge uses
   deterministic in-process gix behavior.

9. **LATTICE history uses a private per-project store.** Every successful Act
   produces an immutable full Microsandbox snapshot as its Act Checkpoint. The
   LATTICE History Store records checkpoint metadata, SDK snapshot identity, and
   reachability rather than requiring snapshot bytes to be rewritten as gix
   objects.
   Checkpoints form session-local lineage but never move project HEAD or enter
   user branch history. The same persistence boundary stores session
   TurnCommits and keeps referenced checkpoints reachable. This decision does
   not require that checkpoint bytes use gix objects rather than ordinary files,
   manifests, or Microsandbox snapshot artifacts. The user's project `.git`
   remains exclusively for Project Commits, refs, and index state produced by
   the user or explicit structured Git Tools; LATTICE does not add
   `refs/lattice/**` to it.

## Microsandbox 0.6.6 Capability Constraint

The pinned SDK does not directly provide the physical workspace assumed by the
logical model:

- An OCI rootfs has Microsandbox-managed read-only image layers and a writable
  `upper.ext4`. That CoW relationship applies to the OCI rootfs, not to an
  arbitrary host project directory mounted later.
- A directory bind mount is a virtiofs passthrough filesystem. A writable bind
  mutates host file contents directly; a read-only bind rejects writes. Stat
  virtualization protects metadata only and is not a content CoW layer. A bind
  rootfs likewise has no overlay.
- Snapshot creation requires a stopped OCI-rooted sandbox and captures only its
  `upper.ext4`; bind-mounted project contents are not part of the artifact.
  Restore copies that upper into a new sandbox rather than creating an online
  savepoint for the existing sandbox.
- Snapshot copy first attempts a filesystem reflink, but falls back to copying
  every allocated extent when reflinks are unavailable. CoW cost is therefore
  host-filesystem-dependent rather than guaranteed by the SDK contract.

Consequently, mounting Local Workspace through the current bind API cannot
simultaneously provide a complete project view, stable Tool execution, private
Agent writes, and Act rollback. The selected implementation therefore boots an
OCI-rooted sandbox, streams the complete Workspace Projection into its writable
upper through `SandboxFsOps`, and uses stopped-sandbox upper snapshots at Act
boundaries. Snapshot creation may be cheap on reflink-capable hosts but has an
allocated-extent copy fallback; this cost is part of the interface rather than
an assumed constant-time operation.

## CubeSandbox Alternative Evaluation

TencentCloud CubeSandbox was evaluated against upstream master commit
`e0beb3740cfc8bf8f09ff6fecb3e31dadef69b97` dated 2026-07-15. It has a stronger
live-VM snapshot primitive than Microsandbox 0.6.6: a running sandbox can be
snapshotted with its memory and rootfs, restored in place under the same sandbox
ID, or cloned into independent sandboxes. CubeCoW uses `FICLONE`, requires its
storage root to be on a reflink-capable filesystem, and rejects startup when
that capability is absent.

Those capabilities do not fit the selected Turn Workspace contract closely
enough to replace Microsandbox:

- A Cube snapshot restores memory, processes, and device state. LATTICE Act
  Savepoints and Checkpoints intentionally restore only filesystem state and
  cold-start Tool execution so background guest processes cannot cross an Act
  boundary.
- `CommitSandbox` reflink-clones the running rootfs before invoking
  `cube-runtime snapshot`; only the latter pauses the VM before capturing
  memory. The inspected path therefore has no single documented pause boundary
  covering both rootfs and memory. LATTICE would need an additional guest
  quiesce protocol before treating it as an Act consistency boundary.
- Cube host-directory and host-path mounts are passthrough dependencies, and
  `validateCommitSandboxTarget` rejects them for commit snapshots. Cube would
  still require a complete Workspace Projection upload into the internal
  rootfs; it cannot turn Local Workspace into a private snapshot by mounting it.
- The official Files API exposes read, write, list, stat, mkdir, move, remove,
  and watch. It exposes no symlink creation or mode/timestamp mutation, and its
  multi-file SDK helper performs one upload request per file. It therefore
  cannot faithfully or efficiently implement the accepted projection contract
  without a new in-guest transfer service.
- The public Go, Python, and Node command wrappers execute a shell string. The
  underlying envd Connect endpoint does carry typed `cmd`, `args`, `envs`, and
  `cwd`, so a custom Rust client could avoid shell interpretation, but Cube has
  no official Rust SDK and LATTICE would own the control-plane, Connect framing,
  CubeProxy routing, token, TLS, and compatibility surface.
- The supported single-node deployment is a root-managed service stack rather
  than an embeddable workspace adapter. It installs CubeAPI, CubeMaster,
  Cubelet, CubeShim, the hypervisor, network-agent, CubeProxy/CoreDNS, MySQL,
  Redis, and Docker-managed services; it also requires KVM, systemd networking,
  and a dedicated XFS-backed `/data/cubelet` in the one-click path. That is a
  materially larger operational boundary than the pinned Microsandbox SDK and
  daemon already used by LATTICE.

CubeSandbox is therefore rejected as the production Turn Workspace backend for
this ADR. Its live snapshot speed does not offset the cold-restore semantic
mismatch, incomplete structured projection surface, custom Rust protocol
ownership, and deployment cost. Reconsideration requires, at minimum, a stable
Rust client surface, faithful bounded tree import including symlinks and
metadata, and a quiesced filesystem-only checkpoint or a documented atomic
rootfs-plus-memory boundary that can be restored without reviving Tool
processes.

## Act Boundary Sequence

1. Materialize the initial Workspace Projection through structured filesystem
   operations, stop the sandbox, capture the initial Act Savepoint, and start it
   for the first Tool.
2. While the Tool runs, queue watcher facts without changing its projected
   files. After it returns, inspect and export the resulting workspace state
   through structured filesystem operations.
3. Reconcile Base, Ours, and current Local state on the Host with gix. A clean
   result is written to Local and synchronized into the still-live Turn
   Workspace before checkpointing.
4. For a clean result, stop the sandbox, capture the full Act Checkpoint, and
   restart it for the next Act. That checkpoint is the next Act Savepoint.
5. For a conflict, stop and discard the current physical sandbox, restore the
   previous Act Savepoint into a replacement sandbox, synchronize current Local
   state, and emit `WorkspaceConflict`. The opaque logical Turn Workspace
   identity does not change.

The capable-host probe
`test_microsandbox_structured_projection_checkpoint_and_rollback` verifies this
physical foundation against pinned Microsandbox 0.6.6: it streams two files and
a symlink through `SandboxFsOps`, restores the initial savepoint, writes an Act
change, restores the resulting checkpoint, and separately restores the old
savepoint as rollback. It passed on 2026-07-16 in 8.77 seconds on the local
btrfs/KVM/libkrun host and removed all probe sandboxes and snapshots afterward.
On the same checkout, a projection that only excluded `.git` and LATTICE state
would contain 121.36 GB across 197,429 files because ignored `target/` alone was
119.49 GB. Applying Git ignore selection reduced the candidate projection to
approximately 5.23 MB across 402 files. This measurement is why ignored files
require explicit inclusion instead of default materialization.

## Module Seams

- `lattice-session` owns plain Workspace Event, reducer state, reducer actions,
  Workspace Transition, and TurnCommit embedding. Its reducer performs no I/O.
- A turn-scoped coordinator in `lattice-api` wraps `CepExecutorAdapter` around
  every Tool call. It uses catalog `ImpactKind` to start the Turn Workspace only
  for the first workspace observation or mutation, but once started it processes
  queued Local changes after every Act. It owns the watcher, drives reducer
  actions, and collects transitions for commit.
- `lattice-workspace::Workspace` remains the adapter seam with deterministic and
  Microsandbox adapters. Its lifecycle interface allocates an opaque logical
  turn handle, begins an Act, completes or aborts it, and closes the turn. The
  implementation hides materialization, physical sandbox replacement,
  stop/snapshot/start, transient gix objects, and Local synchronization from
  callers. Existing typed filesystem operations continue through the same
  logical handle.
- `lancet-native` handlers continue to know only the typed execution backend.
  They do not manage snapshots, watchers, gix repositories, or checkpoint
  history.

The old `Workspace` interface is intentionally not preserved as compatibility
surface. `baseline_oid`, guest `git_status`, candidate bundle export,
guest-repository template assumptions, and promotion operations are artifacts
of the superseded guest-Git model and must not leak into the replacement seam.

## Migration Sequence

1. Extend `lattice-session` with plain Workspace Event, reducer, action, and
   Workspace Transition types, then add ordered transitions to TurnCommit with
   deserialization defaults for already-persisted commits. This step has no
   workspace I/O.
2. Add a bounded Workspace Projection scanner and transient gix Merge Scratch
   implementation inside `lattice-workspace`. Prove tracked, untracked, ignored,
   deletion, rename, executable, symlink, clean merge, and conflict cases using
   only the deterministic test lane. Add and strictly validate
   `included_projection_patterns` and `excluded_projection_patterns` in
   `lattice-profile`, lower them through `lattice-api`, and keep both
   semantically separate from write denial.
3. Atomically replace the public `Workspace` lifecycle interface and all test
   adapters. Introduce logical turn handles and Act boundary results; remove
   guest-Git fields and methods instead of retaining compatibility shims.
4. Add the `lattice-api` turn coordinator and wrap `CepExecutorAdapter` around
   Tool dispatch. Prove no MSB is created for turns without workspace methods,
   first workspace use creates exactly one logical workspace, failed Tools roll
   back, conflicts become structured requires-replan Tool results, and ordered
   transitions reach TurnCommit.
5. Replace the Microsandbox adapter's pack/base64/guest-Git path with structured
   projection streaming and logical-handle state. Reuse the passing capable-host
   probe while adding Local-change reconciliation and repeated Act checkpoint
   coverage.
6. Remove obsolete promotion, candidate bundle, guest status, git-ready helper,
   runtime package installation, shell/coreutils transfer, and corresponding
   tests only after the replacement path owns their behavior.
7. Move session graph refs out of the project repository in a separate
   persisted-data migration. Select the private per-project History Store locator
   before this step; do not mix storage relocation with the workspace adapter
   cutover.

Each step must leave the workspace compiling. Deterministic gates precede the
KVM lane; the final capable-host gate must include the exact structured
projection/checkpoint/rollback test plus searches proving production
`lattice-workspace` contains no guest Git, shell, package-manager, base64, `dd`,
or coreutils transfer path.

## Rejected Alternatives

- A private LATTICE Workspace VCS with Host-persistent Layers, Freeze, Intake,
  Delivery, and an MSB cache.
- A writable bind mount of Local Workspace, because Tool writes would mutate
  Local before reconciliation and concurrent Local writes would change the
  Tool's view.
- Sparse on-demand file materialization, because genuine absence and a cache
  miss both surface as `ENOENT` to arbitrary processes.
- A private host-directory bind as the selected implementation, because it
  would require LATTICE to build a second savepoint implementation despite the
  OCI upper snapshot support already available.
- TencentCloud CubeSandbox as the selected backend: its live full-VM snapshots
  restore process and memory state, its official filesystem API cannot express
  the full projection contract, host mounts are excluded from commit snapshots,
  and its deployment boundary is substantially larger than Microsandbox.
- Treating the Turn Workspace as a disposable projection rebuilt after every
  Act.
- Updating MSB only once at the end of an Act or turn instead of continuously
  observing Local changes and reconciling them at stable Act boundaries.
- Rerunning the same structured Tool call automatically after its assumptions
  were invalidated by a Local change.
- Giving project Git Tools a separate orchestration path from other Tools.
- Introducing a separate crate for Workspace Event schema, reducer, and
  history instead of extending the Session context that owns turn history.
- Persisting every transient Workspace Event or maintaining a pending event
  journal for exact mid-turn crash recovery.

## Consequences

- Root `CONTEXT.md` retires the ADR 0001 vocabulary (`Workspace VCS`, `State`,
  `Layer`, `Freeze`, `Pointer`, `Delivery`, `Intake`, and `Git Bridge`) in favor
  of Local Workspace, Turn Workspace, Act, Act Savepoint, Act Checkpoint,
  Workspace Event, Workspace Changed, and Workspace Conflict.
- The existing guest-Git promotion path remains scheduled for hard removal,
  but its replacement is this turn-scoped event model rather than a private
  LATTICE VCS.
- Embedded Workspace Transitions provide the completed-turn coordinates needed
  by a future `/rewind` operation. Rewind protocol, cross-system transaction
  mechanics, and UI are outside this ADR; the fixed safety constraint is that
  Rewind rejects a dirty Local Workspace rather than overwriting it.
- `lattice-session` currently persists one aggregate `TurnCommitInput` after a
  turn; it has no per-Act Workspace Event reducer or append-only event log.
  `AgentTurnEvent`, public `TurnEvent`, and CEP `ToolEvent` are transient flows
  owned by other crates. It must gain internal workspace-event reducer and
  workspace-transition modules while preserving the distinction from those
  transient presentation flows.
- `GitSessionGraphStore` currently opens the user's project object database and
  writes `refs/lattice/**`. Session commits and refs must migrate to the private
  LATTICE History Store alongside Act Checkpoints; project Git access remains a
  separate structured Tool concern.
- Issues #28/#29 still require rewriting because the Turn Workspace no longer
  needs a guest Git repository or shell/coreutils transfer protocol.

## Open Questions

- How are Act Checkpoints associated with Project Commits and Turn Snapshots
  across project branch switches and rollback?

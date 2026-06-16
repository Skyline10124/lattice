# Cube Upper Workspace and Git Session Backend Design

> **⚠️ Superseded in part (2026-06-14): the execution adapter changed.**
> This document's concrete execution backend — **CubeSandbox** (`CubeWorkspace`,
> CubeCoW, CubeVS, the E2B-compatible service, `lattice doctor` Cube detection) —
> was **not** adopted. The production Upper adapter is **microsandbox (libkrun)**
> via the `microsandbox` Rust SDK, with an in-process **deterministic** test
> adapter for CI; the `CubeWorkspace` adapter has been **removed from the
> codebase** (Phase 33 code-review cleanup). The **architecture** described below
> still holds — the Upper/Lower split, baseline-commit Upper provisioning, Git
> promotion to a clean Lower, Profile-configured egress, credential-by-reference,
> and the Git session backend — so read every "Cube\*" reference as "the Upper
> execution adapter" (now microsandbox). Notable deltas: libkrun snapshots are
> **disk-only** (no live-memory/CubeCoW resume), and egress is enforced via the
> SDK's `NetworkPolicy`, not CubeVS/eBPF. Current state: `.planning/ROADMAP.md`
> (Phase 33) and the `lattice-workspace` crate.

Date: 2026-06-13

Status: Accepted in design discussion; revised 2026-06-13 after written-spec
review (CubeSandbox platform facts, managed Lower default, promotion seam,
`pending_promotion` disposition, Profile-configured egress policy,
snapshot-bounded verification).

## 1. Summary

LATTICE will execute every model-visible workspace operation inside an isolated
CubeSandbox MicroVM called the Upper workspace. The Lower workspace is a clean
host Git worktree owned by LATTICE — by default a managed worktree dedicated to
the target branch, optionally the user's own checkout. Models and tool commands
never access Lower directly.

At the end of a turn, LATTICE accepts only an Upper Git repository whose index
and non-ignored working tree are clean. The host freezes Upper, preserves a Cube
runtime snapshot when needed, imports the exact Git objects created by the
agent, reviews the complete candidate commit range, verifies it, and promotes
it to Lower only after a baseline compare-and-swap check.

The existing CEP envelope concept is deleted completely. CEP policy, Guard,
effect checking, verification, and audit semantics remain, but authorization is
derived directly from immutable host configuration and per-call facts.

The session graph moves from SQLite to Git. Project history and session history
use separate Git DAGs and refs in the same object database. A promoted turn can
therefore update the project ref and session ref in one Git ref transaction
without combining their distinct semantics.

## 2. Goals

1. Give the model broad freedom inside an isolated, persistent execution
   environment.
2. Make Lower writable only through one host-owned promotion module.
3. Preserve CEP's per-call policy, Guard, effect, verification, audit, and
   provenance semantics.
4. Remove all envelope protocol, lifecycle, and mutable authority state.
5. Preserve agent-created Git commits, authors, committers, topology, and OIDs.
6. Use Git directly as the session graph backend.
7. Keep Session, Agent, and Profile independently replaceable.
8. Allow multiple sessions and agents to execute concurrently against their own
   baselines.
9. Keep v1 small: Host CEP plus Cube process/files operations, with no
   persistent LANCET process inside the VM.

## 3. Non-Goals

1. Rolling back network or other remote side effects.
2. Automatically rebasing a candidate after Lower changes.
3. Inferring which dirty changes are relevant on the agent's behalf.
4. Automatically committing or rewriting agent changes.
5. Copying an Upper `.git` directory into Lower.
6. Brokering credentials or preventing a model from reading explicitly exposed
   credentials.
7. Providing a non-Git Lower workspace in v1.
8. Providing a non-Cube production execution adapter in v1.
9. Making a multi-file host working-tree update physically atomic to arbitrary
   external processes. Git refs are atomic; working-tree materialization is
   journaled and crash-recoverable.
10. Supporting non-Linux, non-x86_64, or non-KVM hosts with the production
    Cube adapter.
11. Treating in-VM verification as a security boundary.

## 4. Terminology

### Upper

The CubeSandbox MicroVM and its isolated `/workspace` Git repository. All
model-visible observation, mutation, command execution, and Git operations
happen here.

### Lower

A clean host Git worktree associated with a target project ref. By default
LATTICE provisions and owns a managed worktree per target branch, written only
by promotion; using the user's own checkout as Lower is an explicit opt-in.
Models and tool commands never receive its path and cannot write it.

### Candidate

The clean Upper `HEAD` and the commit range from the recorded Lower baseline to
that `HEAD`.

### Promotion

The host-owned operation that validates a candidate, rechecks the Lower
baseline, imports Git objects, updates refs, and materializes the accepted tree
in Lower.

### Session Commit

A Git commit in the Session DAG. It records one immutable turn: messages,
execution facts, Agent and Profile snapshots, workspace refs, audit refs, and
the terminal disposition.

### Project Commit

A Git commit created in Upper that belongs to the user's project history.
Project commits and Session commits are different objects in different DAGs.

## 5. Trust Model

The model controls commands and files inside Upper. It may modify any file in
`/workspace`, use Git freely, access the network, and read credentials that the
user explicitly exposes.

The host trusts neither shell behavior nor the final state reported by a
command. The host owns:

- Cube lifecycle calls;
- CEP request and result recording;
- runtime snapshot creation;
- Git object import and validation;
- candidate diff calculation;
- promotion policy and final effect checks;
- Lower cleanliness checks;
- baseline compare-and-swap;
- project and session ref updates;
- Lower working-tree materialization.

CubeSandbox and its process/files interface are infrastructure trusted by v1.
There is no additional persistent `lancet-worker` inside the VM. The
CubeSandbox service daemon runs privileged on the host and belongs to the
trusted computing base together with the KVM stack beneath it.

Verification executes inside the model-controlled Upper. A hostile agent can
tamper with the toolchain that verification invokes, so the verification
result is advisory evidence, not a security boundary. Promotion-time policy
and effect checks on the candidate range remain the authoritative gate.

Promotion is the only local path by which Upper content reaches Lower. A
remote round trip — content pushed from Upper and later pulled by the user —
bypasses promotion review entirely. The Profile egress policy is the mechanism
that narrows or closes this channel; under `open` egress the layer guarantee
is local-path-only, and LATTICE documents it as such.

CEP tool-time checks remain useful for early rejection, deterministic policy
semantics, and complete audit evidence. The final candidate and promotion facts
remain authoritative for what reaches Lower.

## 6. Architecture

```text
ProfileResolver       GitSessionGraphStore
       |                       |
       v                       v
                  TurnOrchestrator
                 /       |        \
                v        v         v
          AgentLoop   Host CEP   CubeWorkspace
                         |            |
                         +-----> Upper /workspace
                                      |
                                      v
                               clean candidate
                                      |
                                      v
                                  Promotion
                                      |
                                      v
                                    Lower
```

### 6.1 TurnOrchestrator

`TurnOrchestrator` is the deep module behind the existing
`lattice-api::stream_turn` execution path. It is not a new security layer and
does not replace `stream_turn` as the public live execution interface.

Its interface hides these ordering invariants:

1. resolve Session, Agent, and Profile snapshots;
2. require a clean Lower baseline;
3. create or restore Upper;
4. run AgentLoop with Host CEP;
5. enforce clean completion;
6. freeze, export, and run snapshot-bounded verification;
7. request promotion;
8. write exactly one terminal Session commit for the turn;
9. release or retain Cube resources according to disposition.

Deleting this module would spread lifecycle ordering and recovery logic across
the public turn path, CEP setup, session persistence, and workspace code. It
therefore earns depth and locality.

### 6.2 CubeWorkspace

`CubeWorkspace` is the module for Cube lifecycle and workspace operations. Its
interface covers:

- create an Upper repository from a baseline project commit;
- restore a Cube runtime snapshot;
- execute processes and perform file operations for CEP adapters;
- query Git status and `HEAD`;
- freeze and snapshot the VM;
- export validated Git objects and repository facts;
- destroy or retain the sandbox.

The production adapter uses CubeSandbox, TencentCloud's open-source sandbox
service (github.com/TencentCloud/CubeSandbox, Apache-2.0) built on RustVMM and
KVM. A deterministic test adapter provides the second adapter at this seam.

Facts the production adapter depends on:

- CubeSandbox runs only on x86_64 Linux hosts with KVM; the production adapter
  is unavailable elsewhere — an accepted v1 constraint;
- it is a separately deployed HTTP service with an OpenAPI description and an
  E2B-compatible surface, not an embedded library; `lattice doctor` detects
  service availability, and an unreachable service follows the Cube creation
  failure path;
- the official SDK is Python-only; `lattice-workspace` owns a Rust client
  written against the pinned OpenAPI description;
- snapshot, in-place rollback, clone, snapshot deletion, and
  snapshot-as-template creation are CubeCoW operations (service v0.3.0 and
  later); LATTICE pins the service version it certifies against;
- CubeVS provides eBPF-based egress filtering, used to enforce the Profile
  network policy at Upper creation.

Freeze maps to a CubeCoW consistency point: the adapter quiesces the VM, takes
a snapshot, and exports candidate Git objects from that snapshotted state (for
example via `git pack-objects` through the process interface), so exported
objects always correspond to the frozen state. The same snapshot is the
verification discard boundary: verification runs on the live VM after the
snapshot, so its writes can never reach the exported candidate or a retained
snapshot.

Upper must:

- use the same Git object format as Lower;
- start from the exact `baseline_oid`;
- have no writable host mount;
- never receive the Lower absolute path;
- keep its own `.git` directory;
- use `/workspace` as the model-visible project root.

#### Upper Provisioning and Build Caches

Ignored files are never copied from Lower, so a bare Upper would rebuild
dependency caches on every turn. The adapter therefore maintains a per-project
warm template:

- the template snapshot contains the toolchain and prebuilt dependency caches
  (such as `target/`), never credentials;
- each turn clones from the warm snapshot, then the host resets `/workspace`
  to the exact `baseline_oid`, transferring any missing Git objects;
- template rebuilds are policy-driven: a manual command plus staleness signals
  such as lockfile changes;
- a stale template costs build time only; candidate correctness never depends
  on template state because ancestry and object validation run against
  `baseline_oid` regardless.

### 6.3 Host CEP

CEP dispatch, policy, audit persistence, and result mapping remain on the host.
CEP methods execute through the Cube process/files adapter.

For operations that require local POSIX behavior not available through a direct
Cube operation, the host may invoke a one-shot `lancet-runner` inside Upper.
The runner exits after the operation and holds no persistent authority or audit
state.

`observe.audit` and `observe.audit_replay` remain host operations.

### 6.4 Promotion

Promotion is the only module allowed to write Lower. It has two phases:

**Prepare, outside the Lower lock**

- validate imported Git objects with fsck-level checks, connectivity
  verification, and object size limits;
- surface tree-entry hazards in review facts: `.gitmodules` changes, symlink
  additions or retargets, and any tree entry named `.git`;
- require `baseline_oid` to be an ancestor of `candidate_oid`;
- calculate and review the complete `baseline_oid..candidate_oid` range;
- run final policy and effect checks;
- consume the recorded verification result, requiring its bound OID to equal
  `candidate_oid`; a mismatch requires re-verification, never reuse;
- construct immutable Session payload inputs except the final disposition;
- prepare a recovery journal.

**Commit, inside the Lower lock**

- require Lower to remain clean;
- compare the target project ref with `baseline_oid`;
- create the promoted Session commit object after the compare-and-swap succeeds;
- atomically update the project ref and Session ref with a Git ref
  transaction;
- retain the candidate ref;
- materialize the accepted project tree in Lower;
- complete or recover the journal.

The lock is scoped to the target Lower worktree and project ref. Long-running
verification never holds it.

### 6.5 GitSessionGraphStore

`GitSessionGraphStore` replaces the production SQLite session graph backend. It
implements the session graph interface using Git commits, parents, trees, and
refs. The Session commit manifest is a superset of today's `TurnCommitInput`,
so this extends the existing `SessionGraphStore` contract with workspace and
disposition fields rather than implementing it unchanged.

For promotion, the store separates object preparation from ref movement: it
constructs a Session commit object and returns its OID without updating any
ref, so a caller can move the Session ref inside a wider Git ref transaction.

It owns conversation topology only. It does not resolve Profiles, construct
Agents, operate Cube, execute CEP, or promote workspaces.

## 7. Session, Agent, and Profile Decoupling

### 7.1 Profile

Profile remains a declarative input owned by `lattice-profile`. It contains:

- model and provider configuration;
- system prompt;
- tool exposure;
- CEP policy;
- verification command;
- Cube settings;
- Upper egress network policy (`open`, `allowlist`, or `blocked`);
- Git author and committer identity;
- secret references and exposed injection targets.

Profile loading never accesses Session state or executes an Agent.

At turn start, the resolved Profile is canonicalized and stored as an immutable
Git blob in the Session commit tree. Secret values are never included; only
references and non-secret configuration are snapshotted.

### 7.2 Agent

Agent remains a stateless execution module owned by `lattice-agent`. An Agent
revision identifies the selected Agent behavior and resolved Profile snapshot.
It does not own transcripts, session refs, Cube snapshots, or workspace state.

The resolved Agent revision is also stored as an immutable blob in the Session
commit tree.

### 7.3 Session

Session owns history and references. It treats Agent and Profile snapshots as
opaque immutable blobs. It does not know their source path, discovery rules, or
runtime construction.

Changing the active Agent affects only subsequent turns. Historical Session
commits continue to reference the exact Agent and Profile snapshots used at
execution time.

### 7.4 Composition

Only `lattice-api` composes the three modules:

```text
SessionStore -> transcript and selected Agent name
ProfileResolver -> resolved Profile snapshot
Agent construction -> AgentTurnRequest
AgentLoop -> messages and tool calls
SessionStore -> immutable terminal turn
```

Dependency rules remain:

- `lattice-session` does not depend on `lattice-agent` or `lattice-profile`;
- `lattice-profile` does not depend on Session or Agent runtime code;
- `lattice-agent` does not access Session storage;
- `lattice-api` is the composition layer.

## 8. Git Storage Model

### 8.1 Ref Layout

```text
refs/heads/<branch>
refs/lattice/sessions/<session-id>/<session-branch>
refs/lattice/candidates/<session-id>/<turn-id>
```

Project refs and Lattice refs share one Git object database. Normal project
commands and ordinary `git log` on a project branch do not traverse the Session
DAG.

LATTICE does not add a remote push refspec for `refs/lattice/**`. Session data
is local by default, but it remains readable to any principal with access to
the local Git object database.

Sharing one object database is a deliberate trade-off: it is what allows one
Git ref transaction to span the project ref and the Session ref. The cost is
that session history physically lives in the user's repository, and commands
such as `git push --mirror` or an explicit `refs/lattice/*` push publish it to
a remote. LATTICE documents this hazard, and `lattice doctor` warns when a
configured remote already contains `refs/lattice/**`.

Sessions with no target project repository use a standalone bare repository
under the LATTICE data directory with the same ref layout and commit schema;
only promotion does not apply.

### 8.2 Session Commit Tree

Each Session commit stores per-turn data:

```text
turn/
  manifest.json
  messages.jsonl
  agent.json
  profile.json
  audit.json
```

`manifest.json` includes:

- schema version;
- session ID and turn ID;
- parent Session commit OIDs;
- execution status;
- terminal disposition;
- model and provider;
- baseline project OID;
- candidate project OID, when clean;
- candidate ref;
- target project ref;
- Cube snapshot reference, when retained;
- audit and attestation digests;
- verification result;
- `resumes_turn_oid`, when applicable;
- timestamps and usage.

Messages are stored per turn. Transcript reconstruction walks the reachable
Session DAG in the same way the current graph store reconstructs turn history.

Agent and Profile blobs are embedded in the Session commit tree rather than
given long-lived refs. Git naturally deduplicates identical snapshots by OID.

Cube runtime snapshots and audit databases are not stored in Git. Session
commits store references and integrity digests.

### 8.3 Candidate Reachability

Every clean candidate receives a Lattice candidate ref so its commit range
remains reachable independently of later project branch rewrites.

The candidate ref points to the exact Upper `HEAD`. Promotion never rewrites
the candidate commit.

Candidate refs accumulate one per turn. v1 ships a manual pruning command that
deletes candidate refs and retained Cube snapshots for turns the user
discards; automatic garbage collection stays excluded.

### 8.4 Upper Git Semantics

The Agent may use:

- `git status`, `diff`, `add`, `commit`, `restore`, `checkout`, and branches;
- multiple checkpoint commits in one turn;
- networked Git commands, including fetch and push, subject to their external
  side effects.

Upper `.git/**` is retained by Cube snapshots but is never copied through file
promotion. Host import accepts Git objects and the selected candidate OID only.
It does not import Upper hooks, config, index, reflogs, replace refs, or arbitrary
refs.

The candidate must be a descendant of the recorded baseline. Promotion is a
fast-forward of the target project ref to the exact candidate OID.

The Host preserves every Agent-created commit's:

- tree;
- parents and topology;
- author;
- committer;
- message;
- signature bytes;
- OID.

Signatures are preserved but are not automatically trusted.

## 9. Lower and Upper Cleanliness

### 9.1 Lower Cleanliness

By default the target Lower is a LATTICE-managed worktree created from the
target branch and written only by promotion, so it is clean by construction
and the user's own checkout is never touched. Pointing Lower at the user's
checkout is an explicit opt-in and grants no additional promotion rights.

Before starting a turn and again inside promotion, the target Lower worktree
must satisfy:

- `HEAD` is attached to the target branch;
- index equals `HEAD`;
- tracked working-tree files equal `HEAD`;
- no non-ignored untracked files exist.

Ignored files do not belong to the baseline and are not copied into Upper.
Promotion leaves Lower ignored files untouched.

If Lower is dirty, LATTICE refuses to start or promote. It never stashes,
commits, resets, or deletes the user's host changes.

### 9.2 Upper Cleanliness

An Agent final response is provisional until Upper is clean.

The Host checks Git porcelain status. If Upper has staged, unstaged, or
non-ignored untracked changes, the Host appends a structured
`workspace_dirty` fact to the same in-memory turn and resumes AgentLoop. The
fact lists changed paths and states but does not decide which changes are
relevant.

The Agent must:

- commit valid changes itself;
- restore or remove unrelated changes itself;
- leave index and non-ignored working tree clean.

The Host never auto-commits and never auto-discards.

If the configured cleanup round limit is exhausted, the turn ends as
`pending_worktree` and retains the Cube snapshot.

Verification runs only after a clean check, in a discard layer rebuilt from
CubeCoW: the Host first records the clean `HEAD` as `candidate_oid` and takes
the consistency snapshot, then runs verification on the live VM. The recorded
verification result is bound to `candidate_oid`. Verification writes exist
only in the post-snapshot VM state — they are never exported, never part of a
retained snapshot, and never reach the candidate. No second cleanliness check
is needed because export reads the pre-verify snapshot.

## 10. Turn Lifecycle

### 10.1 New Turn

1. Open the Session ref and reconstruct the transcript.
2. Resolve and snapshot the selected Agent and Profile.
3. Require Lower clean and record `baseline_oid`.
4. Create Upper from the exact baseline.
5. Inject configured credentials and Git identity.
6. Run AgentLoop through Host CEP.
7. Treat final output as provisional until Upper is clean.
8. Record the clean `HEAD` as `candidate_oid` and freeze Upper by taking the
   CubeCoW consistency snapshot.
9. Export and validate the candidate Git objects from the snapshotted state.
10. Run profile-defined verification on the live VM under Host orchestration;
    bind the recorded result to `candidate_oid`. The snapshot boundary
    discards verification writes.
11. Run promotion prepare checks.
12. Acquire the Lower promotion lock and recheck cleanliness and baseline.
13. Commit the terminal disposition; retain or release the snapshot according
    to that disposition.

### 10.2 Resume Turn

A resume is a new turn and creates a new immutable Session commit. It records
`resumes_turn_oid` pointing to the prior pending turn.

- `pending_worktree`: restore the runtime snapshot and continue cleanup.
- `pending_verification`: restore the pre-verify snapshot and let the Agent
  create new fixing commits; a new clean `HEAD` yields a new `candidate_oid`
  and requires fresh verification.
- `pending_promotion`: the candidate stays valid and the baseline unchanged;
  retry promotion only. No Upper restore is needed because the candidate Git
  objects are already exported and reachable.
- `pending_conflict`: never combine the old snapshot with a new baseline.
  The user must abandon it or explicitly start a new Upper from current Lower.

The old Session commit never changes.

### 10.3 No-Op Turn

If Upper is clean and `candidate_oid == baseline_oid`, promotion is a no-op for
the project ref. The Session turn still commits its messages and audit facts.

## 11. Status Model

Agent execution status and workspace disposition are separate fields.

Execution status:

- `completed`;
- `error`;
- `cancelled`.

Terminal workspace disposition:

- `promoted`;
- `pending_worktree`;
- `pending_verification`;
- `pending_promotion`;
- `pending_conflict`;
- `rejected`;
- `aborted`.

`running` is an in-memory lifecycle state and is never stored as a terminal
Session commit.

Every terminal turn, including errors and cancellation, creates one immutable
Session commit.

Resource retention:

- `pending_worktree`, `pending_verification`, `pending_conflict`: retain Cube
  snapshot;
- `pending_promotion`: release the snapshot; the clean candidate is fully
  represented by its exported Git objects;
- `promoted`, `rejected`, `aborted`: release Cube snapshot;
- clean candidate Git objects remain reachable through their candidate ref;
- a lost runtime snapshot can be rebuilt from a clean candidate, without
  process or memory state;
- a lost dirty snapshot cannot recover uncommitted work.

## 12. Promotion and Concurrency

### 12.1 Parallel Execution

Sessions, Agents, Cube VMs, and audit streams execute independently in
parallel. Each turn records its own baseline.

Promotion serialization is scoped to the target Lower worktree and project
ref. Separate managed Git worktrees may promote separate branches in parallel.
Sessions targeting the same Lower worktree serialize.

### 12.2 Conflict

If the target project ref no longer equals `baseline_oid`, the turn becomes
`pending_conflict`. If the baseline still matches but promotion is temporarily
blocked — Lower dirty in the opt-in checkout mode, or recovery incomplete —
the turn becomes `pending_promotion` and only promotion is retried.

LATTICE does not:

- auto-rebase;
- auto-merge;
- silently rebuild Upper on the new baseline;
- partially promote non-conflicting paths.

### 12.3 Ref Transaction

For a successful promotion, the Host creates all required objects before
opening a Git ref transaction, except the Session commit whose final disposition
depends on the lock-time compare-and-swap. Inside the lock:

1. verifies the target project ref still equals `baseline_oid`;
2. creates the promoted Session commit object;
3. updates the project ref to `candidate_oid`;
4. updates the Session branch to the promoted Session commit;
5. creates or retains the candidate ref.

If the compare-and-swap fails, no promoted Session commit is created. The Host
creates one `pending_conflict` Session commit after leaving the project ref
transaction. For other pending or rejected turns, only Lattice refs move.

The workspace module executes the ref transaction but never builds Session
commits itself. Promotion's commit phase takes a caller-supplied preparation
step: after the compare-and-swap succeeds inside the lock, it invokes that
step to obtain the promoted Session commit OID from `lattice-session` (object
creation only, no ref movement), then updates both refs in one transaction.
`lattice-api` wires the step, keeping `lattice-session` and `lattice-workspace`
independent of each other.

### 12.4 Working-Tree Materialization

Git ref updates are atomic. Updating multiple files in an existing host
worktree is not physically atomic to arbitrary external readers.

Promotion therefore uses:

- an exclusive Lower worktree lock for cooperating LATTICE operations;
- a durable journal containing old and new refs and materialization state;
- deterministic recovery to the project ref's tree after interruption,
  rewriting only paths the journal records as in flight; a path whose current
  content matches neither the old nor the new tree is surfaced, never
  overwritten;
- refusal to start another turn while recovery is incomplete.

A promotion is considered committed only when the ref transaction succeeds.
If materialization fails afterward, the repository enters recovery mode rather
than reporting a partial successful turn.

## 13. CEP Without Envelopes

### 13.1 Complete Deletion

The following are removed:

- `session.envelope`;
- envelope request and result wire types;
- declare and supersede operations;
- active envelope stores;
- envelope IDs and lifecycle;
- `BaseExecutionEnvelope`;
- `BaseAuthorityPlan`;
- mutation requirements for an active envelope;
- envelope-specific audit stages and UI text.

The production catalog contains nine methods after deletion.

### 13.2 Direct Policy Evaluation

Tool exposure remains separate from authority.

For each CEP call, Host policy evaluates:

- immutable resolved Profile policy;
- catalog method requirements;
- method parameters;
- Upper workspace facts;
- configured budgets;
- observed prior effects for the turn.

There is no declaration, scope negotiation, grant object, mutable capability
set, or model-addressable authority state.

The internal turn execution context contains facts only:

- turn ID;
- session ID;
- Upper workspace ID;
- baseline OID;
- target project ref;
- Agent snapshot OID;
- Profile snapshot OID;
- audit run ID;
- start time.

This context is not a CEP method and is not an authority object.

### 13.3 Guard and Effect Semantics

Guard and effect stages remain present in CEP audit.

Tool-time checks operate on the Upper namespace and provide:

- early rejection;
- normalized path and command facts;
- deterministic policy reasons;
- effect ledger evidence;
- verification evidence.

Promotion independently evaluates the candidate commit range before any Lower
write. Final diff review does not replace tool-time audit because a final diff
cannot represent:

- denied calls;
- command execution;
- web access;
- intermediate writes that cancel out;
- operation ordering;
- retries and errors.

## 14. Audit and Attestation

Host CEP records:

- request receipt;
- policy decision;
- Guard result;
- dispatch start;
- Cube operation result or error;
- declared and observed tool-time effects;
- verification stages.

The promotion module records:

- baseline and candidate OIDs;
- candidate commit range;
- the verification result's OID binding check;
- final diff and effect verdict;
- Lower cleanliness checks;
- baseline compare-and-swap result;
- ref transaction result;
- working-tree materialization and recovery state.

The turn attestation references both streams. Tool-time events remain the
execution narrative; promotion events are authoritative for Lower changes.

The audit database remains outside Upper and outside the promoted workspace.
Audit, logs, tool results, and Session payloads pass through one host redaction
module before persistence or display.

## 15. Network and Credentials

Upper egress is governed by a Profile-configured network policy with three
modes:

- `open`: unrestricted egress — the default;
- `allowlist`: only configured destinations (domains and CIDRs) are reachable;
- `blocked`: no egress.

Enforcement uses CubeSandbox's eBPF egress filtering (CubeVS), applied by the
host at Upper creation. The policy is part of the immutable Profile snapshot
and is not model-addressable; LATTICE does not implement its own filter, and
domain-rule resolution follows CubeVS semantics.

Under `open` or a permissive allowlist, a remote round trip — `git push` from
Upper, later pulled by the user — bypasses promotion review. `allowlist` and
`blocked` exist to close this channel for users who need the full layer
guarantee.

Remote writes such as `git push`, HTTP mutations, database changes, messages,
and package publication are outside the workspace transaction. Cube rollback
and Git promotion cannot undo them. LATTICE records CEP-visible operations but
does not claim complete syscall or network-effect observation for arbitrary
shell commands; the egress policy bounds where traffic can go, not what it
means.

Host credentials are not inherited.

Users may configure exposed credential injection through secret references.
Supported v1 targets are environment variables and files. Profile and Session
snapshots store only the reference and target, never the resolved value.

Exposed credentials are readable by model-controlled commands and may remain
in VM memory or Cube snapshots. Redaction prevents normal persistence in
LATTICE records but cannot sanitize arbitrary VM memory. This is an accepted v1
risk, not a security guarantee.

## 16. Crate Ownership

### `lattice-api`

- owns `TurnOrchestrator`;
- keeps `stream_turn` as the single live execution core;
- composes Session, Profile, Agent, Host CEP, CubeWorkspace, and Promotion;
- maps lifecycle facts to public turn events.

### `lattice-session`

- owns the session graph interface and Git implementation;
- replaces production SQLite persistence with `GitSessionGraphStore`;
- owns Session commit schemas, refs, transcript reconstruction, branch, reset,
  checkout, and diff semantics;
- remains independent of Agent, Profile, Cube, CEP, and promotion.

### `lattice-profile`

- owns strict schema for verify, Cube settings, egress network policy, Git
  identity, and secret references;
- remains declarative and read-only.

### `lattice-agent`

- remains stateless between turns;
- accepts complete transcript and tool definitions;
- supports continuation after a provisional final response without learning
  Git or Cube semantics.

### `lancet-protocol`

- deletes envelope wire contracts;
- remains data-only and runtime-free.

### `lancet-catalog`

- deletes `session.envelope`;
- retains method-local requirements used as direct policy inputs.

### `lancet-cep`

- retains catalog framing and tool projection;
- deletes base authority and envelope planning.

### `lancet-authority`

- deletes active envelope storage and envelope capability matching;
- retains deterministic policy, Guard, effect regions, effect checking, and
  ledger semantics;
- exposes final candidate policy/effect evaluation without depending on Cube or
  LATTICE session types.

### `lancet-native`

- retains CEP method semantics and local test execution;
- moves filesystem and process access behind an execution interface usable by
  Cube;
- does not own Cube lifecycle, Session storage, or Lower promotion.

The local execution adapter is not reachable from the production LATTICE turn
path and therefore cannot be used by a model to operate Lower.

### `lancet-api`

- remains the LANCET runtime composition layer;
- accepts an execution adapter for process/files operations;
- keeps audit persistence and host audit methods;
- remains independent of `lattice-*` crates.

### Workspace Module

A new `lattice-workspace` crate owns:

- the CubeWorkspace production and test adapters;
- the Rust Cube client, written against the pinned CubeSandbox OpenAPI
  description;
- applying the Profile egress policy at Upper creation through CubeVS;
- managed Lower worktree provisioning and cleanliness inspection;
- warm template provisioning and rebuild policy;
- Git object export/import validation;
- promotion prepare and commit phases;
- locking, journaling, and recovery;
- candidate ref and snapshot deletion for the manual pruning command.

It does not own Session commits, Agent execution, Profile loading, CEP audit
persistence, or model-facing events.

### Unchanged Crates

`lancet-dispatch`, `lancet-audit`, `lattice-llm`, `lattice-tui`, and
`lattice-cli` keep their current roles. The CEP audit SQLite database in
`lancet-audit` is unaffected by the Session migration.

## 17. Failure Handling

- Cube creation failure: terminal `error` plus `aborted`; Lower unchanged.
- CEP transport or Cube operation failure: terminal `error`; promotion denied.
- Agent cleanup exhaustion: `pending_worktree`; snapshot retained.
- Verification failure: `pending_verification`; snapshot retained.
- Candidate object validation failure: `rejected`; Lower unchanged.
- Policy or effect rejection: `rejected`; Lower unchanged.
- Lower dirty at promotion with unchanged baseline: `pending_promotion`;
  Lower unchanged.
- Baseline compare-and-swap failure: `pending_conflict`; Lower unchanged.
- Ref transaction failure: no refs change; journal records failure.
- Materialization failure after ref success: repository recovery mode; no new
  turn starts until Lower matches the committed project ref.
- User abandonment: `aborted`; snapshot released.

## 18. Migration

Git becomes the only production Session backend after migration. LATTICE does
not dual-write SQLite and Git.

A one-shot migration reads an existing SQLite Session graph and writes
equivalent Session Git commits and refs. It records legacy session and commit
IDs in each migrated manifest for traceability. Migration verifies transcript
and topology equivalence before switching the production store.

Legacy SQLite sessions carry no project association; migration writes them
into the standalone data-directory Session repository.

The CEP audit SQLite database remains independent and is not migrated into the
Session Git object database.

The change that deletes the envelope also rewrites workspace documentation:
the `AGENTS.md` CEP contract section (the ten-method catalog list and the
active-envelope requirement for mutating methods) moves to the nine-method
direct-policy model in the same change.

## 19. Verification Strategy

### Unit tests

- envelope types and `session.envelope` are absent;
- catalog contains exactly nine production methods;
- direct policy evaluation has no mutable grant state;
- Session commit serialization is canonical and versioned;
- Session DAG traversal covers linear, branch, merge, reset, and detached
  histories;
- Lower and Upper cleanliness classification covers staged, unstaged,
  untracked, ignored, and conflicted states;
- candidate ancestry and object validation reject invalid inputs;
- import validation flags `.gitmodules` changes, symlink changes, and `.git`
  tree entries, and enforces object size limits;
- secret redaction covers audit, logs, tool results, and Session payloads.

### Adapter tests

- CubeWorkspace test adapter matches production interface behavior;
- no Lower absolute path appears in Cube requests or model context;
- Upper uses the Lower Git object format;
- Cube snapshot restore preserves dirty and clean states;
- egress policy is enforced inside Upper: `blocked` denies all egress and
  `allowlist` denies non-listed destinations;
- object import preserves commit OIDs, topology, authors, and committers;
- Upper hooks, config, refs, index, and reflogs are not imported.

### Integration tests

- a clean multi-commit candidate promotes with exact OIDs;
- dirty provisional completion resumes AgentLoop in the same turn;
- cleanup exhaustion creates `pending_worktree`;
- failed verification creates `pending_verification`;
- verification writes never appear in exported candidates or retained
  snapshots;
- promotion rejects a verification result bound to a different OID;
- concurrent sessions execute independently;
- the first successful promotion makes an older baseline conflict;
- conflict never auto-rebases;
- promoted project and Session refs update in one ref transaction;
- pending and rejected turns update only Lattice refs;
- a `pending_promotion` turn retries promotion without restoring Upper;
- workspace-less sessions persist to the data-directory repository;
- no-op turns create Session history without rewriting project history;
- tool-time audit and promotion-time facts both appear in attestation;
- crash injection at every journal stage recovers Lower to the committed ref.

### Migration tests

- SQLite topology and transcript migrate without loss;
- legacy IDs remain queryable as metadata;
- migration is restartable and does not dual-write.

## 20. V1 Scope

V1 includes:

- CubeSandbox as the production Upper adapter;
- a Rust Cube client pinned to a certified CubeSandbox service version;
- managed Lower worktrees as the default promotion target;
- per-project warm template provisioning;
- Host CEP using Cube process/files operations;
- optional one-shot runner for POSIX-specific tool behavior;
- complete envelope deletion;
- Profile-defined verification;
- clean-completion continuation;
- GitSessionGraphStore;
- exact Git candidate import;
- candidate and Session refs;
- baseline compare-and-swap;
- project and Session ref transactions;
- journaled Lower materialization;
- pending resume through Cube snapshots;
- multiple concurrently executing sessions;
- exposed credential injection and host redaction;
- Profile-configured Upper egress policy (`open`, `allowlist`, `blocked`)
  enforced through CubeVS;
- a manual pruning command for candidate refs and Cube snapshots;
- SQLite Session migration.

V1 deliberately excludes:

- automatic rebase or merge;
- partial-file promotion;
- credential brokerage;
- complete egress accounting and per-protocol traffic inspection;
- persistent VM LANCET workers;
- VM multi-user hardening;
- non-Cube production adapters;
- non-Linux, non-x86_64, or non-KVM production hosts;
- non-Git Lower workspaces;
- automatic snapshot garbage collection;
- remote synchronization of `refs/lattice/**`;
- signed attestation enforcement;
- multi-Agent scheduling policy.

The design leaves multi-Agent orchestration possible by keeping each Session,
Agent execution, Upper workspace, and baseline independent. Future orchestration
can add dependencies between turns without introducing shared mutable
workspaces or changing the Session storage model.

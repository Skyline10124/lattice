# ADR 0001: Workspace VCS Replaces Guest-Git Promotion

- Status: Superseded by ADR 0002
- Date: 2026-07-15
- Canonical vocabulary: root `CONTEXT.md`

## Context

The previous workspace model materialized `/workspace` as a full guest Git
repository, coupled every turn to a three-ref atomic promotion
(`refs/heads/<branch>` + session ref + candidate ref), parasitized the user's
repository under `refs/lattice/**`, and imposed version semantics on turn
boundaries (clean-Lower preflight, per-turn candidates, descendant checks,
snapshot authority for resume).

A dedicated design interrogation (2026-07-15) found this model contradicts the
intended semantics at the root:

- Sessions appeared isolated while turns raced to advance a shared project
  ref — a hybrid model producing rebase pressure that should never exist.
- Turn boundaries were forced to settle work (clean/dirty preconditions),
  while the intended workspace is a long-lived CoW scratch surface.
- "Resume" was modeled as reviving retained live state via sandbox snapshots;
  the intended model has no live state to revive at all.
- The candidate ref namespace was used incompatibly by two crates
  (session commits vs. project content commits).

## Decision

1. **The workspace layer is a LATTICE-owned CoW VCS.** The filesystem working
   directory is the substrate; Git is orthogonal to projection and not
   required. A session is: an initial State fixed zero-copy from the working
   directory as it stands, a persistent CoW Layer (mutation-only
   materialization; observation never copies), and exactly one pointer.
   History is a tree of immutable States navigated via the pointer's movement
   log — no branch management, no merge, no rebase. Presentation is a pure
   function of (State, Layer): nothing is ever resumed. Sandboxes and VMs are
   stateless executors with zero authority over VCS data. The runtime's
   default policy freezes at each turn boundary and attaches the turn's
   conversation record as State metadata, so dialogue and workspace roll back
   as one.

2. **States are never deleted.** No prune, no GC, no expiry. Accumulation cost
   is bounded by CoW structural sharing, not by reclamation.

3. **Delivery is a three-way judgement; LATTICE judges merges but never
   executes one.** Base is the last State the directory is known to embody
   (initial fixation, last successful delivery, or last Intake). With no
   path-level conflicts, the session delta applies atomically and completely
   while unrelated user edits stay untouched. Any conflict raises a
   first-class, structured Conflict (path-level judgement; file contents are
   never woven) and writes zero bytes. Resolution belongs to the user: Git
   Bridge export, an explicit Intake followed by in-session resolution, or by
   hand. Intake is the only channel for user edits to enter a session and is
   never implicit.

4. **The engine is gix (gitoxide) with a private object store.** State =
   commit/tree, pointer = ref, movement log = reflog, bookmark = tag-like ref,
   three-way judgement = tree diffs. LATTICE adds only the CoW skin: reflink-
   backed fixation with lazy object hashing, mutation-only Layer
   materialization, lazy projection into the VM. The object store lives in
   LATTICE's own data directory and never inside the user's `.git`; all Git
   semantics go through structured library calls, never a Git CLI.

## Rejected Alternatives

- **Guest Git repository + three-ref promotion** (status quo): shared-ref
  races, repository parasitism, and turn-boundary version semantics are the
  defects this ADR removes, not properties to preserve.
- **Per-turn snapshot chains, private clones, or HEAD-derived baselines**:
  all impose version identity on the lower side; the working directory itself
  is the substrate.
- **Prune/retention policies**: rejected in favor of "never delete".
- **Hunk-level weaving in the delivery path** (e.g. `gix-merge`): would make
  LATTICE responsible for synthesized content and cannot detect semantic
  conflicts; mechanical merging belongs to the user's Git, semantic
  integration to the agent after an Intake.
- **Building the VCS engine from scratch**: gix already provides the object
  store, refs, reflog, and diff machinery.
- **Pristine-only delivery** (interim draft): self-blocking — a session's own
  successful delivery, or any user edit, permanently closed the delivery
  channel.

## Consequences

- Retired (no-compat, hard removal): `crates/lattice-workspace/src/promotion/**`
  (three-ref transaction, `ManagedLower`, dirty/clean preflight,
  `NotDescendant`, pin/recovery journal), `crates/lattice-api/src/turn/promotion.rs`,
  `refs/lattice/**` parasitic refs in `lattice-session`, guest-Git
  materialization in `microsandbox.rs`, and `snapshot_lost` semantics.
- Issues #28/#29 lose their guest-Git/transfer foundation and need rewriting,
  not amendment.
- The `Workspace` trait contract and Deterministic/Microsandbox parity must be
  reassessed against the new model.
- Conversation storage migrates to State metadata on the Workspace VCS,
  replacing the current git-graph session store implementation.
- New dependency: gix. Fixation performance leans on reflink-capable
  filesystems; the fallback path is an implementation-layer concern.

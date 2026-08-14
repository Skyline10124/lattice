# AGENTS.md

Crate-local guidance for `lattice-store`.

## Role

ADR 0005 §4 domain-neutral private-Git/Linux durability kernel (#143). Owns the
reusable mechanics every private store needs and nothing else: XDG state
topology, `ProjectStoreKey` addressing, typed create/open with a recorded store
identity, canonical bytes, object/tree/commit writes, ref CAS and multi-ref
transactions, `flock` operation and repository locks, fsync/rename durability
barriers, atomic first-create, retention pins, per-location capability probes,
and typed store errors.

It is a Layer 0 leaf: no workspace-internal dependencies, by contract. Domain
semantics — sessions, invocations/evidence, workspace history — stay in
`lattice-session`, `lancet-audit`, and `lancet-runtime`.

## Sub-Architecture

- `src/layout.rs` owns `xdg_state_root` and `StateLayout`: the fixed topology
  (`sessions.git`, `projects/<key>/<store>.git`, `locks/sessions/<id>.lock`,
  `locks/projects/<key>/<name>.lock`). Domains own their file names.
- `src/key.rs` owns `ProjectStoreKey`: a domain-separated SHA-256 over the
  **raw path bytes** of the full canonical Local Workspace root. Not UTF-8, not
  content, not a Git remote, inode, or generation number.
- `src/meta.rs` owns the `refs/lattice/meta` identity record (kernel format,
  domain store kind, domain schema version, Global/Project scope) and
  `check_expected`.
- `src/repo.rs` owns `StoreRepo`: strict `open*`, atomic `create*`, reads,
  writes, `cas_ref`/`ref_transaction`, retention, and the fsync barriers
  (`fsync_publication`, `fsync_pending`).
- `src/lock.rs` owns the two `flock` semantics: re-entrant `acquire_flock` for
  operation locks and strict `acquire_flock_strict` for create/repository write
  locks, mediated by one process-wide registry.
- `src/durability.rs` owns fsync/rename primitives; `src/canon.rs` canonical
  JSON bytes; `src/probe.rs` per-location capability probes; `src/fault.rs`
  test-only fault seams; `src/bin/lattice-store-probe.rs` the probe binary.

## Boundaries

- No workspace-internal dependency, ever. A domain type, schema, ref name,
  store file name, or record shape appearing here is a layering break.
- No central schema authority, generic `put`/`get`, raw repository accessor,
  all-stores registry, or pluggable backend trait. Each domain wraps the kernel
  with its own typed handle.
- Strict open only: a missing store is `NotInitialized`, a wrong
  kind/version/scope is `Corruption`. Never open-failed-init, repair,
  quarantine, legacy path, or empty replacement.
- `create*` is the sole initialization path and is reserved for the composition
  entry points ADR 0005 names. It builds in a sibling temp, publishes with
  `renameat2(RENAME_NOREPLACE)`, and fsyncs the parent; the create lock and the
  repository write lock never overlap.
- Lock order is fixed: `SessionExecutionLock` -> optional
  `WorkspaceOperationLock` -> at most one repository write lock. Never two
  repository write locks together, and never an upgrade from a shared hold.
- No auto GC, repack, maintenance, or cascade delete in the first version.
- The kernel invents no clocks: `write_commit_at` takes the caller's timestamp.

## Verification

- Local: `cargo test -p lattice-store`.
- Clippy: `cargo clippy -p lattice-store --all-targets -- -D warnings`.
- Domain migrations that ride on kernel changes:
  `cargo test -p lattice-session -p lancet-runtime`.
- No workspace deps: `cargo tree -p lattice-store --edges normal` must list no
  `lattice-*`/`lancet-*`/`net-guard` crate.

## Next Actions

- Keep additions mechanism-shaped. The test for a new primitive is whether two
  unrelated domains would use it identically; if only one would, it belongs in
  that domain's crate.
- When a domain needs a new read, prefer a narrow typed accessor
  (`read_tree_path`, `tree_files`, `commit_message`) over exposing the
  underlying `gix::Repository`.
- `invocations.git` is created by #139-A, not here; do not add a placeholder.

## Record Maintenance

- Update this file in the same patch that changes the kernel's surface,
  topology, identity record, lock semantics, or durability contract.
- Record unresolved kernel risks in root `FINDINGS.md` and landed fixes in root
  `FIX.md` with verification commands and changed files.

# AGENTS.md

Crate-local guidance for `lancet-native`.

## Role

This crate owns the current production native CEP implementation: method
handlers, server lifecycle, policy/guard/audit integration, edit application,
web helpers, and effect-contract enforcement. ADR 0004 migrates every Tool to a
WASM Component and absorbs the remaining concrete host responsibilities into
`lancet-runtime`; do not treat this crate as a permanent Tool extension point.

## Sub-Architecture

- `src/handlers/` owns production CEP method handlers: external (`fetch.web`)
  only. There is no `session_state.rs` — #29 migrated `session.todo_write` to an
  embedded Component Tool and deleted its handler and registration — no
  `observation.rs` (#30), and no mutation handlers (#31): those Methods are
  embedded Component Tools whose filesystem authority lives in `lancet-runtime`'s
  `workspace-observe` / `workspace-mutate` host imports. There is no native
  fallback: a runtime whose composition does not embed the Component reports
  the method as unrouted.
- `src/server/` owns registry setup, transport lifecycle, exact durable
  reservation admission, `DispatchStarted` publication, and per-call typed
  `InvocationId` events. `transport.rs` also builds the per-call
  `WorkspaceObservationHost` and `WorkspaceMutationHost` handed into
  `ComponentCallContext`; terminal/evidence durability belongs to the
  `lattice-api` coordinator and `lancet-audit::InvocationStore`.
- `src/enforcement/` owns Guard L1 bridging and preflight metadata.
- `src/edit/` retains the residual hashline apply engine (parser/apply/storage)
  as a `#[cfg(test)]` regression asset and aliases `limits::WriteLimits` to
  `lancet_runtime::MAX_WORKSPACE_WRITE_BYTES` so its behavior tests cannot drift
  from the live mutate host cap (#120 / #31). It is absent from production;
  `mutate.create` / `mutate.edit` execute only as Components.
- `src/workspace/` is gone; `lib.rs` aliases the same name to
  `lancet_runtime::workspace` (#30 moved the concrete module down a layer so
  the Tool Host imports can reach it). Rewind and residual edit-engine tests
  use the *same* concrete `LocalWorkspace` (#59/#60/#61, Codeberg #37/#38/#39;
  plus #63) through that alias — one home for secure per-segment path
  resolution, Policy-Visible Workspace filtering, bounded read/list/grep/find,
  content hashing, no-replace create, atomic edit install, mutation-history
  recording, and filesystem error normalization. The alias is a path, not a
  seam.
- `src/executor/` owns crate-private web helpers and output limits. The
  observation render helpers left with #30: the host renders and charges the
  final `read` content, while the Component wraps that result and shapes the
  host-bounded `list` / `grep` / `find` payloads in `tools/observe-workspace`.
- `src/effect_contract_tests/` exercises runtime effect contracts.

## Boundaries

- Keep low-level executor helpers crate-private behind native handlers.
- Tool projection belongs in `lancet-cep`; do not add native compatibility
  projection shims.
- Mutations must pass active envelope, Guard L1, effect checks, and audit
  recording. Post-mutation verify is retired; do not reintroduce it.
- Do not expose direct native executors to `lattice-agent`.
- Do not add new native Tool implementations or a native Tool ABI. New Tool
  contracts start in Source manifests; Components receive authority only through
  Tool Host imports.
- The concrete guarded Local Workspace is the single workspace path. The
  `ExecutionBackend`/routing seam was deleted in #60 — do not reintroduce a
  pluggable workspace implementation, routing enum, fallback, or parallel
  path. #30 moved the concrete host module into `lancet-runtime` (the Tool
  Host imports cannot reach up a layer); the crate-local alias in `lib.rs`
  keeps native call sites unchanged without creating a second home.

## Recent Issues

- FIXED #158: queued caller cancellation is rejected before invocation
  admission; successful durable admission marks the private transport request
  as `DispatchStarted` before executor handoff. Admission store/task failures
  expose only stable typed, path-free public errors.
- ADDED #30: `observe.workspace` migrated to the embedded
  `tools/observe-workspace` Component Tool. The native observation handler,
  its registration, the executor render helpers, and the authority's
  `observe.workspace` policy arm are gone; `lancet_catalog` drops to three
  native methods (Create/Edit/Fetch). All filesystem authority for the four
  modes lives in `lancet-runtime::host_workspace` behind the
  `workspace-observe` Tool Host imports — System Path Exclusion, safe
  symlink/root containment, read denial, ignored-path inclusion, bounded
  results, and the turn-scoped observation budget are enforced host-side,
  and `ObservedEffect.truncated` is computed by the host, never the guest.
  `transport.rs::workspace_observation_host` builds the per-call host from
  the resolved visibility policy and shares the turn's budget tracker. A
  host refusal reaches the caller as the structured `CepError` (e.g.
  `PathContainment` for root escapes — the legacy policy hook used to
  normalize the same denial to `GuardDenied`), not the guest's paraphrase.
  The concrete `LocalWorkspace` moved to `lancet-runtime::workspace` with
  it; `lib.rs` re-exports the module and `rewind` under their old names.
- ADDED #28 / HARD-CUT #139-B: Component dispatch runs capability admission
  before `rt.call`. A denial returns `CapabilityNotGranted` with typed admission
  evidence; an admitted call carries the admitted set into
  `ComponentCallContext`. The server accepts a request only when its exact
  `InvocationBinding` is durably `Reserved`, publishes `DispatchStarted` before
  executor handoff, and rejects duplicate dispatch. Execution returns one
  content-free `CellEvidenceTriple`; the `lattice-api` TurnCoordinator publishes
  the terminal record and evidence atomically to InvocationStore. Component/native
  method-name collision fails server composition closed
  (`validate_no_component_native_collision`); there is no mixed audit vocabulary
  or SQLite row finalization path.
- ADDED #64: `workspace::rewind::preflight_and_restore` (re-exported as
  `lancet_native::rewind`) applies an explicit Rewind's per-path restores.
  It is a host-side operator surface, not a CEP method — the catalog gains
  no `workspace.rewind`. Phase 1 verifies every affected path against its
  recorded post-state through `WorkspacePath::parse`, System Path Exclusion,
  and the same symlink-rejecting secure walk mutations use, reporting a
  structured conflict for any divergence and writing nothing. Rewind is
  deliberately exempt from the `WriteLimits` byte cap (#120): it reinstates bytes
  the workspace already held, so capping it would make a previously legal state
  unrestorable rather than preventing a new write. Phase 2 runs
  only after every path verified, reusing `install_atomic_sync` /
  `remove_file_sync` (the shared synchronous cores of the async mutation
  primitives). The executor never records history: reversing a mutation must
  not append compensation commits.
- ADDED #63: every successfully installed Atomic Local Mutation is appended to
  the run's private `lancet_runtime::WorkspaceHistory`. `ServerRunConfig`
  carries the history, `run_native` attaches it to the one `LocalWorkspace`,
  and Component `mutate.create`/`mutate.edit` (via the `workspace-mutate`
  Tool Host in `lancet-runtime`) call `record_mutation` only after an install
  receipt proves the target changed — including an atomic install followed by
  parent-directory fsync failure — so precondition, replan, guard, and policy
  failures record nothing and never move the current history ref. The
  recorded commit ids land in durable audit / MutationEvidence. A history
  failure after a successful install is recorded as evidence and never
  triggers a compensating write. The native `handlers/mutation.rs` path was
  removed in #31.
- FIXED GitHub #39 (= Codeberg #17; PR #74 review defects) — four
  `src/workspace/local.rs` fixes. NOTE: #39 here is the operator audit sink
  ticket, not the Direct-Local workspace tickets #59/#60/#61 cited above and
  below.
  (1) Grep byte budget: `GREP_MAX_FILE_BYTES` 8 MiB / `GREP_MAX_TOTAL_BYTES`
  64 MiB walk aggregate / `GREP_MAX_LINE_BYTES` 64 KiB + `struct GrepBudget`;
  `grep_file` streams via `BufReader` + `Read::take` instead of
  `fs::read_to_string`, and over-cap files are skipped before being opened.
  (2) `list_dir` uses non-following `DirEntry::file_type()` via `file_type_kind`,
  so symlinks (including dangling ones) list as `PathKind::Symlink` instead of
  failing the whole listing; `metadata_kind` stays for `resolve_secure` callers.
  (3) `glob_match` is a real iterative two-pointer fnmatch matcher (`*`, `?`,
  `[abc]`, `[a-z]`, `[!...]`; unclosed `[` literal) plus `match_char_class`,
  backing both `find_files` patterns and grep's glob filter.
  (4) Write-path TOCTOU closed: private `open_parent_dir_secure` walks the
  parent chain with `rustix` `openat`
  (`O_RDONLY|O_DIRECTORY|O_NOFOLLOW|O_CLOEXEC`) relative to the already-pinned
  fd (`mkdirat` + EEXIST-tolerant retry when creating), and `create_excl` /
  `install_atomic` / `remove_file` reach the filesystem only through that fd
  with single-component leaf names (`openat O_CREAT|O_EXCL`, `renameat`,
  `unlinkat`, `fsync` on the dirfd; path-based `fsync_dir` is gone). It is NOT
  `openat2(RESOLVE_BENEATH)`:
  pinning defeats symlink substitution but not a rename of the pinned directory
  out of the workspace between pin and leaf op — accepted residual L-39-04.
  Adds `rustix` (features `["fs"]`) to the root workspace deps and here.
  Deliberate deltas: over-cap files are silently skipped by grep (fewer matches,
  no error); grep keeps matches found before a mid-file IO/invalid-UTF-8 error
  instead of dropping the file; a regular-file parent component now surfaces as
  `SymlinkRejected` (ENOTDIR from the `O_DIRECTORY` walk), not `Io`.
  Read/observation paths remain `resolve_secure`-only — accepted residual
  L-39-01, plus L-39-02/L-39-03 on walk depth and symlink reporting.
- FIXED #70 (Codeberg #48): call-scoped cancellation is audited from the
  structured code, not a message string — `finalize_audit_for_dispatch` records
  `AuditOutcome::Cancelled` on `CepErrorCode::CallCancelled` (the old
  exact-message arm was unreachable dead code), and `is_incomplete_dispatch`
  matches the code only. Component and (future) native cancellations share
  one classification authority.
- ADDED #27 (Codeberg #5) slice D: `ServerRunConfig` carries
  `Option<Arc<ComponentRuntime>>`. `handle_request` branches Component vs
  native dispatch after audit start and before finalize, sharing one durable
  audit path. Component calls produce Cell evidence (`component_cells` in
  audit metadata); native calls produce effect-ledger evidence as before.
  Production `from_embedded()` yields an empty Component catalog so all 5
  native methods are unchanged.
- FIXED #60: routing/backend seam and all spy backends deleted; handlers and
  the edit engine call the concrete `LocalWorkspace` directly; audit routing
  metadata replaced with method-local evidence (guarded path, `local_revision`,
  post-install content hash, `PathGuard`/`LocalIo` rejection stages); narrow
  one-shot `fail_next_install` fault injection is the only test seam.
- FIXED #61 follow-up: ignored ancestors reachable only for an exact included
  descendant are navigable but not emitted by list/find; successful
  read/list/grep/find audit metadata now records content-free visibility rule
  categories without hidden paths, content, counts, or hashes.
- FIXED #120: the single-file write byte cap lives once, as
  `lancet_runtime::MAX_WORKSPACE_WRITE_BYTES` (1 MiB). Live mutate installs
  check it in `host_mutate` before any filesystem write; the residual
  `edit::limits::WriteLimits` aliases the same constant. Deliberately not a
  config axis. Rewind restore (`workspace::rewind`) and patch rollback
  (`edit::storage::rollback_edit_target`) stay exempt because neither
  introduces new content — they reinstate bytes the workspace already held.
- FIXED L-30/L-31: cleanup metadata records all cleanup failures, and positive
  commit-baseline hunk summaries are recorded when evidence exists.
- PARTIAL M-05: web fetch rebinding defense uses request-time pinned IP plus a
  maintained rebinding suffix denylist.
- WATCH: current schemas must continue to come from `lancet-cep` projection until
  generated descriptors replace it; never hand-write a native or Component
  compatibility schema.

## Next Actions

- If future fetch capabilities need wildcard hosts, implement semantics across
  protocol, authority, native, and tests instead of using pattern wording alone.
- Keep `lancet_runtime::MAX_WORKSPACE_WRITE_BYTES` the only single-file write
  cap. A write path that installs new content must check it before touching the
  filesystem; do not add a second literal, a profile key, or a per-method budget.
  Restore paths (Rewind, patch rollback) stay exempt — record any new exemption
  and its reason next to the #120 entry above.
- Keep web fetch security centered on request-time DNS pinning; treat denylist
  additions as defense in depth.
- Keep mutation-history recording on the mutate host after a successful install:
  a rollback inside residual patch-engine tests must never produce a history
  commit. #64 consumes this history; do not add a second recording site.

## Record Maintenance

- Update `Recent Issues` and `Next Actions` whenever native handlers,
  enforcement, executor helpers, edit logic, or impact behavior changes.
- Keep open/deferred/residual native issues in root `FINDINGS.md`.
- Add landed native fixes to root `FIX.md` with verification and changed files.
- For partial fixes, update this file and leave explicit residual risk in
  `FINDINGS.md`.
- Keep this file near 80 lines by pruning stale closed issue notes.

## Verification

- Local: `cargo test -p lancet-native`.
- Handler/lifecycle changes: also run affected `lattice-api` round-trip,
  policy, operator-audit, and release tests.
- Edit/security changes need focused regression tests for path, symlink, scope,
  verify, and effect-region behavior.

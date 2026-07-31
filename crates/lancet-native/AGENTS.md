# AGENTS.md

Crate-local guidance for `lancet-native`.

## Role

This crate owns the current production native CEP implementation: method
handlers, server lifecycle, policy/guard/audit integration, edit application,
web helpers, and effect-contract enforcement. ADR 0004 migrates every Tool to a
WASM Component and absorbs the remaining concrete host responsibilities into
`lancet-runtime`; do not treat this crate as a permanent Tool extension point.

## Sub-Architecture

- `src/handlers/` owns production CEP method handlers: mutation and external.
  There is no `session_state.rs` — #29 migrated `session.todo_write` to an
  embedded Component Tool and deleted its handler and registration — and no
  `observation.rs`: #30 migrated `observe.workspace` to an embedded Component
  Tool whose filesystem authority lives in `lancet-runtime`'s
  `workspace-observe` host imports. There is no native fallback: a runtime
  whose composition does not embed the Component reports the method as
  unrouted.
- `src/server/` owns registry setup, transport lifecycle, and durable call
  metadata. `transport.rs` also builds the per-call
  `WorkspaceObservationHost` (workspace scope check, visibility policy,
  `ObservationLimits`, shared turn budget charge) handed into
  `ComponentCallContext`.
- `src/enforcement/` owns Guard L1 bridging and preflight metadata.
- `src/edit/` owns patch parsing, hash checks, application, diagnostics, and
  `limits::WriteLimits` — the one single-file write byte cap (1 MiB) both
  mutation write paths read.
- `src/workspace/` is gone; `lib.rs` aliases the same name to
  `lancet_runtime::workspace` (#30 moved the concrete module down a layer so
  the Tool Host imports can reach it). The still-native mutation handlers and
  the edit engine use the *same* concrete `LocalWorkspace` (#59/#60/#61,
  Codeberg #37/#38/#39; plus #63) through that alias — one home for secure
  per-segment path resolution, Policy-Visible Workspace filtering, bounded
  read/list/grep/find, content hashing, no-replace create, atomic edit
  install, mutation-history recording, and filesystem error normalization.
  The alias is a path, not a seam.
- `src/executor/` owns crate-private web helpers and output limits. The
  observation render helpers left with #30: result shaping for
  `observe.workspace` is guest-side presentation in
  `tools/observe-workspace`.
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
- ADDED #28 (Codeberg #6): the Component branch of `transport.rs` runs `admit_tool` against
  `cep_context.policy.capability_admission` before `rt.call`. A denial answers
  `CapabilityNotGranted` and still finalizes the audit row with its
  `AdmissionEvidence` — a Component call never finalizes with absent metadata.
  An admitted call carries the admitted set into `ComponentCallContext`, and
  finalize merges admission evidence, observed effects, and denied operations.
  `ServerRunConfig::validate_capability_admission` makes an unknown deny name a
  startup error at `run_native`. Admission is unconditional: `CepMode` is not
  consulted. The legacy pre-dispatch deny and all native handlers are untouched.
  The dispatch split also picks the audit vocabulary:
  `CallAuditMetadata::for_component_dispatch` for a catalogued Component (never
  the legacy `unrouted` labeling), finalizing as `component_handler` or
  `component_denied` — a Component refusal is a Cell policy denial, never
  `method_not_found`. Every finalize arm, cancellation included, persists the
  merged evidence metadata: an outcome ends the call, it does not unmake the
  evidence the call already produced. Nothing forbids a Component name that
  collides with a native catalog method: the legacy pre-dispatch effect-kind
  deny runs for any method `lancet_catalog` knows, before the split, so a
  collision would arm both vocabularies in one call. Unreachable while the
  Component catalog is empty — read #28 design §11 before migrating a native
  method.
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
  and `mutate.create`/`mutate.edit` call `record_mutation` only *after* the
  install returned success — so precondition, replan, guard, and policy
  failures record nothing and never move the current history ref. The
  recorded commit ids land in durable audit metadata under
  `workspace_history`. A history failure after a successful install is
  recorded as evidence and never triggers a compensating write.
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
  `unlinkat`, `fsync` on the dirfd; path-based `fsync_dir` is gone). Portable
  (Linux + macOS; no `openat2`/`O_PATH`). It is NOT `openat2(RESOLVE_BENEATH)`:
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
- FIXED #120: the single-file write byte cap lives once, in
  `edit::limits::WriteLimits` (1 MiB, `check_write_size`), replacing
  `edit::apply::PatchApplyLimits`. `mutate.create` was uncapped; it now checks
  the request content length in `preflight_create` before any filesystem write,
  so the no-replace install never leaves a partial target. Both paths report
  `ToolError::SizeLimit` text (limit + actual) as `ExecutionFailed` +
  `CepErrorData::Execution`. Deliberately not a config axis. The cap applies to
  the two Atomic Local Mutation paths. Two write sites are deliberately exempt
  because neither introduces new content: Rewind restore
  (`workspace::rewind`) and patch rollback (`edit::storage::rollback_edit_target`)
  both reinstate bytes the workspace already held.
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
- Keep `edit::limits::WriteLimits` the only single-file write cap. A write path
  that installs new content must call `check_write_size` before touching the
  filesystem; do not add a second literal, a profile key, or a per-method budget.
  Restore paths (Rewind, patch rollback) stay exempt — record any new exemption
  and its reason next to the #120 entry above.
- Keep web fetch security centered on request-time DNS pinning; treat denylist
  additions as defense in depth.
- Keep mutation-history recording at the handler level, after a successful
  install: a rollback inside the patch engine must never produce a history
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
  policy, provenance, and release tests.
- Edit/security changes need focused regression tests for path, symlink, scope,
  verify, and effect-region behavior.

# AGENTS.md

Crate-local guidance for `lattice-api`.

## Role

This crate owns the public facade for session creation/opening and turn
execution. It exposes `SessionApi`, `stream_turn`, `run_turn`, and public turn
events/results. Runtime composition lives in `lancet-api`; this crate translates
profile `[lancet]` policy and drives the journal-backed `TurnCoordinator` over
its reservation-bound Component runtime.

## Sub-Architecture

- `src/session.rs` owns public session facade operations and session error
  mapping.
- `src/turn/live.rs` owns the live streaming turn core.
- `src/turn/execute.rs` owns the `run_turn` entrypoint.
- `src/turn/collector.rs` drains live streams into `RunTurnResult`.
- `src/turn/cep.rs` owns profile-to-policy translation, Tool-event-to-public-event
  mapping, and `build_production_turn_deps` over
  `lancet_api::LancetRuntime::build`. First-time shared Component composition runs
  through `spawn_blocking`; assembly itself remains in `lancet-api`.
- `src/turn/coordinator.rs` owns the production #139-B `TurnCoordinator`:
  blocking store adapters (`spawn_blocking` over `GitSessionGraphStore` and the
  Git `InvocationStore`), the injectable `IdSource`, durable reconciliation,
  and terminal/evidence publication.
- `src/turn/saga.rs` drives each live Turn through the durable journal, binding,
  reservation, sequential Component execution, ResultDraft, terminal, and finalization stages.
- `src/turn/invocation_facts.rs` is the #139-B cross-domain projection
  (`lancet_audit::PinnedLedgerView` ->
  `lattice_session::pending::InvocationFactsSnapshot`). This crate owns that
  composition because `lancet-audit -> lattice-session` must not exist.
- Integration tests cover Component Tool calls, typed operator audit reads, release flows, and
  consistency.
- Project-store tests serialise `XDG_STATE_HOME` through
  `lancet_audit::ScopedXdgStateHome`, pulled in via
  `[dev-dependencies] lancet-audit = { features = ["test-support"] }`. The
  crate-local `src/test_env.rs` copy was deleted in GitHub #39; there is exactly one
  such helper in the workspace and it lives in `lancet-audit/src/test_support.rs`.

## Boundaries

- `stream_turn` is the single live execution core; `run_turn` drains it.
- Keep CLI rendering out of this crate.
- Keep provider protocol details in `lattice-llm`.
- Keep Component execution implementation in `lancet-runtime`, below this crate;
  it does not belong in the Session/Turn facade.
- ADR 0005 assigns the production cross-store saga here. `TurnCoordinator` owns the full-Turn session execution lock, PendingTurn,
  exact binding plan, InvocationStore reservation/finalization, Tool dispatch,
  ResultDraft, reconciliation, and final Turn commit through narrow domain
  interfaces. It must never hold two repository write locks, invent a
  cross-store transaction, or expose a standalone/direct Tool-runtime caller.
- `TurnCoordinator` is the only Turn saga owner, but not the only cross-store
  workflow in this crate: explicit rewind is owned by a separate
  `RewindCoordinator`. Both obey SessionExecutionLock -> project
  WorkspaceOperationLock -> at most one repository write lock. PendingTurn does
  not silently become a rewind crash journal.

## Recent Issues

- FIXED #34 PR review: if runtime admission cannot prove pre-dispatch state, the
  live saga remains at its durable `StepClosing` snapshot and emits typed
  `RecoveryRequired`; reopen reconciles ledger facts and never redispatches the
  Tool or publishes a guessed `NotExecuted` terminal.
- FIXED #159: live and reopen finalization consume the same journal-derived
  cumulative transcript/candidate basis. Reconciliation terminal evidence uses
  the frozen binding method. One shared WorkspaceOperationLock now covers the
  authoritative history `current()` read, TurnClosing publication, and final
  Session ref transaction; a reopened pinned/current mismatch fails closed.
- ADDED #28 (Codeberg #6): `build_production_turn_deps` lowers the loaded
  profile into `CapabilityAdmissionPolicy` via
  `AgentProfile::capability_admission_policy` (`[tools].enabled` is the stage-1
  grant, `[lancet.capabilities]` subtracts) and hands it to
  `LancetRuntimeConfig`. Without a loaded profile nothing is admitted.
  `cep_policy_from_profile` only carries executor policy, so it leaves the
  admission field at its fail-closed default.
- ADDED #64: `rewind_to_turn` is the product-level explicit Rewind. It
  resolves the target Turn in the current session (nonexistent, foreign, and
  unreachable targets are refused before anything is read), refuses a Turn
  whose workspace-history identity is unavailable rather than guessing,
  delegates the workspace half to `lancet_api::rewind_workspace`, and only
  then moves the Session History ref — pinning the abandoned head first. A
  same-path conflict returns structured data with nothing moved; a ref
  durability failure is an error carrying the partial state, never a
  success. It is a free function, not a `SessionApi` method: the session
  facade stays pointer-movement only.
- FIXED #39: the crate-local `src/test_env.rs` XDG helper was deleted; this
  crate now consumes the single workspace-wide `lancet_audit::ScopedXdgStateHome`
  through the `test-support` dev-dependency feature. Do not reintroduce a local
  copy — the helper owns a process-global env mutex, and a second copy
  reintroduces the `XDG_STATE_HOME` race that ENV-OOM-02 closed.
- FIXED #46 (Codeberg #24): public `TurnStarted` includes `turn_id` for audit
  replay, and `[llm].timeout_secs` lowers to the Turn deadline in `turn/saga.rs`
  (with the frozen live request prepared in `turn/live.rs`).
- FIXED H-06: `stream_turn` now streams live instead of replaying buffered
  events after full turn completion.
- FIXED M-19b: observer backpressure no longer blocks live task drain/commit.
- FIXED M-15: session error mapping preserves structured error categories.
- FIXED: `SessionApi` now resolves abbreviated session ID prefixes via
  `resolve_id()` — exact match first, then prefix scan over `store.list()`.
  Ambiguous prefixes list up to 5 candidate IDs and suggest more characters.
  Empty/whitespace IDs are rejected. Canonical IDs remain 32-char UUIDs;
  abbreviation is a presentation/input concern. `display_id` in the CLI
  truncates to 8 chars for compact output.

## Next Actions

- Keep runtime composition in `lancet-api`; this crate translates profile policy
  and drives `TurnCoordinator`. Do not add Component-engine assembly here.
- Consume Tool definitions projected by the LANCET public facade; do not add a
  LATTICE-owned schema or second dispatch path.
- Keep `run_turn` implemented through `stream_turn` and `RunTurnCollector`.
- Add focused tests whenever public `TurnEvent` mapping or commit semantics
  change.
- Read the workspace-history identity after the LANCET runtime has drained;
  the pure reducer emits `TurnCommitCandidateBasis`, and only the composition
  layer may complete it with a validated checkpoint sampled under the shared
  operation lock. Never substitute `Empty` for an error or malformed OID.

## Record Maintenance

- Update `Recent Issues` and `Next Actions` when turn composition, public event
  mapping, enabled tool behavior, or session facade behavior changes.
- Record unresolved API/runtime composition risks in root `FINDINGS.md`.
- Record landed API fixes in root `FIX.md` with verification commands.
- For partial architectural moves, leave residual boundaries explicit in
  `FINDINGS.md`.
- Keep this file near 80 lines by pruning stale fixed bullets after they stop
  guiding edits.

## Verification

- Local: `cargo test -p lattice-api`.
- Runtime composition changes: also run affected `lancet-runtime`, agent,
  session, and CLI tests.
- Streaming changes should cover live events, collector output, failures,
  cancellation, commit, and observer backpressure.

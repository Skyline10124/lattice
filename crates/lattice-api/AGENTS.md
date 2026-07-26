# AGENTS.md

Crate-local guidance for `lattice-api`.

## Role

This crate owns the public facade for session creation/opening and turn
execution. It exposes `SessionApi`, `stream_turn`, `run_turn`, and public turn
events/results. LANCET runtime composition now lives in `lancet-api`; this crate
translates profile `[lancet]` policy into LANCET-native config and wires the
agent loop over `lancet-api`'s CEP runtime.

## Sub-Architecture

- `src/session.rs` owns public session facade operations and session error
  mapping.
- `src/turn/live.rs` owns the live streaming turn core.
- `src/turn/execute.rs` owns the `run_turn` entrypoint.
- `src/turn/collector.rs` drains live streams into `RunTurnResult`.
- `src/turn/cep.rs` is the LATTICE↔LANCET seam: profile→policy translation,
  `CepExecutorAdapter` (LLM tool calls → typed CEP), CEP-event→agent-event
  mapping, and `build_production_turn_deps` (delegates composition to
  `lancet_api::LancetRuntime::build`). Runtime assembly itself is in `lancet-api`.
- `src/turn/mapping.rs` and `src/turn/types.rs` own public event/result mapping.
- Integration tests cover CEP round trips, audit provenance, release flows, and
  consistency.
- `src/test_env.rs` (`#[cfg(test)]` via `lib.rs`) serialises `XDG_STATE_HOME`
  for audit-path tests; duplicated in `lattice-cli/src/commands/test_env.rs` for
  CLI audit/status tests (no shared test-support crate yet).

## Boundaries

- `stream_turn` is the single live execution core; `run_turn` drains it.
- Keep CLI rendering out of this crate.
- Keep provider protocol details in `lattice-llm`.
- Keep LANCET execution implementation below this crate. It currently lives in
  `lancet-native`; after ADR 0004 convergence it lives in `lancet-runtime`.
  Neither path belongs in the session/turn facade.

## Recent Issues

- ADDED #28: `build_production_turn_deps` lowers the loaded profile into
  `CapabilityAdmissionPolicy` via `AgentProfile::capability_admission_policy`
  (`[tools].enabled` is the stage-1 grant, `[lancet.capabilities]` subtracts)
  and hands it to `LancetRuntimeConfig`. Without a loaded profile nothing is
  admitted. `cep_policy_from_profile` only carries executor policy, so it leaves
  the admission field at its fail-closed default.
- ADDED #5 slice D: `CepExecutorAdapter` now takes `definitions: &[ToolDefinition]`
  + `enabled_names` instead of deriving the available set from `catalog()`.
  The hardcoded five-arm `dispatch_typed`/`call_typed`/`schema_mismatch_for_method`
  and the five method type imports are deleted; all methods go through
  `CepClient::call_raw`, so Component Tools are no longer blocked by central
  catalog membership. Params/result validation is done server-side (native
  typed registry or generated descriptor validators).
- FIXED M-24-04: `lancet-cep` now separates `ToolExposure` from
  `BaseAuthorityPlan`, and catalog descriptors declare method-local base
  authority requirements. This crate converts CEP tool definitions to provider
  tool definitions only at the LATTICE↔LANCET seam.
- FIXED M-24-05: turn/LANCET runtime composition extracted into the new
  `lancet-api` crate. `cep.rs` is now only the LATTICE↔LANCET seam (profile→policy
  translation, `CepExecutorAdapter`, event mapping); `enabled_tools.rs` moved to
  `lancet-api`.
- FIXED #24: public `TurnStarted` includes `turn_id` for audit replay, and
  `[llm].timeout_secs` lowers to the agent turn deadline in `turn/mapping.rs`.
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

- Keep runtime composition in `lancet-api`; this crate should only translate
  profile policy and wire the agent loop. Do not reintroduce server/transport
  assembly here.
- Consume Tool definitions projected by the LANCET public facade; do not add a
  LATTICE-owned schema or native/Component dispatch switch during migration.
- Keep `run_turn` implemented through `stream_turn` and `RunTurnCollector`.
- Add focused tests whenever public `TurnEvent` mapping or commit semantics
  change.

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
- Runtime composition changes: also run affected native, agent, session, and CLI
  tests.
- Streaming changes should cover live events, collector output, failures,
  cancellation, commit, and observer backpressure.

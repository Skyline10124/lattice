# AGENTS.md

Crate-local guidance for `lattice-api`.

## Role

This crate owns the public facade for session creation/opening and turn
execution. It exposes `SessionApi`, `stream_turn`, `run_turn`, public turn
events/results, and currently composes the turn-scoped LANCET runtime.

## Sub-Architecture

- `src/session.rs` owns public session facade operations and session error
  mapping.
- `src/turn/live.rs` owns the live streaming turn core.
- `src/turn/execute.rs` owns the `run_turn` entrypoint.
- `src/turn/collector.rs` drains live streams into `RunTurnResult`.
- `src/turn/cep.rs` composes native registry, dispatch client/server, profile
  policy lowering, base envelopes, audit, invariants, and `CepExecutorAdapter`.
- `src/turn/enabled_tools.rs` resolves enabled tool exposure.
- `src/turn/mapping.rs` and `src/turn/types.rs` own public event/result mapping.
- Integration tests cover CEP round trips, audit provenance, release flows, and
  consistency.

## Boundaries

- `stream_turn` is the single live execution core; `run_turn` drains it.
- Keep CLI rendering out of this crate.
- Keep provider protocol details in `lattice-llm`.
- Keep native handler implementation in `lancet-native`.

## Recent Issues

- OPEN M-24-04: `tools.enabled` currently drives both LLM tool exposure and
  parts of base authority derivation.
- OPEN M-24-05: this crate has grown from thin facade into turn runtime
  composition and LANCET control-plane wiring.
- FIXED H-06: `stream_turn` now streams live instead of replaying buffered
  events after full turn completion.
- FIXED M-19b: observer backpressure no longer blocks live task drain/commit.
- FIXED M-15: session error mapping preserves structured error categories.

## Next Actions

- Split tool exposure from authority shaping for M-24-04; exposure should not be
  treated as an authorization boundary.
- Plan M-24-05 by extracting internal turn/LANCET composition while keeping this
  crate as public request/response/event facade.
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

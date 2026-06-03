# AGENTS.md

Crate-local guidance for `lattice-agent`.

## Role

This crate owns the multi-turn `AgentLoop`: turn state machine, message
assembly, streaming event handling, retry behavior, tool execution sequencing,
public agent turn events, and injectable `LlmClient` / `ToolCallExecutor`
traits.

## Sub-Architecture

- `src/agent_loop/mod.rs` owns the main turn loop, tool rounds, cancellation,
  deadlines, event buffering, and live mirror emission.
- `src/agent_loop/messages.rs` owns graph-to-LLM message assembly and append
  helpers.
- `src/agent_loop/traits.rs` owns injectable LLM and tool executor traits.
- `src/agent_loop/types.rs` owns requests, results, status, events, and error
  payloads.
- `src/testing.rs` provides feature-gated test utilities.
- `tests/agent_loop.rs` covers main loop behavior.
- `tests/streaming_*.rs` and `tests/live_sink_and_cancel.rs` cover streaming,
  deltas, live sinks, and cancellation.

## Boundaries

- Do not construct native LANCET transports or call native executor helpers.
- Do not implement session storage; consume `lattice-session` graph values.
- Keep tool execution injectable so `lattice-api` can provide CEP execution.
- `buffered` event sequence remains authoritative; live sink is best-effort.

## Recent Issues

- OPEN L-21: cancellation waits for an in-flight tool future to finish or hit
  the turn deadline before checkpoint exit.
- OPEN L-22: `ChatRequest` still clones the full provider message `Vec` each
  LLM round.
- OPEN L-23: `run_turn_inner` is a large hot-path function with scattered clone
  costs and maintainability risk.
- FIXED M-21: graph-to-LLM message conversion is now incremental across tool
  rounds.
- FIXED M-19a: live mirror emission uses best-effort `try_send` so a full live
  sink does not block the agent loop.

## Next Actions

- Address L-21 only with clear cancellation semantics for in-flight tools and
  tests for long-running tool futures.
- Fold L-22 into an API-shape change such as borrowed requests, `Arc<[Message]>`,
  or builder reuse.
- Split `run_turn_inner` by phase before optimizing ownership; avoid mixing a
  large structural refactor with behavior changes.
- Preserve cancel checkpoint behavior, live best-effort semantics, and buffered
  event ordering during refactors.

## Record Maintenance

- Update `Recent Issues` and `Next Actions` when agent loop, message ownership,
  cancellation, streaming, or tool sequencing changes.
- Record unresolved agent-loop risks in root `FINDINGS.md`.
- Record landed agent fixes in root `FIX.md` with verification commands.
- For partial refactors, state residual ownership/timing risks explicitly in
  `FINDINGS.md`.
- Keep this file near 80 lines by replacing stale closed bullets with current
  risks.

## Verification

- Local: `cargo test -p lattice-agent`.
- Test-utils paths: `cargo test -p lattice-agent --features test-utils`.
- Streaming/tool changes: also run affected `lattice-api` turn and consistency
  tests.

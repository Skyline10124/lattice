# AGENTS.md

Crate-local guidance for `lattice-agent`.

## Role

This crate owns one provider attempt through `ModelStepDriver`: frozen request
assembly from typed conversation history, provider stream opening/consumption,
protocol ceiling enforcement, live model-step events, and typed attempt outcomes.
It owns no multi-turn loop, retry policy, Tool execution, InvocationId allocation,
store handle, reconciliation, or Turn commit.

## Sub-Architecture

- `src/model_step.rs` owns `LlmClient`, `ModelStepRequest`, `ModelStepDriver`,
  request projection, stream polling, cancellation/deadline enforcement, protocol
  metering, and typed attempt assembly.
- `src/testing.rs` provides feature-gated deterministic provider responses.
- `tests/streaming_seam.rs` covers the public injectable streaming seam.

## Boundaries

- Depend only on `lattice-llm` and the data-only `lattice-conversation` contract.
  Do not add `lattice-session`, LANCET runtime/native/audit, profile, or store edges.
- Do not allocate invocation identities, bind or dispatch Tools, reserve/finalize
  invocation records, retry provider attempts, reconcile PendingTurn, or commit a Turn.
  Those responsibilities belong to `lattice-api::TurnCoordinator`.
- Cancellation and deadline apply while opening and consuming the provider stream.
  Opening failures remain typed `ModelStepError::OpeningFailed`; in-flight
  cancellation/deadline/stream failures become typed `ModelStepAttempt` outcomes.
- Provider wire details remain in `lattice-llm`; this crate consumes typed events.
- Live event delivery is observational and must not become persistence authority.

## Recent Issues

- LANDED #139-B Commit 4/6: deleted `AgentLoop` and `ToolCallExecutor`; production
  now calls one-attempt `ModelStepDriver` from the durable Turn saga.
- FIXED #139-B review: provider stream opening now races the same cooperative
  cancellation/deadline poll as stream consumption.

## Verification

- Local: `cargo test -p lattice-agent`.
- Test utilities: `cargo test -p lattice-agent --features test-utils`.
- Request/outcome changes: also run affected `lattice-api` Turn tests and the
  pinned-binary `direct_failures` system harness.

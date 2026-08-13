# AGENTS.md

Crate-local guidance for `lattice-conversation`.

## Role

This crate owns the provider-neutral, data-only conversation contract from
ADR 0005 §7 (landed in GitHub #142):

- ordered `NativeMessage<ToolCallState>` / `MessagePart` values with the
  `UnboundToolCall` / `BoundToolCall` typestates (no `Option<InvocationId>`
  shortcut);
- the provider-neutral Tool result closure (`ToolResultOutcome` closed sum);
- semantic and replay digests (`digest.rs`) — two distinct digests that must
  never merge;
- per-attempt usage (`TokenUsage`), interruption, and rejection records;
- provider origin and bounded part-local opaque replay state;
- the `GraphMessage` / `GraphToolCall` / `HeadTranscript` transcript family
  migrated verbatim from `lattice-session` (serde shape frozen), re-exported
  there for persistence consumers;
- strict ToolCall batch validation and the canonical arguments digest
  (SHA-256 over domain separator + u64_be byte length + canonical bytes).

## Boundaries

- Depends only on `lancet-protocol` plus neutral data deps (`serde`,
  `serde_json`, `sha2`, `thiserror`). No gix, filesystem, store, lock,
  runtime, provider HTTP client, or ID generator — ever.
- The crate parses and validates but never generates `InvocationId` /
  `TurnId`; generation belongs to `lattice-api::TurnCoordinator` (#139-B).
- Tool arguments must be one JSON object; there is no empty-to-`{}`
  normalization. One invalid call rejects the whole batch.
- `semantic_digest` excludes provider origin, provider call IDs, and replay
  state; `replay_state_digest` covers exactly those. Provider metadata must
  never cross into the semantic digest.
- Replay state is preserved verbatim, bounded at 64 KiB per part, and never
  filtered out even on an otherwise-empty part.
- Per-attempt `Option<TokenUsage>`: missing is unknown, never zero.
- `BoundToolCall` deserialization is checked: `arguments_digest` is recomputed
  from `arguments` and must match, so a stored digest always describes its own
  arguments (`ArgumentsDigestMismatch` otherwise).
- `AttemptAssembly` is the ADR 0005 §10 byte accountant. Any code that
  assembles a complete attempt — including the `lattice-llm`
  `provider_projection` seam — must meter through it rather than assembling
  first and checking afterwards.

## Verification

- Local: `cargo test -p lattice-conversation`.
- Digest/construction changes: pin the golden fixtures in `src/digest.rs`
  tests deliberately — a changed golden value is a wire-breaking event.

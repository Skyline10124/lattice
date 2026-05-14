# Streaming

Runtime normalizes provider-specific streaming into `StreamEvent`.

## Events

Important event variants include:

- `Token`: visible assistant text.
- `Reasoning`: provider reasoning or thinking content when available.
- `ToolCallStart`: beginning of a streamed tool call.
- `ToolCallDelta`: streamed tool call argument data.
- `ToolCallEnd`: completion of a tool call.
- `Done`: terminal success event with optional usage.
- `Error`: terminal or recoverable stream error surface.

## Parsers

`lattice-core/src/streaming/` is split by provider:

- `openai.rs`
- `anthropic.rs`
- `gemini.rs`
- `mod.rs`

Provider parsers convert wire-level chunks into normalized runtime events. Limits are configurable, and parsers warn when input approaches configured limits.

## Transport Integration

`TransportDispatcher` selects the correct transport from `ResolvedModel.api_protocol`. Transports normalize request messages and denormalize provider responses back into runtime types.

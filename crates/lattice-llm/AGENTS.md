# AGENTS.md

Crate-local guidance for `lattice-llm`.

## Role

This crate owns LLM vocabulary and provider interaction: chat/stream request and
response types, tool-call types, provider adapters, SSE parsing, model routing,
endpoint metadata, URL/DNS security, request-time pinning, and logging setup.

## Sub-Architecture

- `src/types.rs` owns messages, tool calls, tool definitions, events, requests,
  responses, roles, targets, and token usage.
- `src/client.rs` owns HTTP client behavior and request-time security hooks.
- `src/providers/` owns provider-specific protocol adapters.
- `src/sse.rs` owns streaming parser behavior.
- `src/router.rs` and `src/endpoints.rs` own model alias/provider resolution.
- `src/security.rs` owns URL validation, DNS checks, SSRF defenses, and
  rebinding suffix checks.
- `src/stub.rs` owns local stub client behavior used by tests.
- `src/logging.rs` owns tracing/log initialization.

## Boundaries

- No session graph, profile discovery, agent-loop orchestration, or native
  LANCET execution here.
- Keep provider protocol quirks inside provider modules or the client layer.
- Higher layers should consume typed `LlmEvent`, `LlmError`, and model metadata.
- Preserve SSRF protections on every network-facing change.

## Recent Issues

- PARTIAL M-04: `validate_base_url` DNS checks are best-effort; authoritative
  protection is request-time resolved IP pinning in `client.rs`.
- PARTIAL M-05: `web_fetch`/external fetch rebinding defense relies on pinned IP
  plus a maintained denylist shared with native web behavior.
- FIXED L-02: provider JSON parsing no longer uses a sentinel error value for
  normal control flow.
- WATCH P22.5: strict OpenAI-compatible gateways may reject dotted tool names;
  wire adapters map `a.b` to `a__b` while CEP catalog remains dotted.
- WATCH: model routing and credential status are consumed by CLI/API surfaces.

## Next Actions

- Keep request-time DNS pinning as the primary SSRF defense; do not rely solely
  on configuration-time validation.
- Expand rebinding heuristics or suffix coverage only with tests and a note in
  `FINDINGS.md` if residual risk remains.
- Preserve dotted-name encoding/decoding as a provider wire detail, not a catalog
  contract change.
- When adding providers, include stream parsing, tool-call, usage, and error
  mapping tests.

## Record Maintenance

- Update `Recent Issues` and `Next Actions` when provider adapters, security,
  routing, SSE, or tool-call wire behavior changes.
- Record unresolved LLM/network risks in root `FINDINGS.md`.
- Record landed LLM fixes in root `FIX.md` with verification commands.
- Keep provider-specific notes local and concise; cross-crate policy/security
  residuals belong in `FINDINGS.md`.
- Keep this file near 80 lines by replacing stale bullets with current risks.

## Verification

- Local: `cargo test -p lattice-llm`.
- Provider/streaming changes: also run affected `lattice-agent` and
  `lattice-api` streaming tests.
- URL/DNS changes need explicit private IP, localhost, rebinding, redirect, and
  pinning coverage where applicable.

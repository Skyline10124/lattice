# AGENTS.md

Crate-local guidance for `lattice-llm`.

## Role

This crate owns LLM vocabulary and provider interaction: chat/stream request and
response types, tool-call types, provider adapters, SSE parsing, model routing,
endpoint metadata, request-time DNS pinning, and logging setup.

## Sub-Architecture

- `src/types.rs` owns messages, tool calls, tool definitions, events, requests,
  responses, roles, targets, and token usage.
- `src/client.rs` owns HTTP client behavior and request-time security hooks.
- `src/providers/` owns provider-specific protocol adapters.
- `src/provider_projection/` owns the pure-data provider replay/projection
  seam (#142): official wire JSON ↔ `lattice-conversation` `NativeMessage`,
  with official-wire fixtures in `tests/fixtures/projection/`. It is not
  wired into the production codecs until #139-B. `wire_to_native` meters every
  body through `lattice_conversation::AttemptAssembly`, so an over-limit wire
  fails closed with `ProjectionError::OutputLimitExceeded` instead of yielding
  a partial message. `project_cross_provider` drops replay state, provider
  call IDs, **and** reasoning parts: ADR 0005 §7 forbids replaying source
  reasoning into another provider.
- `src/sse.rs` owns streaming parser behavior.
- `src/router.rs` and `src/endpoints.rs` own model alias/provider resolution.
- `net-guard` owns shared URL validation, DNS checks, private-address
  rejection, and rebinding suffix checks; `client.rs` maps its neutral errors
  into `LlmError`.
- `src/stub.rs` owns local stub client behavior used by tests.
- `src/logging.rs` owns tracing/log initialization.

## Boundaries

- No session graph, profile discovery, agent-loop orchestration, or native
  LANCET execution here.
- Keep provider protocol quirks inside provider modules or the client layer.
- Higher layers should consume typed `LlmEvent`, `LlmError`, and model metadata.
- Preserve SSRF protections on every network-facing change.
- ADR 0005 makes provider Tool Call IDs optional opaque wire metadata, never
  LANCET invocation identity. Since GitHub #142 provider-neutral messages and
  bounded part-local replay state live in `lattice-conversation`; same-provider
  replay must preserve real IDs/signatures/part order, while cross-provider
  projection must not leak source-provider opaque state. Do not synthesize
  `tc_*` IDs as domain identity.

## Recent Issues

- PARTIAL M-04: `validate_base_url` DNS checks are best-effort; authoritative
  protection is request-time resolved IP pinning in `client.rs`.
- FIXED #90: `http_client_builder` sets `redirect::Policy::none()`. Pinning and
  URL validation bind the first hop only, so a followed redirect reached an
  unvalidated host and replayed the body plus `x-api-key`/`x-goog-api-key`.
  Do not re-enable redirects without re-running the net-guard checks per hop.
- PARTIAL M-05: `web_fetch`/external fetch rebinding defense relies on pinned IP
  plus the maintained `net-guard` denylist shared with native web behavior.
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

---
phase: 14
slug: agentloop-streaming-delta-engine
status: verified
threats_open: 0
asvs_level: 1
created: 2026-05-21
---

# Phase 14 — Security

> Per-phase security contract: threat register, accepted risks, and audit trail.

---

## Threat Register

**Phase:** 14 -- AgentLoop Streaming Delta Engine
**ASVS Level:** 1
**Audit Date:** 2026-05-21
**Threats Total:** 4 | **Closed:** 4 | **Open:** 0

## Threat Verification

| Threat ID | Category | Disposition | Evidence | Status |
|-----------|----------|-------------|----------|--------|
| T-14-01 | Tampering | mitigate | `serde_json::from_str::<serde_json::Value>` at `crates/lattice-agent/src/agent_loop/mod.rs:346`; all-or-nothing validation loop at `mod.rs:334-360`; test 5 (`streaming_invalid_tool_call_json_no_execution`) and test 7 (`streaming_multiple_tool_calls_one_invalid_none_executed`) at `crates/lattice-agent/tests/streaming_delta.rs:335-386,462-508` | CLOSED |
| T-14-02 | Denial of Service | accept | `MAX_TURN_ROUNDS = 200` caps round count at `mod.rs:28,140-154`; optional caller timeout at `mod.rs:95-114`; no per-stream deadline for single in-flight stream -- accepted per Phase 15/17 scope | CLOSED |
| T-14-03 | Information Disclosure | accept | Partial content preservation at `mod.rs:235-264` (Error) and `mod.rs:265-294` (Exhausted); content stays in session graph, no external leakage | CLOSED |
| T-14-SC | Tampering | accept | `serde_json = { workspace = true }` at `crates/lattice-agent/Cargo.toml:15`; mature crate already in workspace | CLOSED |

## Accepted Risks Log

| Threat ID | Risk | Acceptance Rationale | Scope Limitation |
|-----------|------|---------------------|-----------------|
| T-14-02 | A single in-flight LLM stream that never terminates can consume resources indefinitely. MAX_TURN_ROUNDS caps round count but not per-stream duration. | Caller timeout (`AgentTurnRequest.timeout`) provides optional per-turn deadline. Live cancellation/deadline semantics are Phase 15/17 scope. | Resolved when per-stream deadline/cancellation semantics are implemented in Phase 15 or 17. |
| T-14-03 | Partial assistant content (tokens, reasoning) is committed to the session graph when a stream encounters Error or Exhausted outcomes. | By design: partial content is internal to the session graph and is not leaked externally. The GraphMessage records what was received, enabling audit and recovery. | No external exposure path exists; content remains within the session boundary. |
| T-14-SC | serde_json crate introduces a supply-chain dependency. | serde_json is a 9+ year old crate with 200M+ downloads, maintained by the serde project, already in the workspace dependency tree. No new trust boundary introduced. | Already a workspace dependency; no additional risk surface. |

## Unregistered Flags

None. Neither 14-01-SUMMARY.md nor 14-02-SUMMARY.md contain a `## Threat Flags` section. No new attack surface was flagged during implementation.

## Trust Boundaries Verified

| Boundary | Verification |
|----------|-------------|
| LLM stream -> AgentLoop | Untrusted stream events are consumed by `consume_model_stream`; tool call arguments validated via serde_json before execution; malformed stream events (duplicate ids, delta/end without start) return StreamOutcome::Error |
| Tool call arguments -> ToolExecutor | JSON arguments validated all-or-nothing via `serde_json::from_str` at `mod.rs:346`; normalized empty arguments to "{}" at `mod.rs:340-344`; no tool execution occurs if any argument fails validation |
| Test doubles -> AgentLoop | FakeStreamingLlmClient provides controlled event sequences; ChatOnlyFakeLlm proves backward compatibility via default stream_chat fallback |

---

## Security Audit Trail

| Audit Date | Threats Total | Closed | Open | Run By |
|------------|---------------|--------|------|--------|
| 2026-05-21 | 4 | 4 | 0 | gsd-security-auditor |

---

## Sign-Off

- [x] All threats have a disposition (mitigate / accept / transfer)
- [x] Accepted risks documented in Accepted Risks Log
- [x] `threats_open: 0` confirmed
- [x] `status: verified` set in frontmatter

**Approval:** verified 2026-05-21
---
phase: 16
date: 2026-05-13
depth: standard
status: complete
files_reviewed: 12
findings_total: 16
findings_critical: 5
findings_warning: 6
findings_info: 5
---

# Phase 16 — Code Review

## Summary

| Severity | Count | Description |
|----------|-------|-------------|
| 🔴 Critical | 5 | Data loss, error/schema regressions, session isolation, assembly drift |
| 🟡 Warning | 6 | Correctness gaps, duplicate code, incomplete lifecycle wiring |
| 🔵 Info | 5 | Code quality, naming, maintenance concerns |

---

## 🔴 Critical

### CR-16-01: `latest_turn_completion` silently discards rich completion data

**File:** `crates/lattice-runtime/src/turn.rs`
**Lines:** ~95–108
**Severity:** Critical
**Category:** Data Loss

**Finding:**
`HandleState` stores a lean `handle::TurnCompletion` (4 fields: handle_id, status, assistant_text, model, provider). When `latest_turn_completion` reconstructs the public `turn::TurnCompletion` (13 fields), it fills token_usage=None, task_state_snapshot=TaskState::new(), final_report=None, turn_manifests=vec![], saved_session_id=None, error=None — discarding the actual values computed in `execute_turn`.

**Impact:** Callers querying completion get empty task state, no manifests, no token usage, no final report. The `RuntimeAgentTurnExecutor` in turn_executor.rs depends on this method and returns broken output (empty model/provider strings, no final report).

**Fix:**
1. Either store the full `turn::TurnCompletion` in `HandleState` instead of the lean `handle::TurnCompletion`, or
2. Keep both: lean for retention + full in a separate map keyed by TurnId.

### CR-16-12: Runtime pipeline turns agent errors into successful pipeline outputs

**Files:** `crates/lattice-runtime/src/turn_executor.rs`, `crates/lattice-orchestrator/src/agent_executor.rs`, `crates/lattice-orchestrator/src/pipeline.rs`
**Lines:** `turn_executor.rs` ~113-146, `agent_executor.rs` ~72-84, `pipeline.rs` ~275-281
**Severity:** Critical
**Category:** Correctness / Control Flow

**Finding:**
`RuntimeAgentTurnExecutor::run_turn` queries completion status, maps `TurnStatus::Error` to `AgentTurnStatus::Error`, then still returns `Ok(AgentTurnOutput { ... })`. `AgentExecutor::run_standard` and `Pipeline::run` treat any `Ok` from the turn executor as a successful agent result, route handoff rules against `json_output`, and emit normal completion events.

The old `AgentRunner::run_once` path returned `Err` for `LoopEvent::Error`, so this is a behavior regression in the new runtime-owned pipeline path.

**Impact:** Provider errors, hook errors, prompt compilation errors, and runtime turn errors can be recorded as successful pipeline steps. Skippable/non-skippable error routing is bypassed, and downstream agents may receive an empty/raw-text wrapper instead of a real failure.

**Fix:** In `RuntimeAgentTurnExecutor::run_turn`, return `AgentTurnExecutionError::Runtime` (or `Cancelled`) when completion status is `Error` or `Cancelled`, before parsing assistant text as successful output.

### CR-16-13: Runtime pipeline drops `output_schema` validation and retry semantics

**Files:** `crates/lattice-runtime/src/turn_executor.rs`, `crates/lattice-orchestrator/src/runner.rs`
**Lines:** `turn_executor.rs` ~120-146, `runner.rs` ~61-131 and ~215-237
**Severity:** Critical
**Category:** Behavioral Regression

**Finding:**
The legacy `AgentRunner` validates `profile.handoff.output_schema`, retries invalid JSON with correction hints, and errors when schema-bound agents return non-JSON or empty output. The new runtime-backed executor ignores `spec.handoff.output_schema` entirely and always falls back to `{"_raw_text": true, "content": ...}` when JSON parsing fails.

**Impact:** Any pipeline profile that relies on structured output can silently pass invalid data into handoff rules and downstream agents. This breaks a core orchestrator contract while Phase 16 switches standard pipeline execution to the runtime-owned path.

**Fix:** Move the schema validation/retry behavior into the shared `AgentTurnExecutor` path, or keep `AgentRunner`'s validation wrapper around runtime turn execution before returning `AgentTurnOutput`.

### CR-16-14: Runtime handle identity is not session-scoped

**Files:** `crates/lattice-runtime/src/lib.rs`, `crates/lattice-runtime/src/turn_executor.rs`, `crates/lattice-runtime/src/turn.rs`
**Lines:** `lib.rs` ~619-623, `turn_executor.rs` ~84-101, `turn.rs` ~78-81
**Severity:** Critical
**Category:** State Isolation / Data Leakage

**Finding:**
Phase 16's context and decisions require handle identity to be session-scoped. `get_or_create_agent_handle` hard-codes `.with_session_id(None)`, while `RuntimeAgentTurnExecutor` creates/reuses a handle before passing `request.session_id` into `submit_turn`. `submit_turn` then drops `session_id` entirely.

**Impact:** Two calls with the same profile/workspace/model/security but different session IDs will reuse the same live `Agent`, messages, task state, approvals, and recent completions. That leaks state across sessions and makes the public `session_id` field misleading.

**Fix:** Include session identity in the handle request/key before handle lookup, and pass it through turn execution/persistence. If session-less reuse is desired, make that an explicit separate mode.

### CR-16-15: Runtime internal assembly ignores PromptSpec and tool restrictions

**Files:** `crates/lattice-runtime/src/lib.rs`, `crates/lattice-orchestrator/src/assembly.rs`
**Lines:** `lib.rs` ~795-847, `assembly.rs` ~191-228 and ~258-353
**Severity:** Critical
**Category:** Security / Behavioral Regression

**Finding:**
`lower_profile_to_turn_spec` preserves `prompt_spec`, `tools.enabled`, behavior, and memory fields, but `Runtime::build_internal_agent` does not apply most of them. It ignores `spec.prompt_spec` entirely, so PromptSpec content, static sections, dynamic providers, and hooks disappear in the runtime path. It also resolves all capability tools and authorizes `capabilities.tool_names` without applying `spec.tools.enabled`, retired-tool checks, or custom registry tool handling from `orchestrator::assembly`.

**Impact:** Migrating a standard pipeline profile to runtime-owned execution can remove its system prompt/provider stack and simultaneously grant broader tool access than the profile requested. This is both a correctness regression and a security policy regression.

**Fix:** Reuse the existing profile assembly logic for prompt/tool resolution, or factor shared assembly helpers so runtime-owned handles preserve PromptSpec, tool filtering, registry tools, retired-tool rejection, and behavior/memory semantics.

---

## 🟡 Warning

### CR-16-02: `execute_turn` stores empty model/provider strings

**File:** `crates/lattice-runtime/src/turn.rs`
**Lines:** ~165
**Severity:** Warning
**Category:** Correctness

**Finding:**
`execute_turn` produces `TurnCompletion { model: String::new(), provider: String::new(), ... }`. The model and provider information is available in `HandleState.model` / `HandleState.model.provider` but never passed into the turn execution. The `agent.run_streaming()` callback doesn't have access to handle metadata.

**Fix:** Pass model/provider from the handle into `execute_turn` or fetch them from handle state after the turn completes.

### CR-16-03: Agent permanently lost if spawned task panics

**File:** `crates/lattice-runtime/src/turn.rs`
**Lines:** ~78–95
**Severity:** Warning
**Category:** Robustness

**Finding:**
`submit_turn` takes the agent out of `HandleState` with `.take()`, then spawns a `tokio::spawn` task. If the spawned task panics (e.g., `tx.send` failure causing unwrap, or `execute_turn` panic), the agent is never returned to the handle. The handle is left with `agent: None` and `active_turn: true` permanently. No timeout, no panic guard, no recovery.

**Fix:** Use `tokio::spawn` with `JoinHandle` + `catch_unwind`, or add a timeout mechanism that forces agent return. Add a test that verifies panic recovery.

### CR-16-04: `session_id` and `persist_session` silently dropped

**File:** `crates/lattice-runtime/src/turn.rs`
**Lines:** ~78–85
**Severity:** Warning
**Category:** Correctness

**Finding:**
`submit_turn` receives `request.session_id` and `request.persist_session` but the spawned tokio task closure only captures `input` and `max_turns`. The session/persistence fields are not passed to `execute_turn`. Sessions are never persisted through the runtime-owned path.

**Fix:** Pass `session_id`, `persist_session`, and `session_store` into the spawned task/`execute_turn`.

### CR-16-16: `Completed` stream events are emitted before the turn is terminal

**File:** `crates/lattice-runtime/src/turn.rs`
**Lines:** ~156-159
**Severity:** Warning
**Category:** Event Semantics

**Finding:**
`execute_turn` maps every `LoopEvent::Done` to `RuntimeTurnEvent::Completed`. `Agent::run_streaming` can emit `LoopEvent::Done` for intermediate tool-call model responses before tool execution continues, and `TurnEngine` also emits a final `Done` even after an earlier `LoopEvent::Error`.

**Impact:** Runtime event consumers can see `Completed` before later `ToolFinished`, `Token`, `Error`, or final lifecycle events. That violates the semantic runtime event contract and will make the Phase 17 CLI/TUI migration race-prone.

**Fix:** Defer `RuntimeTurnEvent::Completed` emission until `agent.run_streaming(...)` returns and the final completion status is known. Do not emit `Completed` after an error or cancellation.

### CR-16-05: Security cache key does not include allowlists

**Files:** `crates/lattice-runtime/src/turn_executor.rs`, `crates/lattice-runtime/src/lib.rs`
**Lines:** `turn_executor.rs` ~65–78, `lib.rs` ~617-623
**Severity:** Warning
**Category:** Security

**Finding:**
`RuntimeAgentTurnExecutor` caches `RuntimeSecurity` by `"workspace:sandbox_mode"` key. Two different `RuntimeSecurityConfig`s with the same `sandbox_mode` (`"project"`) but different `read_allowlist`, `command_allowlist`, or `audit` settings will incorrectly share the same `RuntimeSecurity` instance. `get_or_create_agent_handle` repeats the same pattern by deriving `AgentHandleKey.security_policy` from `security.mode_label` only.

This can lead to sandbox bypass if a caller configures strict allowlists but the cache or handle registry returns a permissive earlier-built security/executor.

**Fix:** Include all security-significant config fields in the cache key, or use a hash of the full `RuntimeSecurityConfig`.

### CR-16-06: Duplicate `looks_like_error` / `strip_markdown_fence` implementations

**File:** `crates/lattice-runtime/src/turn_executor.rs` (new) vs `crates/lattice-orchestrator/src/runner.rs` (existing)
**Severity:** Warning
**Category:** Code Quality / DRY

**Finding:**
`turn_executor.rs` copies `looks_like_error` (30+ lines) and `strip_markdown_fence` from `orchestrator/src/runner.rs`. These are identical implementations in two crates, creating a maintenance burden and risk of divergence.

**Fix:** Move `looks_like_error` and `strip_markdown_fence` to a shared location (e.g., `lattice-core` or a new `lattice-orchestrator/src/output.rs`). Phase 16's plan mentions creating `output.rs` for this purpose.

---

## 🔵 Info

### CR-16-07: Two `TurnCompletion` types with overlapping names

**Files:** `crates/lattice-runtime/src/handle.rs` (lean, 4 fields) + `crates/lattice-runtime/src/turn.rs` (rich, 13 fields)
**Severity:** Info
**Category:** Naming

**Finding:**
`handle::TurnCompletion` is marked as a "placeholder" but is actively used by `submit_turn` to store completions. Meanwhile `turn::TurnCompletion` is the public type. The duplication causes the data loss in CR-16-01. Consider consolidating into one type, or renaming the internal one to `HandleCompletionRecord`.

### CR-16-08: `build_internal_agent` partial coding preset application

**File:** `crates/lattice-runtime/src/lib.rs`
**Lines:** ~800–845
**Severity:** Info
**Category:** Completeness

**Finding:**
`build_internal_agent` applies a subset of the coding preset: system prompt + WorkspaceContextProvider. It does NOT replicate the full `apply_coding_preset` logic (ToolPreset::Coding capabilities, DefaultToolExecutor with sandbox, ProjectInstructionsProvider). This means handles built via `get_or_create_agent_handle` have different behavior than those built via the existing `build_agent` path.

**Fix:** Either call `preset::apply_coding_preset` from `build_internal_agent`, or ensure the spec carries enough information for full preset composition.

### CR-16-09: `ProfileLoweringError::InvalidPreset` never constructed

**File:** `crates/lattice-orchestrator/src/agent_spec.rs`
**Severity:** Info
**Category:** Dead Code

**Finding:**
The `InvalidPreset(String)` variant is defined but the lowering function explicitly comments "unknown presets are not errors at this level" and silently ignores them. Either remove the variant or implement preset validation.

### CR-16-10: Handle key not updated on rebuild

**File:** `crates/lattice-runtime/src/lib.rs` (rebuild_agent_handle)
**Severity:** Info
**Category:** Correctness edge case

**Finding:**
`rebuild_agent_handle` updates the internal `Agent`, model, and security but does NOT update the `AgentHandleKey`. If the rebuild changed the model or provider override, the old key remains in `by_key`, potentially preventing correct handle reuse for the new spec.

**Fix:** Recompute and update the key in `by_key` during rebuild.

### CR-16-11: `render_runtime` loses multi-call tool count

**File:** `crates/lattice-cli/src/coding_agent.rs`
**Severity:** Info
**Category:** UX

**Finding:**
The `LoopEvent::ToolCallRequired` handler prints `"N tool call(s)"` for concurrent calls. The `RuntimeTurnEvent::ToolStarted` handler only prints the tool name. When a model emits multiple concurrent tool calls, the count information is lost in the runtime path.

**Fix:** Include call count in `ToolStarted` or batch consecutive `ToolStarted` events in the renderer.

---

## Verification Commands

```sh
# Core compile check
cargo check -p lattice-runtime -p lattice-orchestrator -p lattice-cli

# Full test suite
cargo test -p lattice-runtime -p lattice-orchestrator -p lattice-cli

# Clippy
cargo clippy -p lattice-runtime -p lattice-orchestrator -p lattice-cli -- -D warnings

# Grep guard: no direct live Agent in TUI
rg -n 'Arc<Mutex<Agent>>' crates/lattice-cli/src/frontend/tui/app.rs
```

---

## Summary by File

| File | Lines Changed | Critical | Warning | Info | Notes |
|------|--------------|----------|---------|------|-------|
| `crates/lattice-orchestrator/src/agent_spec.rs` | +470 (new) | 0 | 0 | 1 | CR-16-09 |
| `crates/lattice-orchestrator/src/agent_executor.rs` | +60/-3 | 1 | 0 | 0 | CR-16-12 |
| `crates/lattice-orchestrator/src/assembly.rs` | +8 | 0 | 0 | 0 | Minor helper |
| `crates/lattice-orchestrator/src/lib.rs` | +6 | 0 | 0 | 0 | Re-exports |
| `crates/lattice-orchestrator/src/pipeline.rs` | +10 | 1 | 0 | 0 | CR-16-12 |
| `crates/lattice-runtime/src/handle.rs` | +250 (new) | 0 | 0 | 1 | CR-16-07 |
| `crates/lattice-runtime/src/lib.rs` | +450 | 2 | 1 | 2 | CR-16-14, CR-16-15, CR-16-05, CR-16-08, CR-16-10 |
| `crates/lattice-runtime/src/turn.rs` | +180 (new) | 2 | 4 | 0 | CR-16-01, CR-16-14, CR-16-02..04, CR-16-16 |
| `crates/lattice-runtime/src/turn_executor.rs` | +220 (new) | 2 | 2 | 0 | CR-16-12, CR-16-13, CR-16-05, CR-16-06 |
| `crates/lattice-cli/src/coding_agent.rs` | +45 | 0 | 0 | 1 | CR-16-11 |
| `crates/lattice-cli/src/frontend/tui/app.rs` | +15 | 0 | 0 | 0 | Bridge fields |
| `crates/lattice-cli/src/frontend/tui/agent_runner.rs` | +12 | 0 | 0 | 0 | Bridge fields |

---

## Assessment

The architectural direction (lower contract, handle registry, turn API) is promising, but the current runtime-owned path is not yet behaviorally equivalent to the direct `AgentRunner` path. The critical issues above make `latest_turn_completion` unusable for rich turn state, let pipeline errors/schema violations masquerade as successful outputs, break session-scoped handle isolation, and drop PromptSpec/tool restriction semantics.

The remaining warnings are correctness gaps in the initial implementation: missing model/provider info, missing session persistence wiring, premature terminal events, security cache keying, and duplicated utility code. These should be addressed before Phase 17 builds more frontend behavior on top of this API.

The TUI/CLI bridge additions are minimal and safe — they add optional fields with `#[allow(dead_code)]` and don't change existing behavior paths.

---
phase: 06-crate-topology-and-main-path-skeleton
verified: 2026-05-16T09:10:00Z
status: passed
score: 17/17 must-haves verified
overrides_applied: 0
overrides: []
gaps: []
---

# Phase 06: Crate Topology and Main-Path Skeleton Verification Report

**Phase Goal:** Establish the eight-crate target and a compiling skeleton for the simplified main path.
**Verified:** 2026-05-16T09:10:00Z
**Status:** passed
**Re-verification:** No -- initial verification

## Goal Achievement

### Observable Truths

| # | Truth | Status | Evidence |
|---|-------|--------|----------|
| 1 | Workspace manifests define target crate topology via staged additive path | VERIFIED | `Cargo.toml` includes `lattice-llm` and `lattice-api` in `[workspace].members` and `[workspace.dependencies]` without removing legacy crates (D-01 through D-05) |
| 2 | `lattice-llm` exists as active crate with no internal `lattice-*` dependencies | VERIFIED | `crates/lattice-llm/Cargo.toml` depends only on `serde`, `serde_json`, `thiserror`, `futures`. Cargo metadata confirms zero `lattice-*` deps. |
| 3 | `lattice-llm` defines direct v1.1 LLM vocabulary (Message, Role, ToolCall, ToolDefinition, ChatRequest, ChatResponse, LlmEvent, ModelRouter, ModelTarget) | VERIFIED | `crates/lattice-llm/src/types.rs` contains all 9 types as direct structs/enums with serde derives, no lattice-core aliases |
| 4 | `StubLlm` returns deterministic chat and stream responses without provider credentials | VERIFIED | `stub.rs` `chat()` returns "stub response: {last_user_msg}"; `stream()` yields [Started, Token("stub response: ..."), Done]. No env reads, no HTTP, no lattice-core imports. |
| 5 | `lattice-api` exists as active crate with narrow dependencies | VERIFIED | `crates/lattice-api/Cargo.toml` depends on `lattice-llm`, `serde`, `serde_json`, `thiserror`, `futures`, `uuid`. No legacy runtime/core/prompt/sandbox/policy deps. |
| 6 | `lattice-api` exposes `create_session`, `open_session`, `run_turn`, `stream_turn` | VERIFIED | `lib.rs` re-exports all four functions from `session` and `turn` modules |
| 7 | `run_turn` calls `lattice-llm` stub, returns assistant_output, records skeleton stage trace, sets commit_id: None | VERIFIED | `turn.rs` `run_turn()` calls `StubLlm::new().chat()`, returns stage trace [Profile, Session, Agent, Llm, Tools, Commit, Render], sets `commit_id: None` |
| 8 | `stream_turn` exposes Started/Token/Done event shape | VERIFIED | `turn.rs` `stream_turn()` maps `LlmEvent` variants to `TurnEvent::Started`, `TurnEvent::Token{content}`, `TurnEvent::Done` |
| 9 | Migration homes for `lattice-prompt`, `lattice-sandbox`, `lattice-policy` are explicit and tested | VERIFIED | `topology.rs` `migration_homes()` returns three `TopologyEntry` records with legacy crate names, v1.1 homes, and owning phases |
| 10 | New API public surface does not re-export old runtime/core types | VERIFIED | `rg` scan across `lattice-api/src/` finds zero matches for `pub use lattice_(runtime|core)`, `RuntimeAgentSession`, `RuntimeInvocationContext`, `GenericAgentSpec`, `RuntimeSecurity`, `SubmittedTurn`, `TurnCompletion`, `ProfileRuntimeSessionRequest` |
| 11 | User can run `cargo run -p lattice-cli -- run-v11 "hello"` and see stub output | VERIFIED | Smoke test prints `stub response: hello` to stdout |
| 12 | `run-v11` dispatches through `lattice_api::run_turn`, never calls legacy runtime | VERIFIED | `run_v11.rs` constructs `lattice_api::RunTurnRequest`, calls `lattice_api::run_turn`. Zero `lattice_runtime`, `lattice_core`, `model_runtime`, or `commands::run` imports. |
| 13 | Legacy `run` command remains registered and compiling | VERIFIED | `main.rs` `Commands::Run` variant and dispatch to `run::run` unchanged. `cargo check --workspace` passes for all legacy crates. |
| 14 | CLI `run-v11` parses as clap subcommand and tests pass without credentials | VERIFIED | `main.rs` `#[command(name = "run-v11")]` parses in test `run_v11_parses_prompt`. Handler tests run without any credential env vars. |
| 15 | `cargo check --workspace` succeeds | VERIFIED | `cargo check --workspace` exits clean with "Finished" (0 errors, 0 warnings) |
| 16 | All Phase 6 crate tests pass | VERIFIED | `lattice-llm`: 18/18, `lattice-api`: 15/15, `lattice-cli run_v11`: 9/9 |
| 17 | Forbidden boundary references absent across all new code | VERIFIED | `lattice-api -> lattice-llm` is the only internal dependency edge. Zero forbidden references in `lattice-llm/src/`, `lattice-api/src/`, `run_v11.rs` via combined rg scan. |

**Score:** 17/17 truths verified

### Required Artifacts

| Artifact | Expected | Status | Details |
|----------|----------|--------|---------|
| `Cargo.toml` | Workspace member + dep entry for lattice-llm and lattice-api | VERIFIED | Both crates in `[workspace].members` and `[workspace.dependencies]` |
| `crates/lattice-llm/Cargo.toml` | Standalone crate manifest, no lattice-* deps | VERIFIED | Exact required deps: serde, serde_json, thiserror, futures; tokio dev-dep only |
| `crates/lattice-llm/src/lib.rs` | Flat public LLM facade | VERIFIED | Exports `chat`, `stream`, `StubLlm`, all vocabulary types. No legacy imports. |
| `crates/lattice-llm/src/types.rs` | Direct v1.1 LLM vocabulary (9 types) | VERIFIED | `Role`, `Message`, `FunctionCall`, `ToolCall`, `ToolDefinition`, `ChatRequest`, `ChatResponse`, `LlmEvent`, `ModelTarget`, `ModelRouter` -- all direct structs/enums |
| `crates/lattice-llm/src/stub.rs` | Deterministic StubLlm with chat/stream | VERIFIED | `StubLlm`, `LlmError`, `LlmStream`, `chat()`, `stream()`. Returns "stub response: {input}". Stream yields Started/Token/Done. |
| `crates/lattice-api/Cargo.toml` | Standalone crate manifest, only lattice-llm internal dep | VERIFIED | Dependencies: lattice-llm, serde, serde_json, thiserror, futures, uuid. No legacy deps. |
| `crates/lattice-api/src/lib.rs` | Flat public v1.1 API facade | VERIFIED | Re-exports create/open/run/stream functions and TopologyEntry |
| `crates/lattice-api/src/session.rs` | Session-shaped API target types | VERIFIED | `CreateSessionRequest`, `OpenSessionRequest`, `ApiSession`, `create_session()`, `open_session()`, `ApiError` |
| `crates/lattice-api/src/turn.rs` | Run/stream turn skeleton | VERIFIED | `RunTurnRequest`, `StreamTurnRequest`, `RunTurnResult`, `RunTurnStage`, `TurnEvent`, `TurnEventStream`, `run_turn()`, `stream_turn()` |
| `crates/lattice-api/src/topology.rs` | Migration-home table | VERIFIED | `TopologyEntry`, `migration_homes()`. Entries for prompt/sandbox/policy with v1.1 homes and phases. |
| `crates/lattice-cli/Cargo.toml` | CLI depends on lattice-api | VERIFIED | `lattice-api = { workspace = true }` present alongside legacy deps |
| `crates/lattice-cli/src/main.rs` | Clap registration + dispatch for RunV11 | VERIFIED | `Commands::RunV11{prompt:String}` with `#[command(name = "run-v11")]`. Dispatches to `run_v11::run(prompt)`. Parser test passes. |
| `crates/lattice-cli/src/commands/mod.rs` | Command module registration | VERIFIED | `pub mod run_v11;` present |
| `crates/lattice-cli/src/commands/run_v11.rs` | Isolated v1.1 skeleton command handler | VERIFIED | Constructs `RunTurnRequest`, calls `lattice_api::run_turn`, renders `assistant_output`. No legacy imports. |


### Key Link Verification

| From | To | Via | Status | Details |
|------|----|-----|--------|---------|
| `Cargo.toml [workspace]` | `crates/lattice-llm/Cargo.toml` | Workspace member + dependency path | WIRED | Both `members` and `workspace.dependencies` entries present |
| `Cargo.toml [workspace]` | `crates/lattice-api/Cargo.toml` | Workspace member + dependency path | WIRED | Both `members` and `workspace.dependencies` entries present |
| `lattice-llm/src/lib.rs` | `types.rs`, `stub.rs` | `pub use` module exports | WIRED | `mod stub; mod types; pub use stub::{...}; pub use types::{...};` |
| `lattice-llm/src/stub.rs` | `lattice-llm/src/types.rs` | ChatRequest/ChatResponse/LlmEvent usage | WIRED | `use crate::types::{ChatRequest, ChatResponse, LlmEvent, ModelRouter};` |
| `lattice-api/src/lib.rs` | `session.rs`, `turn.rs`, `topology.rs` | `pub use` module exports | WIRED | Three `pub use` blocks for all public API items |
| `lattice-api/src/turn.rs` | `lattice-llm/src/lib.rs` | `lattice_llm::StubLlm`, `ChatRequest`, `Message`, `ModelTarget` | WIRED | `use lattice_llm::{ChatRequest, LlmEvent, Message, ModelTarget, StubLlm};` Called in `run_turn()` and `stream_turn()`. |
| `lattice-api/src/topology.rs` | Phase 9/11 migration boundaries | Migration home table | WIRED | Three entries: prompt (Phase 7/9), sandbox (Phase 9), policy (Phase 9/11) |
| `lattice-cli/src/main.rs Commands::RunV11` | `lattice-cli/src/commands/run_v11.rs` | Match dispatch | WIRED | `Commands::RunV11 { prompt } => { run_v11::run(prompt).await?; }` |
| `lattice-cli/src/commands/run_v11.rs` | `lattice-api/src/turn.rs` | `RunTurnRequest`, `run_turn` | WIRED | `lattice_api::RunTurnRequest{...}`, `lattice_api::run_turn(request).await?` |
| `cargo run -p lattice-cli -- run-v11 "hello"` | `lattice-api::run_turn -> lattice-llm stub` | CLI smoke path | WIRED | Outputs "stub response: hello" from `StubLlm` through `lattice-api` |

### Data-Flow Trace (Level 4)

| Artifact | Data Variable | Source | Produces Real Data | Status |
|----------|--------------|--------|--------------------|--------|
| `lattice-llm/src/stub.rs:chat()` | `ChatResponse.content` | `format!("stub response: {}", last_user_msg)` | N/A (intentional stub) | VERIFIED -- deterministic stub data per D-18/D-19 |
| `lattice-llm/src/stub.rs:stream()` | `LlmStream` (events) | `vec![Started, Token{content}, Done]` | N/A (intentional stub) | VERIFIED -- deterministic stub stream per D-17/D-18 |
| `lattice-api/src/turn.rs:run_turn()` | `RunTurnResult` | `StubLlm::new().chat(chat_request)` | Flows from stub | VERIFIED -- stub response flows through API to caller |
| `lattice-api/src/turn.rs:stream_turn()` | `TurnEventStream` | `StubLlm::new().stream(chat_request)` mapped to TurnEvent | Flows from stub | VERIFIED -- LlmEvent-to-TurnEvent mapping complete |
| `lattice-cli/src/commands/run_v11.rs:run()` | stdout output | `lattice_api::run_turn(request).await?.assistant_output` | Flows from API | VERIFIED -- end-to-end chain: CLI -> API -> LLM stub -> stdout |

All wired artifacts with dynamic data flow correctly through the skeleton path. The data source is the deterministic `StubLlm` -- this is the intended Phase 6 behavior. Real providers come in Phase 7.

### Behavioral Spot-Checks

| Behavior | Command | Result | Status |
|----------|---------|--------|--------|
| Workspace compiles cleanly | `cargo check --workspace` | "Finished" (0 errors, 0 warnings) | PASS |
| lattice-llm tests pass | `cargo test -p lattice-llm` | 18/18 passed | PASS |
| lattice-api tests pass | `cargo test -p lattice-api` | 15/15 passed | PASS |
| run-v11 tests pass | `cargo test -p lattice-cli run_v11` | 9/9 passed | PASS |
| Smoke: run-v11 prints stub output | `cargo run -p lattice-cli -- run-v11 "hello"` | "stub response: hello" | PASS |

### Requirements Coverage

| Requirement | Source Plan | Description | Status | Evidence |
|-------------|-------------|-------------|--------|----------|
| SIMP-01 | 06-02, 06-03 | Developer can trace run flow as explicit chain: profile->session->agent->llm->tools->commit->render | SATISFIED | `RunTurnStage` enum in `turn.rs` defines the 7-stage ordered trace; `run_turn()` records them in order; tests assert ordered trace; `run-v11` exercises the full chain |
| CRATE-01 | 06-01, 06-02 | Workspace active members limited to eight target crates | SATISFIED (Phase 6 scope) | `lattice-llm` and `lattice-api` added additively. Legacy crates remain compiling. Full member limitation requires Phase 11 deletion (CRATE-05). |
| CRATE-02 | 06-01 | `lattice-core` replaced by `lattice-llm` for v1.1 LLM vocabulary | SATISFIED (Phase 6 scope) | `lattice-llm` defines all LLM types directly in `types.rs`. `lattice-core` still compiles for legacy path. Full replacement completed by Phase 7/11. |
| CRATE-03 | 06-02, 06-03 | `lattice-runtime` replaced by `lattice-api` as thin public facade | SATISFIED (Phase 6 scope) | `lattice-api` exposes `create_session`, `open_session`, `run_turn`, `stream_turn`. `lattice-runtime` still compiles for legacy path. Full replacement by Phase 9/11. |
| CRATE-04 | 06-02 | `lattice-sandbox`, `lattice-policy`, `lattice-prompt` removed or merged into narrower modules | SATISFIED (Phase 6 scope) | `topology.rs` records migration homes for all three with v1.1 destinations and owning phases. Actual migration/code-move in Phase 9/11. |

### Anti-Patterns Found

No anti-patterns detected in any Phase 6 files:
- Zero `TBD`, `FIXME`, or `XXX` markers
- Zero `TODO`, `HACK`, or `PLACEHOLDER` comments
- Zero empty implementations (`return null`, `return {}`, `return []`)
- Zero `console.log`-only implementations
- Zero hardcoded empty data that flows to rendering without an alternative code path

The known stubs documented in SUMMARY.md (`StubLlm`, `ModelRouter::resolve`, `run_turn` skeleton stages, `commit_id: None`) are intentional Phase 6 architectural stubs documented with explicit phase references for future work (Phase 7/8/9), not unmanaged debt.

### Human Verification Required

None. All must-have truths are verifiable through automated checks (compilation, test execution, boundary grep, cargo metadata, and CLI smoke output). This phase is purely structural -- no visual appearance, real-time behavior, or external service integration to verify.

---

_Verified: 2026-05-16T09:10:00Z_
_Verifier: Claude (gsd-verifier)_

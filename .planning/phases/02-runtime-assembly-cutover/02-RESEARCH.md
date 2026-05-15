# Phase 2: Runtime Assembly Cutover - Research

**Researched:** 2026-05-14 [VERIFIED: date command]
**Domain:** Rust runtime assembly, session-owned turn transactions, sandbox/tool binding, and public API removal [VERIFIED: .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md]
**Confidence:** HIGH for current codebase shape, MEDIUM for exact final API names [VERIFIED: crates/lattice-runtime/src/lib.rs; crates/lattice-session/src/lib.rs; crates/lattice-profile/src/spec.rs]

<user_constraints>
## User Constraints (from CONTEXT.md)

### Locked Decisions
## Implementation Decisions

### Runtime Spec Boundary

- **D-01:** Runtime directly consumes `lattice_profile::GenericAgentSpec`. Do not introduce a public `RuntimeAgentSpec`, and do not keep `lattice_orchestrator::AgentTurnSpec` as a runtime assembly input.
- **D-02:** Keep exactly one main execution chain: `agent.toml -> lattice-profile -> GenericAgentSpec -> runtime assembly -> turn execution -> session commit`. Do not build compatibility bridges, dual spec paths, or transitional public APIs.
- **D-03:** `GenericAgentSpec` remains pure profile data. Runtime invocation context such as `cwd`, `project_root`, session selector, and effective runtime security is supplied separately by a `RuntimeInvocationContext`-style input.
- **D-04:** Phase 2 deletes old runtime assembly/spec entry points once disconnected. It should not preserve dead code for legacy profile/orchestrator assembly paths.
- **D-05:** Runtime public API should have explicit generic entry points that separate agent/session setup from turn submission. Do not hide profile/spec inside `TurnRequest`, and do not make a one-shot API the only primary path.
- **D-06:** Profile discovery/loading/lowering belongs to `lattice-profile`, not frontend code and not runtime core. CLI/TUI/callers may choose an `AgentSource` such as default/name/path, but must call `lattice-profile` for source resolution and lowering. Runtime consumes the resulting `GenericAgentSpec` or `ProfileSnapshot`.

### Session-Owned Agent State

- **D-07:** Delete the live-agent-handle chain as the runtime main path. Runtime must not use a long-lived `Agent` object to own conversation messages, task state, approvals, or session continuity.
- **D-08:** Session is the only owner of conversation state. Messages, turn manifests, branch/head state, restore, history, and audit records belong to `lattice-session`.
- **D-09:** Agent profile is execution configuration for a turn/session, not a state owner. A profile does not own history and a session does not belong to a profile.
- **D-10:** Runtime acts as a stateless-ish assembler/executor. It may construct transient per-turn execution objects internally, but these objects must not become cross-turn state containers.
- **D-11:** Remove unused live-handle compatibility logic and tests. Do not keep handle key matching, handle rebuild, plugin overlay, or recent-completion cache semantics as generic runtime requirements.

### Profile Snapshot State Machine

- **D-12:** A session binds to an immutable profile snapshot when the session starts. The snapshot is derived from `GenericAgentSpec` and must be durable enough to keep running the session even if the source profile file changes or disappears.
- **D-13:** A profile file changing on disk must not silently affect an existing session. This is a security boundary: an injected or auto-updated prompt/profile must not change the behavior of an in-progress session.
- **D-14:** When a session already has a bound profile snapshot, later turns use that locked snapshot by default.
- **D-15:** Explicit profile switching must create an auditable boundary, such as starting a new session or an explicit future rebind/fork operation. Do not implement implicit profile switching.
- **D-16:** If runtime/caller detects that the profile source changed on disk, default behavior is to continue using the session-bound snapshot and surface a warning or debug/audit record. The warning should be equivalent to: `profile changed on disk; current session remains locked to snapshot <hash>`.

### Runtime Extension API Cleanup

- **D-17:** Phase 2 should aggressively remove runtime public APIs for pipeline, micro-agent, event bus, plugin registry, plugin overlay, and runtime-backed orchestrator executor concepts.
- **D-18:** `lattice-runtime` must no longer act as a facade for orchestrator/plugin systems. Its public surface should center on model resolution, profile snapshot binding, prompt/tool/sandbox assembly, turn execution, and session persistence.
- **D-19:** Full crate/workspace deletion is still Phase 4 scope, but runtime should cut direct dependencies on `lattice-orchestrator` and plugin crates in Phase 2 if possible.
- **D-20:** Removed runtime extension APIs should be accompanied by deletion or rewrite of their tests. Do not keep compatibility shims just to satisfy old tests.

### Clean Session API

- **D-21:** Do not continue patching the old runtime `save_with_manifests` flow as the primary design. Define a clean, simple, session-owned turn transaction API.
- **D-22:** The session API should make profile snapshot binding explicit. New sessions bind a `ProfileSnapshot`; existing sessions return their locked snapshot.
- **D-23:** The session API should make turn transaction boundaries explicit. Runtime opens a turn against a session/head, executes using returned history and profile snapshot, then commits a status-bearing turn result.
- **D-24:** Error and cancellation outcomes are auditable turn outcomes. `Completed`, `Error`, and `Cancelled` should all be representable in manifests unless execution failed before any user-visible or model-visible turn boundary existed.
- **D-25:** Turn manifests must record profile snapshot identity. At minimum, store a stable profile hash and a readable summary: profile name/source label/model/tools/security/context/behavior.
- **D-26:** Do not store secrets in session metadata or manifests. Invocation summaries may record safe fields such as cwd/project root/security mode, but must not persist API keys or credential material.
- **D-27:** Keep the API small. Avoid migration machinery, branch policy expansion, extension hooks, or plugin-oriented manifest fields in Phase 2 unless required to compile and preserve existing session graph behavior.

### Suggested API Shape

- **D-28:** `lattice-session` should expose a `ProfileSnapshot` or equivalent durable record. It should include a stable hash, source audit record, canonical spec record, and capture timestamp. The hash should be computed from stable canonical profile/spec content, not from timestamps.
- **D-29:** `open_turn`-style semantics should return a turn id, session id, base commit id, turn index, locked profile snapshot, profile drift information, session history messages, and parent manifest ids.
- **D-30:** `commit_turn`-style semantics should accept turn id, session id, base commit id, profile hash, status, new messages, and a manifest draft. It should validate that the base commit has not changed and that the profile hash matches the session-bound snapshot.
- **D-31:** Runtime events should not require `AgentHandleId`. Prefer `session_id + turn_id` identity for the new generic path.

### Verification Gate

- **D-32:** Phase 2 verification uses a targeted-plus-workspace-check gate:
  - `cargo test -p lattice-session`
  - `cargo test -p lattice-profile`
  - `cargo test -p lattice-runtime`
  - targeted CLI/runtime adapter tests affected by the new API
  - `cargo check --workspace`
  - `cargo fmt --all --check`
- **D-33:** Full `cargo test --workspace` and `cargo clippy --workspace -- -D warnings` are desirable but not the minimum Phase 2 gate. If run and failing, classify failures as Phase 2 regressions, pre-existing failures, or obsolete legacy tests.

### the agent's Discretion

- The planner may choose exact Rust type and method names, as long as the semantic boundaries above hold.
- The planner may decide whether the profile snapshot record lives directly in `lattice-session/src/lib.rs` initially or in a new module, as long as the public API stays small and reviewable.
- The planner may preserve existing CAS object internals where useful, but the public API should read as session-owned turn transactions rather than runtime-owned handle persistence.

### Deferred Ideas (OUT OF SCOPE)
## Deferred Ideas

- Full CLI/TUI user-facing command cleanup, including final command names and TUI presentation of profile drift, belongs primarily to Phase 3.
- Full workspace member deletion for `lattice-orchestrator`, `lattice-plugin`, and `lattice-plugins` belongs to Phase 4, though runtime should cut dependencies earlier if practical.
- Explicit profile rebind/fork UX is a future feature unless required for tests. Phase 2 should define the safe default and audit boundary, not a full switching workflow.
</user_constraints>

<phase_requirements>
## Phase Requirements

| ID | Description | Research Support |
|----|-------------|------------------|
| RUN-01 | Runtime can construct and reuse one generic agent handle from profile/spec, resolved model, prompt providers, tool capabilities, sandbox policy, memory/context, and optional session seed. | Replace the old handle-owned continuity model with session-owned `open_turn` plus transient runtime assembly from `GenericAgentSpec`, resolved model, prompt providers, tool capabilities, `RuntimeSecurity`, history seed, and task-state seed if preserved. [VERIFIED: .planning/REQUIREMENTS.md; crates/lattice-profile/src/spec.rs; crates/lattice-runtime/src/lib.rs; crates/lattice-agent/src/config.rs] |
| RUN-02 | Runtime tool execution remains bound to `RuntimeSecurity` sandbox and policy settings after plugin/orchestrator assembly is removed. | Build `DefaultToolExecutor` directly in runtime with `.with_sandbox(security.sandbox.clone())` and `.with_authorized_tools(capabilities.tool_names.clone())`; this is the existing secure binding used outside orchestrator assembly. [VERIFIED: .planning/REQUIREMENTS.md; crates/lattice-tools/src/executor.rs; crates/lattice-runtime/src/lib.rs] |
| RUN-03 | Runtime turn completion persists session messages and turn manifests through the existing session store path. | Preserve manifest CAS semantics but replace runtime `save_completed_turn`/`save_with_manifests` fallback with a session-owned `commit_turn` API that validates base commit and profile hash. [VERIFIED: .planning/REQUIREMENTS.md; crates/lattice-runtime/src/turn.rs; crates/lattice-session/src/lib.rs] |
| RUN-04 | Runtime public APIs no longer expose pipeline, micro-agent, event bus, plugin registry, plugin directory loading, plugin overlay, or runtime-backed orchestrator executor concepts. | Delete runtime exports and builder/runtime methods for `Pipeline`, `MicroAgent`, `EventBus`, `PluginRegistry`, `RuntimePluginOverlay`, `RuntimeAgentTurnExecutor`, `load_plugin_dir`, `watch_plugin_dir`, `orchestrator_config`, and orchestrator-backed registries. [VERIFIED: .planning/REQUIREMENTS.md; crates/lattice-runtime/src/lib.rs; crates/lattice-runtime/src/handle.rs; crates/lattice-runtime/src/turn_executor.rs] |
</phase_requirements>

## Project Constraints (from AGENTS.md)

- Use the documented Rust verification commands: `cargo build`, `cargo test`, `cargo clippy -- -D warnings`, `cargo fmt --all --check`, targeted `cargo test <test_name>`, and `cargo bench` when benchmarks are relevant. [VERIFIED: AGENTS.md]
- Preserve the workspace dependency direction `hooks -> core -> session/prompt/sandbox/tools -> agent -> plugin -> orchestrator -> runtime -> cli` until the milestone removes the extension crates in later phases. [VERIFIED: AGENTS.md]
- Treat `Runtime` / `RuntimeBuilder`, `ResolvedModel`, `StreamEvent`, `Agent`, `Pipeline`, `Plugin`, `EventBus`, and `SandboxConfig` as key existing concepts, but Phase 2 must remove runtime exposure of the extension-layer concepts locked out by `02-CONTEXT.md`. [VERIFIED: AGENTS.md; .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md]
- The project supports provider credentials through environment/config and only set keys are used; Phase 2 must not persist credential material in profile snapshots, turn metadata, or manifests. [VERIFIED: AGENTS.md; crates/lattice-cli/src/credentials.rs; .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md]
- No project-defined `.codex/skills/` or `.agents/skills/` directories were found in this checkout. [VERIFIED: find .codex/skills .agents/skills -maxdepth 2 -name SKILL.md]

## Summary

Phase 2 is an internal architecture cutover, not an ecosystem/library selection problem. The useful Phase 1 input already exists: `lattice-profile` owns strict `agent.toml` loading/discovery/defaults and lowers loaded profiles into a pure `GenericAgentSpec` that intentionally carries no cwd, project root, session store, live agent, orchestrator, plugin, or CLI state. [VERIFIED: .planning/phases/01-profile-contract-extraction/01-01-SUMMARY.md; .planning/phases/01-profile-contract-extraction/01-02-SUMMARY.md; crates/lattice-profile/src/spec.rs; crates/lattice-profile/src/lowering.rs]

The main planning move is to invert runtime continuity ownership. Current runtime stores live `Agent` objects behind `AgentHandleId`, keys them by session/workspace/profile/model/security, rebuilds handles while carrying messages/task state forward, emits events with `AgentHandleId`, and saves successful turns through `save_with_manifests`; Phase 2 should instead make `lattice-session` bind a durable profile snapshot, open a turn transaction, hand runtime the locked snapshot plus history, and commit a status-bearing result with manifests. [VERIFIED: crates/lattice-runtime/src/handle.rs; crates/lattice-runtime/src/turn.rs; crates/lattice-session/src/lib.rs; .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md]

Runtime should assemble a transient `lattice_agent::Agent` directly from `GenericAgentSpec`, `ResolvedModel`, prompt providers, resolved tool capabilities, `DefaultToolExecutor`, `RuntimeSecurity`, memory/context providers, session history, and optional task-state seed; it should not call `lattice_orchestrator::assembly` or keep runtime public APIs for pipeline/plugin/micro-agent/event-bus surfaces. [VERIFIED: crates/lattice-agent/src/config.rs; crates/lattice-tools/src/capabilities.rs; crates/lattice-tools/src/executor.rs; crates/lattice-runtime/src/lib.rs; .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md]

**Primary recommendation:** Plan this as three dependency-safe cuts: add session-owned `ProfileSnapshot/open_turn/commit_turn`, add runtime-owned generic assembly and turn submission using `GenericAgentSpec`, then delete runtime orchestrator/plugin public surfaces and rewrite affected tests/adapters. [VERIFIED: .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md; crates/lattice-runtime/src/lib.rs; crates/lattice-session/src/lib.rs]

## Architectural Responsibility Map

| Capability | Primary Tier | Secondary Tier | Rationale |
|------------|--------------|----------------|-----------|
| Profile discovery/loading/lowering | Profile crate (`lattice-profile`) | CLI/TUI choose `ProfileSource` | Phase 1 created `ProfileSource`, `DiscoveryPaths`, `resolve_agent_profile`, and `lower_profile_to_spec`; runtime must consume the resulting spec/snapshot rather than duplicate discovery. [VERIFIED: crates/lattice-profile/src/discovery.rs; crates/lattice-profile/src/lowering.rs; .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md] |
| Profile snapshot binding | Session crate (`lattice-session`) | Runtime supplies initial spec/source audit | The locked decision says sessions bind immutable profile snapshots and later turns use the locked snapshot by default. [VERIFIED: .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md] |
| Turn transaction ownership | Session crate (`lattice-session`) | Runtime executes between open and commit | Session owns messages, branch/head, restore, history, audit records, and turn manifests. [VERIFIED: .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md; crates/lattice-session/src/lib.rs] |
| Runtime assembly | Runtime crate (`lattice-runtime`) | Agent/prompt/tools/sandbox crates provide primitives | Runtime already owns model resolution, security construction, tool registry injection, memory injection, and session store injection. [VERIFIED: crates/lattice-runtime/src/lib.rs] |
| Agent loop execution | Agent crate (`lattice-agent`) | Runtime constructs transient agent | `Agent::new`, `with_tools`, `with_tool_executor`, `with_provider`, `with_task_state_provider`, `seed_messages`, and `run_streaming` are existing agent loop APIs. [VERIFIED: crates/lattice-agent/src/config.rs; crates/lattice-agent/src/run.rs] |
| Tool exposure | Tools crate (`lattice-tools`) | Runtime maps profile string bundles into `ToolBundle` | `CapabilitySelection` exposes bundle plus enabled-tool union and `resolve_capabilities` returns definitions and authorized names. [VERIFIED: crates/lattice-tools/src/capabilities.rs] |
| Tool execution policy | Runtime + Sandbox/Policy/Tools crates | Agent calls executor | `RuntimeSecurity` builds `SandboxConfig` plus shared approvals; `DefaultToolExecutor` enforces sandbox and authorized tool names. [VERIFIED: crates/lattice-runtime/src/lib.rs; crates/lattice-tools/src/executor.rs; crates/lattice-sandbox/src/config.rs] |
| Public event identity | Runtime API | CLI/TUI adapters consume | New generic events should prefer `session_id + turn_id` over `AgentHandleId` per Phase 2 D-31. [VERIFIED: .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md] |
| CLI/TUI UX cleanup | CLI/TUI | Runtime API compatibility fixes only in Phase 2 | Full command/TUI cleanup belongs to Phase 3, while Phase 2 may do narrow adapter fixes needed to compile. [VERIFIED: .planning/ROADMAP.md; .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md] |

## Standard Stack

### Core

| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| `lattice-profile` | 0.1.0 | Strict profile schema, discovery, default template, and pure `GenericAgentSpec` lowering. | Phase 1 created it specifically to own profile concerns below runtime. [VERIFIED: cargo metadata; crates/lattice-profile/src/lib.rs; .planning/phases/01-profile-contract-extraction/01-02-SUMMARY.md] |
| `lattice-session` | 0.1.0 | Git-style session graph, messages, branch/head refs, SQLite CAS store, turn manifests, and manifest reachability. | Phase 2 decisions make session the owner of conversation state and turn transactions. [VERIFIED: cargo metadata; crates/lattice-session/src/lib.rs; .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md] |
| `lattice-runtime` | 0.1.0 | Application-level assembly, model resolution, runtime security, turn streaming, and session persistence boundary. | Runtime is the top-level composition layer and Phase 2 target. [VERIFIED: cargo metadata; AGENTS.md; crates/lattice-runtime/src/lib.rs] |
| `lattice-agent` | 0.1.0 | Generic multi-turn agent loop, prompt lifecycle, tool execution events, task state, and turn manifests. | Runtime should construct transient agents rather than making agent own durable session state. [VERIFIED: cargo metadata; crates/lattice-agent/src/lib.rs; crates/lattice-agent/src/run.rs] |
| `lattice-tools` | 0.1.0 | Built-in tool definitions, capability bundles, enabled-tool union, default executor, and registry-backed custom tools. | It already provides bundle/explicit tool resolution after Phase 1 preset removal. [VERIFIED: cargo metadata; crates/lattice-tools/src/capabilities.rs; .planning/phases/01-profile-contract-extraction/01-profile-contract-extraction-03-SUMMARY.md] |
| `lattice-sandbox` + `lattice-policy` | 0.1.0 | Path/command/URL policy, dynamic approvals, denial reports, and risk classes. | Runtime security uses these as the enforcement substrate for tool execution. [VERIFIED: cargo metadata; crates/lattice-runtime/src/lib.rs; crates/lattice-sandbox/src/config.rs] |
| `lattice-prompt` | 0.1.0 | Project instructions, workspace context, memory providers, tool schema provider pattern, and prompt compilation. | Runtime assembly must wire prompt providers directly instead of using orchestrator assembly. [VERIFIED: cargo metadata; crates/lattice-runtime/src/lib.rs; .planning/codebase/ARCHITECTURE.md] |

### Supporting

| Library | Version | Purpose | When to Use |
|---------|---------|---------|-------------|
| `serde` / `serde_json` | workspace `1` | Serialize profile snapshots, manifest metadata, and session records. | Use for canonical snapshot records and safe manifest summaries. [VERIFIED: Cargo.toml; crates/lattice-session/Cargo.toml; crates/lattice-profile/Cargo.toml] |
| `sha2` | workspace `0.10` | Existing session manifest CAS identity uses SHA-256 over stable JSON payloads. | Use for stable profile snapshot hash if staying consistent with session CAS hashing. [VERIFIED: Cargo.toml; crates/lattice-session/src/lib.rs] |
| `uuid` | workspace `1` | Existing sessions and runtime turns use UUID-style simple ids. | Use for turn ids only if the planner keeps random ids rather than deterministic transaction ids. [VERIFIED: Cargo.toml; crates/lattice-session/src/lib.rs; crates/lattice-runtime/src/turn.rs] |
| `rusqlite` | workspace `0.32` with `bundled` | SQLite CAS session store. | Use for `ProfileSnapshot` schema and transaction-aware open/commit logic. [VERIFIED: Cargo.toml; crates/lattice-session/Cargo.toml; crates/lattice-session/src/lib.rs] |
| `tokio` + `futures` | workspace `tokio 1`, `futures 0.3` | Async turn execution and runtime event stream. | Use for preserving `RuntimeTurnEvent` stream semantics while changing event identity. [VERIFIED: Cargo.toml; crates/lattice-runtime/src/turn.rs] |
| `thiserror` | workspace `2` | Library error enums. | Use for new session transaction/profile snapshot errors. [VERIFIED: Cargo.toml; .planning/codebase/CONVENTIONS.md] |

### Alternatives Considered

| Instead of | Could Use | Tradeoff |
|------------|-----------|----------|
| Add a public `RuntimeAgentSpec` | Keep all runtime inputs in a new runtime-owned spec type | Rejected by D-01; it would create a second spec path instead of directly consuming `GenericAgentSpec`. [VERIFIED: .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md] |
| Keep `AgentHandleId` as session continuity | Reuse existing live handle registry with profile hash in the key | Rejected by D-07 through D-11; live handles currently carry messages/task state/recent completions and conflict with session-owned state. [VERIFIED: .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md; crates/lattice-runtime/src/handle.rs] |
| Continue patching `save_with_manifests` from runtime | Add more metadata to current `save_completed_turn` | Rejected by D-21 through D-30; the needed invariant is a session-owned transaction that validates base commit and profile hash. [VERIFIED: .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md; crates/lattice-runtime/src/turn.rs] |
| Keep runtime `Pipeline`/plugin facade until Phase 4 | Leave compile shims in runtime public API | Rejected by D-17 through D-20; Phase 4 deletes crates, but Phase 2 should cut runtime direct dependencies first if practical. [VERIFIED: .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md; crates/lattice-runtime/Cargo.toml] |

**Installation:**
```bash
# No new external install is recommended for Phase 2.
# Runtime should add the existing workspace crate and remove old extension deps:
# crates/lattice-runtime/Cargo.toml: add lattice-profile = { workspace = true }
# crates/lattice-runtime/Cargo.toml: remove lattice-orchestrator and lattice-plugin if compile allows.
```

**Version verification:** Local package versions were verified with `cargo metadata --no-deps --format-version 1`; external workspace dependency requirements were verified in root `Cargo.toml`; no npm registry lookup applies to this Rust workspace. [VERIFIED: cargo metadata; Cargo.toml]

## Architecture Patterns

### System Architecture Diagram

```text
CLI/TUI/Profile caller
  -> lattice-profile resolve_agent_profile + lower_profile_to_spec
  -> GenericAgentSpec + RuntimeInvocationContext
  -> lattice-session open_turn(spec/source audit, session selector)
       |-- new session: bind ProfileSnapshot(hash, canonical spec, source audit)
       |-- existing session: return locked ProfileSnapshot and drift info
       v
     OpenTurn { session_id, turn_id, base_commit_id, turn_index, history, parent_manifest_ids, locked_snapshot }
  -> lattice-runtime transient assembly
       |-- resolve model through ModelRouter
       |-- resolve tools through lattice-tools CapabilitySelection
       |-- build DefaultToolExecutor with RuntimeSecurity sandbox
       |-- add prompt providers from GenericAgentSpec context toggles
       |-- seed Agent with session history/task state
       v
     lattice-agent run_streaming
       |-- emits tokens, reasoning, tool events, manifests
       |-- tools execute through sandbox-bound executor
       v
     RuntimeTurnCompletion { session_id, turn_id, status, messages, manifests, profile_hash }
  -> lattice-session commit_turn(base_commit_id, profile_hash, status, messages, manifest draft)
       |-- validate head/base commit
       |-- validate profile hash
       |-- write manifests and commit in one transaction
       v
     durable session commit + manifest references
```

This flow intentionally has no `lattice_orchestrator::assembly`, no plugin registry, no event bus, no pipeline, and no long-lived live-agent handle as the continuity owner. [VERIFIED: .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md; crates/lattice-runtime/src/lib.rs; crates/lattice-session/src/lib.rs]

### Recommended Project Structure

```text
crates/lattice-session/src/
├── lib.rs                  # acceptable initial location for small ProfileSnapshot/open_turn/commit_turn API
└── turn_tx.rs              # recommended split only if lib.rs growth blocks review

crates/lattice-runtime/src/
├── lib.rs                  # Runtime, RuntimeBuilder, security, model/session/tool entrypoints
├── turn.rs                 # generic session/turn event stream and completion
├── assembly.rs             # recommended new private module for GenericAgentSpec -> transient Agent
└── handle.rs               # delete or shrink to non-continuity compatibility only if still needed by adapters
```

Keep the first implementation small; the context explicitly allows the planner to keep session types in `lattice-session/src/lib.rs` if that is more reviewable. [VERIFIED: .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md]

### Pattern 1: Durable Profile Snapshot Before Execution

**What:** Convert `GenericAgentSpec` into a stable session-owned snapshot before a turn starts, hash only canonical spec/source audit content, and exclude capture timestamp and secrets from the hash. [VERIFIED: .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md; crates/lattice-profile/src/spec.rs]

**When to use:** Use for every new session and any first turn where the session has no locked profile snapshot. [VERIFIED: .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md]

**Example:**
```rust
// Source: derived from Phase 2 D-22/D-28 and existing GenericAgentSpec fields.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProfileSnapshot {
    pub hash: String,
    pub source_label: String,
    pub captured_at: String,
    pub spec: ProfileSnapshotSpec,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProfileSnapshotSpec {
    pub profile_name: String,
    pub model: String,
    pub description: Option<String>,
    pub system_prompt_hash: String,
    pub tools: Vec<String>,
    pub security_mode: String,
    pub project_instructions: bool,
    pub workspace_context: bool,
    pub max_turns: Option<u32>,
    pub reasoning_effort: Option<String>,
}
```

### Pattern 2: Open/Commit Turn Transaction

**What:** `open_turn` returns the locked snapshot, current head/base commit, history, parent manifest ids, and drift information; `commit_turn` validates base commit and profile hash before writing messages and manifests. [VERIFIED: .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md]

**When to use:** Use around every runtime turn; avoid `load -> mutate Session -> save` as the primary path. [VERIFIED: crates/lattice-runtime/src/turn.rs; .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md]

**Example:**
```rust
// Source: derived from Phase 2 D-29/D-30 and existing SessionStore commit/manifest APIs.
let opened = session_store.open_turn(OpenTurnRequest {
    session_selector,
    initial_profile_snapshot: Some(snapshot),
    current_profile_hash: Some(current_hash),
})?;

let completion = runtime.execute_open_turn(&opened, invocation).await?;

session_store.commit_turn(CommitTurnRequest {
    turn_id: opened.turn_id,
    session_id: opened.session_id,
    base_commit_id: opened.base_commit_id,
    profile_hash: opened.profile_snapshot.hash.clone(),
    status: completion.status,
    new_messages: completion.messages,
    manifest: completion.manifest,
})?;
```

### Pattern 3: Direct Generic Runtime Assembly

**What:** Build `Agent` directly from `GenericAgentSpec` and invocation context with existing agent, prompt, tools, and sandbox APIs. [VERIFIED: crates/lattice-agent/src/config.rs; crates/lattice-tools/src/capabilities.rs; crates/lattice-tools/src/executor.rs]

**When to use:** Use for the new runtime generic path; delete calls to `lattice_orchestrator::assembly::assemble_agent*` from runtime. [VERIFIED: crates/lattice-runtime/src/lib.rs; .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md]

**Example:**
```rust
// Source: existing Agent, CapabilitySelection, RuntimeSecurity, and DefaultToolExecutor APIs.
let resolved = self.resolve_with_provider(&spec.model, invocation.provider_override.as_deref())?;
let capabilities = resolve_capabilities(&runtime_capability_selection(&spec.tools)?);
let project_root = invocation.project_root.clone();

let executor = DefaultToolExecutor::new(project_root.to_string_lossy().as_ref())
    .map_err(RuntimeError::Config)?
    .with_sandbox(invocation.security.sandbox.clone())
    .with_authorized_tools(capabilities.tool_names.clone());

let mut agent = lattice_agent::Agent::new(resolved)
    .with_tools(capabilities.definitions)
    .with_tool_executor(Box::new(executor))
    .with_task_state_provider()
    .with_thinking_effort(spec.behavior.reasoning_effort.clone());

agent.set_system_prompt(&spec.system_prompt);
agent.seed_messages(opened.history_messages);
```

### Pattern 4: Profile Context Toggles Control Providers

**What:** `GenericAgentSpec.context.project_instructions` and `workspace_context` should gate provider registration rather than always adding both providers. [VERIFIED: crates/lattice-profile/src/spec.rs; crates/lattice-runtime/src/lib.rs]

**When to use:** Use during runtime transient assembly so profile context toggles have observable behavior. [VERIFIED: crates/lattice-profile/src/lowering.rs]

**Example:**
```rust
// Source: existing ProjectInstructionsProvider and WorkspaceContextProvider usage.
if spec.context.project_instructions {
    agent = agent.with_provider(lattice_prompt::ProjectInstructionsProvider::new(
        invocation.project_root.clone(),
        invocation.cwd.clone(),
    ));
}
if spec.context.workspace_context {
    agent = agent.with_provider(lattice_prompt::WorkspaceContextProvider::new(
        invocation.project_root.clone(),
        invocation.cwd.clone(),
    ));
}
```

### Anti-Patterns to Avoid

- **Compatibility bridge through `AgentTurnSpec`:** This preserves the exact old runtime/orchestrator dependency Phase 2 exists to remove. [VERIFIED: .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md; crates/lattice-runtime/src/lib.rs]
- **Runtime handle owns history:** Existing `HandleState` stores `Agent`, `spec`, `security`, active state, plugin overlay, and recent completions; keeping it as the main path violates session-owned state decisions. [VERIFIED: crates/lattice-runtime/src/handle.rs; .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md]
- **Profile drift silently reloads:** Existing profile discovery can load changed files, but Phase 2 requires existing sessions to keep the locked snapshot by default. [VERIFIED: crates/lattice-profile/src/discovery.rs; .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md]
- **Manifest fallback to `store.save()` on unsupported manifests:** Current runtime falls back to `save()` when `save_with_manifests` is not implemented; Phase 2 should make manifest-capable session transactions explicit for the runtime path. [VERIFIED: crates/lattice-runtime/src/turn.rs; .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md]
- **Plugin-overlay reset behavior as generic behavior:** Existing runtime overlay code mutates prompt/tools/executor around plugin mode; Phase 2 should delete it rather than reinterpret it as generic runtime state. [VERIFIED: crates/lattice-runtime/src/lib.rs; crates/lattice-runtime/src/handle.rs]

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| Profile discovery and default lookup | New runtime profile resolver | `lattice_profile::resolve_agent_profile` and `lower_profile_to_spec` | Phase 1 already encoded explicit path, named project-first/global-second, and default-agent behavior. [VERIFIED: crates/lattice-profile/src/discovery.rs; .planning/phases/01-profile-contract-extraction/01-02-SUMMARY.md] |
| Tool bundle and enabled-tool union | Manual string expansion in runtime | `lattice_tools::CapabilitySelection` and `resolve_capabilities` | Phase 1 removed presets and established deterministic bundle plus enabled-tool resolution. [VERIFIED: crates/lattice-tools/src/capabilities.rs; .planning/phases/01-profile-contract-extraction/01-profile-contract-extraction-03-SUMMARY.md] |
| Sandbox path/command/tool authorization | Ad hoc checks in runtime assembly | `RuntimeSecurity`, `SandboxConfig`, `PolicyAuthority`, `DefaultToolExecutor` | Existing code shares approvals between runtime security and sandbox and returns typed denials. [VERIFIED: crates/lattice-runtime/src/lib.rs; crates/lattice-tools/src/executor.rs; crates/lattice-sandbox/src/checks.rs] |
| Agent loop and tool-call orchestration | New runtime executor loop | `lattice_agent::Agent::run_streaming` | Existing agent loop already emits `LoopEvent` token/reasoning/tool/manifest/error/done events. [VERIFIED: crates/lattice-agent/src/run.rs; crates/lattice-agent/src/manifest.rs] |
| Prompt project/workspace providers | String concatenation in runtime | `ProjectInstructionsProvider`, `WorkspaceContextProvider`, memory/task providers | Existing prompt provider model carries layered context and manifest metadata. [VERIFIED: crates/lattice-agent/src/config.rs; .planning/codebase/ARCHITECTURE.md] |
| Turn manifest CAS identity | New hash format unrelated to session store | `lattice-session` stable JSON and `sha256("turn_manifest\\0" || compact_json(payload))` pattern | Existing manifest IDs deliberately ignore reverse-index `commit_id` and are reachable from commits for GC. [VERIFIED: crates/lattice-session/src/lib.rs] |
| Session branch/head persistence | Runtime-owned session index | `lattice-session` refs, commits, reflog, and SQLite CAS store | Session already owns current/head/branch/commit/message storage. [VERIFIED: crates/lattice-session/src/lib.rs] |

**Key insight:** The hard part is not constructing an `Agent`; the hard part is preserving security and persistence invariants while deleting the old handle/orchestrator continuity owner. [VERIFIED: crates/lattice-runtime/src/handle.rs; crates/lattice-runtime/src/turn.rs; .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md]

## Runtime State Inventory

| Category | Items Found | Action Required |
|----------|-------------|-----------------|
| Stored data | `SqliteCasSessionStore` stores `session.db` with `sessions`, `refs`, `objects`, `commits`, `turn_manifests`, `reflog`, `stash`, and `artifacts`; the current schema has no profile snapshot table/column. [VERIFIED: crates/lattice-session/src/lib.rs] | Add durable profile snapshot storage plus migration/backfill semantics for existing DBs that lack snapshots. [VERIFIED: .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md] |
| Stored data | `turn_manifest_ids_json` was already added with an `ALTER TABLE` migration for older commits tables. [VERIFIED: crates/lattice-session/src/lib.rs] | Follow the same explicit migration style for any new profile snapshot fields; `CREATE TABLE IF NOT EXISTS` alone is not enough for existing DBs. [VERIFIED: crates/lattice-session/src/lib.rs] |
| Stored data | Current runtime `save_completed_turn` creates or loads a `Session`, appends user/assistant messages, converts agent manifests, and calls `save_with_manifests` only for completed turns. [VERIFIED: crates/lattice-runtime/src/turn.rs] | Replace this runtime-owned save helper with `open_turn`/`commit_turn`; represent `Completed`, `Error`, and `Cancelled` where a visible turn boundary exists. [VERIFIED: .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md] |
| Live service config | No external live service configuration is part of Phase 2 scope; searches found local files and code paths, not UI-hosted service config needing patching. [VERIFIED: rg profile_snapshot/open_turn/commit_turn/systemd/launchd/pm2/datadog/cloudflare/tailscale] | None for live services; do not plan Datadog/Cloudflare/Tailscale/n8n-style migration work for this phase. [VERIFIED: .planning/ROADMAP.md] |
| OS-registered state | No OS-registered runtime state was identified for the runtime assembly cutover. [VERIFIED: rg systemd/launchd/Task Scheduler/pm2] | None. [VERIFIED: rg systemd/launchd/Task Scheduler/pm2] |
| Secrets/env vars | Provider credential env vars and config keys exist in CLI/core docs/code, but Phase 2 decisions forbid storing secrets in session metadata or manifests. [VERIFIED: crates/lattice-cli/src/credentials.rs; crates/lattice-core/src/router.rs; .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md] | Snapshot/manifest summaries may record safe labels such as model, profile name, source label, tool names, security mode, cwd/project root, and provider id; do not persist API key values. [VERIFIED: .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md] |
| Build artifacts | Cargo dependency graph currently has `lattice-runtime` depending on `lattice-orchestrator` and `lattice-plugin`, and `blob-store` forwards to `lattice-orchestrator/blob-store`. [VERIFIED: crates/lattice-runtime/Cargo.toml] | Update `crates/lattice-runtime/Cargo.toml` and `Cargo.lock`; run `cargo check --workspace` to refresh build artifacts. [VERIFIED: .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md; crates/lattice-runtime/Cargo.toml] |

## Common Pitfalls

### Pitfall 1: Keeping Live Handles as Hidden Session State
**What goes wrong:** The code compiles but messages/task state/recent completions still live in `HandleState`, so session restore and profile snapshot semantics are not authoritative. [VERIFIED: crates/lattice-runtime/src/handle.rs]
**Why it happens:** The existing API centers on `get_or_create_agent_handle_for_session_seeded`, `rebuild_agent_handle`, `latest_turn_completion`, and `AgentHandleId` events. [VERIFIED: crates/lattice-runtime/src/lib.rs; crates/lattice-runtime/src/turn.rs]
**How to avoid:** Make the new runtime path accept `GenericAgentSpec`/snapshot plus invocation context, open a session turn, build a transient `Agent`, then commit through session. [VERIFIED: .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md]
**Warning signs:** New tests still call `Runtime::get_or_create_agent_handle*`, `agent_handle_matches`, `rebuild_agent_handle`, or inspect recent completions. [VERIFIED: crates/lattice-runtime/src/lib.rs]

### Pitfall 2: Retaining Orchestrator Types in Runtime Under New Names
**What goes wrong:** `lattice-runtime` still depends on `lattice-orchestrator` through `AgentTurnSpec`, `AgentRegistry`, `Pipeline`, `EventBus`, `MicroAgent`, or `RuntimeAgentTurnExecutor`. [VERIFIED: crates/lattice-runtime/src/lib.rs; crates/lattice-runtime/src/turn_executor.rs]
**Why it happens:** Current runtime uses orchestrator assembly and public facades throughout `lib.rs`. [VERIFIED: crates/lattice-runtime/src/lib.rs]
**How to avoid:** Add a boundary grep gate for `lattice_orchestrator`, `Pipeline`, `MicroAgent`, `EventBus`, `AgentRegistry`, and `RuntimeAgentTurnExecutor` under `crates/lattice-runtime/src` after generic assembly lands. [VERIFIED: rg crates/lattice-runtime/src]
**Warning signs:** `crates/lattice-runtime/Cargo.toml` still lists `lattice-orchestrator`, or runtime `blob-store` still forwards to `lattice-orchestrator/blob-store`. [VERIFIED: crates/lattice-runtime/Cargo.toml]

### Pitfall 3: Tool Schema Exposure and Execution Authorization Drift Apart
**What goes wrong:** The model sees one tool set but `DefaultToolExecutor` authorizes another set, causing confusing denials or over-broad execution. [VERIFIED: crates/lattice-tools/src/capabilities.rs; crates/lattice-tools/src/executor.rs]
**Why it happens:** `resolve_capabilities` returns both definitions and `tool_names`; runtime must use the same resolved value for `Agent::with_tools` and executor authorization. [VERIFIED: crates/lattice-tools/src/capabilities.rs]
**How to avoid:** Resolve capabilities once from profile tools, pass `definitions` to the agent, and pass `tool_names` to `.with_authorized_tools`. [VERIFIED: crates/lattice-tools/src/capabilities.rs; crates/lattice-tools/src/executor.rs]
**Warning signs:** Tests only assert visible tool definitions and never attempt a denied tool or sandbox-bound execution. [VERIFIED: crates/lattice-tools/src/executor.rs]

### Pitfall 4: Profile Snapshot Hash Includes Unstable or Secret Fields
**What goes wrong:** Existing sessions drift because timestamps/cwd/API keys affect snapshot identity, or secrets leak into session metadata. [VERIFIED: .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md]
**Why it happens:** It is tempting to serialize the whole runtime invocation or `ResolvedModel`/credentials instead of a safe canonical profile summary. [VERIFIED: crates/lattice-core/src/router.rs; crates/lattice-cli/src/credentials.rs]
**How to avoid:** Hash canonical profile/spec content only; store safe readable summary fields; exclude timestamps from hash and exclude credential material entirely. [VERIFIED: .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md]
**Warning signs:** Snapshot JSON contains `api_key`, provider credential maps, capture timestamp in hash payload, or raw environment values. [VERIFIED: crates/lattice-cli/src/credentials.rs; .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md]

### Pitfall 5: Error/Cancel Turns Disappear from Audit Trail
**What goes wrong:** Runtime emits an error/cancel stream event but does not persist an auditable turn outcome. [VERIFIED: crates/lattice-runtime/src/turn.rs]
**Why it happens:** Current `execute_turn` persists only when status is `Completed` and `persist_session` is true. [VERIFIED: crates/lattice-runtime/src/turn.rs]
**How to avoid:** Model `Completed`, `Error`, and `Cancelled` in the session `commit_turn` request; only skip commit when execution failed before any user-visible/model-visible boundary. [VERIFIED: .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md]
**Warning signs:** Tests assert failed turns return an event but never check session log/manifests. [VERIFIED: crates/lattice-runtime/src/turn.rs]

### Pitfall 6: Phase 3 UX Cleanup Sneaks Into Phase 2
**What goes wrong:** Runtime cutover gets blocked by broad CLI/TUI command/menu/plugin-mode deletion that belongs to Phase 3. [VERIFIED: .planning/ROADMAP.md; .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md]
**Why it happens:** CLI/TUI currently import runtime handle and plugin-overlay surfaces, so compilation fixes can tempt broad UX redesign. [VERIFIED: crates/lattice-cli/src/frontend/tui/agent_runner.rs; crates/lattice-cli/src/frontend/tui/plugin_mode.rs]
**How to avoid:** In Phase 2, do only adapter/test changes needed to compile and prove runtime behavior; leave command tree and TUI presentation cleanup to Phase 3. [VERIFIED: .planning/ROADMAP.md]
**Warning signs:** A Phase 2 plan includes `/plugins` menu redesign, final command naming, or full `code` UX removal instead of narrow runtime API adapter work. [VERIFIED: .planning/ROADMAP.md]

## Code Examples

Verified patterns from current source and locked decisions:

### Convert Profile Tools to Runtime Capabilities

```rust
// Source: crates/lattice-profile/src/spec.rs + crates/lattice-tools/src/capabilities.rs
fn runtime_capability_selection(
    tools: &lattice_profile::ToolSelectionSpec,
) -> Result<lattice_tools::CapabilitySelection, RuntimeError> {
    let bundles = tools
        .bundles
        .iter()
        .map(|bundle| bundle.parse::<lattice_tools::ToolBundle>())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| RuntimeError::Config(err.to_string()))?;

    Ok(lattice_tools::CapabilitySelection {
        bundles,
        enabled: tools.enabled.clone(),
    })
}
```

### Bind Tool Execution to RuntimeSecurity

```rust
// Source: crates/lattice-runtime/src/lib.rs + crates/lattice-tools/src/executor.rs
let capabilities = lattice_tools::resolve_capabilities(&selection);
let executor = lattice_tools::DefaultToolExecutor::new(project_root)
    .map_err(RuntimeError::Config)?
    .with_sandbox(security.sandbox.clone())
    .with_authorized_tools(capabilities.tool_names.clone());
agent = agent
    .with_tools(capabilities.definitions)
    .with_tool_executor(Box::new(executor));
```

### Commit Manifest Metadata With Profile Identity

```rust
// Source: crates/lattice-session/src/lib.rs metadata_json + Phase 2 D-25/D-26.
let metadata = serde_json::json!({
    "profile_snapshot": {
        "hash": opened.profile_snapshot.hash,
        "name": opened.profile_snapshot.spec.profile_name,
        "source": opened.profile_snapshot.source_label,
        "model": opened.profile_snapshot.spec.model,
        "tools": opened.profile_snapshot.spec.tools,
        "security_mode": opened.profile_snapshot.spec.security_mode,
        "context": {
            "project_instructions": opened.profile_snapshot.spec.project_instructions,
            "workspace_context": opened.profile_snapshot.spec.workspace_context
        }
    },
    "turn_status": completion.status.as_str()
});
```

### Boundary Grep Gate

```bash
# Source: Phase 2 D-17 through D-20 plus current runtime imports.
rg -n "lattice_orchestrator|lattice_plugin|Pipeline|MicroAgent|EventBus|PluginRegistry|RuntimePluginOverlay|RuntimeAgentTurnExecutor" crates/lattice-runtime
```

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| Orchestrator-owned profile schema and lowering | `lattice-profile` strict v1 schema, discovery, defaults, and pure `GenericAgentSpec` lowering | Phase 1 on 2026-05-14 | Runtime should consume `GenericAgentSpec` directly. [VERIFIED: .planning/phases/01-profile-contract-extraction/01-01-SUMMARY.md; .planning/phases/01-profile-contract-extraction/01-02-SUMMARY.md] |
| Tool presets such as coding/reader/minimal | Bundle plus explicitly enabled tool names | Phase 1 Plan 03 on 2026-05-14 | Runtime must not use preset-driven assembly or typed coding behavior. [VERIFIED: .planning/phases/01-profile-contract-extraction/01-profile-contract-extraction-03-SUMMARY.md; crates/lattice-tools/src/capabilities.rs] |
| Runtime `AgentTurnSpec`/handle path | Session-owned snapshot and transient runtime assembly | Planned Phase 2 | Planner should replace current runtime handle APIs rather than expand them. [VERIFIED: .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md; crates/lattice-runtime/src/lib.rs] |
| Runtime facade for pipeline/plugin/event-bus APIs | Runtime public surface centered on model/profile/session/tool/sandbox/turn | Planned Phase 2 | Runtime direct dependencies on `lattice-orchestrator` and `lattice-plugin` should be cut before Phase 4 workspace deletion if practical. [VERIFIED: .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md; crates/lattice-runtime/Cargo.toml] |
| Runtime `save_with_manifests` fallback | Explicit session `open_turn`/`commit_turn` | Planned Phase 2 | Session API should own transaction boundaries, base commit validation, profile hash validation, and manifest persistence. [VERIFIED: .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md; crates/lattice-session/src/lib.rs] |

**Deprecated/outdated:**
- Runtime `assemble_agent`, `run_agent`, `run_agent_events`, `run_pipeline`, `pipeline`, `micro_agent`, `dry_run_pipeline`, `event_bus`, `plugin_registry`, `load_plugin_dir`, `watch_plugin_dir`, and `RuntimeAgentTurnExecutor` are obsolete for Phase 2 runtime public API. [VERIFIED: crates/lattice-runtime/src/lib.rs; crates/lattice-runtime/src/turn_executor.rs; .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md]
- Runtime `RuntimePluginOverlay` and related set/apply overlay behavior are obsolete for the generic runtime path. [VERIFIED: crates/lattice-runtime/src/handle.rs; crates/lattice-runtime/src/lib.rs; .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md]
- Runtime `AgentSpec::capability_preset("coding")` compatibility method remains in current code/tests and should not be carried into the generic Phase 2 API. [VERIFIED: crates/lattice-runtime/src/lib.rs; crates/lattice-runtime/src/preset.rs; .planning/phases/01-profile-contract-extraction/01-05-SUMMARY.md]

## Assumptions Log

| # | Claim | Section | Risk if Wrong |
|---|-------|---------|---------------|
| - | No unverified `[ASSUMED]` claims were intentionally used in this research. | All sections | N/A. [VERIFIED: source review in this file] |

## Open Questions (RESOLVED)

1. **Should the initial session transaction API live in `lib.rs` or a new `turn_tx.rs` module?** [VERIFIED: .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md]
   - RESOLVED: Use a new `crates/lattice-session/src/turn_tx.rs` module for the new profile snapshot and open/commit transaction types, with `lib.rs` exporting the public API and implementing the `SessionStore` methods. This keeps the already-large session root reviewable while preserving a small public surface. [VERIFIED: .planning/phases/02-runtime-assembly-cutover/02-01-PLAN.md]

2. **Should file-backed `FileSessionStore` implement profile snapshot/open/commit or return unsupported?** [VERIFIED: crates/lattice-session/src/lib.rs]
   - RESOLVED: Implement the new transaction API for `SqliteCasSessionStore` first and leave the `SessionStore` default implementation as an explicit unsupported error for stores that do not provide manifest-capable transactions. Add narrower `FileSessionStore` behavior only if compile-critical call sites or targeted tests prove it is still needed. [VERIFIED: .planning/phases/02-runtime-assembly-cutover/02-01-PLAN.md]

3. **How much CLI/TUI adapter work is allowed in Phase 2?** [VERIFIED: .planning/ROADMAP.md]
   - RESOLVED: Phase 2 includes only compile-critical CLI/TUI adapter changes required to consume the new generic runtime session/turn API. Broad command-tree, menu, plugin label, and user-facing UX cleanup remains Phase 3. [VERIFIED: .planning/phases/02-runtime-assembly-cutover/02-04-PLAN.md; .planning/phases/02-runtime-assembly-cutover/02-05-PLAN.md]

## Environment Availability

| Dependency | Required By | Available | Version | Fallback |
|------------|-------------|-----------|---------|----------|
| `cargo` | Rust build/test/check gates | yes | `cargo 1.95.0 (f2d3ce0bd 2026-03-21)` | None needed. [VERIFIED: command -v cargo && cargo --version] |
| `rustc` | Rust compiler/MSRV sanity | yes | `rustc 1.95.0 (59807616e 2026-04-14)` | None needed. [VERIFIED: command -v rustc && rustc --version] |
| `rustfmt` | Formatting gate | yes | `rustfmt 1.9.0` | None needed. [VERIFIED: command -v rustfmt && rustfmt --version] |
| `rg` | Boundary and code search gates | yes | `ripgrep 15.1.0` | Use `grep` only if unavailable. [VERIFIED: command -v rg && rg --version] |
| `git` | Commit/status/diff workflow | yes | `git version 2.54.0` | None needed. [VERIFIED: command -v git && git --version] |
| SQLite via `rusqlite` bundled | Session transaction tests | yes as Cargo dependency | workspace `rusqlite = 0.32` with `bundled` | None needed. [VERIFIED: Cargo.toml; crates/lattice-session/Cargo.toml] |

**Missing dependencies with no fallback:**
- None identified for Phase 2 planning and local verification. [VERIFIED: command -v cargo/rustc/rustfmt/rg/git]

**Missing dependencies with fallback:**
- None identified. [VERIFIED: command -v cargo/rustc/rustfmt/rg/git]

## Validation Architecture

### Test Framework

| Property | Value |
|----------|-------|
| Framework | Rust built-in `cargo test` with inline crate tests and crate integration tests. [VERIFIED: cargo metadata; .planning/codebase/CONVENTIONS.md] |
| Config file | None dedicated; root `Cargo.toml` workspace settings and crate `Cargo.toml` files drive tests. [VERIFIED: Cargo.toml; cargo metadata] |
| Quick run command | `cargo test -p lattice-session && cargo test -p lattice-profile && cargo test -p lattice-runtime` [VERIFIED: .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md] |
| Full suite command | `cargo check --workspace && cargo fmt --all --check`; full `cargo test --workspace` and `cargo clippy --workspace -- -D warnings` are desirable but not minimum Phase 2 gate. [VERIFIED: .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md] |

### Phase Requirements -> Test Map

| Req ID | Behavior | Test Type | Automated Command | File Exists? |
|--------|----------|-----------|-------------------|--------------|
| RUN-01 | Runtime assembles a transient generic agent from `GenericAgentSpec`, locked profile snapshot, resolved model, providers, tool capabilities, history, and `RuntimeSecurity`. | unit/integration | `cargo test -p lattice-runtime generic` | Partial: runtime has inline tests, but new generic assembly tests are Wave 0. [VERIFIED: crates/lattice-runtime/src/lib.rs; crates/lattice-runtime/src/turn.rs] |
| RUN-02 | Tool execution is sandbox-bound after orchestrator assembly removal. | unit/integration | `cargo test -p lattice-runtime sandbox` and `cargo test -p lattice-tools execute_outcome_returns_typed_denial_for_unlisted_builtin` | Partial: tools/sandbox tests exist; runtime generic binding test is Wave 0. [VERIFIED: crates/lattice-tools/src/executor.rs; crates/lattice-runtime/src/lib.rs] |
| RUN-03 | Completed/error/cancelled turn completion persists session messages and manifests with profile snapshot identity. | unit/integration | `cargo test -p lattice-session profile_snapshot open_turn commit_turn` and `cargo test -p lattice-runtime turn_completion` | Partial: session manifest tests exist; snapshot/open/commit tests are Wave 0. [VERIFIED: crates/lattice-session/src/lib.rs; crates/lattice-runtime/src/turn.rs] |
| RUN-04 | Runtime public API and dependency graph no longer expose extension surfaces. | boundary grep + compile | `rg -n "lattice_orchestrator|lattice_plugin|Pipeline|MicroAgent|EventBus|PluginRegistry|RuntimePluginOverlay|RuntimeAgentTurnExecutor" crates/lattice-runtime` and `cargo check -p lattice-runtime` | No dedicated source guard exists; add Wave 0 or final gate. [VERIFIED: rg crates/lattice-runtime/src; crates/lattice-runtime/Cargo.toml] |

### Sampling Rate

- **Per task commit:** Run the narrow crate test for the touched crate, plus `cargo fmt --all --check` after formatting-sensitive edits. [VERIFIED: AGENTS.md; .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md]
- **Per wave merge:** Run `cargo test -p lattice-session`, `cargo test -p lattice-profile`, `cargo test -p lattice-runtime`, and adapter tests touched in CLI/runtime. [VERIFIED: .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md]
- **Phase gate:** Run `cargo check --workspace` and `cargo fmt --all --check`; optionally run full workspace tests/clippy and classify failures. [VERIFIED: .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md]

### Wave 0 Gaps

- [ ] `crates/lattice-session/src/lib.rs` or `crates/lattice-session/src/turn_tx.rs` tests for profile snapshot hash stability, source drift reporting, open existing session returns locked snapshot, commit rejects profile hash mismatch, commit rejects base commit mismatch, and manifests include profile identity. [VERIFIED: .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md; crates/lattice-session/src/lib.rs]
- [ ] `crates/lattice-runtime/src/assembly.rs` or inline runtime tests for `GenericAgentSpec` to transient `Agent` assembly without `lattice_orchestrator::assembly`. [VERIFIED: crates/lattice-runtime/src/lib.rs; crates/lattice-profile/src/spec.rs]
- [ ] Runtime sandbox binding test proving an unenabled tool or blocked command is denied through the generic path. [VERIFIED: crates/lattice-tools/src/executor.rs; crates/lattice-runtime/src/lib.rs]
- [ ] Runtime boundary grep/source guard for removed extension concepts under `crates/lattice-runtime`. [VERIFIED: .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md; rg crates/lattice-runtime/src]
- [ ] Targeted CLI/runtime adapter tests only for compile-critical call sites after public API removal. [VERIFIED: crates/lattice-cli/src/commands/run.rs; crates/lattice-cli/src/frontend/tui/agent_runner.rs]

## Security Domain

OWASP ASVS is appropriate as a verification checklist source because OWASP describes ASVS as a basis for testing web application technical security controls and secure-development requirements, and the latest stable ASVS release is 5.0.0 dated May 2025. [CITED: https://owasp.org/www-project-application-security-verification-standard/; CITED: https://github.com/OWASP/ASVS]

### Applicable ASVS Categories

| ASVS Category | Applies | Standard Control |
|---------------|---------|------------------|
| V2 Authentication | No direct auth change in Phase 2 | Do not persist provider credentials or env-secret values in snapshots/manifests. [VERIFIED: .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md; crates/lattice-cli/src/credentials.rs] |
| V3 Session Management | Yes | `lattice-session` owns conversation state, branch/head refs, profile snapshot binding, and turn transaction boundaries. [VERIFIED: .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md; crates/lattice-session/src/lib.rs] |
| V4 Access Control | Yes | `RuntimeSecurity`, `PolicyAuthority`, `SandboxConfig`, and approved-rule validation remain the authority for tool/path/command access. [VERIFIED: crates/lattice-runtime/src/lib.rs; crates/lattice-sandbox/src/checks.rs] |
| V5 Input Validation | Yes | Validate profile snapshot hash, base commit, session id, branch names, tool bundle strings, and safe manifest metadata before commit. [VERIFIED: crates/lattice-session/src/lib.rs; crates/lattice-tools/src/capabilities.rs] |
| V6 Cryptography | Yes for hashing, not encryption | Use existing SHA-256/stable JSON CAS identity pattern for profile snapshot/manifest identity; do not invent reversible secret storage. [VERIFIED: crates/lattice-session/src/lib.rs; .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md] |

### Known Threat Patterns for Runtime Assembly Cutover

| Pattern | STRIDE | Standard Mitigation |
|---------|--------|---------------------|
| Profile file changes silently alter in-progress session behavior | Tampering | Bind immutable profile snapshot to session and warn/audit on source drift. [VERIFIED: .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md] |
| Runtime commit races overwrite or append to the wrong head | Tampering | `commit_turn` validates `base_commit_id` before writing messages/manifests. [VERIFIED: .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md] |
| Tool executor not bound to `RuntimeSecurity` after assembly rewrite | Elevation of privilege | Construct `DefaultToolExecutor` with `security.sandbox.clone()` and authorized tool names from the same resolved capability set exposed to the model. [VERIFIED: crates/lattice-tools/src/executor.rs; crates/lattice-tools/src/capabilities.rs] |
| Secrets leak into durable snapshot/manifest metadata | Information disclosure | Persist safe profile summaries only; exclude API keys, credential maps, and raw env values. [VERIFIED: .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md; crates/lattice-cli/src/credentials.rs] |
| Error/cancel outcomes lack audit trail | Repudiation | Represent `Completed`, `Error`, and `Cancelled` as status-bearing turn outcomes when a visible/model-visible boundary exists. [VERIFIED: .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md] |
| Old plugin/orchestrator APIs remain callable through runtime | Elevation of privilege / confusion | Remove runtime public APIs and dependency edges for plugin registry, plugin overlay, pipeline, micro-agent, event bus, and runtime-backed orchestrator executor. [VERIFIED: crates/lattice-runtime/src/lib.rs; crates/lattice-runtime/Cargo.toml; .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md] |

## Sources

### Primary (HIGH confidence)

- `.planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md` - locked Phase 2 decisions, suggested API shape, scope, and verification gate. [VERIFIED: file read]
- `.planning/REQUIREMENTS.md` - RUN-01 through RUN-04 requirement text. [VERIFIED: file read]
- `.planning/ROADMAP.md` - Phase 2 goal/success criteria and Phase 3/4 boundaries. [VERIFIED: file read]
- `.planning/PROJECT.md` and `.planning/STATE.md` - milestone single-agent core direction and accumulated decisions. [VERIFIED: file read]
- `AGENTS.md` - build/test commands and crate responsibility map. [VERIFIED: file read]
- `crates/lattice-profile/src/spec.rs`, `lowering.rs`, `discovery.rs` - `GenericAgentSpec`, pure lowering, and profile source resolution. [VERIFIED: source read]
- `crates/lattice-runtime/src/lib.rs`, `handle.rs`, `turn.rs`, `turn_executor.rs`, `preset.rs` - current runtime public API, live handle state, turn execution, orchestrator/plugin coupling, and sandbox construction. [VERIFIED: source read]
- `crates/lattice-session/src/lib.rs` - session messages, commits, SQLite CAS, turn manifests, `save_with_manifests`, migration style, and current lack of profile snapshots/open/commit turn APIs. [VERIFIED: source read]
- `crates/lattice-agent/src/config.rs`, `run.rs`, `manifest.rs` - existing transient agent assembly primitives and manifest collector behavior. [VERIFIED: source read]
- `crates/lattice-tools/src/capabilities.rs`, `executor.rs` and `crates/lattice-sandbox/src/config.rs` - tool capability resolution, executor authorization, sandbox policies. [VERIFIED: source read]
- `Cargo.toml`, `cargo metadata --no-deps --format-version 1`, and `crates/lattice-runtime/Cargo.toml` - versions, dependency graph, and current runtime extension dependencies. [VERIFIED: command output]

### Secondary (MEDIUM confidence)

- `.planning/codebase/ARCHITECTURE.md`, `.planning/codebase/INTEGRATIONS.md`, `.planning/codebase/STACK.md`, `.planning/codebase/CONVENTIONS.md` - codebase maps created earlier on 2026-05-14, used as orientation and cross-check against source. [VERIFIED: file read]
- Phase 1 plan summaries `01-01` through `01-05` - prior phase outcomes used to avoid re-planning completed profile/tool work. [VERIFIED: file read]
- OWASP ASVS official project page and GitHub README - ASVS purpose and latest stable version context for security-domain framing. [CITED: https://owasp.org/www-project-application-security-verification-standard/; CITED: https://github.com/OWASP/ASVS]

### Tertiary (LOW confidence)

- None. [VERIFIED: source list]

## Metadata

**Confidence breakdown:**
- Standard stack: HIGH - all recommended crates and dependency versions were verified locally through source and `cargo metadata`. [VERIFIED: cargo metadata; Cargo.toml]
- Architecture: HIGH - locked Phase 2 context and current runtime/session/profile source agree on the needed ownership inversion. [VERIFIED: .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md; crates/lattice-runtime/src/lib.rs; crates/lattice-session/src/lib.rs]
- Pitfalls: HIGH - each pitfall corresponds to current source behavior or explicit Phase 2 decisions. [VERIFIED: crates/lattice-runtime/src/handle.rs; crates/lattice-runtime/src/turn.rs; .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md]
- Exact API names: MEDIUM - the context allows planner discretion for names and module placement. [VERIFIED: .planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md]

**Research date:** 2026-05-14 [VERIFIED: date command]
**Valid until:** 2026-05-21, because this is an active local refactor and runtime/session source may change quickly during Phase 2 planning/execution. [VERIFIED: current phase state in .planning/STATE.md]

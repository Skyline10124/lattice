# Phase 2: Runtime Assembly Cutover - Context

**Gathered:** 2026-05-14
**Status:** Ready for planning

<domain>
## Phase Boundary

This phase moves active single-agent assembly into `lattice-runtime` and makes the runtime path consume the generic profile contract created in Phase 1. The runtime must stop using orchestrator-owned specs, plugin/orchestrator assembly, and long-lived live-agent handles as the source of session continuity.

The target shape is a session-owned execution path:

```text
AgentSource
  -> lattice-profile discovery/loading/lowering
  -> GenericAgentSpec / ProfileSnapshot
  -> session-bound profile snapshot
  -> runtime-owned single-turn assembly
  -> model/tool/sandbox execution
  -> lattice-session turn commit and manifest
```

This phase may aggressively clean `lattice-runtime` public APIs that expose pipeline, plugin, event-bus, micro-agent, runtime-backed orchestrator executor, plugin overlay, or live-agent-handle concepts. Full workspace deletion of `lattice-orchestrator`, `lattice-plugin`, and `lattice-plugins` remains Phase 4 scope, but `lattice-runtime` should stop depending on them if Phase 2 can do so cleanly.

This phase does not design the CLI/TUI final UX surface. CLI/TUI may need narrow compile fixes or adapter changes, but Phase 3 owns user-facing command and TUI cleanup.

</domain>

<decisions>
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

</decisions>

<canonical_refs>
## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Project and Phase Scope
- `.planning/PROJECT.md` — milestone scope, single-agent core framing, and out-of-scope extension layers.
- `.planning/REQUIREMENTS.md` — RUN-01, RUN-02, RUN-03, RUN-04 and cross-phase traceability.
- `.planning/ROADMAP.md` — Phase 2 goal, success criteria, and boundary with Phases 3 and 4.
- `.planning/STATE.md` — current project position and accumulated decisions.

### Prior Phase Decisions
- `.planning/phases/01-profile-contract-extraction/01-CONTEXT.md` — locked profile contract, `GenericAgentSpec` ownership, strict profile schema, and Phase 1 crate boundaries.
- `.planning/phases/01-profile-contract-extraction/01-01-SUMMARY.md` — `lattice-profile` strict schema and loader.
- `.planning/phases/01-profile-contract-extraction/01-02-SUMMARY.md` — profile discovery, default template, and pure lowering.
- `.planning/phases/01-profile-contract-extraction/01-profile-contract-extraction-03-SUMMARY.md` — tool bundles plus enabled-tool API and preset removal.
- `.planning/phases/01-profile-contract-extraction/01-04-SUMMARY.md` — `AgentPreset` removal and existing runtime/orchestrator bridge changes.
- `.planning/phases/01-profile-contract-extraction/01-05-SUMMARY.md` — workspace bridge and current Phase 1 verification gates.

### Current Source Inputs
- `crates/lattice-profile/src/spec.rs` — current `GenericAgentSpec` shape and forbidden-field guards.
- `crates/lattice-profile/src/lowering.rs` — pure profile-to-spec lowering path.
- `crates/lattice-profile/src/discovery.rs` — profile source resolution behavior that runtime should not duplicate.
- `crates/lattice-runtime/src/lib.rs` — current runtime public API, orchestrator assembly entry points, handle creation, and runtime dependencies to cut.
- `crates/lattice-runtime/src/handle.rs` — current live-agent handle registry to replace/delete on the generic path.
- `crates/lattice-runtime/src/turn.rs` — current handle-bound turn execution and session saving flow to replace with session-owned transaction flow.
- `crates/lattice-session/src/lib.rs` — session graph, CAS commits, manifests, branches, stores, and current `save_with_manifests` behavior.
- `crates/lattice-cli/src/coding_agent.rs` — current CLI runtime handle usage and session seeding path that shows what must not remain as the generic model.
- `crates/lattice-cli/src/session.rs` — current conversion between agent manifests and session manifests.
- `crates/lattice-orchestrator/src/agent_spec.rs` — old `AgentTurnSpec` and runtime-backed executor contract to remove from runtime main path.

### Codebase Maps
- `.planning/codebase/ARCHITECTURE.md` — current layered architecture and runtime/orchestrator/plugin coupling.
- `.planning/codebase/INTEGRATIONS.md` — runtime, CLI/TUI, session, sandbox, plugin, and manifest integration surfaces.
- `.planning/codebase/STACK.md` — workspace crate map and verification conventions.

</canonical_refs>

<code_context>
## Existing Code Insights

### Reusable Assets
- `lattice-profile::GenericAgentSpec` already carries the pure profile-derived data needed for runtime assembly: profile name, model, system prompt, tool selection, security intent, context toggles, and behavior.
- `lattice-profile` already owns discovery/loading/lowering and should remain the source of truth for `AgentSource` semantics.
- `lattice-session` already has session messages, commit objects, branch/head refs, `NewTurnManifest`, `TurnManifestObject`, manifest metadata, and SQLite CAS manifest indexes.
- Runtime already has model resolution, security construction, prompt/tool/sandbox wiring pieces, and session store injection that can be reused in a cleaner assembly path.

### Established Patterns
- Lower crates own durable data contracts; runtime owns application-level assembly; CLI/TUI should stay as adapters.
- Session persistence already follows a git-like graph with commits and refs. Phase 2 should extend this model instead of hiding state in runtime handles.
- Manifests already support metadata JSON, but profile snapshot identity should be promoted to a clear typed or well-defined manifest/session record rather than being an undocumented ad hoc blob.
- Existing Phase 1 decisions prefer deletion of legacy paths over compatibility layers because this project has not shipped the old profile/orchestrator contract.

### Integration Points
- Replace `Runtime::get_or_create_agent_handle*`, `Runtime::agent_handle_matches`, `Runtime::rebuild_agent_handle`, and handle-bound `submit_turn` as the generic runtime path.
- Remove or rewrite runtime APIs such as `assemble_agent`, `run_agent`, `run_agent_events`, pipeline/plugin/event-bus/plugin-overlay/orchestrator-executor surfaces.
- Add a small runtime invocation context type carrying cwd/project root/session selector/effective security without putting those fields into `GenericAgentSpec`.
- Add session-owned `ProfileSnapshot`, `open_turn`, and `commit_turn` semantics in `lattice-session` or a narrow module inside that crate.
- Update runtime turn events and completions to identify turns by session/turn ids rather than agent handle ids.

</code_context>

<specifics>
## Specific Ideas

- Treat an agent profile like a developer's working configuration and the session like a git repository. The profile decides how this turn works; the session owns the commit history.
- A profile injection or automatic profile update must not silently change an in-progress session. The session should stay locked to the profile snapshot it started with.
- If a profile changes on disk, the program should continue the locked snapshot and surface an audit/debug hint, not switch automatically.
- The new API should be clean and simple enough that runtime reads like an executor over session transactions, not a registry of live stateful agents.

</specifics>

<deferred>
## Deferred Ideas

- Full CLI/TUI user-facing command cleanup, including final command names and TUI presentation of profile drift, belongs primarily to Phase 3.
- Full workspace member deletion for `lattice-orchestrator`, `lattice-plugin`, and `lattice-plugins` belongs to Phase 4, though runtime should cut dependencies earlier if practical.
- Explicit profile rebind/fork UX is a future feature unless required for tests. Phase 2 should define the safe default and audit boundary, not a full switching workflow.

</deferred>

---

*Phase: 2-Runtime Assembly Cutover*
*Context gathered: 2026-05-14*

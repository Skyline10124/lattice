# Roadmap: v1.0 Single Agent Core

**Created:** 2026-05-14
**Phase numbering:** Reset to Phase 1 for this first formal GSD milestone.
**Requirements:** 20 total, 20 mapped, 0 unmapped.

## Milestone Goal

Reduce LATTICE to a coherent generic single-agent runtime and remove plugin/orchestrator/pipeline/subagent/code-agent-specific architecture from the current codebase.

## Phase Summary

| # | Phase | Goal | Requirements | Success Criteria |
|---|-------|------|--------------|------------------|
| 1 | Profile Contract Extraction | Establish generic profile/spec contracts outside orchestration semantics. | PROF-01, PROF-02, PROF-03 | 4 |
| 2 | Runtime Assembly Cutover | Make runtime assemble and run generic agents without plugin/orchestrator dependencies. | RUN-01, RUN-02, RUN-03, RUN-04 | 5 |
| 3 | CLI/TUI Single-Agent UX | Route CLI/TUI through the generic path and remove plugin/code-agent/orchestrator UI surfaces. | CLI-01, CLI-02, CLI-03, CLI-04, CLI-05 | 5 |
| 4 | Workspace Layer Removal | Remove plugin/orchestrator crates, examples, imports, and dead tests. | REM-01, REM-02, REM-03, REM-04 | 4 |
| 5 | Verification And Docs | Prove the new architecture builds, behaves, and is documented. | VER-01, VER-02, VER-03, VER-04 | 5 |

## Phase Details

### Phase 1: Profile Contract Extraction

**Goal:** Establish a generic profile and turn/build spec model that preserves useful declarative configuration while removing plugin, handoff, orchestrator, and coding-preset semantics.

**Requirements:** PROF-01, PROF-02, PROF-03

**Success criteria:**
1. A generic profile/spec contract exists in `lattice-profile` or an equivalent narrow module with no plugin/handoff/orchestrator fields.
2. Existing runtime/CLI default profile data can be represented without `AgentPreset::Coding`.
3. Profile lowering is pure data conversion and does not construct live agents, read CLI/TUI state, or access session stores.
4. Unit tests cover profile parsing/defaulting/lowering and reject removed fields or concepts.

**Notes:**
- Extract from `lattice-orchestrator/src/profile.rs` and `agent_spec.rs` before deleting orchestrator.
- Keep capability config focused on tool exposure.

### Phase 2: Runtime Assembly Cutover

**Goal:** Move active agent assembly into runtime-owned generic logic and remove runtime dependence on orchestrator/plugin abstractions.

**Requirements:** RUN-01, RUN-02, RUN-03, RUN-04

**Plans:** 6 plans

Plans:
- [x] 02-01-PLAN.md - Session-owned profile snapshot and turn transaction API
- [x] 02-02-PLAN.md - Runtime-owned generic assembly and sandbox-bound tools
- [x] 02-03-PLAN.md - Session/turn identity runtime submission and completion path
- [x] 02-04-PLAN.md - CLI generic runtime adapters
- [x] 02-05-PLAN.md - TUI session-owned runtime adapters
- [x] 02-06-PLAN.md - Runtime extension API and dependency cleanup

**Success criteria:**
1. Runtime builds/reuses a generic agent handle from profile/spec and runtime security.
2. Runtime assembly wires prompt providers, tool schema provider, memory/context, and sandbox-bound tool executor without `lattice_orchestrator::assembly`.
3. Runtime turn completion still saves session messages and turn manifests.
4. Runtime public API no longer exposes pipeline/micro-agent/event-bus/plugin-registry/plugin-overlay/orchestrator-executor concepts.
5. Targeted runtime tests prove sandbox binding and generic turn completion.

**Notes:**
- Preserve current `RuntimeTurnEvent`, `TurnRequest`, `TurnCompletion`, and handle architecture where possible.
- Avoid unrelated session/tool large-file splits.

### Phase 3: CLI/TUI Single-Agent UX

**Goal:** Make the user-facing CLI and TUI reflect one generic agent path.

**Requirements:** CLI-01, CLI-02, CLI-03, CLI-04, CLI-05

**Success criteria:**
1. CLI `run` and TUI prompt submission use the same generic runtime spec/profile path.
2. `code` is removed or becomes a thin generic profile/default-tool alias with no separate `coding_agent.rs` module.
3. CLI command tree no longer exposes pipeline, orchestrator status, plugin loading, plugin execution, or agent-registry validation/listing commands.
4. TUI removes plugin commands, plugin statusline labels, plugin handles, plugin overlay application, and plugin menus.
5. TUI still supports core single-agent interactions: model/provider, prompt streaming, tools, approvals/permissions, and sessions.

**Notes:**
- Rename tests and fixtures away from `coding_agent` terminology.
- Keep frontend runtime guard tests, but update them for the generic runtime path.

### Phase 4: Workspace Layer Removal

**Goal:** Delete obsolete extension/orchestration layers from the active workspace.

**Requirements:** REM-01, REM-02, REM-03, REM-04

**Success criteria:**
1. Root `Cargo.toml` no longer lists `lattice-plugin`, `lattice-plugins`, or `lattice-orchestrator`.
2. Runtime/CLI/agent/profile crates compile without imports from removed crates.
3. Examples and tests that only covered plugin/orchestrator/pipeline/subagent behavior are deleted or rewritten for generic agent behavior.
4. Source grep checks show removed symbols and crate names absent from active source, except historical planning docs or explicit migration notes.

**Notes:**
- This phase should happen after Phases 1-3 to avoid losing profile/spec code before replacement exists.
- Expect test fixture cleanup to be a major part of this phase.

### Phase 5: Verification And Docs

**Goal:** Prove the simplified architecture is stable and make docs match the new code.

**Requirements:** VER-01, VER-02, VER-03, VER-04

**Success criteria:**
1. `cargo check --workspace` succeeds.
2. Targeted tests pass for runtime, agent, CLI/TUI, tools, sandbox, and session paths.
3. Boundary checks confirm active workspace has no plugin/orchestrator/code-agent runtime surface.
4. README, AGENTS/CLAUDE where appropriate, and `.planning/codebase` maps describe the simplified single-agent architecture.
5. Final report identifies any deferred extension work as future requirements rather than hidden code.

**Notes:**
- Preferred final gate: `cargo fmt --all --check`, `cargo clippy --workspace -- -D warnings`, and `cargo test --workspace` if the refactor reaches full closure.
- If full workspace tests expose unrelated pre-existing failures, document them and run focused gates for changed crates.

## Coverage

| Requirement | Phase | Status |
|-------------|-------|--------|
| PROF-01 | Phase 1 | Pending |
| PROF-02 | Phase 1 | Pending |
| PROF-03 | Phase 1 | Pending |
| RUN-01 | Phase 2 | Complete |
| RUN-02 | Phase 2 | Complete |
| RUN-03 | Phase 2 | Complete |
| RUN-04 | Phase 2 | Complete |
| CLI-01 | Phase 3 | Pending |
| CLI-02 | Phase 3 | Pending |
| CLI-03 | Phase 3 | Pending |
| CLI-04 | Phase 3 | Pending |
| CLI-05 | Phase 3 | Pending |
| REM-01 | Phase 4 | Pending |
| REM-02 | Phase 4 | Pending |
| REM-03 | Phase 4 | Pending |
| REM-04 | Phase 4 | Pending |
| VER-01 | Phase 5 | Pending |
| VER-02 | Phase 5 | Pending |
| VER-03 | Phase 5 | Pending |
| VER-04 | Phase 5 | Pending |

**Coverage status:** 20 / 20 requirements mapped.

---
*Roadmap created: 2026-05-14 for v1.0 Single Agent Core*

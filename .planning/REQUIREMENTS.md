# Requirements: LATTICE v1.0 Single Agent Core

**Defined:** 2026-05-14
**Core Value:** A user can run one generic agent with reliable model routing, prompt/context assembly, sandboxed tools, session persistence, and CLI/TUI feedback without immature extension layers in the execution path.

## v1.0 Requirements

### Profile

- [ ] **PROF-01**: Developer can define a generic agent profile that describes model, prompt, tool capabilities, behavior, working directory, and security intent without plugin, handoff, orchestrator, or subagent fields.
- [ ] **PROF-02**: Runtime can lower a profile or CLI default into a pure generic turn/build spec without constructing a live `Agent` or reading CLI/TUI state.
- [ ] **PROF-03**: Coding-capable behavior is represented by profile prompt and tool capabilities, not by `AgentPreset::Coding`, `coding_agent.rs`, or a dedicated agent subtype.

### Runtime

- [ ] **RUN-01**: Runtime can construct and reuse one generic agent handle from profile/spec, resolved model, prompt providers, tool capabilities, sandbox policy, memory/context, and optional session seed.
- [ ] **RUN-02**: Runtime tool execution remains bound to `RuntimeSecurity` sandbox and policy settings after plugin/orchestrator assembly is removed.
- [ ] **RUN-03**: Runtime turn completion persists session messages and turn manifests through the existing session store path.
- [ ] **RUN-04**: Runtime public APIs no longer expose pipeline, micro-agent, event bus, plugin registry, plugin directory loading, plugin overlay, or runtime-backed orchestrator executor concepts.

### CLI/TUI

- [ ] **CLI-01**: CLI `run` and TUI submit turns through the same generic runtime path.
- [ ] **CLI-02**: CLI no longer exposes orchestrator status, pipeline run, agent registry validation/listing, plugin loading, or plugin execution commands.
- [ ] **CLI-03**: The existing `code` experience is removed or reduced to a generic profile/default-tool selection with no separate `coding_agent` module or preset branch.
- [ ] **CLI-04**: TUI no longer shows plugin status, `/plugins`, `/plugin`, plugin menus, plugin handles, or runtime plugin overlay state.
- [ ] **CLI-05**: TUI keeps core single-agent workflows: model/provider display, prompt submission, streaming output, tool visibility, approvals/permissions, and session restore.

### Removal

- [ ] **REM-01**: Workspace no longer includes `crates/lattice-plugin`, `crates/lattice-plugins`, or `crates/lattice-orchestrator` as active members.
- [ ] **REM-02**: Active source no longer imports `lattice_plugin`, `lattice_plugins`, or `lattice_orchestrator`.
- [ ] **REM-03**: Active source no longer contains runtime/CLI/TUI definitions for `PluginRegistry`, `RuntimePluginOverlay`, `Pipeline`, `MicroAgent`, `AgentPreset::Coding`, or `coding_agent`.
- [ ] **REM-04**: Examples, tests, and docs that only exercise plugin/orchestrator/pipeline/subagent behavior are removed or rewritten for the generic single-agent path.

### Verification

- [ ] **VER-01**: `cargo check --workspace` succeeds after dependency removal.
- [ ] **VER-02**: Targeted runtime, agent, CLI/TUI, tools, sandbox, and session tests cover the generic single-agent path.
- [ ] **VER-03**: Boundary grep checks verify removed plugin/orchestrator/code-agent concepts are absent from active runtime, CLI, and workspace manifests.
- [ ] **VER-04**: Project docs and codebase maps are updated to describe the new dependency flow and single-agent scope.

## Future Requirements

### Extensions

- **EXT-01**: Plugin system can be reconsidered as a separate optional extension layer after the single-agent runtime is stable.
- **EXT-02**: Subagents, pipelines, handoff, and orchestration can be reconsidered as a separate milestone after profile/runtime boundaries are proven.
- **EXT-03**: Large mechanical file splits in session/tools/runtime/TUI can be planned after the architecture cutover.

## Out of Scope

| Feature | Reason |
|---------|--------|
| Plugin framework replacement | Would recreate the immature extension layer instead of stabilizing the core. |
| Multi-agent orchestration | Explicitly deferred until one-agent runtime is solid. |
| New tools or model providers | Not needed for the architectural cutover. |
| Whole-workspace file modularization | Useful later, but too much churn for this milestone. |
| External ecosystem research beyond current codebase | The scope is internal architecture and code removal. |

## Traceability

| Requirement | Phase | Status |
|-------------|-------|--------|
| PROF-01 | Phase 1 | Pending |
| PROF-02 | Phase 1 | Pending |
| PROF-03 | Phase 1 | Pending |
| RUN-01 | Phase 2 | Pending |
| RUN-02 | Phase 2 | Pending |
| RUN-03 | Phase 2 | Pending |
| RUN-04 | Phase 2 | Pending |
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

**Coverage:**
- v1.0 requirements: 20 total
- Mapped to phases: 20
- Unmapped: 0

---
*Requirements defined: 2026-05-14*
*Last updated: 2026-05-14 after milestone v1.0 requirements definition*

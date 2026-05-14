---
gsd_state_version: 1.0
milestone: v1.3
milestone_name: Generic Agent Runtime & Session Graph
status: verifying
last_updated: "2026-05-13T16:24:38Z"
last_activity: 2026-05-13 -- Phase 17 execution complete; verification pending
progress:
  total_phases: 5
  completed_phases: 1
  total_plans: 16
  completed_plans: 10
  percent: 63
---

# Project State

## Project Reference

See: .planning/PROJECT.md (updated 2026-05-13)

**Core value:** Clear dependency direction and capability boundaries -- each layer owns its own business, and powerful agent abilities are opt-in capabilities rather than global assumptions.
**Current focus:** Phase 17 — Frontend Adapter Migration

## Current Position

Phase: 17 (frontend-adapter-migration) — EXECUTING
Plan: 6 of 6
Status: Phase complete — ready for verification
Last activity: 2026-05-13 -- Phase 17 execution complete; verification pending

## Performance Metrics

**Velocity:**

- Total plans completed: 49
- Total phases completed: 15
- Latest milestone shipped: v1.2 on 2026-05-12

**By Phase:**

| Phase | Plans | Status |
|-------|-------|--------|
| 1. M1 Code Cleanup | 3/3 | Complete |
| 2. M1 Sandbox Extraction | 2/2 | Complete |
| 3. M2 Core Decoupling | 1/1 | Complete |
| 4. M2 Tools Consolidation | 1/1 | Complete |
| 5. M2 Orchestrator Rename | 2/2 | Complete |
| 6. Contribution Path Migration | 2/2 | Complete |
| 7. PromptSpec Profile Loading | 2/2 | Complete |
| 8. Declarative Hook Builder | 2/2 | Complete |
| 9. Manifest Collection | 2/2 | Complete |
| 10. Capability Bundles & Profile Isolation | 4/4 | Complete |
| 11. Structured Editing Engine | 4/4 | Complete |
| 12. Permission & Approval Workflow | 4/4 | Complete |
| 13. Agent Task & Recovery State | 4/4 | Complete |
| 14. Main Code Agent & Eval Harness | 7/7 | Complete |
| 15. Generic Agent Runtime Naming & Profile Assembly | 4/4 | Complete |
| 16. Runtime-Owned Agent Instance & Turn Lifecycle | 0/6 | Ready to execute |
| 17. Frontend Adapter Migration | 6/6 | Ready for verification |
| 18. Session Graph Interface & Agent/Manifest Decoupling | — | Pending |
| 19. Regression & Compatibility Hardening | — | Pending |
| Phase 14-main-code-agent-eval-harness P01 | 14 min | 3 tasks | 5 files |
| Phase 14-main-code-agent-eval-harness P02 | 28 min | 2 tasks | 2 files |
| Phase 14-main-code-agent-eval-harness P03 | 3h 14m | 3 tasks | 10 files |
| Phase 14-main-code-agent-eval-harness P04 | 20 min | 3 tasks | 6 files |
| Phase 14-main-code-agent-eval-harness P05 | 24 min | 4 tasks | 6 files |
| Phase 14-main-code-agent-eval-harness P06 | 7 min | 3 tasks | 2 files |
| Phase 14-main-code-agent-eval-harness P07 | 24 min | 5 tasks | 6 files |

## Accumulated Context

### Decisions

- Bottom-up ordering: clean CI first, then extract, then decouple.
- Registry traits merged into orchestrator as stubs (design intent preserved).
- lattice-hooks kept as standalone crate (not merged into agent).
- v1.1 phases followed crate dependency DAG: refactor first, then spec loading, then hooks, then manifest.
- Code agent is the first flagship main agent, but CC/Codex-like powers should become platform capabilities.
- Capability bundles/presets should decide which agents receive filesystem, editing, shell, planning, web, and coding tools.
- Editing engine belongs in `lattice-tools`; profile/capability selection belongs in orchestrator/runtime/CLI wiring.
- [Phase 14-main-code-agent-eval-harness]: Runtime::build_code_agent is the authoritative code-agent factory. — Plan 14-01 moved code-agent prompt, provider, capability, executor, security, and task-state assembly into lattice-runtime so CLI code remains a thin session/rendering entrypoint.
- [Phase 14-main-code-agent-eval-harness]: Project instruction precedence is deterministic. — Plan 14-02 renders parent instructions before child instructions, renders same-directory CLAUDE.md before AGENTS.md before LATTICE.md, and treats @file text literally.
- [Phase 14-main-code-agent-eval-harness]: Approval retry is proposal-driven and explicit. — Plan 14-03 stores pending approval proposals in TaskState, approves them through RuntimeSecurity, and keeps retry/replay as an explicit follow-up action.
- [Phase 14-main-code-agent-eval-harness]: Final reporting is state-derived. — Plan 14-04 renders Changed Files, Verification, and Residual Risk from TaskState plus git status sanity checks; model prose cannot mark failed or missing verification as passed.
- [Phase 14-main-code-agent-eval-harness]: Code-agent behavior is eval-locked. — Plan 14-05 adds deterministic product-loop Rust evals for dirty worktree safety, project instructions, targeted reading, patch recovery, verification honesty, profile isolation, and gap-analysis linkage.
- [Phase 14-main-code-agent-eval-harness]: Project instruction render order composes file-source and directory precedence. — Plan 14-06 renders CLAUDE.md, AGENTS.md, and LATTICE.md tiers globally, with parent-before-child ordering inside each tier.
- [Phase 14-main-code-agent-eval-harness]: Approval blocker cleanup is state-linked and idempotent. — Plan 14-07 links approval blocker failures to proposal IDs, coalesces equivalent pending proposals by approve_command plus risk_class, suppresses only resolved approval-linked failures in renderers, and deduplicates identical validated grants in PolicyAuthority.

- [Phase 15-generic-agent-runtime-naming-profile-assembly]: Coding is now a runtime preset, not a legacy code-agent factory. — ToolPreset::Coding and AgentPreset::Coding are the canonical typed names; `code-agent` remains only as a compatibility alias for capability deserialization/parsing.
- [Phase 15-generic-agent-runtime-naming-profile-assembly]: Coding preset requires RuntimeSecurity. — The generic AgentSpec path refuses AgentPreset::Coding without RuntimeSecurity so visible coding tools always have an authorized DefaultToolExecutor behind them.

### Pending Todos

None.

### Blockers/Concerns

None. Phase 14 UAT gaps are resolved and phase-level verification passed.

## Deferred Items

Items acknowledged and carried forward from v1.0 close:

| Category | Item | Status | Deferred At |
|----------|------|--------|-------------|
| V2 | TUI app.rs split (2541 lines) | Deferred | 2026-05-08 |
| V2 | SQLite mutex scaling | Deferred | 2026-05-08 |
| V2 | notify crate async migration | Deferred | 2026-05-08 |
| V2 | MCP protocol support | Deferred | 2026-05-08 |

Items acknowledged at v1.1 close:

| Category | Item | Status | Deferred At |
|----------|------|--------|-------------|
| UAT gap | Phase 02 `02-UAT.md` reported by audit-open despite zero pending scenarios | Acknowledged | 2026-05-10 |
| Verification gap | Phase 02 `02-VERIFICATION.md` status `gaps_found` | Acknowledged | 2026-05-10 |
| Verification gap | Phase 06 `06-VERIFICATION.md` status `gaps_found` | Acknowledged | 2026-05-10 |

Items acknowledged at v1.2 close:

| Category | Item | Status | Deferred At |
|----------|------|--------|-------------|
| UAT audit-open false positive | Phase 02 `02-UAT.md` reports `passed` with zero pending scenarios | Acknowledged | 2026-05-13 |
| UAT audit-open false positive | Phase 14 `14-UAT.md` reports `resolved` with zero pending scenarios | Acknowledged | 2026-05-13 |
| Verification gap | Phase 02 `02-VERIFICATION.md` status `gaps_found` | Acknowledged legacy planning debt | 2026-05-13 |
| Human verification note | Phase 12 `12-VERIFICATION.md` status `human_needed`; PERM requirements are satisfied and Phase 14 wires `/approve` UX | Acknowledged manual UX note | 2026-05-13 |

## Session Continuity

Last session: 2026-05-13T12:45:20.886Z
Current milestone: v1.3 Generic Agent Runtime & Session Graph
Next step: execute Phase 17 plans.

## Operator Next Steps

- Execute Phase 17 with `$gsd-execute-phase 17`.
- Review `.planning/phases/17-frontend-adapter-migration/*-PLAN.md` before execution if you want to tweak the plan sequence.

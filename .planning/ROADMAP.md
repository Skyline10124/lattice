# Roadmap: LATTICE

## Milestones

- **v1.0 Tech Debt Cleanup & Architecture Refactoring** -- Phases 1-5 (shipped 2026-05-08, archived)
- **v1.1 Prompt Lifecycle & Session Runtime** -- Phases 6-9 (shipped 2026-05-10, archived)
- **v1.2 CC/Codex Capability Foundation & Code Agent** -- Phases 10-14 (shipped 2026-05-12, archived)
- **v1.3 Generic Agent Runtime & Session Graph** -- Phases 15-19 (planning)

## Phases

<details>
<summary>v1.0 Tech Debt Cleanup & Architecture Refactoring (Phases 1-5) -- SHIPPED 2026-05-08</summary>

- [x] Phase 1: M1 Code Cleanup (3/3 plans) -- completed 2026-05-08
- [x] Phase 2: M1 Sandbox Extraction (2/2 plans) -- completed 2026-05-08
- [x] Phase 3: M2 Core Decoupling (1/1 plan) -- completed 2026-05-08
- [x] Phase 4: M2 Tools Consolidation (1/1 plan) -- completed 2026-05-08
- [x] Phase 5: M2 Orchestrator Rename (2/2 plans) -- completed 2026-05-08

Archive: `.planning/milestones/v1.0-ROADMAP.md`

</details>

<details>
<summary>v1.1 Prompt Lifecycle & Session Runtime (Phases 6-9) -- SHIPPED 2026-05-10</summary>

- [x] Phase 6: Contribution Path Migration (2/2 plans) -- completed 2026-05-09
- [x] Phase 7: PromptSpec Profile Loading (2/2 plans) -- completed 2026-05-10
- [x] Phase 8: Declarative Hook Builder (2/2 plans) -- completed 2026-05-10
- [x] Phase 9: Manifest Collection (2/2 plans) -- completed 2026-05-10

Archive: `.planning/milestones/v1.1-ROADMAP.md`
Phase archive: `.planning/milestones/v1.1-phases/`

</details>

<details>
<summary>v1.2 CC/Codex Capability Foundation & Code Agent (Phases 10-14) -- SHIPPED 2026-05-12</summary>

- [x] Phase 10: Capability Bundles & Profile Isolation (4/4 plans) -- completed 2026-05-10
- [x] Phase 11: Structured Editing Engine (4/4 plans) -- completed 2026-05-10
- [x] Phase 12: Permission & Approval Workflow (4/4 plans) -- completed 2026-05-11
- [x] Phase 13: Agent Task & Recovery State (4/4 plans) -- completed 2026-05-11
- [x] Phase 14: Main Code Agent & Eval Harness (7/7 plans) -- completed 2026-05-12

Archive: `.planning/milestones/v1.2-ROADMAP.md`
Requirements: `.planning/milestones/v1.2-REQUIREMENTS.md`
Milestone audit: `.planning/milestones/v1.2-MILESTONE-AUDIT.md`

</details>

<details open>
<summary>v1.3 Generic Agent Runtime & Session Graph (Phases 15-19) -- PLANNING</summary>

- [x] Phase 15: Generic Agent Runtime Naming & Profile Assembly — completed 2026-05-13
  - **Goal:** Replace code-agent-specific internal construction with generic agent specs, profile assembly, and compatibility boundaries.
  - **Requirements:** GEN-01, GEN-02, GEN-03, GEN-04
  - **Plans:** 4 plans
  - **Success criteria:**
    1. Runtime exposes product-neutral agent spec/factory concepts.
    2. `lattice code` remains backed by the coding preset and preserves v1.2 behavior.
    3. Code-agent naming remains only where it describes the user-facing coding preset or an explicit compatibility shim.
    4. Non-coding profiles do not inherit coding capabilities by default.
  - **Plans:**
    - [x] 15-01-PLAN.md — Rename ToolPreset::CodeAgent to ToolPreset::Coding with backward-compatible FromStr
    - [x] 15-02-PLAN.md — Create runtime preset module, extend AgentSpec, add preset dispatch to build_agent, delete code_agent.rs
    - [x] 15-03-PLAN.md — Update orchestrator assembly test fixtures and test names to canonical 'coding' preset
    - [x] 15-04-PLAN.md — Migrate CLI coding_agent.rs and code_agent_eval.rs to AgentSpec + AgentPreset::Coding

- [ ] Phase 16: Runtime-Owned Agent Instance & Turn Lifecycle
  - **Goal:** Move live agent instance ownership, turn submission, streaming, security, task-state carryover, and manifest handoff into `lattice-runtime`.
  - **Requirements:** OWN-01, OWN-02, OWN-03, OWN-04
  - **Success criteria:**
    1. Runtime can create, reuse, and dispose agent handles keyed by profile/session/model/workspace/security.
    2. A runtime turn API streams events without exposing mutable `Agent` ownership to callers.
    3. Runtime owns turn completion data needed for final reports, task state, token usage, and manifest persistence.
    4. Runtime security approvals survive agent reuse or rebuild within the same runtime/session flow.
  - **Plans:** 6 plans
    - [ ] 16-01-PLAN.md — Lower profile/spec/turn abstractions in orchestrator
    - [ ] 16-02-PLAN.md — Add runtime-owned agent handle registry and lifecycle state
    - [ ] 16-03-PLAN.md — Implement runtime submit_turn, semantic events, completion bundles, and persistence
    - [ ] 16-04-PLAN.md — Migrate orchestrator pipeline execution to runtime-owned handles
    - [ ] 16-05-PLAN.md — Bridge CLI one-shot commands and coding evals to runtime-owned turns
    - [ ] 16-06-PLAN.md — Bridge TUI runtime handles/events, plugin overlays, approval state, and guardrails

- [ ] Phase 17: Frontend Adapter Migration
  - **Goal:** Convert CLI and TUI execution paths into adapters over runtime-owned agent handles and stream events.
  - **Requirements:** FE-01, FE-02, FE-03, FE-04
  - **Success criteria:**
    1. TUI `App` state no longer stores `Arc<Mutex<Agent>>`.
    2. TUI turn execution maps runtime stream events into the existing UI event model.
    3. CLI `lattice code` one-shot execution uses the same runtime turn path as the TUI.
    4. Session resume, queued input, plugin-mode, approval retry, and final reporting keep their existing behavior.
  - **Plans:** 6 plans
    - **Wave 1**
      - [x] 17-01-PLAN.md - Runtime turn gap closure and frontend support APIs
    - **Wave 2** *(blocked on Wave 1 completion)*
      - [x] 17-02-PLAN.md - CLI `code`/`run`/`debug` hard cut to runtime turns
      - [x] 17-03-PLAN.md - TUI runtime handle/event/completion migration
    - **Wave 3** *(blocked on Wave 2 TUI migration)*
      - [x] 17-04-PLAN.md - TUI queued prompt and cancellation UX
      - [x] 17-05-PLAN.md - TUI plugin mode and approval runtime APIs
    - **Wave 4** *(blocked on Waves 2-3 completion)*
      - [x] 17-06-PLAN.md - Regression guards and final workspace gates

- [ ] Phase 18: Session Graph Interface & Agent/Manifest Decoupling
  - **Goal:** Preserve core git-style conversation graph operations while separating session graph storage from generic agent execution details.
  - **Requirements:** SESS-01, SESS-02, SESS-03, SESS-04, DEC-01, DEC-02, DEC-03
  - **Success criteria:**
    1. Session core graph operations are explicit and covered for both file and SQLite stores.
    2. Advanced helpers such as reflog, tag, reset, rebase, stash, and gc are separated or clearly extension-marked.
    3. `lattice-agent` manifests no longer require branch or parent git commit fields.
    4. Runtime/CLI layers record workspace git metadata when needed without leaking it into the generic agent core.

- [ ] Phase 19: Regression & Compatibility Hardening
  - **Goal:** Lock down behavior, compatibility, and documentation after the generic runtime/session migration.
  - **Requirements:** EVAL-01, EVAL-02, EVAL-03, EVAL-04
  - **Success criteria:**
    1. Existing deterministic code-agent evals pass through the generic runtime path.
    2. Tests or compile-time checks prevent CLI/TUI from regressing to direct live `Agent` ownership.
    3. Session graph tests cover preserved core operations and the extension boundary.
    4. Relevant formatting, clippy, and test commands pass or any unavailable command is explicitly documented.

</details>

## Progress

**Active Execution Order:** 15 -> 16 -> 17 -> 18 -> 19

| Phase | Milestone | Plans Complete | Status | Completed |
|-------|-----------|----------------|--------|-----------|
| 1. M1 Code Cleanup | v1.0 | 3/3 | Complete | 2026-05-08 |
| 2. M1 Sandbox Extraction | v1.0 | 2/2 | Complete | 2026-05-08 |
| 3. M2 Core Decoupling | v1.0 | 1/1 | Complete | 2026-05-08 |
| 4. M2 Tools Consolidation | v1.0 | 1/1 | Complete | 2026-05-08 |
| 5. M2 Orchestrator Rename | v1.0 | 2/2 | Complete | 2026-05-08 |
| 6. Contribution Path Migration | v1.1 | 2/2 | Complete | 2026-05-09 |
| 7. PromptSpec Profile Loading | v1.1 | 2/2 | Complete | 2026-05-10 |
| 8. Declarative Hook Builder | v1.1 | 2/2 | Complete | 2026-05-10 |
| 9. Manifest Collection | v1.1 | 2/2 | Complete | 2026-05-10 |
| 10. Capability Bundles & Profile Isolation | v1.2 | 4/4 | Complete    | 2026-05-10 |
| 11. Structured Editing Engine | v1.2 | 4/4 | Complete   | 2026-05-10 |
| 12. Permission & Approval Workflow | v1.2 | 4/4 | Complete    | 2026-05-11 |
| 13. Agent Task & Recovery State | v1.2 | 4/4 | Complete    | 2026-05-11 |
| 14. Main Code Agent & Eval Harness | v1.2 | 7/7 | Complete   | 2026-05-12 |
| 15. Generic Agent Runtime Naming & Profile Assembly | v1.3 | 4/4 | Complete    | 2026-05-13 |
| 16. Runtime-Owned Agent Instance & Turn Lifecycle | v1.3 | 0/6 | Ready to execute | — |
| 17. Frontend Adapter Migration | v1.3 | 6/6 | Complete   | 2026-05-13 |
| 18. Session Graph Interface & Agent/Manifest Decoupling | v1.3 | — | Pending | — |
| 19. Regression & Compatibility Hardening | v1.3 | — | Pending | — |

---
*Created: 2026-05-10*
*Last updated: 2026-05-13 after Phase 17 planning*

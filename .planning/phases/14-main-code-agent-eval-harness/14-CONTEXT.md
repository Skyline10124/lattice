# Phase 14: Main Code Agent & Eval Harness - Context

**Gathered:** 2026-05-12
**Status:** Ready for planning

<domain>
## Phase Boundary

Phase 14 turns `lattice code` into the flagship code-agent main profile assembled from the reusable v1.2 foundations: capability bundles, structured editing, permission approvals, project instructions, task/recovery state, session resume, and final reporting.

This phase must also prove that behavior with deterministic eval fixtures. The goal is not to make all of Lattice a code-agent-only product; the code agent remains one strong main profile that validates the platform capability model.

</domain>

<decisions>
## Implementation Decisions

### Code-Agent Source of Truth
- **D-01:** The built-in code-agent profile/factory is the single source of truth for code-agent behavior. CLI code must become a thin entrypoint that passes user/session/display inputs into the shared factory.
- **D-02:** The authoritative default implementation is a Rust built-in factory, not a TOML-first migration in this phase. Exact type names are flexible, but downstream planning should look for a runtime/orchestrator-facing `code_agent` factory path.
- **D-03:** The factory owns assembly of the `code-agent` capability preset, authorized executor, runtime security, project instruction provider, repo/workspace context provider, task/recovery provider, and default code-agent system prompt.
- **D-04:** The repo context currently assembled in `crates/lattice-cli/src/coding_agent.rs` must move into a shared provider attached by the code-agent factory. CLI-built and runtime-built code agents should receive the same workspace summary semantics.
- **D-05:** Runtime-built code agents get strong defaults and fail clearly when critical inputs are missing. Do not produce a partial "code agent" that exposes schemas without security, executor, project root, or task/recovery wiring.

### Project Instruction Behavior
- **D-06:** Hierarchical project instructions load from project root to current working directory. When rules conflict at the same topic/scope, more deeply nested directory instructions take precedence over parent directory instructions.
- **D-07:** Within the same directory, file precedence is `LATTICE.md` > `AGENTS.md` > `CLAUDE.md`. `LATTICE.md` is the native project rule source, `AGENTS.md` is general agent guidance, and `CLAUDE.md` is compatibility input.
- **D-08:** Phase 14 does not expand `@file` or similar include syntax inside instruction files. It only loads the hierarchical instruction files themselves. Include expansion is deferred.
- **D-09:** Instruction behavior must be verified in two layers: deterministic provider/unit tests for path ordering and precedence, plus a code-agent-level fixture proving the final assembled agent sees and obeys nested instructions.

### Approval Retry Loop
- **D-10:** Phase 14 must close the denied-operation approval retry loop. It is not enough to generate `DenialReport` and `__approval_proposal__`; users need a session-scoped approval path and a successful retry path.
- **D-11:** Provide an explicit CLI `/approve` entrypoint and a TUI entrypoint with the same semantics. Both must use the shared `RuntimeSecurity.approve_rule` path rather than duplicating approval logic.
- **D-12:** Approval proposals need short, stable proposal IDs. At minimum, the user can approve `/approve last` or `/approve <id>`. Proposal IDs and proposal state should be visible through task/recovery/session state so resume and audit flows can distinguish multiple denials.
- **D-13:** Approval must not automatically replay the denied command. `/approve` updates session-scoped security state; the user explicitly continues/retries, and the agent uses task/recovery state to retry the blocked operation.

### Final Reporting Contract
- **D-14:** Final answers must be driven by structured task/recovery state and checked by evals. Prompt-only guidance is insufficient for Phase 14.
- **D-15:** Verification status is a three-state contract: `passed`, `failed`, or `not_run`. Only commands that actually ran and exited successfully can be reported as `passed`.
- **D-16:** Changed files in final reporting are primarily sourced from structured edit manifests and task state. Git status/diff is used as a sanity check to detect unrelated dirty work or state gaps.
- **D-17:** Final answers use human-friendly fixed sections, not a JSON envelope. Required sections are `Changed Files`, `Verification`, and `Residual Risk`.

### Eval Harness
- **D-18:** The eval harness should be deterministic and scriptable, using mocked/scripted model and tool behavior rather than real model calls. It must be stable enough for CI.
- **D-19:** Must-have fixtures cover all six Phase 14 behavior families: dirty worktree safety, project instruction obedience, targeted file reading/searching, patch recovery, verification honesty, and profile isolation.
- **D-20:** Eval fixtures should live in Rust integration tests or equivalent workspace test modules using temporary repositories and fixture files. They should run through the normal `cargo test`/CI path.
- **D-21:** `.planning/research/CC_CODEX_GAP_ANALYSIS.md` is a canonical research input. Phase 14 should reference it and summarize which gaps are closed by the new code-agent/eval work; it does not need to rewrite the full gap analysis document.

### the agent's Discretion
- Exact Rust type names, module placement, and fixture helper names are flexible.
- The planner can choose whether the code-agent factory lives primarily in `lattice-runtime`, `lattice-orchestrator`, or a small shared module, as long as CLI and runtime-built agents call the same authoritative path.
- The exact format of proposal IDs is flexible if they are stable enough for `/approve last`, `/approve <id>`, session resume, and tests.
- The scripted eval harness design is flexible if it remains deterministic and does not depend on live model behavior.

</decisions>

<canonical_refs>
## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Planning
- `.planning/PROJECT.md` — v1.2 milestone goal, code-agent-as-flagship boundary, and capability-foundation direction.
- `.planning/REQUIREMENTS.md` — CODE-01 through CODE-05 and EVAL-01 through EVAL-04.
- `.planning/ROADMAP.md` — Phase 14 goal, dependencies, and success criteria.
- `.planning/STATE.md` — current milestone state and accumulated v1.2 decisions.
- `.planning/v1.2-MILESTONE-AUDIT.md` — audit blockers this phase must close: `/approve`, runtime-built code-agent recovery wiring, nested instruction behavior, final-report honesty, and missing evals.
- `.planning/research/CC_CODEX_GAP_ANALYSIS.md` — research input for EVAL-04 and Codex/Claude Code-inspired behavior gaps.

### Prior Phase Context
- `.planning/phases/10-capability-bundles-profile-isolation/10-CONTEXT.md` — `code-agent` preset, bundle boundaries, and profile isolation decisions.
- `.planning/phases/11-structured-editing-engine/11-CONTEXT.md` — one visible structured `patch` tool, changed-file manifest, and `multi_patch` retirement.
- `.planning/phases/12-permission-approval-workflow/12-CONTEXT.md` — denial/proposal semantics and session-scoped approval direction.
- `.planning/phases/13-agent-task-recovery-state/13-CONTEXT.md` — task/recovery state, manifest metadata, resume provider, changed files, failed verification, and unverified work.

### Code-Agent Assembly
- `crates/lattice-cli/src/coding_agent.rs` — current CLI-owned code-agent prompt, repo context, `AgentSpec.capability_preset("code-agent")`, task-state provider wiring, and resume seed.
- `crates/lattice-runtime/src/lib.rs` — `AgentSpec`, `Runtime::build_agent`, capability resolution, project instruction provider attachment, security/executor wiring, and current runtime tests.
- `crates/lattice-orchestrator/src/profile.rs` — profile capability configuration and profile-loading behavior.
- `crates/lattice-tools/src/capabilities.rs` — capability bundles and `code-agent` preset membership.

### Prompt Providers and State
- `crates/lattice-prompt/src/project_instructions_provider.rs` — current `LATTICE.md` / `AGENTS.md` / `CLAUDE.md` provider and path traversal behavior.
- `crates/lattice-agent/src/prompt/task_state_provider.rs` — prompt rendering of changed files, failures, failed verification, and unverified work.
- `crates/lattice-agent/src/state.rs` — task state model, changed-file records, verification records, and unverified work.
- `crates/lattice-agent/src/manifest.rs` — turn manifest metadata collection and emission.
- `crates/lattice-cli/src/session.rs` — agent manifest metadata preservation in session manifests.

### Tests and Fixtures
- `crates/lattice-agent/tests/agent_mock.rs` — existing mock-agent integration-test pattern.
- `crates/lattice-agent/tests/context_e2e.rs` — existing context/provider integration-test pattern.
- `crates/lattice-tools/tests/exposure.rs` — existing exposed-tool/profile-isolation checks.
- `AGENTS.md` — current repository instruction file used as a project-instruction example.
- `CLAUDE.md` — current repository instruction file used as a compatibility-instruction example.

</canonical_refs>

<code_context>
## Existing Code Insights

### Reusable Assets
- `AgentSpec.capability_preset("code-agent")` already exists and is used by the CLI code-agent path.
- `ToolPreset::CodeAgent` already resolves to workspace, filesystem, editing, shell, planning, and web bundles.
- `ProjectInstructionsProvider` already walks from root to cwd and reads `LATTICE.md`, `AGENTS.md`, and `CLAUDE.md`.
- `TaskStateProvider` already renders current plan, changed files, tool failures, failed verification, and unverified work into prompts.
- Turn manifest metadata is already preserved through the CLI session conversion path.
- Existing Rust integration tests already use mock server/model-style patterns that can be adapted for deterministic code-agent fixtures.

### Current Gaps
- `coding_agent.rs` still owns too much code-agent behavior: system prompt, repo context, task-state provider hookup, and resume-state seeding.
- Runtime-built code agents can expose code-agent schemas, but do not yet have the same strong factory guarantees as `lattice code`.
- The project instructions provider has tests for loading, but Phase 14 needs explicit precedence and agent-level obedience fixtures.
- Approval proposals exist, but the user-facing `/approve` and TUI approval loop is not closed.
- Final reporting is currently prompt guidance, not a state-driven/eval-checked contract.
- No deterministic eval harness currently proves the six Phase 14 behavior families.

### Integration Points
- Add or deepen a shared code-agent factory used by CLI and runtime callers.
- Move repo context prompt material into a shared provider attached by the factory.
- Extend project-instruction tests for nested precedence and same-directory file precedence.
- Add approval proposal IDs and expose approval handling through CLI/TUI paths backed by `RuntimeSecurity.approve_rule`.
- Add final-report aggregation/checking from task/recovery state with git sanity checks.
- Add Rust integration fixtures for dirty worktree safety, instruction obedience, targeted reading, patch recovery, verification honesty, and profile isolation.

</code_context>

<specifics>
## Specific Ideas

The user consistently chose the strongest Phase 14 closure path: shared Rust factory, strong runtime-built guarantees, explicit approval loop, state-driven final reporting, and full six-fixture eval coverage.

Phase 14 should treat the v1.2 audit blockers as direct implementation targets. It should close the main product loop rather than producing only lower-level library tests.

</specifics>

<deferred>
## Deferred Ideas

- Expanding `@file` or include syntax inside `LATTICE.md`, `AGENTS.md`, or `CLAUDE.md` is deferred to a later instruction-loader enhancement.
- Rewriting the full `.planning/research/CC_CODEX_GAP_ANALYSIS.md` is not required in Phase 14; reference and coverage summary are enough.
- A standalone eval harness binary or productized eval platform is deferred; Phase 14 uses Rust integration tests.

</deferred>

---

*Phase: 14-Main Code Agent & Eval Harness*
*Context gathered: 2026-05-12*

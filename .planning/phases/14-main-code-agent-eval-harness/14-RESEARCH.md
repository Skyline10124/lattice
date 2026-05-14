# Phase 14: Main Code Agent & Eval Harness - Research

**Researched:** 2026-05-12 [VERIFIED: system date]  
**Domain:** Rust CLI/runtime code-agent assembly, prompt providers, permission approval, task state, and deterministic integration evals [VERIFIED: .planning/phases/14-main-code-agent-eval-harness/14-CONTEXT.md]  
**Confidence:** HIGH for local architecture and required behavior; MEDIUM for exact final module placement because CONTEXT.md leaves that to planner discretion [VERIFIED: .planning/phases/14-main-code-agent-eval-harness/14-CONTEXT.md]

<user_constraints>
## User Constraints (from CONTEXT.md)

Source for this whole section: `.planning/phases/14-main-code-agent-eval-harness/14-CONTEXT.md`. The subsections below are copied verbatim except for this source note. [VERIFIED: .planning/phases/14-main-code-agent-eval-harness/14-CONTEXT.md]

### Locked Decisions

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

### Deferred Ideas (OUT OF SCOPE)
- Expanding `@file` or include syntax inside `LATTICE.md`, `AGENTS.md`, or `CLAUDE.md` is deferred to a later instruction-loader enhancement.
- Rewriting the full `.planning/research/CC_CODEX_GAP_ANALYSIS.md` is not required in Phase 14; reference and coverage summary are enough.
- A standalone eval harness binary or productized eval platform is deferred; Phase 14 uses Rust integration tests.
</user_constraints>

<phase_requirements>
## Phase Requirements

| ID | Description | Research Support |
|----|-------------|------------------|
| CODE-01 | `lattice code` is backed by an explicit code-agent profile rather than one-off hard-coded CLI behavior. [VERIFIED: .planning/REQUIREMENTS.md] | Use a Rust built-in code-agent factory as the single source of truth; CLI/TUI call it instead of owning prompt/context/task wiring. [VERIFIED: 14-CONTEXT.md; crates/lattice-cli/src/coding_agent.rs; crates/lattice-runtime/src/lib.rs] |
| CODE-02 | Code agent loads hierarchical project instructions from `LATTICE.md`, `AGENTS.md`, and `CLAUDE.md` through provider/context mechanisms. [VERIFIED: .planning/REQUIREMENTS.md] | Fix provider wiring so it receives project root plus cwd, and add precedence tests for directory depth and same-directory file order. [VERIFIED: crates/lattice-prompt/src/project_instructions_provider.rs; crates/lattice-runtime/src/lib.rs; 14-CONTEXT.md] |
| CODE-03 | Code agent starts substantial work with workspace context and uses targeted file reads/searches instead of broad dumps. [VERIFIED: .planning/REQUIREMENTS.md] | Move CLI repo context into a shared workspace provider and eval scripted tool order: workspace_context before grep/read_file/read_files. [VERIFIED: crates/lattice-cli/src/coding_agent.rs; crates/lattice-tools/src/tool_definitions.rs; crates/lattice-tools/src/executor.rs] |
| CODE-04 | Code agent protects unrelated dirty worktree changes and reports changed files, verification, and residual risk in final answers. [VERIFIED: .planning/REQUIREMENTS.md] | Build final reports from TaskState/edit manifests, verify against git porcelain state, and reject `passed` unless a command actually succeeded. [VERIFIED: crates/lattice-agent/src/state.rs; crates/lattice-agent/src/tool_result.rs; 14-CONTEXT.md; CITED: https://git-scm.com/docs/git-status] |
| CODE-05 | Code agent uses the new editing, planning, permission, and session capabilities as its default working loop. [VERIFIED: .planning/REQUIREMENTS.md] | The factory must attach `code-agent` capabilities, authorized executor, RuntimeSecurity, TaskStateProvider, session resume seed, patch, todo, approval, and final reporting. [VERIFIED: crates/lattice-tools/src/capabilities.rs; crates/lattice-runtime/src/lib.rs; crates/lattice-agent/src/config.rs; crates/lattice-cli/src/coding_agent.rs] |
| EVAL-01 | Evaluation fixtures cover dirty worktree safety, project instruction obedience, targeted file reading, patch recovery, and verification honesty. [VERIFIED: .planning/REQUIREMENTS.md] | Add deterministic Rust integration fixtures with temporary repos and scripted model/tool sequences for each behavior family. [VERIFIED: 14-CONTEXT.md; crates/lattice-agent/tests/agent_mock.rs; crates/lattice-agent/tests/context_e2e.rs] |
| EVAL-02 | Evaluation fixtures prove coding capabilities do not leak into non-coding profiles. [VERIFIED: .planning/REQUIREMENTS.md] | Extend existing exposure/profile tests to code-agent factory level, not only `lattice-tools` resolver level. [VERIFIED: crates/lattice-tools/tests/exposure.rs; crates/lattice-runtime/src/lib.rs] |
| EVAL-03 | Test commands for touched crates are documented per phase and included in verification criteria. [VERIFIED: .planning/REQUIREMENTS.md] | Use per-crate quick commands and full workspace gate listed in Validation Architecture. [VERIFIED: AGENTS.md; .planning/config.json] |
| EVAL-04 | Gap analysis against Codex and Claude Code is linked as research input and kept current for this milestone. [VERIFIED: .planning/REQUIREMENTS.md] | Reference `.planning/research/CC_CODEX_GAP_ANALYSIS.md` in plan, eval names, and verification summary; do not rewrite the document. [VERIFIED: .planning/research/CC_CODEX_GAP_ANALYSIS.md; 14-CONTEXT.md] |
</phase_requirements>

## Summary

Phase 14 should be planned as a product-loop closure phase: a shared Rust code-agent factory builds the flagship agent, and the CLI/TUI become entrypoints that pass prompt/session/display inputs into that factory. [VERIFIED: 14-CONTEXT.md; crates/lattice-cli/src/coding_agent.rs; crates/lattice-runtime/src/lib.rs] The current code already has many primitives: `AgentSpec.capability_preset("code-agent")`, `ToolPreset::CodeAgent`, structured `patch`, `RuntimeSecurity.approve_rule`, `TaskStateProvider`, manifest metadata, and temporary-repo test patterns. [VERIFIED: crates/lattice-runtime/src/lib.rs; crates/lattice-tools/src/capabilities.rs; crates/lattice-agent/src/config.rs; crates/lattice-agent/src/manifest.rs; crates/lattice-agent/tests/agent_mock.rs]

The main gaps are assembly drift and product-level proof: repo context and default prompt still live in CLI code, runtime-built code agents do not automatically get the same workspace/recovery semantics, project instruction wiring uses `ProjectInstructionsProvider::from_root(project_root)` rather than root-to-cwd context, `/approve` exists only as an unused helper, and final reporting is still prompt guidance rather than a state-driven contract. [VERIFIED: crates/lattice-cli/src/coding_agent.rs; crates/lattice-runtime/src/lib.rs; crates/lattice-cli/src/security.rs; crates/lattice-cli/src/frontend/tui/app.rs; .planning/v1.2-MILESTONE-AUDIT.md]

**Primary recommendation:** implement `code_agent` as a shared Rust factory path that owns capability/security/provider/task/final-report wiring, then prove it with deterministic Rust integration evals using temporary repositories and scripted model behavior. [VERIFIED: 14-CONTEXT.md; .planning/REQUIREMENTS.md]

## Project Constraints (from AGENTS.md)

- Build and validation commands are `cargo build`, `cargo test`, `cargo clippy -- -D warnings`, `cargo fmt --all --check`, single-test `cargo test <test_name>`, and `cargo bench`. [VERIFIED: AGENTS.md]
- The workspace dependency flow is `hooks -> core -> session/prompt/sandbox/tools -> agent -> plugin -> orchestrator -> runtime -> cli`; Phase 14 plans must not introduce reverse dependencies. [VERIFIED: AGENTS.md; Cargo.toml]
- `Runtime` / `RuntimeBuilder`, `ResolvedModel`, `StreamEvent`, `Agent`, `Pipeline`, `Plugin`, `EventBus`, and `SandboxConfig` are named key types in the project architecture. [VERIFIED: AGENTS.md]
- `lattice-plugins` feature-gates official plugins and enables all listed plugin features by default. [VERIFIED: AGENTS.md; crates/lattice-plugins/Cargo.toml]
- Environment credentials are loaded from `.env`/config and only set keys are used. [VERIFIED: AGENTS.md; .env.example; crates/lattice-cli/src/credentials.rs]
- No project-local `.codex/skills/**/SKILL.md` or `.agents/skills/**/SKILL.md` files were found, so Phase 14 does not need extra project skill rules. [VERIFIED: find .codex/.agents commands returned no SKILL.md]

## Architectural Responsibility Map

| Capability | Primary Tier | Secondary Tier | Rationale |
|------------|--------------|----------------|-----------|
| Code-agent factory | `lattice-runtime` or runtime-facing shared module | `lattice-cli` entrypoints | The factory must build reusable runtime agents and CLI agents through one path; CLI remains thin. [VERIFIED: 14-CONTEXT.md; crates/lattice-runtime/src/lib.rs; crates/lattice-cli/src/coding_agent.rs] |
| Capability exposure and authorization | `lattice-tools` plus `lattice-runtime` assembly | `lattice-orchestrator` profile assembly | Tool bundles/presets are owned by `lattice-tools`, while runtime applies resolved definitions and executor authorization. [VERIFIED: crates/lattice-tools/src/capabilities.rs; crates/lattice-runtime/src/lib.rs] |
| Project instructions | `lattice-prompt` provider | code-agent factory wiring | Loading belongs in provider/context mechanisms, but the factory must pass the correct root and cwd. [VERIFIED: crates/lattice-prompt/src/project_instructions_provider.rs; 14-CONTEXT.md] |
| Workspace/repo context | Shared context provider | `workspace_context` tool | Startup summary should not stay CLI-private; `workspace_context` remains the model-invoked refresh tool. [VERIFIED: crates/lattice-cli/src/coding_agent.rs; crates/lattice-tools/src/tool_definitions.rs] |
| Approval retry loop | `lattice-runtime` security + `lattice-policy` approval types | CLI/TUI slash command handlers | Approval state must flow through shared `RuntimeSecurity.approve_rule`; UI layers only parse/dispatch user commands. [VERIFIED: crates/lattice-runtime/src/lib.rs; crates/lattice-policy/src/approval.rs; crates/lattice-cli/src/security.rs] |
| Task/recovery state | `lattice-agent` state and manifest metadata | `lattice-session` persistence | The live state model, provider, and manifest emission are agent-owned; session persists metadata unchanged. [VERIFIED: crates/lattice-agent/src/state.rs; crates/lattice-agent/src/turn_engine.rs; crates/lattice-cli/src/session.rs] |
| Final reporting | shared renderer/validator near agent/runtime | CLI/TUI display | Required sections must be generated/checked from state, not trusted from model prose alone. [VERIFIED: 14-CONTEXT.md; crates/lattice-agent/src/state.rs] |
| Eval harness | Rust integration tests | per-crate unit tests | Evals must be deterministic and run under `cargo test`; unit tests remain for provider/parser/factory seams. [VERIFIED: 14-CONTEXT.md; .planning/config.json; crates/lattice-agent/tests/agent_mock.rs] |

## Standard Stack

### Core

| Library/Crate | Version | Purpose | Why Standard |
|---------------|---------|---------|--------------|
| Rust workspace | edition 2021, rust-version 1.85, local crates 0.1.0 | All implementation and eval code | Existing workspace standard and dependency graph. [VERIFIED: Cargo.toml; cargo tree] |
| `lattice-runtime` | 0.1.0 | Runtime builder, `AgentSpec`, `RuntimeSecurity`, shared agent construction | Correct home for reusable code-agent assembly because CLI already calls runtime build paths. [VERIFIED: crates/lattice-runtime/src/lib.rs; cargo tree -p lattice-runtime] |
| `lattice-agent` | 0.1.0 | Agent loop, task state, prompt providers, manifest metadata | Owns live task/recovery state and tool-result classification. [VERIFIED: crates/lattice-agent/src/state.rs; crates/lattice-agent/src/turn_engine.rs; cargo tree -p lattice-agent] |
| `lattice-prompt` | 0.1.0 | `ContextProvider` and `ProjectInstructionsProvider` | Required for provider-based instruction/context loading. [VERIFIED: crates/lattice-prompt/src/project_instructions_provider.rs; cargo tree -p lattice-prompt] |
| `lattice-tools` | 0.1.0 | Capability resolver and default tool executor | Owns bundle/preset membership and executor authorization. [VERIFIED: crates/lattice-tools/src/capabilities.rs; crates/lattice-tools/src/executor.rs; cargo tree -p lattice-tools] |
| `lattice-policy` / `lattice-sandbox` | 0.1.0 | Approval rules, denial reports, risk classes, sandbox checks | Existing policy/security foundation for `/approve` and denied-operation retry. [VERIFIED: crates/lattice-policy/src/approval.rs; crates/lattice-policy/src/proposal.rs; crates/lattice-sandbox/src/checks.rs; cargo tree] |
| `lattice-session` | 0.1.0 | SQLite/CAS session graph and turn manifests | Persists manifest metadata and resume inputs. [VERIFIED: crates/lattice-session/src/lib.rs; crates/lattice-cli/src/session.rs; cargo tree -p lattice-session] |
| `lattice-cli` | 0.1.0 | CLI and ratatui TUI entrypoints | Should become the display/session shell around the shared code-agent factory. [VERIFIED: crates/lattice-cli/src/main.rs; crates/lattice-cli/src/frontend/tui/app.rs; cargo tree -p lattice-cli] |

### Supporting

| Library | Resolved Version | Purpose | When to Use |
|---------|------------------|---------|-------------|
| `tokio` | 1.52.2 | async tests, mock server tasks, runtime operations | Use for deterministic integration tests and existing agent async APIs. [VERIFIED: cargo tree] |
| `serde` / `serde_json` | 1.0.228 / 1.0.149 | task state, denial/proposal markers, manifests, fixture assertions | Use for typed state and JSON tool results; avoid ad hoc string protocols. [VERIFIED: cargo tree; crates/lattice-agent/src/state.rs; crates/lattice-policy/src/proposal.rs] |
| `tempfile` | 3.27.0 | temporary repositories and fixture workdirs | Use for deterministic eval repos and provider tests. [VERIFIED: cargo tree -p lattice-tools; crates/lattice-tools/src/executor.rs tests] |
| `ratatui` / `crossterm` | 0.30.0 / 0.28.1 | TUI command surface | Use only for TUI `/approve` entry/rendering, not for approval policy logic. [VERIFIED: cargo tree -p lattice-cli; crates/lattice-cli/src/frontend/tui/app.rs] |
| `clap` | 4.6.1 | CLI subcommands and flags | Existing command parser for `lattice code` and `lattice run`. [VERIFIED: cargo tree -p lattice-cli; crates/lattice-cli/src/main.rs] |
| `git` CLI | 2.54.0 installed | worktree sanity checks and temporary repo evals | Use porcelain output for stable machine checks. [VERIFIED: git --version; CITED: https://git-scm.com/docs/git-status] |

### Alternatives Considered

| Instead of | Could Use | Tradeoff |
|------------|-----------|----------|
| Rust built-in factory | TOML/PromptSpec-first migration | Out of scope because Phase 14 locked Rust built-in factory as authoritative. [VERIFIED: 14-CONTEXT.md] |
| Rust integration tests | standalone eval binary | Out of scope because Phase 14 locked Cargo-test evals and deferred a productized harness. [VERIFIED: 14-CONTEXT.md] |
| Shared `RuntimeSecurity.approve_rule` | duplicate CLI/TUI approval lists | Violates locked approval decision and risks divergent semantics. [VERIFIED: 14-CONTEXT.md; crates/lattice-runtime/src/lib.rs] |
| Provider-based instruction loading | embed all instruction text in CLI system prompt | Violates CODE-02 and leaves runtime-built code agents weaker than CLI-built agents. [VERIFIED: .planning/REQUIREMENTS.md; crates/lattice-cli/src/coding_agent.rs] |

**Installation:**
```bash
# No new dependency is required for the recommended Phase 14 stack.
cargo test -p lattice-runtime code_agent_preset_exposes_workspace_editing_shell_planning_and_web
```
[VERIFIED: Cargo.toml; cargo tree; test command run 2026-05-12]

**Version verification:** Recommended dependencies are existing workspace crates and already-resolved crates from `Cargo.lock`; I verified them with `cargo tree -p lattice-cli`, `cargo tree -p lattice-runtime`, `cargo tree -p lattice-agent`, `cargo tree -p lattice-tools`, `cargo tree -p lattice-prompt`, `cargo tree -p lattice-policy`, `cargo tree -p lattice-sandbox`, and `cargo tree -p lattice-session`. [VERIFIED: cargo tree outputs]

## Architecture Patterns

### System Architecture Diagram

```text
--------------------+       +---------------------------------------+
| CLI `lattice code` | ----> | shared Rust code_agent factory        |
| TUI message submit | ----> | - validates project root/security     |
+--------------------+       | - selects code-agent preset           |
                             | - attaches executor + RuntimeSecurity |
                             | - attaches instruction/provider stack |
                             | - attaches task/recovery state        |
                             +------------------+--------------------+
                                                |
                                                v
                         +----------------------+----------------------+
                         | Agent prompt/runtime loop                   |
                         | system prompt + providers + scripted tools |
                         +----------+----------+-----------+-----------+
                                    |          |           |
                                    v          v           v
                           tools/executor  TaskState   Turn manifests
                           patch/bash/etc  recovery    session metadata
                                    |          |           |
                                    +----------+-----------+
                                               |
                                               v
                           Final report renderer/validator
                           Changed Files / Verification / Residual Risk
                                               |
                                               v
                               Rust deterministic eval fixtures
```
[VERIFIED: 14-CONTEXT.md; crates/lattice-cli/src/coding_agent.rs; crates/lattice-runtime/src/lib.rs; crates/lattice-agent/src/turn_engine.rs]

### Recommended Project Structure

```text
crates/
├── lattice-runtime/src/code_agent.rs       # shared factory and CodeAgentBuildOptions
├── lattice-prompt/src/workspace_provider.rs # shared startup repo/workspace context
├── lattice-prompt/src/project_instructions_provider.rs # precedence/root-to-cwd fixes
├── lattice-agent/src/final_report.rs       # state-driven final report renderer/checker
├── lattice-cli/src/coding_agent.rs         # thin CLI wrapper around shared factory
├── lattice-cli/src/frontend/tui/app.rs     # TUI /approve dispatch and display
└── lattice-cli/tests/code_agent_eval.rs    # deterministic Phase 14 product fixtures
```
[RECOMMENDED FROM: 14-CONTEXT.md; VERIFIED CURRENT GAPS: crates/lattice-cli/src/coding_agent.rs; crates/lattice-runtime/src/lib.rs]

### Pattern 1: Shared Code-Agent Factory

**What:** Build code agents through one runtime-facing function that validates inputs and attaches all required providers/executors/state. [VERIFIED: 14-CONTEXT.md]  
**When to use:** Every `lattice code`, TUI code turn, and runtime-built code-agent path. [VERIFIED: crates/lattice-cli/src/coding_agent.rs; crates/lattice-cli/src/frontend/tui/app.rs]

```rust
// Source pattern: current pieces in crates/lattice-cli/src/coding_agent.rs and crates/lattice-runtime/src/lib.rs.
let built = runtime.build_code_agent(CodeAgentSpec {
    model,
    provider_override,
    project_root,
    cwd,
    messages,
    security,
    task_state_seed,
})?;
```
[RECOMMENDED FROM: 14-CONTEXT.md]

### Pattern 2: Provider-Based Context Instead of CLI Prompt Stuffing

**What:** Move `repo_context()` semantics into a shared provider and attach it in the factory beside `ProjectInstructionsProvider`. [VERIFIED: crates/lattice-cli/src/coding_agent.rs; crates/lattice-prompt/src/project_instructions_provider.rs]  
**When to use:** Startup workspace summary, project instruction file discovery, likely verification command hints, and git dirty-state summary. [VERIFIED: crates/lattice-cli/src/coding_agent.rs; crates/lattice-tools/src/executor.rs]

```rust
// Source pattern: Agent::with_provider and ProjectInstructionsProvider existing APIs.
agent = agent
    .with_provider(ProjectInstructionsProvider::new(project_root, cwd))
    .with_provider(WorkspaceContextProvider::new(project_root, cwd))
    .with_task_state_provider();
```
[RECOMMENDED FROM: 14-CONTEXT.md; VERIFIED API: crates/lattice-agent/src/config.rs; crates/lattice-prompt/src/project_instructions_provider.rs]

### Pattern 3: `/approve` Parses Proposal State, Then Calls RuntimeSecurity

**What:** Store proposal IDs in task/recovery/session state, parse `/approve last` or `/approve <id>`, convert the proposal to `ApprovedRule`, and call `RuntimeSecurity.approve_rule`. [VERIFIED: 14-CONTEXT.md; crates/lattice-runtime/src/lib.rs; crates/lattice-policy/src/proposal.rs]  
**When to use:** CLI slash command, TUI slash command, and eval retry fixtures. [VERIFIED: crates/lattice-cli/src/frontend/tui/app.rs; crates/lattice-cli/src/security.rs]

```rust
// Source pattern: RuntimeSecurity::approve_rule already validates and stores rules.
let proposal = proposal_store.resolve(selector)?;
let rule = proposal.to_approved_rule()?;
security.approve_rule(rule)?;
```
[RECOMMENDED FROM: 14-CONTEXT.md; VERIFIED API: crates/lattice-runtime/src/lib.rs]

### Pattern 4: State-Driven Final Report

**What:** Render `Changed Files`, `Verification`, and `Residual Risk` from `TaskState`, successful verification records, and git sanity checks; then assert the final model-facing/user-facing answer matches that state. [VERIFIED: 14-CONTEXT.md; crates/lattice-agent/src/state.rs]  
**When to use:** End of code-agent runs and eval assertions for verification honesty. [VERIFIED: .planning/REQUIREMENTS.md]

```rust
// Source pattern: TaskState already records changed files, failed verifications, and unverified work.
let report = FinalReport::from_task_state(&task_state)
    .with_git_status(git_status_porcelain(project_root)?)
    .render_human_sections();
```
[RECOMMENDED FROM: 14-CONTEXT.md; CITED: https://git-scm.com/docs/git-status]

### Pattern 5: Scripted Model Evals

**What:** Use mock OpenAI-compatible SSE responses or a small scripted model adapter to force tool-call sequences and final text without live provider calls. [VERIFIED: crates/lattice-agent/tests/agent_mock.rs; 14-CONTEXT.md]  
**When to use:** Dirty worktree, instruction obedience, targeted reads/searches, patch recovery, approval retry, verification honesty, and profile isolation fixtures. [VERIFIED: 14-CONTEXT.md]

### Anti-Patterns to Avoid

- **CLI-owned code-agent behavior:** keeping `coding_system_prompt()`, `repo_context()`, task-state seeding, and provider selection as the authoritative implementation in `lattice-cli` leaves runtime-built code agents weaker. [VERIFIED: crates/lattice-cli/src/coding_agent.rs; .planning/v1.2-MILESTONE-AUDIT.md]
- **Provider loaded with root only:** `Runtime::build_agent()` currently uses `ProjectInstructionsProvider::from_root(project_root)`, which cannot express root-to-cwd hierarchy when project root and cwd differ. [VERIFIED: crates/lattice-runtime/src/lib.rs; crates/lattice-prompt/src/project_instructions_provider.rs]
- **Same-directory precedence by prompt wish alone:** current provider reads `LATTICE.md`, `AGENTS.md`, then `CLAUDE.md`; tests must prove this satisfies `LATTICE.md > AGENTS.md > CLAUDE.md` rather than relying on model interpretation. [VERIFIED: crates/lattice-prompt/src/project_instructions_provider.rs; 14-CONTEXT.md]
- **Auto-replaying approved commands:** locked decision says approval only updates session-scoped security; user explicitly continues/retries. [VERIFIED: 14-CONTEXT.md]
- **Prompt-only final report:** audit already marks prompt-only reporting as insufficient for CODE-04. [VERIFIED: .planning/v1.2-MILESTONE-AUDIT.md]

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| Capability membership | ad hoc code-agent tool list in CLI | `ToolPreset::CodeAgent` and `resolve_capabilities` | Existing resolver owns bundles and tests profile isolation. [VERIFIED: crates/lattice-tools/src/capabilities.rs; crates/lattice-tools/tests/exposure.rs] |
| Tool authorization | schema-only filtering | `DefaultToolExecutor::with_authorized_tools` plus RuntimeSecurity sandbox | Executor-side denial prevents manually constructed unauthorized calls. [VERIFIED: crates/lattice-tools/src/executor.rs; 10-CONTEXT.md] |
| Instruction hierarchy | string concatenation in system prompt | `ProjectInstructionsProvider` with root/cwd and precedence tests | CODE-02 requires provider/context mechanisms. [VERIFIED: .planning/REQUIREMENTS.md; crates/lattice-prompt/src/project_instructions_provider.rs] |
| Approval policy | separate CLI/TUI approval stores | `RuntimeSecurity.approve_rule` and `lattice-policy` approval types | Shared runtime path already validates narrowness and stores rules. [VERIFIED: crates/lattice-runtime/src/lib.rs; crates/lattice-policy/src/approval.rs] |
| Final-report honesty | parsing model prose | `TaskState`, edit manifests, verification records, and git porcelain sanity check | State exists and is more reliable than prose; git porcelain is intended for machine parsing. [VERIFIED: crates/lattice-agent/src/state.rs; CITED: https://git-scm.com/docs/git-status] |
| Eval behavior | live model calls | deterministic Rust tests with mock/scripted model responses | Locked decision requires stable CI-safe evals. [VERIFIED: 14-CONTEXT.md; crates/lattice-agent/tests/agent_mock.rs] |

**Key insight:** Phase 14 should compose and validate existing platform capabilities; inventing parallel CLI-specific logic will recreate the drift this phase is intended to close. [VERIFIED: 14-CONTEXT.md; .planning/v1.2-MILESTONE-AUDIT.md]

## Runtime State Inventory

| Category | Items Found | Action Required |
|----------|-------------|-----------------|
| Stored data | Session turns and turn manifests are persisted under the CLI session manager default directory, and manifest metadata can contain `TaskState` JSON. [VERIFIED: crates/lattice-cli/src/session.rs; crates/lattice-session/src/lib.rs] | Do not change existing `TaskState` JSON shape unless a migration or tolerant deserializer is planned; proposal IDs added to recovery state need resume tests. [VERIFIED: crates/lattice-agent/src/state.rs; 14-CONTEXT.md] |
| Live service config | No external live service config was found in repo for Phase 14; `.lattice/agents` does not exist in this checkout. [VERIFIED: find .lattice returned no directory] | None for external services; keep runtime-built defaults in code, not untracked UI config. [VERIFIED: 14-CONTEXT.md] |
| OS-registered state | No OS-level registrations are part of the Phase 14 code-agent path. [VERIFIED: AGENTS.md; .planning/REQUIREMENTS.md] | None. [VERIFIED: no Phase 14 requirement references OS registration] |
| Secrets/env vars | Provider credentials are environment/config keys such as `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, and `GITHUB_TOKEN`; Phase 14 does not rename them. [VERIFIED: .env.example; crates/lattice-cli/src/credentials.rs; crates/lattice-core/src/router.rs] | No secret-key migration; do not serialize credentials into eval fixtures or final reports. [VERIFIED: crates/lattice-cli/src/credentials.rs; AGENTS.md] |
| Build artifacts | `target/` exists and will contain pre-Phase-14 compiled artifacts until rebuild. [VERIFIED: find target command] | Normal `cargo test`/`cargo build` rebuild is sufficient; no package reinstall or artifact migration required. [VERIFIED: Cargo.toml; AGENTS.md] |

**Nothing found in category:** Live service config and OS-registered state have no Phase 14 runtime artifacts in this checkout. [VERIFIED: find .lattice; .planning/REQUIREMENTS.md]

## Common Pitfalls

### Pitfall 1: Mistaking `code-agent` Schemas for a Code-Agent Product
**What goes wrong:** `AgentSpec.capability_preset("code-agent")` exposes tools but does not by itself guarantee security, project root, workspace context, task recovery, approval loop, or final reporting. [VERIFIED: crates/lattice-runtime/src/lib.rs; .planning/v1.2-MILESTONE-AUDIT.md]  
**How to avoid:** Make factory validation fail when critical inputs are missing and assert factory-built agents have the full provider/executor/state stack. [VERIFIED: 14-CONTEXT.md]

### Pitfall 2: Nested Instructions Loaded from the Wrong Root
**What goes wrong:** `ProjectInstructionsProvider::from_root(project_root)` only sets cwd to the same path, so a nested cwd cannot inherit parent rules through that path. [VERIFIED: crates/lattice-runtime/src/lib.rs; crates/lattice-prompt/src/project_instructions_provider.rs]  
**How to avoid:** Use `ProjectInstructionsProvider::new(project_root, cwd)` in the code-agent factory and test root-to-child rendering. [VERIFIED: crates/lattice-prompt/src/project_instructions_provider.rs; 14-CONTEXT.md]

### Pitfall 3: Same-Directory File Precedence Is Unproven
**What goes wrong:** The current provider includes all three file names but only has a load-all test; it does not test conflict precedence. [VERIFIED: crates/lattice-prompt/src/project_instructions_provider.rs]  
**How to avoid:** Add deterministic tests where `LATTICE.md`, `AGENTS.md`, and `CLAUDE.md` conflict and assert `LATTICE.md` wins. [VERIFIED: 14-CONTEXT.md]

### Pitfall 4: Approval IDs Live Only in Text
**What goes wrong:** Current proposal markers include `/approve ...` text, but no stable proposal ID store was found. [VERIFIED: crates/lattice-policy/src/proposal.rs; crates/lattice-agent/src/tool_result.rs]  
**How to avoid:** Add proposal records to task/recovery state and persist them through manifest metadata. [VERIFIED: 14-CONTEXT.md; crates/lattice-agent/src/manifest.rs]

### Pitfall 5: Verification Honesty Cannot Be Reconstructed from a Final Paragraph
**What goes wrong:** The model may say tests passed even when only a failed or not-run command exists. [VERIFIED: 14-CONTEXT.md; crates/lattice-agent/src/tool_result.rs]  
**How to avoid:** Track verification as `passed`, `failed`, or `not_run` and assert final report sections against state. [VERIFIED: 14-CONTEXT.md]

### Pitfall 6: Tests That Only Verify the Mock
**What goes wrong:** Scripted evals can accidentally assert that the script emitted a phrase rather than that the agent stack enforced behavior. [ASSUMED]  
**How to avoid:** Assert state changes, tool exposure, tool-call order, sandbox denial/approval outcomes, manifest metadata, and final rendered sections. [VERIFIED: crates/lattice-agent/tests/agent_mock.rs; crates/lattice-agent/src/turn_engine.rs]

## Code Examples

### Current CLI-Owned Code-Agent Assembly

```rust
// Source: crates/lattice-cli/src/coding_agent.rs
runtime.build_agent(
    AgentSpec::new(options.model)
        .provider_override(options.provider_override)
        .capability_preset("code-agent")
        .system_prompt(coding_system_prompt(&options.workdir))
        .project_root(options.workdir.to_string_lossy().to_string())
        .security(Some(options.security.clone())),
)?;
```
[VERIFIED: crates/lattice-cli/src/coding_agent.rs]

### Current Runtime Provider Gap

```rust
// Source: crates/lattice-runtime/src/lib.rs
let project_root = spec.project_root.clone().unwrap_or_else(|| ".".to_string());
agent = agent.with_provider(prompt::ProjectInstructionsProvider::from_root(
    project_root.clone(),
));
```
[VERIFIED: crates/lattice-runtime/src/lib.rs]

### Current Task-State Provider Hook

```rust
// Source: crates/lattice-agent/src/config.rs
pub fn with_task_state_provider(mut self) -> Self {
    let shared = self.state.shared_task_state();
    let provider = crate::prompt::TaskStateProvider::new().with_state(shared);
    self.registry.register(Box::new(provider));
    self
}
```
[VERIFIED: crates/lattice-agent/src/config.rs]

### Current Approval Entry Point

```rust
// Source: crates/lattice-runtime/src/lib.rs
pub fn approve_rule(&self, rule: ApprovedRule) -> Result<(), ValidationError> {
    self.policy_authority.approve(rule.clone(), None)?;
    Ok(())
}
```
[VERIFIED: crates/lattice-runtime/src/lib.rs]

## State of the Art

| Old Approach | Current/Required Approach | When Changed | Impact |
|--------------|---------------------------|--------------|--------|
| CLI-owned coding prompt and repo context | Shared Rust code-agent factory with providers | Phase 14 locked decision on 2026-05-12 | Prevents CLI/runtime drift. [VERIFIED: 14-CONTEXT.md] |
| Search/replace patch and `multi_patch` | One structured `patch` tool with changed-file manifest | Phase 11 completed 2026-05-10 | Code-agent evals should use structured patch recovery, not legacy surfaces. [VERIFIED: 11-CONTEXT.md; crates/lattice-tools/tests/exposure.rs] |
| Static denial text | `DenialReport` plus approval proposal marker and shared approval state | Phase 12 completed 2026-05-11, closure remains Phase 14 | Phase 14 must add user `/approve` retry path. [VERIFIED: 12-CONTEXT.md; crates/lattice-agent/src/tool_result.rs; .planning/v1.2-MILESTONE-AUDIT.md] |
| Plain text todos and raw tool outputs | `TaskState`, `TaskStateProvider`, and manifest metadata | Phase 13 completed 2026-05-11 | Final report and resume behavior should be state-driven. [VERIFIED: 13-CONTEXT.md; crates/lattice-agent/src/state.rs] |
| Ad hoc manual behavior checks | Deterministic Rust eval fixtures | Phase 14 requirement | EVAL-01..04 require CI-stable proof. [VERIFIED: .planning/REQUIREMENTS.md; 14-CONTEXT.md] |

**Deprecated/outdated:**
- `multi_patch` as a visible tool is retired; evals must assert no reintroduction. [VERIFIED: 11-CONTEXT.md; crates/lattice-tools/tests/exposure.rs]
- CLI-private `repo_context()` as authoritative code-agent context is outdated for Phase 14. [VERIFIED: 14-CONTEXT.md; crates/lattice-cli/src/coding_agent.rs]
- Prompt-only final reporting is insufficient. [VERIFIED: 14-CONTEXT.md; .planning/v1.2-MILESTONE-AUDIT.md]

## Assumptions Log

| # | Claim | Section | Risk if Wrong |
|---|-------|---------|---------------|
| A1 | Scripted evals can accidentally test the script instead of real enforcement. [ASSUMED] | Common Pitfalls | Planner should design eval assertions around state/tool/executor outcomes, not only output text. |
| A2 | ASVS category applicability below is a planning heuristic for this local CLI agent, not a formal compliance certification. [ASSUMED] | Security Domain | Planner should not treat the mapping as a compliance audit. |
| A3 | Research remains valid until 2026-06-11 if the branch does not change materially. [ASSUMED] | Metadata | Planner should re-check code refs and dependency versions if planning happens after substantial branch movement. |

## Open Questions

1. **Factory module placement**
   - What we know: Context allows `lattice-runtime`, `lattice-orchestrator`, or a small shared module. [VERIFIED: 14-CONTEXT.md]
   - What's unclear: Exact public API shape. [VERIFIED: 14-CONTEXT.md]
   - Recommendation: Put the first built-in factory in `lattice-runtime/src/code_agent.rs` because runtime already owns `AgentSpec`, `RuntimeSecurity`, session store access, and CLI-facing build APIs. [VERIFIED: crates/lattice-runtime/src/lib.rs]

2. **Proposal ID storage shape**
   - What we know: Proposal IDs must support `/approve last`, `/approve <id>`, session resume, and tests. [VERIFIED: 14-CONTEXT.md]
   - What's unclear: Whether IDs belong directly in `TaskState` or a nested approval-proposal recovery collection. [VERIFIED: crates/lattice-agent/src/state.rs]
   - Recommendation: Add typed proposal records to task/recovery state so manifest metadata and `TaskStateProvider` can surface pending approvals. [VERIFIED: crates/lattice-agent/src/state.rs; crates/lattice-agent/src/manifest.rs]

3. **Final report interception point**
   - What we know: Required sections are fixed and state-driven, but not JSON. [VERIFIED: 14-CONTEXT.md]
   - What's unclear: Whether the renderer should append after model output, validate model output, or both. [VERIFIED: no final-report module found via rg]
   - Recommendation: Build a renderer plus validator; use renderer for deterministic eval expectations and validator to fail evals when model text contradicts state. [RECOMMENDED FROM: 14-CONTEXT.md]

## Environment Availability

| Dependency | Required By | Available | Version | Fallback |
|------------|-------------|-----------|---------|----------|
| Rust compiler | all code and tests | yes | rustc 1.95.0 | none needed; workspace MSRV is 1.85. [VERIFIED: rustc --version; Cargo.toml] |
| Cargo | build/test/bench | yes | cargo 1.95.0 | none. [VERIFIED: cargo --version] |
| Git | dirty worktree evals and final-report sanity checks | yes | git 2.54.0 | skip git sanity checks only for non-git temp fixtures. [VERIFIED: git --version] |
| ripgrep | research and possible shell fixture searches | yes | ripgrep 15.1.0 | use built-in `grep` tool in agent fixtures. [VERIFIED: rg --version; crates/lattice-tools/src/tool_definitions.rs] |
| SQLite CLI | manual session inspection only | yes | sqlite3 3.53.1 | tests use `rusqlite` directly. [VERIFIED: sqlite3 --version; crates/lattice-session/src/lib.rs] |

**Missing dependencies with no fallback:** None found. [VERIFIED: environment audit commands]  
**Missing dependencies with fallback:** None found for required Phase 14 work. [VERIFIED: environment audit commands]

## Validation Architecture

### Test Framework

| Property | Value |
|----------|-------|
| Framework | Rust `cargo test` with `tokio` tests and `tempfile` temp repos. [VERIFIED: Cargo.toml; crates/lattice-agent/tests/agent_mock.rs; crates/lattice-tools/src/executor.rs] |
| Config file | root `Cargo.toml`, workspace resolver 2. [VERIFIED: Cargo.toml] |
| Quick run command | `cargo test -p lattice-cli code_agent_eval --test code_agent_eval` once the eval file exists. [RECOMMENDED FROM: 14-CONTEXT.md] |
| Full suite command | `cargo test --workspace` plus `cargo clippy --workspace -- -D warnings` and `cargo fmt --all --check`. [VERIFIED: AGENTS.md] |

### Phase Requirements -> Test Map

| Req ID | Behavior | Test Type | Automated Command | File Exists? |
|--------|----------|-----------|-------------------|--------------|
| CODE-01 | CLI/TUI and runtime code agents use the same factory. [VERIFIED: .planning/REQUIREMENTS.md] | integration/unit | `cargo test -p lattice-runtime code_agent_factory` and `cargo test -p lattice-cli code_agent_uses_shared_factory` [RECOMMENDED] | no, Wave 0 [VERIFIED: rg code_agent_factory returned none] |
| CODE-02 | Root-to-cwd project instruction precedence and agent-level obedience. [VERIFIED: .planning/REQUIREMENTS.md] | unit + integration | `cargo test -p lattice-prompt project_instruction_precedence` and `cargo test -p lattice-cli instruction_obedience --test code_agent_eval` [RECOMMENDED] | partial: provider exists, precedence tests missing [VERIFIED: crates/lattice-prompt/src/project_instructions_provider.rs] |
| CODE-03 | Workspace context plus targeted grep/read usage for substantial work. [VERIFIED: .planning/REQUIREMENTS.md] | integration eval | `cargo test -p lattice-cli targeted_file_reading --test code_agent_eval` [RECOMMENDED] | no, Wave 0 [VERIFIED: find Phase 14 files] |
| CODE-04 | Dirty-worktree protection and final sections with honest verification state. [VERIFIED: .planning/REQUIREMENTS.md] | integration eval | `cargo test -p lattice-cli dirty_worktree_final_report --test code_agent_eval` [RECOMMENDED] | no, Wave 0 [VERIFIED: rg "FinalReport" found no module] |
| CODE-05 | Default loop includes patch, todo, permission approval, session resume, and recovery retry. [VERIFIED: .planning/REQUIREMENTS.md] | integration eval | `cargo test -p lattice-cli approval_retry_recovery --test code_agent_eval` [RECOMMENDED] | partial lower-level tests exist; product eval missing [VERIFIED: crates/lattice-agent/src/tool_result.rs; crates/lattice-runtime/src/lib.rs] |
| EVAL-01 | Five behavior families except profile isolation. [VERIFIED: .planning/REQUIREMENTS.md] | eval suite | `cargo test -p lattice-cli --test code_agent_eval` [RECOMMENDED] | no, Wave 0 [VERIFIED: crates/lattice-cli has no tests/code_agent_eval.rs] |
| EVAL-02 | Coding capabilities do not leak to non-coding profiles. [VERIFIED: .planning/REQUIREMENTS.md] | unit/integration | `cargo test -p lattice-tools --test exposure` and `cargo test -p lattice-runtime profile_isolation` [VERIFIED/RECOMMENDED] | partial exists [VERIFIED: crates/lattice-tools/tests/exposure.rs; crates/lattice-runtime/src/lib.rs] |
| EVAL-03 | Phase test commands documented. [VERIFIED: .planning/REQUIREMENTS.md] | documentation/verification | `cargo test --workspace` and per-crate quick commands in PLAN.md/VERIFICATION.md [RECOMMENDED] | research now documents them [VERIFIED: this file] |
| EVAL-04 | Gap analysis linked and coverage summarized. [VERIFIED: .planning/REQUIREMENTS.md] | documentation + eval naming | `rg "CC_CODEX_GAP_ANALYSIS|dirty worktree|verification honesty" .planning/phases/14-main-code-agent-eval-harness crates/lattice-cli/tests` [RECOMMENDED] | partial source exists; Phase 14 summary missing [VERIFIED: .planning/research/CC_CODEX_GAP_ANALYSIS.md] |

### Sampling Rate

- **Per task commit:** run the touched crate quick command, for example `cargo test -p lattice-runtime code_agent_factory` or `cargo test -p lattice-prompt project_instruction_precedence`. [RECOMMENDED FROM: AGENTS.md; .planning/config.json]
- **Per wave merge:** run `cargo test -p lattice-runtime`, `cargo test -p lattice-agent`, `cargo test -p lattice-prompt`, `cargo test -p lattice-tools`, and `cargo test -p lattice-cli --test code_agent_eval` when the eval file exists. [RECOMMENDED FROM: touched crates verified by code refs]
- **Phase gate:** run `cargo fmt --all --check`, `cargo clippy --workspace -- -D warnings`, and `cargo test --workspace`. [VERIFIED: AGENTS.md]

### Wave 0 Gaps

- [ ] `crates/lattice-runtime/src/code_agent.rs` or equivalent shared factory module. [RECOMMENDED FROM: 14-CONTEXT.md]
- [ ] `crates/lattice-prompt/src/workspace_provider.rs` or equivalent shared repo context provider. [RECOMMENDED FROM: 14-CONTEXT.md]
- [ ] `crates/lattice-agent/src/final_report.rs` or equivalent state-driven renderer/validator. [RECOMMENDED FROM: 14-CONTEXT.md]
- [ ] `crates/lattice-cli/tests/code_agent_eval.rs` deterministic Phase 14 fixtures. [RECOMMENDED FROM: 14-CONTEXT.md]
- [ ] Provider precedence tests for nested directories and same-directory `LATTICE.md > AGENTS.md > CLAUDE.md`. [VERIFIED MISSING: crates/lattice-prompt/src/project_instructions_provider.rs only has load-all test]
- [ ] `/approve` parser and proposal-state tests for CLI/TUI. [VERIFIED MISSING: crates/lattice-cli/src/frontend/tui/app.rs has no `/approve` branch]

**Targeted tests run during research:**  
- `cargo test -p lattice-runtime code_agent_preset_exposes_workspace_editing_shell_planning_and_web` passed. [VERIFIED: command output 2026-05-12]  
- `cargo test -p lattice-cli slash_permissions_switches_runtime_security` passed. [VERIFIED: command output 2026-05-12]  
- `cargo test -p lattice-prompt loads_lattice_agents_and_claude_files_from_root_to_cwd` passed. [VERIFIED: command output 2026-05-12]  
- `cargo test -p lattice-tools code_agent_preset_exposes_patch_without_multi_patch` passed. [VERIFIED: command output 2026-05-12]  
- `cargo test -p lattice-agent classify_approval_proposal_records_blocker` passed. [VERIFIED: command output 2026-05-12]

## Security Domain

Security enforcement is enabled because `.planning/config.json` does not set `security_enforcement` to false. [VERIFIED: .planning/config.json]

### Applicable ASVS Categories

| ASVS Category | Applies | Standard Control |
|---------------|---------|------------------|
| V2 Authentication | no for Phase 14 agent loop | Do not modify provider credential auth; credentials remain env/config-driven. [VERIFIED: .env.example; crates/lattice-cli/src/credentials.rs] |
| V3 Session Management | yes | Session-scoped approvals and manifest metadata must resume deterministically. [VERIFIED: 14-CONTEXT.md; crates/lattice-session/src/lib.rs] |
| V4 Access Control | yes | Capability resolver, executor authorization, sandbox allowlists, and `RuntimeSecurity.approve_rule`. [VERIFIED: crates/lattice-tools/src/capabilities.rs; crates/lattice-tools/src/executor.rs; crates/lattice-runtime/src/lib.rs] |
| V5 Input Validation | yes | Parse `/approve` into typed `ApprovedRule`; validate command/path/tool rules through existing policy validators. [VERIFIED: crates/lattice-policy/src/approval.rs; crates/lattice-policy/src/validation.rs] |
| V6 Cryptography | limited | Do not add custom cryptography; existing hashes are for manifests/content, not security proofs. [VERIFIED: crates/lattice-agent/src/manifest.rs; crates/lattice-tools/src/edit/types.rs] |

### Known Threat Patterns for Phase 14

| Pattern | STRIDE | Standard Mitigation |
|---------|--------|---------------------|
| Unauthorized tool call constructed manually | Elevation of privilege | Executor-side authorization with `with_authorized_tools`; do not rely on schemas only. [VERIFIED: crates/lattice-tools/src/executor.rs] |
| Broad approval rule | Elevation of privilege | Existing approval validation rejects broad/dangerous command/path rules. [VERIFIED: crates/lattice-policy/src/validation.rs; crates/lattice-runtime/src/lib.rs tests] |
| Approval replay without user intent | Tampering | `/approve` must update state only; user explicitly continues/retries. [VERIFIED: 14-CONTEXT.md] |
| Prompt injection through project instruction files | Spoofing/Tampering | Load scoped files with explicit precedence and no `@file` include expansion in Phase 14. [VERIFIED: 14-CONTEXT.md] |
| False verification claim | Repudiation | Final report validator maps state to `passed`/`failed`/`not_run` and rejects unsupported `passed`. [VERIFIED: 14-CONTEXT.md; crates/lattice-agent/src/tool_result.rs] |
| Unrelated dirty worktree overwritten | Tampering | Use git sanity checks plus structured edit manifests and dirty-worktree evals. [VERIFIED: 14-CONTEXT.md; CITED: https://git-scm.com/docs/git-status] |

## Sources

### Primary (HIGH confidence)
- `.planning/phases/14-main-code-agent-eval-harness/14-CONTEXT.md` - locked decisions, scope, canonical refs, and deferred items.  
- `.planning/REQUIREMENTS.md` - CODE-01..05 and EVAL-01..04 requirement text.  
- `.planning/STATE.md` and `.planning/ROADMAP.md` - Phase 14 milestone position and dependencies.  
- `.planning/v1.2-MILESTONE-AUDIT.md` - audit blockers for `/approve`, runtime-built code-agent wiring, nested instructions, final reporting, and evals.  
- `.planning/research/CC_CODEX_GAP_ANALYSIS.md` - canonical EVAL-04 input and behavior gap categories.  
- `AGENTS.md` - build/test commands and crate responsibility constraints.  
- `Cargo.toml`, `Cargo.lock`, `cargo tree` - workspace stack and resolved dependency versions.  
- `crates/lattice-cli/src/coding_agent.rs` - current CLI-owned prompt, repo context, task seed, and code-agent preset use.  
- `crates/lattice-runtime/src/lib.rs` - `AgentSpec`, `Runtime::build_agent`, `RuntimeSecurity.approve_rule`, and current provider/executor wiring.  
- `crates/lattice-prompt/src/project_instructions_provider.rs` - instruction file traversal and current load test.  
- `crates/lattice-tools/src/capabilities.rs`, `crates/lattice-tools/src/executor.rs`, `crates/lattice-tools/tests/exposure.rs` - capability resolver, executor authorization, and profile isolation tests.  
- `crates/lattice-agent/src/state.rs`, `crates/lattice-agent/src/tool_result.rs`, `crates/lattice-agent/src/turn_engine.rs`, `crates/lattice-agent/src/manifest.rs`, `crates/lattice-agent/src/config.rs` - task/recovery state, denial enrichment, manifest metadata, and provider hooks.  
- `crates/lattice-session/src/lib.rs`, `crates/lattice-cli/src/session.rs` - session and manifest metadata persistence.  
- `crates/lattice-agent/tests/agent_mock.rs`, `crates/lattice-agent/tests/context_e2e.rs` - existing deterministic mock/SSE test patterns.

### Secondary (MEDIUM confidence)
- Git official docs, `git status --porcelain` format - stable machine-readable git status guidance. [CITED: https://git-scm.com/docs/git-status]

### Tertiary (LOW confidence)
- ASVS category applicability mapping is heuristic planning guidance, not a formal compliance audit. [ASSUMED]

## Metadata

**Confidence breakdown:**
- Standard stack: HIGH - verified from `Cargo.toml`, `Cargo.lock`, `cargo tree`, and installed tool versions. [VERIFIED: cargo tree; cargo/rustc/git version commands]
- Architecture: HIGH - verified from Phase 14 context and current crate code. [VERIFIED: 14-CONTEXT.md; code refs listed in Sources]
- Pitfalls: HIGH for local gaps; MEDIUM for eval-design risk because exact harness shape is still discretionary. [VERIFIED: code refs; 14-CONTEXT.md]

**Research date:** 2026-05-12 [VERIFIED: system date]  
**Valid until:** 2026-06-11 for local architecture; re-check dependency versions and code state before planning if the branch changes. [ASSUMED]

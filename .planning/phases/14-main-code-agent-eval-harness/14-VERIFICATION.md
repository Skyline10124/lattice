---
phase: 14-main-code-agent-eval-harness
verified: 2026-05-12T00:00:00Z
status: passed
score: 14/14 truths verified
overrides_applied: 0
reverification: yes
source:
  - 14-01-SUMMARY.md
  - 14-02-SUMMARY.md
  - 14-03-SUMMARY.md
  - 14-04-SUMMARY.md
  - 14-05-SUMMARY.md
  - 14-06-SUMMARY.md
  - 14-07-SUMMARY.md
  - 14-UAT.md
  - 14-REVIEW.md
---

# Phase 14: Main Code Agent & Eval Harness -- Verification Report

**Phase Goal:** `lattice code` becomes a strong main agent built from reusable capabilities, with behavior validated against Codex/Claude Code-inspired scenarios.
**Verified:** 2026-05-12
**Status:** passed
**Re-verification:** Yes -- after UAT gap closures 14-06 and 14-07 plus post-review hardening.

## Goal Achievement

### Roadmap Success Criteria

| # | Truth | Status | Evidence |
|---|-------|--------|----------|
| SC-1 | `lattice code` is assembled from an explicit code-agent profile/capability preset | VERIFIED | `Runtime::build_code_agent(CodeAgentSpec)` is implemented in `crates/lattice-runtime/src/code_agent.rs`; `crates/lattice-cli/src/coding_agent.rs` delegates to `runtime.build_code_agent`; tests cover factory construction and authorized code-agent tools. |
| SC-2 | Hierarchical `LATTICE.md`, `AGENTS.md`, and `CLAUDE.md` instructions are loaded through provider/context mechanisms | VERIFIED | `ProjectInstructionsProvider::new(project_root, cwd)` is wired by the runtime factory; provider tests cover literal `@file` handling, same-directory order, nested scope, full root/child file matrix, and symlink skipping. |
| SC-3 | Code agent uses workspace context and targeted file reads/searches for substantial work | VERIFIED | `WorkspaceContextProvider` is attached in `Runtime::build_code_agent`; `code_agent_eval.rs` verifies the product path starts with workspace context and uses targeted `grep`/`read_file` behavior. |
| SC-4 | Final answers report changed files, verification run, and residual risk | VERIFIED | `FinalReport::from_task_state` renders `Changed Files`, `Verification`, and `Residual Risk`; CLI text, JSON, and saved assistant content use the same report renderer; evals cover verification honesty and dirty worktree risk. |
| SC-5 | Eval fixtures cover worktree safety, instruction obedience, targeted file reading, patch recovery, verification honesty, and profile isolation | VERIFIED | `cargo test -p lattice-cli --test code_agent_eval` passes 8 deterministic offline evals covering the required behavior families plus factory/profile isolation and gap-analysis linkage. |

### UAT Gap Closure

| Gap | Status | Evidence |
|-----|--------|----------|
| Project instruction precedence composed incorrectly across root/child `CLAUDE.md`, `AGENTS.md`, and `LATTICE.md` | RESOLVED | 14-06 added `renders_mixed_root_child_files_by_file_tier_then_directory_depth` and changed provider ordering to file-tier-major with parent-before-child ordering inside each tier. |
| Approval proposal cleanup left stale blockers and duplicate equivalent approvals | RESOLVED | 14-07 links approval blocker failures to proposal IDs, resolves linked blockers, coalesces equivalent pending proposals, suppresses resolved approval blockers in prompt/report renderers, and deduplicates identical policy grants. |

### Code Review Gate

| Item | Status | Evidence |
|------|--------|----------|
| Phase 14 post-gap code review | CLEAN | `14-REVIEW.md` status is `clean`; real findings CR-02, CR-03, CR-04, CR-05, and WR-01 were resolved in `b34ca77`; CR-01 was accepted as a false positive because 14-06 intentionally locks file-tier-major instruction ordering. |
| Symlinked instruction files | VERIFIED | `ProjectInstructionsProvider` skips symlinked instruction files and checks canonical paths remain under the project root. |
| Approval proposal trust boundary | VERIFIED | Approval proposals are derived from typed `ToolOutcome::Denied` values; arbitrary tool text can no longer forge approval state. |
| Shell-control allowlist hardening | VERIFIED | `PolicyAuthority::command_matches_allowlist` rejects shell-control composition before token-prefix matching. |
| UTF-8 verification trimming | VERIFIED | Tool output trimming backs up to a valid UTF-8 character boundary before slicing. |
| Empty `todo_write` cleanup | VERIFIED | Successful empty `todo_write` calls clear stale todo state; malformed non-empty payloads preserve the previous state atomically. |

## Requirements Coverage

| Requirement | Status | Evidence |
|-------------|--------|----------|
| CODE-01 | SATISFIED | Runtime-owned code-agent factory plus CLI delegation prove `lattice code` is no longer a one-off CLI prompt path. |
| CODE-02 | SATISFIED | `ProjectInstructionsProvider` loads `CLAUDE.md`, `AGENTS.md`, and `LATTICE.md` through the prompt provider stack with deterministic precedence and symlink hardening. |
| CODE-03 | SATISFIED | `WorkspaceContextProvider` supplies bounded workspace context; evals assert targeted reads/searches rather than broad dumps. |
| CODE-04 | SATISFIED | Final report renderer and evals prove dirty worktree safety, verification honesty, changed files, and residual risk reporting. |
| CODE-05 | SATISFIED | Code-agent factory attaches workspace/project providers, authorized executor, task-state provider, editing/shell/planning/web capabilities, approval flow, and final report integration. |
| EVAL-01 | SATISFIED | Offline eval fixtures cover dirty worktree safety, instruction obedience, targeted reading, patch recovery, verification honesty, approval cleanup, and instruction gap closures. |
| EVAL-02 | SATISFIED | `crates/lattice-tools/tests/exposure.rs` and `code_agent_eval.rs` prove non-coding profiles do not expose code-agent editing/shell/planning tools. |
| EVAL-03 | SATISFIED | Plan summaries and this report document the touched-crate commands; workspace build/test/clippy/fmt gates pass. |
| EVAL-04 | SATISFIED | `code_agent_eval.rs` asserts Phase 14 planning docs reference `.planning/research/CC_CODEX_GAP_ANALYSIS.md`. |

All 9 Phase 14 CODE/EVAL requirements are satisfied.

## Required Artifacts

| Artifact | Expected | Status |
|----------|----------|--------|
| `crates/lattice-runtime/src/code_agent.rs` | Shared runtime code-agent factory | VERIFIED |
| `crates/lattice-prompt/src/workspace_provider.rs` | Bounded workspace startup context provider | VERIFIED |
| `crates/lattice-prompt/src/project_instructions_provider.rs` | Deterministic instruction loading with full root/child matrix coverage and symlink hardening | VERIFIED |
| `crates/lattice-cli/src/coding_agent.rs` | Thin CLI construction path plus canonical final report output | VERIFIED |
| `crates/lattice-agent/src/state.rs` | Approval proposal records, linked blocker resolution, equivalent proposal coalescing | VERIFIED |
| `crates/lattice-agent/src/tool_result.rs` | Typed denial-derived approval proposal capture, verification status capture, UTF-8-safe output trimming, todo cleanup | VERIFIED |
| `crates/lattice-agent/src/final_report.rs` | State-derived final report renderer | VERIFIED |
| `crates/lattice-agent/src/prompt/task_state_provider.rs` | Recovery context suppresses only resolved approval-linked blockers | VERIFIED |
| `crates/lattice-cli/src/security.rs` | `/approve last` and `/approve <id>` dispatch through shared runtime security | VERIFIED |
| `crates/lattice-policy/src/authority.rs` | Idempotent approval grants and shell-control rejection in command prefix matching | VERIFIED |
| `crates/lattice-tools/tests/exposure.rs` | Profile isolation regression coverage | VERIFIED |
| `crates/lattice-cli/tests/code_agent_eval.rs` | Deterministic product-loop eval harness | VERIFIED |

## Behavioral Verification

| Command | Result | Status |
|---------|--------|--------|
| `cargo build` | Finished successfully | PASS |
| `cargo test` | Workspace tests passed | PASS |
| `cargo test -p lattice-cli --test code_agent_eval` | 8 passed, 0 failed | PASS |
| `cargo clippy -- -D warnings` | Finished successfully with no warnings | PASS |
| `cargo fmt --all --check` | No diffs | PASS |

## Human Verification Required

None. The Phase 14 success criteria are covered by deterministic Rust tests, provider/factory structural checks, offline product-loop evals, code review triage, and workspace Cargo gates.

## Residual Risk

None identified for Phase 14 scope. The review finding about instruction ordering was intentionally rejected as a false positive because the accepted UAT contract is global file-tier order (`CLAUDE.md`, then `AGENTS.md`, then `LATTICE.md`) with parent-before-child ordering inside each tier.

---

_Verified: 2026-05-12T00:00:00Z_
_Verifier: Codex (gsd-execute-phase local verification)_

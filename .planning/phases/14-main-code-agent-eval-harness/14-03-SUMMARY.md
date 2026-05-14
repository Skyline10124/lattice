---
phase: 14-main-code-agent-eval-harness
plan: 03
subsystem: permission
tags: [code-agent, approval, task-state, tui, runtime-security]

requires:
  - phase: 12-permission-approval-workflow
    provides: structured denials and RuntimeSecurity approval rules
  - phase: 13-agent-task-recovery-state
    provides: durable task/recovery state carried across turns
  - phase: 14-main-code-agent-eval-harness
    plan: 01
    provides: runtime-built code-agent loop with task state provider
provides:
  - Durable approval proposal records with stable ap-N identifiers
  - Shared approve_proposal helper backed by RuntimeSecurity.approve_rule
  - TUI /approve last and /approve <id> slash command support without auto-replay
affects: [phase-14, lattice-agent, lattice-cli, lattice-policy, lattice-sandbox]

tech-stack:
  added: []
  patterns:
    - Proposal IDs resolve only pending approval records
    - CLI and TUI approval entrypoints share one security dispatcher
    - Approval changes session security state but never replays denied work

key-files:
  modified:
    - crates/lattice-agent/src/lib.rs
    - crates/lattice-agent/src/state.rs
    - crates/lattice-agent/src/tool_result.rs
    - crates/lattice-cli/src/security.rs
    - crates/lattice-cli/src/frontend/tui/app.rs
    - crates/lattice-cli/src/frontend/tui/commands.rs
    - crates/lattice-cli/src/frontend/tui/state.rs
    - crates/lattice-policy/src/lib.rs
    - crates/lattice-policy/src/proposal.rs
    - crates/lattice-sandbox/src/lib.rs

key-decisions:
  - "Approval proposal records live in TaskState and use stable short IDs such as ap-1."
  - "Stored approval commands are parsed through shared policy helpers and then applied through RuntimeSecurity.approve_rule."
  - "TUI /approve only approves the stored proposal; the user or model must explicitly retry the blocked request."

patterns-established:
  - "TaskState records approval_proposals with resolved flags and latest/id lookup helpers."
  - "approve_proposal marks records resolved only after runtime approval succeeds."
  - "TUI slash command handling lives in CommandRegistry and calls App helpers for stateful effects."

requirements-completed: [CODE-05, EVAL-01, EVAL-03]

duration: 3h 14m wall-clock with handoff
completed: 2026-05-12
---

# Phase 14 Plan 03: Approval Retry Loop Summary

**Session-scoped approval proposals can be approved from CLI or TUI through shared runtime security without replaying blocked work**

## Performance

- **Duration:** 3h 14m wall-clock with handoff
- **Started:** 2026-05-12T08:40:45Z
- **Completed:** 2026-05-12T11:54:37Z
- **Tasks:** 3
- **Files modified:** 10 code files

## Accomplishments

- Added `ApprovalProposalRecord` storage to `TaskState`, including stable `ap-N` IDs, pending lookup, resolution, and old-session serde compatibility.
- Recorded enriched approval blockers from tool results so denied operations produce actionable pending proposals.
- Added shared CLI approval dispatch that parses stored `/approve command|tool|path ...` text and calls `RuntimeSecurity.approve_rule`.
- Added TUI `/approve <id|last>` suggestions and slash command handling with success/failure system messages.
- Verified approvals mutate session security state only; they do not enqueue, submit, replay, or retry the blocked operation.

## Task Commits

Each task was committed atomically:

1. **Task 1: Store approval proposal records in task state** - `2d31318` (feat)
2. **Task 2: Convert stored approval proposals into RuntimeSecurity approvals** - `c00f7b4` (feat)
3. **Task 3: Add TUI /approve command with same semantics** - `37b3ef6` (feat)

**Plan hygiene:** `6eee37a` (style: rustfmt approval parser)

## Files Created/Modified

- `crates/lattice-agent/src/state.rs` - Adds approval proposal records, stable IDs, lookup, resolution, and serde compatibility tests.
- `crates/lattice-agent/src/tool_result.rs` - Records approval proposals from enriched denial tool results.
- `crates/lattice-agent/src/lib.rs` - Exports approval proposal state types.
- `crates/lattice-cli/src/security.rs` - Adds shared `approve_proposal` dispatcher and tests for last/id/error paths.
- `crates/lattice-cli/src/frontend/tui/app.rs` - Adds stateful TUI approval helper and slash approval tests.
- `crates/lattice-cli/src/frontend/tui/commands.rs` - Routes `/approve` through the TUI command registry.
- `crates/lattice-cli/src/frontend/tui/state.rs` - Adds `/approve <id|last>` slash suggestion.
- `crates/lattice-policy/src/proposal.rs` - Parses stored approval command shapes.
- `crates/lattice-policy/src/lib.rs` - Exports approval parser helpers.
- `crates/lattice-sandbox/src/lib.rs` - Re-exports approval-kind parsing for CLI policy dispatch.

## Verification

- `cargo test -p lattice-agent approval_proposal` - PASS
- `cargo test -p lattice-cli approve_proposal` - PASS
- `cargo test -p lattice-cli slash_approve` - PASS
- `cargo fmt --all --check` - PASS

The plan-listed combined command `cargo test -p lattice-cli approve_proposal slash_approve` was represented as two Cargo-compatible filtered test commands because Cargo accepts one test filter before test binary args.

## Decisions Made

- Proposal selection accepts only `last` or stored pending IDs such as `ap-1`.
- Approvals are rejected for unknown selectors, resolved proposals, malformed command text, and unsupported approval kinds.
- TUI approval reports "Continue or retry the blocked request when ready" to keep replay an explicit follow-up action.

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Rustfmt drift in approval parser**
- **Found during:** Plan-level verification
- **Issue:** `cargo fmt --all --check` failed on `crates/lattice-policy/src/proposal.rs`.
- **Fix:** Applied rustfmt-compatible formatting to the parser error construction.
- **Files modified:** `crates/lattice-policy/src/proposal.rs`
- **Verification:** `cargo fmt --all --check` and `cargo test -p lattice-cli approve_proposal`
- **Committed in:** `6eee37a`

---

**Total deviations:** 1 auto-fixed formatting issue.
**Impact on plan:** No behavior change; it clears the project formatting gate.

## Issues Encountered

- The TUI slash command path now lives in `crates/lattice-cli/src/frontend/tui/commands.rs`, so Task 3 touched that file in addition to the plan-listed `app.rs` and `state.rs`.
- Cargo's filtered test syntax required running `approve_proposal` and `slash_approve` as separate commands.

## Known Stubs

None.

## Threat Flags

None. Stable proposal IDs, shared runtime approval dispatch, and no auto-replay cover T-14-04, T-14-05, and T-14-06.

## User Setup Required

None.

## Next Phase Readiness

Ready for 14-04. The approval loop now leaves pending approval state visible for final reporting, including unresolved proposal risk.

## Self-Check: PASSED

- Summary file exists and records all task commits.
- Targeted agent and CLI approval tests pass.
- Formatting gate passes.
- ROADMAP/STATE reflect Phase 14 advancement to 3/5 plans.

---
*Phase: 14-main-code-agent-eval-harness*
*Completed: 2026-05-12*

---
phase: 14-main-code-agent-eval-harness
plan: 07
subsystem: agent
tags: [code-agent, approval, task-state, final-report, policy]

requires:
  - phase: 14-main-code-agent-eval-harness
    plan: 03
    provides: session approval proposal records and approve_proposal dispatch
  - phase: 14-main-code-agent-eval-harness
    plan: 04
    provides: task-state recovery prompt and final report renderers
provides:
  - Approval blocker tool failures linked to ap-N proposal IDs
  - Equivalent pending approval proposal coalescing by approve_command and risk_class
  - Resolved approval blocker suppression in recovery prompts and final reports
  - Idempotent storage for identical approval grants
  - Code-review hardening for typed denial trust boundaries, UTF-8-safe verification trimming, and shell-control rejection
affects: [phase-14, lattice-agent, lattice-cli, lattice-policy, lattice-prompt, lattice-tools]

tech-stack:
  added: []
  patterns:
    - Approval blocker cleanup is state-linked through ToolFailureRecord metadata
    - Pending approval proposal equivalence is approve_command plus risk_class
    - PolicyAuthority validates every approval but stores identical grants idempotently
    - Approval proposal state is derived only from typed ToolOutcome::Denied values, not arbitrary tool text

key-files:
  created: []
  modified:
    - crates/lattice-agent/src/state.rs
    - crates/lattice-agent/src/tool_result.rs
    - crates/lattice-agent/src/prompt/task_state_provider.rs
    - crates/lattice-agent/src/final_report.rs
    - crates/lattice-agent/src/turn_engine.rs
    - crates/lattice-cli/src/security.rs
    - crates/lattice-policy/src/authority.rs
    - crates/lattice-prompt/src/project_instructions_provider.rs
    - crates/lattice-tools/src/lib.rs

key-decisions:
  - "Resolved approval blockers are retained for history but marked inactive through ToolFailureRecord.resolved."
  - "Equivalent pending proposals reuse the existing ap-N when approve_command and risk_class match."
  - "PolicyAuthority remains the shared approval storage owner and deduplicates identical validated grants."
  - "Approval proposal recording trusts typed denial outcomes only; model-visible tool text cannot forge approval state."

patterns-established:
  - "Renderers suppress only resolved approval-linked tool failures; unresolved blockers and non-approval failures remain visible."
  - "TaskState record_approval_proposal is idempotent for unresolved equivalent proposals."
  - "Approval grant dedupe occurs after validation so malformed or broad rules still fail normally."

requirements-completed: [CODE-05, EVAL-01, EVAL-03]

duration: 24 min
completed: 2026-05-12
---

# Phase 14 Plan 07: Approval State Cleanup Summary

**Approval proposals now resolve cleanly, reuse equivalent pending IDs, and avoid duplicate approval grants**

## Performance

- **Duration:** 24 min
- **Started:** 2026-05-12T15:26:05Z
- **Completed:** 2026-05-12T15:49:55Z
- **Tasks:** 5
- **Files modified:** 6 code files

## Accomplishments

- Linked approval blocker tool failures to their `ap-N` proposal IDs with backward-compatible serde defaults.
- Marked linked approval blocker failures resolved when the proposal is approved.
- Coalesced equivalent unresolved approval proposals by `approve_command` plus `risk_class`.
- Suppressed resolved approval blockers from task recovery prompts and final report residual risk.
- Made identical approval grants idempotent in shared policy storage while preserving validation failures.
- Added regression coverage for stale blockers, duplicate path proposals, duplicate grants, and non-equivalent approvals.
- Closed post-review blockers by skipping symlinked project instruction files, hardening approval proposal trust boundaries, rejecting shell-control suffixes in shared command matching, making verification-output trimming UTF-8 safe, and allowing successful empty `todo_write` calls to clear stale state.

## Task Commits

Each task was committed atomically:

1. **Task 1: Link approval blocker failures to proposal IDs** - `e0a3ae9` (feat)
2. **Task 2: Coalesce equivalent pending approval proposals** - `22ef16d` (feat)
3. **Task 3: Suppress resolved approval blockers in recovery prompt and final report** - `65d27bf` (feat)
4. **Task 4: Make approval grant insertion idempotent** - `76a1f30` (feat)
5. **Task 5: Cover the user-reported approval cleanup cases** - `efe1834` (test)

**Plan hygiene:** `ad3b3a2` (style: cargo fmt approval cleanup files)

**Post-review hardening:** `b34ca77` (fix: resolve approval cleanup review blockers)

## Files Created/Modified

- `crates/lattice-agent/src/state.rs` - Adds approval metadata to tool failures, coalesces equivalent pending proposals, resolves linked blockers, and covers state behavior.
- `crates/lattice-agent/src/tool_result.rs` - Records approval blocker failures with proposal IDs and avoids duplicate active blocker rows.
- `crates/lattice-agent/src/turn_engine.rs` - Classifies typed tool outcomes before trusting approval denial metadata.
- `crates/lattice-agent/src/prompt/task_state_provider.rs` - Filters resolved approval-linked failures out of recovery prompt context.
- `crates/lattice-agent/src/final_report.rs` - Excludes resolved approval blockers from residual risk while keeping unresolved blockers visible.
- `crates/lattice-cli/src/security.rs` - Covers repeated equivalent proposal approval and non-equivalent proposal approval behavior.
- `crates/lattice-policy/src/authority.rs` - Deduplicates identical approval grants after validation and covers idempotent storage.
- `crates/lattice-prompt/src/project_instructions_provider.rs` - Skips symlinked instruction files and covers the leak case.
- `crates/lattice-tools/src/lib.rs` - Keeps plain tool text plain so arbitrary output cannot become a typed denial.

## Verification

- `cargo test -p lattice-agent approval_proposal` - PASS
- `cargo test -p lattice-agent final_report` - PASS
- `cargo test -p lattice-cli approve_proposal` - PASS
- `cargo test -p lattice-cli slash_approve` - PASS
- `cargo test -p lattice-policy approve` - PASS
- `cargo test -p lattice-agent tool_result` - PASS after post-review hardening.
- `cargo test -p lattice-policy command_matches_allowlist` - PASS after post-review hardening.
- `cargo test -p lattice-prompt project_instruction_precedence` - PASS after post-review hardening.
- `cargo fmt --all --check` - PASS

## Decisions Made

- Resolved approval blockers are not removed from state; they are marked resolved so old session data remains inspectable.
- Pending approval proposal equivalence is intentionally narrow: `approve_command` plus `risk_class`.
- Grant idempotency lives in `PolicyAuthority::approve`, after normal validation, so unsafe or malformed rules still return the same errors.
- Render suppression checks both `resolved` and `approval_proposal_id`, preventing unrelated tool failures from being hidden.
- Code review item CR-01 was triaged as a false positive against the locked 14-06 UAT contract: Phase 14 intentionally uses file-tier-major instruction ordering with parent-before-child ordering inside each tier.

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Aligned targeted formatting with cargo fmt**
- **Found during:** Plan-level verification
- **Issue:** `cargo fmt --all --check` failed because earlier direct `rustfmt` calls used the wrong edition for targeted files.
- **Fix:** Re-ran targeted `rustfmt --edition 2021` on the 14-07 files and re-ran every plan-level verification command.
- **Files modified:** `crates/lattice-agent/src/final_report.rs`, `crates/lattice-agent/src/state.rs`, `crates/lattice-cli/src/security.rs`, `crates/lattice-policy/src/authority.rs`
- **Verification:** All plan-level commands passed after formatting.
- **Committed in:** `ad3b3a2`

---

**Total deviations:** 1 auto-fixed blocking formatting issue.
**Impact on plan:** No behavior change; it cleared the required formatting gate.

### Post-Review Fixes

**2. [Rule 3 - Blocking] Resolved code-review trust-boundary and robustness findings**
- **Found during:** Required advisory code review gate
- **Issue:** Review found real blockers in symlinked instruction loading, approval-proposal trust boundaries, shell-control command matching, UTF-8 output trimming, and stale todo clearing. One additional finding about instruction render order conflicted with the locked 14-06 UAT contract and was accepted as a false positive.
- **Fix:** Applied narrow hardening in the affected Phase 14 files and added regression tests.
- **Files modified:** `crates/lattice-prompt/src/project_instructions_provider.rs`, `crates/lattice-agent/src/tool_result.rs`, `crates/lattice-agent/src/turn_engine.rs`, `crates/lattice-policy/src/authority.rs`, `crates/lattice-tools/src/lib.rs`
- **Verification:** Targeted tests passed before final phase-level verification.
- **Committed in:** `b34ca77`

## Issues Encountered

- Direct `rustfmt` must use the workspace edition (`2021`) when formatting individual files outside `cargo fmt`.

## Known Stubs

None.

## Threat Flags

None. The plan did not introduce new network endpoints, auth paths, file access patterns, or schema trust boundaries.

## User Setup Required

None.

## Next Phase Readiness

Phase 14 gap closure is complete. Approval recovery state is clean, idempotent, and covered by targeted regression tests.

## Self-Check: PASSED

- Summary file exists at `.planning/phases/14-main-code-agent-eval-harness/14-07-SUMMARY.md`.
- Task, hygiene, and post-review commits exist in git history: `e0a3ae9`, `22ef16d`, `65d27bf`, `76a1f30`, `efe1834`, `ad3b3a2`, `b34ca77`.
- Plan-level verification commands passed after the final formatting commit.

---
*Phase: 14-main-code-agent-eval-harness*
*Completed: 2026-05-12*

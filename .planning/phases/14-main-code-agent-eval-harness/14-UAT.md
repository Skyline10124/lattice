---
status: resolved
phase: 14-main-code-agent-eval-harness
source:
  - 14-01-SUMMARY.md
  - 14-02-SUMMARY.md
  - 14-03-SUMMARY.md
  - 14-04-SUMMARY.md
  - 14-05-SUMMARY.md
  - 14-06-SUMMARY.md
  - 14-07-SUMMARY.md
started: "2026-05-12T21:59:29+08:00"
updated: "2026-05-13T00:52:00+08:00"
---

## Current Test

[testing complete; gaps resolved by 14-06 and 14-07]

## Tests

### 1. Runtime Code-Agent Factory Is the Shared Entry Point
expected: When building or running the code agent through the CLI or runtime, the user sees the same runtime-owned code-agent assembly: workspace context is provided by `WorkspaceContextProvider`, project instructions are attached by provider, authorized tools come from the code-agent preset, and the CLI no longer owns a separate prompt-stuffing path.
result: pass

### 2. Project Instructions Follow Deterministic Precedence
expected: In nested workdirs, parent instruction files render before child instruction files; same-directory instruction files render in the documented `CLAUDE.md`, then `AGENTS.md`, then `LATTICE.md` order; include-looking `@file` text remains literal.
result: pass
reported: "根目录和子目录的CLAUDE.md、AGENTS.md和LATTICE.md的优先级是反着来的，根目录是顺着的，子目录是逆着的"
severity: major
resolved_by: "14-06-SUMMARY.md"
resolution: "ProjectInstructionsProvider now renders by global file tier (`CLAUDE.md`, `AGENTS.md`, `LATTICE.md`) with parent-before-child ordering inside each tier, and the full root/child file matrix is covered by regression tests."

### 3. Approval Retry Loop Stores and Resolves Proposals
expected: A denied command/tool/path operation records a pending approval proposal with a stable `ap-N` ID. `/approve last` or `/approve <id>` applies the stored approval through shared runtime security, marks the proposal resolved, and does not automatically replay the blocked operation.
result: pass
reported: |
  问题 1 — 陈旧 Tool Failures 未清除 审批提案的 resolved: true 已标记，但 tool_failures 列表仍保持原始拒绝信息。如果 agent 按 "blocked pending approval ap-X" 去重新 /approve，会被拒绝（因为已解决，find_pending_approval 返回 None），浪费一次交互。

  问题 2 — ap-2 和 ap-3 路径冗余 两者都是 /home/astrin/Dev/lattice/crates read。ap-3 已批准后，ap-2 再批准不会带来额外效果，只是把同一个 ApprovedRule 插入了两次。好在 RuntimeSecurity 的规则去重（如果实现了）或幂等性不会造成实际危害，但 state 里多了个冗余记录。
severity: minor
resolved_by: "14-07-SUMMARY.md"
resolution: "Approval blocker failures are linked to proposal IDs and marked resolved, equivalent pending proposals are coalesced, renderers suppress resolved approval blockers, and identical approval grants are idempotent."

### 4. Final Report Is Generated from Task State
expected: Code-agent text output, JSON output, and saved assistant content include the same canonical final report sections: `Changed Files`, `Verification`, and `Residual Risk`. Verification rows distinguish passed, failed, and not_run states, and dirty git paths missing from task state appear only as residual risk.
result: pass

### 5. Deterministic Code-Agent Eval Harness Runs Offline
expected: Running `cargo test -p lattice-cli --test code_agent_eval` exercises the runtime-built code-agent product path through local scripted OpenAI-compatible SSE responses and covers dirty worktree safety, project instructions, targeted reading, patch recovery, verification honesty, and profile isolation without live provider credentials.
result: pass

### 6. Profile Isolation and Gap Analysis Stay Covered
expected: Non-coding profiles do not expose code-agent editing, shell, or planning tools, while Phase 14 context/research/plan docs remain tied to `.planning/research/CC_CODEX_GAP_ANALYSIS.md` through regression coverage.
result: pass

## Summary

total: 6
passed: 6
issues: 0
pending: 0
skipped: 0
blocked: 0

## Gaps

- truth: "In nested workdirs, parent instruction files render before child instruction files; same-directory instruction files render in the documented `CLAUDE.md`, then `AGENTS.md`, then `LATTICE.md` order; include-looking `@file` text remains literal."
  status: resolved
  reason: "User reported: 根目录和子目录的CLAUDE.md、AGENTS.md和LATTICE.md的优先级是反着来的，根目录是顺着的，子目录是逆着的"
  severity: major
  test: 2
  root_cause: "ProjectInstructionsProvider used directory-major render ordering and only tested same-file nested order plus same-directory file order. There was no matrix test for root+child CLAUDE.md/AGENTS.md/LATTICE.md, so cross-directory file-type precedence could be reversed from user expectation."
  resolved_by: "14-06-SUMMARY.md"
  resolution: "Added a mixed root/child instruction matrix test and changed source ordering to file-tier-major with parent-before-child ordering inside each tier."
  verification: "`cargo test -p lattice-prompt project_instruction_precedence` and `cargo test -p lattice-runtime code_agent_uses_root_and_cwd_project_instructions` pass; full workspace gates pass in 14-VERIFICATION.md."
  artifacts:
    - path: "crates/lattice-prompt/src/project_instructions_provider.rs"
      issue: "instruction_sources loops directories first and file names second, which leaves mixed root/child file-type precedence untested and potentially inverted."
    - path: ".planning/phases/14-main-code-agent-eval-harness/14-02-PLAN.md"
      issue: "acceptance criteria cover same-file nested order and same-directory file order, but not the full root+child file matrix."
  closed:
    - "Added a provider test where root and child both contain CLAUDE.md, AGENTS.md, and LATTICE.md, asserting canonical low-to-high precedence across the combined output."
    - "Adjusted ProjectInstructionsProvider ordering so deeper directory precedence and file-type precedence compose deterministically for mixed root/child instruction files."
  debug_session: ".planning/debug/resolved/phase-14-instruction-precedence.md"
- truth: "A denied command/tool/path operation records a pending approval proposal with a stable `ap-N` ID. `/approve last` or `/approve <id>` applies the stored approval through shared runtime security, marks the proposal resolved, and does not automatically replay the blocked operation."
  status: resolved
  reason: "User reported: 问题 1 — 陈旧 Tool Failures 未清除；审批提案 resolved: true 后 tool_failures 仍保持原始拒绝信息，agent 可能按 blocked pending approval ap-X 重新 /approve 并被拒绝。问题 2 — ap-2 和 ap-3 都是 /home/astrin/Dev/lattice/crates read，批准后产生冗余记录/重复规则插入。"
  severity: minor
  test: 3
  root_cause: "Approval proposal resolution was not connected to recovery-state cleanup or equivalent-rule deduplication. The state model had no proposal-id link on ToolFailureRecord and record_approval_blocker/PolicyAuthority appended duplicate equivalent approvals."
  resolved_by: "14-07-SUMMARY.md"
  resolution: "Linked blocker failures to proposal IDs, coalesced equivalent pending approval proposals, suppressed resolved approval blockers from recovery/final-report renderers, and deduplicated identical approved grants."
  verification: "`cargo test -p lattice-agent approval_proposal`, `cargo test -p lattice-cli approve_proposal`, `cargo test -p lattice-policy command_matches_allowlist`, and full workspace gates pass in 14-VERIFICATION.md."
  artifacts:
    - path: "crates/lattice-agent/src/state.rs"
      issue: "ToolFailureRecord has no proposal_id/resolved link, and mark_approval_resolved only flips the proposal flag."
    - path: "crates/lattice-agent/src/tool_result.rs"
      issue: "record_approval_blocker always appends a new approval proposal and a new tool failure for equivalent denial reports."
    - path: "crates/lattice-policy/src/authority.rs"
      issue: "PolicyAuthority::approve pushes duplicate equivalent ApprovalGrant values."
    - path: "crates/lattice-cli/src/security.rs"
      issue: "approve_proposal resolves one proposal but does not clear stale blocker failures or equivalent pending proposals."
  closed:
    - "Linked approval blocker tool failures to proposal IDs and hide/clear resolved blocker failures from recovery context."
    - "Coalesced equivalent pending approval proposals so repeated denials for the same command/tool/path reuse an existing pending ap-N."
    - "Made approval grant insertion idempotent for identical ApprovedRule values."
  debug_session: ".planning/debug/resolved/phase-14-approval-state-cleanup.md"

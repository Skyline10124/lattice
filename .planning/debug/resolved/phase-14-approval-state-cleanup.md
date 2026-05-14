---
status: resolved
trigger: "Phase 14 UAT gap: approval proposals resolve but stale failures and duplicate equivalent proposals remain"
created: "2026-05-12T22:51:00+08:00"
updated: "2026-05-13T00:52:00+08:00"
---

## Current Focus

hypothesis: "Approval state is append-only except for resolved=true; tool failures and equivalent pending proposals are not linked to approval resolution."
test: "Inspect TaskState approval and failure methods, approval dispatcher, and RuntimeSecurity approval store."
expecting: "If true, approve_proposal marks only one proposal resolved after approve_rule succeeds, leaving stale tool_failures text and duplicate proposal/rule records possible."
next_action: "Resolved by 14-07; no further action."
reasoning_checkpoint: null
tdd_checkpoint: null

## Symptoms

expected: "A denied operation records a stable approval proposal; approving it applies shared RuntimeSecurity approval, marks the proposal resolved, and leaves a clean recovery state for retry."
actual: "User reported resolved approval proposals leave original tool_failures containing blocked pending approval ap-X text, and equivalent ap-2/ap-3 path-read proposals can both be approved redundantly."
errors: "Re-approving a resolved ap-X returns no pending approval because find_pending_approval returns None."
reproduction: "Phase 14 UAT test 3."
started: "Discovered during UAT."

## Eliminated

- hypothesis: "The approval helper auto-replays blocked work."
  evidence: "approve_proposal only calls RuntimeSecurity.approve_rule and mark_approval_resolved; no replay path is present."
  timestamp: "2026-05-12T22:51:00+08:00"

## Evidence

- timestamp: "2026-05-12T22:51:00+08:00"
  checked: "crates/lattice-agent/src/state.rs"
  found: "TaskState has record_tool_failure, record_approval_proposal, find/latest pending lookup, and mark_approval_resolved, but no relation between proposal IDs and tool failure cleanup."
  implication: "Resolved proposals can remain visible through stale Tool Failures prompt context."
- timestamp: "2026-05-12T22:51:00+08:00"
  checked: "crates/lattice-agent/src/tool_result.rs"
  found: "record_approval_blocker always records a new proposal and then appends a tool_failure message containing `blocked pending approval {id}`."
  implication: "Equivalent denials are not coalesced and stale blocker text is expected."
- timestamp: "2026-05-12T22:51:00+08:00"
  checked: "crates/lattice-policy/src/authority.rs and crates/lattice-runtime/src/lib.rs"
  found: "PolicyAuthority::approve pushes every grant; RuntimeSecurity::approve_rule delegates to the shared store without deduping when the store is shared."
  implication: "Equivalent ApprovedRule values can be inserted repeatedly."

## Resolution

root_cause: "Approval proposal resolution was not connected to recovery-state cleanup or equivalent-rule deduplication. The state model had no proposal-id link on ToolFailureRecord and record_approval_blocker/PolicyAuthority appended duplicate equivalent approvals."
fix: "Plan 14-07 links approval blocker failures to proposal IDs, marks linked blockers resolved after successful approval, coalesces equivalent pending proposals, suppresses resolved approval blockers from recovery/final-report renderers, and deduplicates identical validated grants in PolicyAuthority."
verification: "`cargo test -p lattice-agent approval_proposal`, `cargo test -p lattice-cli approve_proposal`, `cargo test -p lattice-policy command_matches_allowlist`, and the Phase 14 full verification gates pass."
files_changed:
  - "crates/lattice-agent/src/state.rs"
  - "crates/lattice-agent/src/tool_result.rs"
  - "crates/lattice-agent/src/prompt/task_state_provider.rs"
  - "crates/lattice-agent/src/final_report.rs"
  - "crates/lattice-cli/src/security.rs"
  - "crates/lattice-policy/src/authority.rs"
  - ".planning/phases/14-main-code-agent-eval-harness/14-07-SUMMARY.md"

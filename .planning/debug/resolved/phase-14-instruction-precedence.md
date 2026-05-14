---
status: resolved
trigger: "Phase 14 UAT gap: project instruction precedence differs between root and child directories"
created: "2026-05-12T22:51:00+08:00"
updated: "2026-05-13T00:52:00+08:00"
---

## Current Focus

hypothesis: "InstructionSource ordering is directory-major, so depth ordering and file-type ordering interact incorrectly when multiple directories each contain CLAUDE.md/AGENTS.md/LATTICE.md."
test: "Compare ProjectInstructionsProvider::instruction_sources ordering against Phase 14 D-06/D-07 and UAT report."
expecting: "If true, root files are emitted CLAUDE->AGENTS->LATTICE before child files CLAUDE->AGENTS->LATTICE, letting child CLAUDE render after root LATTICE and override a higher-native root rule."
next_action: "Resolved by 14-06; no further action."
reasoning_checkpoint: null
tdd_checkpoint: null

## Symptoms

expected: "In nested workdirs, parent instruction files render before child instruction files; same-directory instruction files render in CLAUDE.md, AGENTS.md, LATTICE.md order so canonical priority is LATTICE.md > AGENTS.md > CLAUDE.md."
actual: "User reported root and child CLAUDE.md/AGENTS.md/LATTICE.md priority are reversed: root behaves in forward order while child behaves in reverse order."
errors: "None reported."
reproduction: "Phase 14 UAT test 2."
started: "Discovered during UAT."

## Eliminated

- hypothesis: "The provider lacks tests for same-directory precedence at all."
  evidence: "project_instruction_precedence tests cover root-only same-directory order and same-file root-vs-child order."
  timestamp: "2026-05-12T22:51:00+08:00"

## Evidence

- timestamp: "2026-05-12T22:51:00+08:00"
  checked: "crates/lattice-prompt/src/project_instructions_provider.rs"
  found: "instruction_sources reverses root-to-cwd dirs, then loops each dir and emits CLAUDE.md, AGENTS.md, LATTICE.md."
  implication: "The current order proves same-directory and same-file nested cases, but it does not prove mixed root/child file precedence."
- timestamp: "2026-05-12T22:51:00+08:00"
  checked: ".planning/phases/14-main-code-agent-eval-harness/14-02-PLAN.md"
  found: "Plan requires parent directories before child directories and same-directory CLAUDE->AGENTS->LATTICE, but acceptance criteria do not include a mixed matrix where both root and child contain all three files."
  implication: "The gap is a coverage and ordering edge case in the instruction provider plan."

## Resolution

root_cause: "ProjectInstructionsProvider used directory-major render ordering and only tested same-file nested order plus same-directory file order. There was no matrix test for root+child CLAUDE.md/AGENTS.md/LATTICE.md, so cross-directory file-type precedence could be reversed from user expectation."
fix: "Plan 14-06 added a mixed root/child instruction precedence regression test and changed ordering to global file-tier-major order (`CLAUDE.md`, `AGENTS.md`, `LATTICE.md`) with parent-before-child ordering inside each tier."
verification: "`cargo test -p lattice-prompt project_instruction_precedence`, `cargo test -p lattice-runtime code_agent_uses_root_and_cwd_project_instructions`, and the Phase 14 full verification gates pass."
files_changed:
  - "crates/lattice-prompt/src/project_instructions_provider.rs"
  - ".planning/phases/14-main-code-agent-eval-harness/14-06-SUMMARY.md"

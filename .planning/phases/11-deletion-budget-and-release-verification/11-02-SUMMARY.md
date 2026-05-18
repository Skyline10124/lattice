---
phase: 11-deletion-budget-and-release-verification
plan: 02
subsystem: cli
tags: [cli, tui-removal, deletion, release-boundary, source-budget]

requires:
  - phase: 10-cli-cutover
    provides: Final v1.1 CLI parser shape and SessionApi-backed session command
provides:
  - Unsupported CLI command modules deleted from active source
  - CLI TUI frontend tree deleted from active source
  - CLI cutover tests focused on final supported command surface
affects: [phase-11-release-verification, cli-surface, source-budget]

tech-stack:
  added: []
  patterns:
    - "Rust CLI tests assert final supported surface directly"
    - "Broad deleted-name denylist belongs to release boundary scripts"

key-files:
  created:
    - .planning/phases/11-deletion-budget-and-release-verification/11-02-SUMMARY.md
  modified:
    - crates/lattice-cli/Cargo.toml
    - crates/lattice-cli/src/main.rs
    - crates/lattice-cli/src/lib.rs
    - crates/lattice-cli/src/commands/mod.rs
    - crates/lattice-cli/src/commands/run.rs
    - crates/lattice-cli/tests/cli_cutover.rs
    - crates/lattice-cli/src/frontend/mod.rs
    - crates/lattice-cli/src/frontend/tui/
    - crates/lattice-cli/src/commands/run_v11.rs
    - crates/lattice-cli/src/commands/sessions.rs
    - crates/lattice-cli/src/commands/debug.rs
    - crates/lattice-cli/src/commands/config_cmd.rs
    - crates/lattice-cli/src/commands/validate.rs
    - crates/lattice-cli/src/commands/new_agent.rs
    - crates/lattice-cli/src/commands/list_agents.rs
    - crates/lattice-cli/src/commands/stats.rs
    - crates/lattice-cli/src/commands/agent_inventory.rs

key-decisions:
  - "Kept the command registry to doctor, models, resolve, run, and session only"
  - "Moved default agent directory validation into run.rs as private implementation detail instead of keeping commands/mod.rs helpers"
  - "Removed old command-name denylist tests from Rust and kept one neutral unknown-command rejection test"

patterns-established:
  - "Final CLI parser tests should count supported commands rather than preserving removed command vocabulary"
  - "Unsupported frontend source is deleted, not hidden behind module reachability"

requirements-completed: [SIMP-02, SIMP-03, VER-02, VER-03]

duration: 15min
completed: 2026-05-17
---

# Phase 11 Plan 02: CLI/TUI Deletion Summary

**Unsupported CLI command modules and the old TUI frontend tree were removed from active `lattice-cli` source, with CLI tests narrowed to the final v1.1 surface.**

## Performance

- **Duration:** 15 min
- **Started:** 2026-05-17T13:40:15Z
- **Completed:** 2026-05-17T13:55:45Z
- **Tasks:** 3
- **Files modified:** 54 tracked paths

## Accomplishments

- Deleted the unsupported command modules: `run_v11.rs`, `sessions.rs`, `debug.rs`, `config_cmd.rs`, `validate.rs`, `new_agent.rs`, `list_agents.rs`, `stats.rs`, and `agent_inventory.rs`.
- Reduced `commands/mod.rs` to only `doctor`, `models`, `resolve`, `run`, and `session`.
- Deleted `crates/lattice-cli/src/frontend/tui/` and the now-empty `frontend/mod.rs`; removed the `frontend` module from `main.rs` and `lib.rs`.
- Removed TUI-only dependencies from `crates/lattice-cli/Cargo.toml`: `ratatui`, `crossterm`, `unicode-width`, `pulldown-cmark`, `syntect`, and `similar`.
- Rewrote CLI surface tests so Rust keeps positive supported-command coverage plus one neutral unknown-command rejection.

## Task Commits

Each task was committed atomically:

1. **Task 1: Delete unsupported command modules** - `7b859fd` (feat)
2. **Task 2: Delete TUI frontend tree and dependencies** - `b060e2b` (feat)
3. **Task 3: Rewrite CLI boundary tests around supported surface** - `8f7468d` (test)

## Files Created/Modified

- `crates/lattice-cli/src/commands/mod.rs` - Supported command module registry only.
- `crates/lattice-cli/src/commands/run.rs` - Private default agent directory validation moved out of the deleted command registry helper.
- `crates/lattice-cli/Cargo.toml` - CLI description narrowed and TUI-only dependencies removed.
- `crates/lattice-cli/src/main.rs` - Removed frontend module declaration and legacy denylist parser tests.
- `crates/lattice-cli/src/lib.rs` - Removed frontend module declaration.
- `crates/lattice-cli/tests/cli_cutover.rs` - Integration tests now assert final command surface and neutral unknown-command rejection.
- `crates/lattice-cli/src/frontend/tui/` - Deleted.
- `crates/lattice-cli/src/frontend/mod.rs` - Deleted.
- `crates/lattice-cli/src/commands/{run_v11,sessions,debug,config_cmd,validate,new_agent,list_agents,stats,agent_inventory}.rs` - Deleted.

## Verification

Passed:

- `for file in run_v11.rs sessions.rs debug.rs config_cmd.rs validate.rs new_agent.rs list_agents.rs stats.rs agent_inventory.rs; do test ! -e "crates/lattice-cli/src/commands/$file"; done`
- `test ! -d crates/lattice-cli/src/frontend/tui`
- `rg -n "run_v11|sessions.rs|debug.rs|config_cmd.rs|validate.rs|new_agent.rs|list_agents.rs|stats.rs|agent_inventory.rs|frontend/tui" crates/lattice-cli/src crates/lattice-cli/tests` returned no matches.
- `./scripts/source-loc-budget.sh --max 100000` passed with current main-worktree total `52693`.
- `cargo test -p lattice-cli cli_surface` passed during Task 1 before concurrent non-CLI work changed the workspace build state: 29 parser tests passed.

Blocked by out-of-scope compile state:

- `cargo test -p lattice-cli --test cli_cutover` could not complete in the shared worktree because concurrent `crates/lattice-tools` edits left `executor.rs` missing `SandboxConfig`, `DenialReport`, `ToolOutcome`, and related fields.
- A clean `git archive HEAD` verification snapshot also failed before reaching the CLI integration tests because `crates/lattice-agent/src/turn_engine.rs:299` passes `lattice_core::ToolCall` to `lattice-tools::ToolExecutor::execute_outcome`, which expects `lattice_llm::ToolCall`.

## Decisions Made

- Kept `safe_agents_dir` functionality only as a private `run.rs` detail so removing the command registry helper did not break default profile resolution.
- Treated broad deleted-name denylist checks as release-boundary script responsibility, matching D-08/D-09.

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Preserved run profile discovery after deleting command registry helpers**
- **Found during:** Task 1
- **Issue:** The plan required deleting `safe_agents_dir` and `validate_agents_dir` from `commands/mod.rs`, but supported `run.rs` still used `crate::commands::safe_agents_dir(None)`.
- **Fix:** Moved the helper logic into private `run.rs` functions and updated `load_profile` to call the private helper.
- **Files modified:** `crates/lattice-cli/src/commands/run.rs`, `crates/lattice-cli/src/commands/mod.rs`
- **Verification:** `cargo test -p lattice-cli cli_surface` passed after the move.
- **Committed in:** `7b859fd`

---

**Total deviations:** 1 auto-fixed (1 blocking)
**Impact on plan:** The command registry helper was still removed as planned; the surviving logic is private to the supported `run` command and does not reintroduce agent inventory or validation surfaces.

## Issues Encountered

- Concurrent agents committed and edited other Phase 11 areas while this plan ran. I did not stage, edit, or revert their `lattice-tools`, `lattice-session`, `lattice-api`, or `Cargo.lock` work.
- Final Cargo test verification is blocked by non-11-02 cross-crate build errors listed above. The CLI/TUI deletion-specific file and grep gates pass.

## Known Stubs

None introduced. The `enabled = []` occurrence in `run.rs` is test fixture data, and `default = []` in `Cargo.toml` is the existing empty feature list.

## Threat Flags

None. This plan deleted CLI/TUI surfaces and did not introduce new network endpoints, auth paths, file access patterns, or schemas.

## User Setup Required

None - no external service configuration required.

## Self-Check: PASSED

- Summary file created at `.planning/phases/11-deletion-budget-and-release-verification/11-02-SUMMARY.md`.
- Required deletion targets are absent from the working tree.
- Task commits found: `7b859fd`, `b060e2b`, `8f7468d`.
- Remaining uncommitted files are outside the 11-02 write set and were left untouched.

## Next Phase Readiness

11-02's CLI/TUI deletion scope is complete. Full Cargo verification should be rerun after the concurrent `lattice-agent`/`lattice-tools` type-boundary work is reconciled.

---
*Phase: 11-deletion-budget-and-release-verification*
*Completed: 2026-05-17*

---
phase: 17-frontend-adapter-migration
fixed_at: 2026-05-13T17:24:33Z
review_path: .planning/phases/17-frontend-adapter-migration/17-REVIEW.md
iteration: 1
findings_in_scope: 9
fixed: 9
skipped: 0
status: all_fixed
---

# Phase 17: Code Review Fix Report

**Fixed at:** 2026-05-13T17:24:33Z
**Source review:** `.planning/phases/17-frontend-adapter-migration/17-REVIEW.md`
**Iteration:** 1

**Summary:**
- Findings in scope: 9
- Fixed: 9
- Skipped: 0

## Fixed Issues

### CR-01: BLOCKER - Continued sessions are keyed but not seeded

**Status:** fixed: requires human verification
**Files modified:** `crates/lattice-cli/src/coding_agent.rs`, `crates/lattice-cli/src/commands/run.rs`, `crates/lattice-cli/src/frontend/tui/agent_runner.rs`, `crates/lattice-runtime/src/lib.rs`, `crates/lattice-runtime/src/turn.rs`
**Commit:** `c46b191`
**Applied fix:** Added a seeded runtime handle path and routed CLI code, CLI run, and TUI continued-session builds through it so prior messages and latest task metadata seed only newly created handles.

### CR-02: BLOCKER - Runtime cancellation does not stop agent/tool side effects

**Status:** fixed: requires human verification
**Files modified:** `crates/lattice-runtime/src/handle.rs`, `crates/lattice-runtime/src/lib.rs`, `crates/lattice-runtime/src/turn.rs`
**Commit:** `399727d`
**Applied fix:** Added active task abort tracking, abortive cancellation, cancelled completion recording, partial assistant preservation, and replacement-agent recovery after abort.

### CR-03: BLOCKER - First plugin submission can reuse and mutate the normal coding handle

**Status:** fixed: requires human verification
**Files modified:** `crates/lattice-cli/src/frontend/tui/agent_runner.rs`, `crates/lattice-cli/src/frontend/tui/app.rs`, `crates/lattice-runtime/src/lib.rs`
**Commit:** `66debc4`
**Applied fix:** Removed plugin fallback to the normal handle and added runtime handle metadata matching so stale or wrong-profile handles are rebuilt instead of reused.

### CR-04: BLOCKER - CLI adapters return success for runtime error/cancel completions

**Status:** fixed: requires human verification
**Files modified:** `crates/lattice-cli/src/coding_agent.rs`, `crates/lattice-cli/src/commands/run.rs`
**Commit:** `9546918`
**Applied fix:** Centralized terminal completion status handling so runtime `Error` and `Cancelled` completions become CLI errors while `Completed` remains successful.

### CR-05: BLOCKER - Runtime persistence drops the final report from saved sessions

**Status:** fixed: requires human verification
**Files modified:** `crates/lattice-runtime/src/turn.rs`, `crates/lattice-cli/tests/code_agent_eval.rs`
**Commits:** `339fda8`, `4d7bbd7`, `54a6a1a`
**Applied fix:** Persisted coding final reports into saved assistant messages, scoped report appending to coding-preset handles so generic run sessions keep their existing transcript shape, and updated eval coverage to assert saved coding sessions retain the final report.

### WR-01: WARNING - `/approve` cannot target plugin handles after a plugin turn finishes

**Status:** fixed: requires human verification
**Files modified:** `crates/lattice-cli/src/frontend/tui/app.rs`
**Commit:** `47f35ea`
**Applied fix:** Tracked the latest handle with pending approval state and resolved `/approve last` through active, last-approval, then normal handles.

### WR-02: WARNING - Active handle rebuild can race with an in-flight turn

**Status:** fixed: requires human verification
**Files modified:** `crates/lattice-runtime/src/lib.rs`
**Commit:** `e450ce2`
**Applied fix:** Made `rebuild_agent_handle` reject active turns with `RuntimeError::HandleBusy`.

### WR-03: WARNING - TUI `--plugins-dir` validates a directory but never loads it

**Status:** fixed
**Files modified:** `crates/lattice-runtime/src/lib.rs`
**Commit:** `64c470a`
**Applied fix:** Changed `RuntimeBuilder::load_plugin_dir` to load manifest plugin bundles and replace registry entries from the supplied directory.

### WR-04: WARNING - `/permissions off` is advertised but rejected by runtime security

**Status:** fixed
**Files modified:** `crates/lattice-cli/src/frontend/tui/app.rs`
**Commit:** `ac3ade7`
**Applied fix:** Removed unsupported `off` from the permissions mode list, slash usage, and acceptance tests.

## Additional Commits

- `e6b31ae` - formatted review-fix source changes after `cargo fmt --all`.

## Verification

- `cargo test -p lattice-runtime seeded_session_handle_includes_prior_messages_and_task_metadata` - passed
- `cargo test -p lattice-runtime runtime_turn_cancel` - passed
- `cargo test -p lattice-cli plugin_submission_does_not_fall_back_to_normal_handle` - passed
- `cargo test -p lattice-cli ensure_agent_rejects_existing_handle_with_wrong_profile` - passed
- `cargo test -p lattice-cli completion_becomes_cli_error` - passed
- `cargo test -p lattice-cli runtime_completed_completion_remains_successful` - passed
- `cargo test -p lattice-runtime runtime_turn_completion_contains_authoritative_fields` - passed
- `cargo test -p lattice-cli approval_handle` - passed
- `cargo test -p lattice-runtime handle_rebuild_rejects_active_turn` - passed
- `cargo test -p lattice-runtime builder_load_plugin_dir_registers_manifest_plugins` - passed
- `cargo test -p lattice-cli slash_permissions` - passed
- `cargo test -p lattice-cli run_command_save_uses_runtime_persistence` - passed
- `cargo test -p lattice-cli --test code_agent_eval coding_agent_json_uses_runtime_turn_completion_final_report` - passed
- `cargo fmt --all --check` - passed
- `cargo clippy --workspace -- -D warnings` - passed
- `cargo test --workspace` - passed

## Skipped Issues

None - all in-scope findings were fixed.

---

_Fixed: 2026-05-13T17:24:33Z_
_Fixer: the agent (gsd-code-fixer)_
_Iteration: 1_

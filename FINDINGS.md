# FINDINGS — Changelog

Open findings across code review phases. Entries are append-only; mark resolved with `[resolved]` tag and date, never delete.

---

## Phase 11 — 2026-05-18

7 findings deferred beyond hot-fix scope. Each requires architectural refactoring.

### WR-02: Inconsistent env var test synchronization — `flaky-ci`
- **Files:** `crates/lattice-llm/src/router.rs` (tests), `crates/lattice-cli/src/commands/doctor.rs` (tests)
- **Issue:** `doctor.rs` uses `ENV_MUTEX` for `std::env::set_var`; `router.rs` uses raw calls. Parallel `cargo test` races.
- **Fix needed:** Apply `ENV_MUTEX` to router tests, or refactor to dependency injection for credential resolution.

### WR-03: SQLite per-method connection with no pooling — `consistency`
- **File:** `crates/lattice-session/src/sqlite_graph.rs`
- **Issue:** `self.conn()` opens fresh `rusqlite::Connection` per call. No pooling, no consistent snapshot between operations.
- **Fix needed:** Persistent `Connection` or `Mutex<Connection>` in struct, opened once at construction.

### WR-04: tool_output_is_ok fragile string prefix check — `correctness`
- **File:** `crates/lattice-agent/src/agent_loop.rs:504-506`
- **Issue:** Checks `!output.starts_with("Error:") && !output.starts_with("Tool denied:")`. Different error formats pass silently.
- **Fix needed:** `ToolCallExecutor::execute` returns `ExecutionResult { output, success }` or `Result<String, ToolError>`.

### WR-06: TOCTOU race in turn parent_commit_id resolution — `race-condition`
- **File:** `crates/lattice-api/src/turn.rs:476-481`
- **Issue:** `head_transcript` called again after agent loop for `parent_commit_id`. Concurrent turn could change HEAD between reads.
- **Fix needed:** Capture `parent_commit_id` from initial transcript read, carry through turn execution.

### IN-01: Duplicated test helpers across modules — `maintainability`
- **Files:** `crates/lattice-agent/src/agent_loop.rs`, `crates/lattice-api/src/turn.rs`, `crates/lattice-api/src/session.rs`
- **Issue:** `user_msg`, `assistant_msg`, `tool_msg` copied in 3+ test modules.
- **Fix needed:** Extract to `lattice-test-utils` crate or `tests/common`.

### IN-02: lattice-hooks crate unused — `dead-code`
- **File:** `crates/lattice-hooks/src/lib.rs`
- **Issue:** 405-line hook system never imported. Deferred per CLAUDE.md future work section.
- **Fix needed:** Remove crate and dependency, or gate with `#[cfg(feature = "hooks")]`.

### IN-03: lattice-cli depends on lattice-hooks but never imports — `unused-dep`
- **File:** `crates/lattice-cli/Cargo.toml`
- **Issue:** `lattice-hooks` in deps, no `use lattice_hooks::` in any source file. Now surfaces as warning after WR-01 fix.
- **Fix needed:** Remove from Cargo.toml until actually needed.
# FINDINGS — Changelog

Open findings across code review phases. Entries are append-only; mark resolved with `[resolved]` tag and date, never delete.

---

## Phase 11 — 2026-05-18

7 findings tracked across hardening. Some are now resolved; unresolved architectural items remain deferred.

### WR-02: Inconsistent env var test synchronization — `flaky-ci` [resolved 2026-05-19]
- **Files:** `crates/lattice-llm/src/router.rs` (tests), `crates/lattice-cli/src/commands/doctor.rs` (tests)
- **Issue:** Env-var tests previously used unsynchronized `std::env::set_var` / `remove_var`, which could race under parallel `cargo test`.
- **Fix:** Router tests now use a local env mutex helper; CLI doctor tests use shared env helpers for set/remove isolation.

### WR-03: SQLite per-method connection with no pooling — `consistency`
- **File:** `crates/lattice-session/src/sqlite_graph.rs`
- **Issue:** `self.conn()` opens fresh `rusqlite::Connection` per call. No pooling, no consistent snapshot between operations.
- **Fix needed:** Persistent `Connection` or `Mutex<Connection>` in struct, opened once at construction.

### WR-04: tool_output_is_ok fragile string prefix check — `correctness`
- **File:** `crates/lattice-agent/src/agent_loop.rs:504-506`
- **Issue:** Checks `!output.starts_with("Error:") && !output.starts_with("Tool denied:")`. Different error formats pass silently.
- **Fix needed:** `ToolCallExecutor::execute` returns `ExecutionResult { output, success }` or `Result<String, ToolError>`.

### WR-06: TOCTOU race in turn parent_commit_id resolution — `race-condition` [resolved 2026-05-19]
- **File:** `crates/lattice-api/src/turn/execute.rs`
- **Issue:** `head_transcript` was called again after agent loop for `parent_commit_id`. Concurrent turn could change HEAD between reads.
- **Fix:** Parent commit id is now captured from the initial transcript read and carried through turn execution into `TurnCommitInput`.

### IN-01: Duplicated test helpers across modules — `maintainability`
- **Files:** `crates/lattice-agent/src/agent_loop.rs`, `crates/lattice-api/src/turn.rs`, `crates/lattice-api/src/session.rs`
- **Issue:** `user_msg`, `assistant_msg`, `tool_msg` copied in 3+ test modules.
- **Fix needed:** Extract to `lattice-test-utils` crate or `tests/common`.

### IN-02: lattice-hooks crate unused — `dead-code`
- **File:** `crates/lattice-hooks/src/lib.rs`
- **Issue:** 405-line hook system never imported. Deferred per CLAUDE.md future work section.
- **Fix needed:** Remove crate and dependency, or gate with `#[cfg(feature = "hooks")]`.

### IN-03: lattice-cli depends on lattice-hooks but never imports — `unused-dep` [resolved 2026-05-19]
- **File:** `crates/lattice-cli/Cargo.toml`
- **Issue:** `lattice-hooks` was listed in CLI dependencies without any `use lattice_hooks::` in CLI source.
- **Fix:** Removed the unused `lattice-hooks` dependency from `lattice-cli`.
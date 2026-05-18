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

---

## PR #6 — 2026-05-19

5 unresolved P2 findings from code review.

### P2-01: Persist overridden model in turn commit metadata — `correctness` [resolved 2026-05-19]
- **File:** `crates/lattice-api/src/turn.rs`
- **Issue:** `model_name` is recomputed from `loaded_profile` first, overriding `request.model` during persistence even though execution honors `request.model` in `resolve_model_and_prompt`. With a loaded profile plus `-m/--model` override, committed turn metadata records the wrong model, making `session log`/audit data inaccurate.

### P2-02: Treat existing NULL-tip branch as a valid checkout target — `correctness` [resolved 2026-05-19]
- **File:** `crates/lattice-session/src/sqlite_graph.rs`
- **Issue:** Lookup flattens an existing branch with `commit_id = NULL` into `None`, making it indistinguishable from a missing branch. Checking out an existing empty branch (e.g. `main` before first commit) falls through to commit-id resolution and returns `MissingCommit` instead of succeeding as a no-op branch checkout.

### P2-03: Use detached HEAD commit when branching without --from — `correctness` [resolved 2026-05-19]
- **File:** `crates/lattice-session/src/sqlite_graph.rs`
- **Issue:** `from: None` path calls `current_branch_commit_id`, which returns `None` when HEAD is detached (only reads `head_ref`). `session branch <name>` fails with "no commits on current HEAD" immediately after `session checkout <commit-id>`, breaking the standard detached-to-branch workflow unless `--from` is manually passed.

### P2-04: Emit stream events incrementally during turn execution — `streaming`
- **File:** `crates/lattice-api/src/turn/execute.rs`
- **Issue:** `stream_turn_with_deps` awaits `execute_turn_inner` before constructing the `TurnEventStream`, so callers receive no events until the entire LLM/tool/commit pipeline finishes. In long generations or tool-heavy turns this defeats streaming semantics and can cause client-side timeouts or frozen UIs.

### P2-05: Keep session HEAD metadata in sync when moving a branch — `consistency` [resolved 2026-05-19]
- **File:** `crates/lattice-session/src/sqlite_graph.rs`
- **Issue:** Branch upsert updates `graph_refs.commit_id` but never updates `graph_sessions.head_commit_id` when the moved branch is currently checked out (`head_ref == name`). After `session branch <current-branch> --from <commit>`, status/list/reset metadata can report a stale HEAD commit.
---
phase: 18-cli-live-rendering-and-alpha-release-gate
reviewed: 2026-05-22T00:00:00Z
depth: standard
files_reviewed: 3
files_reviewed_list:
  - crates/lattice-cli/src/commands/run.rs
  - crates/lattice-cli/src/main.rs
  - scripts/release-gate.sh
findings:
  critical: 0
  warning: 3
  info: 4
  total: 7
status: issues_found
---

# Phase 18: Code Review Report

**Reviewed:** 2026-05-22
**Depth:** standard
**Files Reviewed:** 3
**Status:** issues_found

## Summary

Reviewed the Phase 18 CLI live-rendering rewrite (`run.rs`), the `--json` help text update (`main.rs`), and the new release gate script (`release-gate.sh`). Reference type `TurnEvent` from `lattice-api/src/turn/types.rs` was used to verify correctness.

The stream-draining logic (D-A6), stdout/stderr separation (D-B1), JSONL contract (D-D1/D-D3), and exit-code matrix (D-A4 rows 1-7) are structurally sound. The most significant finding is a bug in the mid-token newline trick (D-B3): the `tokens_emitted_without_newline` flag is never reset after `flush_before_status` applies the trick, causing duplicate blank lines on stderr for every status event that follows a partial token. Two additional warnings concern the release-gate script omitting three workspace crates and the `exit_code_from_state` function not guarding against a protocol-violating `TurnFailed + TurnCommitted{Completed}` combination.

## Warnings

### WR-01: Mid-token newline trick never resets flag, producing duplicate stderr blank lines

**File:** `crates/lattice-cli/src/commands/run.rs:288-297`
**Severity:** Warning
**Category:** bug
**Description:** The `flush_before_status` function checks `state.tokens_emitted_without_newline` and, if true, flushes stdout and emits `\n` on stderr. However, after applying this trick, the flag is never reset to `false`. This means every subsequent status event (ToolCallFinished, TurnCommitting, TurnCommitted, etc.) will also trigger the newline trick, inserting an unwanted blank line on stderr between each status line.

Concrete scenario: events `[TokenDelta("foo"), ToolCallStarted, ToolCallFinished, TurnCommitted{Completed}]` produce stderr with three extra `\n` prefixes (one per status event after the token), rather than just one before the first status event.

The test suite does not cover this case — all interleave tests have only a single status event after a partial token, so the duplicate-newline behavior is never exercised.

**Fix:** Reset `tokens_emitted_without_newline` to `false` after the newline trick is applied. Since `render_human_event` already takes `&mut RunRenderState`, it can perform this reset. Move the reset into `flush_before_status` (which needs a `&mut` borrow on state) or into `render_human_event` after calling `flush_before_status`:

```rust
fn flush_before_status<WOut: Write, WErr: Write>(
    out: &mut WOut,
    err: &mut WErr,
    state: &mut RunRenderState,  // changed from &RunRenderState
) -> std::io::Result<()> {
    if state.tokens_emitted_without_newline {
        out.flush()?;
        writeln!(err)?;
        state.tokens_emitted_without_newline = false;  // reset after applying trick
    }
    Ok(())
}
```

Add a test covering consecutive status events after a partial token to prevent regression.

### WR-02: release-gate.sh omits three workspace crates

**File:** `scripts/release-gate.sh:13-23`
**Severity:** Warning
**Category:** quality
**Description:** The release gate script tests four crates (`lattice-llm`, `lattice-agent`, `lattice-api`, `lattice-cli`) but omits `lattice-session`, `lattice-tools`, and `lattice-profile`. These three crates contain core functionality (session persistence with SQLite, tool execution and path containment, agent profile resolution). A bug in any of them would pass the release gate undetected.

**Fix:** Replace the per-crate test lines with a single workspace-wide test, or add the missing crates:

```bash
echo "==> cargo test --workspace"
cargo test --workspace
```

Or add the three missing crates individually:

```bash
echo "==> cargo test -p lattice-session"
cargo test -p lattice-session

echo "==> cargo test -p lattice-tools"
cargo test -p lattice-tools

echo "==> cargo test -p lattice-profile"
cargo test -p lattice-profile
```

### WR-03: exit_code_from_state does not guard against TurnFailed + TurnCommitted{Completed} protocol violation

**File:** `crates/lattice-cli/src/commands/run.rs:148-169`
**Severity:** Warning
**Category:** bug
**Description:** The `exit_code_from_state` function checks `CommitSignal::Failed` first (Row 4), then `last_status == Completed` (Row 1). If a stream emits both `TurnFailed` and `TurnCommitted{Completed}` (a protocol violation), the function returns exit 0 because `last_status == Completed` matches before any check on `last_terminal`.

The Row 5/6 guard (`last_terminal.is_some() && last_commit.is_none()`) only fires when there is NO commit signal. It does not fire when both terminal and commit signals coexist.

While this is a protocol-violating scenario that should not occur in normal operation, defensive code should return exit 1 if any `TurnFailed` or `TurnCancelled` event was received, regardless of what `TurnCommitted` status follows. The current code trusts `TurnCommitted.status` over the `TurnFailed` signal, which is the correct interpretation per the D-A4 matrix rows, but the matrix does not define a row for this impossible combination. A defensive check would make the CLI more robust against upstream stream bugs.

**Fix:** Add a guard before the `last_status == Completed` check:

```rust
fn exit_code_from_state(state: &RunRenderState) -> std::process::ExitCode {
    // Row 4: CommitFailed -> 1
    if matches!(state.last_commit, Some(CommitSignal::Failed { .. })) {
        return std::process::ExitCode::from(1);
    }
    // Defensive: if TurnFailed/TurnCancelled was seen, exit 1 regardless of commit status
    if state.last_terminal.is_some() {
        return std::process::ExitCode::from(1);
    }
    // Row 1: TurnCommitted{Completed} -> 0
    if state.last_status == Some(RunTurnStatus::Completed) {
        return std::process::ExitCode::SUCCESS;
    }
    // Row 2/3: TurnCommitted{Error|Cancelled} -> 1
    if state.last_status == Some(RunTurnStatus::Error)
        || state.last_status == Some(RunTurnStatus::Cancelled)
    {
        return std::process::ExitCode::from(1);
    }
    // Row 7: no terminal/commit signal -> 1
    std::process::ExitCode::from(1)
}
```

Note: This changes the semantics of Row 5/6 (they are now subsumed by the broader `last_terminal.is_some()` guard). The existing Row 5/6 check (`last_terminal.is_some() && last_commit.is_none()`) becomes redundant and can be removed.

## Info

### IN-01: render_human_event takes &mut RunRenderState but never mutates it

**File:** `crates/lattice-cli/src/commands/run.rs:186-192`
**Severity:** Info
**Category:** quality
**Description:** The `render_human_event` function signature takes `state: &mut RunRenderState`, but the doc comment states "This function does not mutate `state`." The function only reads state (via `flush_before_status` which currently takes `&RunRenderState`). The `&mut` borrow is unnecessarily restrictive and misleading about the function's contract.

Note: If WR-01 is fixed by making `flush_before_status` reset the flag (requiring `&mut`), this signature becomes honestly necessary and this Info item is resolved.

**Fix:** If WR-01 is NOT adopted, change the signature to `state: &RunRenderState` and propagate the immutable borrow to `flush_before_status`.

### IN-02: verbose epilogue formats status with {:?} (Debug) instead of lowercase display

**File:** `crates/lattice-cli/src/commands/run.rs:315-317`
**Severity:** Info
**Category:** quality
**Description:** The verbose epilogue renders `last_status` using `{:?}` (Debug format), which produces capitalized enum variant names (`Completed`, `Error`, `Cancelled`). The `TurnCommitted` human-mode renderer produces lowercase strings (`completed`, `error`, `cancelled`). This formatting inconsistency means the committed line and the epilogue line show the same status value in different capitalization.

**Fix:** Use a Display-style match instead of Debug format:

```rust
if let Some(ref status) = state.last_status {
    let status_str = match status {
        RunTurnStatus::Completed => "completed",
        RunTurnStatus::Error => "error",
        RunTurnStatus::Cancelled => "cancelled",
    };
    writeln!(err, "{}: {}", "status".dimmed(), status_str)?;
}
```

### IN-03: status_to_exit_code is dead code preserved only for legacy tests

**File:** `crates/lattice-cli/src/commands/run.rs:612-619`
**Severity:** Info
**Category:** quality
**Description:** The `status_to_exit_code` function is annotated `#[allow(dead_code)]` and only used by three legacy tests (`run_status_to_exit_code_completed_is_success`, `run_status_to_exit_code_error_is_1`, `run_status_to_exit_code_cancelled_is_1`). The new `exit_code_from_state` function and its D-A4 matrix tests cover the same behavior more comprehensively. The dead function and its tests add maintenance burden without increasing coverage.

**Fix:** Remove `status_to_exit_code` and its three dedicated tests. The `exit_code_from_state_covers_all_da4_rows` test already covers Rows 1-3 which correspond to the same cases.

### IN-04: handle_profile_error has unused _source parameter

**File:** `crates/lattice-cli/src/commands/run.rs:489-493`
**Severity:** Info
**Category:** quality
**Description:** The `handle_profile_error` function takes `_source: &ProfileSource` but never uses it (the underscore prefix confirms this). The parameter was likely intended for future error-context rendering but currently adds nothing.

**Fix:** Remove the `_source` parameter from the function signature and update the call site:

```rust
fn handle_profile_error(
    error: anyhow::Error,
    project_root: &PathBuf,
) -> Result<std::process::ExitCode> {
```

---

_Reviewed: 2026-05-22_
_Reviewer: Claude (gsd-code-reviewer)_
_Depth: standard_
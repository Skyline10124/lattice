---
phase: 17-frontend-adapter-migration
reviewed: 2026-05-13T16:38:24Z
depth: standard
files_reviewed: 28
files_reviewed_list:
  - crates/lattice-cli/src/coding_agent.rs
  - crates/lattice-cli/src/commands/debug.rs
  - crates/lattice-cli/src/commands/mod.rs
  - crates/lattice-cli/src/commands/run.rs
  - crates/lattice-cli/src/frontend/tui/action.rs
  - crates/lattice-cli/src/frontend/tui/agent_runner.rs
  - crates/lattice-cli/src/frontend/tui/app.rs
  - crates/lattice-cli/src/frontend/tui/bottom.rs
  - crates/lattice-cli/src/frontend/tui/commands.rs
  - crates/lattice-cli/src/frontend/tui/event.rs
  - crates/lattice-cli/src/frontend/tui/interaction.rs
  - crates/lattice-cli/src/frontend/tui/mod.rs
  - crates/lattice-cli/src/frontend/tui/plugin_mode.rs
  - crates/lattice-cli/src/frontend/tui/session_state.rs
  - crates/lattice-cli/src/frontend/tui/turn.rs
  - crates/lattice-cli/src/frontend/tui/ui.rs
  - crates/lattice-cli/src/frontend/tui/update.rs
  - crates/lattice-cli/src/frontend/tui/view_model.rs
  - crates/lattice-cli/src/frontend/tui/widgets/selection.rs
  - crates/lattice-cli/src/lib.rs
  - crates/lattice-cli/src/main.rs
  - crates/lattice-cli/src/session.rs
  - crates/lattice-cli/src/test_support.rs
  - crates/lattice-cli/tests/code_agent_eval.rs
  - crates/lattice-cli/tests/frontend_runtime_guards.rs
  - crates/lattice-runtime/src/handle.rs
  - crates/lattice-runtime/src/lib.rs
  - crates/lattice-runtime/src/turn.rs
findings:
  critical: 5
  warning: 4
  info: 0
  total: 9
status: issues_found
---

# Phase 17: Code Review Report

**Reviewed:** 2026-05-13T16:38:24Z
**Depth:** standard
**Files Reviewed:** 28
**Status:** issues_found

## Summary

Reviewed the Phase 17 frontend adapter migration source scope with focus on runtime-owned handles, approval state, TUI cancellation/queue/plugin consistency, final report persistence, and regression guards.

The migration has several correctness blockers: continued sessions lose prompt history and task metadata, cancellation does not stop underlying agent/tool execution, plugin submissions can contaminate the normal coding handle, CLI commands ignore runtime terminal error status, and saved sessions drop final-report content. The existing `frontend_runtime_guards` test passed, but it only checks broad active-source strings and does not protect these behavioral regressions.

## Critical Issues

### CR-01: BLOCKER - Continued sessions are keyed but not seeded

**File:** `crates/lattice-cli/src/coding_agent.rs:130`

**Issue:** `build_coding_agent_with_runtime` loads prior messages from `prior_messages` or `previous_session`, then discards them with `let _messages = messages;` before creating the runtime handle. `commands/run.rs` similarly passes only `session_id` into `get_or_create_agent_handle_for_session` without seeding history, and the TUI build path passes `previous_session` only to derive the handle key. This means `--continue`, `--session`, and TUI checkout reuse a durable session id for persistence, but the LLM does not see previous user/assistant/tool messages. The old task-state manifest seed is also now dead code, so pending approvals, todos, verification failures, and changed-file state from the previous turn are lost.

**Fix:**
Add a seeded-handle path and use it from CLI run/code and TUI:

```rust
let seed_messages = previous_session
    .as_ref()
    .map(crate::session::messages_for_agent)
    .unwrap_or_default();
let task_state_seed = load_latest_manifest_metadata(&runtime, previous_session.as_ref());

let handle_id = runtime.get_or_create_agent_handle_for_session_seeded(
    turn_spec,
    security,
    session_id,
    seed_messages,
    task_state_seed,
)?;
```

The runtime should apply the seed only when creating a fresh handle for that session/profile/model key, and tests should assert that a continued scripted request includes earlier session content in the provider request.

### CR-02: BLOCKER - Runtime cancellation does not stop agent/tool side effects

**File:** `crates/lattice-runtime/src/turn.rs:219`

**Issue:** `cancel_turn` only flips `cancellation_requested`. `execute_turn` checks that flag inside the `run_streaming` observer and suppresses emitted events, then marks the completion as `Cancelled` after `run_streaming` returns. The observer has no control channel back into `Agent::run_streaming`; the underlying turn continues running until completion. In particular, if a cancelled turn has already received or later receives tool calls, the agent turn engine can still execute those tools, including mutating filesystem commands, while the UI reports cancellation and suppresses tool output.

**Fix:**
Make cancellation cooperative at the agent/tool boundary or abortive at the task boundary:

```rust
// Runtime handle state should track the spawned JoinHandle or a CancellationToken.
state.active_task = Some(tokio::spawn(run_turn(token.clone(), ...)));

pub fn cancel_turn(&self, handle_id: &AgentHandleId) -> Result<(), RuntimeError> {
    state.cancellation.cancel();
    if let Some(task) = state.active_task.take() {
        task.abort();
    }
    Ok(())
}
```

If aborting is too coarse, pass a cancellation token into `Agent::run_streaming` and check it before every provider retry and before each `executor.execute_outcome(call).await`; cancelled turns must not execute later tool calls or persist state.

### CR-03: BLOCKER - First plugin submission can reuse and mutate the normal coding handle

**File:** `crates/lattice-cli/src/frontend/tui/app.rs:356`

**Issue:** For plugin submissions, `existing_handle` falls back to `self.normal_handle` when the plugin has no cached handle. `ensure_agent` then accepts any existing handle as long as the model did not change and `agent_handle_snapshot` succeeds. The first `/plugin CodeReview ...` after a normal coding turn therefore reuses the normal handle, inserts the same id into `plugin_handles`, and applies the plugin overlay to the normal conversation state. This breaks plugin/normal state isolation and can pollute prompt history, task state, and pending approvals across modes.

**Fix:**
Never fall back to the normal handle for a plugin request, and validate the intended handle kind before reuse:

```rust
let existing_handle = match plugin.as_ref() {
    Some(plugin) => self.plugin_handles.get(&plugin.name).cloned(),
    None => self.normal_handle.clone(),
};
```

Also extend `AgentHandleSnapshot` with profile/session key metadata or add a runtime `agent_handle_matches` helper so `ensure_agent` can reject an existing handle whose profile/provider/security/session does not match the requested build.

### CR-04: BLOCKER - CLI adapters return success for runtime error/cancel completions

**File:** `crates/lattice-cli/src/coding_agent.rs:85`

**Issue:** `submit_coding_runtime_turn` and `submit_run_runtime_turn` drain the runtime stream and return the latest `TurnCompletion` regardless of `completion.status`. The callers then render the completion and return `Ok(())`. Provider errors, persistence failures, and cancellations are therefore converted into successful CLI exits, which breaks automation and hides failed work. The renderers may print an error event while still exiting with success.

**Fix:**
Centralize terminal-status handling in the submit helpers:

```rust
let completion = runtime
    .latest_turn_completion(handle_id)?
    .ok_or_else(|| anyhow::anyhow!("runtime turn completed without stored completion"))?;

match completion.status {
    TurnStatus::Completed => Ok(completion),
    TurnStatus::Cancelled => anyhow::bail!("runtime turn was cancelled"),
    TurnStatus::Error => anyhow::bail!(
        "{}",
        completion.error.as_deref().unwrap_or("runtime turn failed")
    ),
}
```

Add CLI tests that a scripted `RuntimeTurnEvent::Error` or persistence failure produces a non-zero command result.

### CR-05: BLOCKER - Runtime persistence drops the final report from saved sessions

**File:** `crates/lattice-runtime/src/turn.rs:381`

**Issue:** `execute_turn` computes `final_report`, but `persist_turn_session` saves only `assistant_text` as the assistant message. The CLI still renders the final report to stdout/JSON from `TurnCompletion`, but the durable session omits verification, residual risk, and changed-file report content. Reloading or continuing a saved session loses the user-visible audit trail that the command printed at completion.

**Fix:**
Persist the final report either in the assistant message or in a structured session field that is loaded back into the transcript:

```rust
let persisted_assistant = match final_report.as_deref() {
    Some(report) if !report.trim().is_empty() => {
        format!("{}\n\n{}", assistant_text.trim_end(), report)
    }
    _ => assistant_text.clone(),
};

persist_turn_session(..., &persisted_assistant, ...)?;
```

If generic `run` should not append reports, add an explicit `TurnRequest` persistence mode and keep coding-agent sessions report-complete.

## Warnings

### WR-01: WARNING - `/approve` cannot target plugin handles after a plugin turn finishes

**File:** `crates/lattice-cli/src/frontend/tui/app.rs:496`

**Issue:** `approve_pending_proposal` only checks `active_handle` or `normal_handle`. After a plugin turn finishes, `active_handle` is cleared and pending proposals remain on the plugin handle. If no normal handle exists, `/approve last` reports no agent state; if one exists, it tries to approve the wrong handle.

**Fix:** Track `last_completed_handle`/`last_approval_handle`, or search `plugin_handles` for pending approval proposals via a runtime query. `/approve last` should resolve against the handle that produced the latest pending proposal, not always the normal handle.

### WR-02: WARNING - Active handle rebuild can race with an in-flight turn

**File:** `crates/lattice-runtime/src/lib.rs:766`

**Issue:** `rebuild_agent_handle` does not reject `state.active_turn`. During a submitted turn, `state.agent` has been taken, so rebuild snapshots empty messages/task state, installs a new agent and key while the old task is still running, and the old task can later overwrite the rebuilt agent when it returns. That corrupts handle model/key/state consistency.

**Fix:** Return `RuntimeError::HandleBusy` when `state.active_turn` is true. If rebuild-after-cancel is needed, cancel and wait for the active turn to store its completion before replacing the agent/key.

### WR-03: WARNING - TUI `--plugins-dir` validates a directory but never loads it

**File:** `crates/lattice-cli/src/frontend/tui/mod.rs:95`

**Issue:** TUI accepts `plugins_dir` and calls `RuntimeBuilder::load_plugin_dir`, but `load_plugin_dir` only checks existence and stores `_plugin_dir`; it does not mutate the plugin registry. Users selecting a local manifest plugin directory will still see only the official plugin registry.

**Fix:** Either load the plugin manifests into `PluginRegistry` before assigning `app.plugin_registry`, or return an explicit unsupported error instead of silently ignoring the directory.

### WR-04: WARNING - `/permissions off` is advertised but rejected by runtime security

**File:** `crates/lattice-cli/src/frontend/tui/app.rs:778`

**Issue:** The TUI menu and usage text include `off`, but `build_runtime_security` only accepts `project`, `strict`, and `permissive`. Selecting `/permissions off` passes local validation and then fails with an "unknown security.sandbox_mode" error. This is a broken control path in the permissions UI.

**Fix:** Implement `off` in `build_runtime_security` with the intended semantics, or remove `off` from `permission_modes()` and the usage text until it is supported.

---

_Reviewed: 2026-05-13T16:38:24Z_
_Reviewer: the agent (gsd-code-reviewer)_
_Depth: standard_

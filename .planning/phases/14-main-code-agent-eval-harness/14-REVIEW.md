---
phase: 14-main-code-agent-eval-harness
reviewed: 2026-05-12T16:11:10Z
depth: standard
files_reviewed: 7
files_reviewed_list:
  - crates/lattice-prompt/src/project_instructions_provider.rs
  - crates/lattice-agent/src/state.rs
  - crates/lattice-agent/src/tool_result.rs
  - crates/lattice-agent/src/prompt/task_state_provider.rs
  - crates/lattice-agent/src/final_report.rs
  - crates/lattice-cli/src/security.rs
  - crates/lattice-policy/src/authority.rs
findings:
  critical: 0
  warning: 0
  info: 0
  total: 0
status: clean
triage:
  reviewed_commit: b34ca77
  resolved: [CR-02, CR-03, CR-04, CR-05, WR-01]
  accepted_false_positive: [CR-01]
---

# Phase 14: Code Review Report

**Reviewed:** 2026-05-12T16:11:10Z
**Depth:** standard
**Files Reviewed:** 7
**Status:** clean after triage

## Summary

Reviewed the Phase 14 gap-closure source scope for project instruction precedence, task-state recovery, approval cleanup, final reporting, CLI approval dispatch, and policy authority changes. The original review found six items. Follow-up commit `b34ca77` resolved the real trust-boundary, command matching, prompt loading, UTF-8 trimming, and todo-state findings. CR-01 was accepted as a false positive because 14-06 intentionally locked file-tier-major instruction ordering in UAT.

## Triage Result

| ID | Status | Resolution |
|----|--------|------------|
| CR-01 | Accepted false positive | 14-06 UAT explicitly requires `CLAUDE.md` tier, then `AGENTS.md` tier, then `LATTICE.md` tier, with parent-before-child ordering inside each tier. |
| CR-02 | Resolved | `ProjectInstructionsProvider` now skips symlinked instruction files and checks canonical targets stay under the project root. |
| CR-03 | Resolved | Approval proposal recording now trusts typed `ToolOutcome::Denied` values only; plain tool text is no longer parsed into approval proposals. |
| CR-04 | Resolved | Shared `PolicyAuthority` command-prefix matching now rejects shell-control operators before token-prefix matching. |
| CR-05 | Resolved | Verification output trimming now backs up to a UTF-8 character boundary before slicing. |
| WR-01 | Resolved | Successful empty `todo_write` calls now clear stale todos; malformed non-empty todo payloads preserve existing state atomically. |

## Follow-Up Verification

- `cargo test -p lattice-agent tool_result` - PASS
- `cargo test -p lattice-agent approval_proposal` - PASS
- `cargo test -p lattice-policy command_matches_allowlist` - PASS
- `cargo test -p lattice-prompt project_instruction_precedence` - PASS

## Critical Issues

### CR-01: BLOCKER - Project instruction render order violates the stated directory precedence

**File:** `/home/astrin/Dev/lattice/crates/lattice-prompt/src/project_instructions_provider.rs:72`
**Issue:** `instruction_sources()` iterates file tier first and directory second, so the rendered order is `root CLAUDE`, `child CLAUDE`, `root AGENTS`, `child AGENTS`, `root LATTICE`, `child LATTICE`. That contradicts the emitted rule text at lines 148-151 saying deeper directories override parent directories. In a root/child conflict, `root LATTICE.md` appears after `child CLAUDE.md` and `child AGENTS.md`, so a parent file can override local instructions. The byte cap at lines 122-134 also consumes budget in this low-to-high render order, so large low-precedence files can prevent higher-precedence `AGENTS.md`/`LATTICE.md` content from being loaded at all.
**Fix:**
```rust
// Build by directory from root to cwd, and within each directory render
// low-to-high file precedence. This preserves "later local blocks win".
for (dir, directory_priority) in &dirs {
    for (filename, file_priority) in INSTRUCTION_FILES_RENDER_ORDER {
        let path = dir.join(filename);
        if path.is_file() {
            sources.push(InstructionSource {
                path,
                priority_label: format!("priority={directory_priority}/{file_priority}"),
            });
        }
    }
}
```
Also apply the byte budget after prioritizing required high-precedence files, or reserve per-source budget so `LATTICE.md` cannot be starved by earlier lower-precedence files.

### CR-02: BLOCKER - Symlinked instruction files can leak files outside the project root

**File:** `/home/astrin/Dev/lattice/crates/lattice-prompt/src/project_instructions_provider.rs:75`
**Issue:** `path.is_file()` follows symlinks, and line 129 reads the file without checking the canonical target. A repository can make `AGENTS.md`, `CLAUDE.md`, or `LATTICE.md` a symlink to a sensitive file outside the project; the provider will load up to 64 KiB of that target into the model prompt.
**Fix:**
```rust
let root = canonical_or_original(&self.root);
let path = dir.join(filename);
let Ok(meta) = std::fs::symlink_metadata(&path) else {
    continue;
};
if meta.file_type().is_symlink() {
    continue;
}
let Ok(canonical_path) = path.canonicalize() else {
    continue;
};
if !canonical_path.starts_with(&root) || !canonical_path.is_file() {
    continue;
}
```

### CR-03: BLOCKER - Untrusted tool text can forge approval proposals

**File:** `/home/astrin/Dev/lattice/crates/lattice-agent/src/tool_result.rs:15`
**Issue:** `render_tool_outcome()` enriches `ToolOutcome::Text` by parsing arbitrary text as `DenialReport` (lines 25-28), and `classify_tool_result()` trusts any result containing `"__approval_proposal__"` before proving it is a typed policy denial (lines 116-118). A file read, command output, or other untrusted tool result that is JSON with `__denial_report__` or `__approval_proposal__` can create a pending approval proposal even though no sandbox/policy denial occurred.
**Fix:**
```rust
pub(crate) fn render_tool_outcome(outcome: ToolOutcome) -> String {
    match outcome {
        ToolOutcome::Text(result) => result,
        ToolOutcome::Denied(denial) => Self::enrich_denial_report(&denial),
    }
}

pub(crate) fn classify_tool_result(&mut self, call: &ToolCall, result: &str) {
    let Ok(denial) = serde_json::from_str::<DenialReport>(result) else {
        // continue with patch/bash/verification classification
        return self.classify_non_denial_tool_result(call, result);
    };
    if denial.sentinel {
        self.record_approval_blocker_from_policy_denial(call, &denial);
        return;
    }
    self.classify_non_denial_tool_result(call, result);
}
```
The approval marker should be derived from the parsed `DenialReport` inside trusted denial handling, not accepted from arbitrary result JSON.

### CR-04: BLOCKER - Static command allowlist permits shell command chaining

**File:** `/home/astrin/Dev/lattice/crates/lattice-policy/src/authority.rs:34`
**Issue:** `command_matches_allowlist()` authorizes by whitespace token prefix. Because the bash executor runs commands through `sh -c`, a command such as `cargo test && disallowed-command` matches the static allowlist entry `cargo test` at lines 250-255 and then executes the entire shell string. Dangerous-pattern revalidation only runs inside the dynamic grant branch at lines 258-273, so static defaults like `cargo test`, `git status`, `grep`, and `find` can be used as a prefix for additional shell operations.
**Fix:**
```rust
fn contains_shell_control(command: &str) -> bool {
    ["&&", "||", ";", "|", "$(", "`", ">", "<"].iter().any(|op| command.contains(op))
}

pub fn command_matches_allowlist(cmd: &str, entry: &str) -> bool {
    if contains_shell_control(cmd) {
        return false;
    }
    // existing token-prefix comparison after rejecting shell composition
}
```
A stronger fix is to parse and execute argv without `sh -c`, then apply prefix matching to the parsed command only.

### CR-05: BLOCKER - Verification output truncation can panic on UTF-8 boundaries

**File:** `/home/astrin/Dev/lattice/crates/lattice-agent/src/tool_result.rs:331`
**Issue:** `trim_tool_output()` slices `&result[..500]` when `result.len() > 500`. Rust string indices are byte offsets; if byte 500 falls inside a multi-byte character, classification of a verification result panics and can abort the agent turn. `AgentState::push_tool_result()` already handles this correctly with `is_char_boundary`, but this helper does not.
**Fix:**
```rust
fn trim_tool_output(result: &str) -> String {
    const MAX: usize = 500;
    if result.len() <= MAX {
        return result.to_string();
    }
    let mut end = MAX;
    while end > 0 && !result.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &result[..end])
}
```

## Warnings

### WR-01: WARNING - Successful empty todo_write results cannot clear stale task state

**File:** `/home/astrin/Dev/lattice/crates/lattice-agent/src/tool_result.rs:75`
**Issue:** `record_todo_result()` parses todo items with `filter_map()` and returns when `items.is_empty()` at lines 75-103. A successful `todo_write` with `{"todos":[]}` should clear the todo list, but this code leaves the previous todos in task state. The same `filter_map()` also silently drops malformed items and applies a partial replacement, instead of treating the successful tool call atomically.
**Fix:**
```rust
let mut items = Vec::with_capacity(todos.len());
for todo in todos {
    let Some(content) = todo.get("content").and_then(|v| v.as_str()) else {
        return;
    };
    let status = match todo.get("status").and_then(|v| v.as_str()) {
        Some("pending") => TodoStatus::Pending,
        Some("in_progress") => TodoStatus::InProgress,
        Some("completed") => TodoStatus::Completed,
        _ => return,
    };
    items.push(TodoItem {
        id: 0,
        content: content.trim().to_string(),
        status,
    });
}
self.state.task_state_mut().replace_todos(items).ok();
```
This lets an empty successful list clear state while preserving all-or-nothing parsing for non-empty lists.

---

_Reviewed: 2026-05-12T16:11:10Z_
_Reviewer: the agent (gsd-code-reviewer)_
_Depth: standard_

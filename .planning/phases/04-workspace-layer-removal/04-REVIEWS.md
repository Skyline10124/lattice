---
phase: 4
reviewers: [claude, opencode]
failed_reviewers: [cursor]
reviewed_at: 2026-05-15T12:01:18Z
plans_reviewed: ["04-01-PLAN.md", "04-02-PLAN.md", "04-03-PLAN.md"]
---

# Cross-AI Plan Review - Phase 4

## Consensus Summary

Phase 4 is approved for execution by both successful reviewers. Both reviews assess the plan set as low risk, primarily mechanical deletion work, and aligned with the milestone goal of removing plugin/orchestrator/pipeline/subagent layers from the active workspace. No blocking issues were identified.

### Agreed Strengths

- The phase uses compiler/test gates as the main structural verification instead of adding replacement grep-based guards.
- The plan set correctly preserves the remaining generic single-agent path while deleting obsolete extension/orchestration crates, test guards, stale dependencies, and wiki material.
- The execution order is reviewable: 04-01 removes workspace members first, then 04-02 and 04-03 perform follow-up cleanup in parallel.
- The plans are grounded in exact paths, exact string edits, and read-before-edit discipline.

### Agreed Concerns

- MEDIUM: 04-01 should explicitly ensure crate directory deletion and root `Cargo.toml` cleanup are committed atomically, or document that an intermediate broken workspace commit is intentional.
- MEDIUM: 04-02 and 04-03 contain a `\n---\n---` formatting artifact immediately after frontmatter; this should be cleaned before execution if the GSD parser treats frontmatter strictly.
- LOW: `Cargo.lock` stale entries after crate removal are expected and harmless, but the plans should mention this to prevent unnecessary lockfile churn.
- LOW: 04-03 would benefit from light preflight checks before deleting passing guard tests and `wiki/`, especially preserving `profile_runtime_eval.rs` and confirming the wiki file count.
- LOW: Phase 5 owns documentation/map cleanup, but the execution summary should call out any remaining stale `wiki/` or removed-crate references instead of silently leaving them.

### Divergent Views

- Commit/task granularity in 04-01: Claude flags the task split as a medium concern because per-task commits could create a broken intermediate revision. OpenCode considers the current single-plan grouping acceptable but still suggests documenting stale lockfile and failure-investigation expectations.
- 04-03 dependency on 04-01: Claude treats the dependency as correct because the two Wave 2 plans are parallel after 04-01. OpenCode notes guard test deletion is technically independent, but agrees the dependency is narratively coherent and not a blocker.

### Recommendation

Proceed after incorporating the small plan cleanup items above, especially the frontmatter artifact fix and an explicit note about atomic 04-01 commit handling.

---

## Claude Review

# Phase 4 Plan Review: Workspace Layer Removal

## Overall Assessment

Phase 4 is a pure deletion phase - three obsolete crates, dead Cargo.toml declarations, transitional guard tests, a wiki directory, and stale forbidden-string checks. The three plans collectively cover all four phase requirements (REM-01 through REM-04) with clear task breakdowns, exact string-match edits, and compiler-gated verification. The plans are well-researched, conservative about blast radius, and correctly defer documentation/map updates to Phase 5. The primary risk is mechanical (exact string matching against Cargo.toml/forbidden arrays drifting from current file state), but the plans' emphasis on read-before-edit mitigates this.

---

## 04-01-PLAN.md - Crate Directory Deletion + Root Cargo.toml Cleanup

### Strengths

- **Compile-gate at the right point.** Task 3 runs `cargo check --workspace` as the single source of truth - no custom grep scripts or fragile CI checks.
- **Explicit acceptance of transient failure.** Task 1 documents that `cargo check` will fail between directory deletion and Cargo.toml edits, and explicitly tells the executor not to run it during Task 1. This avoids false-alarm debugging.
- **Exact string matching for Cargo.toml edits.** Six independent Edit operations with precise match strings (including trailing newlines) eliminate regex ambiguity and accidental cross-line matches.
- **Good threat model coverage.** T-04-01 (wrong directory deletion) and T-04-02 (wrong Cargo.toml line removal) both mitigated by exact paths/strings.
- **Success criterion #4 correctly calls out stale Cargo.lock entries as expected.** No unnecessary lockfile churn.

### Concerns

- **MEDIUM - The plan splits directory deletion and Cargo.toml edits across two tasks but doesn't specify whether they share a commit.** RESEARCH.md recommends batching them atomically in one commit to avoid a broken intermediate state. The plan's task structure allows an executor to commit after Task 1 (broken workspace) and again after Task 2 (fixed). The `autonomous: true` flag and GSD convention likely produce per-task commits, so the intermediate commit would fail `cargo check`. Either batch them or document that the intermediate commit is intentionally broken.
- **LOW - Task 3 verification command is fragile.** `grep -q "Finished"` depends on Cargo's exact output phrasing. A cargo version bump could change "Finished" to "Completed" or similar. Prefer `cargo check --workspace; echo $?` and check exit code 0.
- **LOW - No mention of `Cargo.lock` in the plan actions.** The RESEARCH.md notes `Cargo.lock` will contain stale entries after crate removal, which is harmless. The plan's success criteria mention it, but an executor who hasn't read RESEARCH.md might wonder about the stale lockfile. A one-line note in Task 2 or 3 would help.

### Suggestions

- Consider batching Tasks 1+2 into a single task (or at minimum, add a note that they should share one commit) to match the RESEARCH.md recommendation.
- Replace the grep-based verification in Task 3 with exit-code checking: `cargo check --workspace && echo "Workspace compiles cleanly"`.
- Add a brief note in Task 2 action: "Cargo.lock will contain stale entries - this is expected and harmless; it auto-regenerates on next `cargo build`."

---

## 04-02-PLAN.md - Dead Dependency + Forbidden-String Cleanup

### Strengths

- **Correct scoping of D-03.** Only `lattice_orchestrator` and `lattice_plugin` are removed from forbidden arrays. All other entries (`lattice_runtime`, `lattice_agent`, `code_agent`, `AgentPreset`, `ToolPreset`, etc.) remain. This is exactly right - these are still valid architectural boundaries.
- **Good read_first discipline.** Both tasks specify reading the target files before editing, which guards against stale line numbers or formatting drift.
- **Explicit non-action on features.** The plan notes `[features]` section doesn't reference removed crates, so no feature-flag changes are needed. This prevents over-engineering.
- **Verification uses `cargo check -p lattice-cli` and `cargo test -p lattice-profile`.** Scoped to the affected crates rather than full workspace, which is faster and more precise.

### Concerns

- **MEDIUM - Document structure has a formatting artifact.** There's a `\n---\n---` between the YAML frontmatter closing and the `<objective>` block. This appears to be a template rendering issue (the `\n---` looks like an unrendered escape). Any YAML parser or GSD executor reading the frontmatter may misinterpret the document boundary. Same issue in 04-03-PLAN.md.
- **LOW - Requirements field claims REM-02 and REM-03, but these are already substantively satisfied by Phases 1-3.** The plan correctly notes this in `key_links`, but the `requirements` metadata at the top could mislead a status tracker into thinking REM-02/REM-03 are newly completed here. The plan is doing cleanup, not implementation.
- **LOW - Task 1 verify uses `grep -c ... | grep -q "^0$"`.** This pipe chain is fragile. Simpler: `! grep -q "lattice-plugin\|lattice-orchestrator\|lattice-plugins" crates/lattice-cli/Cargo.toml`.

### Suggestions

- Fix the `\n---\n---` artifact by removing the stray `\n---` between the frontmatter closing `---` and the `<objective>` tag.
- Change the `requirements` metadata to `["REM-02-cleanup", "REM-03-cleanup"]` or add a clarifying comment that substantive compliance was achieved in prior phases.
- Simplify the grep verification: `! grep -q "lattice-plugin\|lattice-orchestrator\|lattice-plugins" crates/lattice-cli/Cargo.toml && echo "CLI Cargo.toml clean"`.

---

## 04-03-PLAN.md - Guard Test + Wiki Deletion

### Strengths

- **Explicit preservation of `profile_runtime_eval.rs`.** The plan mentions this in three places (read_first, action CRITICAL note, acceptance criteria). Pitfall 4 from RESEARCH.md is well-mitigated.
- **Depends only on 04-01, not 04-02.** Correct - the guard tests read active source strings (not Cargo.toml), so 04-02's CLI Cargo.toml cleanup is irrelevant to whether the guard tests can run. The two Wave 2 plans are genuinely parallelizable.
- **Task 3 runs `cargo test --workspace` as the final gate.** Good - this catches any unexpected test breakage across the entire workspace, not just the crates being modified.
- **Threat model correctly identifies `profile_runtime_eval.rs` as the asset to protect.** T-04-08 mitigation includes reading the file first to confirm identity before deleting its sibling files.

### Concerns

- **MEDIUM - Same `\n---\n---` formatting artifact as 04-02.** See above.
- **LOW - The plan deletes guard tests that currently pass, but doesn't verify they pass before deletion.** If a guard test was already broken (e.g., someone introduced a forbidden string), deleting the test would hide the regression. A pre-deletion `cargo test -p lattice-runtime` and `cargo test -p lattice-cli` would confirm green before removal.
- **LOW - Wiki deletion has no pre-flight check.** The plan says "None required" for read_first in Task 2. While the research thoroughly cataloged the wiki contents, a quick `ls wiki/ | head -5` or `find wiki/ -name "*.md" | wc -l` in the read_first would confirm the directory is what we expect before `rm -rf`.
- **LOW - The success criteria and verification checklists overlap but aren't identical.** The verification checklist has 6 items; success criteria has 6 items but they're phrased differently. This is minor but could cause confusion about what "done" means. For example, verification includes `test -f profile_runtime_eval.rs` but success criteria doesn't mention this preservation.

### Suggestions

- Add a pre-deletion verification step in Task 1: run `cargo test -p lattice-runtime` and `cargo test -p lattice-cli` to confirm guard tests pass before deletion.
- Add a lightweight read_first to Task 2 (wiki deletion): `find wiki/ -name "*.md" | wc -l` to confirm about 66 files before deleting.
- Align the success criteria with the verification checklist - either deduplicate or explicitly note that verification is the gate and success criteria describe the end state.

---

## Cross-Plan Observations

| Area | Finding | Severity |
|------|---------|----------|
| Commit granularity | Plans don't specify whether tasks share commits. RESEARCH.md recommends atomic batching for 04-01 Tasks 1+2; this should be explicit in the plan. | MEDIUM |
| Cargo.lock awareness | Only 04-01's success criteria mention stale lockfile entries. An executor might run `cargo update` unnecessarily. | LOW |
| Phase 5 boundary | ROADMAP success criterion #4 (grep checks) is deferred to Phase 5 per CONTEXT.md, but the plans' `requirements` metadata doesn't flag this deferral. A status tracker might mark REM-04 incomplete waiting for grep results. | LOW |
| No backout instructions | None of the plans describe recovery if a task fails. Since this is `git`-tracked, `git checkout -- .` suffices, but noting this would help an autonomous executor. | LOW |
| Parallelism | 04-02 and 04-03 are correctly marked as independent (both Wave 2, both only depend on 04-01). No artificial sequencing. | good |

---

## Risk Assessment: LOW

This is a pure deletion phase with no new code, no external dependencies, and no runtime state changes. Phase 3 already removed all `use` statements from active source, so the compiler is the authoritative guard. The plans are mechanically straightforward - delete directories, remove lines from Cargo.toml, remove array entries from test code, delete files. The exact string matching for edits is precise given the files were read during plan creation. The main risks are formatting drift in Cargo.toml or forbidden arrays, commit granularity around 04-01 Tasks 1+2, and the `\n---\n---` artifact in 04-02 and 04-03.

The plans achieve all four phase requirements and correctly defer documentation/map updates to Phase 5. No blockers identified.

---

## OpenCode Review

# Cross-AI Plan Review: Phase 4 - Workspace Layer Removal

## Summary

Three well-researched, precisely-scoped execution plans for Phase 4 of the LATTICE v1.0 single-agent core refactor. The phase deletes three obsolete extension/orchestration crate directories (92 files), cleans up Cargo.toml dependency declarations at workspace and CLI levels (9 total lines removed), removes four forbidden-field string checks from profile test guards, deletes two transitional guard test files (317 total lines), and removes the obsolete wiki directory (66 files). The plans are grounded in exhaustive research (every file count, line number, and grep result verified), respect all eight user decisions from CONTEXT.md, and follow the established deletion-over-shim pattern from prior phases.

Overall risk: LOW - Pure deletion operations on dead code paths with compiler-enforced structural guards. No new code, no logic changes, no API surface modifications. The only material risk is accidental deletion of an active file, which all three plans mitigate with exact-path operations and cargo-test verification.

---

## Plan 04-01: Delete Crate Directories + Clean Root Cargo.toml

Wave: Wave 1. Dependencies: None. Requirements: REM-01.

### Strengths

- **Clear task decomposition:** Three tasks (delete dirs, edit Cargo.toml, verify) with explicit separation of concerns. Task 1 acknowledges the expected transitional build failure and instructs the executor not to run `cargo check` mid-task.
- **Commit ordering is acceptable within one plan:** The plan batches directory deletion and Cargo.toml edits into separate tasks within one plan, following the recommendation to either batch them or accept transitional failure.
- **Threat model covers the real risks:** T-04-01 (wrong directories deleted) is mitigated by exact absolute paths and `test -d` verification. T-04-03 (workspace will not build) is mitigated by the Task 3 cargo check gate.
- **Verification is compiler-enforced:** After Task 3, `cargo check --workspace` must pass. This is the correct structural guard.
- **Success criteria are precise and falsifiable:** Three directories gone, zero references in Cargo.toml, `cargo check --workspace` succeeds.

### Concerns

| # | Severity | Issue |
|---|----------|-------|
| C1 | LOW | No explicit stale-artifact cleanup step. After deleting three crate directories, `target/` contains stale compiled artifacts and `Cargo.lock` contains stale entries for all three crates. These are harmless but should be documented. |
| C2 | LOW | No fallback if Task 3 `cargo check` fails unexpectedly. The plan correctly says to investigate and not proceed, but could add a likely grep command for remaining imports. |

### Suggestions

- Add a verification observation note about `Cargo.lock` stale entries after Task 3.
- Add a grep fallback command to Task 3's failure investigation section, such as `grep -r 'use lattice_plugin\|use lattice_orchestrator\|use lattice_plugins' crates/`.

---

## Plan 04-02: Clean CLI Dependencies + Profile Forbidden Strings

Wave: Wave 2, depends on 04-01. Requirements: REM-02, REM-03.

### Strengths

- **Respects D-03 precisely:** Removes only `"lattice_orchestrator"` and `"lattice_plugin"` from both forbidden arrays.
- **Respects D-04:** Explicit statement that no replacement guard tests or grep-based CI checks are added.
- **Respects D-06:** CLI Cargo.toml cleanup is correctly described as dead declarations only.
- **Task 2 has correct verification:** `cargo test -p lattice-profile` verifies the remaining forbidden entries still enforce their guards.
- **The note about REM-02/REM-03 being already substantively satisfied** is accurate and frames this plan as formal cleanup.

### Concerns

| # | Severity | Issue |
|---|----------|-------|
| C3 | LOW | Profile test dependency on forbidden array ordering is implicit, not verified. The remaining guards could be vacuously true if no active source violates them, which is acceptable for a regression test but worth knowing. |
| C4 | LOW | `Cargo.lock` entries for the three removed workspace dependencies will remain. Harmless, but not mentioned. |

### Suggestions

- Add a one-line note after Task 1: `cargo check -p lattice-cli` verifies that no remaining code, including dev-dependencies, depends on the removed crates.

---

## Plan 04-03: Delete Guard Tests and Wiki

Wave: Wave 2, depends on 04-01. Requirements: REM-04.

### Strengths

- **Explicitly preserves `profile_runtime_eval.rs`:** The research pitfall is carried into the plan and read_first includes the file.
- **Correctly identifies that no `[[test]]` entries exist:** Cargo auto-discovery handles test detection, so simple deletion is enough.
- **D-05 implemented correctly:** Wiki deleted entirely with no historical archive or selective preservation.
- **D-01/D-02 respected:** Guard files are deleted because the compiler is now the structural guard.
- **D-04 respected:** No replacement guard tests, grep CI checks, or new test files added.
- **Task 3 has proper pre-existing-failure handling:** Unrelated test failures should be documented in SUMMARY.md and Phase 5 owns final verification.

### Concerns

| # | Severity | Issue |
|---|----------|-------|
| C5 | MEDIUM | The `depends_on: ["04-01"]` metadata is technically unnecessary for guard test deletion. The sequencing remains coherent and is not a blocker. |
| C6 | LOW | Wiki deletion may leave stale references in README, AGENTS.md, CLAUDE.md, or other docs. This is by design and deferred to Phase 5, but should be noted. |

### Suggestions

- Add a `grep -r "wiki/" . --include="*.md"` check at the end of Task 3 to enumerate stale wiki cross-references and list them in SUMMARY.md as Phase 5 cleanup.

---

## Cross-Cutting Concerns

| # | Severity | Issue | Affected Plans |
|---|----------|-------|----------------|
| CC1 | LOW | No plan explicitly addresses `Cargo.lock` state after deletion. Research notes stale entries are harmless and auto-regenerated, but documenting this prevents confusion. | 04-01, 04-02, 04-03 |
| CC2 | LOW | No `cargo clippy` or `cargo fmt --all --check` in verification steps. Deletions should not introduce clippy or formatting violations, so this is optional. | 04-01, 04-03 |
| CC3 | LOW | Phase 5 documentation boundary is clean but worth noting. README, AGENTS.md, CLAUDE.md, and `.planning/codebase/` maps may still reference removed crates after Phase 4. | All |

---

## Dependency Ordering Analysis

```text
04-01 (Wave 1): Delete crate dirs + root Cargo.toml
    |
    +--> 04-02 (Wave 2): CLI deps + profile forbidden strings
    |
    +--> 04-03 (Wave 2): Guard tests + wiki deletion
```

The dependency graph is correct for logical sequencing. All three plans could technically execute in any order because they operate on mostly disjoint files, but the current sequencing (delete crates, clean up references, clean up guards) is the most reviewable order. No deadlock or blocking issue exists.

---

## Risk Assessment

Overall risk: LOW.

1. No new code is written.
2. Compiler-enforced verification is stronger than the deleted guards.
3. Every operation is exact-path or exact-string.
4. User decisions D-01 through D-08 are respected.
5. Research-backed line numbers and file counts were verified by the reviewer.
6. Pre-existing failure handling is addressed.

Residual risks are low: slightly imprecise dependency metadata for 04-03, stale `Cargo.lock` entries, and stale wiki cross-references that Phase 5 should catch.

### Summary Table

| Criterion | 04-01 | 04-02 | 04-03 |
|-----------|-------|-------|-------|
| Requirements coverage | REM-01 | REM-02, REM-03 | REM-04 |
| Verification strength | HIGH - compiler gate | HIGH - cargo test | HIGH - cargo test workspace |
| Risk level | LOW | LOW | LOW |
| Blocking concerns | None | None | None |

Recommendation: APPROVED for execution. All three plans are ready for implementation. The suggestions are non-blocking improvements.

---

## Attempted Reviewer Failures

### Cursor

Cursor Agent was available at `/usr/bin/cursor-agent`, but headless review failed because the CLI is not authenticated and `CURSOR_API_KEY` is not set.

```text
Error: Authentication required. Please run 'agent login' first, or set CURSOR_API_KEY environment variable.
```

### Skipped Reviewers

- `codex`: skipped for independence because this session is running in Codex. A previous Phase 4 attempt also failed with HTTP 401 from the configured custom provider.
- `gemini`: CLI not found.
- `qwen`: CLI not found.
- local OpenAI-compatible reviewers (`ollama`, `lm_studio`, `llama_cpp`): no local model server responded on the configured/default endpoints.

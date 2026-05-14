# Project Retrospective

*A living document updated after each milestone. Lessons feed forward into future planning.*

## Milestone: v1.2 -- CC/Codex Capability Foundation & Code Agent

**Shipped:** 2026-05-12
**Phases:** 5 | **Plans:** 23 | **Tasks:** 38

### What Was Built

- Capability bundles and profile presets make code-agent powers opt-in instead of global defaults.
- Structured patch editing supports multi-file edits with content hashes, sandbox checks, rollback, diagnostics, and changed-file manifests.
- Permission and approval flow now produces typed denial reports and scoped approval proposals through shared runtime security state.
- Agent task/recovery state persists todos, changed files, verification records, failures, and unverified work across turns and manifests.
- `Runtime::build_code_agent` became the shared code-agent factory used by runtime callers and `lattice code`.
- Deterministic offline evals now exercise the flagship code-agent loop and profile isolation.

### What Worked

- The crate-layered phase order paid off: capabilities, editing, permissions, state, and code-agent assembly stacked cleanly.
- Deterministic Rust evals gave the code-agent work a real product-level safety net instead of relying on prompt inspection.
- Gap-closure plans 14-06 and 14-07 were small, targeted, and easy to verify after UAT surfaced edge cases.

### What Was Inefficient

- Milestone audit state lagged behind later verification artifacts, so closeout required separating stale audit findings from current evidence.
- Some artifact scanners report resolved UAT files as open when there are zero pending scenarios.
- Phase 11 has UAT/security/validation evidence but no formal `11-VERIFICATION.md`, which makes audit language noisier than the implementation state.

### Patterns Established

- Capabilities are selected explicitly at profile/runtime boundaries.
- Tool results should carry typed state transitions rather than forcing the model to parse arbitrary text.
- Final reports should derive truth from task state and verification records, not from model prose.
- Code-agent behavior should be guarded by deterministic product-loop evals.

### Key Lessons

1. Audit artifacts need refresh or triage after late gap closures; otherwise closeout reads stale.
2. Provider precedence rules need full matrix tests when two independent priority axes compose.
3. Approval UX needs idempotent state and stale-blocker cleanup, not just a successful approval path.

### Cost Observations

- Model mix: quality profile for planning/execution; local Cargo gates for final proof.
- Sessions: multi-session milestone with explicit GSD phase artifacts.
- Notable: The biggest closeout cost was artifact consistency, not code changes.

---

## Cross-Milestone Trends

### Process Evolution

| Milestone | Phases | Key Change |
|-----------|--------|------------|
| v1.0 | 5 | Cleaned architecture and dependency direction before adding larger features. |
| v1.1 | 4 | Moved prompt lifecycle and manifest capture into typed runtime structures. |
| v1.2 | 5 | Turned code-agent powers into reusable capabilities and proved them with evals. |

### Cumulative Quality

| Milestone | Quality Signal | Notes |
|-----------|----------------|-------|
| v1.0 | Workspace CI clean | Established crate DAG discipline. |
| v1.1 | Prompt/manifest verification | Added structured turn evidence. |
| v1.2 | Code-agent eval harness | Added deterministic product-loop coverage. |

### Top Lessons

1. Keep capability boundaries explicit; global defaults hide product risk.
2. Convert model-visible behavior into typed state and deterministic tests wherever possible.
3. Treat planning artifacts as living state: after gap closure, update the audit-facing files too.

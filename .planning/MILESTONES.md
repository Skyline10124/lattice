# Milestones

## v1.2 CC/Codex Capability Foundation & Code Agent (Shipped: 2026-05-12)

**Delivered:** Capability-isolated agent assembly, structured editing, scoped approvals, durable recovery state, and a runtime-built `lattice code` agent with deterministic eval coverage.

**Phases completed:** 10-14 (5 phases, 23 plans, 38 tasks)

**Key accomplishments:**

- Capability bundles and code-agent/reader presets now gate tool exposure explicitly instead of leaking global defaults.
- Structured patch editing now supports multi-file add/update/delete operations with blake3 content hashes, sandbox preflight, rollback, diagnostics, and changed-file manifests.
- Permission and approval flow now returns typed denial reports, scoped approval proposals, and shared runtime security state.
- Agent task/recovery state now persists todos, changed files, tool failures, verification records, and unverified work across turns and session manifests.
- `Runtime::build_code_agent` is now the authoritative code-agent factory, shared by runtime callers and `lattice code`.
- Deterministic code-agent evals cover dirty worktree safety, project instructions, targeted reading, patch recovery, verification honesty, profile isolation, and gap-analysis linkage.

**Known close notes:**

- Historical Phase 02 UAT/verification artifacts still appear in `audit-open`; they predate v1.2 and remain acknowledged planning debt.
- Phase 12 verification remains `human_needed` for manual UX confirmation, but PERM-01..04 are satisfied and Phase 14 wires the user-facing `/approve` path.
- Phase 11 and Phase 14 have UAT/security/validation/review evidence; the stale milestone audit file was archived with its original `gaps_found` result for traceability.

**Archive:** `.planning/milestones/v1.2-ROADMAP.md`
**Requirements:** `.planning/milestones/v1.2-REQUIREMENTS.md`
**Milestone audit:** `.planning/milestones/v1.2-MILESTONE-AUDIT.md`

---

## v1.1 Prompt Lifecycle & Session Runtime

**Shipped:** 2026-05-10
**Phases:** 4 | **Plans:** 8 | **Tasks:** 14

### What Was Built

- Agent loop migrated from legacy prompt sections to the typed contribution path.
- PromptSpec profile loading wired through ProviderFactory and built-in providers.
- Declarative prompt hook builder implemented with add/remove/budget/abort actions and hash tracking.
- Turn manifest foundation added: per-turn ManifestCollector, hook/tool decision capture, PromptSnapshot, and LoopEvent::Manifest emission before Done.

### Key Decisions

- v1.1 followed crate dependency order: agent prompt refactor first, then profile/spec loading, hook assembly, and manifest collection.
- Prompt hooks stay in `lattice-hooks`; the agent consumes the hook chain instead of owning hook definitions.
- Manifest P1 stores hashes, metadata, and previews, not full prompt content.
- Historical audit gaps were acknowledged at close and carried forward as deferred context in STATE.md.

### Known Gaps

- Historical Phase 02 UAT artifact was reported by audit-open despite zero pending scenarios.
- Historical Phase 02 and Phase 06 verification artifacts still report `gaps_found`; accepted as legacy planning debt, not v1.1 blockers.

### Stats

- 8 plan summaries completed.
- Phase 9 verification passed 12/12 must-have truths.
- `cargo test -p lattice-agent`, `cargo clippy -p lattice-agent -- -D warnings`, and `cargo fmt -p lattice-agent --check` were recorded as passing in Phase 9 verification.

**Archive:** .planning/milestones/v1.1-ROADMAP.md
**Requirements:** .planning/milestones/v1.1-REQUIREMENTS.md
**Phase archive:** .planning/milestones/v1.1-phases/

---

## v1.0 — Tech Debt Cleanup & Architecture Refactoring

**Shipped:** 2026-05-08
**Phases:** 5 | **Plans:** 9 | **Tasks:** 18

### What Was Built

- CI passes clean with zero clippy warnings, 9762 lines of dead code removed
- `lattice-sandbox` crate extracted, breaking agent/tools circular dependency
- `lattice-core` decoupled as true leaf crate with zero internal workspace deps
- 845-line duplicate DefaultToolExecutor removed from agent
- `lattice-bus` renamed to `lattice-orchestrator`, registry merged, final 6-layer DAG achieved

### Key Decisions

- Bottom-up ordering: clean CI first, then extract, then decouple
- Registry traits merged into orchestrator as stubs (design intent preserved)
- lattice-hooks kept as standalone crate (not merged into agent)
- ARCH-04 (ToolExecutor) assigned to Phase 4, not Phase 3

### Known Gaps

- 13 BusEventCollector references remain in lattice-agent (internal type, minor)
- 3 comment references to old bus_config naming in orchestrator source
- 1 clippy warning in lattice-prompt (unrelated to milestone)

### Stats

- 829 tests passing
- 0 clippy warnings (workspace)
- Target DAG verified via cargo tree

**Archive:** .planning/milestones/v1.0-ROADMAP.md
**Requirements:** .planning/milestones/v1.0-REQUIREMENTS.md

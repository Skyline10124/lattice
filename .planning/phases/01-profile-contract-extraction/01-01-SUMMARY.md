---
phase: "01-profile-contract-extraction"
plan: "01"
tasks_completed: 3
created: "2026-05-14T11:10:00Z"
has_checkpoints: false
key-files:
  created:
    - crates/lattice-profile/Cargo.toml
    - crates/lattice-profile/src/lib.rs
    - crates/lattice-profile/src/schema.rs
    - crates/lattice-profile/src/error.rs
    - crates/lattice-profile/src/load.rs
  modified:
    - Cargo.toml
    - Cargo.lock
---

## Plan 01-01 Summary: lattice-profile crate skeleton, strict schema, and loader

### What was built

Created the `lattice-profile` workspace crate — a narrow, dependency-minimal crate
that owns the v1 `agent.toml` profile contract. The crate has zero dependencies on
`lattice-runtime`, `lattice-orchestrator`, `lattice-agent`, `lattice-session`,
`lattice-plugin`, or CLI/TUI modules.

**Schema (schema.rs):** Strict allowlist-only TOML schema with `#[serde(deny_unknown_fields)]`
on all structs. Only `schema_version = 1` is accepted. Allowed top-level sections:
`schema_version`, `[agent]`, `[prompt]`, `[tools]`, `[security]`, `[context]`,
`[behavior]`. Forbidden sections (`[capabilities]`, `[handoff]`, `[orchestrator]`,
`[plugins]`, `[pipeline]`, `[subagents]`, `[micro_agents]`, `[memory]`) and
forbidden fields (`agent.preset`, `agent.type`, `agent.skippable`, `agent.tags`,
`behavior.behavior_type`, `behavior.confidence_threshold`, `behavior.max_retries`,
`behavior.enforce_sandbox`) are rejected via serde's `deny_unknown_fields`.

**Error model (error.rs):** Typed `ProfileError` variants carrying path, section,
field, rejected value, reason, and expected schema guidance. Includes variants:
`Io`, `Toml`, `UnsupportedSchemaVersion`, `ForbiddenSection`, `ForbiddenField`,
`InvalidField`, `PromptMutuallyExclusive`, `MissingPrompt`, `PathEscape`,
`AbsoluteSystemPath`, `SystemPathTraversal`, `SystemPathNotAFile`,
`SystemPathNotFound`, `MissingDefault`.

**Loader (load.rs):** `load_agent_profile()` reads `agent.toml`, pre-scans for
forbidden sections via `toml::Value` walk, deserializes with strict serde, validates
`schema_version = 1`, resolves `system`/`system_path` mutual exclusivity, and
validates package-local path containment (rejects absolute paths, `..` traversal,
and canonical escapes). The loader is read-only — no side files created.

### Task Completion

| Task | Status | Commits |
|------|--------|---------|
| Task 1: Add lattice-profile crate and module surface | Complete | feat(01-01): add lattice-profile crate skeleton |
| Task 2: Implement strict schema and actionable errors | Complete | feat(01-01): implement strict v1 schema and actionable error model |
| Task 3: Implement read-only loader and path checks | Complete | (included in Task 2 commit — loader + schema implemented together) |

### Tests

- **33 tests pass** in `lattice-profile` (schema parsing + loader validation)
- All forbidden sections/fields covered by rejection tests
- Source guard test verifies no forbidden symbols in production code

### Deviations

Tasks 2 and 3 were implemented together in one commit because the loader depends
on the schema types and error model — separating them would have required stubs.

### Self-Check: PASSED

- `cargo test -p lattice-profile` — 33 passed
- `cargo fmt --all --check` — passes
- No forbidden crate dependencies in `lattice-profile`
- No forbidden symbols in production source code

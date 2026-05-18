<!-- refreshed: 2026-05-17 -->
# Codebase Concerns

**Analysis Date:** 2026-05-17 (post-Phase 11 deletion)

## Resolved (removed crates, commands, and features)

The following were removed during the v1.1 "Cut Off Cleverness" milestone:
- Phase 6 removed: orchestrator, plugin, and plugins crates.
- Phase 11 removed: core, runtime, prompt, sandbox, and policy crates.
- Phase 11 removed: terminal UI frontend (unsupported in v1.1).
- Phase 11 removed: unsupported CLI commands (see boundary test for the complete list).
- Phase 11 removed: API migration-home table (topology.rs).
- Phase 11 collapsed: legacy Agent path into AgentLoop with injectable traits.
- Phase 11 resolved: Landlock duplication (sandbox crate removed).

---

## Active Concerns

### Low Severity

**Profile system retains explicit validation for removed concepts.**
- Location: `crates/lattice-profile/src/load.rs`, `crates/lattice-profile/src/schema.rs`, `crates/lattice-profile/src/defaults.rs`
- Description: The `FORBIDDEN_SECTIONS` constant explicitly rejects removed concepts at the TOML section level. This is deliberate migration-guard validation.
- Severity: LOW -- not a bug, but the list should be reviewed when the v1.1 stabilization period ends.

**No architecture boundary enforcement in CI.**
- Location: Dependency flow declared in `AGENTS.md` and `CLAUDE.md`.
- Description: The declared dependency hierarchy `hooks -> llm -> session/profile/tools -> agent -> api -> cli` has no automated CI enforcement. New dependencies could violate the layering without a test catching it.
- Severity: LOW -- the current 8-crate graph is clean and small enough to review manually, but should get automated enforcement before the crate count grows again.
- Remediation: Add a CI step using `cargo metadata` to verify the dependency graph against the declared ordering.

---

## Watch Items

**Crate dependency graph cleanliness:**
- Current graph is unidirectional: `hooks -> llm -> session/profile/tools -> agent -> api -> cli`. No cycles detected.
- Watch for any future import that reverses the declared direction.

**LOC budget:**
- Current total: 19,922 strict lines under `crates/*/src/**/*.rs` (budget: 20,000).
- Watch for source growth that exceeds the budget.

**Test coverage distribution across crates:**
- `lattice-hooks`: 404 lines, small crate. Monitor for proportional coverage.
- `lattice-api`: 2,030 lines. Monitor for facade-level integration tests.

---

## Security Audit Notes

**Tool safety is simple in v1.1:**
- Project-root containment, path traversal rejection, file-size limits, profile-gated optional shell/web tools.
- The removed sandbox and approval modules are not part of the current release.
- Future milestones should design a new security model on the simplified substrate.

**Credential handling:**
- `CredentialStore` warns on literal API keys but does not reject them.
- Debug formatting redacts key values. Keys are zeroized on Drop.
- CLI smoke tests explicitly verify no secret leakage in output.

---

*Concerns audit: 2026-05-17 (post-Phase 11 deletion)*

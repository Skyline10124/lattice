---
phase: 01
slug: profile-contract-extraction
status: draft
nyquist_compliant: true
wave_0_complete: false
created: 2026-05-14
---

# Phase 01 — Validation Strategy

> Per-phase validation contract for feedback sampling during execution.

---

## Test Infrastructure

| Property | Value |
|----------|-------|
| **Framework** | Rust `cargo test` |
| **Config file** | `Cargo.toml` workspace + crate manifests |
| **Quick run command** | `cargo test -p lattice-profile` |
| **Full suite command** | `cargo test -p lattice-profile && cargo test -p lattice-tools capabilities && cargo check --workspace` |
| **Estimated runtime** | ~60-180 seconds |

---

## Sampling Rate

- **After every task commit:** Run the plan-specific targeted command, preferring `cargo test -p lattice-profile <test_filter>` or `cargo test -p lattice-tools capabilities`.
- **After every plan wave:** Run `cargo test -p lattice-profile` after Wave 1, then `cargo test -p lattice-profile && cargo test -p lattice-tools capabilities` after Wave 2, then `cargo check --workspace` after Wave 3 if bridge code is touched.
- **Before `$gsd-verify-work`:** Run `cargo fmt --all --check`, `cargo test -p lattice-profile`, `cargo test -p lattice-tools capabilities`, and `cargo check --workspace` or document any pre-existing unrelated failures.
- **Max feedback latency:** 180 seconds for targeted checks.

---

## Per-Task Verification Map

| Task ID | Plan | Wave | Requirement | Threat Ref | Secure Behavior | Test Type | Automated Command | File Exists | Status |
|---------|------|------|-------------|------------|-----------------|-----------|-------------------|-------------|--------|
| 01-01-01 | 01 | 1 | PROF-01 | T-01 path escape | Strict schema and prompt paths reject legacy/escape inputs | unit | `cargo test -p lattice-profile schema` | ❌ W0 | ⬜ pending |
| 01-01-02 | 01 | 1 | PROF-01 | T-01 path escape | Loader errors include path/section/field/value guidance | unit | `cargo test -p lattice-profile load` | ❌ W0 | ⬜ pending |
| 01-02-01 | 02 | 1 | PROF-01 | T-02 unwanted write | Discovery never writes default files without explicit caller action | unit | `cargo test -p lattice-profile discovery` | ❌ W0 | ⬜ pending |
| 01-02-02 | 02 | 1 | PROF-02 | T-03 live assembly leak | Default template and lowering remain pure data | unit | `cargo test -p lattice-profile defaults lowering` | ❌ W0 | ⬜ pending |
| 01-03-01 | 03 | 2 | PROF-03 | T-04 preset bypass | `ToolPreset` and `CapabilitySelection.presets` are absent from active tool capability API | unit | `cargo test -p lattice-tools capabilities` | ✅ | ⬜ pending |
| 01-04-01 | 04 | 3 | PROF-01 PROF-02 PROF-03 | T-03 live assembly leak | Workspace compiles with profile crate and transitional bridge surfaces | build | `cargo check --workspace` | ✅ | ⬜ pending |

*Status: ⬜ pending · ✅ green · ❌ red · ⚠️ flaky*

---

## Wave 0 Requirements

- [ ] `crates/lattice-profile/Cargo.toml` — new crate manifest with narrow dependencies.
- [ ] `crates/lattice-profile/src/lib.rs` — public exports for schema/load/discovery/defaults/spec/lowering/errors.
- [ ] `crates/lattice-profile/src/schema.rs` or equivalent — strict v1 TOML schema tests.
- [ ] `crates/lattice-profile/src/load.rs` or equivalent — prompt `system_path` package-boundary tests.
- [ ] `crates/lattice-profile/src/discovery.rs` or equivalent — explicit/name/default lookup tests.
- [ ] `crates/lattice-profile/src/lowering.rs` or equivalent — pure profile-to-spec tests.

---

## Manual-Only Verifications

| Behavior | Requirement | Why Manual | Test Instructions |
|----------|-------------|------------|-------------------|
| Interactive default-agent prompt text | PROF-01 | Phase 1 can expose a `MissingDefault`/template contract without wiring final CLI prompt UX until CLI phase | Review generated default template text and verify it tells users to edit role, prompt, tools, and security settings. |

---

## Threat Model

| Threat ID | Threat | Severity | Mitigation |
|-----------|--------|----------|------------|
| T-01 | `prompt.system_path` escapes the agent package and reads arbitrary filesystem content. | high | Canonicalize package dir and target file; require target starts with canonical package dir; reject absolute paths and `..`; test symlink/escape cases where feasible. |
| T-02 | Missing default agent silently writes files into a project/global location without user consent. | medium | `lattice-profile` returns a missing-default/template action; CLI performs interactive confirmation later; non-interactive callers fail clearly. |
| T-03 | Profile lowering constructs live agents, reads sessions, or reaches CLI/TUI state. | high | `lattice-profile` dependency boundary forbids runtime/agent/session/CLI/orchestrator/plugin deps; pure lowering tests and grep guards enforce no live assembly imports. |
| T-04 | Legacy presets or `[capabilities]` bypass the new `[tools]` vocabulary and reintroduce coding-agent semantics. | medium | Strict schema rejects forbidden sections/fields; `lattice-tools` removes preset API; tests cover `coding`, `code-agent`, `reader`, `minimal`, and `[capabilities]` rejection. |

---

## Validation Sign-Off

- [x] All tasks have automated verify commands or Wave 0 dependencies.
- [x] Sampling continuity: no 3 consecutive tasks without automated verify.
- [x] Wave 0 covers all missing references.
- [x] No watch-mode flags.
- [x] Feedback latency target < 180s for targeted checks.
- [x] `nyquist_compliant: true` set in frontmatter.

**Approval:** approved 2026-05-14

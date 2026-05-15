---
phase: 02
slug: runtime-assembly-cutover
status: draft
nyquist_compliant: true
wave_0_complete: false
created: 2026-05-14
---

# Phase 02 - Validation Strategy

> Per-phase validation contract for feedback sampling during execution.

---

## Test Infrastructure

| Property | Value |
|----------|-------|
| **Framework** | Rust built-in `cargo test` with inline crate tests and crate integration tests |
| **Config file** | Root `Cargo.toml` workspace settings and crate `Cargo.toml` files |
| **Quick run command** | `cargo test -p lattice-session && cargo test -p lattice-profile && cargo test -p lattice-runtime` |
| **Full suite command** | `cargo check --workspace && cargo fmt --all --check` |
| **Estimated runtime** | ~180 seconds for targeted tests; workspace check may vary |

---

## Sampling Rate

- **After every task commit:** Run the narrow crate test for the touched crate.
- **After every plan wave:** Run `cargo test -p lattice-session && cargo test -p lattice-profile && cargo test -p lattice-runtime`.
- **Before `$gsd-verify-work`:** Run `cargo check --workspace && cargo fmt --all --check`.
- **Optional closure evidence:** Run `cargo test --workspace` and `cargo clippy --workspace -- -D warnings`; classify any failures as Phase 2 regressions, pre-existing failures, or obsolete legacy tests.
- **Max feedback latency:** 300 seconds for targeted test feedback.

---

## Per-Task Verification Map

| Task ID | Plan | Wave | Requirement | Threat Ref | Secure Behavior | Test Type | Automated Command | File Exists | Status |
|---------|------|------|-------------|------------|-----------------|-----------|-------------------|-------------|--------|
| 02-01-01 | 01 | 1 | RUN-03 | T-02-01 / T-02-02 / T-02-05 | Session binds an immutable profile snapshot, rejects profile hash/base commit mismatches, and records status-bearing manifests. | unit | `cargo test -p lattice-session profile_snapshot` | W0 | pending |
| 02-01-02 | 01 | 1 | RUN-03 | T-02-05 | Completed, Error, and Cancelled outcomes are representable when a visible turn boundary exists. | unit | `cargo test -p lattice-session commit_turn` | W0 | pending |
| 02-02-01 | 02 | 2 | RUN-01 | T-02-01 / T-02-03 | Runtime assembles transient execution from `GenericAgentSpec` and locked session snapshot, not from live handle state. | unit/integration | `cargo test -p lattice-runtime generic` | W0 | pending |
| 02-02-02 | 02 | 2 | RUN-02 | T-02-03 | Generic runtime path constructs `DefaultToolExecutor` with `RuntimeSecurity` sandbox and authorized tool names. | unit/integration | `cargo test -p lattice-runtime sandbox` | W0 | pending |
| 02-03-01 | 03 | 3 | RUN-03 | T-02-02 / T-02-05 | Runtime turn completion commits messages and manifests through the session transaction API. | integration | `cargo test -p lattice-runtime turn_completion` | W0 | pending |
| 02-03-02 | 03 | 3 | RUN-01 / RUN-03 | T-02-01 / T-02-05 | Runtime events and completion identity use session/turn identity for the generic path. | unit/integration | `cargo test -p lattice-runtime runtime_turn_event` | W0 | pending |
| 02-06-01 | 06 | 6 | RUN-04 | T-02-06 | Runtime no longer exposes extension-layer API names or dependency edges. | boundary grep + compile | `cargo check -p lattice-runtime` plus runtime boundary grep | W0 | pending |
| 02-04-02 | 04 | 4 | RUN-01 / RUN-04 | T-02-06 | CLI/TUI adapters compile only through the narrowed generic runtime surface; UX cleanup remains Phase 3. | integration/compile | `cargo check --workspace` | Existing | pending |

*Status values: pending, green, red, flaky.*

---

## Wave 0 Requirements

- [ ] `crates/lattice-session/src/lib.rs` or `crates/lattice-session/src/turn_tx.rs` has tests for profile snapshot hash stability, source drift reporting, locked snapshot reuse, profile hash mismatch rejection, base commit mismatch rejection, and manifest profile identity.
- [ ] `crates/lattice-runtime/src/assembly.rs` or runtime inline tests cover `GenericAgentSpec` to transient `Agent` assembly without `lattice_orchestrator::assembly`.
- [ ] Runtime sandbox binding tests prove an unenabled tool or blocked command is denied through the generic path.
- [ ] Runtime boundary guard checks removed extension concepts under `crates/lattice-runtime`.
- [ ] Targeted CLI/runtime adapter tests cover only compile-critical call sites after runtime public API removal.

---

## Manual-Only Verifications

| Behavior | Requirement | Why Manual | Test Instructions |
|----------|-------------|------------|-------------------|
| Profile drift warning wording | RUN-03 | Exact presentation may be debug/audit output rather than a stable public string in Phase 2. | Confirm the implementation records or surfaces the equivalent of `profile changed on disk; current session remains locked to snapshot <hash>` without switching snapshots. |
| Optional full workspace clippy/test classification | RUN-01/RUN-02/RUN-03/RUN-04 | Phase 2 closure gate allows targeted-plus-workspace-check minimum; full tests/clippy may expose pre-existing or obsolete legacy failures. | If `cargo test --workspace` or `cargo clippy --workspace -- -D warnings` fails, classify each failure before deciding whether it blocks Phase 2. |

---

## Threat Model

| Threat ID | STRIDE | Surface | Mitigation | Verification |
|-----------|--------|---------|------------|--------------|
| T-02-01 | Tampering | Profile file changes during active session | Bind an immutable profile snapshot to the session and reuse the locked snapshot by default. | `cargo test -p lattice-session profile_snapshot` |
| T-02-02 | Tampering | Concurrent turn commit against changed head | `commit_turn` validates `base_commit_id` before writing messages/manifests. | `cargo test -p lattice-session commit_turn` |
| T-02-03 | Elevation of privilege | Tool executor after orchestrator removal | Runtime constructs `DefaultToolExecutor` with `RuntimeSecurity` sandbox and authorized tool names from resolved capabilities. | `cargo test -p lattice-runtime sandbox` |
| T-02-04 | Information disclosure | Durable snapshot/manifest metadata | Persist safe profile summaries only; exclude API keys, credential maps, and raw environment values. | Snapshot/manifest tests plus code review |
| T-02-05 | Repudiation | Error or cancellation outcomes | Represent Completed, Error, and Cancelled as auditable turn outcomes when a visible/model-visible boundary exists. | `cargo test -p lattice-session commit_turn` and `cargo test -p lattice-runtime turn_completion` |
| T-02-06 | Elevation of privilege / confusion | Old runtime plugin/orchestrator API surface | Remove runtime public APIs and dependency edges for plugin registry, plugin overlay, pipeline, micro-agent, event bus, and runtime-backed orchestrator executor. | Runtime boundary grep plus `cargo check -p lattice-runtime` |

---

## Boundary Grep

Run after runtime API cleanup:

```sh
rg -n "lattice_orchestrator|lattice_plugin|Pipeline|MicroAgent|EventBus|PluginRegistry|RuntimePluginOverlay|RuntimeAgentTurnExecutor|load_plugin_dir|watch_plugin_dir" crates/lattice-runtime Cargo.toml
```

Expected result: no matches in active runtime source or runtime crate manifest. Historical `.planning` matches are allowed and should not be part of this command.

---

## Validation Sign-Off

- [x] All tasks have automated verification commands or Wave 0 dependencies.
- [x] Sampling continuity: no 3 consecutive tasks without automated verify.
- [x] Wave 0 covers all missing references from `02-RESEARCH.md`.
- [x] No watch-mode flags.
- [x] Feedback latency target documented.
- [x] `nyquist_compliant: true` set in frontmatter.

**Approval:** pending

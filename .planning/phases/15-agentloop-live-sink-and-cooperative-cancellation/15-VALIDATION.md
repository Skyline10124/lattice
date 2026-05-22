---
phase: 15
slug: agentloop-live-sink-and-cooperative-cancellation
status: draft
nyquist_compliant: false
wave_0_complete: false
created: 2026-05-22
---

# Phase 15 — Validation Strategy

> Per-phase validation contract for feedback sampling during execution.

---

## Test Infrastructure

| Property | Value |
|----------|-------|
| **Framework** | `cargo test` (Rust 2021, tokio 1.x) |
| **Config file** | `crates/lattice-agent/Cargo.toml` (`test-utils` feature already declared) |
| **Quick run command** | `cargo test -p lattice-agent --features test-utils` |
| **Full suite command** | `cargo test --workspace --all-features` |
| **Estimated runtime** | ~25 s (lattice-agent crate alone, scenarios all `#[tokio::test]`, no real I/O) |

Sampling-strategy notes (Nyquist):
- Behavioral bands B1–B4 defined in `15-RESEARCH.md` §2 are sampled at least twice each by the seven-scenario test set in `tests/live_sink_and_cancel.rs` (D-18).
- Test concurrency must follow the `tokio::join!(run, drain)` recipe in `15-RESEARCH.md` §5; sequential drain deadlocks the bounded channel.
- All scenarios use `#[tokio::test]` (single-threaded fine — the consumer and tools are sequential).

---

## Sampling Rate

- **After every task commit:** Run `cargo test -p lattice-agent --features test-utils`
- **After every plan wave:** Run `cargo test --workspace --all-features`
- **Before `/gsd:verify-work`:** Full suite must be green
- **Max feedback latency:** ~25 s (single crate) / ~3 min (workspace)

---

## Per-Task Verification Map

> Task IDs are placeholders until `gsd-planner` writes `15-XX-PLAN.md`. Update this table after PLAN.md is created and before any execution begins.

| Task ID | Plan | Wave | Requirement | Threat Ref | Secure Behavior | Test Type | Automated Command | File Exists | Status |
|---------|------|------|-------------|------------|-----------------|-----------|-------------------|-------------|--------|
| 15-01-01 | 01 | 1 | AGENT-08 | — | partial-flush + timeout helpers preserve existing behavior | unit (existing tests) | `cargo test -p lattice-agent --features test-utils` | ❌ W0 | ⬜ pending |
| 15-01-02 | 01 | 1 | AGENT-08 | — | `AgentTurnEvent` derives `PartialEq, Eq` (live-mirror equality) | compile guard | `cargo check -p lattice-agent` | ❌ W0 | ⬜ pending |
| 15-02-01 | 02 | 1 | AGENT-08, AGENT-09 | — | `emit_agent_event` mirrors buffered + live; `consume_model_stream` cancel-aware; `StreamOutcome::Cancelled` reaches partial-flush | unit (existing tests still green) | `cargo test -p lattice-agent --features test-utils` | ❌ W0 | ⬜ pending |
| 15-02-02 | 02 | 1 | AGENT-08, AGENT-09 | — | 5 new cancel checkpoints land (1b, 2, 3, 4, 5); `run_turn_live` public method exists | compile guard + existing tests | `cargo build -p lattice-agent` | ❌ W0 | ⬜ pending |
| 15-03-01 | 03 | 2 | VER-11 | — | scenarios #1 (cancel before tool) + #2 (cancel during tool) green | behavioral test | `cargo test -p lattice-agent --features test-utils --test live_sink_and_cancel` | ❌ W0 | ⬜ pending |
| 15-03-02 | 03 | 2 | VER-11, AGENT-08 | — | scenarios #3 (live mirror equality) + #4 (receiver drop tolerance) green | behavioral test | `cargo test -p lattice-agent --features test-utils --test live_sink_and_cancel` | ❌ W0 | ⬜ pending |
| 15-03-03 | 03 | 2 | VER-11, AGENT-09 | — | scenarios #5 (mid-stream cancel partial preservation) + #6 (multi-tool batch cancel) green | behavioral test | `cargo test -p lattice-agent --features test-utils --test live_sink_and_cancel` | ❌ W0 | ⬜ pending |
| 15-03-04 | 03 | 2 | AGENT-08 | — | scenario #7 (`run_turn` buffered regression) green | behavioral test | `cargo test -p lattice-agent --features test-utils --test live_sink_and_cancel` | ❌ W0 | ⬜ pending |
| 15-04-01 | 04 | 3 | AGENT-08, AGENT-09, VER-11 | — | workspace lint, fmt, full test pass | gate | `cargo fmt --all --check && cargo clippy --workspace -- -D warnings && cargo test --workspace --all-features` | ✅ | ⬜ pending |

*Status: ⬜ pending · ✅ green · ❌ red · ⚠️ flaky*

> **Sampling continuity check:** No 3 consecutive tasks without an automated verify command. Each row above lists a runnable `cargo test` (or `cargo check`/`cargo build`) — passes the rule.

---

## Wave 0 Requirements

- [ ] `crates/lattice-agent/tests/live_sink_and_cancel.rs` — new file, gated `#![cfg(feature = "test-utils")]`. Houses all 7 scenarios from `15-CONTEXT.md` D-18.
- [ ] `crates/lattice-agent/Cargo.toml` — verify `tokio = { workspace = true }` exposes the `sync` feature (it does via the workspace default; check `[workspace.dependencies]` if unsure). No new dev-dep is mandatory; if scenario #5 picks the `ReceiverStream` route, add `tokio-stream` to `[dev-dependencies]`. Prefer the no-new-dep path (`futures::stream::unfold` already available).
- [ ] No new shared fixtures file required — each scenario's helpers live inline in `live_sink_and_cancel.rs` per D-19.

*Existing infrastructure covers everything else: `FakeStreamingLlmClient` (testing.rs), `FakeToolExecutor` (tests/streaming_delta.rs), `CancelAwareLlm` pattern (tests/agent_loop.rs:570).*

---

## Manual-Only Verifications

*All phase behaviors have automated verification.*

The seven-scenario behavioral suite (D-18) covers every band identified in `15-RESEARCH.md` §2. Checkpoint 1b (post-`ModelStarted`, pre-`stream_chat`) is the only band-1 sample without dedicated coverage; per RESEARCH §8 it is exercised on every successful round so a regression there fails every other test indirectly. PLAN.md `must_haves` should explicitly note this is an accepted sampling gap.

---

## Validation Sign-Off

- [ ] All tasks have `<automated>` verify or Wave 0 dependencies
- [ ] Sampling continuity: no 3 consecutive tasks without automated verify
- [ ] Wave 0 covers all MISSING references (single new test file + tokio feature confirmation)
- [ ] No watch-mode flags (cargo test in this project does not use `cargo watch`)
- [ ] Feedback latency < 30 s for the lattice-agent crate quick-run
- [ ] `nyquist_compliant: true` set in frontmatter once PLAN.md task IDs replace placeholders

**Approval:** pending — task IDs are placeholders until `gsd-planner` writes `15-XX-PLAN.md`.

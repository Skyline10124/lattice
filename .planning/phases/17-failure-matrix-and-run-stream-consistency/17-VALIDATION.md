---
phase: 17
slug: failure-matrix-and-run-stream-consistency
status: passed
nyquist_compliant: true
wave_0_complete: true
created: 2026-05-22
verified: 2026-05-22
---

# Phase 17 — Validation Strategy

> Per-phase validation contract and closeout state for failure behavior, timeout reconciliation, and run/stream semantic consistency.

---

## Test Infrastructure

| Property | Value |
|----------|-------|
| **Framework** | cargo test (built-in) |
| **Config file** | Cargo.toml workspace test settings |
| **Quick run command** | `cargo test -p lattice-agent --features test-utils timeout` and `cargo test -p lattice-api --test consistency` |
| **Full suite command** | `cargo test --workspace` |
| **Estimated runtime** | ~2-3 minutes for workspace suite |

---

## Sampling Rate

- **After every task commit:** Run targeted crate tests for touched code.
- **After review fixes:** Run timeout, live stream, consistency, fmt, clippy, and workspace tests.
- **Before Phase 18:** Phase 17 targeted tests and workspace test suite must be green.
- **Max feedback latency:** targeted tests under 60 seconds; workspace under several minutes.

---

## Per-Task Verification Map

| Task ID | Plan | Requirement | Behavior | Automated Command | File Exists | Status |
|---------|------|-------------|----------|-------------------|-------------|--------|
| 17-01-01 | 01 | D-12/D-15 | Cooperative timeout preserves partial state | `cargo test -p lattice-agent --features test-utils timeout` | ✅ | ✅ green |
| 17-01-02 | 01 | D-12/D-15 | Timeout covers stream creation, stream consumption, and tool execution | `cargo test -p lattice-agent --features test-utils timeout` | ✅ | ✅ green |
| 17-01-03 | 01 | D-12/D-15 | Expired deadline wins before ready stream events are consumed | `cargo test -p lattice-agent --features test-utils timeout` | ✅ | ✅ green |
| 17-02-01 | 02 | VER-12 | Deterministic commit failure emits `CommitFailed`, no `TurnCommitted` | `cargo test -p lattice-api stream_turn` | ✅ | ✅ green |
| 17-02-02 | 02 | VER-12 | Receiver drop does not cancel background live turn | `cargo test -p lattice-api stream_turn` | ✅ | ✅ green |
| 17-02-03 | 02 | VER-10 | Cancelled commit behavior covered through buffered seam | `cargo test -p lattice-api stream_turn` | ✅ | ✅ green |
| 17-03-01 | 03 | VER-13 | `run_turn` / `stream_turn` durable outcome consistency | `cargo test -p lattice-api --test consistency` | ✅ | ✅ green |

*Status: ✅ green · ❌ red · ⚠️ flaky*

---

## Wave 0 Requirements

- [x] `crates/lattice-api/tests/consistency.rs` — run/stream consistency tests (VER-13).
- [x] Deterministic commit failure test in `live_tests.rs` — replaces racy Phase 16 test (VER-12).
- [x] Cancelled buffered seam test — covers VER-10 while public live cancellation remains out of scope for alpha.
- [x] Cooperative deadline timeout tests in `live_sink_and_cancel.rs` — covers D-12/D-15 plus review regressions.

---

## Accepted Gaps

- Live channel backpressure is not covered by `AgentTurnRequest.timeout`: `emit_agent_event` still awaits bounded `mpsc::Sender::send`. This remains an accepted alpha gap because making event emission deadline-aware requires a separate live/result event-consistency contract.
- `RunTurnStage` remains a coarse breadcrumb field, not a precise execution timeline. Phase 18 CLI rendering should consume `TurnEvent`, not `RunTurnStage`.
- Public live cancellation does not expose a cancellation handle in alpha. Cancelled commit semantics are covered through the buffered seam.

---

## Manual-Only Verifications

All Phase 17 runtime behaviors have automated verification. Documentation closeout is verified by source review and `git diff --check`.

---

## Validation Sign-Off

- [x] All tasks have automated verification.
- [x] Sampling continuity: no 3 consecutive tasks without automated verify.
- [x] Wave 0 covers all missing Phase 17 references.
- [x] No watch-mode flags.
- [x] Feedback latency acceptable for targeted and workspace runs.
- [x] `nyquist_compliant: true` set in frontmatter.

**Approval:** passed

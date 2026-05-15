# Phase 2: Runtime Assembly Cutover - Discussion Log

> **Audit trail only.** Do not use as input to planning, research, or execution agents.
> Decisions are captured in CONTEXT.md — this log preserves the alternatives considered.

**Date:** 2026-05-14
**Phase:** 2-runtime-assembly-cutover
**Areas discussed:** runtime spec boundary, session-owned agent state, orchestrator/runtime cleanup scope, session API, verification gate

---

## Runtime Spec Boundary

| Option | Description | Selected |
|--------|-------------|----------|
| Directly consume `GenericAgentSpec` | Runtime consumes the Phase 1 profile-derived spec directly and avoids a duplicated public runtime spec. | ✓ |
| Introduce `RuntimeAgentSpec` | Runtime defines its own public spec and converts from `GenericAgentSpec`. | |
| Private adapter only | Public API accepts `GenericAgentSpec`, runtime converts into private assembly structs internally. | |

**User's choice:** Directly consume `GenericAgentSpec`.
**Notes:** User explicitly required KISS and a single chain. Unused code and logic should be deleted, not kept. Follow-up clarified that `cwd/project_root` must come from runtime invocation context, not from `GenericAgentSpec`; runtime public API should expose explicit generic handle/turn setup semantics; profile discovery/loading/lowering belongs to `lattice-profile`, not frontend logic or runtime core.

---

## Session-Owned Agent State

| Option | Description | Selected |
|--------|-------------|----------|
| Content-hash handle reuse | Keep live handles but key them by profile content and invocation context. | |
| Field-based handle reuse | Keep live handles and reuse by selected fields such as profile/model/session/security. | |
| Delete live-agent state path | Remove live agent as session owner; session owns history and runtime constructs per-turn execution state. | ✓ |

**User's choice:** Delete live-agent state path and return conversation ownership to `lattice-session`.
**Notes:** User rejected the assumption that runtime should reason about whether an agent object is live. The correct model is that the program uses an agent profile to assemble context and execute work, while session owns the conversation graph like a git repository. The agent profile behaves like a developer committing into a git repo: it influences work, but does not own the repo history.

---

## Profile Snapshot Safety

| Option | Description | Selected |
|--------|-------------|----------|
| Auto-use latest profile | Re-read profile each turn and automatically use the newest version. | |
| Lock session-start snapshot | Bind a session to the profile snapshot captured at session start. | ✓ |
| Prompt/fork on every change | Detect disk changes and require a choice before continuing. | |

**User's choice:** Lock session-start profile snapshot.
**Notes:** User identified a concrete security threat: if a profile is modified by prompt injection or automatic update during an active session, silently adopting the new profile becomes a fast attack path. Follow-up choice selected default behavior: continue using the locked snapshot and surface a warning when disk profile changes.

---

## Orchestrator / Runtime Cleanup Scope

| Option | Description | Selected |
|--------|-------------|----------|
| Replace only single-turn assembly | Implement only generic single-agent runtime assembly and leave broader extension API cleanup to later phases. | |
| Aggressively remove runtime extension APIs | Delete runtime public APIs for pipeline/plugin/event-bus/micro-agent/plugin-overlay/orchestrator-executor surfaces in Phase 2. | ✓ |
| Add generic path and keep old APIs | Preserve old runtime extension APIs while adding generic runtime path. | |

**User's choice:** Aggressively remove runtime extension APIs.
**Notes:** This is intentionally more aggressive than a minimal cutover. Full workspace crate removal remains Phase 4, but runtime should stop acting as the extension facade in Phase 2.

---

## Session / Manifest API

| Option | Description | Selected |
|--------|-------------|----------|
| Patch existing save flow | Keep existing `save_with_manifests` shape and add metadata. | |
| Design a clean session-first API | Add simple API semantics for profile snapshot binding, opening a turn, and committing a turn. | ✓ |
| Large session rewrite | Redesign session graph/storage broadly. | |

**User's choice:** Design a clean session-first API.
**Notes:** A concrete API sketch was discussed: durable `ProfileSnapshot`; `open_turn` returns locked profile snapshot, base commit, history, turn index, drift info, and parent manifests; `commit_turn` validates base commit/profile hash and writes messages plus status-bearing manifest. Runtime should use this as a transaction boundary.

---

## Verification Gate

| Option | Description | Selected |
|--------|-------------|----------|
| Minimal targeted tests | Only run new session/runtime tests and a few adapter tests. | |
| Targeted tests plus workspace check | Run targeted session/profile/runtime/CLI tests, `cargo check --workspace`, and `cargo fmt --all --check`. | ✓ |
| Full workspace test and clippy gate | Require full workspace tests and clippy warnings-as-errors for Phase 2 closure. | |

**User's choice:** Targeted tests plus workspace check.
**Notes:** Full workspace tests and clippy are useful if practical, but not the minimum closure gate. Any failures should be classified as Phase 2 regressions, pre-existing failures, or obsolete legacy tests.

---

## the agent's Discretion

- Exact Rust type and method names may be chosen during planning/implementation.
- Internal module placement for new session API may be chosen for reviewability.
- Existing CAS internals may be reused if the public API becomes clean and session-owned.

## Deferred Ideas

- Final CLI/TUI UX for profile drift and profile switching.
- Full workspace deletion of orchestrator/plugin crates.
- Explicit profile rebind/fork workflow beyond the safe default.

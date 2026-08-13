# Documentation

This directory separates maintained documentation from historical design and
release records.

## Platform contract

All maintained documentation assumes a Linux host. LATTICE supports Linux only
for development, build, test, deployment, and runtime operation. The
`wasm32-unknown-unknown` Source Tool target is a sandboxed guest-artifact
format within that Linux host, not another supported deployment platform.

Do not introduce macOS, Windows, BSD, generic-Unix, or other host compatibility
paths, or weaken a Linux implementation with a portable, degraded, or no-op
fallback for a possible future port. Expanding beyond Linux requires an
explicit architecture decision and a complete native implementation.

## Start Here

- [Getting Started](GETTING-STARTED.md) - install, configure, and run LATTICE.
- [Architecture](ARCHITECTURE.md) - current workspace structure and execution flow.
- [Configuration](CONFIGURATION.md) - `agent.toml`, providers, workspace, and LANCET policy.
- [Development](DEVELOPMENT.md) - contributor workflow and repository conventions.
- [Testing](TESTING.md) - test strategy, commands, and CI lanes.
- [LANCET](LANCET.md) - CEP and LANCET reference. Read its drift notice: current
  contracts and explicitly retained legacy sections coexist in this document.

## Architecture Decisions

- [ADR 0001](adr/0001-workspace-vcs-replaces-guest-git-promotion.md) — historical
  Workspace VCS design, superseded by ADR 0002.
- [ADR 0002](adr/0002-turn-scoped-msb-workspace.md) — historical turn-scoped MSB
  design, superseded by ADR 0003.
- [ADR 0003](adr/0003-lancet-operates-directly-on-local-workspace.md) — guarded
  Direct-Local Workspace execution.
- [ADR 0004](adr/0004-build-closed-wasm-tool-catalog.md) — build-closed WASM
  Component Tool catalog and Tool Host authority.
- [ADR 0005](adr/0005-durable-turn-invocation-and-private-git-stores.md) — typed
  Turn/Invocation identity, data-only conversation contracts, private-Git
  durability kernel, PendingTurn reconciliation, and ordered #139 hard cut.

## Operational Records

- [Test triage](TEST-TRIAGE.md) and
  [`test-triage-decisions.json`](test-triage-decisions.json) track the active
  test-suite rewrite decisions.
- [`agents/`](agents/) contains repository configuration consumed by engineering
  skills.
- [`specs/`](specs/) contains active design contracts. The Issue #27
  effect-model and tool-context specification remains authoritative while the
  migrated GitHub issue #27 (formerly Codeberg #5) is open.
- [`plans/`](plans/) contains per-issue implementation plans for active work.
  A document belongs in `specs/` or `plans/` only while its issue or
  implementation remains active; completed, superseded, or abandoned documents
  move to [`archive/`](archive/).

## Archive

[`archive/`](archive/) contains point-in-time release evidence, superseded
designs, and unimplemented research drafts. Archive documents are not current
requirements or architecture guidance.

When documents conflict, prefer the root [`AGENTS.md`](../AGENTS.md), the
maintained documents listed above, and the current source code.

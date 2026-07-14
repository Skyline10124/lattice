# Documentation

This directory separates maintained documentation from historical design and
release records.

## Start Here

- [Getting Started](GETTING-STARTED.md) - install, configure, and run LATTICE.
- [Architecture](ARCHITECTURE.md) - current workspace structure and execution flow.
- [Configuration](CONFIGURATION.md) - `agent.toml`, providers, workspace, and LANCET policy.
- [Development](DEVELOPMENT.md) - contributor workflow and repository conventions.
- [Testing](TESTING.md) - test strategy, commands, and CI lanes.
- [LANCET](LANCET.md) - CEP and LANCET reference. Read its drift notice: current
  contracts and explicitly retained legacy sections coexist in this document.

## Operational Records

- [Test triage](TEST-TRIAGE.md) and
  [`test-triage-decisions.json`](test-triage-decisions.json) track the active
  test-suite rewrite decisions.
- [`agents/`](agents/) contains repository configuration consumed by engineering
  skills.
- [`superpowers/`](superpowers/) contains active design contracts.
  The issue #5 effect-model and tool-context specification remains authoritative
  while that Codeberg issue is open.

## Archive

[`archive/`](archive/) contains point-in-time release evidence, superseded
designs, and unimplemented research drafts. Archive documents are not current
requirements or architecture guidance.

When documents conflict, prefer the root [`AGENTS.md`](../AGENTS.md), the
maintained documents listed above, and the current source code.

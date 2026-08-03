<!-- generated-by: gsd-doc-writer -->

# Contributing to LATTICE

Thanks for your interest in contributing to LATTICE! LATTICE is a session-centric
LLM runtime written in Rust and released under the [Apache License, Version 2.0](LICENSE).
It is open source, and all contributions are welcome — bug reports, fixes, tests,
docs, and new features that fit the architecture.

This document covers **process**: how to set up, develop, style, record, and submit
changes. For the workspace architecture, layering, and crate roles, read
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) and the workspace-level
[AGENTS.md](AGENTS.md).

> **Note:** The required section list below is process-focused. Architecture
> specifics (CEP method catalog, turn execution flow, dependency layering) live
> in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) and [AGENTS.md](AGENTS.md) and
> are intentionally not duplicated here.

## 1. Introduction

Welcome. LATTICE is a Cargo workspace containing 18 runtime/test crates plus one
root-level `xtask` tool crate (see root `Cargo.toml`), layered so dependencies always
point from higher layers down. Before making changes, please:

- Read [LICENSE](LICENSE) — contributions are licensed under Apache-2.0.
- Read [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the architecture overview
  and the layering diagram.
- Read the workspace-level [AGENTS.md](AGENTS.md) for architecture guidance and
  ownership rules.

By contributing, you agree your contributions are licensed under the Apache-2.0
license that covers the project.

## 2. Getting started

LATTICE requires Rust **1.94+** (edition 2024). The toolchain is pinned to
rustc 1.96.0 via `rust-toolchain.toml` (which also installs the
`wasm32-unknown-unknown` target for Source Tool builds). Build and test the
workspace:

```sh
git clone <your-fork-url>
cd lattice
cargo build
cargo test
```

`cargo xtask ci` additionally requires **`wasm-tools 1.254.0`** on `PATH`. It is
not a Cargo dependency, so nothing installs it for you, and `cargo xtask ci`
fails fast without it (`cargo xtask tool-build` shells out to it to encode
Source Tool Components and validate their import surface). Install it with:

```sh
cargo install wasm-tools --version 1.254.0 --locked
```

The version is pinned because the encoder determines the committed Component
bytes; a different version warns rather than fails, since bit-for-bit
reproducibility is still warn-only (see `#73` / `#80` and
[docs/TESTING.md](docs/TESTING.md)).

For any runtime testing that actually drives a model (e.g. `session send`, `tui`,
end-to-end turn tests that hit a provider), set a provider environment variable:

```sh
export ANTHROPIC_API_KEY="..."   # or OPENAI_API_KEY / GEMINI_API_KEY
```

Most unit and integration tests do not require a live provider key; set one only
when your change exercises the live provider path.

For a guided first-run walkthrough, see
[docs/GETTING-STARTED.md](docs/GETTING-STARTED.md).
<!-- VERIFY: docs/GETTING-STARTED.md is referenced as a planned Wave doc but is
     not present in the repo as of this writing. Link is aspirational; remove
     this marker once the file exists. -->

## 3. Development workflow

LATTICE uses two tiers of architecture guidance:

- **Workspace-level:** [AGENTS.md](AGENTS.md) — the layering, crate roles, CEP/LANCET
  contracts, and ownership rules.
- **Crate-local:** `crates/<crate>/AGENTS.md` — key files, ownership, and focused
  verification for that crate.

**Before editing a crate, read its `crates/<crate>/AGENTS.md`.** It tells you which
files own which concerns and where verification should land.

### Respect the dependency layering

The workspace is layered (see [AGENTS.md](AGENTS.md) and
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)). Keep dependencies pointing from
higher layers to lower layers — **no reverse dependencies**. Do not make a lower
layer depend on a higher one, and do not make `lancet-*` crates depend on
`lattice-*` crates.

### Follow the Ownership Rules

When your change crosses crate boundaries, start in the owning crate and update
the dependents in order:

- **Protocol shape changes** start in `lancet-protocol`, then update
  `lancet-catalog`, `lancet-authority`, `lancet-native` handlers, API mapping,
  and tests.
- **CEP method changes** start in `lancet-catalog`, then update native handlers
  and catalog-derived tool projection (`lancet-cep::project_tool_definitions`).
  Do not reintroduce native/API shim projection or parallel hard-coded tool
  definitions.
- **Provider protocol details** stay in `lattice-llm`; higher layers consume
  typed events and errors.
- **Native web/transport/server lifecycle** stays in `lancet-native`. Workspace
  observation and mutation Tool Host imports live in `lancet-runtime` (#30/#31).
- **LANCET runtime composition** stays in `lancet-api`; anything that drives CEP
  (CLI, tests, embedders) spawns through `lancet_api::LancetServer`/`LancetRuntime`.
- **Public session/turn API** stays in `lattice-api` (profile→policy translation
  and agent-loop wiring). **TUI terminal lifecycle** stays in `lattice-tui`.
  **CLI output/parsing** stays in `lattice-cli`.

## 4. Code style

All contributions must pass the workspace style gates:

```sh
cargo fmt --all --check
cargo clippy -- -D warnings
```

`cargo clippy -- -D warnings` treats warnings as errors. CI runs the stricter
`cargo clippy --workspace --all-targets -- -D warnings`, so match that locally
if you touch multiple targets.

### File headers

New Rust source files under `crates/**` **must** start with the workspace
copyright and SPDX header:

```rust
// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0
```

Preserve existing copyright and SPDX headers when editing source files. Do not
add or alter headers for generated, vendored, or external files unless the
surrounding maintained source already uses that convention.

## 5. Commit conventions

LATTICE uses [Conventional Commits](https://www.conventionalcommits.org/) scoped
to a crate or subsystem. Observed prefixes in the git log:

- `feat(scope):` — new feature
- `fix(scope):` — bug fix
- `refactor(scope):` — refactoring with no behavior change
- `chore:` — tooling, deps, hygiene (scope optional)
- `docs(scope):` — documentation
- `test(scope):` — tests

The default branch is **`main`**. Keep commits focused: one logical change per
commit, with a clear subject line and a body that explains *why* when the change
is non-obvious. Reference a finding or issue id in the body when relevant.

<!-- VERIFY: Conventional Commits and the prefix set above are inferred from the
     recent git log (`feat(tui):`, `fix(session):`, `refactor(execution):`,
     `chore:`, `docs(session):`, `test(session):`). The project does not appear
     to enforce commit format via tooling; follow observed convention. -->

## 6. Record-keeping

LATTICE keeps three review-record files at the repo root. Update them as part of
landing a change.

### `FIX.md` — append-only changelog of landed fixes

`FIX.md` is the append-only changelog for applied fixes. **Never delete or
overwrite old entries.** Add a `FIX.md` entry only *after* the fix has landed.
Each entry should include, when practical:

- issue id / severity
- the problem
- fix summary
- verification commands and results
- changed files

### `FINDINGS.md` — open / deferred / partial / residual issues

`FINDINGS.md` tracks open, deferred, partial, or residual issues. Every open
entry should include severity/id, location, problem, current status, and plan.

When you fix a finding: record the applied fix in `FIX.md`, then **remove it from
the open section of `FINDINGS.md`** or move it to an obsolete/closed section.

For **partial fixes**: record the completed work in `FIX.md` and keep the
remaining risk in `FINDINGS.md` with an explicit residual status and plan.

### `INFO.md` — non-blocking notes

`INFO.md` holds non-blocking notes that are neither fixes nor open issues. Use it
for observations, context, and reminders that don't require action.

## 7. Submitting changes

Before opening a pull request, run the full local gate and make sure it is green:

```sh
cargo xtask ci
```

`cargo xtask ci` is the canonical local CI replica. It runs, in order
(`xtask/src/ci.rs`):

1. toolchain precheck — pinned rustc 1.96.0, the `wasm32-unknown-unknown`
   target, and `wasm-tools` on `PATH` (missing encoder fails; wrong version
   warns)
2. `cargo fmt --all --check`
3. `cargo clippy --workspace --all-targets -- -D warnings`
4. `cargo build -p lattice-cli --bin lattice` (builds the binary the
   `lattice-system-harness` black-box tests drive via `LATTICE_BIN`)
5. `cargo xtask harness-boundary` (verifies the harness crate has
   no product-crate dependency, direct or transitive)
6. `cargo xtask lancet-boundary` (LANCET executor/dependency boundary checks)
7. `cargo xtask tool-codegen --check` (generated Tool artifacts are in sync
   with their source manifests)
8. `cargo test --workspace` with `LATTICE_BIN` defaulted to
   `$(pwd)/target/debug/lattice`, run per crate in dependency order under a
   `CARGO_BUILD_JOBS` cap and a per-step timeout (see `ENV-OOM-01` in
   `FINDINGS.md`)
9. `cargo xtask tool-build --verify` — Component digest reproducibility;
   **digest drift is warn-only**, but a failed guest build, encode, validate,
   `wasi:*` import assertion, or WIT conformance check does fail the gate

Note that the ordinary GitHub lane (`.github/workflows/ci.yml`) does **not** run
step 1 or step 9; the nightly `full-gate` lane runs step 9 and installs the
pinned `wasm-tools` for it. See [docs/TESTING.md](docs/TESTING.md) for the full
lane-by-lane breakdown.

CI (`.github/workflows/ci.yml`) runs the fmt/clippy/test gates on
`ubuntu-latest` for pushes to `main` and pull requests. LATTICE currently
supports Linux hosts only; macOS and Windows are not release or CI targets.
A clean local run avoids surprises.

Open a PR against **`main`**. There is currently no `.github/PULL_REQUEST_TEMPLATE.md`,
so in your PR description please:

- summarize the change and its motivation;
- list the verification commands you ran and their results;
- note any `FIX.md` / `FINDINGS.md` / `INFO.md` record updates you made;
- call out anything that is a partial fix with residual risk.

Keep PRs focused and reviewable. If a change spans multiple concerns, prefer
several small PRs over one large one.

<!-- VERIFY: No .github/PULL_REQUEST_TEMPLATE.md and no .github/CONTRIBUTING.md
     exist as of this writing; the .github/ directory contains only workflows/.
     Re-check before relying on a template. -->

## 8. Workspace hygiene

- **Do not modify `.gitignore` in agent-driven changes.** In particular, do not
  add ignore exceptions or whitelist rules to force ignored private/planning/build
  paths into commits. If a path appears to need tracking despite ignore rules,
  **stop and ask a human maintainer** to change the policy explicitly.
- **Keep crate-local `AGENTS.md` files near 80 lines** by pruning stale or
  closed bullets. They are navigation aids, not archives — move settled history
  into `FIX.md`/`FINDINGS.md` and keep the crate `AGENTS.md` focused on current
  ownership and key files.
- Preserve existing copyright/SPDX headers when editing source (see §4).
- Prefer crate-local `AGENTS.md` for detailed navigation before editing a crate.

## Summary checklist

- [ ] Read [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) and the relevant
      `crates/<crate>/AGENTS.md`
- [ ] Respect layering (no reverse deps) and Ownership Rules
- [ ] `cargo xtask ci` is green (fmt, clippy, build, harness boundary,
      `cargo test --workspace`)
- [ ] New files have the `// Copyright 2026 Ast Rin` / SPDX header
- [ ] Conventional-commit messages, PR against `main`
- [ ] `FIX.md` / `FINDINGS.md` / `INFO.md` updated as needed
- [ ] No `.gitignore` changes without maintainer approval

Thank you for contributing to LATTICE.

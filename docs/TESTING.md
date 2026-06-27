<!-- generated-by: gsd-doc-writer -->

# LATTICE Testing

This document describes how LATTICE tests are written and run. LATTICE is a
Rust Cargo workspace (16 member crates, see root `Cargo.toml`); testing uses
Rust's built-in `#[test]` attribute and the `cargo test` runner. There is no
Node/JavaScript toolchain, no `package.json`, and no external test framework
(jest, vitest, etc.).

All commands below are grounded in the workspace manifest
(`Cargo.toml`), the CI workflow (`.github/workflows/ci.yml`), the local CI
script (`scripts/ci-local.sh`), and crate `Cargo.toml` feature definitions.

---

## 1. Test framework and setup

LATTICE uses Rust's first-party test framework:

- Tests are functions annotated with `#[test]`, discovered and run by
  `cargo test` (the built-in `libtest` harness). No external runner is
  required.
- There is **no separate test install step**. `cargo test` compiles test
  targets on demand; a prior `cargo build` is not required, though the first
  `cargo test` invocation will compile the workspace and its test targets.
- Property/fuzz tests use `proptest` (a workspace dev-dependency,
  `proptest = "1"` in `Cargo.toml`); regression seeds live alongside the
  tests, e.g. `crates/lancet-authority/tests/guard_decide_fuzz.proptest-regressions`.

No `cargo nextest` configuration is present in the repo (no `.config/nextest.toml`,
no `nextest` section in `.cargo/config.toml`), so `cargo nextest` is **not** part
of the standard workflow. It can be used as a drop-in runner if installed locally,
but CI and the local gate use `cargo test`.

---

## 2. Running tests

### Whole workspace

```bash
cargo test --workspace
```

This is the exact command used by CI (`.github/workflows/ci.yml`) and by
`scripts/ci-local.sh`.

### Single crate

```bash
cargo test -p lattice-api
cargo test -p lancet-authority
```

### Single test

```bash
cargo test -p lattice-api turn_lifecycle
cargo test -p lancet-authority guard_decide
```

(`cargo test` matches by substring against the test path/module path.)

### Release-mode tests

```bash
cargo test --workspace --release
```

### Tests behind a feature

Some test targets and helpers are gated behind features (see §3). Enable them
explicitly:

```bash
# DeterministicWorkspace + contract helper test targets (lattice-workspace)
cargo test -p lattice-workspace --features test-utils

# Feature surface for lattice-workspace (recommended in its AGENTS.md)
cargo test -p lattice-workspace --features test-utils
```

### Lint and format (part of the CI gate)

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```

These two run **before** `cargo test` in both `.github/workflows/ci.yml` and
`scripts/ci-local.sh`, and are part of the release gate
(`scripts/release-gate-v1.4.sh`).

### Local CI replica

```bash
scripts/ci-local.sh
```

Runs `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
and `cargo test --workspace` in sequence with pass/fail markers — a local mirror
of the CI `check` job.

### LANCET self-check

```bash
cargo run -p lattice-cli -- lancet doctor
```

`lattice lancet doctor` (dispatched in `crates/lattice-cli/src/commands/lancet.rs`,
backed by `lancet_api::run_doctor_checks`) performs catalog count, transport
smoke, and invariant-card parse checks. It is part of the release gate and a
useful post-build sanity check.

### Reference: release gate entrypoint

```bash
scripts/release-gate-v1.4.sh          # offline deterministic (default)
scripts/release-gate-v1.4.sh --live   # also runs live Linux physical canaries
```

This script exists at `scripts/release-gate-v1.4.sh` (verified). It is offline
deterministic by default: it runs `cargo fmt`, `cargo clippy`, targeted crate
tests, the dogfood suite, the LANCET boundary check, and `lattice lancet doctor`.
Live Linux physical canaries run only with `--live` or when
`LATTICE_LINUX_ISOLATION_LIVE=1` is set. See `docs/LANCET.md` "Release gate"
for the narrative; this file documents the script as the release-gate
entrypoint.

---

## 3. Writing new tests

LATTICE follows Rust conventions with two coexisting patterns:

### Inline unit tests

Unit tests live in `#[cfg(test)] mod tests` (often `mod tests`) blocks inline
in `src/*.rs` files. Every crate has inline `#[cfg(test)]` modules; counts per
crate (files containing `#[cfg(test)]` under `crates/<crate>/src/`):

| Crate | Files with `#[cfg(test)]` |
|---|---|
| `lancet-native` | 19 |
| `lattice-llm` | 11 |
| `lancet-authority` | 11 |
| `lattice-cli` | 10 |
| `lattice-api` | 9 |
| `lancet-catalog` | 7 |
| `lancet-protocol` | 6 |
| `lattice-profile` | 4 |
| `lancet-audit` | 4 |
| `lattice-session` | 3 |
| `lancet-api` | 2 |
| `lancet-dispatch` | 2 |
| `lattice-agent` | 2 |
| `lattice-workspace` | 2 |
| `lancet-cep` | 1 |
| `lattice-tui` | 1 |

Minimal inline unit-test example (matches the style in
`crates/lancet-protocol/src/error.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn family_range_is_contiguous() {
        // ...assertions against items in the parent module via `super::*`...
    }
}
```

### Co-located `*_tests.rs` files

Several crates split larger unit-test groups into sibling `*_tests.rs` files
that are `mod`-included from a `src/*.rs` file (still compiled under
`#[cfg(test)]`). Confirmed examples:

- `crates/lancet-audit/src/audit_tests.rs`
- `crates/lancet-native/src/handlers/command_tests.rs`
- `crates/lattice-api/src/turn/{cep_tests,stream_tests,run_tests,live_tests,types_tests}.rs`
- `crates/lattice-cli/src/commands/run_tests.rs`
- `crates/lattice-workspace/src/testing/contract_tests.rs`

### Integration tests (`tests/` directories)

Per-crate `tests/` directories hold integration tests (separate test binaries
per file). Crates with `tests/` directories:

- `crates/lancet-authority/tests/` — `guard_decide_fuzz.rs`, `policy_coverage.rs`, `policy_tests.rs`
- `crates/lancet-native/tests/` — `cep_warn_harness.rs`, `policy_integration.rs`, `prov_isomorphism.rs`
- `crates/lattice-agent/tests/` — `agent_loop.rs`, `live_sink_and_cancel.rs`, `provisional_completion.rs`, `streaming_delta.rs`, `streaming_seam.rs`
- `crates/lattice-api/tests/` — turn lifecycle, CEP round-trip, git-store, promotion, resume, snapshot journal, usage, dogfood, and release e2e suites (plus `support/` helpers)
- `crates/lattice-cli/tests/` — `cli_cutover.rs`, `release_smoke.rs`
- `crates/lattice-session/tests/` — `promotion_commit.rs`
- `crates/lattice-workspace/tests/` — promotion, git status, credential redaction, egress, SDK probe, and workspace primitive suites

Some `lattice-workspace` integration test binaries are declared explicitly in
`crates/lattice-workspace/Cargo.toml` via `[[test]]` entries, several with
`required-features = ["test-utils"]` (e.g. `promotion_candidate_validation`,
`promotion_end_to_end`, `workspace_primitives`, `retained_snapshot`).

### Test helpers behind the `test-utils` feature

Shared test helpers that must be usable from integration tests (outside
`#[cfg(test)]`) are exported behind a `test-utils` Cargo feature:

- **`lattice-workspace`** defines `test-utils = []`, which gates
  `DeterministicWorkspace` and the adapter contract suite in
  `crates/lattice-workspace/src/testing/` (see
  `crates/lattice-workspace/AGENTS.md`). `DeterministicWorkspace` is a
  deterministic, KVM-free test adapter for CI; it records but does not enforce
  egress policy and must not be treated as a security sandbox.
- **`lattice-agent`** defines `test-utils = []` for its agent-loop test seams.
- Downstream crates pull these helpers in by depending on the crate with the
  feature enabled, e.g. `lattice-api/Cargo.toml` and `lattice-cli/Cargo.toml`
  declare `lattice-workspace = { features = ["test-utils"] }` and
  `lattice-agent = { features = ["test-utils"] }`.

When adding a new integration test that needs `DeterministicWorkspace` or the
agent test seams, either (a) add the test under a crate that already enables
`test-utils`, or (b) declare a `[[test]]` entry with
`required-features = ["test-utils"]` in the relevant `Cargo.toml`.

### Live KVM integration tests (NOT in normal CI)

`lattice-workspace` additionally defines
`integration_tests = ["test-utils"]`, which gates **live** KVM/libkrun
integration tests. These require a provisioned KVM host, `libkrunfw.so`, and a
reachable image registry. Per
`crates/lattice-workspace/AGENTS.md` "Verification":

- They are **excluded from normal CI**; CI must remain deterministic and KVM-free.
- Compile-check them without running:
  `cargo clippy -p lattice-workspace --features integration_tests --all-targets -- -D warnings`
- Run them only on an explicitly provisioned KVM/libkrun host.

`lattice-api` re-exports this feature as
`integration_tests = ["lattice-workspace/integration_tests"]`.

---

## 4. Coverage requirements

No coverage tooling is configured. There is no `tarpaulin.toml`, no
`llvm-cov`/`.llvm-cov*` configuration, and no `nextest` coverage profile in
the repository.

**No coverage threshold configured.** The CI gate enforces correctness via
`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
and `cargo test --workspace`, not via a coverage percentage.

<!-- VERIFY: No coverage threshold configured — confirmed by absence of
tarpaulin/llvm-cov/nextest coverage config in the repo as of writing. -->

---

## 5. CI integration

### GitHub Actions workflow

File: `.github/workflows/ci.yml`

- **Workflow name:** `CI`
- **Trigger:** `push` to `main`, and all `pull_request` events.
- **Matrix:** `ubuntu-latest` and `macos-latest`.
- **Job:** `check` (runs on `${{ matrix.os }}`).
- **Steps (in order):**
  1. `actions/checkout@v4`
  2. `dtolnay/rust-toolchain` (stable) with `rustfmt, clippy` components
  3. `Swatinem/rust-cache@v2`
  4. `cargo fmt --all --check`
  5. `cargo clippy --workspace --all-targets -- -D warnings`
  6. `cargo test --workspace`

The lint and format checks run before tests and are `-D warnings`-strict, so a
single clippy warning or formatting diff fails CI before tests execute.

### Local pre-commit hook

The repo ships `.githooks/pre-commit` (install with
`git config core.hooksPath .githooks`). This hook is **not** a test runner — it
blocks commits that stage files matching the repository's Git ignore rules
(e.g. `.env`, `.codex/`, `.agents/`, `.planning/`, build outputs). It guards
against accidentally force-adding private/build state into history; it does
not run `cargo test` or clippy. Use `scripts/ci-local.sh` for a local test
replica.

### Release gate

`scripts/release-gate-v1.4.sh` is the release-gate entrypoint (verified to
exist). It is offline deterministic by default and additionally runs
`lattice lancet doctor`, the dogfood lane, and the LANCET boundary check.
Live Linux physical canaries are opt-in via `--live` or
`LATTICE_LINUX_ISOLATION_LIVE=1`. See `docs/LANCET.md` "Release gate" for the
narrative.

### Exclusions from CI

As noted in §3, `lattice-workspace` live KVM/libkrun integration tests (the
`integration_tests` feature) are **not** part of the `CI` workflow or
`ci-local.sh`. They run only on a provisioned KVM host. The CI `cargo test
--workspace` invocation runs the default-feature deterministic test set
across all 16 crates.

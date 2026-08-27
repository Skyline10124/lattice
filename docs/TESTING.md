# LATTICE Testing

This document describes how LATTICE tests are written and run. LATTICE is a
Rust Cargo workspace: 19 product crates under `crates/`, plus `tools/noop` and
the root-level `xtask` automation crate (21 workspace members total; see root
`Cargo.toml`). Testing uses Rust's built-in `#[test]` attribute and the
`cargo test` runner. There is no Node/JavaScript toolchain, no `package.json`,
and no external test framework.

The toolchain is pinned to `rustc 1.96.0` with the `wasm32-unknown-unknown`
target installed (`rust-toolchain.toml`); `cargo xtask ci` and the rollout gate
both assert this up front. Building Source Tool Components additionally needs
**`wasm-tools 1.254.0`** on `PATH` — it is not a Cargo dependency, so nothing
installs it for you (`cargo install wasm-tools --version 1.254.0 --locked`).
`cargo xtask ci` fails fast when it is missing and warns on a version mismatch,
because the encoder determines the committed `.wasm` bytes (see `#73` / `#80`).
Property/fuzz tests use `proptest` (a workspace
dev-dependency, `proptest = "1"` in `Cargo.toml`); regression seeds live
alongside the tests (e.g.
`crates/lancet-authority/tests/guard_decide_fuzz.proptest-regressions`).

The test architecture is the **Direct-Local** model landed in issues #41 / #40:
product behavior is proven by driving the built `lattice` binary through
the real turn path against the canonical Local Workspace, and crate tests keep
local invariants only. The per-test keep/elevate/delete triage for the
The per-test keep/elevate/delete triage for the pre-rewrite suite was recorded in historical issue #40.

---

## 1. The four test layers

LATTICE testing is organized into four layers. Each layer owns a distinct
class of assertion; the ownership rules in [§6](#6-where-a-new-test-belongs)
decide where a new test belongs.

### Layer 1 — Crate-local invariants

`#[cfg(test)] mod tests` blocks inline in `src/*.rs`, co-located `*_tests.rs`
files `mod`-included under `#[cfg(test)]`, and per-crate `tests/` integration
test binaries.

This layer owns **crate-internal contracts only**: parsing, error mapping,
pure logic, type invariants, and closed-set checks. It must **never** prove
end-to-end product behavior (a real turn, a real file mutation, a real audit
row) from inside a product crate — that belongs to layer 2 or 3.

Confirmed `*_tests.rs` and `tests/` examples include
`crates/lancet-audit/src/audit_tests.rs`,
`crates/lattice-api/src/turn/{cep_tests,stream_tests,run_tests,live_tests,types_tests}.rs`,
`crates/lattice-cli/src/commands/run_tests.rs`,
`crates/lancet-authority/tests/` (guard-decide fuzz, `policy_tests.rs`),
`crates/lancet-native/tests/` (CEP warn harness, policy integration, Component
evidence round trips), and `crates/lattice-agent/tests/` (agent loop, streaming,
completion).

**Pre-existing exception.** Several suites under `crates/lattice-api/tests/`
(Direct-Local acceptance, dogfood matrix, and release e2e) still
prove end-to-end behavior from inside a product crate, which the rule above
forbids. They predate this architecture and are retained because CI and the
rollout gate consume their offline/dogfood evidence (§7). They are
**not a precedent**: a new test of that shape belongs in layer 3.

### Layer 2 — Fast binary black-box

The `lattice-system-harness` crate's `fast_*` test targets
(`tests/fast_cli_help.rs`, `tests/fast_audit_cli.rs`,
`tests/fast_config_cli.rs`, `tests/fast_lancet_cli.rs`,
`tests/fast_session_cli.rs`) plus all `#[test]` unit tests in the harness
`src/`. These drive the built `lattice` binary for fast CLI-surface checks:
help output, HOME/XDG isolation, config/session/audit/lancet subcommands.

`lattice-system-harness` is an **external** black-box harness, not an
integration target of any product crate. Its manifest must not depend on
`lattice-*`, `lancet-*`, or workspace path product crates — enforced by
`cargo xtask harness-boundary`. It obtains the product only through
`LATTICE_BIN` (see [§2](#2-the-lattice_bin-contract)).

### Layer 3 — Direct-Local external-binary scenarios

The `direct_*` scenario matrix plus `direct_local_turn` and `pty_tui_smoke` in
`lattice-system-harness/tests/`. These drive the real turn path
(`lattice_api::stream_turn`) against the canonical Local Workspace — the
project root — with no hypervisor, daemon, or container image required.

The Direct-Local matrix (landed in #46 (Codeberg #24), slices 0–8):

- `direct_turn_contract.rs` — turn lifecycle + session reopen.
- `direct_observation.rs` — workspace visibility and bounds.
- `direct_mutation.rs` — atomic mutation, concurrency, effect denial.
- `direct_tools.rs` — fetch, todo, catalog, retired methods.
- `direct_failures.rs` — provider failures, SIGINT, deadline, no-rollback.
- `direct_operator.rs` — independent native/Component/mixed audit contracts,
  query/tail cursors, replay completeness, and newest-20 status bounds.
- `direct_secrets.rs` — sentinel scanning.
- `direct_session_isolation.rs` — two-session graph isolation.
- `direct_local_turn.rs` — plain-text turn, session reopen against changed
  Local state, policy denial.
- `pty_tui_smoke.rs` — proves the TUI entrypoint wires into the same
  Direct-Local `stream_turn` chain.

This layer owns **product behavior observable through the `lattice` CLI or a
real turn**: turn commit/replay, observation/mutation contracts, tool
dispatch, failure and cancellation semantics, operator cross-validation, and
TUI wiring. A broken Direct-Local environment is a **product failure, never a
skip** — none of these targets are `#[ignore]`-gated, and all run in the
ordinary CI lane.

### Layer 4 — Manual UX checks

Human judgement of interaction quality: TUI feel, output readability, error
phrasing, and end-to-end ergonomics. This layer is exercised by a person, not
by `cargo test`.

Per [#41](https://github.com/Skyline10124/lattice/issues/41), this layer must
**never be the sole correctness gate for any feature**. Anything that can be
asserted mechanically belongs in layers 1–3; a manual check may only add
judgement on top of automated coverage that already exists.

The alpha rollout gate (`cargo xtask rollout --channel alpha`) is **not** a
test layer — it is release-evidence tooling that composes layers 1–3 against a
locked release artifact. See [§7](#7-running-tests) and
[§8](#8-ci-integration).

### Retired lanes (historical)

Earlier releases ran a hypervisor-backed sandbox-isolation test lane and a set
of shell gate scripts. Both are removed — the isolation lane in #40 and the
shell gates in #47 — and are **not** current test layers. They must not be
reintroduced. For the precise removed-subsystems list, see
ADR 0003 and root `AGENTS.md` ("Architecture"); release evidence now flows
through the layer-4 rollout gate.

---

## 2. The `LATTICE_BIN` contract

Layers 2–4 drive a pre-built `lattice` binary located through the `LATTICE_BIN`
environment variable.

**Who sets it:**

- GitHub CI (`.github/workflows/ci.yml`, `full-gate.yml`) sets
  `LATTICE_BIN=${{ github.workspace }}/target/debug/lattice` (an absolute
  path) before `cargo test --workspace`.
- `cargo xtask ci` defaults it to `$(pwd)/target/debug/lattice` when unset
  (`xtask/src/ci.rs`).
- `cargo xtask rollout --channel alpha` sets it to the absolute release-binary
  path `target/release/lattice` (`xtask/src/rollout.rs`).
- For manual harness runs, set it yourself before `cargo test`.

**How it is resolved:** `LatticeDriver::from_env`
(`crates/lattice-system-harness/src/driver.rs`) runs **in the test process**:
it reads `LATTICE_BIN`, checks `path.is_file()` (resolved from the test-process
cwd), then `std::fs::canonicalize`s it to an absolute path before any scenario
child is spawned. Unset ⇒ `MissingLatticeBin`; not a file or uncanonicalizable
⇒ `BinaryNotFound`.

**Why an absolute path is preferred:** `cargo test -p lattice-system-harness`
runs with the **crate directory** as cwd, where `target/debug/lattice` is not a
file (the binary is built at the workspace root's `target/`). A relative path
that is a real file from the test-process cwd works fine — `canonicalize` then
stores an absolute path so the later per-scenario child `current_dir` change
cannot break it — but `$(pwd)/target/debug/lattice` from the workspace root is
the cwd-independent form and avoids the mismatch entirely. It is a robustness
recommendation, not a code-enforced "must be absolute" contract.

```bash
cargo build -p lattice-cli --bin lattice
export LATTICE_BIN="$(pwd)/target/debug/lattice"
```

**Git worktrees:** `xtask ci` only fills `LATTICE_BIN` in when it is *unset*, so
a value exported for the main checkout (by `direnv`, a shell profile, or an
earlier session) survives into a worktree run and points at the main
checkout's `target/`. The gate then builds the worktree binary, runs the
harness against the stale path, and every Layer 2–4 test fails with
`BinaryNotFound` — a green branch reads as broken. Either unset it
(`env -u LATTICE_BIN cargo xtask ci`, letting the gate resolve its own) or
point it at the worktree.

---

## 3. Fixture rules

Every `lattice-system-harness` scenario (layers 2–3) follows the same
isolation contract (`crates/lattice-system-harness/AGENTS.md`):

- **Isolated HOME/XDG** — the scenario world owns a temp HOME; the developer's
  HOME never leaks (`real_home_leak_check`).
- **Dedicated session store** under the scenario HOME, not the developer's.
- **Scenario-local ports** — OS-assigned ephemeral ports, never shared.
- **Empty project `.env`** — no host environment bleeds into the project.
- **`env_clear()` + allow-list** — child processes start from a cleared
  environment with only an explicit allow-list set.
- **Shared cleared env for git and `lattice`** — both subprocesses use the same
  allow-list; no host HOME or git config reaches them.
- **CLI-only state reads** — session/audit state is read through the `lattice`
  CLI, never by importing product crates.
- **Fake LLM via formal config** — the fake provider
  (`src/fake_llm.rs`) connects through the real `OPENAI_COMPATIBLE_*`
  configuration; there is no test-only provider, hidden flag, or product-side
  change (RFC #41 (Codeberg #19) decision four).
- **Process-group children** — every child is its own process-group leader on
  Linux; timeout and controlled interrupt retain drained stdout/stderr and clean
  the whole group.

---

## 4. Direct-Local scenario rules

**Time budgets** (`crates/lattice-system-harness/src/scenario.rs`):

- `ScenarioSpec::direct_local` sets a **30 s per-command** timeout and a
  **180 s whole-scenario** wall-clock budget.
- PTY smoke (`tests/pty_tui_smoke.rs`) uses **120 s per-command** and a
  **300 s** whole-scenario budget.
- `ScenarioBudget::cap_timeout` applies `min(step_timeout, remaining_total)`
  before every spawned command, so a sequence of steps that each finish within
  their step timeout but never terminates is still bounded.

**Layer tagging:** `ScenarioLayer` has two variants — `FastBinary` and
`PtySmoke`. Direct-Local scenarios use `ScenarioSpec::direct_local`, which
carries the `FastBinary` layer tag with a whole-scenario budget; PTY smoke uses
`PtySmoke`.

**No external runtime dependency:** Direct-Local scenarios need no
hypervisor, no daemon, and no container image. A broken Direct-Local
environment is a product failure, never a skip — there are no `#[ignore]`-gated
tests in the harness (verified: the only `#[ignore]` mentions in
`crates/lattice-system-harness/` are negative doc comments stating scenarios
are *not* ignore-gated).

**Concurrency:** scenarios are self-isolated (own HOME, port, and project
directory) and run in parallel under `cargo test --workspace`. Scenario-internal
races — e.g. `direct_failures` provider failures and SIGINT — are coordinated
with `ResponseGate` plus bounded request waits, not with a global
`--test-threads=1` directive.

---

## 5. Failure artifacts

When a scenario fails an assertion wrapped in `assert_with_failure_artifacts`
(`crates/lattice-system-harness/src/assert.rs`), the harness writes a redacted
bundle under `$LATTICE_HARNESS_FAILURE_DIR/<scenario>/`.

**Where they land:**

- Default: `<harness crate>/target/harness-failures/<scenario>/`, anchored to
  `CARGO_MANIFEST_DIR` so it is absolute and independent of the test-process
  cwd.
- `cargo xtask ci` and CI redirect to the workspace-level
  `target/harness-failures/` so a full run collects every scenario's bundle in
  one place.
- `cargo xtask rollout` redirects to `target/rollout/harness-failures/`.
- `target/` is gitignored, so bundles persist for post-mortem inspection
  without polluting the tree.

**Bundle contents** (`src/artifacts.rs`):

- `stdout.txt`, `stderr.txt`, `command.txt` — redacted command output and
  summary.
- `world.txt` — scenario name, redacted command, and env context.
- `workspace_after.txt`, `workspace_diff.txt` — the workspace tree after the
  scenario and a unified diff against the pre-scenario baseline.
- Multi-step scenarios additionally write per-step `0-command.txt`,
  `0-stdout.txt`, `0-stderr.txt`, … for each tracked command.

**Redaction and secret scanning:**

- `redact()` replaces every known secret with `[REDACTED]` in stdout, stderr,
  the command summary, and the world context before anything is written.
- `scan_artifacts_for_secrets` recursively byte-level scans the bundle for
  secret sentinels, redacts hits in place, and **fails closed** on any
  traversal/read/rewrite error — a bundle that could not be fully inspected is
  not certified clean.
- `verify_persisted_bundle` re-scans after writing; a post-write sentinel hit
  is a hard error.
- Stale bundles are cleared when evidence capture begins; **passing scenarios
  write no bundle.**

---

## 6. Where a new test belongs

| Assertion you want to add | Belongs in | Not in |
| --- | --- | --- |
| A crate-internal contract (parsing, error mapping, pure logic, type invariant, closed-set check) | Layer 1: `#[cfg(test)]` in the product crate, a co-located `*_tests.rs`, or a per-crate `tests/` binary | the harness |
| Fast CLI-surface behavior against the built binary (help, config, a subcommand's output) | Layer 2: a `fast_*` target or harness `src/` unit test in `lattice-system-harness` | a product crate's `#[cfg(test)]` |
| Product behavior through a real turn / real Local Workspace mutation / operator audit query-tail-replay / TUI wiring | Layer 3: a `direct_*` scenario, `direct_local_turn`, or `pty_tui_smoke` in `lattice-system-harness` | a product crate's `#[cfg(test)]` |
| Interaction quality that only a human can judge (TUI feel, output readability, error phrasing) | Layer 4: a manual UX check | any automated gate — and it may never be a feature's *only* coverage |
| A release-evidence step against the locked release binary | Not a test layer: a step in `xtask/src/rollout.rs` | a shell wrapper script |
| Default-distribution built-in Tool behavior (the five shipped Tools) | Layer 3: a `direct_*` scenario | a product crate's `#[cfg(test)]` |
| Tool Bundle install / rebuild / activation lifecycle | Out of scope here — owned by [#35](https://github.com/Skyline10124/lattice/issues/35) | the #47 rollout gate or the Direct-Local default-product matrix |

Hard rules:

- **Never** add a new shell wrapper script for test/release gating — release
  evidence flows through `cargo xtask rollout` and the xtask contracts.
- **Never** resurrect a retired isolation lane (see "Retired lanes" above).
- **Never** write a test that counts as passing when it ran zero assertions.
  The rollout gate's anti-vacuous guard rejects any `cargo test` step whose
  summed `test result: ok. N passed` is zero (the proven zero-pass failure
  mode), and crate tests should not rely on filter-by-substring matching that
  can silently match nothing.

---

## 7. Running tests

### Build the binary first (layers 2–4 need it)

```bash
cargo build -p lattice-cli --bin lattice
```

### Whole workspace (what CI runs)

```bash
export LATTICE_BIN="$(pwd)/target/debug/lattice"
export LATTICE_HARNESS_FAILURE_DIR="$(pwd)/target/harness-failures"
cargo test --workspace
```

`cargo test --workspace` runs every member, including `lattice-system-harness`
— i.e. crate-local invariants **and** every fast / Direct-Local / PTY scenario
in one step. `lattice-system-harness` resolves the binary via `LATTICE_BIN`.

### Harness-only iteration

```bash
LATTICE_BIN="$(pwd)/target/debug/lattice" \
  cargo test -p lattice-system-harness
```

### Single crate

```bash
cargo test -p lattice-api
cargo test -p lancet-authority
```

### Single test target

```bash
cargo test -p lattice-system-harness --test direct_local_turn
cargo test -p lattice-system-harness --test direct_operator
cargo test -p lattice-system-harness --features release-operator \
  --test release_operator release_operator_native_component_mixed_matrix -- --exact
```

### Single test function (substring match)

```bash
cargo test -p lattice-system-harness fast_home_xdg_isolated_from_developer_home
```

(`cargo test` matches the name by substring against the full test path.)

### Tests behind a feature

Shared test helpers exported for use from integration tests live behind a
`test-utils` Cargo feature. `lattice-agent` defines `test-utils = []`
(`crates/lattice-agent/Cargo.toml`); `lattice-api` and `lattice-cli` enable it
on their `lattice-agent`/`lattice-session` deps. Run them explicitly:

```bash
cargo test -p lattice-agent --features test-utils
```

### Release-mode tests

```bash
cargo test --workspace --release
```

### Local CI replica

```bash
cargo xtask ci
```

A local mirror of the ordinary CI lane. Steps, in order
(`xtask/src/ci.rs`):

1. toolchain precheck (pinned `rustc 1.96.0` + `wasm32-unknown-unknown` +
   `wasm-tools` on `PATH`; a wrong `wasm-tools` version warns, a missing one
   fails)
2. `cargo fmt --all --check`
3. `cargo clippy --workspace --all-targets -- -D warnings`
4. `cargo build -p lattice-cli --bin lattice` (debug)
5. `cargo xtask harness-boundary`
6. `cargo xtask lancet-boundary`
7. `cargo xtask tool-codegen --check`
8. `cargo test` — one `cargo test -p <crate>` per workspace member in
   dependency order, each under a hard timeout
   (`LATTICE_TEST_TIMEOUT_SECS`, default 300 s), with `LATTICE_BIN` and
   `LATTICE_HARNESS_FAILURE_DIR` defaulted to the workspace `target/` tree.
   Build parallelism is capped (`CARGO_BUILD_JOBS`, default 4).
9. `cargo xtask tool-build --verify` (warn-only)

### Alpha rollout gate (layer 4)

```bash
cargo xtask rollout --channel alpha
```

Runs the release-evidence lane against a **locked release** binary and writes a
versioned JSON record. The plan (11 steps, `xtask/src/rollout.rs`):

1. `clean_worktree` — `git status --porcelain=v1 --untracked-files=no`;
   tracked dirt (staged or unstaged) is rejected. Untracked files are
   **not** fatal.
2. `capability_probes` — `rustc_version` (gating, must contain `1.96.0`),
   `wasm32_target_installed` (gating), plus informational `wasm_tools_version`,
   `git_version`, `dev_kvm_present` (never gating), `os`, `arch`.
3. `build_release_locked` — `cargo build --locked --release -p lattice-cli
   --bin lattice`.
4. `artifact_identity` — sha256 + size of `target/release/lattice`.
5. `harness_boundary` — `cargo xtask harness-boundary`.
6. `lancet_boundary` — `cargo xtask lancet-boundary`.
7. `direct_local` — `cargo test -p lattice-system-harness` against the release
   binary (`LATTICE_BIN`).
8. `offline_correctness` — `cargo test` for `lancet-protocol`, `lancet-catalog`,
   `lancet-authority`, `lancet-audit`, `lancet-native`, `lattice-session`.
9. `dogfood` — `cargo test -p lattice-api --test dogfood_impact_matrix`.
10. `release_operator_matrix` — `cargo test -p lattice-system-harness
    --features release-operator --test release_operator
    release_operator_native_component_mixed_matrix -- --exact` against the
    release binary. This feature-gated target owns N/C/M fixtures and
    assertions independent of `direct_operator`.
11. `doctor` — `lattice --json lancet doctor` with a cleared env and temp HOME;
    JSON saved to `target/rollout/evidence/doctor.json`.

**Skipped is not pass:** every step is seeded `blocked` and overwritten only by
actually running it; the verdict is `pass` iff every required step is `passed`
(`Failed`, `Blocked`, or `Skipped` ⇒ `Fail`). Each `cargo test` step also
requires a non-zero summed `N passed` count (anti-vacuous guard).

**Exit codes:** `0` verdict pass; `1` ran and verdict fail; `2` precondition
rejection (dirty worktree, git failure, CLI misuse).

**Record paths:** primary record at
`target/rollout/rollout-alpha-<yyyymmddTHHMMSSZ>.json`, plus a stable copy at
`target/rollout/alpha-latest.json`. `--out <path>` overrides the primary. The
record is initialized `verdict=fail` with all steps `blocked` and atomically
rewritten (temp file + rename) after every step, so a crash mid-run still
leaves a valid, failing record.

**Record contents** (`SCHEMA_VERSION = 1`): `schema_version`, `channel`,
`commit_sha`, `started_utc`, `finished_utc`, `artifact` (`{path, sha256,
size_bytes}`, `null` only if the locked build failed), `probes` (the capability
probes above), `steps` (one `StepRecord` per planned step: `id`, `required`,
`command`, `status`, `duration_ms`, `detail`, `evidence_paths`), and
`failure_artifacts` (repo-relative paths to harness bundles, doctor stderr,
etc.), plus `verdict` (`pass`/`fail`) and `rejection` (set only on
precondition rejection).

### LANCET self-check

```bash
lattice lancet doctor          # human-readable
lattice --json lancet doctor   # one JSON object on stdout (rollout evidence)
```

`lattice lancet doctor` (`crates/lattice-cli/src/commands/lancet.rs`, backed by
`run_doctor_checks`) runs registry/transport/policy checks and needs no
project. It is a rollout evidence step and a useful standalone post-build
sanity check. Related: `lattice lancet status` shows audit-derived CEP status
(recent calls, last guard verdict).

---

## 8. CI integration

### Ordinary lane — `.github/workflows/ci.yml`

- **Triggers:** `push` to `main`, all `pull_request` events.
- **Host:** `ubuntu-latest`. Linux is the sole supported host; non-Linux CI,
  release, compatibility, and fallback lanes are outside the project contract.
- **Steps (in order):**
  1. `actions/checkout@v4` (SHA-pinned)
  2. `dtolnay/rust-toolchain` (stable) with `rustfmt, clippy` components — the
     version resolves from `rust-toolchain.toml` (`1.96.0`)
  3. `Swatinem/rust-cache@v2`
  4. `cargo fmt --all --check`
  5. `cargo clippy --workspace --all-targets -- -D warnings`
  6. `cargo build -p lattice-cli --bin lattice` (debug)
  7. `cargo test --workspace` with
     `LATTICE_BIN=${{ github.workspace }}/target/debug/lattice` and
     `LATTICE_HARNESS_FAILURE_DIR=${{ github.workspace }}/target/harness-failures`
     — one step covers crate-local invariants **and** every external-binary
     scenario (`fast_*`, `direct_*`, `pty_tui_smoke`)
  8. `cargo xtask harness-boundary`
  9. `cargo xtask lancet-boundary`
  10. `cargo xtask tool-codegen --check`
  11. `actions/upload-artifact@v4` (`if: failure()`) uploads
      `target/harness-failures/`

Lint and format run before tests and are `-D warnings`-strict, so a single
clippy warning or formatting diff fails CI before tests execute. The build and
boundary checks run before `cargo test` because the harness needs a built
binary and a verified dependency boundary. Direct-Local scenarios run on the
supported Linux host without a hypervisor, daemon, or container image; the
ordinary lane has no host matrix.

### Full gate lane — `.github/workflows/full-gate.yml`

- **Triggers:** `workflow_dispatch` and `schedule` cron `0 3 * * *` (nightly).
  This lane is long (locked release build + full harness against the release
  binary) and does not gate every PR — the ordinary lane does.
- **Runner:** `ubuntu-latest` (single OS to bound the long lane's cost).
- **Steps:** everything the ordinary lane runs (checkout; rust-toolchain with
  `targets: wasm32-unknown-unknown`; rust-cache; fmt; clippy; debug build;
  `cargo test --workspace` with both env vars; `harness-boundary`;
  `lancet-boundary`; `tool-codegen --check`), plus a **pinned `wasm-tools`
  install** that only this lane needs (the ordinary lane does not install it —
  nothing there shells out to the encoder), then:
  - `cargo xtask rollout --channel alpha` — the layer-4 release-evidence lane.
  - `cargo xtask tool-build --verify` — rebuilds each Source Tool Component and
    compares its digest with the committed artifact. This is the only gate that
    binds a committed `.wasm` to its source, and it is what accumulates the
    cross-machine bit-for-bit evidence `#73` asks for. `wasm-tools` is installed
    at a pinned version because the encoder determines the resulting bytes
    (`#80`), and the same version is named in each manifest's `[artifact]
    toolchain` fingerprint.

    **Only digest drift is warn-only** per `#5` decision 1 — a mismatch prints a
    warning and does not fail. Every other outcome is a hard failure: the guest
    build, `wasm-tools component new`, `wasm-tools validate`, the `wasi:*`
    import-surface assertion, and WIT world conformance all abort the step. So
    this step can turn the nightly lane red.

    It runs last so a drift warning never masks an earlier hard failure, and
    carries `if: always()` — the preceding rollout gate is the lane's most
    failure-prone step, and skipping verify on its failure would lose the
    evidence from exactly the runs most worth inspecting. Output is teed to
    `target/rollout/component-verify.log` so the upload below carries it.
  - `actions/upload-artifact@v4` (`if: always()`) uploads `target/rollout/` as
    `rollout-alpha-record` — the record is written even on rejection/failure,
    alongside harness bundles, doctor evidence, and the Component verify log.
  - `actions/upload-artifact@v4` (`if: failure()`) uploads
    `target/harness-failures/` as `harness-failures-full-gate`.

No policy logic is duplicated in YAML: every step calls `cargo`, a public
xtask contract, or the built binary.

### Local pre-commit hook

The repo ships `.githooks/pre-commit` (install with
`git config core.hooksPath .githooks`). It is **not** a test runner — it
blocks commits that stage files matching the repository's Git ignore rules
(e.g. `.env`, `.codex/`, `.agents/`, `.planning/`, build outputs), catching
force-added private state before it enters history. It does not run `cargo
test` or clippy; use `cargo xtask ci` for a local test replica.

### Exclusions

No coverage tooling is configured: there is no `tarpaulin.toml`, no
`llvm-cov`/`.llvm-cov*` configuration, and no nextest coverage profile. No
`.config/nextest.toml` and no `nextest` section in `.cargo/config.toml` (which
defines only the `xtask` alias), so `cargo nextest` is **not** part of the
standard workflow. The gate enforces correctness via fmt, clippy
(`-D warnings`), and `cargo test`, not via a coverage percentage.

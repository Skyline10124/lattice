# AGENTS.md

Crate-local guidance for `lattice-system-harness`.

## Role

External system-test harness for the built `lattice` binary. It is **not** an
integration-test target of any product crate.

## Boundaries

- Manifest must not depend on `lattice-*`, `lancet-*`, `{ workspace = true }`,
  or `path = "../…"` product crates. Enforced by `cargo xtask harness-boundary`.
  General test/system libraries are allowed (`tempfile`, `portable-pty`,
  `serde`, `thiserror`); the boundary check keys off product-crate names and
  workspace membership, not off "any dependency".
- Only obtain the product via **`LATTICE_BIN`**; never import session/agent/API crates.
- Scenario worlds use isolated HOME/XDG, dedicated session store under scenario
  HOME, scenario-local ports, empty project `.env`, and `env_clear()` before allow-list.
- Git and `lattice` subprocesses share the same cleared allow-list (no host HOME/git config).
- Read session/audit state through `lattice` CLI only.
- The fake LLM (`src/fake_llm.rs`) connects through the formal
  `OPENAI_COMPATIBLE_*` configuration; there is no test-only provider,
  hidden flag, or product-side change (RFC #41 (Codeberg #19) decision four).
- `SessionSendDriver` (`src/session_send.rs`) drives the public
  `lattice --json session send` entrypoint and returns raw command evidence
  together with parsed JSONL. Unknown valid `TurnEvent` variants are kept as
  `Unknown`; malformed nonblank lines are retained separately. Exit code
  classification mirrors the product's 0/1/2 contract without importing it.
- Subprocesses are spawned in their own process group
  (`process_group(0)` on Unix). Timeout and controlled interrupt retain drained
  stdout/stderr, reap the direct child, and clean the whole group. The repo does
  not commit to Windows (see #48 (Codeberg #26) Out-of-Scope); process groups
  are a no-op there.
- Fake-provider races use `ResponseGate` plus bounded request waits. Failure
  evidence is redacted before writing, stale bundles are cleared, and recursive
  byte-level sentinel scans fail closed on both hits and I/O errors.
- `ScenarioBudget::cap_timeout` applies `min(step_timeout, remaining_total)`;
  multi-command drivers must use the capped value for every spawned command.
- Direct-Local integration targets share only generic plumbing through private
  `tests/support/mod.rs`; scenario-specific fixtures and assertions stay local.

## Test Layers (RFC #41 decision two / seven, updated by #62 (Codeberg #40))

- **Fast binary black-box** (`tests/fast_cli_help.rs`, plus all `#[test]`
  unit tests in `src/`): run on every ordinary CI. `cargo xtask ci` runs
  these by default.
- **Direct-Local turn scenarios** (`tests/direct_local_turn.rs`): plain-text
  turn, session reopen against changed Local state, and policy denial
  skeletons. They drive the real turn path against the canonical Local
  Workspace — no KVM, no MSB daemon, no OCI image, no `[workspace]` profile
  section — and run in the ordinary lane. The full Direct-Local scenario
  matrix landed in #46 (Codeberg #24) slices 0–8: `direct_turn_contract`
  (turn lifecycle + reopen), `direct_observation` (visibility/bounds),
  `direct_mutation` (atomic mutation/concurrency/effect denial), `direct_tools`
  (fetch/todo/catalog/retired methods), `direct_failures` (provider
  failures/SIGINT/deadline/no-rollback), `direct_operator` (audit/session/
  lancet cross-validation), `direct_secrets` (sentinel scanning),
  `direct_session_isolation` (two-session graph isolation),
  `direct_workspace_history` (#63: a real turn's installed mutation is
  recorded in the private bare history under the scenario's isolated HOME,
  read back with plain `gix`; a pre-install failure records nothing). The
  `TurnCommit.workspace_history_oid` binding is not observable through the
  CLI by design (#63 adds no user command), so it stays a library-layer
  assertion.
- **PTY smoke** (`tests/pty_tui_smoke.rs`): proves the TUI entrypoint wires
  into the same Direct-Local `stream_turn` chain. Also ordinary-lane since
  #62; the retired KVM-lane opt-in gate and its `layer_guard` module are
  gone — a broken Direct-Local environment is a product failure, never a
  skip.

## Verification

Run from the workspace root:

- `cargo xtask harness-boundary`
- `cargo build -p lattice-cli --bin lattice`
- Full suite (ordinary lane, no KVM/MSB required):
  `LATTICE_BIN="$(pwd)/target/debug/lattice" cargo test -p lattice-system-harness`
`LATTICE_BIN` is resolved by `LatticeDriver::from_env` **in the test process**
(`is_file()` then `canonicalize`), i.e. relative to the test-process cwd,
before any scenario child is spawned. A relative path that is a real file from
that cwd works fine — `canonicalize` then stores an absolute path so the later
child `current_dir` change to the scenario project dir cannot break it. The
old `LATTICE_BIN=target/debug/lattice` example failed only because `cargo test
-p lattice-system-harness` runs with the crate dir as cwd, where
`target/debug/lattice` does not exist (the binary is built at the workspace
root's `target/`). So the requirement is "a real file resolvable from the
test-process cwd"; `$(pwd)/target/debug/lattice` from the workspace root is the
cwd-independent way to satisfy it (and is what `cargo xtask ci` defaults
to). An absolute path is recommended but not a code-enforced contract.

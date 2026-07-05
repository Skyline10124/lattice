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
  hidden flag, or product-side change (RFC #19 decision four).
- `SessionSendDriver` (`src/session_send.rs`) drives the public
  `lattice --json session send` entrypoint and parses JSONL with a tolerant
  parser (unknown `TurnEvent` variants are kept as `Unknown`, never a parse
  error — the public contract is incremental). Exit code classification
  mirrors the product's 0/1/2 contract but never imports the product enum.
- Subprocesses are spawned in their own process group
  (`process_group(0)` on Unix) and a timeout kills the whole group via
  `kill -KILL -<pgid>` (RFC #19 decision eight, PRD user story 20). The repo
  does not commit to Windows (see #26 Out-of-Scope); `set_new_process_group`
  is a no-op on non-Unix.

## Test Layers (RFC #19 decision two / seven)

- **Fast binary black-box** (`tests/fast_cli_help.rs`, plus all `#[test]`
  unit tests in `src/`): run on every ordinary CI. No Microsandbox
  dependency. `cargo xtask ci` runs these by default.
- **Real Microsandbox + PTY smoke** (`tests/msb_full_turn.rs`,
  `tests/pty_tui_smoke.rs`): `#[ignore]`-gated, opted into via
  `LATTICE_HARNESS_MSB=1`. When the flag is set, the test body runs and
  `require_msb_env` probes the MSB stack via `lattice lancet status`; a
  broken MSB environment **panics** (the suite fails rather than silently
  passing — RFC #19 irreversible boundary 7). When the flag is unset the
  tests are skipped, keeping ordinary CI clean.

## Verification

Run from the workspace root:

- `cargo xtask harness-boundary`
- `cargo build -p lattice-cli --bin lattice`
- Fast lane: `LATTICE_BIN="$(pwd)/target/debug/lattice" cargo test -p lattice-system-harness`
- Full gate (real Microsandbox, owner's KVM host):
  `LATTICE_HARNESS_MSB=1 LATTICE_BIN="$(pwd)/target/debug/lattice" cargo test -p lattice-system-harness -- --include-ignored`
  (or `cargo test ... -- --include-ignored` to run ignored tests too). A
  broken MSB environment makes this fail, not skip.

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
# AGENTS.md

Crate-local guidance for `lattice-system-harness`.

## Role

External system-test harness for the built `lattice` binary. It is **not** an
integration-test target of any product crate.

## Boundaries

- Manifest must not depend on `lattice-*`, `lancet-*`, `{ workspace = true }`,
  or `path = "../…"` product crates. Enforced by `cargo xtask harness-boundary`.
- Only obtain the product via **`LATTICE_BIN`**; never import session/agent/API crates.
- Scenario worlds use isolated HOME/XDG, dedicated session store under scenario
  HOME, scenario-local ports, empty project `.env`, and `env_clear()` before allow-list.
- Git and `lattice` subprocesses share the same cleared allow-list (no host HOME/git config).
- Read session/audit state through `lattice` CLI only.

## Verification

Run from the workspace root:

- `cargo xtask harness-boundary`
- `cargo build -p lattice-cli --bin lattice`
- `LATTICE_BIN="$(pwd)/target/debug/lattice" cargo test -p lattice-system-harness`

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
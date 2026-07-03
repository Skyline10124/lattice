# AGENTS.md

Crate-local guidance for `lattice-cli`.

## Role

This crate owns the `lattice` binary entry point, clap command surface,
human-readable rendering, JSONL rendering, dotenv/logging setup, exit-code
semantics, and command modules that call into public API/session/model/audit
layers.

## Sub-Architecture

- `src/main.rs` owns clap commands, global flags, command dispatch, and process
  exit handling.
- `src/commands/run.rs` owns turn stream rendering, JSONL output, idle timeout,
  usage rendering, and exit-code derivation.
- `src/commands/session.rs` owns session command operations and turn send flow.
- `src/commands/models.rs`, `resolve.rs`, and `model_render.rs` own model
  listing and resolution output.
- `src/commands/doctor.rs` owns diagnostics.
- `src/commands/audit.rs` and `lancet.rs` own audit and LANCET inspection
  surfaces.
- `src/commands/test_env.rs` owns CLI test environment helpers.
- Integration tests cover cutover and release smoke behavior.

## Boundaries

- Keep the CLI thin; do not duplicate turn runtime or native LANCET execution.
- Prefer typed API/session/model calls over lower-level internals.
- Keep JSONL output aligned with public `TurnEvent` values from `lattice-api`.
- Command parsing belongs here; durable runtime semantics belong below the CLI.

- FIXED issue #11: `lattice audit` uses `lancet-api::{query_audit, tail_audit,
  replay_audit}` only (no CEP server for audit reads). `lattice audit replay
  --turn <id>` reconstructs turn provenance; `lattice audit` (no subcommand) and
  `lattice audit query` do row query/tail.

- FIXED L-18-03: turn stream rendering now has a five-minute idle timeout for
  hung providers.
- FIXED L-18-02: `TurnEvent` JSONL serialization no longer panics through
  `.expect()`.
- FIXED M-18-02: dead `status_to_exit_code` logic and legacy tests were removed.
- WATCH H-06/M-19b: CLI turn send relies on API live streaming and backpressure
  behavior.
- WATCH: `session send` still needs an existing session, matching agent profile,
  and provider credentials.

## Next Actions

- Keep parser/help tests updated for every command surface change.
- If API/agent durable deadlines are introduced, align CLI idle timeout behavior
  and exit-code messaging.
- Keep JSONL mode machine-readable: no human status lines on stdout in JSON
  mode.
- Keep diagnostics and LANCET inspection commands thin wrappers over public
  lower-layer surfaces.

## Record Maintenance

- Update `Recent Issues` and `Next Actions` when command surface, rendering,
  JSONL, timeout, or exit-code behavior changes.
- Record unresolved CLI/user-facing risks in root `FINDINGS.md`.
- Record landed CLI fixes in root `FIX.md` with verification commands.
- If a runtime issue is only visible in CLI, record the owning lower crate too.
- Keep this file near 80 lines by pruning stale fixed notes once they no longer
  guide edits.

## Verification

- Local: `cargo test -p lattice-cli`.
- Command-surface changes need parser/help tests.
- Turn rendering changes should run API streaming tests plus CLI release smoke
  or run integration tests.

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
- `src/commands/doctor.rs` owns diagnostics, including the credential/endpoint
  `source` column and the workspace `.env` summary.
- `src/env_boundary.rs` owns the workspace `.env` trust boundary: it loads the
  file, refuses the untrusted key classes, and records the provenance `doctor`
  reports. It is the only place that mutates the process environment, and
  `main` calls it before the tokio runtime exists because `set_var` is only
  sound single-threaded.
- `src/commands/audit.rs` and `lancet.rs` own audit and LANCET inspection
  surfaces.
- CLI audit/status tests serialise `XDG_STATE_HOME` through
  `lancet_audit::ScopedXdgStateHome`, pulled in via
  `[dev-dependencies] lancet-audit = { features = ["test-support"] }`. The
  crate-local `src/commands/test_env.rs` copy was deleted in GitHub #39.
- Integration tests cover cutover and release smoke behavior.

## Boundaries

- Keep the CLI thin; do not duplicate turn runtime or native LANCET execution.
- Prefer typed API/session/model calls over lower-level internals.
- Keep JSONL output aligned with public `TurnEvent` values from `lattice-api`.
- Command parsing belongs here; durable runtime semantics belong below the CLI.

- A workspace `.env` is untrusted input. `src/env_boundary.rs`
  refuses `LATTICE_*`, provider endpoint keys (`*_BASE_URL`, `*_API_BASE`,
  `*_API_URL`, `*_ENDPOINT`), `*_PROXY`, `HOME`/`XDG_*`, and
  `SSL_CERT_FILE`/`SSL_CERT_DIR` from it (case-insensitively), while ordinary
  credentials still apply. Do not add a second `.env` loader, do not call
  `dotenvy::dotenv()` directly, and do not make refusals print at startup —
  `doctor` is the observability surface. Refusals are keyed on patterns, not an
  enumerated key list, so a new `LATTICE_*` or `*_BASE_URL` variable is covered
  without touching the boundary. `plan_dotenv` is the pure seam: it takes the
  file bytes plus an `already_set` predicate and returns the report and the
  pairs to install, so precedence/BOM/parse-failure are unit-testable without
  mutating the test process env — keep new decisions there, not in the
  `set_var` loop. When adding an env var that a dependency reads (not just our
  own code), check it against `classify`: `*_PROXY` is load-bearing because
  `hyper-util` reads it, and `SSL_CERT_*` because `rustls-native-certs`
  *replaces* the trust store when either is set. Refused values must not enter
  dotenv substitution state, and diagnostics must never echo source lines.

## Next Actions

- Keep parser/help tests updated for every command surface change.
- Preserve the public 0/1/2 exit contract and add exact black-box assertions
  whenever a pre-stream error classification changes.
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

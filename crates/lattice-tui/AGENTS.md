# AGENTS.md

Crate-local guidance for `lattice-tui`.

## Role

This crate owns the inline CLI chat REPL for LATTICE. It reads lines from
stdin, executes turns through `lattice_api::stream_turn`, and renders tokens
to stdout and tool/status lines to stderr. No alternate screen, no raw mode,
no ratatui — just a `> ` prompt and streaming output.

## Boundaries

- Keep runtime calls through public `lattice-api` types.
- Keep terminal I/O simple: `print!`/`write!` to stdout, `eprintln!`/`writeln!`
  to stderr.
- Keep CLI argument parsing in `lattice-cli`; the CLI calls `lattice_tui::run`.
- Surface missing session/profile/provider problems on stderr before the REPL
  starts.

## Source layout

- `lib.rs` — module declarations + public re-exports
- `app.rs` — thin types: `AppConfig`, `AppEvent`
- `runtime.rs` — REPL loop, session bootstrap, turn execution, event rendering

## Verification

- Local: `cargo test -p lattice-tui`.
- CLI surface changes should also run `cargo test -p lattice-cli`.

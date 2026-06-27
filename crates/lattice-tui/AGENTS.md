# AGENTS.md

Crate-local guidance for `lattice-tui`.

## Role

This crate owns the inline CLI chat REPL for LATTICE. It reads lines from
stdin via `rustyline` (line editing, history, Ctrl-C/Ctrl-D handling), executes
turns through `lattice_api::stream_turn`, and renders tokens to stdout and
tool/status lines to stderr. No alternate screen and no ratatui — rustyline
enters raw mode only for the duration of each readline and restores it
afterward, so the session stays inline.

## Boundaries

- Keep runtime calls through public `lattice-api` types.
- Keep terminal I/O simple: `print!`/`write!` to stdout, `eprintln!`/`writeln!`
  to stderr. The only raw-mode surface is `rustyline`'s readline; do not add
  other raw-mode or alternate-screen code.
- Keep CLI argument parsing in `lattice-cli`; the CLI calls `lattice_tui::run`.
- Surface missing session/profile/provider problems on stderr before the REPL
  starts.
- REPL history persists to `<session_dir>/repl_history.txt` (best effort).
- Cooperative turn cancellation uses `RunTurnRequest::cancel_flag` set by a
  Ctrl-C watcher spawned for the duration of each turn.

## Source layout

- `lib.rs` — module declarations + public re-exports
- `app.rs` — thin types: `AppConfig`, `AppEvent`
- `runtime.rs` — REPL loop, session bootstrap, turn execution, event rendering

## Verification

- Local: `cargo test -p lattice-tui`.
- CLI surface changes should also run `cargo test -p lattice-cli`.

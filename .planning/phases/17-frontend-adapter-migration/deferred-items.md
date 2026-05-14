# Deferred Items

## 17-02 Out-of-Scope Clippy Finding

- `cargo clippy -p lattice-cli --all-targets -- -D warnings` still fails on pre-existing `clippy::field-reassign-with-default` warnings in `crates/lattice-cli/src/frontend/tui/turn.rs` test setup at lines 212, 262, and 289.
- This is outside Plan 17-02's CLI `code`/`run`/`debug` runtime-turn migration scope and should be handled by the upcoming TUI migration/hardening plans.

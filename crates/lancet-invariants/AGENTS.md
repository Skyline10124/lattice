# AGENTS.md

Crate-local guidance for `lancet-invariants`.

## Role

This crate owns LANCET invariant cards, `.lancet/invariants` resolution, card
loading and indexing, opaque human-review helpers, and Guard L2 Rust AST
category checks.

## Sub-Architecture

- `src/invariants/types.rs` defines card, anchor, forbidden, and opaque review
  data.
- `src/invariants/loader.rs` loads and validates TOML cards.
- `src/invariants/index.rs` indexes cards by anchor/path for native lookup.
- `src/invariants/review.rs` produces opaque human-review messages and notes.
- `src/invariants/mod.rs` exports the card subsystem.
- `src/guard_l2.rs` maps tree-sitter Rust syntax into AST categories and Guard
  L2 violations.
- `src/lib.rs` re-exports the stable public surface for native integration.

## Boundaries

- No native mutations, command execution, audit persistence, or policy decisions.
- Keep invariant parsing independent of native server lifecycle.
- Preserve opaque-review behavior; do not expose hidden review details
  accidentally.
- Keep tree-sitter checks shallow and method-driven unless a deeper L2 design is
  intentionally introduced.

## Recent Issues

- WATCH M-24-05: `lattice-api` currently composes invariant loading/checking as
  part of the large turn-scoped LANCET runtime wiring.
- WATCH: native mutation handlers depend on this crate for Guard L2 category
  status but own the actual mutation lifecycle.
- WATCH: opaque review helpers are security-sensitive because wording can reveal
  or hide internal invariant details.
- No open crate-local invariant finding is currently listed in `FINDINGS.md`.

## Next Actions

- If turn composition is split out of `lattice-api`, keep invariant card model
  here and move only orchestration/wiring.
- Add fixture cards and Rust snippets when changing Guard L2 category mapping.
- Keep card loader errors precise enough for operators but not leaky for opaque
  review flows.
- Coordinate new card fields with native handler behavior and profile/project
  root resolution.

## Record Maintenance

- Update `Recent Issues` and `Next Actions` when card format, loader behavior,
  opaque review wording, or Guard L2 checks change.
- Record unresolved invariant risks in root `FINDINGS.md`.
- Record landed invariant fixes in root `FIX.md` with verification commands.
- Mention cross-crate issues here only when they affect local ownership or next
  edits.
- Keep this file near 80 lines; prune old closed notes once `FIX.md` has the
  durable record.

## Verification

- Local: `cargo test -p lancet-invariants`.
- Guard L2 changes: also run affected `lancet-native` mutation and policy tests.
- Card format changes should include loader, index, and review helper coverage.

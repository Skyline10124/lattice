# Issue #46 Slice 6 — Operator audit paths and secret discipline

## Goal

Cross-validate completed/failed Turns through public audit/session/LANCET
commands and prove sentinels never appear in terminal, durable projections,
provenance, or failure artifacts.

## Public turn identity

Audit replay requires a Turn ID. Add an additive `turn_id` field to
`TurnStarted` (and parse it in the harness) rather than making the harness
reconstruct `<session>:<parent-or-root>` from implementation details.

```json
{"type":"turn_started","session_id":"...","turn_id":"...","branch":"main"}
```

Update public type docs and JSON tests; old consumers remain compatible with an
additive field.

## Operator matrix

After real success and denial Tool calls, run separate bounded commands:

```text
lattice --json audit query --session <id>
lattice --json audit query --session <id> --method <method> --verdict <verdict>
lattice --json audit query --session <id> --tail --since 0
lattice --json audit replay --session <id> --turn <turn-id>
lattice --json lancet status
lattice --json session status <id>
lattice --json session log <id>
```

Also query an unknown session (empty success) and replay an unknown Turn
(explicit nonzero failure).

Assert row count, methods, impacts, verdicts, actual path/host/content identity,
activity order, stored attestation match, source hash/range, recent status calls,
and last guard verdict. Use required fragments, not full provenance goldens.

## Sentinel sources and sinks

Use unique per-scenario sentinels for provider credential, provider error text,
denied workspace content, and artifact command arguments. Scan in memory before
persistence and recursively after failure-artifact persistence.

Required sinks:

- every command stdout/stderr;
- parsed JSONL and malformed lines;
- captured fake-provider request JSON (authorization records scheme/presence,
  never value);
- `session status/log` output;
- audit query/tail/replay and `lancet status` output;
- provenance/attestation JSON;
- redacted failure bundle including fake requests and workspace diff.

A detected sentinel is an assertion failure even if the scanner can redact the
bundle copy.

## Exact replacement IDs

Land all six `planned_direct_audit_*` IDs,
`planned_direct_turn_audit_replay`, and
`planned_direct_lancet_status_seeded`. The old CLI helper tests currently assert
mostly `Ok`; delete them only after JSON fragment assertions pass.

## Files

- public Turn event type/mapping/parser tests
- `crates/lattice-system-harness/tests/direct_operator.rs`
- `crates/lattice-system-harness/tests/direct_secrets.rs`
- harness evidence/scanner integration
- mapped CLI/API tests and triage ledger
- `FIX.md` for any leak or turn-id contract fix

## Verification

```bash
cargo +1.96.0 test -p lattice-api audit
cargo +1.96.0 test -p lattice-cli audit
LATTICE_BIN="$(pwd)/target/debug/lattice" \
  cargo +1.96.0 test -p lattice-system-harness --test direct_operator
LATTICE_BIN="$(pwd)/target/debug/lattice" \
  cargo +1.96.0 test -p lattice-system-harness --test direct_secrets
```

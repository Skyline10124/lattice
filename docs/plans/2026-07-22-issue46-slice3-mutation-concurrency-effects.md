# Issue #46 Slice 3 — Atomic mutation, external concurrency, and effect denial

## Goal

Prove `mutate.create`/`mutate.edit` operate directly and atomically on current
Local state, never roll back after later Turn failure, and reject stale or
denied mutations before changing the target.

## Approved product correction

The user approved pre-dispatch effect denial. The server must compare a method's
static declared effect kinds with `denied_effect_kinds` before invoking the
handler. The existing post-dispatch observed-effect check remains defense in
depth, not the first enforcement point.

```rust
let descriptor = catalog_descriptor(method)?;
if descriptor.effect.kinds.intersects(&policy.denied_effect_kinds) {
    return audit_denial_without_dispatch(...);
}
let result = dispatch(...).await;
check_observed_effects(...)?;
```

This belongs in LANCET server enforcement/composition, not in Tool handlers.
Add a focused `lancet-native` regression proving a denied workspace mutation
never calls the backend and records a denied audit row.

## Vertical scenarios

1. `mutate.create` succeeds only while the target is absent; bytes appear
   immediately and atomically.
2. Existing create target returns structured failure and preserves bytes.
3. `mutate.edit` uses a known `blake3:<64 hex>` Local Revision and succeeds.
4. Valid hash + nonmatching edit anchor fails before install and preserves bytes.
5. External writer changes the file between observe and edit; stale edit returns
   `ReplanRequired` and preserves external bytes.
6. External writes between Tool calls are visible to the next observe call.
7. Successful create/edit followed by provider disconnect/illegal SSE commits an
   error Turn but retains the Local mutation (no implicit rollback).
8. Write policy, Guard L1/system exclusion, and preflight effect denial produce
   no host fallback and no filesystem side effect.
9. Two gated sends prepared from one session parent produce one winner and one
   deterministic `commit_failed`; Local mutation assertions remain independent
   of session commit success.

## Harness synchronization

```rust
let running = send.spawn("edit the file")?;
server.wait_for_requests(1, STEP)?;
world.write_project_file("tracked.txt", "external\n")?;
gate.release();
let outcome = running.wait(STEP)?;
```

No automatic retry. The fake provider scripts one response per request and each
wait is bounded by both step and total scenario timeout.

## Evidence

Cross-check JSONL, next provider Tool message, exact Local bytes, public session
log/status, audit query/replay, and failure bundle. Audit assertions require
path/content identity hashes and verdict, never raw secret content.

## Files

- `crates/lattice-system-harness/tests/direct_mutation.rs` (new)
- `crates/lancet-native/src/server/transport.rs` and focused tests
- catalog/descriptor lookup only through existing authorities; no third schema
- mapped internal tests/docs ledger after each replacement passes
- `FIX.md` after the effect-denial fix lands

## Verification

```bash
cargo +1.96.0 test -p lancet-native effect
LATTICE_BIN="$(pwd)/target/debug/lattice" \
  cargo +1.96.0 test -p lattice-system-harness --test direct_mutation -- --test-threads=1
cargo +1.96.0 test -p lattice-api
```

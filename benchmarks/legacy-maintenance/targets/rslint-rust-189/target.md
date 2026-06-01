# Target: rslint-rust-189

## Repository

- URL: https://github.com/rslint/rslint.git
- Pinned commit: `7bc011e57c76ee114c440521a51907089584b10a`
- Default branch at freeze time: `master`
- License: MIT
- Primary language/tooling: Rust workspace
- Source page: https://github.com/rslint/rslint

## Why This Target

`rslint/rslint` is a realistic stale-maintenance target for LATTICE:

- It is a non-trivial Rust codebase rather than a synthetic fixture.
- It is code-focused enough to exercise repair and regression workflows.
- It has real user-facing issues and open PRs.
- GitHub metadata checked on 2026-06-01 showed the repository was last pushed on
  2023-03-05, while open issue/PR activity continued into 2025-2026.
- The selected task is precise and reproducible: newer Rust rejects an implicit
  autoref in unsafe pointer code.

## Frozen Task

- Original PR / issue URL: https://github.com/rslint/rslint/pull/165
- Frozen title: `fix: rust 1.89 compatibility`
- Frozen body: `issue.md`

The benchmark should treat the PR body as an issue report, not as an already
accepted patch. The agent should inspect the code, reproduce or narrow the
compile error, make a minimal repair, and add/update coverage only if there is a
reasonable local regression hook.

## Suggested Verification

Primary:

```sh
cargo check -p rslint_rowan
```

Fallback if workspace dependency resolution requires broader context:

```sh
cargo check --workspace
```

## Expected Edit Surface

Likely file:

```text
crates/rslint_rowan/src/arc.rs
```

The expected fix is to make the mutable slice reference explicit around the raw
pointer dereference before calling `get_mut(0)`.


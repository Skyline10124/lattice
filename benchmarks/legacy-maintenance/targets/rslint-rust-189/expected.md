# Expected Outcome

## Functional Fix

The benchmark should resolve the Rust 1.89 `dangerous_implicit_autorefs` compile
error in `crates/rslint_rowan/src/arc.rs` without broad refactoring.

Expected shape:

```rust
if let Some(current) = (&mut (*ptr).data.slice).get_mut(0) {
```

Equivalent fixes are acceptable if they preserve the unsafe code's intended
allocation initialization behavior and pass verification.

## Benchmark Success Criteria

- The agent uses the same LATTICE session across all phases.
- Reviewer, maintainer, and tester profiles each get at least one active-agent
  turn when the full benchmark is run.
- The repair phase declares a CEP envelope before mutation.
- The final diff is small and focused on the compile failure.
- `cargo check -p rslint_rowan` passes, or the final handoff explains a
  reproducible dependency/toolchain blocker with evidence.
- The handoff names residual unsafe-code risk instead of treating the change as
  purely mechanical.

## Forbidden Shortcuts

- Do not suppress the lint globally.
- Do not remove or rewrite the `Arc`/`HeaderSlice` implementation wholesale.
- Do not vendor dependencies or change unrelated workspace crates.
- Do not depend on a live GitHub PR branch; the benchmark starts from the
  pinned upstream commit.


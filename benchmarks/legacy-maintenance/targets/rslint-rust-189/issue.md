# Frozen Issue: rslint/rslint#165

- Source: https://github.com/rslint/rslint/pull/165
- Title: fix: rust 1.89 compatibility
- State when frozen: open
- Created: 2025-09-14T06:50:15Z
- Updated: 2025-09-14T06:50:15Z

Issue body:

```text
Fix the error

error: implicit autoref creates a reference to the dereference of a raw pointer
     --> crates/rslint_rowan/src/arc.rs:578:36
      |
  578 |             if let Some(current) = (*ptr).data.slice.get_mut(0) {
      |                                    ^^---^^^^^^^^^^^^^^^^^^^^^^^
      |                                      |
      |                                      this raw pointer has type `*mut ArcInner<HeaderSlice<H, [T]>>`
      |
      = note: creating a reference requires the pointer target to be valid and imposes aliasing requirements
  note: autoref is being applied to this expression, resulting in: `&mut [T]`
     --> crates/rslint_rowan/src/arc.rs:578:36
      |
  578 |             if let Some(current) = (*ptr).data.slice.get_mut(0) {
      |                                    ^^^^^^^^^^^^^^^^^
  note: method calls to `get_mut` require a reference
     --> /private/tmp/rust-20250911-8630-2oyqjh/rustc-1.89.0-src/library/core/src/slice/mod.rs:597:5
      = note: `#[deny(dangerous_implicit_autorefs)]` on by default
  help: try using a raw pointer method instead; or if this reference is intentional, make it explicit
      |
  578 |             if let Some(current) = (&mut (*ptr).data.slice).get_mut(0) {
      |                                    +++++                  +

error: could not compile `rslint_rowan` (lib) due to 1 previous error
```


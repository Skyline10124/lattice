# VULN-002 Sandbox write allowlist uses substring matching

Severity: High  
Component: `lattice-agent/src/sandbox.rs`  
Affected logic: `SandboxConfig::check_write`

## Summary

The sandbox write policy checks whether a requested path is allowed by testing whether the path `contains()` one of the allowlisted directory strings. This is not a path-boundary check. A crafted path that merely includes an allowlisted substring can satisfy the policy even when the real target is outside the intended directory tree.

## Relevant code

```rust
let allowed = self
    .write_allowlist
    .iter()
    .any(|prefix| path.contains(prefix));
```

Default allowlist includes values such as:
- `lattice-core/`
- `lattice-agent/`
- `lattice-python/`
- `lattice-plugin/`
- `lattice-harness/`
- `lattice-cli/`
- `lattice-tui/`

## Impact

If a caller can influence a write path, sandboxed writes may reach unintended locations as long as the user-controlled path string embeds an allowlisted fragment. Depending on how the executor resolves paths, this can enable unauthorized file creation or overwrite outside the intended project subtree.

## Exploit idea

If the tool accepts relative or nested paths, examples to test conceptually include names such as:
- `tmp/lattice-core/../../docs/pwn.md`
- `not-really-lattice-core/evil.txt`
- `/var/tmp/x-lattice-cli-/payload`

Whether each exact example succeeds depends on downstream path normalization, but the policy bug exists before any such normalization.

## Why this matters

Authorization should be based on canonical path ancestry, not string inclusion. Substring checks confuse naming with location.

## Recommended fix

1. Resolve the requested path against a trusted workspace root.
2. Canonicalize both the requested path and each allowlisted root.
3. Approve only if the requested canonical path is equal to or a descendant of an allowlisted canonical root.
4. Reject non-existent parent traversal patterns before write creation, or canonicalize the parent directory.

## Verification

Add unit tests proving these are rejected:
- sibling directories whose names contain `lattice-core`
- paths that include an allowlisted fragment but escape via `..`
- absolute paths outside workspace

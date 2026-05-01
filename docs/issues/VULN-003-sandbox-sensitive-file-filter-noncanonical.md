# VULN-003 Sandbox sensitive-file filter is substring-based and non-canonical

Severity: Medium  
Component: `lattice-agent/src/sandbox.rs`  
Affected logic: `SandboxConfig::check_read`

## Summary

Sensitive-file blocking relies on naive substring checks such as `path.contains(".env")` and `path.contains(".git/credentials")`. This is brittle in both directions:
- it may overblock benign files whose names merely contain a protected fragment;
- it may underprotect via alternate path forms, symlinked access, or canonical path differences if path resolution happens elsewhere.

## Relevant code

```rust
for sensitive in &self.sensitive_files {
    if path.contains(sensitive) {
        return Err(...);
    }
}
```

## Impact

This is a defense-boundary weakness. On its own it is not a guaranteed exfiltration path, but when combined with symlinks, alternate roots, or separate file resolution layers, it can allow sensitive targets to be referenced without matching the literal blocked substring.

## Blue-team assessment

The filter expresses intent but not identity. Security checks should bind to resolved filesystem objects or to trusted rooted path prefixes, not to raw user strings.

## Recommended fix

1. Canonicalize the target path where possible.
2. Maintain a set of protected canonical roots/files.
3. Compare with ancestry checks rather than substring checks.
4. Consider deny-by-default for dotfiles and credential-like names outside approved source trees.

## Additional note

The current check also rejects any path containing `..`, which helps, but that still does not replace canonicalization and does not address symlink traversal.

# VULN-001: Sandbox newline/carriage-return command injection bypass

| Field | Value |
|-------|-------|
| ID | VULN-001 |
| Severity | HIGH |
| Category | command_injection |
| Status | **OPEN** |
| Confidence | 9/10 |
| Affected files | `lattice-agent/src/sandbox.rs:121`, `lattice-agent/src/tools.rs:223`, `lattice-agent/src/tools.rs:286-290` |
| Introduced | commit `b2f2f34` (P2-1: bash tool command injection prevention) |

## Description

`SandboxConfig::check_command()` filters shell metacharacters `[";", "|", "&&", "||", "$(", "`"]`
but **does not include `\n` (newline) or `\r` (carriage return)**. In `sh -c` execution,
`\n` and `\r` act as command separators equivalent to `;`. The allowlist check at line 135
uses `cmd.split_whitespace().next()` which splits on `\n`, so only the first token is validated —
the second command after `\n` is never examined.

The sandbox's stated purpose (line 112-114 comment) is to "reject dangerous shell metacharacters
that enable command injection via `sh -c` execution." Newline and carriage return serve the exact
same function as `;` in that context — this is a direct bypass of an existing security control.

## Root cause (5-Why)

1. **Why was the LLM able to inject a second command?** `\n` in the command string was not blocked.
2. **Why was `\n` not blocked?** The metacharacter list at line 121 omits `\n` and `\r`.
3. **Why were they omitted?** The list was constructed by enumerating *symbolic* shell operators
   (`;`, `|`, `&&`, etc.) but not *whitespace* separators that `sh -c` also interprets.
4. **Why was the enumeration incomplete?** No systematic analysis of `sh -c` token separators
   was performed during the P2-1 fix; only the most common symbolic metacharacters were listed.
5. **Why no systematic analysis?** The fix was reactive (fix specific injection patterns seen
   in tests) rather than proactive (enumerate ALL `sh -c` separator characters).

## Exploit scenario

LLM outputs tool call:
```json
{"name": "bash", "arguments": "{\"command\": \"ls\\nrm -rf /\"}"}
```

After serde_json deserialization, `command` = `"ls\nrm -rf /"` (real newline).

- `check_command("ls\nrm -rf /")` — no `;`, `|`, `&&`, `||`, `$(`, or backtick found → passes metacharacter check
- `split_whitespace().next()` returns `"ls"` → `"ls"` IS in default allowlist → passes allowlist check
- `sh -c "ls\nrm -rf /"` — shell executes both `ls` AND `rm -rf /`

Permissive mode (`SandboxConfig::permissive()`) is also bypassed: metacharacter check runs
before the empty-allowlist shortcut at line 130, so `\n` passes through unfiltered.

## Reproduction

```rust
let s = SandboxConfig::default();
// Should be rejected, but PASSES:
assert!(s.check_command("ls\nrm -rf /").is_ok()); // BUG
assert!(s.check_command("ls\rrm -rf /").is_ok()); // BUG

// Currently only these pass the filter (correctly):
assert!(s.check_command("ls; rm -rf /").is_err());  // ; is blocked
assert!(s.check_command("ls | rm -rf /").is_err()); // | is blocked
```

## Fix

**sandbox.rs line 121** — add `\n` and `\r` to the metacharacter list:

```rust
// Before:
for meta in &[";", "|", "&&", "||", "$(", "`"] {

// After:
for meta in &[";", "|", "&&", "||", "$(", "`", "\n", "\r"] {
```

**sandbox.rs tests** — add coverage for newline/carriage-return injection:

```rust
#[test]
fn test_shell_injection_via_newline_rejected() {
    let s = default_sandbox();
    assert!(s.check_command("ls\nrm -rf /").is_err());
}

#[test]
fn test_shell_injection_via_carriage_return_rejected() {
    let s = default_sandbox();
    assert!(s.check_command("ls\rrm -rf /").is_err());
}

#[test]
fn test_permissive_mode_rejects_newline_injection() {
    let s = SandboxConfig::permissive();
    assert!(s.check_command("curl http://example.com\nrm -rf /").is_err());
}
```

## SOP: preventing recurrence

When adding shell metacharacter filters for `sh -c` execution, enumerate ALL characters
that `sh` interprets as token separators or operators, not just the most common symbolic ones.

Full list: `;`, `|`, `&&`, `||`, `$(`, `` ` ``, `\n`, `\r`, `&` (background),
`>` (redirect), `<` (redirect), `>>` (append), `<>` (open rw). For `sh -c`,
the minimum separator set is: `;`, `|`, `&&`, `||`, `\n`, `\r`, `$(`, `` ` ``.
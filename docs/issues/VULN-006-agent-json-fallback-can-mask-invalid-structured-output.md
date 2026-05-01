# VULN-006 Agent JSON fallback can mask invalid structured output

Severity: Low / Defense-in-depth  
Component: `lattice-harness/src/runner.rs`  
Affected logic: `AgentRunner::run_once`

## Summary

When agent output is expected to be JSON, parse failure falls back to wrapping the entire raw content as `{"content": ...}` instead of surfacing a hard error. This improves resilience, but it also weakens the semantic boundary between structured and unstructured output. Downstream logic may proceed on a shape that was never truly produced by the agent.

## Relevant code

```rust
let output: serde_json::Value = serde_json::from_str(&json_str)
    .unwrap_or_else(|_| serde_json::json!({"content": content}));
```

## Security concern

If downstream handoff or policy code assumes structured JSON fields are genuine, an attacker can steer behavior by forcing schema retries, malformed fenced blocks, or plain-text output that still gets accepted as JSON-shaped fallback content.

This is not a direct code-execution issue. It is a trust-boundary softening issue.

## Impact

- invalid structured responses may be treated as successful outputs
- policy and routing logic may operate on degraded semantics
- incident triage becomes harder because parse failures are hidden

## Recommended fix

1. Distinguish strict-JSON mode from best-effort mode.
2. In strict mode, return an explicit parse error rather than fallback wrapping.
3. Emit telemetry when fallback occurs.
4. Ensure handoff rules do not silently treat fallback content as a valid structured decision.

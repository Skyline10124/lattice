# Security

Runtime security is enforced on the Rust execution path.

## Model Endpoint Safety

Base URL validation happens in `ModelRouter::resolve()`. This ensures all Rust callers go through the same validation.

Validation covers:

- URL parsing
- supported schemes
- missing host rejection
- private and reserved IP handling
- credential redaction in logs and debug output

## Tool Safety

Tool execution passes through sandbox checks before process execution or network access.

Checks include:

- command allowlist
- shell metacharacter rejection
- environment override rejection
- sensitive path checks
- URL validation
- private/reserved IP checks
- optional hook chain decisions

## Audit

The audit log is append-only JSONL with rolling files. Synchronous audit writes keep background task handles so failures can be reaped instead of silently dropping work.

## Plugin Safety

Runtime plugins are typed functions. `Behavior` controls retry and escalation, while deterministic handoff rules decide routing. Official plugin code lives outside Runtime so the runtime contract remains smaller and easier to audit.

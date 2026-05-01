# VULN-005 Python binding lock poisoning can panic host process

Severity: Medium  
Component: `lattice-python/src/engine.rs`  
Affected methods: `resolve_model`, `list_models`, `list_authenticated_models`

## Summary

The Python binding wraps `ModelRouter` in `Mutex<ModelRouter>` and acquires the lock using `.lock().unwrap()`. If the mutex is ever poisoned by a panic while held, later calls panic immediately instead of returning a Python exception. In embedded Python hosts, this can crash the extension caller and convert an internal fault into process-level denial of service.

## Relevant code

```rust
self.router.lock().unwrap().resolve(model, None)
self.router.lock().unwrap().list_models()
self.router.lock().unwrap().list_authenticated_models()
```

## Impact

Any panic occurring while the mutex is held poisons the lock. Subsequent benign calls then panic deterministically. Because this sits on the FFI boundary, robustness matters more than usual.

## Exploitability

This is not a straightforward remote exploit by itself. It becomes relevant when untrusted inputs can trigger panics in code executed while holding the lock, or when the host embeds this extension in a long-running service. The main security consequence is denial of service.

## Recommended fix

1. Replace `.lock().unwrap()` with poison-tolerant recovery, for example:
   - `.lock().unwrap_or_else(|e| e.into_inner())` when safe, or
   - return a structured Python exception on poison.
2. Minimize the amount of code executed while holding the lock.
3. Audit resolver paths for panic-on-invalid-state behavior.

## Verification

Add a test that intentionally poisons the mutex in a controlled way, then confirm the Python API returns an error rather than panicking.

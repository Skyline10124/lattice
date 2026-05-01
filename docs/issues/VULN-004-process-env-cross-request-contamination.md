# VULN-004 Process-wide environment mutation creates cross-request contamination risk

Severity: High  
Component: model resolution / credential routing  
Primary evidence: repository-wide use of `std::env::set_var` and `remove_var`; prior review focus on `resolve` flows

## Summary

Parts of the repository use process-wide environment variables as a transport for credentials or provider selection state. Environment variables are global mutable state shared by the entire process. When used for temporary overrides or request-scoped behavior, they can leak across concurrent operations and contaminate unrelated resolutions.

## Why this is a security issue

Credentials and provider selection are authorization-sensitive inputs. Process-global mutation means one request can affect another request's routing or credentials. In the worst case, one tenant or task may cause another to execute with the wrong provider key, wrong endpoint, or stale authentication state.

## Evidence

Repository scan shows multiple reads from env in production paths and multiple `set_var/remove_var` sites in tests and resolve-related flows. Even if current production code mostly reads env and test code performs the writes, the overall design still normalizes env as a control plane. That is fragile and tends to reappear in real code.

`lattice-core/src/router.rs` also caches resolved credentials, which magnifies the risk if cache keys do not perfectly capture all credential-selection dimensions.

## Blue-team scenario

A local concurrent harness, CLI wrapper, plugin host, or future service embedding may:
1. temporarily set `OPENAI_API_KEY` or `ANTHROPIC_API_KEY` for one operation;
2. run another resolution concurrently in the same process;
3. observe cross-request credential bleed or stale cache use.

This is a classic shared-state hazard: not flashy, but operationally real.

## Impact

- wrong credentials attached to outbound API requests
- cross-tenant or cross-task confusion
- nondeterministic failures that are hard to reproduce
- possible disclosure through error messages, billing mix-up, or incorrect model/provider use

## Recommended fix

1. Stop using process env as request-scoped transport.
2. Pass credentials explicitly through configuration objects.
3. Keep env reads at process bootstrap only, then freeze into immutable config.
4. Ensure credential cache keys include every dimension that influences credential choice.
5. Add concurrency tests for mixed-provider resolution.

## Note

This issue is architectural rather than a single-line bug, but it deserves high priority because it affects trust boundaries.
